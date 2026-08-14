#!/usr/bin/env bash
set -euo pipefail
echo "== pull =="
"$MVM" pull alpine >/dev/null 2>&1
check "image listed" "1" "$("$MVM" images | grep -c alpine)"
check "re-pull up to date" "1" "$("$MVM" pull alpine | grep -c 'up to date')"
echo
echo "$PASS passed, $SKIP skipped, $FAIL failed"
[ "$FAIL" -eq 0 ]
