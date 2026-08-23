#!/usr/bin/env bash
set -euo pipefail
echo "== agent API (VM-scoped bearer token, vsock transport) =="
if [ "$AGENT_API" = 1 ]; then

guest_token_of() { # guest_token_of <name> -> token (read from the guest /proc/cmdline)
    "$MVM" exec "$1" sh -c 'cat /proc/cmdline' \
        | grep -o 'MVM_GUEST_TOKEN=[^ ]*' | head -n1 | cut -d= -f2- | tr -d '"\r'
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
# doc/agentic/notes.md), so the round-trip check below
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
    "$("$MVM" exec agent-a sh -c 'env | grep -c MVM_GUEST_TOKEN' || true)"

TOKEN_A=$(guest_token_of agent-a)
TOKEN_B=$(guest_token_of agent-b)
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
    "$("$MVM" inspect agent-a | grep -c 'guest_token_hash')"

if [ "$HAVE_PROBE" = 1 ]; then
    check "agent API inspect self (A)" "1" \
        "$(agent_api_call agent-a "$TOKEN_A" inspect | grep -c "\"id\": *\"$SB_A\"")"
    check "agent API inspect hides control-plane fields" "0" \
        "$(agent_api_call agent-a "$TOKEN_A" inspect | grep -c -E '"(mounts|ports|env|pid|gvproxy_pid|lifecycle|spec|guest_token_hash)"')"
    check "A's token cannot reach B's socket" "1" \
        "$(agent_api_call agent-b "$TOKEN_A" inspect | grep -c '"ok": *false')"
    check "agent API set_notification_command registers template" "1" \
        "$(agent_api_call agent-a "$TOKEN_A" set_notification_command '{"command":"echo '\''<MSG>'\'' >> /tmp/notifs.log"}' | grep -c '"ok": *true')"
    check "notification command surfaces in mvm inspect" "1" \
        "$("$MVM" inspect agent-a | grep -c '"notification_command": *"echo '"'"'<MSG>'"'"' >> /tmp/notifs.log"')"
    check "notification command is sandbox-scoped (B unaffected)" "0" \
        "$("$MVM" inspect agent-b | grep -c 'notification_command')"

    # The real control-plane delivery path: the agent asks the control plane
    # (via the Agent API) to fire one mock notification of every kind, and the
    # control plane delivers each through the registered command — a
    # full-loop test of the async-notification mechanism.
    PROBE_RESP=$(agent_api_call agent-a "$TOKEN_A" test_notification)
    # The response is one compact JSON line: the outer `{"ok":true,"result":[...]}`
    # envelope plus a 6-element delivery array. Count only the deliveries whose
    # `kind` is followed by `"ok":true` (skipping the envelope's own ok field).
    check "control plane fired all 6 mock notification kinds" "6" \
        "$(printf '%s\n' "$PROBE_RESP" | grep -o '"kind": *"[^"]*", *"ok": *true' | wc -l | tr -d ' ')"
    NOTIF_LOG=$("$MVM" exec agent-a cat /tmp/notifs.log 2>/dev/null || true)
    MISSING=0
    # Notifications are delivered as human-readable text, one line per kind.
    for marker in "about to hit its TTL" "restarted after an idle stop" "is requesting input" "finished (exit code" "was terminated" "Daddy is requesting"; do
        printf '%s\n' "$NOTIF_LOG" | grep -q "$marker" || MISSING=$((MISSING + 1))
    done
    check "mock notifications reached the agent's endpoint" "0" "$MISSING"

    # Delegation: the parent can never set the child's command — it supplies a
    # message, and the control plane boots an interactive CLONE of the caller
    # with the message queued as a Daddy notification, delivered through the
    # child's own registered notification command once the child declares ready.
    DELEGATE_RESP=$(agent_api_call agent-a "$TOKEN_A" delegate '{"timeout":60,"message":"integration test task"}')
    check "agent API delegate creates a child" "1" \
        "$(printf '%s\n' "$DELEGATE_RESP" | grep -c '"ok": *true')"
    CHILD_ID=$(printf '%s\n' "$DELEGATE_RESP" | sed -n 's/.*"id": *"\([^"]*\)".*/\1/p')
    check "delegate response names the child" "1" "$(test -n "$CHILD_ID" && echo 1)"
    check "delegate child boots the parent's own workload" "1" \
        "$("$MVM" inspect "$CHILD_ID" | tr -d ' \n' | grep -c '"command":\["sleep","infinity"\]')"
    check "delegate message queued on the child" "1" \
        "$("$MVM" inspect "$CHILD_ID" | grep -c 'integration test task')"
    AGENTS_JSON=$(curl -sf "$MVM_HOST/api/v1/agents")
    check "delegate child links back to its parent" "1" \
        "$(printf '%s\n' "$AGENTS_JSON" | grep -c "\"id\":\"$CHILD_ID\"[^}]*\"parent\":\"$SB_A\"")"
    check "delegate timeout becomes the child's TTL" "1" \
        "$(printf '%s\n' "$AGENTS_JSON" | grep -c "\"id\":\"$CHILD_ID\"[^}]*\"ttl_deadline\":\"")"
    check "queued notification surfaces in /agents (mailbox data)" "1" \
        "$(printf '%s\n' "$AGENTS_JSON" | grep -c '"pending_notifications":\[{')"

    # The queued task is only delivered once the child is ready AND has
    # registered its notification command (the mvm-agent-mcp bridge does both
    # at boot; here the probe stands in for the bridge, in that order).
    wait_agent "$CHILD_ID"
    TOKEN_CHILD=$(guest_token_of "$CHILD_ID")
    check "delegate child can declare ready" "1" \
        "$(agent_api_call "$CHILD_ID" "$TOKEN_CHILD" ready | grep -c '"ok": *true')"
    check "ready child registers its notification command" "1" \
        "$(agent_api_call "$CHILD_ID" "$TOKEN_CHILD" set_notification_command '{"command":"echo '\''<MSG>'\'' >> /tmp/deleg.log"}' | grep -c '"ok": *true')"
    DELEG_LOG=
    for _ in $(seq 1 50); do
        DELEG_LOG=$("$MVM" exec "$CHILD_ID" cat /tmp/deleg.log 2>/dev/null || true)
        printf '%s\n' "$DELEG_LOG" | grep -q 'integration test task' && break
        sleep 0.2
    done
    check "daddy task delivered to the child once ready" "1" \
        "$(printf '%s\n' "$DELEG_LOG" | grep -c 'integration test task')"
    check "daddy task arrived as a daddy notification" "1" \
        "$(printf '%s\n' "$DELEG_LOG" | grep -c 'Daddy is requesting:')"

    # A delegation without a message is refused outright.
    check "delegate with empty message refused" "1" \
        "$(agent_api_call agent-b "$TOKEN_B" delegate '{"timeout":0,"message":"  "}' | grep -c '"ok": *false')"
else
    skip "agent API protocol round-trip (no static cc or zig to build vsockprobe)"
fi

# Stopping a VM revokes its token...
"$MVM" stop agent-a >/dev/null 2>&1
for _ in $(seq 1 100); do "$MVM" ps | grep -q agent-a || break; sleep 0.1; done
check "guest token revoked after stop" "1" \
    "$("$MVM" inspect agent-a | grep -c '"state": *"stopped"')"

# ...and restarting a VM invalidates the previous token: the old one is dead
# while the boot mints a fresh one.
"$MVM" start agent-a >/dev/null 2>&1
wait_agent agent-a
TOKEN_A2=$(guest_token_of agent-a)
check "restart mints a fresh token" "1" "$([ "$TOKEN_A2" != "$TOKEN_A" ] && echo 1)"
if [ "$HAVE_PROBE" = 1 ]; then
    check "fresh token works after restart" "1" \
        "$(agent_api_call agent-a "$TOKEN_A2" inspect | grep -c "\"id\": *\"$SB_A\"")"
    check "old token invalid after restart" "1" \
        "$(agent_api_call agent-a "$TOKEN_A" inspect | grep -c '"ok": *false')"
fi

"$MVM" rm -f agent-a agent-b ${CHILD_ID:-} >/dev/null 2>&1 || true
rm -rf "$PROBE_DIR"

else
    skip "agent API (mvm built without the agent-api feature)"
fi

echo
echo "$PASS passed, $SKIP skipped, $FAIL failed"
[ "$FAIL" -eq 0 ]
