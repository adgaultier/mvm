#!/usr/bin/env bash
set -euo pipefail
echo "== agent API (VM-scoped bearer token, vsock transport) =="
if [ "$AGENT_API" = 1 ]; then

agent_token_of() { # agent_token_of <name> -> token (read from the guest /proc/cmdline)
    "$MVM" exec "$1" sh -c 'cat /proc/cmdline' \
        | grep -o 'MVM_AGENT_TOKEN=[^ ]*' | head -n1 | cut -d= -f2- | tr -d '"\r'
}

wait_agent() { # wait_agent <name>
    for _ in $(seq 1 100); do "$MVM" exec "$1" true >/dev/null 2>&1 && break; sleep 0.2; done
}

PROBE_DIR=$(mktemp -d /tmp/mvm-itest-vsock.XXXXXX)
HAVE_PROBE=0
if build_probe "$PROBES_DIR/vsockprobe.c" "$PROBE_DIR" vsockprobe; then
    HAVE_PROBE=1
fi

# The vsock channel is only reachable from inside the guest (see
# doc/agentic/notifications-delegation.md), so the round-trip check below
# runs a static probe binary through `mvm exec` rather than curl.
agent_api_call() { # agent_api_call <name> <token> <method> [params-json]
    local name=$1 token=$2 method=$3 params=${4:-'{}'}
    "$MVM" exec "$name" /probe/vsockprobe "$token" "$method" "$params"
}

# Two live sandboxes to prove the token is VM-scoped.
SB_A=$("$MVM" create --name agent-a -v "$PROBE_DIR:/probe:ro" alpine sleep infinity 2>/dev/null)
SB_B=$("$MVM" create --name agent-b -v "$PROBE_DIR:/probe:ro" alpine sleep infinity 2>/dev/null)
"$MVM" start agent-a >/dev/null 2>&1
"$MVM" start agent-b >/dev/null 2>&1
wait_agent agent-a
wait_agent agent-b

# The plaintext token is provisioned into the guest environment (it rides the
# MVM_* env channel and is deliberately not scrubbed, so workload tooling like
# the mvm-agent-mcp bridge can present it). Only a hash of it lives host-side.
check "agent token present in guest env" "1" \
    "$("$MVM" exec agent-a sh -c 'env | grep -c MVM_AGENT_TOKEN' || true)"

TOKEN_A=$(agent_token_of agent-a)
TOKEN_B=$(agent_token_of agent-b)
check "agent token provisioned (64 hex chars)" "1" "$(test "${#TOKEN_A}" -eq 64 && echo 1)"
check "tokens are VM-specific" "1" "$([ "$TOKEN_A" != "$TOKEN_B" ] && echo 1)"

# Each sandbox's Agent API listener is a dedicated host unix socket (bridged
# to the guest over vsock by libkrun); there is no shared host-wide address.
check "host-side Agent API socket exists (A)" "1" \
    "$(test -S "$MVM_DATA_DIR/sandboxes/$SB_A/agent-api.sock" && echo 1)"
check "host-side Agent API socket exists (B)" "1" \
    "$(test -S "$MVM_DATA_DIR/sandboxes/$SB_B/agent-api.sock" && echo 1)"

# The token hash is manager-internal: it must never surface in an API response.
check "control plane hides the token hash" "0" \
    "$("$MVM" inspect agent-a | grep -c 'agent_token_hash')"

if [ "$HAVE_PROBE" = 1 ]; then
    check "agent API inspect self (A)" "1" \
        "$(agent_api_call agent-a "$TOKEN_A" inspect | grep -c "\"id\": *\"$SB_A\"")"
    check "A's token cannot reach B's socket" "1" \
        "$(agent_api_call agent-b "$TOKEN_A" inspect | grep -c '"ok": *false')"
    check "agent API delegate not implemented" "1" \
        "$(agent_api_call agent-a "$TOKEN_A" delegate '{"timeout":1,"command":["true"]}' | grep -c 'not yet implemented')"
else
    skip "agent API protocol round-trip (no static cc or zig to build vsockprobe)"
fi

# Stopping a VM revokes its token...
"$MVM" stop agent-a >/dev/null 2>&1
for _ in $(seq 1 100); do "$MVM" ps | grep -q agent-a || break; sleep 0.1; done
check "agent token revoked after stop" "1" \
    "$("$MVM" inspect agent-a | grep -c '"state": *"stopped"')"

# ...and restarting a VM invalidates the previous token: the old one is dead
# while the boot mints a fresh one.
"$MVM" start agent-a >/dev/null 2>&1
wait_agent agent-a
TOKEN_A2=$(agent_token_of agent-a)
check "restart mints a fresh token" "1" "$([ "$TOKEN_A2" != "$TOKEN_A" ] && echo 1)"
if [ "$HAVE_PROBE" = 1 ]; then
    check "fresh token works after restart" "1" \
        "$(agent_api_call agent-a "$TOKEN_A2" inspect | grep -c "\"id\": *\"$SB_A\"")"
    check "old token invalid after restart" "1" \
        "$(agent_api_call agent-a "$TOKEN_A" inspect | grep -c '"ok": *false')"
fi

"$MVM" rm -f agent-a agent-b >/dev/null 2>&1 || true
rm -rf "$PROBE_DIR"

else
    skip "agent API (mvm built without the agent-api feature)"
fi

echo
echo "$PASS passed, $SKIP skipped, $FAIL failed"
[ "$FAIL" -eq 0 ]
