
LIB := justfile_directory() + "/lib.sh"
RUNTIME := justfile_directory() + "/.runtime"

# Boot a fresh test daemon (isolated data dir, kills any daemon on the port).
_serve:
    #!/usr/bin/env bash
    set -euo pipefail
    source "{{LIB}}"
    # Resolve the fixed, per-run environment. Overridable via MVM_BIN /
    # MVM_GUESTD_BIN (bare MVM/AGENT collide with unrelated env vars).
    case "$(uname -m)" in
        arm64|aarch64) MUSL_TARGET=aarch64-unknown-linux-musl ;;
        *) MUSL_TARGET=x86_64-unknown-linux-musl ;;
    esac
    MVM=${MVM_BIN:-{{justfile_directory()}}/../../target/release/mvm}
    GUESTD=${MVM_GUESTD_BIN:-{{justfile_directory()}}/../../target/$MUSL_TARGET/release/mvm-guestd}
    PORT=24699
    MVM_HOST="http://127.0.0.1:$PORT"
    # Keep the data dir SHORT: gvproxy binds unix sockets under
    # <data>/sandboxes/<id>/, and macOS's sockaddr_un.sun_path is limited to
    # 104 bytes. A repo-relative .runtime/data/... path runs past that and
    # gvproxy fails with "bind: invalid argument" (EINVAL).
    MVM_DATA_DIR="/tmp/mvm-itest-$$"
    MVM_GUESTD_PATH="$(cd {{justfile_directory()}}/../.. && pwd)/$GUESTD"
    MVM_GVPROXY_CONTROL="$MVM_DATA_DIR/gvproxy-control.sock"
    # The Agent API is a per-sandbox vsock channel now (no host-wide address
    # or CLI flag to probe); it is on by default (`agent-api` feature) and
    # this repo doesn't maintain a build without it.
    AGENT_API=1

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
    [ -f "$GUESTD" ] || { echo "FAIL: static guestd $GUESTD not built"; exit 1; }

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
        "export GUESTD=\"$GUESTD\"" \
        "export PORT=$PORT" \
        "export MVM_HOST=\"$MVM_HOST\"" \
        "export MVM_DATA_DIR=\"$MVM_DATA_DIR\"" \
        "export MVM_GUESTD_PATH=\"$MVM_GUESTD_PATH\"" \
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

    "$MVM" serve --port "$PORT" >/dev/null 2>&1 &
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
