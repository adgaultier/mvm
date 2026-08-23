#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

MVM="$(pwd)/../../../../target/release/mvm"
SDBX_NAME=$1


"$MVM" create -it --net gvproxy \
  --name "$SDBX_NAME" \
  --cpus 2  -m 2048 \
  -v "$(pwd)/conf:/home/agent/opencode:rw" \
  -v "$(pwd)/../../SKILLS:/home/agent/skills:ro" \
  -e OPENCODE_CONFIG_DIR=/home/agent/opencode \
  opencode-agent:latest  opencode -c --prompt "hello" --port 4096 #--prompt "xxx" necessary to actually start a session (if first start)

echo "$SDBX_NAME sdbx created with opencode agent"

