#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/../lib.sh"
echo "== console resize =="
# A -t workload that reports its pty size every second. stdin here is a pipe,
# so the client sends no resize itself (non-tty consoles don't poll); only the
# explicit console/resize call changes the workload's geometry.
"$MVM" run --name csrz -t alpine \
    sh -c 'while true; do stty size; sleep 1; done' >"$MVM_DATA_DIR/csrz.log" 2>&1 &
CSRZ_PID=$!
for _ in $(seq 1 100); do
    "$MVM" exec csrz true >/dev/null 2>&1 && break
    sleep 0.2
done
sleep 1
check "no resize before an explicit call" "0" \
    "$(grep -c '45 123' "$MVM_DATA_DIR/csrz.log" || true)"

# The console-specific endpoint resizes the workload's pty mid-session.
curl -fsS -X POST -H 'content-type: application/json' \
    -d '{"cols":123,"rows":45}' \
    "http://127.0.0.1:$PORT/api/v1/sandboxes/csrz/console/resize"
for _ in $(seq 1 20); do
    grep -q '45 123' "$MVM_DATA_DIR/csrz.log" && break
    sleep 0.5
done
check "console resize reaches the workload pty" "1" \
    "$(grep -c '45 123' "$MVM_DATA_DIR/csrz.log" || true)"

# The live geometry is recorded on the sandbox (spec.tty_size stays the
# create-time initial), so inspect shows it.
check "inspect reports the live console size" "1" \
    "$("$MVM" inspect csrz | tr -d '\n ' | grep -c '"console_size":\[123,45\]')"

# Resize after teardown is refused, and the daemon must stay healthy.
"$MVM" stop csrz >/dev/null 2>&1 || true
set +e
RESIZE_RC=$(curl -s -o /dev/null -w '%{http_code}' -X POST \
    -H 'content-type: application/json' -d '{"cols":9,"rows":9}' \
    "http://127.0.0.1:$PORT/api/v1/sandboxes/csrz/console/resize")
set -e
check "resize on a stopped sandbox is refused, daemon unharmed" "1" \
    "$([ "$RESIZE_RC" != "204" ] && curl -sf "$MVM_HOST/health" >/dev/null && echo 1)"
"$MVM" rm -f csrz >/dev/null 2>&1 || true
echo
echo "$PASS passed, $SKIP skipped, $FAIL failed"
[ "$FAIL" -eq 0 ]
