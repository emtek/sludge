# Packaging

## Fedora 43 (RPM)

```bash
# From repo root
VERSION=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
git archive --prefix=sludge-$VERSION/ -o ~/rpmbuild/SOURCES/sludge-$VERSION.tar.gz HEAD

rpmbuild -ba packaging/sludge.spec
# Output: ~/rpmbuild/RPMS/x86_64/sludge-$VERSION-*.rpm
```

Install with:

```bash
sudo dnf install ~/rpmbuild/RPMS/x86_64/sludge-*.rpm
```

## Ubuntu (latest) / Debian

### Build with podman (recommended)

```bash
./packaging/build-deb.sh
# Output: dist/sludge_*.deb
```

Uses Ubuntu 24.04 by default. Override with `IMAGE=ubuntu:25.04 ./packaging/build-deb.sh`.

### Build natively

Copy the packaging files into a `debian/` directory at the repo root, then build:

```bash
cp -r packaging/debian ./debian
sudo apt install debhelper cargo rustc pkg-config libgtk-4-dev libadwaita-1-dev libssl-dev
dpkg-buildpackage -us -uc -b
# Output: ../sludge_*.deb
```

Install with:

```bash
sudo apt install ./dist/sludge_*.deb
```
