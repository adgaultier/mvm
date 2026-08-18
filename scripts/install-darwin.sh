#!/usr/bin/env bash
# Install the macOS dependencies used by mvm:
#   - libkrun (+ libkrunfw) from the libkrun/krun tap
#   - zig + cargo-zigbuild for the static guestd cross-build
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
[ -x "$(command -v cargo 2>/dev/null || true)" ] || \
    fail "cargo not found; install Rust separately before cargo-zigbuild"

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

step "verify"
[ -f "$BREW_PREFIX/lib/libkrun.dylib" ] || fail "libkrun.dylib missing after install"
[ -f "$BREW_PREFIX/include/libkrun.h" ] || fail "libkrun.h missing after install"
zig version >/dev/null || fail "zig missing"
cargo-zigbuild --version >/dev/null || fail "cargo-zigbuild missing"

echo
echo "installed:"
echo "  zig            $(zig version)"
echo "  cargo-zigbuild $(cargo-zigbuild --version 2>&1 | cut -d' ' -f2)"
echo "  libkrun        $BREW_PREFIX/lib/libkrun.dylib"
echo
echo "Run scripts/build.sh to install the Rust target and build mvm."
