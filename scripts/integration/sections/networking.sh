#!/usr/bin/env bash
set -euo pipefail
echo "== networking =="
# --net none must be truly isolated: libkrun defaults to TSI (transparent
# host networking) without a NIC, which mvm now disables with a dead NIC.
set +e
"$MVM" run alpine timeout 5 wget -q -O /dev/null http://1.1.1.1 >/dev/null 2>&1
NONE_RC=$?
set -e
check "none profile is isolated" "isolated" "$([ "$NONE_RC" -ne 0 ] && echo isolated)"

# --net tsi: libkrun transparent socket impersonation — outbound + DNS with
# zero host setup.
check "tsi outbound + dns" "TSI-OK" \
    "$("$MVM" run --net tsi alpine sh -c \
        'timeout 15 wget -q -O /dev/null http://detectportal.firefox.com/success.txt && echo TSI-OK' 2>/dev/null)"

if command -v gvproxy >/dev/null 2>&1; then
    # `gvproxy:<socket>` attaches to a gvproxy the caller runs. Its vfkit
    # datagram endpoint only ever serves the first VM that talks to it, so
    # this instance gets exactly one sandbox.
    GVSOCK="$MVM_DATA_DIR/gvproxy.sock"
    gvproxy -ssh-port -1 -listen "unix://$MVM_GVPROXY_CONTROL" \
        -listen-vfkit "unixgram://$GVSOCK" >/dev/null 2>&1 &
    GVPID=$!
    for _ in $(seq 1 50); do
        [ -S "$GVSOCK" ] && [ -S "$MVM_GVPROXY_CONTROL" ] && break
        sleep 0.1
    done

    check "gvproxy outbound + dns (external socket)" "NET-OK" \
        "$("$MVM" run --net "gvproxy:$GVSOCK" alpine sh -c \
            'timeout 15 wget -q -O /dev/null http://detectportal.firefox.com/success.txt && echo NET-OK' 2>/dev/null)"
    kill "$GVPID" 2>/dev/null || true
    GVPID=

    # Bare `gvproxy`: the daemon runs a private one per sandbox. Every
    # sandbox must get working networking — a shared socket only ever
    # served the first VM, silently leaving later ones with no route at all.
    check "gvproxy managed outbound" "NET-OK" \
        "$("$MVM" run --net gvproxy alpine sh -c \
            'timeout 15 wget -q -O /dev/null http://detectportal.firefox.com/success.txt && echo NET-OK' 2>/dev/null)"
    check "gvproxy managed outbound (later sandbox)" "NET-OK" \
        "$("$MVM" run --net gvproxy alpine sh -c \
            'timeout 15 wget -q -O /dev/null http://detectportal.firefox.com/success.txt && echo NET-OK' 2>/dev/null)"

    # Port forwarding: a guest TCP listener reachable from the host.
    "$MVM" run --name web --net gvproxy -p 18080:8000 alpine \
        sh -c 'while true; do printf "gv-web\\n" | nc -l -p 8000; done' >/dev/null 2>&1 &
    WEB_PID=$!
    sleep 3
    WEB_RESPONSE=$(printf 'gv-web\n' | nc -w 3 127.0.0.1 18080 2>/dev/null || true)
    check "gvproxy port forward" "gv-web" "$WEB_RESPONSE"

    # The managed gvproxy is the sandbox's, and dies with it: no leftover
    # process squatting on the host port.
    WEB_GVPID=$("$MVM" inspect web | grep -o '"gvproxy_pid": *[0-9]*' | grep -o '[0-9]*')
    "$MVM" stop web >/dev/null 2>&1 || true
    wait "$WEB_PID" 2>/dev/null || true
    sleep 1
    check "managed gvproxy stops with sandbox" "gone" \
        "$([ -n "$WEB_GVPID" ] && ! kill -0 "$WEB_GVPID" 2>/dev/null && echo gone)"
    "$MVM" rm web >/dev/null 2>&1 || true
else
    skip "gvproxy not installed (outbound + port-forward checks)"
fi
echo
echo "$PASS passed, $SKIP skipped, $FAIL failed"
[ "$FAIL" -eq 0 ]
