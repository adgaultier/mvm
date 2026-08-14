#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/../lib.sh"
echo "== logs =="
"$MVM" run --name logtest alpine sh -c 'echo l1; echo l2' >/dev/null 2>&1
check "logs" "l1 l2" "$("$MVM" logs logtest | tr '\n' ' ' | sed 's/ $//')"
# Follow mode must terminate promptly on an exited sandbox, not hang.
check "logs -f terminates" "l1 l2" \
    "$(timeout 10 "$MVM" logs -f logtest | tr '\n' ' ' | sed 's/ $//')"
"$MVM" rm logtest >/dev/null 2>&1

# Terminal *queries* must not reach a reader that never answers them: the
# reply lands in the reader's own input buffer instead. The recording drops
# them, and so must the live tail of `logs -f`. An interactive session does
# answer, so `attach` / `run -it` ask for ?raw=true and keep them byte-exact.
# Both followers subscribe while the workload sleeps, so the query travels
# the live path rather than the replayed backlog.
QF_FILTERED=$(mktemp /tmp/mvm-itest-qf.XXXXXX)
QF_RAW=$(mktemp /tmp/mvm-itest-qr.XXXXXX)
"$MVM" create --name qfilter alpine sh -c 'sleep 4; printf "Q\033[6nZ\n"' >/dev/null 2>&1
"$MVM" start qfilter >/dev/null 2>&1
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
"$MVM" rm -f qfilter >/dev/null 2>&1
echo
echo "$PASS passed, $SKIP skipped, $FAIL failed"
[ "$FAIL" -eq 0 ]
