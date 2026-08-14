#!/usr/bin/env bash
set -euo pipefail
echo "== lifecycle =="
# exec's `itest`; create it if this section runs standalone.
ensure_sandbox itest alpine sleep 60
"$MVM" stop itest >/dev/null 2>&1
wait "${RUN_PID:-}" 2>/dev/null || true
check "stopped state" "1" "$("$MVM" ps -a | grep itest | grep -c stopped)"

"$MVM" start itest >/dev/null 2>&1
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
"$MVM" stop itest >/dev/null 2>&1

"$MVM" rm itest >/dev/null 2>&1
check "removed" "0" "$("$MVM" ps -a | grep -c itest || true)"
echo
echo "$PASS passed, $SKIP skipped, $FAIL failed"
[ "$FAIL" -eq 0 ]
