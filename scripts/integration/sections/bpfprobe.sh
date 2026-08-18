#!/usr/bin/env bash
set -euo pipefail
echo "== guest kernel eBPF probe (bpfprobe) =="
# Informs the Phase 2 plan (in-guest cgroup_skb egress policy): whether the
# libkrunfw guest kernel can load and attach eBPF programs at all. The probe
# runs as guest root (the threat model's hostile-root assumption), so bpf()
# and the cgroup2/bpffs mounts it attempts are exactly what the trusted
# guestd-init would do. Not a hard pass/fail on any value — it records the
# kernel's capabilities — but it must *run* and produce the full key set.
PROBE_DIR=$(mktemp -d /tmp/mvm-itest-bpf.XXXXXX)
if build_probe "$PROBES_DIR/bpfprobe.c" "$PROBE_DIR" bpfprobe; then
    "$MVM" run --name bpfprobe -v "$PROBE_DIR:/probe" alpine \
            sh -c '/probe/bpfprobe > /probe/out; sleep 300' \
            >/dev/null 2>&1 &
    PROBE_RUN_PID=$!
    for _ in $(seq 1 100); do
        "$MVM" exec bpfprobe true >/dev/null 2>&1 && break
        sleep 0.2
    done
    for _ in $(seq 1 50); do
        [ -s "$PROBE_DIR/out" ] && break
        sleep 0.2
    done
    BPF_OUT=$(cat "$PROBE_DIR/out" 2>/dev/null || true)
    # Every key must be present; the values are informational (they
    # depend on the pinned libkrunfw kernel, not on mvm). The greps are
    # guarded: a missing key (or an empty probe output) must yield an
    # empty value, not a non-zero pipeline that set -e turns into an exit.
    for key in btf configgz jit cgroup2 bpffs progl attach; do
        VAL=$(printf '%s' "$BPF_OUT" | grep -o "$key=[0-9a-z]*" | cut -d= -f2 | head -1 || true)
        check "bpfprobe $key reported" "yes" "$([ -n "$VAL" ] && echo yes || echo no)"
    done
    # Surface the verdict for humans reviewing the log.
    printf '%s\n' "$BPF_OUT" | sed 's/^/  bpfprobe: /' || true
    PROG_L=$(printf '%s' "$BPF_OUT" | grep -o 'progl=[0-9]*' | cut -d= -f2 | head -1 || true)
    ATTACH_L=$(printf '%s' "$BPF_OUT" | grep -o 'attach=[0-9]*' | cut -d= -f2 | head -1 || true)
    if [ "$PROG_L" = 0 ] && [ "$ATTACH_L" = 0 ]; then
        echo "  ✅ bpfprobe: cgroup_skb load+attach works — Phase 2 (in-guest egress policy) is viable"
    else
        echo "  ℹ️ bpfprobe: cgroup_skb load+attach unavailable — Phase 2 stays behind the strict-mode probe gate"
    fi
    "$MVM" stop bpfprobe >/dev/null 2>&1 || true
    wait "$PROBE_RUN_PID" 2>/dev/null || true
    "$MVM" rm bpfprobe >/dev/null 2>&1 || true
else
    skip "bpfprobe (no static cc or zig)"
fi
rm -rf "$PROBE_DIR"
echo
echo "$PASS passed, $SKIP skipped, $FAIL failed"
[ "$FAIL" -eq 0 ]
