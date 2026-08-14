#!/usr/bin/env bash
# Launch an opencode sandbox with an attached interactive terminal (-it) and
# TSI networking, so the guest can reach the host's Agent API on loopback.
#
# The opencode MCP config (opencode.json) and the mvm-agent-mcp bridge are
# bind-mounted into the guest's ~/.config/opencode, matching the layout the
# config expects ({env:HOME}/.config/opencode/mvm-agent-mcp). Override
# GUEST_HOME if the image's default user isn't root.
set -euo pipefail
cd "$(dirname "$0")/.."

GUEST_HOME="${GUEST_HOME:-/root}"
MVM=${1:-target/release/mvm}
"$MVM" create -it --net tsi \
  --name opencode-mcp \
  --cpus 2  -m 2048 \
   -e OPENCODE_CONFIG_DIR=/home/agent/opencode \
  -v "$(pwd)/scripts/agents/opencode:/home/agent/opencode:rw" \
  agent:latest opencode -c
"$MVM" start -a opencode-mcp
