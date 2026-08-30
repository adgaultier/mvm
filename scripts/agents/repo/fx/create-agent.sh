#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

MVM="$(pwd)/../../../../target/release/mvm"
SDBX_NAME=$1

source .env
: "${AI_GATEWAY_API_KEY:?AI_GATEWAY_API_KEY is not set in .env}"
"$MVM" create -it --net gvproxy \
  --name "$SDBX_NAME" \
  --cpus 1  -m 512 \
  -v "$(pwd)/conf:/home/agent/.fx:rw" \
  -v "workspace:$(pwd)/../../workspace:/home/agent/workspace:rw" \
  -v "$(pwd)/../../experiments:/home/agent/workspace/experiments:ro" \
  -e AI_GATEWAY_API_KEY=$AI_GATEWAY_API_KEY \
  fx-agent:latest fx -c

echo "$SDBX_NAME sdbx created with fx agent"

