#!/usr/bin/env bash
set -euo pipefail
echo "== raw socket ban (seccomp) =="
# The guestd installs a seccomp filter that forbids raw packet/IP sockets for
# the whole guest. Probed twice inside one VM: as the workload (a child of the
# guestd) and via an exec session, so inheritance through both spawn paths is
# covered. The static probe is built with `build_probe` (cc -static on Linux,
# zig cross-compile elsewhere — see the helper above).
PROBE_DIR=$(mktemp -d /tmp/mvm-itest-probe.XXXXXX)
if build_probe "$PROBES_DIR/rawprobe.c" "$PROBE_DIR" rawprobe; then
    # Workload probe results go to the mounted volume; sleep keeps the
    # sandbox alive long enough for the exec-session probe below.
    "$MVM" run --name rawprobe -v "$PROBE_DIR:/probe" alpine \
        sh -c '/probe/rawprobe > /probe/workload.out; sleep 300' \
        >/dev/null 2>&1 &
    PROBE_RUN_PID=$!
    for _ in $(seq 1 100); do
        "$MVM" exec rawprobe true >/dev/null 2>&1 && break
        sleep 0.2
    done
    for _ in $(seq 1 50); do
        [ -s "$PROBE_DIR/workload.out" ] && break
        sleep 0.2
    done
    WORKLOAD_OUT=$(cat "$PROBE_DIR/workload.out" 2>/dev/null || true)
    EXEC_OUT=$("$MVM" exec rawprobe /probe/rawprobe 2>/dev/null || true)
    for spec in "packet_raw:1:AF_PACKET raw denied" \
                "inet_raw:1:AF_INET raw denied" \
                "inet6_raw:1:AF_INET6 raw denied" \
                "inet_raw_nonblock:1:AF_INET raw+nonblock denied" \
                "inet_raw_cloexec:1:AF_INET raw+cloexec denied" \
                "inet_dgram:0:AF_INET datagram allowed" \
                "netlink_dgram:0:AF_NETLINK datagram allowed"; do
        KEY=${spec%%:*}
        EXPECTED=$(printf '%s' "$spec" | cut -d: -f2)
        NAME=$(printf '%s' "$spec" | cut -d: -f3-)
        check "rawprobe(w) $NAME" "$EXPECTED" \
            "$(printf '%s' "$WORKLOAD_OUT" | grep -o "$KEY=[0-9]*" | cut -d= -f2 | head -1)"
        check "rawprobe(e) $NAME" "$EXPECTED" \
            "$(printf '%s' "$EXEC_OUT" | grep -o "$KEY=[0-9]*" | cut -d= -f2 | head -1)"
    done
    "$MVM" stop rawprobe >/dev/null 2>&1 || true
    wait "$PROBE_RUN_PID" 2>/dev/null || true
    "$MVM" rm rawprobe >/dev/null 2>&1 || true
else
    skip "raw socket ban (no static cc or zig)"
fi

if build_probe "$PROBES_DIR/strictprobe.c" "$PROBE_DIR" strictprobe; then
    STRICT_OUT=$(timeout 30 "$MVM" run --security=strict -v "$PROBE_DIR:/probe" alpine \
        /probe/strictprobe 2>/dev/null || true)
    for key in no_new_privs ptrace mount umount2 pivot_root unshare setns init_module \
               delete_module finit_module kexec_load kexec_file_load; do
        check "strict seccomp $key" "1" \
            "$(printf '%s' "$STRICT_OUT" | grep -o "$key=[0-9]*" | cut -d= -f2 | head -1)"
    done
else
    skip "strict seccomp probe (no static cc or zig)"
fi

rm -rf "$PROBE_DIR"
echo
echo "$PASS passed, $SKIP skipped, $FAIL failed"
[ "$FAIL" -eq 0 ]
