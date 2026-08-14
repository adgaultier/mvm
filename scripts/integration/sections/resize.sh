#!/usr/bin/env bash
set -euo pipefail
echo "== resize (cpu/memory) =="
"$MVM" run --name rsz alpine sleep 180 >/dev/null 2>&1 &
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

"$MVM" resize rsz --cpus 2 -m 1024 --restart >/dev/null 2>&1
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
"$MVM" rm -f rsz >/dev/null 2>&1
echo
echo "$PASS passed, $SKIP skipped, $FAIL failed"
[ "$FAIL" -eq 0 ]
