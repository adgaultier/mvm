#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

MVM="$(pwd)/../../../../target/release/mvm"
SDBX_NAME=$1


"$MVM" create -it --net gvproxy \
  --name "$SDBX_NAME" \
  --cpus 2  -m 2048 \
  -v "$(pwd)/conf:/home/agent/.fx:rw" \
  -e AI_GATEWAY_API_KEY=$AI_GATEWAY_API_KEY \
  fx-agent:latest bash -c "fx ask 'register yourself' && fx -c"

echo "$SDBX_NAME sdbx spawned with fx agent"


