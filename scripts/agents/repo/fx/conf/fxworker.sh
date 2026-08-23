#!/usr/bin/env bash
set -u

FIFO="$HOME/.fx/fxq.fifo"
SESSION_ID="${FX_SESSION_ID:-}"

[[ -p "$FIFO" ]] || mkfifo "$FIFO"

TMP=$(mktemp -d)
trap 'kill "$FX_PID" 2>/dev/null; rm -rf "$TMP"' EXIT

mkfifo "$TMP/in" "$TMP/out"


fx acp --log-file /tmp/acp-server.log <"$TMP/in" >"$TMP/out" 2>/tmp/acp-worker.log &
FX_PID=$!

exec 3>"$TMP/in"
exec 4<"$TMP/out"

send() {
    printf '%s\n' "$1" >&3
}

# ------------------------------------------------------------
# Initialize ACP
# ------------------------------------------------------------

send '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientInfo":{"name":"fxq-worker","version":"0.1.0"},"capabilities":{}}}'

while IFS= read -r line <&4; do
    [[ "$line" == *'"id":1'* ]] && break
done

# ------------------------------------------------------------
# Find latest session
# ------------------------------------------------------------

if [[ -z "$SESSION_ID" ]]; then
    send '{"jsonrpc":"2.0","id":2,"method":"session/list","params":{}}'

    while IFS= read -r line <&4; do
        if [[ "$line" == *'"id":2'* ]]; then
            SESSION_ID=$(
                printf '%s\n' "$line" |
                sed -n 's/.*"sessionId":"\([^"]*\)".*/\1/p'
            )
            break
        fi
    done
fi

if [[ -z "$SESSION_ID" ]]; then
    echo "ERROR: no fx session found" >&2
    exit 1
fi

echo "Using fx session: $SESSION_ID" >&2

# ------------------------------------------------------------
# Resume session
# ------------------------------------------------------------
MCP_CFG="{\"name\":\"mvm-agent\",\"command\":\"uvx\",\"args\":[\"mvm-agent-mcp\"],\"env\":[{\"name\":\"NOTIFICATION_CMD\",\"value\":\"\"},{\"name\":\"MVM_GUEST_TOKEN\",\"value\":\"$MVM_GUEST_TOKEN\"}]}"
#send "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session/resume\",\"params\":{\"sessionId\":\"$SESSION_ID\",\"mcpServers\":[$MCP_CFG]}}"

send "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session/resume\",\"params\":{\"sessionId\":\"$SESSION_ID\"}}"

while IFS= read -r line <&4; do
    [[ "$line" == *'"id":3'* ]] && break
done

echo "Worker ready." >&2
echo "Waiting for notifications..." >&2
# ------------------------------------------------------------
# Stream conversation to STDOUT
# ------------------------------------------------------------

LOG="$HOME/.fx/sessions/$SESSION_ID/events.jsonl"

{
    cat "$LOG"
    tail -F "$LOG"
} | jq -r '
    select(.kind == "history_turn_committed")
    | "USER: " + (.payload.turn.user.text // "")
    + "\nASSISTANT: " + (.payload.turn.assistant // "")
    + "\n---"
' &

LOG_PID=$!
# ------------------------------------------------------------
# Send one notification to fx and wait internally
# ------------------------------------------------------------

prompt() {
    local text="$1"
    local id="$2"
    local line
    local text_json

    echo ">>> $text" >&2

    if command -v jq >/dev/null 2>&1; then
        text_json=$(printf '%s' "$text" | jq -Rs .)
    else
        text_json="\"${text//\\/\\\\}\""
        text_json="${text_json//\"/\\\"}"
    fi

    send "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"session/prompt\",\"params\":{\"sessionId\":\"$SESSION_ID\",\"prompt\":[{\"type\":\"text\",\"text\":$text_json}]}}"

    while IFS= read -r line <&4; do
        if [[ "$line" == *"\"id\":$id"* ]]; then
            echo "<<< fx finished notification $id" >&2
            return 0
        fi
    done

    return 1
}

# ------------------------------------------------------------
# Persistent FIFO consumer
# ------------------------------------------------------------

exec 5<> "$FIFO"

id=100

while IFS= read -r -u 5 notification; do
    [[ -z "$notification" ]] && continue

    prompt "$notification" "$id" || {
        echo "ERROR: ACP prompt failed" >&2
        exit 1
    }

    ((id++))
done
