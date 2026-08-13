#!/usr/bin/env bash
# Launch an opencode sandbox with an attached interactive terminal (-it) and
# TSI networking, so the guest can reach the host's Agent API on loopback.
#
# The opencode MCP config (opencode.json) and the mvm-agent-mcp bridge are
# bind-mounted into the guest's ~/.config/opencode, matching the layout the
# config expects ({env:HOME}/.config/opencode/mvm-agent-mcp).
#
# The host path MUST be absolute: libkrun's virtiofs opens it from the daemon's
# working directory, so a relative path fails with ENOENT at VM boot.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# Run as root: its HOME is /root, and guest root writes anywhere on the
# copy-driver rootfs (sidesteps the macOS home-ownership issue).
GUEST_HOME="${GUEST_HOME:-/root}"

mvm run -it --net tsi -u root \
  -v "$SCRIPT_DIR:$GUEST_HOME/.config/opencode:ro" \
  opencode
