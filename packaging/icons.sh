#!/usr/bin/env bash
# Regenerate every app and tray icon from the ArtCraft "A" mark in docs/brand/, rendered exactly as
# supplied. The mark is an ArtCraft trademark, NOT MIT/Apache: docs/brand/LICENSE-brand.txt.
#
# - App icon (Dock, taskbar, Start menu, Linux menus, installers): the blue mark, which is also
#   getartcraft.com's favicon.
# - Tray: the black mark as the macOS menu-bar template; the blue mark on Windows, Linux and BSD.
#
# Needs: resvg (brew install resvg / cargo install resvg). The .ico and .icns are packed by
# `cargo xtask ico` / `cargo xtask icns`, so this runs on any OS. The outputs are committed, so
# packaging (and the Rust build, which include_bytes!s some of them) never needs these tools.
#
#   packaging/icons.sh
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIR="$ROOT/assets/app-icon"
BRAND="$ROOT/docs/brand"
SVG="$BRAND/artcraft-mark.svg"
ID="ai.storyteller.artcraft-launcher"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

command -v resvg >/dev/null || { echo "error: resvg not found (brew install resvg)" >&2; exit 1; }

# The mark's artwork is untouched. On macOS it sits on a light squircle tile drawn on Apple's
# 1024 icon grid (824 px tile, ~185 px corners). A bare mark is a "legacy" icon to macOS 26, which
# puts it on a grey tile in the Dock while the app is closed but shows it bare while it runs
# (issue #293); a tile of our own looks the same in both states. Everything else uses the file as is.
BOX='viewBox="-10 -19.42 136.34 136.34"'
grep -q "$BOX" "$SVG" || { echo "error: expected $BOX in $SVG" >&2; exit 1; }
MAC="$TMP/mac.svg"
{
  echo '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">'
  echo '  <rect x="100" y="100" width="824" height="824" rx="185.4" fill="#ffffff"/>'
  sed -e '/^<?xml/d' -e "s/<svg /<svg x=\"232\" y=\"232\" width=\"560\" height=\"560\" /" "$SVG"
  echo '</svg>'
} >"$MAC"
FLAT="$SVG"

render() { resvg -w "$2" -h "$2" "$1" "$3" </dev/null; }

render "$MAC" 1024 "$DIR/artcraft-launcher-1024.png"

# Linux hicolor theme.
for s in 16 24 32 48 64 128 256 512; do
  mkdir -p "$DIR/hicolor/${s}x${s}/apps"
  render "$FLAT" "$s" "$DIR/hicolor/${s}x${s}/apps/$ID.png"
done
mkdir -p "$DIR/hicolor/scalable/apps"
cp "$SVG" "$DIR/hicolor/scalable/apps/$ID.svg"
cp "$SVG" "$DIR/artcraft-launcher.svg"

# Windows .ico.
ICO_PNGS=()
for s in 16 20 24 32 40 48 64 128 256; do
  render "$FLAT" "$s" "$TMP/ico-$s.png"
  ICO_PNGS+=("$TMP/ico-$s.png")
done
(cd "$ROOT" && cargo run -q -p xtask -- ico "$DIR/artcraft-launcher.ico" "${ICO_PNGS[@]}")

# macOS .icns.
ICNS_PNGS=()
for s in 16 32 64 128 256 512 1024; do
  render "$MAC" "$s" "$TMP/icns-$s.png"
  ICNS_PNGS+=("$TMP/icns-$s.png")
done
(cd "$ROOT" && cargo run -q -p xtask -- icns "$DIR/artcraft-launcher.icns" "${ICNS_PNGS[@]}")

# Tray icons: the black mark is the macOS menu-bar template (44 px = 22 pt @2x; macOS tints it);
# the blue one is for the Windows notification area and Linux/BSD status notifiers.
render "$BRAND/artcraft-mark-black.svg" 44 "$BRAND/artcraft-mark-black-44.png"
render "$BRAND/artcraft-mark.svg" 64 "$BRAND/artcraft-mark-64.png"
echo "icons written to $DIR"
