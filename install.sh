#!/usr/bin/env bash
# Build and install ArtCraft Launcher for the current user (no sudo, no Cargo at runtime).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CARGO="${CARGO:-cargo}"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"
APP_DIR="$DATA_DIR/applications"
ICON_ROOT="$DATA_DIR/icons/hicolor"
APP_ID="ai.storyteller.artcraft-launcher"
ENTRY="$APP_DIR/artcraft-launcher.desktop"

if ! command -v "$CARGO" >/dev/null 2>&1; then
    printf 'install: cargo was not found; install Rust first\n' >&2
    exit 1
fi

printf 'Building ArtCraft Launcher (release)…\n'
"$CARGO" build --release --manifest-path "$ROOT/Cargo.toml" -p artcraft-launcher

install -d "$BIN_DIR" "$APP_DIR"
install -m 755 "$ROOT/target/release/artcraft-launcher" "$BIN_DIR/artcraft-launcher"

# The launcher's own icon, at every size the release ships.
for icon in "$ROOT/assets/app-icon/hicolor"/*/apps/"$APP_ID".*; do
    [ -e "$icon" ] || continue
    size="$(basename "$(dirname "$(dirname "$icon")")")"
    install -d "$ICON_ROOT/$size/apps"
    install -m 644 "$icon" "$ICON_ROOT/$size/apps/$(basename "$icon")"
done

cat >"$ENTRY" <<EOF
[Desktop Entry]
Type=Application
Name=ArtCraft Launcher
GenericName=App Launcher
Comment=Discover and manage Craft applications
Exec="$BIN_DIR/artcraft-launcher"
TryExec=$BIN_DIR/artcraft-launcher
Icon=$APP_ID
Terminal=false
StartupNotify=true
StartupWMClass=$APP_ID
Categories=Utility;Development;
Keywords=artcraft;craft;launcher;installer;updater;photocraft;filmcraft;apps;
EOF
chmod 644 "$ENTRY"

# Remove the temporary ASUSCraft entry and icon from the earlier rebrand.
rm -f "$APP_DIR/asuscraft.desktop"
for size in 16x16 24x24 32x32 48x48 64x64 128x128 256x256 512x512 scalable; do
    rm -f "$ICON_ROOT/$size/apps/asuscraft.png" "$ICON_ROOT/$size/apps/asuscraft.svg"
done

# Refresh the caches when the tools exist; neither is required.
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$APP_DIR" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t -f "$ICON_ROOT" >/dev/null 2>&1 || true
fi

printf '\nInstalled ArtCraft Launcher.\n'
printf '  executable: %s\n' "$BIN_DIR/artcraft-launcher"
printf '  desktop:    %s\n' "$ENTRY"
printf '\nLaunch it from your app menu, or run: %s\n' "$BIN_DIR/artcraft-launcher"
