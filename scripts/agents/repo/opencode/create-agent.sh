#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

MVM="$(pwd)/../../../../target/release/mvm"
SDBX_NAME=$1


"$MVM" create -it --net gvproxy \
  --name "$SDBX_NAME" \
  --cpus 2  -m 2048 \
  -v "$(pwd)/conf:/home/agent/opencode:rw" \
  -v "$(pwd)/workspace:/home/agent/workspace:rw" \
  -v "$(pwd)/../../experiments:/home/agent/workspace/experiments:ro" \
  -e OPENCODE_CONFIG_DIR=/home/agent/opencode \
  opencode-agent:latest  opencode -c --prompt "hello again,agent" --port 4096 #--prompt "xxx" necessary to actually start a session (if first start)

echo "$SDBX_NAME sdbx created with opencode agent"

