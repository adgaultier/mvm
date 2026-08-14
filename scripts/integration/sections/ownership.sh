#!/usr/bin/env bash
set -euo pipefail
echo "== ownership + persistence =="
# Reuse exec's `itest` sandbox; create it if this section runs standalone.
ensure_sandbox itest alpine sleep 60
# Guest chown fidelity and rootfs persistence need an ownership-capable,
# persistent driver (userns/overlay on Linux). The macOS copy driver
# provides neither, so those checks are Linux-only.
if [ "$(uname -s)" = Linux ]; then
    check "guest chown" "daemon" "$("$MVM" exec itest sh -c 'chown daemon:daemon /tmp && stat -c %U /tmp')"
    check "root-owned files" "0" "$("$MVM" exec itest stat -c %u /bin/busybox)"
    "$MVM" exec itest touch /persist-marker >/dev/null 2>&1
else
    skip "chown/ownership checks (copy driver on macOS)"
fi
echo
echo "$PASS passed, $SKIP skipped, $FAIL failed"
[ "$FAIL" -eq 0 ]
