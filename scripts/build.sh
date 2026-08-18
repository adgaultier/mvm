#!/usr/bin/env bash
# Build mvm release binaries into dist/.
#
# The guestd must be a fully static binary (it runs as PID 1 inside
# guests whose libc we don't control), so it is built for the musl target;
# everything else builds for the host target. Guests are same-arch as the
# host (KVM and Hypervisor.framework both are), so the musl target follows
# the host arch. On macOS the cross-link needs zig (scripts/install-darwin.sh).
set -euo pipefail

cd "$(dirname "$0")/.."

case "$(uname -m)" in
    arm64|aarch64) MUSL_TARGET=aarch64-unknown-linux-musl ;;
    x86_64) MUSL_TARGET=x86_64-unknown-linux-musl ;;
    *) echo "unsupported host arch: $(uname -m)" >&2; exit 1 ;;
esac

if ! rustup target list --installed | grep -q "$MUSL_TARGET"; then
    echo "==> adding rust target $MUSL_TARGET"
    rustup target add "$MUSL_TARGET"
fi

echo "==> building workspace (release)"
cargo build --release --workspace

echo "==> building static guestd ($MUSL_TARGET)"
if [ "$(uname -s)" = Darwin ]; then
    command -v cargo-zigbuild >/dev/null || {
        echo "error: cargo-zigbuild required to cross-compile the guestd on macOS" >&2
        echo "       (run scripts/install-darwin.sh)" >&2
        exit 1
    }
    cargo zigbuild --release -p mvm-guestd --target "$MUSL_TARGET"
else
    cargo build --release -p mvm-guestd --target "$MUSL_TARGET"
fi

echo "==> collecting dist/"
mkdir -p dist
cp target/release/mvm dist/
cp target/release/mvm-tui dist/
# The static guestd replaces the host-linked one; mvm looks for it next to
# its own binary (or via MVM_GUESTD_PATH).
cp "target/$MUSL_TARGET/release/mvm-guestd" dist/

if [ "$(uname -s)" = Darwin ]; then
    # Hypervisor.framework refuses binaries without the hypervisor
    # entitlement (krun_start_enter fails with EINVAL).
    codesign --force --sign - --entitlements scripts/hypervisor.entitlements dist/mvm
fi

echo "==> done"
file dist/mvm-guestd
ls -l dist/
