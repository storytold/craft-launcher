#!/usr/bin/env bash
# Remove the files install.sh put in the user's XDG directories. Nothing else is touched.
set -euo pipefail

BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"
APP_DIR="$DATA_DIR/applications"
ICON_ROOT="$DATA_DIR/icons/hicolor"
APP_ID="ai.storyteller.artcraft-launcher"
ENTRY="$APP_DIR/artcraft-launcher.desktop"

rm -f "$BIN_DIR/artcraft-launcher"
rm -f "$ENTRY"
# The earlier ASUSCraft rebrand's entry, if this machine ever got it.
rm -f "$APP_DIR/asuscraft.desktop"

for size in 16x16 24x24 32x32 48x48 64x64 128x128 256x256 512x512 scalable; do
    rm -f "$ICON_ROOT/$size/apps/$APP_ID.png" "$ICON_ROOT/$size/apps/$APP_ID.svg"
    rm -f "$ICON_ROOT/$size/apps/asuscraft.png" "$ICON_ROOT/$size/apps/asuscraft.svg"
    rmdir "$ICON_ROOT/$size/apps" 2>/dev/null || true
    rmdir "$ICON_ROOT/$size" 2>/dev/null || true
done

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$APP_DIR" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t -f "$ICON_ROOT" >/dev/null 2>&1 || true
fi

printf 'Uninstalled ArtCraft Launcher (settings and user data were left alone).\n'
