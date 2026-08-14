#!/usr/bin/env bash
set -euo pipefail
echo "== agent API (VM-scoped bearer token) =="
if [ "$AGENT_API" = 1 ]; then
AGENT_HOST="http://127.0.0.1:$AGENT_PORT"

agent_token_of() { # agent_token_of <name> -> token (read from the guest /proc/cmdline)
    "$MVM" exec "$1" sh -c 'cat /proc/cmdline' \
        | grep -o 'MVM_AGENT_TOKEN=[^ ]*' | head -n1 | cut -d= -f2- | tr -d '"\r'
}

wait_agent() { # wait_agent <name>
    for _ in $(seq 1 100); do "$MVM" exec "$1" true >/dev/null 2>&1 && break; sleep 0.2; done
}

# Two live sandboxes to prove the token is VM-scoped.
SB_A=$("$MVM" create --name agent-a alpine sleep infinity 2>/dev/null)
SB_B=$("$MVM" create --name agent-b alpine sleep infinity 2>/dev/null)
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

# The vm id is derived from the token, so the paths carry no {id}.
check "agent API rejects missing token" "401" \
    "$(curl -s -o /dev/null -w '%{http_code}' "$AGENT_HOST/agent/v1/sandbox")"
check "agent API rejects invalid token" "401" \
    "$(curl -s -o /dev/null -w '%{http_code}' -H 'Authorization: Bearer deadbeef' "$AGENT_HOST/agent/v1/sandbox")"

# Valid token -> access granted, and it resolves to exactly that VM: A's token
# inspects A (never B), B's token inspects B.
check "agent API inspect self (A)" "1" \
    "$(curl -s -H "Authorization: Bearer $TOKEN_A" "$AGENT_HOST/agent/v1/sandbox" | grep -c "\"id\":\"$SB_A\"")"
check "agent API inspect self (B)" "1" \
    "$(curl -s -H "Authorization: Bearer $TOKEN_B" "$AGENT_HOST/agent/v1/sandbox" | grep -c "\"id\":\"$SB_B\"")"
check "A's token cannot access B" "0" \
    "$(curl -s -H "Authorization: Bearer $TOKEN_A" "$AGENT_HOST/agent/v1/sandbox" | grep -c "\"id\":\"$SB_B\"")"

# The token hash is manager-internal: it must never surface in an API response.
check "control plane hides the token hash" "0" \
    "$(curl -s "$MVM_HOST/api/v1/sandboxes/$SB_A" | grep -c 'agent_token_hash')"
check "agent API hides the token hash" "0" \
    "$(curl -s -H "Authorization: Bearer $TOKEN_A" "$AGENT_HOST/agent/v1/sandbox" | grep -c 'agent_token_hash')"

check "agent API delegate not implemented" "501" \
    "$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "Authorization: Bearer $TOKEN_A" -H 'Content-Type: application/json' -d '{"timeout":1,"command":["true"]}' "$AGENT_HOST/agent/v1/sandbox/delegate")"
# The agent surface is not reachable through the control-plane listener.
check "agent API not on control port" "404" \
    "$(curl -s -o /dev/null -w '%{http_code}' "$MVM_HOST/agent/v1/sandbox")"

# Stopping a VM revokes its token...
check "agent API stop self" "200" \
    "$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "Authorization: Bearer $TOKEN_A" "$AGENT_HOST/agent/v1/sandbox/stop")"
for _ in $(seq 1 100); do "$MVM" ps | grep -q agent-a || break; sleep 0.1; done
check "agent token revoked after stop" "401" \
    "$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $TOKEN_A" "$AGENT_HOST/agent/v1/sandbox")"

# ...and restarting a VM invalidates the previous token: the old one is dead
# while the boot mints a fresh one.
"$MVM" start agent-a >/dev/null 2>&1
wait_agent agent-a
check "old token invalid after restart" "401" \
    "$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $TOKEN_A" "$AGENT_HOST/agent/v1/sandbox")"
TOKEN_A2=$(agent_token_of agent-a)
check "restart mints a fresh token" "1" "$([ "$TOKEN_A2" != "$TOKEN_A" ] && echo 1)"
check "fresh token works after restart" "1" \
    "$(curl -s -H "Authorization: Bearer $TOKEN_A2" "$AGENT_HOST/agent/v1/sandbox" | grep -c "\"id\":\"$SB_A\"")"

"$MVM" rm -f agent-a agent-b >/dev/null 2>&1 || true

else
    skip "agent API (mvm built without the agent-api feature)"
fi

echo
echo "$PASS passed, $SKIP skipped, $FAIL failed"
[ "$FAIL" -eq 0 ]
