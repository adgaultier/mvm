#!/usr/bin/env bash
set -euo pipefail
echo "== attach =="
# -i/-t are create-time properties, so a sandbox created with them stays
# attachable after a plain (detached) start — by name as well as by id.
"$MVM" create -it --name att alpine sh >/dev/null 2>&1
ATT_ID=$("$MVM" ps -a | grep att | awk '{print $1}')
"$MVM" start att >/dev/null 2>&1
for _ in $(seq 1 100); do
    "$MVM" exec att true >/dev/null 2>&1 && break
    sleep 0.2
done
check "start by name" "1" "$("$MVM" ps | grep -c att)"
check "exec by id prefix" "prefix-ok" \
    "$("$MVM" exec "${ATT_ID:0:6}" echo prefix-ok | tr -d '\r')"

if command -v script >/dev/null 2>&1; then
    # Attach, run a command through the console, then detach with ^P^Q: the
    # workload must survive it (an EOF here would end the shell instead).
    ATT_OUT=$(mktemp /tmp/mvm-itest-attach.XXXXXX)
    run_pty timeout 40 "$MVM" attach att \
        < <(sleep 3; printf 'echo ATTACH-OK\n'; sleep 3; printf '\020\021') \
        > "$ATT_OUT" 2>&1 || true
    # The marker appears twice — the pty echoes the command, then the command
    # prints it — so match presence, not a count.
    check "attach drives the console" "found" \
        "$(grep -q ATTACH-OK "$ATT_OUT" && echo found)"
    check "detach keys leave it running" "1" \
        "$(grep -c 'detached (sandbox still running)' "$ATT_OUT")"
    rm -f "$ATT_OUT"
    check "sandbox alive after detach" "alive" \
        "$("$MVM" exec att echo alive | tr -d '\r')"
fi

# Attaching to something that is not running names the fix.
set +e
"$MVM" attach nosuchsandbox >/dev/null 2>&1
check "attach rejects unknown sandbox" "1" "$?"
set -e
"$MVM" rm -f att >/dev/null 2>&1

# Console backlog can be capped (what attach uses to avoid dumping history).
"$MVM" run --name tailtest alpine sh -c 'echo one; echo two; echo three' >/dev/null 2>&1
check "logs --tail" "two three" \
    "$("$MVM" logs -n 2 tailtest | tr -d '\r' | tr '\n' ' ' | sed 's/ $//')"
"$MVM" rm tailtest >/dev/null 2>&1
echo
echo "$PASS passed, $SKIP skipped, $FAIL failed"
[ "$FAIL" -eq 0 ]
