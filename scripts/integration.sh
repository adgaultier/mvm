#!/usr/bin/env bash
# End-to-end integration test: boots real microVMs.
#
# Requirements: Linux with /dev/kvm (rw), or macOS Apple Silicon with
# libkrun installed (scripts/install-darwin.sh); libkrun + libkrunfw;
# network access to docker.io. Runs against an isolated data dir and
# port, so it never touches your normal mvm state.
#
# Usage: scripts/integration.sh [path-to-mvm] [path-to-static-mvm-agent]
set -euo pipefail

cd "$(dirname "$0")/.."

case "$(uname -m)" in
    arm64|aarch64) MUSL_TARGET=aarch64-unknown-linux-musl ;;
    *) MUSL_TARGET=x86_64-unknown-linux-musl ;;
esac

MVM=${1:-target/debug/mvm}
AGENT=${2:-target/$MUSL_TARGET/release/mvm-agent}
PORT=24699
export MVM_HOST="http://127.0.0.1:$PORT"
export MVM_DATA_DIR=$(mktemp -d /tmp/mvm-itest.XXXXXX)
export MVM_AGENT_PATH="$PWD/$AGENT"
export MVM_GVPROXY_CONTROL="$MVM_DATA_DIR/gvproxy-control.sock"

if [ "$(uname -s)" = Darwin ]; then
    [ -f "$(brew --prefix)/lib/libkrun.dylib" ] || {
        echo "SKIP: libkrun not installed (run scripts/install-darwin.sh)"; exit 0; }
    # Hypervisor.framework only serves binaries carrying the hypervisor
    # entitlement; (re-)sign whatever binary we're about to run.
    codesign --force --sign - --entitlements scripts/hypervisor.entitlements \
        "$MVM" >/dev/null 2>&1
else
    [ -e /dev/kvm ] || { echo "SKIP: /dev/kvm not available"; exit 0; }
fi
[ -x "$MVM" ] || { echo "FAIL: $MVM not built (run scripts/build.sh or cargo build)"; exit 1; }
[ -f "$AGENT" ] || { echo "FAIL: static agent $AGENT not built"; exit 1; }

# GNU/BSD portability shims. timeout(1) must be a real command — it runs
# inside script(1)'s pty, which execs it — so shim it on PATH if missing.
if ! command -v timeout >/dev/null 2>&1; then
    SHIM_DIR=$(mktemp -d /tmp/mvm-itest-shim.XXXXXX)
    if command -v gtimeout >/dev/null 2>&1; then
        printf '#!/bin/sh\nexec gtimeout "$@"\n' > "$SHIM_DIR/timeout"
    else
        printf '#!/bin/sh\nsecs=$1; shift\nexec perl -e '\''alarm shift; exec @ARGV'\'' "$secs" "$@"\n' > "$SHIM_DIR/timeout"
    fi
    chmod +x "$SHIM_DIR/timeout"
    PATH="$SHIM_DIR:$PATH"
fi
sha256_stream() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum; else shasum -a 256; fi
}
# GNU and BSD script(1) disagree on syntax; wrap both.
#
# BSD script takes argv, GNU script takes a *command string* — so the GNU
# branch has to re-quote each argument. Joining with "$*" instead loses the
# caller's quoting: `sh -c '[ -t 0 ] && echo OK'` flattens to
# `sh -c [ -t 0 ] && echo OK`, where the && is re-parsed by this shell and
# the guest runs a truncated `sh -c [`.
run_pty() {
    if [ "$(uname -s)" = Darwin ]; then
        script -q /dev/null "$@"
    else
        local cmd= arg
        for arg in "$@"; do
            cmd="$cmd${cmd:+ }$(printf '%q' "$arg")"
        done
        script -qec "$cmd" /dev/null
    fi
}

PASS=0
FAIL=0
DAEMON_PID=
GVPID=

cleanup() {
    [ -n "$GVPID" ] && kill "$GVPID" 2>/dev/null || true
    # On Linux `mvm serve` re-execs into a userns *child*, so killing the pid
    # we spawned leaves the real daemon alive holding the port. It then
    # answers the next run's health check — against a data dir this cleanup
    # has already deleted — and that run fails in ways that look like the
    # change under test. Kill whatever still owns the port, by pid, and wait
    # for it to actually go.
    [ -n "$DAEMON_PID" ] && kill "$DAEMON_PID" 2>/dev/null || true
    for _ in $(seq 1 30); do
        curl -sf "$MVM_HOST/health" >/dev/null 2>&1 || break
        for p in $(pgrep -x mvm 2>/dev/null); do
            case "$(tr '\0' ' ' < "/proc/$p/cmdline" 2>/dev/null)" in
                *"--addr 127.0.0.1:$PORT"*) kill -9 "$p" 2>/dev/null || true ;;
            esac
        done
        sleep 0.2
    done
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
"$MVM" serve --addr "127.0.0.1:$PORT" >/dev/null 2>&1 &
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
        "$(printf 'exit\n' | run_pty timeout 60 "$MVM" run -it alpine sh -c '[ -t 0 ] && echo RAW-TTY-OK' | grep -q RAW-TTY-OK && echo found)"

    # The workload gets its own guest pty, so its output is CRLF-terminated
    # like any terminal session (the client runs raw and adds nothing).
    check "run -t emits crlf" "found" \
        "$(timeout 60 "$MVM" run -t alpine printf 'A\n' | grep -q "$(printf 'A\r')" && echo found)"

    # A live -it session must show output that does not end in a newline —
    # a shell prompt, or in raw mode every echoed keystroke. Buffered client
    # output flushes only on '\n' (and at exit), so this has to be sampled
    # while the session is still open: that blank window *is* the freeze.
    PROMPT_OUT=$(mktemp /tmp/mvm-itest-prompt.XXXXXX)
    run_pty timeout 40 "$MVM" run -it alpine sh \
        < <(sleep 20; printf 'exit\n') > "$PROMPT_OUT" 2>&1 &
    PROMPT_PID=$!
    sleep 12
    check "run -it prompt arrives unbuffered" "found" \
        "$(grep -q '#' "$PROMPT_OUT" && echo found)"
    wait "$PROMPT_PID" 2>/dev/null || true
    rm -f "$PROMPT_OUT"
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
    "$(sha256_stream < "$BINFILE" | cut -d' ' -f1)" \
    "$("$MVM" exec -i itest cat < "$BINFILE" | sha256_stream | cut -d' ' -f1)"
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
    # `gvproxy:<socket>` attaches to a gvproxy the caller runs. Its vfkit
    # datagram endpoint only ever serves the first VM that talks to it, so
    # this instance gets exactly one sandbox.
    GVSOCK="$MVM_DATA_DIR/gvproxy.sock"
    gvproxy -ssh-port -1 -listen "unix://$MVM_GVPROXY_CONTROL" \
        -listen-vfkit "unixgram://$GVSOCK" >/dev/null 2>&1 &
    GVPID=$!
    for _ in $(seq 1 50); do
        [ -S "$GVSOCK" ] && [ -S "$MVM_GVPROXY_CONTROL" ] && break
        sleep 0.1
    done

    check "gvproxy outbound + dns (external socket)" "NET-OK" \
        "$("$MVM" run --net "gvproxy:$GVSOCK" alpine sh -c \
            'timeout 15 wget -q -O /dev/null http://detectportal.firefox.com/success.txt && echo NET-OK')"
    kill "$GVPID" 2>/dev/null || true
    GVPID=

    # Bare `gvproxy`: the daemon runs a private one per sandbox. Every
    # sandbox must get working networking — a shared socket only ever
    # served the first VM, silently leaving later ones with no route at all.
    check "gvproxy managed outbound" "NET-OK" \
        "$("$MVM" run --net gvproxy alpine sh -c \
            'timeout 15 wget -q -O /dev/null http://detectportal.firefox.com/success.txt && echo NET-OK')"
    check "gvproxy managed outbound (later sandbox)" "NET-OK" \
        "$("$MVM" run --net gvproxy alpine sh -c \
            'timeout 15 wget -q -O /dev/null http://detectportal.firefox.com/success.txt && echo NET-OK')"

    # Port forwarding: a guest TCP listener reachable from the host.
    "$MVM" run --keep --name web --net gvproxy -p 18080:8000 alpine \
        sh -c 'while true; do printf "gv-web\\n" | nc -l -p 8000; done' >/dev/null 2>&1 &
    WEB_PID=$!
    sleep 3
    WEB_RESPONSE=$(printf 'gv-web\n' | nc -w 3 127.0.0.1 18080 2>/dev/null || true)
    check "gvproxy port forward" "gv-web" "$WEB_RESPONSE"

    # The managed gvproxy is the sandbox's, and dies with it: no leftover
    # process squatting on the host port.
    WEB_GVPID=$("$MVM" inspect web | grep -o '"gvproxy_pid": *[0-9]*' | grep -o '[0-9]*')
    "$MVM" stop web >/dev/null 2>&1 || true
    wait "$WEB_PID" 2>/dev/null || true
    sleep 1
    check "managed gvproxy stops with sandbox" "gone" \
        "$([ -n "$WEB_GVPID" ] && ! kill -0 "$WEB_GVPID" 2>/dev/null && echo gone)"
    "$MVM" rm web >/dev/null 2>&1 || true
else
    echo "skip: gvproxy not installed (outbound + port-forward checks)"
fi

echo "==> volumes"
VOLDIR=$(mktemp -d /tmp/mvm-itest-vol.XXXXXX)
echo vol-data > "$VOLDIR/f.txt"
check "volume mount" "vol-data" "$("$MVM" run alpine -v "$VOLDIR:/data" cat /data/f.txt)"
rm -rf "$VOLDIR"

echo "==> ownership + persistence"
# Guest chown fidelity and rootfs persistence need an ownership-capable,
# persistent driver (userns/overlay on Linux). The macOS copy driver
# provides neither, so those checks are Linux-only.
if [ "$(uname -s)" = Linux ]; then
    check "guest chown" "daemon" "$("$MVM" exec itest sh -c 'chown daemon:daemon /tmp && stat -c %U /tmp')"
    check "root-owned files" "0" "$("$MVM" exec itest stat -c %u /bin/busybox)"
    "$MVM" exec itest touch /persist-marker >/dev/null
else
    echo "skip: chown/ownership checks (copy driver on macOS)"
fi

echo "==> lifecycle"
"$MVM" stop itest >/dev/null
wait "$RUN_PID" 2>/dev/null || true
check "stopped state" "1" "$("$MVM" ps -a | grep itest | grep -c stopped)"

"$MVM" start itest >/dev/null
for _ in $(seq 1 100); do
    "$MVM" exec itest true >/dev/null 2>&1 && break
    sleep 0.2
done
if [ "$(uname -s)" = Linux ]; then
    set +e
    "$MVM" exec itest test -f /persist-marker >/dev/null 2>&1
    check "rootfs persists across restart" "0" "$?"
    set -e
fi
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

# Terminal *queries* must not reach a reader that never answers them: the
# reply lands in the reader's own input buffer instead. The recording drops
# them, and so must the live tail of `logs -f`. An interactive session does
# answer, so `attach` / `run -it` ask for ?raw=true and keep them byte-exact.
# Both followers subscribe while the workload sleeps, so the query travels
# the live path rather than the replayed backlog.
QF_FILTERED=$(mktemp /tmp/mvm-itest-qf.XXXXXX)
QF_RAW=$(mktemp /tmp/mvm-itest-qr.XXXXXX)
"$MVM" create --name qfilter alpine sh -c 'sleep 4; printf "Q\033[6nZ\n"' >/dev/null
"$MVM" start qfilter >/dev/null
timeout 30 "$MVM" logs -f qfilter > "$QF_FILTERED" 2>/dev/null &
QF_PID=$!
curl -sN "$MVM_HOST/api/v1/sandboxes/qfilter/logs?follow=true&raw=true" > "$QF_RAW" 2>/dev/null &
QR_PID=$!
wait "$QF_PID" 2>/dev/null || true
kill "$QR_PID" 2>/dev/null || true
check "logs -f strips queries from the live tail" "QZ" \
    "$(tr -d '\r' < "$QF_FILTERED" | grep -a Q || true)"
check "raw stream keeps queries byte-exact" "1" \
    "$(grep -acF "$(printf 'Q\033[6nZ')" "$QF_RAW" || true)"
rm -f "$QF_FILTERED" "$QF_RAW"
"$MVM" rm -f qfilter >/dev/null

echo "==> attach"
# -i/-t are create-time properties, so a sandbox created with them stays
# attachable after a plain (detached) start — by name as well as by id.
"$MVM" create -it --name att alpine sh >/dev/null
ATT_ID=$("$MVM" ps -a | grep att | awk '{print $1}')
"$MVM" start att >/dev/null
for _ in $(seq 1 100); do
    "$MVM" exec att true >/dev/null 2>&1 && break
    sleep 0.2
done
check "start by name" "1" "$("$MVM" ps | grep -c att)"
check "exec by id prefix" "prefix-ok" \
    "$("$MVM" exec "${ATT_ID:0:6}" echo prefix-ok | tr -d '\r')"

if command -v script >/dev/null 2>&1; then
    # Attach, run a command through the console, then detach with ^P^Q: the
    # workload must survive it (an EOF here would end the shell instead).
    ATT_OUT=$(mktemp /tmp/mvm-itest-attach.XXXXXX)
    run_pty timeout 40 "$MVM" attach att \
        < <(sleep 3; printf 'echo ATTACH-OK\n'; sleep 3; printf '\020\021') \
        > "$ATT_OUT" 2>&1 || true
    # The marker appears twice — the pty echoes the command, then the command
    # prints it — so match presence, not a count.
    check "attach drives the console" "found" \
        "$(grep -q ATTACH-OK "$ATT_OUT" && echo found)"
    check "detach keys leave it running" "1" \
        "$(grep -c 'detached (sandbox still running)' "$ATT_OUT")"
    rm -f "$ATT_OUT"
    check "sandbox alive after detach" "alive" \
        "$("$MVM" exec att echo alive | tr -d '\r')"
fi

# Attaching to something that is not running names the fix.
set +e
"$MVM" attach nosuchsandbox >/dev/null 2>&1
check "attach rejects unknown sandbox" "1" "$?"
set -e
"$MVM" rm -f att >/dev/null

# Console backlog can be capped (what attach uses to avoid dumping history).
"$MVM" run --name tailtest --keep alpine sh -c 'echo one; echo two; echo three' >/dev/null
check "logs --tail" "two three" \
    "$("$MVM" logs -n 2 tailtest | tr -d '\r' | tr '\n' ' ' | sed 's/ $//')"
"$MVM" rm tailtest >/dev/null

echo "==> resize (cpu/memory)"
"$MVM" run --name rsz --keep alpine sleep 180 >/dev/null 2>&1 &
RSZ_PID=$!
for _ in $(seq 1 100); do
    "$MVM" exec rsz true >/dev/null 2>&1 && break
    sleep 0.2
done
check "default size in guest" "1" "$("$MVM" exec rsz nproc | tr -d '\r')"

# A microVM's allocation is fixed at boot: resizing a running one rewrites
# the spec and says so, and the guest only changes after a restart.
check "resize reports pending restart" "1" \
    "$("$MVM" resize rsz --cpus 2 -m 1024 | grep -c 'restart to apply')"
check "resize persisted in spec" "1024" \
    "$("$MVM" inspect rsz | grep -o '"ram_mib": *[0-9]*' | grep -o '[0-9]*')"
check "running guest keeps its size" "1" "$("$MVM" exec rsz nproc | tr -d '\r')"

"$MVM" resize rsz --cpus 2 -m 1024 --restart >/dev/null
wait "$RSZ_PID" 2>/dev/null || true
for _ in $(seq 1 100); do
    "$MVM" exec rsz true >/dev/null 2>&1 && break
    sleep 0.2
done
check "resized vcpus in guest" "2" "$("$MVM" exec rsz nproc | tr -d '\r')"
RSZ_MEM_KB=$("$MVM" exec rsz grep MemTotal /proc/meminfo | tr -dc '0-9')
check "resized ram in guest" "ok" \
    "$([ "${RSZ_MEM_KB:-0}" -gt 700000 ] && echo ok)"

# Nonsense sizes are refused, and refusing must not change the spec.
set +e
"$MVM" resize rsz -m 8 >/dev/null 2>&1
check "resize rejects tiny ram" "1" "$?"
set -e
check "rejected resize left spec alone" "1024" \
    "$("$MVM" inspect rsz | grep -o '"ram_mib": *[0-9]*' | grep -o '[0-9]*')"
"$MVM" rm -f rsz >/dev/null

echo
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
