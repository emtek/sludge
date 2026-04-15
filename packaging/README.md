# Packaging

Both formats are driven entirely by metadata in `Cargo.toml` — no spec files
or debian/ directories to maintain.

## RPM (Fedora / RHEL)

Uses [cargo-generate-rpm](https://github.com/cat-in-136/cargo-generate-rpm).

```bash
cargo install cargo-generate-rpm   # one-time
cargo build --release
cargo generate-rpm
# Output: target/generate-rpm/sludge-*.rpm
sudo dnf install target/generate-rpm/sludge-*.rpm
```

## DEB (Debian / Ubuntu)

Uses [cargo-deb](https://github.com/kornelski/cargo-deb).

```bash
cargo install cargo-deb   # one-time
cargo build --release
cargo deb --no-build
# Output: target/debian/sludge_*.deb
sudo apt install target/debian/sludge_*.deb
```
