"""MCP bridge exposing the mvm Agent API to an agent running inside a microVM.

Guest -> host communication uses AF_VSOCK directly.

Wire protocol:

    uint32_be payload_length
    uint8[payload_length] UTF-8 JSON

Each connection carries exactly one request and one response.

Configuration:

    MVM_AGENT_CID=2
    MVM_AGENT_PORT=24643
    MVM_AGENT_TOKEN=<vm-scoped-token>
"""

from __future__ import annotations

import json
import os
import socket
import struct
from typing import Any

from fastmcp import FastMCP


AGENT_CID = int(os.environ.get("MVM_AGENT_CID", "2"))
AGENT_PORT = int(os.environ.get("MVM_AGENT_PORT", "24643"))
TOKEN = os.environ.get("MVM_AGENT_TOKEN", "")

MAX_MESSAGE_SIZE = 4 * 1024 * 1024

CONNECT_TIMEOUT = 5.0
IO_TIMEOUT = 30.0


mcp = FastMCP("mvm-agent")


class AgentTransportError(RuntimeError):
    """A failure while connecting to or communicating with the Agent."""


class AgentProtocolError(RuntimeError):
    """The Agent sent an invalid or malformed response."""


class VsockTransport:
    """Length-prefixed JSON transport over AF_VSOCK."""

    def __init__(
        self,
        cid: int,
        port: int,
        *,
        max_message_size: int = MAX_MESSAGE_SIZE,
        connect_timeout: float = CONNECT_TIMEOUT,
        io_timeout: float = IO_TIMEOUT,
    ):
        if max_message_size <= 0:
            raise ValueError("max_message_size must be positive")

        if connect_timeout <= 0:
            raise ValueError("connect_timeout must be positive")

        if io_timeout <= 0:
            raise ValueError("io_timeout must be positive")

        self.cid = cid
        self.port = port
        self.max_message_size = max_message_size
        self.connect_timeout = connect_timeout
        self.io_timeout = io_timeout

    def connect(self) -> socket.socket:
        sock = socket.socket(
            socket.AF_VSOCK,
            socket.SOCK_STREAM,
        )

        try:
            sock.settimeout(self.connect_timeout)
            sock.connect((self.cid, self.port))

            # Connection timeout and I/O timeout are intentionally separate.
            sock.settimeout(self.io_timeout)

            return sock

        except (TimeoutError, OSError) as exc:
            sock.close()
            raise AgentTransportError(
                f"failed to connect to Agent via vsock "
                f"(cid={self.cid}, port={self.port})"
            ) from exc

    def send_frame(self, sock: socket.socket, payload: bytes) -> None:
        size = len(payload)

        if size > self.max_message_size:
            raise AgentProtocolError(
                f"message too large: {size} bytes "
                f"(maximum {self.max_message_size})"
            )

        header = struct.pack("!I", size)

        try:
            sock.sendall(header)
            sock.sendall(payload)
        except (TimeoutError, OSError) as exc:
            raise AgentTransportError(
                "failed to send message to Agent"
            ) from exc

    def recv_frame(self, sock: socket.socket) -> bytes:
        header = self._recv_exact(sock, 4)

        (size,) = struct.unpack("!I", header)

        if size > self.max_message_size:
            raise AgentProtocolError(
                f"Agent response is too large: {size} bytes "
                f"(maximum {self.max_message_size})"
            )

        return self._recv_exact(sock, size)

    @staticmethod
    def _recv_exact(sock: socket.socket, size: int) -> bytes:
        data = bytearray()

        while len(data) < size:
            try:
                chunk = sock.recv(size - len(data))
            except TimeoutError as exc:
                raise AgentTransportError(
                    "timed out while receiving message from Agent"
                ) from exc
            except OSError as exc:
                raise AgentTransportError(
                    "failed while receiving message from Agent"
                ) from exc

            if not chunk:
                raise AgentTransportError(
                    "Agent closed the vsock connection unexpectedly"
                )

            data.extend(chunk)

        return bytes(data)


class AgentClient:
    """Client for the host Agent API over AF_VSOCK."""

    def __init__(
        self,
        transport: VsockTransport,
        token: str,
    ):
        self.transport = transport
        self.token = token

    def request(
        self,
        method: str,
        *,
        params: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        if not self.token:
            raise RuntimeError(
                "MVM_AGENT_TOKEN is not set"
            )

        request = {
            "method": method,
            "token": self.token,
            "params": params if params is not None else {},
        }

        try:
            payload = json.dumps(
                request,
                separators=(",", ":"),
                ensure_ascii=False,
            ).encode("utf-8")
        except (TypeError, ValueError) as exc:
            raise AgentProtocolError(
                "failed to encode Agent request as JSON"
            ) from exc

        with self.transport.connect() as sock:
            self.transport.send_frame(sock, payload)
            response_payload = self.transport.recv_frame(sock)

        try:
            response = json.loads(response_payload)
        except json.JSONDecodeError as exc:
            raise AgentProtocolError(
                "Agent returned invalid JSON"
            ) from exc

        if not isinstance(response, dict):
            raise AgentProtocolError(
                "Agent response must be a JSON object"
            )

        if "ok" not in response:
            raise AgentProtocolError(
                "Agent response is missing 'ok'"
            )

        ok = response["ok"]

        if not isinstance(ok, bool):
            raise AgentProtocolError(
                "Agent response field 'ok' must be a boolean"
            )

        if not ok:
            error = response.get(
                "error",
                "unknown Agent error",
            )

            raise RuntimeError(
                f"mvm agent API error: {error}"
            )

        result = response.get("result")

        if not isinstance(result, dict):
            raise AgentProtocolError(
                "Agent response result must be an object"
            )

        return result


transport = VsockTransport(
    cid=AGENT_CID,
    port=AGENT_PORT,
)

client = AgentClient(
    transport=transport,
    token=TOKEN,
)


@mcp.tool()
def inspect() -> dict:
    """Inspect the calling sandbox: its own mvm record."""
    return client.request("inspect")


@mcp.tool()
def stop() -> dict:
    """Stop the calling sandbox's VM."""
    return client.request("stop")


@mcp.tool()
def delegate(timeout: int, command: list[str]) -> dict:
    """Launch a child clone of this sandbox."""
    return client.request(
        "delegate",
        params={
            "timeout": timeout,
            "command": command,
        },
    )


def main() -> None:
    if not TOKEN:
        raise SystemExit(
            "mvm-agent-mcp: MVM_AGENT_TOKEN is not set"
        )

    mcp.run()


if __name__ == "__main__":
    main()
