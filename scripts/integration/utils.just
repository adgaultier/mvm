
LIB := justfile_directory() + "/lib.sh"
RUNTIME := justfile_directory() + "/.runtime"

# Boot a fresh test daemon (isolated data dir, kills any daemon on the port).
_serve:
    #!/usr/bin/env bash
    set -euo pipefail
    source "{{LIB}}"
    # Resolve the fixed, per-run environment. Overridable via MVM_BIN /
    # MVM_AGENT_BIN (bare MVM/AGENT collide with unrelated env vars).
    case "$(uname -m)" in
        arm64|aarch64) MUSL_TARGET=aarch64-unknown-linux-musl ;;
        *) MUSL_TARGET=x86_64-unknown-linux-musl ;;
    esac
    MVM=${MVM_BIN:-{{justfile_directory()}}/../../target/release/mvm}
    AGENT=${MVM_AGENT_BIN:-{{justfile_directory()}}/../../target/$MUSL_TARGET/release/mvm-agent}
    PORT=24699
    AGENT_PORT=24700
    MVM_HOST="http://127.0.0.1:$PORT"
    MVM_AGENT_ADDR="127.0.0.1:$AGENT_PORT"
    MVM_DATA_DIR="{{RUNTIME}}/data"
    MVM_AGENT_PATH="$(cd {{justfile_directory()}}/../.. && pwd)/$AGENT"
    MVM_GVPROXY_CONTROL="$MVM_DATA_DIR/gvproxy-control.sock"
    AGENT_API=0
    "$MVM" serve --help 2>&1 | grep -q -- '--agent-addr' && AGENT_API=1

    if [ "$(uname -s)" = Darwin ]; then
        [ -f "$(brew --prefix)/lib/libkrun.dylib" ] || {
            echo "SKIP: libkrun not installed (run scripts/install-darwin.sh)"; exit 0; }
        codesign --force --sign - \
            --entitlements {{justfile_directory()}}/../hypervisor.entitlements \
            "$MVM" >/dev/null 2>&1
    else
        [ -e /dev/kvm ] || { echo "SKIP: /dev/kvm not available"; exit 0; }
    fi
    [ -x "$MVM" ] || { echo "FAIL: $MVM not built (run scripts/build.sh or cargo build)"; exit 1; }
    [ -f "$AGENT" ] || { echo "FAIL: static agent $AGENT not built"; exit 1; }

    # GNU/BSD portability shims. timeout(1) must be a real command — it runs
    # inside script(1)'s pty, which execs it — so shim it on PATH if missing.
    if ! command -v timeout >/dev/null 2>&1; then
        SHIM_DIR=$(mktemp -d /tmp/mvm-itest-shim.XXXXXX)
        if command -v gtimeout >/dev/null 2>&1; then
            printf '#!/bin/sh\nexec gtimeout "$@"\n' > "$SHIM_DIR/timeout"
        else
            printf '#!/bin/sh\nsecs=$1; shift\nexec perl -e '\''alarm shift; exec @ARGV'\'' "$secs" "$@"\n' > "$SHIM_DIR/timeout"
        fi
        chmod +x "$SHIM_DIR/timeout"
        PATH="$SHIM_DIR:$PATH"
        export PATH
    fi

    # Persist the env for section processes, then boot a *fresh* daemon:
    # anything still on the port is stale and must die, and the data dir must
    # not survive from a previous run.
    mkdir -p "{{RUNTIME}}" "$MVM_DATA_DIR"
    printf '%s\n' \
        "export MUSL_TARGET=\"$MUSL_TARGET\"" \
        "export MVM=\"$MVM\"" \
        "export AGENT=\"$AGENT\"" \
        "export PORT=$PORT" \
        "export AGENT_PORT=$AGENT_PORT" \
        "export MVM_HOST=\"$MVM_HOST\"" \
        "export MVM_AGENT_ADDR=\"$MVM_AGENT_ADDR\"" \
        "export MVM_DATA_DIR=\"$MVM_DATA_DIR\"" \
        "export MVM_AGENT_PATH=\"$MVM_AGENT_PATH\"" \
        "export MVM_GVPROXY_CONTROL=\"$MVM_GVPROXY_CONTROL\"" \
        "export AGENT_API=$AGENT_API" \
        > "{{RUNTIME}}/env.sh"
    # Export the env into THIS shell: the daemon is spawned below and must see
    # MVM_DATA_DIR (and friends), or `mvm serve` falls back to the real data
    # dir and the run pollutes (and collides with) actual sandboxes.
    # shellcheck disable=SC1090
    source "{{RUNTIME}}/env.sh"
    for p in $(port_pids); do kill -9 "$p" 2>/dev/null || true; done
    for _ in $(seq 1 20); do
        curl -sf "$MVM_HOST/health" >/dev/null 2>&1 || break
        sleep 0.2
    done
    rm -rf "$MVM_DATA_DIR"
    mkdir -p "$MVM_DATA_DIR"

    if [ "$AGENT_API" = 1 ]; then
        "$MVM" serve --addr "127.0.0.1:$PORT" --agent-addr "127.0.0.1:$AGENT_PORT" >/dev/null 2>&1 &
    else
        "$MVM" serve --addr "127.0.0.1:$PORT" >/dev/null 2>&1 &
    fi
    echo $! > "{{RUNTIME}}/daemon.pid"
    for _ in $(seq 1 50); do
        curl -sf "$MVM_HOST/health" >/dev/null 2>&1 && break
        sleep 0.1
    done
    curl -sf "$MVM_HOST/health" >/dev/null
    # Almost every section boots the alpine base image; make sure it is in the
    # store so a section can run without a prior `just pull`.
    "$MVM" pull alpine >/dev/null 2>&1 || true

# Tear down the test daemon and its runtime dir. Called via `@just cleanup` at
# the end of every section recipe (and by `all`).
_cleanup:
    #!/usr/bin/env bash
    set -euo pipefail
    source "{{LIB}}" 
    cleanup
