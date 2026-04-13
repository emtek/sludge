#!/bin/bash
# Build the .rpm package inside a podman container using cargo-generate-rpm.
# Output lands in ./dist/ at the repo root.
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
IMAGE="${IMAGE:-fedora:43}"
OUT_DIR="$REPO_DIR/dist"

mkdir -p "$OUT_DIR"

echo "Building sludge .rpm in $IMAGE ..."

podman run --rm \
    -v "$REPO_DIR:/src:ro,Z" \
    -v "$OUT_DIR:/out:Z" \
    "$IMAGE" \
    bash -euxc '
        dnf install -y \
            cargo \
            rust \
            gcc \
            pkg-config \
            gtk4-devel \
            libadwaita-devel \
            openssl-devel \
            glib2-devel \
            cairo-devel \
            pango-devel

        # Copy the read-only source into a writable build dir.
        cp -a /src /build
        cd /build
        rm -rf target

        # Build release binary, then invoke cargo-generate-rpm to produce an .rpm.
        cargo install --locked cargo-generate-rpm
        export PATH="$HOME/.cargo/bin:$PATH"
        cargo build --release --locked
        cargo generate-rpm

        # cargo-generate-rpm writes under target/generate-rpm/
        cp target/generate-rpm/*.rpm /out/
    '

echo "Done. Packages written to: $OUT_DIR"
ls -la "$OUT_DIR"/*.rpm 2>/dev/null || echo "No .rpm produced — check build output above."
