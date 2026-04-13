#!/bin/bash
# Build the .deb package inside a podman container.
# Output lands in ./dist/ at the repo root.
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
IMAGE="${IMAGE:-ubuntu:24.04}"
OUT_DIR="$REPO_DIR/dist"

mkdir -p "$OUT_DIR"

echo "Building sludge .deb in $IMAGE ..."

podman run --rm \
    -v "$REPO_DIR:/src:ro,Z" \
    -v "$OUT_DIR:/out:Z" \
    "$IMAGE" \
    bash -euxc '
        export DEBIAN_FRONTEND=noninteractive
        apt-get update
        # Note: Ubunt 24.04 ships rustc 1.75; we need >= 1.85 for edition 2024,
        # so install via rustup instead of apt.
        apt-get install -y --no-install-recommends \
            build-essential \
            ca-certificates \
            curl \
            pkg-config \
            debhelper \
            devscripts \
            fakeroot \
            libgtk-4-dev \
            libadwaita-1-dev \
            libglib2.0-dev \
            libcairo2-dev \
            libpango1.0-dev \
            libssl-dev

        # Install Rust via rustup to get edition-2024 support.
        curl -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal
        . "$HOME/.cargo/env"

        # Copy the read-only source into a writable build dir, then drop the
        # packaging/debian files into debian/ at the build root.
        cp -a /src /build
        cd /build
        rm -rf debian target
        cp -a packaging/debian ./debian

        # Skip the apt-based rustc build-dep check since we provide it via rustup.
        dpkg-buildpackage -us -uc -b -d

        # Deb files get written to the parent dir; grab them.
        cp /*.deb /out/ 2>/dev/null || true
        cp ../*.deb /out/ 2>/dev/null || true
    '

echo "Done. Packages written to: $OUT_DIR"
ls -la "$OUT_DIR"/*.deb 2>/dev/null || echo "No .deb produced — check build output above."
