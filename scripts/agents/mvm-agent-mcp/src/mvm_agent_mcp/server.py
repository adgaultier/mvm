"""MCP bridge exposing the mvm Agent API to an agent running inside a microVM.

Guest -> host communication uses AF_VSOCK directly.

Wire protocol:

    uint32_be payload_length
    uint8[payload_length] UTF-8 JSON

Each connection carries exactly one request and one response.

Configuration:

    MVM_AGENT_CID=2
    MVM_AGENT_PORT=24643
    MVM_GUEST_TOKEN=<vm-scoped-token>
    NOTIFICATION_CMD=<shell command template, `<MSG>` = notification text>

At boot, `NOTIFICATION_CMD` is registered with the control plane over the
Agent API (`set_notification_command`) so the host can deliver async
notifications later. If it is unset or empty, nothing is registered.

Delegation needs no template: `delegate` boots an interactive clone of the
calling agent (same image/workload/env), and the delegation message is queued
on the child as a Daddy notification, delivered through the child's own
registered `NOTIFICATION_CMD` once the child declares `ready`.
"""

from __future__ import annotations

import json
import logging
import os
import socket
import struct
from typing import Any

from mcp.server import MCPServer

AGENT_CID = int(os.environ.get("MVM_AGENT_CID", "2"))
AGENT_PORT = int(os.environ.get("MVM_AGENT_PORT", "24643"))
TOKEN = os.environ.get("MVM_GUEST_TOKEN", "")
NOTIFICATION_CMD = os.environ.get("NOTIFICATION_CMD", "")

MAX_MESSAGE_SIZE = 4 * 1024 * 1024

CONNECT_TIMEOUT = 5.0
IO_TIMEOUT = 30.0

logger = logging.getLogger("mvm-agent-mcp")

mcp = MCPServer("mvm-agent")


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
                f"message too large: {size} bytes (maximum {self.max_message_size})"
            )

        header = struct.pack("!I", size)

        try:
            sock.sendall(header)
            sock.sendall(payload)
        except (TimeoutError, OSError) as exc:
            raise AgentTransportError("failed to send message to Agent") from exc

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
            raise RuntimeError("MVM_GUEST_TOKEN is not set")

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
            raise AgentProtocolError("failed to encode Agent request as JSON") from exc

        with self.transport.connect() as sock:
            self.transport.send_frame(sock, payload)
            response_payload = self.transport.recv_frame(sock)

        try:
            response = json.loads(response_payload)
        except json.JSONDecodeError as exc:
            raise AgentProtocolError("Agent returned invalid JSON") from exc

        if not isinstance(response, dict):
            raise AgentProtocolError("Agent response must be a JSON object")

        if "ok" not in response:
            raise AgentProtocolError("Agent response is missing 'ok'")

        ok = response["ok"]

        if not isinstance(ok, bool):
            raise AgentProtocolError("Agent response field 'ok' must be a boolean")

        if not ok:
            error = response.get(
                "error",
                "unknown Agent error",
            )

            raise RuntimeError(f"mvm agent API error: {error}")

        result = response.get("result")

        # The result is a JSON value, not necessarily an object: `inspect` and
        # `stop` return the sandbox record, but `test_notification` returns an
        # array of per-kind delivery reports.
        if not isinstance(result, (dict, list)):
            raise AgentProtocolError(
                "Agent response result must be a JSON object or array"
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
def inspect() -> dict[str, Any]:
    """Inspect the calling sandbox: its own mvm record."""
    return client.request("inspect")


@mcp.tool()
def stop() -> dict[str, Any]:
    """Stop the calling sandbox's VM."""
    return client.request("stop")


@mcp.tool()
def delegate(timeout: int, message: str) -> dict[str, Any]:
    """Launch an interactive clone of this sandbox to work on `message`. The
    child boots with your own workload; the message is queued on it as a Daddy
    notification and delivered through its own NOTIFICATION_CMD once it
    declares ready — you supply the task, never the child's command."""
    return client.request(
        "delegate",
        params={
            "timeout": timeout,
            "message": message,
        },
    )


@mcp.tool()
def test_notification() -> dict[str, Any]:
    """Ask the control plane to fire one mock notification of every kind at
    this agent, through the real delivery path (its registered
    NOTIFICATION_CMD, `<MSG>` substituted). Returns a per-kind report
    (kind/ok/exit_code/output/error) — a good end-to-end check of a fresh
    agent's notification wiring."""
    return {"notifications": client.request("test_notification")}


def _notify_agent_ready():
    try:
        client.request("ready")
        logger.info("registered readiness with control plane")
    except Exception as exc:  # noqa: BLE001
        logger.error("failed to register agent readiness: %s", exc)


def _register_notification_command() -> None:
    """Register the `NOTIFICATION_CMD` template with the control plane so it
    can deliver async notifications to this agent (`<MSG>` is substituted with
    the notification's human-readable text at delivery time). Best-effort: an unset
    or empty variable is a no-op, and a registration failure only warns —
    the MCP server still boots."""
    if not NOTIFICATION_CMD:
        return

    try:
        client.request(
            "set_notification_command",
            params={
                "command": NOTIFICATION_CMD,
            },
        )
        logger.info("registered notification command with control plane")
    except Exception as exc:  # noqa: BLE001
        logger.error("failed to register notification command: %s", exc)


def main() -> None:
    if not TOKEN:
        raise SystemExit("mvm-agent-mcp: MVM_GUEST_TOKEN is not set")
    _register_notification_command()
    _notify_agent_ready()
    mcp.run()


if __name__ == "__main__":
    main()
