#!/usr/bin/env bash
# Install everything mvm needs on macOS Apple Silicon:
#   - rustup + the aarch64-unknown-linux-musl target (guest agent)
#   - zig + cargo-zigbuild (static musl cross-compile from macOS)
#   - libkrun (+ libkrunfw) and gvproxy from the libkrun/krun tap
#
# Idempotent: safe to re-run; skips whatever is already present.
set -euo pipefail

fail() { echo "error: $*" >&2; exit 1; }
step() { echo "==> $*"; }

[ "$(uname -s)" = Darwin ] || fail "this script is for macOS only"
# libkrun's Hypervisor.framework backend is arm64-only (and so are its
# guests: HVF runs same-arch VMs).
[ "$(uname -m)" = arm64 ] || fail "Apple Silicon (arm64) required"

macos_major=$(sw_vers -productVersion | cut -d. -f1)
[ "$macos_major" -ge 14 ] || fail "macOS 14+ required (found $(sw_vers -productVersion))"

command -v brew >/dev/null || fail "Homebrew not found; install it from https://brew.sh first"
BREW_PREFIX=$(brew --prefix)

echo "==> rustup"
if command -v rustup >/dev/null; then
    echo "    already installed: $(rustup --version 2>/dev/null | head -1)"
elif command -v cargo >/dev/null; then
    # A brew-installed rust has no target management; rustup must own the
    # toolchain for the musl cross-target.
    fail "cargo found without rustup (brew install rust?) — run 'brew uninstall rust' and re-run"
else
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
# shellcheck source=/dev/null
. "$HOME/.cargo/env"
echo "    $(rustc --version)"

# Guests are aarch64, so this is the only agent target this host can boot.
step "rust target aarch64-unknown-linux-musl"
rustup target add aarch64-unknown-linux-musl

step "zig"
brew install zig

step "cargo-zigbuild"
if command -v cargo-zigbuild >/dev/null; then
    echo "    already installed: $(cargo-zigbuild --version 2>&1 | head -1)"
else
    # Compiles from source; takes a few minutes.
    cargo install cargo-zigbuild
fi

step "brew tap libkrun/krun"
# Homebrew 6 refuses to load formulae from untrusted third-party taps.
brew trust --tap libkrun/krun
brew tap libkrun/krun

step "libkrun (pulls libkrunfw and friends)"
brew install libkrun/krun/libkrun

step "gvproxy"
if ! brew install libkrun/krun/gvproxy; then
    echo "    warning: gvproxy install failed; podman bundles one at" \
         "$BREW_PREFIX/opt/podman/libexec/gvproxy (set MVM_GVPROXY_BIN to use it)"
fi

step "verify"
[ -f "$BREW_PREFIX/lib/libkrun.dylib" ] || fail "libkrun.dylib missing after install"
[ -f "$BREW_PREFIX/include/libkrun.h" ] || fail "libkrun.h missing after install"
rustup target list --installed | grep -q aarch64-unknown-linux-musl \
    || fail "aarch64-unknown-linux-musl target missing"
zig version >/dev/null || fail "zig missing"
cargo-zigbuild --version >/dev/null || fail "cargo-zigbuild missing"

echo
echo "installed:"
echo "  rustc          $(rustc --version | cut -d' ' -f2)"
echo "  zig            $(zig version)"
echo "  cargo-zigbuild $(cargo-zigbuild --version 2>&1 | cut -d' ' -f2)"
echo "  libkrun        $BREW_PREFIX/lib/libkrun.dylib"
if command -v gvproxy >/dev/null; then
    echo "  gvproxy        $(command -v gvproxy)"
elif [ -x "$BREW_PREFIX/opt/podman/libexec/gvproxy" ]; then
    echo "  gvproxy        not on PATH; use podman's bundled one:"
    echo "                 export MVM_GVPROXY_BIN=$BREW_PREFIX/opt/podman/libexec/gvproxy"
else
    echo "  gvproxy        MISSING (managed --net gvproxy will not work)"
fi
echo
echo "Open a new shell (or: source \$HOME/.cargo/env), then:"
echo "  scripts/build.sh"
