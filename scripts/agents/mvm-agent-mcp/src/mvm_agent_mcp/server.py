"""MCP bridge exposing the mvm Agent API (`/agent/v1`) to an agent running
inside a microVM.

Every request is authenticated with the caller's VM-scoped bearer token
(`MVM_AGENT_TOKEN`), which the guest agent forwards into this process's
environment. The VM's identity is derived from the token, so the Agent API
routes carry no id — this bridge can only act on its own sandbox.

Run via `uvx mvm-agent-mcp` (stdio transport, as configured in
`scripts/agents/opencode.json`).
"""

import os

import httpx
from fastmcp import FastMCP

AGENT_ADDR = os.environ.get("MVM_AGENT_ADDR", "http://127.0.0.1:24643").rstrip("/")
TOKEN = os.environ.get("MVM_AGENT_TOKEN", "")

mcp = FastMCP("mvm-agent")


def _request(method: str, path: str, *, json: dict | None = None) -> dict:
    if not TOKEN:
        raise RuntimeError(
            "MVM_AGENT_TOKEN is not set — run this bridge inside the guest, "
            "where the agent forwards its token"
        )
    resp = httpx.request(
        method,
        f"{AGENT_ADDR}{path}",
        headers={"Authorization": f"Bearer {TOKEN}"},
        json=json,
        timeout=30.0,
    )
    if resp.status_code >= 400:
        raise RuntimeError(f"mvm agent API {resp.status_code}: {resp.text.strip()}")
    return resp.json()


@mcp.tool()
def inspect() -> dict:
    """Inspect the calling sandbox: its own mvm record (id, spec, state, ...)."""
    return _request("GET", "/agent/v1/sandbox")


@mcp.tool()
def stop() -> dict:
    """Stop the calling sandbox's VM."""
    return _request("POST", "/agent/v1/sandbox/stop")


@mcp.tool()
def delegate(timeout: int, command: list[str]) -> dict:
    """Launch a child clone of this sandbox, bounded by `timeout` seconds.

    Not yet implemented by mvm: the Agent API authenticates and authorizes the
    call, then reports that delegation is still in progress.
    """
    return _request(
        "POST",
        "/agent/v1/sandbox/delegate",
        json={"timeout": timeout, "command": command},
    )


def main() -> None:
    if not TOKEN:
        raise SystemExit(
            "mvm-agent-mcp: MVM_AGENT_TOKEN is not set "
            "(run inside the guest where the agent forwards it)"
        )
    mcp.run()


if __name__ == "__main__":
    main()
