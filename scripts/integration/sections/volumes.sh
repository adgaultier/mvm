#!/usr/bin/env bash
set -euo pipefail
echo "== volumes =="
VOLDIR=$(mktemp -d /tmp/mvm-itest-vol.XXXXXX)
echo vol-data > "$VOLDIR/f.txt"
check "volume mount" "vol-data" "$("$MVM" run alpine -v "$VOLDIR:/data" cat /data/f.txt 2>/dev/null)"
rm -rf "$VOLDIR"
echo
echo "$PASS passed, $SKIP skipped, $FAIL failed"
[ "$FAIL" -eq 0 ]
