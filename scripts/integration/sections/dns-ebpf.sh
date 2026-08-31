#!/usr/bin/env bash
set -euo pipefail
echo "== eBPF DNS enforcement =="

# TSI is intentionally not covered: it is an insecure test backend and does
# not receive the NIC DNS policy.
if ! command -v gvproxy >/dev/null 2>&1; then
    skip "DNS eBPF enforcement (gvproxy not installed)"
else
    "$MVM" run --name dns-ebpf --net gvproxy alpine sh -c 'sleep 300' \
        >/dev/null 2>&1 &
    RUN_PID=$!
    for _ in $(seq 1 100); do
        "$MVM" exec dns-ebpf true >/dev/null 2>&1 && break
        sleep 0.2
    done

    check "configured resolver works" "DNS-OK" \
        "$(timeout "$T" "$MVM" exec dns-ebpf sh -c \
            'nslookup example.com >/dev/null 2>&1 && echo DNS-OK' 2>/dev/null || true)"

    # Rewriting resolv.conf must not create a DNS egress bypass. The packet
    # filter denies both UDP and TCP port 53 outside the configured gateway.
    check "rewritten resolver is denied" "DNS-DENIED" \
        "$(timeout "$T" "$MVM" exec dns-ebpf sh -c \
            'printf "nameserver 1.1.1.1\\n" > /etc/resolv.conf; \
             nslookup example.com >/dev/null 2>&1 && echo DNS-BYPASS || echo DNS-DENIED' \
            2>/dev/null || true)"

    "$MVM" stop dns-ebpf >/dev/null 2>&1 || true
    wait "$RUN_PID" 2>/dev/null || true
    "$MVM" rm dns-ebpf >/dev/null 2>&1 || true
fi

echo
echo "$PASS passed, $SKIP skipped, $FAIL failed"
[ "$FAIL" -eq 0 ]
