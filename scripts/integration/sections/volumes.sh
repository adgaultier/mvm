#!/usr/bin/env bash
set -euo pipefail
echo "== volumes =="
# Parent dir is owned by the invoking user and non-sticky, so the host can
# clean it up even after the daemon chowns the mount subdir to a subuid.
VOPARENT=$(mktemp -d /tmp/mvm-itest-volpar.XXXXXX)
VOLDIR="$VOPARENT/data"
mkdir "$VOLDIR"
echo vol-data > "$VOLDIR/f.txt"
check "volume mount" "vol-data" "$("$MVM" run alpine -v "$VOLDIR:/data" cat /data/f.txt 2>/dev/null)"

if [ "$(uname -s)" = Linux ]; then
    # Rootless userns: the daemon chowns each `:rw` bind mount to the
    # guest's agent user (uid 1000) before boot, so a non-root workload can
    # write through virtiofs (which enforces host ownership bits under
    # LINUX_COMPLETE). Ownership is asserted from inside the guest, which owns
    # the mount — the host user loses direct write access to the chowned dir,
    # so host-side stat/rm would fail. Assertions run as the workload user.
    #
    # 1. Guest uid 1000 can write into a :rw mount and read it back.
    NRW_OUT="$("$MVM" run -u 1000 alpine -v "$VOLDIR:/data" \
        sh -c 'echo hi > /data/nonroot.txt && cat /data/nonroot.txt' 2>/dev/null || true)"
    check "non-root rw mount write" "hi" "$NRW_OUT"
    # 2. The mount root and a guest-1000-created file are owned by uid 1000
    #    (the guest's view), proving the prepare-time chown mapped correctly.
    check "guest sees :rw mount owned by uid 1000" "1000:1000" \
        "$("$MVM" run -u 1000 alpine -v "$VOLDIR:/data" stat -c %u:%g /data 2>/dev/null || true)"
    check "guest sees its new file owned by uid 1000" "1000:1000" \
        "$("$MVM" run -u 1000 alpine -v "$VOLDIR:/data" stat -c %u:%g /data/nonroot.txt 2>/dev/null || true)"
    # 3. Guest root can still write the :rw mount.
    ROOT_OUT="$("$MVM" run alpine -v "$VOLDIR:/data" \
        sh -c 'echo root > /data/rootfile.txt && cat /data/rootfile.txt' 2>/dev/null || true)"
    check "root rw mount write" "root" "$ROOT_OUT"
    # Cleanup: the guest owns the mount root, so have it drop write locks;
    # the host then removes the (rlmp-owned, non-sticky) parent tree.
    "$MVM" run -u 1000 alpine -v "$VOLDIR:/data" \
        sh -c 'chmod -R a+rwx /data && find /data -mindepth 1 -delete' >/dev/null 2>&1 || true
else
    skip "volume permission-semantics checks (macOS)"
fi
chmod -R a+rwx "$VOLDIR" 2>/dev/null || true
rm -rf "$VOPARENT"
echo
echo "$PASS passed, $SKIP skipped, $FAIL failed"
[ "$FAIL" -eq 0 ]
