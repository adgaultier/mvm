#!/usr/bin/env bash
# Utility helpers shared by the mvm integration sections.

set -euo pipefail

IT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNTIME="$IT_ROOT/.runtime"
ENV_FILE="$RUNTIME/env.sh"
PROBES_DIR="$IT_ROOT/probes"

# Load the env written by `just serve` (a fresh daemon + isolated data dir) and
# pin the CWD to the repo root for repo-relative paths
if [ -f "$ENV_FILE" ]; then
    # shellcheck disable=SC1090
    source "$ENV_FILE"
    cd "$IT_ROOT/../.."
fi

# --- assertion helpers ------------------------------------------------------

# Fuse for host-side `timeout` wrappers: seconds. One knob; override with
# MVM_ITEST_T when a slow runner needs more room than the default of 15.
T="${MVM_ITEST_T:-15}"

PASS=0
FAIL=0
SKIP=0

check() { # check <name> <expected> <actual>
    if [ "$2" = "$3" ]; then
        echo "✅   $1"
        PASS=$((PASS + 1))
    else
        echo "❌ $1: expected '$2', got '$3'"
        FAIL=$((FAIL + 1))
    fi
}

skip() { # skip <reason>
    echo "⏭️ $*"
    SKIP=$((SKIP + 1))
}

# Tear down the test daemon and its runtime dir. Used as `trap cleanup EXIT`
# by `all`, and by the `stop` recipe (utils.just).
cleanup() {
    for p in $(port_pids); do kill -9 "$p" 2>/dev/null || true; done
    sleep 0.5
    # The data dir lives in /tmp (kept short for macOS sun_path limits), so
    # remove it alongside the repo-local runtime state.
    rm -rf "${MVM_DATA_DIR:-}" "$RUNTIME"
}

# Ensure a named sandbox exists and its guestd is reachable. Used by
# sections that reuse a sandbox another section creates (exec's `itest`):
# make it self-sufficient so the section also runs standalone.
ensure_sandbox() { # ensure_sandbox <name> <image> <cmd...>
    local name=$1 image=$2
    shift 2
    if ! "$MVM" ps -a 2>/dev/null | grep -q "$name"; then
        "$MVM" run --name "$name" "$image" "$@" >/dev/null 2>&1 &
    fi
    for _ in $(seq 1 100); do
        "$MVM" exec "$name" true >/dev/null 2>&1 && return 0
        sleep 0.2
    done
    return 1
}



# PIDs currently listening on $PORT (any process). Empty if none.
port_pids() {
    if command -v lsof >/dev/null 2>&1; then
        lsof -tiTCP:"${PORT:-24699}" -sTCP:LISTEN 2>/dev/null || true
    else
        pgrep -x mvm 2>/dev/null || true
    fi
}

sha256_stream() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum; else shasum -a 256; fi
}

# GNU and BSD script(1) disagree on syntax; wrap both.
#
# BSD script takes argv, GNU script takes a *command string* — so the GNU
# branch has to re-quote each argument. Joining with "$*" instead loses the
# caller's quoting: `sh -c '[ -t 0 ] && echo OK'` flattens to
# `sh -c [ -t 0 ] && echo OK`, where the && is re-parsed by this shell and
# the guest runs a truncated `sh -c [`.
run_pty() {
    if [ "$(uname -s)" = Darwin ]; then
        script -q /dev/null "$@"
    else
        local cmd= arg
        for arg in "$@"; do
            cmd="$cmd${cmd:+ }$(printf '%q' "$arg")"
        done
        script -qec "$cmd" /dev/null
    fi
}

# Build a static Linux probe binary for the guest. The probe runs *inside*
# the VM (a Linux guest with no compiler), so the binary must be a static
# Linux ELF. On a Linux host plain `cc -static` works; anywhere else (macOS)
# we cross-compile with zig, which the guestd cross-build already requires.
# Prints nothing; returns 0 on success.
build_probe() { # build_probe <src.c> <outdir> <name>
    local src=$1 outdir=$2 name=$3 zt
    case "$MUSL_TARGET" in
        aarch64-unknown-linux-musl) zt=aarch64-linux-musl ;;
        x86_64-unknown-linux-musl)  zt=x86_64-linux-musl ;;
        *) return 1 ;;
    esac
    if [ "$(uname -s)" = Linux ] && command -v cc >/dev/null 2>&1; then
        cc -static -O2 -o "$outdir/$name" "$src" 2>/dev/null
    elif command -v zig >/dev/null 2>&1; then
        zig cc -static -O2 -target "$zt" -o "$outdir/$name" "$src" 2>/dev/null
    else
        return 1
    fi
}
