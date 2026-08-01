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
check "re-pull up to date" "1" "$("$MVM" pull alpine | grep -c 'up to date')"

echo "==> run"
check "run stdout" "hello-vm" "$("$MVM" run alpine echo hello-vm)"

set +e
"$MVM" run alpine sh -c 'exit 7' >/dev/null 2>&1
check "run exit code" "7" "$?"
set -e

# run -i attaches local stdin to the guest console.
check "run -i stdin" "found" \
    "$(printf 'from-stdin\n' | timeout 60 "$MVM" run -i alpine cat | tr -d '\r' | grep -q from-stdin && echo found)"

# The guest console is a tty for the workload (independent of client -t).
check "run console is a tty" "CONSOLE-TTY" \
    "$("$MVM" run alpine sh -c '[ -t 0 ] && echo CONSOLE-TTY' | tr -d '\r')"

# run -it end to end: wrap the client in a real pty via script(1) so the
# raw-mode path (termios guard enable/restore) actually engages.
if command -v script >/dev/null 2>&1; then
    check "run -it raw mode" "found" \
        "$(printf 'exit\n' | timeout 60 script -qec "$MVM run -it alpine sh -c 'echo RAW-OK'" /dev/null | grep -q RAW-OK && echo found)"
else
    echo "skip: script(1) not available (run -it raw-mode check)"
fi

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

# Binary safety: random bytes must survive the exec stdin/stdout path intact.
BINFILE=$(mktemp /tmp/mvm-itest-bin.XXXXXX)
head -c 65536 /dev/urandom > "$BINFILE"
check "exec binary roundtrip" \
    "$(sha256sum < "$BINFILE" | cut -d' ' -f1)" \
    "$("$MVM" exec -i itest cat < "$BINFILE" | sha256sum | cut -d' ' -f1)"
rm -f "$BINFILE"

# -t allocates a pty: the guest command must see a terminal on stdin.
check "exec tty allocation" "TTY-OK" \
    "$("$MVM" exec -t itest sh -c '[ -t 0 ] && echo TTY-OK' | tr -d '\r')"

# rmi must refuse while a sandbox references the image.
set +e
"$MVM" rmi alpine >/dev/null 2>&1
RMI_RC=$?
set -e
check "rmi refused while in use" "1" "$RMI_RC"
check "image survived rmi attempt" "1" "$("$MVM" images | grep -c alpine)"

# Killing the client mid-exec must not orphan the guest process.
"$MVM" exec itest sleep 299 >/dev/null 2>&1 &
EXEC_PID=$!
sleep 1
kill "$EXEC_PID" 2>/dev/null || true
wait "$EXEC_PID" 2>/dev/null || true
sleep 1
check "exec killed on disconnect" "0" \
    "$("$MVM" exec itest sh -c 'c=0; for p in $(pgrep -x sleep); do grep -q 299 /proc/$p/cmdline && c=$((c+1)); done; echo $c')"

echo "==> networking"
# --net none must be truly isolated: libkrun defaults to TSI (transparent
# host networking) without a NIC, which mvm now disables with a dead NIC.
set +e
"$MVM" run alpine timeout 5 wget -q -O /dev/null http://1.1.1.1 >/dev/null 2>&1
NONE_RC=$?
set -e
check "none profile is isolated" "isolated" "$([ "$NONE_RC" -ne 0 ] && echo isolated)"

# --net tsi: libkrun transparent socket impersonation — outbound + DNS with
# zero host setup.
check "tsi outbound + dns" "TSI-OK" \
    "$("$MVM" run --net tsi alpine sh -c \
        'timeout 15 wget -q -O /dev/null http://detectportal.firefox.com/success.txt && echo TSI-OK')"

if command -v gvproxy >/dev/null 2>&1; then
    GVSOCK="$MVM_DATA_DIR/gvproxy.sock"
    gvproxy -listen-vfkit "unixgram://$GVSOCK" >/dev/null 2>&1 &
    GVPID=$!
    for _ in $(seq 1 50); do [ -S "$GVSOCK" ] && break; sleep 0.1; done

    check "gvproxy outbound + dns" "NET-OK" \
        "$("$MVM" run --net "gvproxy:$GVSOCK" alpine sh -c \
            'timeout 15 wget -q -O /dev/null http://detectportal.firefox.com/success.txt && echo NET-OK')"

    # Port forwarding: guest httpd reachable from the host.
    "$MVM" run --keep --name web --net "gvproxy:$GVSOCK" -p 18080:8000 alpine \
        sh -c 'mkdir -p /www && echo gv-web > /www/index.html && httpd -f -p 8000 -h /www' >/dev/null 2>&1 &
    WEB_PID=$!
    for _ in $(seq 1 100); do
        curl -sf --max-time 1 http://127.0.0.1:18080/ >/dev/null 2>&1 && break
        sleep 0.2
    done
    check "gvproxy port forward" "gv-web" "$(curl -sf --max-time 3 http://127.0.0.1:18080/)"
    "$MVM" stop web >/dev/null 2>&1 || true
    wait "$WEB_PID" 2>/dev/null || true
    "$MVM" rm web >/dev/null 2>&1 || true
    kill "$GVPID" 2>/dev/null || true
else
    echo "skip: gvproxy not installed (outbound + port-forward checks)"
fi

echo "==> volumes"
VOLDIR=$(mktemp -d /tmp/mvm-itest-vol.XXXXXX)
echo vol-data > "$VOLDIR/f.txt"
check "volume mount" "vol-data" "$("$MVM" run alpine -v "$VOLDIR:/data" cat /data/f.txt)"
rm -rf "$VOLDIR"

echo "==> ext4 root (chown + ownership)"
# Rootless virtiofs roots can't chown; the ext4 driver must allow it and
# must have restored root ownership from the manifest.
check "guest chown" "daemon" "$("$MVM" exec itest sh -c 'chown daemon:daemon /tmp && stat -c %U /tmp')"
check "root-owned files" "0" "$("$MVM" exec itest stat -c %u /bin/busybox)"
"$MVM" exec itest touch /persist-marker >/dev/null

echo "==> lifecycle"
"$MVM" stop itest >/dev/null
wait "$RUN_PID" 2>/dev/null || true
check "stopped state" "1" "$("$MVM" ps -a | grep itest | grep -c stopped)"

# ext4 disks survive stop/start (docker-like persistence).
"$MVM" start itest >/dev/null
for _ in $(seq 1 100); do
    "$MVM" exec itest true >/dev/null 2>&1 && break
    sleep 0.2
done
set +e
"$MVM" exec itest test -f /persist-marker >/dev/null 2>&1
check "rootfs persists across restart" "0" "$?"
set -e
"$MVM" stop itest >/dev/null

"$MVM" rm itest >/dev/null
check "removed" "0" "$("$MVM" ps -a | grep -c itest || true)"

echo "==> logs"
"$MVM" run --name logtest --keep alpine sh -c 'echo l1; echo l2' >/dev/null
check "logs" "l1 l2" "$("$MVM" logs logtest | tr '\n' ' ' | sed 's/ $//')"
# Follow mode must terminate promptly on an exited sandbox, not hang.
check "logs -f terminates" "l1 l2" \
    "$(timeout 10 "$MVM" logs -f logtest | tr '\n' ' ' | sed 's/ $//')"
"$MVM" rm logtest >/dev/null

echo
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
