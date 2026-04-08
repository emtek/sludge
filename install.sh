#!/bin/bash
set -euo pipefail

PREFIX="${PREFIX:-$HOME/.local}"
BINDIR="$PREFIX/bin"
DATADIR="$PREFIX/share"
SCRIPTDIR="$(cd "$(dirname "$0")" && pwd)"

echo "Installing Sludge to $PREFIX"

# Build and install binary
echo "Building and installing binary..."
cargo install --path "$SCRIPTDIR" --root "$PREFIX"

BINARY="$BINDIR/sludge"

# Desktop file (rewrite Exec path to installed binary)
echo "Installing desktop file..."
sed "s|^Exec=.*|Exec=$BINARY|" \
    "$SCRIPTDIR/assets/dev.sludge.app.desktop" | \
    install -Dm644 /dev/stdin "$DATADIR/applications/dev.sludge.app.desktop"

# Icons
echo "Installing icons..."
for size in 48 64 128 256; do
    install -Dm644 "$SCRIPTDIR/assets/hicolor/${size}x${size}/apps/sludge.png" \
        "$DATADIR/icons/hicolor/${size}x${size}/apps/sludge.png"
done

# GNOME Shell search provider
# GNOME Shell only scans system data dirs (not XDG_DATA_HOME) for search providers,
# so this must go into a system-wide location.
echo "Installing search provider..."
sudo install -Dm644 "$SCRIPTDIR/assets/dev.sludge.app.search-provider.ini" \
    /usr/local/share/gnome-shell/search-providers/dev.sludge.app.search-provider.ini

# D-Bus services (rewrite Exec path to installed binary)
echo "Installing D-Bus services..."
sed "s|^Exec=.*|Exec=$BINARY|" \
    "$SCRIPTDIR/assets/dev.sludge.app.service" | \
    install -Dm644 /dev/stdin "$DATADIR/dbus-1/services/dev.sludge.app.service"
sed "s|^Exec=.*|Exec=$BINARY --search-provider|" \
    "$SCRIPTDIR/assets/dev.sludge.app.SearchProvider.service" | \
    install -Dm644 /dev/stdin "$DATADIR/dbus-1/services/dev.sludge.app.SearchProvider.service"

# Update icon cache if available
if command -v gtk-update-icon-cache &>/dev/null; then
    echo "Updating icon cache..."
    gtk-update-icon-cache -f -t "$DATADIR/icons/hicolor" 2>/dev/null || true
fi

# Update desktop database so GNOME Settings discovers the search provider
if command -v update-desktop-database &>/dev/null; then
    echo "Updating desktop database..."
    update-desktop-database "$DATADIR/applications" 2>/dev/null || true
fi

echo "Done! Sludge installed to $PREFIX"
echo "Make sure $BINDIR is in your PATH."
