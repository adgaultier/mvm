#!/usr/bin/env bash
set -euo pipefail
echo "== run =="
check "run stdout" "hello-vm" "$("$MVM" run alpine echo hello-vm 2>/dev/null)"

set +e
"$MVM" run alpine sh -c 'exit 7' >/dev/null 2>&1
check "run exit code" "7" "$?"
set -e

# run -i attaches local stdin to the guest console.
check "run -i stdin" "found" \
    "$(printf 'from-stdin\n' | timeout 60 "$MVM" run -i alpine cat | tr -d '\r' | grep -q from-stdin && echo found)"

# The default guest console is a TTY for the workload on Linux/KVM,
# independent of client -t.
# On Linux/KVM, the default guest console is backed by a PTY, so the
# workload's fd 0 is a TTY even without client-side -t.
#
# On macOS, libkrun's Hypervisor.framework backend connects the guest
# console through virtio-console/serial emulation rather than a guest PTY.
# Consequently [ -t 0 ] is false for the default console. This does not
# mean -t/-it is unsupported: the explicit PTY path is tested below.
if [ "$(uname -s)" = Linux ]; then
    check "run console is a tty" "CONSOLE-TTY" \
        "$("$MVM" run alpine sh -c '[ -t 0 ] && echo CONSOLE-TTY' 2>/dev/null | tr -d '\r')"
else
    skip "guest console is not a tty on macOS (libkrun hv backend)"
fi

# run -it end to end: wrap the client in a real pty via script(1) so the
# raw-mode path (termios guard enable/restore) actually engages.
if command -v script >/dev/null 2>&1; then
    # Capture to a variable, then grep: piping straight into `grep -q` makes
    # grep exit on the first match, which SIGPIPEs `script` as it writes the
    # trailing "kept" notice — and with `pipefail` that 141 fails the pipeline.
    RAW_OUT=$(printf 'exit\n' | run_pty timeout 60 "$MVM" run -it alpine sh -c '[ -t 0 ] && echo RAW-TTY-OK' 2>/dev/null)
    check "run -it raw mode" "found" \
        "$(printf '%s' "$RAW_OUT" | grep -q RAW-TTY-OK && echo found)"

    # The workload gets its own guest pty, so its output is CRLF-terminated
    # like any terminal session (the client runs raw and adds nothing).
    check "run -t emits crlf" "found" \
        "$(timeout 60 "$MVM" run -t alpine printf 'A\n' | grep -q "$(printf 'A\r')" && echo found)"

    # A live -it session must show output that does not end in a newline —
    # a shell prompt, or in raw mode every echoed keystroke. Buffered client
    # output flushes only on '\n' (and at exit), so this has to be sampled
    # while the session is still open: that blank window *is* the freeze.
    PROMPT_OUT=$(mktemp /tmp/mvm-itest-prompt.XXXXXX)
    run_pty timeout 20 "$MVM" run -it alpine sh \
        < <(sleep 10; printf 'exit\n') > "$PROMPT_OUT" 2>&1 &
    PROMPT_PID=$!
    sleep 6
    check "run -it prompt arrives unbuffered" "found" \
        "$(grep -q '#' "$PROMPT_OUT" && echo found)"
    wait "$PROMPT_PID" 2>/dev/null || true
    rm -f "$PROMPT_OUT"
else
    skip "script(1) not available (run -it raw-mode check)"
fi
echo
echo "$PASS passed, $SKIP skipped, $FAIL failed"
[ "$FAIL" -eq 0 ]
