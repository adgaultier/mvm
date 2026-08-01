#!/usr/bin/env bash
# End-to-end integration test: boots real microVMs.
#
# Requirements: /dev/kvm (rw), libkrun + libkrunfw installed, network access
# to docker.io. Runs against an isolated data dir and port, so it never
# touches your normal mvm state.
#
# Usage: scripts/integration.sh [path-to-mvm] [path-to-static-mvm-agent]
set -euo pipefail

cd "$(dirname "$0")/.."

MVM=${1:-target/debug/mvm}
AGENT=${2:-target/x86_64-unknown-linux-musl/release/mvm-agent}
PORT=24699
export MVM_HOST="http://127.0.0.1:$PORT"
export MVM_DATA_DIR=$(mktemp -d /tmp/mvm-itest.XXXXXX)
export MVM_AGENT_PATH="$PWD/$AGENT"

[ -e /dev/kvm ] || { echo "SKIP: /dev/kvm not available"; exit 0; }
[ -x "$MVM" ] || { echo "FAIL: $MVM not built (run scripts/build.sh or cargo build)"; exit 1; }
[ -f "$AGENT" ] || { echo "FAIL: static agent $AGENT not built"; exit 1; }

PASS=0
FAIL=0
DAEMON_PID=

cleanup() {
    [ -n "$DAEMON_PID" ] && kill "$DAEMON_PID" 2>/dev/null || true
    rm -rf "$MVM_DATA_DIR"
}
trap cleanup EXIT

check() { # check <name> <expected> <actual>
    if [ "$2" = "$3" ]; then
        echo "ok   $1"
        PASS=$((PASS + 1))
    else
        echo "FAIL $1: expected '$2', got '$3'"
        FAIL=$((FAIL + 1))
    fi
}

echo "==> starting daemon (data dir $MVM_DATA_DIR, port $PORT)"
"$MVM" serve --addr "127.0.0.1:$PORT" &
DAEMON_PID=$!
for _ in $(seq 1 50); do
    curl -sf "$MVM_HOST/health" >/dev/null 2>&1 && break
    sleep 0.1
done
check "daemon health" "ok" "$(curl -sf "$MVM_HOST/health")"

echo "==> pull"
"$MVM" pull alpine >/dev/null
check "image listed" "1" "$("$MVM" images | grep -c alpine)"

echo "==> run"
check "run stdout" "hello-vm" "$("$MVM" run alpine echo hello-vm)"

set +e
"$MVM" run alpine sh -c 'exit 7' >/dev/null 2>&1
check "run exit code" "7" "$?"
set -e

echo "==> exec"
"$MVM" run --name itest --keep alpine sleep 60 >/dev/null &
RUN_PID=$!
# Wait for the guest agent to come up ("running" state precedes the
# agent's vsock connection by a moment).
for _ in $(seq 1 100); do
    "$MVM" exec itest true >/dev/null 2>&1 && break
    sleep 0.2
done
check "sandbox running" "1" "$("$MVM" ps | grep -c itest)"
check "exec stdout" "in-vm" "$("$MVM" exec itest echo in-vm)"
set +e
"$MVM" exec itest sh -c 'exit 3' >/dev/null 2>&1
check "exec exit code" "3" "$?"
set -e
check "exec -i stdin" "roundtrip" "$(printf 'roundtrip' | "$MVM" exec -i itest cat)"

echo "==> volumes"
VOLDIR=$(mktemp -d /tmp/mvm-itest-vol.XXXXXX)
echo vol-data > "$VOLDIR/f.txt"
check "volume mount" "vol-data" "$("$MVM" run alpine -v "$VOLDIR:/data" cat /data/f.txt)"
rm -rf "$VOLDIR"

echo "==> lifecycle"
"$MVM" stop itest >/dev/null
wait "$RUN_PID" 2>/dev/null || true
check "stopped state" "1" "$("$MVM" ps -a | grep itest | grep -c stopped)"
"$MVM" rm itest >/dev/null
check "removed" "0" "$("$MVM" ps -a | grep -c itest || true)"

echo "==> logs"
"$MVM" run --name logtest --keep alpine sh -c 'echo l1; echo l2' >/dev/null
check "logs" "l1 l2" "$("$MVM" logs logtest | tr '\n' ' ' | sed 's/ $//')"
"$MVM" rm logtest >/dev/null

echo
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
