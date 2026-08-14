#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/../lib.sh"
echo "== clone =="
# A sandbox that writes a marker to its rootfs and then exits, so its disk
# holds state to fork. `run` leaves the sandbox behind in `exited`.
"$MVM" run --name clsrc alpine sh -c 'echo base-disk > /marker' >/dev/null 2>&1
CLSRC_ID=$("$MVM" ps -a | grep clsrc | awk '{print $1}')

# A plain clone inherits the spec but starts from a fresh disk. The inherited
# command (which writes the marker) is overridden with a keep-alive so the
# sandbox stays up long enough to inspect its disk.
CLONE_ID=$("$MVM" clone clsrc --name clplain sleep 60 2>/dev/null)
check "clone prints the new id" "1" \
    "$([ -n "$CLONE_ID" ] && [ "$CLONE_ID" != "$CLSRC_ID" ] && echo 1)"
check "clone inherits the image" '"image": "alpine"' \
    "$("$MVM" inspect clplain | grep -o '"image": *"alpine"')"
"$MVM" start clplain >/dev/null 2>&1
for _ in $(seq 1 100); do
    "$MVM" exec clplain true >/dev/null 2>&1 && break
    sleep 0.2
done
if [ "$(uname -s)" = Linux ]; then
    check "clone disk is fresh" "fresh" \
        "$("$MVM" exec clplain sh -c 'cat /marker 2>/dev/null || echo fresh' | tr -d '\r')"
fi
"$MVM" stop clplain >/dev/null 2>&1

# Forking carries the current disk. Needs the persistent overlay upper layer
# (Linux userns); the macOS copy driver rebuilds rootfs on every boot. A
# keep-alive command again, since the marker was already written to the
# source's disk when it ran.
if [ "$(uname -s)" = Linux ]; then
    "$MVM" clone clsrc --fork --name clfork sleep 60 >/dev/null 2>&1
    "$MVM" start clfork >/dev/null 2>&1
    for _ in $(seq 1 100); do
        "$MVM" exec clfork true >/dev/null 2>&1 && break
        sleep 0.2
    done
    check "fork carries the disk" "base-disk" "$("$MVM" exec clfork cat /marker | tr -d '\r')"
    "$MVM" stop clfork >/dev/null 2>&1
else
    skip "fork disk checks (copy driver on macOS)"
fi

# Overrides rewrite the inherited spec (validated daemon-side on start).
CLBIG_ID=$("$MVM" clone clsrc --fork --name clbig --cpus 2 -m 768 2>/dev/null)
check "clone flags override the spec" "768" \
    "$("$MVM" inspect clbig | grep -o '"ram_mib": *[0-9]*' | grep -o '[0-9]*')"

# The source's name is taken; an explicit reuse is refused.
set +e
"$MVM" clone clsrc --name clsrc >/dev/null 2>&1
check "clone rejects a taken name" "1" "$?"
set -e

# Without --name, create and clone get a generated `<adj>-<animal>` name.
GEN_ID=$("$MVM" create alpine 2>/dev/null)
GEN_NAME=$("$MVM" ps -a | grep -w "$GEN_ID" | awk '{print $2}')
check "no-name create gets a generated name" "1" \
    "$(echo "$GEN_NAME" | grep -Eq '^[a-z]+-[a-z_]+$' && echo 1)"
GNCLONE_ID=$("$MVM" clone clsrc 2>/dev/null)
GNCLONE_NAME=$("$MVM" ps -a | grep -w "$GNCLONE_ID" | awk '{print $2}')
check "no-name clone gets a generated name" "1" \
    "$(echo "$GNCLONE_NAME" | grep -Eq '^[a-z]+-[a-z_]+$' && echo 1)"
check "generated names are distinct" "1" "$([ "$GEN_NAME" != "$GNCLONE_NAME" ] && echo 1)"

# run without --name too: reports the generated name on exit (stderr).
RUN_OUT=$("$MVM" run alpine true 2>&1)
RUN_ID=${RUN_OUT#mvm: sandbox }
RUN_ID=${RUN_ID%% *}
RUN_NAME=$("$MVM" ps -a | grep -w "$RUN_ID" | awk '{print $2}')
check "run without --name gets a generated name" "1" \
    "$(echo "$RUN_NAME" | grep -Eq '^[a-z]+-[a-z_]+$' && echo 1)"

for s in clplain clfork clbig clsrc; do "$MVM" rm -f "$s" >/dev/null 2>&1 || true; done
for s in "$GEN_ID" "$GNCLONE_ID" "$RUN_ID"; do "$MVM" rm -f "$s" >/dev/null 2>&1 || true; done
check "clones removed" "0" "$("$MVM" ps -a | grep -c clplain || true)"

echo
echo "$PASS passed, $SKIP skipped, $FAIL failed"
[ "$FAIL" -eq 0 ]
