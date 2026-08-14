#!/usr/bin/env bash
set -euo pipefail
echo "== exec =="
"$MVM" run --name itest alpine sleep 60 >/dev/null 2>&1 &
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
echo
echo "$PASS passed, $SKIP skipped, $FAIL failed"
[ "$FAIL" -eq 0 ]
