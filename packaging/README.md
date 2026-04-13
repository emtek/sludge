# Packaging

## Fedora 43 (RPM)

The RPM is built from `[package.metadata.generate-rpm]` in `Cargo.toml`
using the [cargo-generate-rpm](https://github.com/cat-in-136/cargo-generate-rpm)
crate — there's no traditional spec file to maintain.

### Build with podman (recommended)

```bash
./packaging/build-rpm.sh
# Output: dist/sludge-*.rpm
```

Uses `fedora:43` by default. Override with `IMAGE=fedora:42 ./packaging/build-rpm.sh`.

### Build natively

```bash
sudo dnf install cargo rust gtk4-devel libadwaita-devel openssl-devel
cargo install --locked cargo-generate-rpm
cargo build --release --locked
cargo generate-rpm
# Output: target/generate-rpm/sludge-*.rpm
```

### Install

```bash
sudo dnf install ./dist/sludge-*.rpm
```

## Ubuntu (latest) / Debian

### Build with podman (recommended)

```bash
./packaging/build-deb.sh
# Output: dist/sludge_*.deb
```

Uses `ubuntu:24.04` by default. Override with `IMAGE=ubuntu:25.04 ./packaging/build-deb.sh`.

### Build natively

Copy the packaging files into a `debian/` directory at the repo root, then build:

```bash
cp -r packaging/debian ./debian
sudo apt install debhelper cargo rustc pkg-config libgtk-4-dev libadwaita-1-dev libssl-dev
dpkg-buildpackage -us -uc -b
# Output: ../sludge_*.deb
```

### Install

```bash
sudo apt install ./dist/sludge_*.deb
```
