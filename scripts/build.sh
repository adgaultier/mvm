#!/usr/bin/env bash
# Build mvm release binaries into dist/.
#
# The guest agent must be a fully static binary (it runs as PID 1 inside
# guests whose libc we don't control), so it is built for the musl target;
# everything else builds for the host gnu target.
set -euo pipefail

cd "$(dirname "$0")/.."

MUSL_TARGET=x86_64-unknown-linux-musl

if ! rustup target list --installed | grep -q "$MUSL_TARGET"; then
    echo "==> adding rust target $MUSL_TARGET"
    rustup target add "$MUSL_TARGET"
fi

echo "==> building workspace (release)"
cargo build --release --workspace

echo "==> building static guest agent ($MUSL_TARGET)"
cargo build --release -p mvm-agent --target "$MUSL_TARGET"

echo "==> collecting dist/"
mkdir -p dist
cp target/release/mvm dist/
cp target/release/mvm-tui dist/
# The static agent replaces the host-linked one; mvm looks for it next to
# its own binary (or via MVM_AGENT_PATH).
cp "target/$MUSL_TARGET/release/mvm-agent" dist/

echo "==> done"
file dist/mvm-agent
ls -l dist/
