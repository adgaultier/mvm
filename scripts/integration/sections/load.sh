#!/usr/bin/env bash
set -euo pipefail
echo "== load (OCI image layout archive) =="
# Build a minimal image with podman, save it as an oci-archive, and load it
# with `mvm load` — the no-registry path for getting images in. Gated on
# podman (and its machine being up): `podman build` needs to reach docker.io
# for the base image, so a cold/missing podman machine skips rather than fails.
if command -v podman >/dev/null 2>&1; then
    BUILD_DIR=$(mktemp -d /tmp/mvm-itest-load.XXXXXX)
    cat > "$BUILD_DIR/Dockerfile" <<'EOF'
FROM alpine:3.20
RUN echo load-marker > /mvm-load.txt
EOF
    if podman build -q -t mvm-itest-load:latest "$BUILD_DIR" >/dev/null 2>&1 \
        && podman save --format oci-archive mvm-itest-load:latest \
            -o "$BUILD_DIR/load.tar" 2>/dev/null; then
        # mvm keeps its own copy in the store; don't leave a podman image.
        podman rmi mvm-itest-load:latest >/dev/null 2>&1 || true

        "$MVM" load --name itest-load:latest "$BUILD_DIR/load.tar" >/dev/null 2>&1
        check "load lists the image" "1" "$("$MVM" images | grep -c itest-load)"
        # The marker proves the archive actually unpacked and the image boots.
        # --rm: don't leave the sandbox behind (run keeps by default), or the
        # later `ps -a | grep itest` lifecycle checks would match its image.
        check "loaded image runs" "load-marker" \
            "$("$MVM" run --rm itest-load:latest cat /mvm-load.txt 2>/dev/null | tr -d '\r')"
    else
        skip "podman build/save (podman machine down or no network)"
    fi
    rm -rf "$BUILD_DIR"
else
    skip "podman not installed (load check)"
fi
echo
echo "$PASS passed, $SKIP skipped, $FAIL failed"
[ "$FAIL" -eq 0 ]
