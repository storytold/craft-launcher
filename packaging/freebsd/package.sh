#!/usr/bin/env bash
# Build and package ArtCraft Launcher for FreeBSD:
#
#   $DIST/artcraft-launcher-<version>-freebsd-x86_64.tar.gz   a /usr/local-style tree:
#       artcraft-launcher-<version>-freebsd-x86_64/{bin, share/applications, share/icons,
#       share/metainfo, share/doc/artcraft-launcher}
#
# Install by copying the tree's contents into /usr/local:
#   tar -xzf artcraft-launcher-<version>-freebsd-x86_64.tar.gz --strip-components 1 -C /usr/local
#
# Usage: packaging/freebsd/package.sh [--skip-build] [--dry-run]
#   --dry-run   stage the tree from a stub binary and list it, without building (works on any OS)
#
# Needs: bash, cargo, and the packages installed in .github/workflows/freebsd.yml.
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
LINUX="$ROOT/packaging/linux"
APP_ID=ai.storyteller.artcraft-launcher
PKG=artcraft-launcher

SKIP_BUILD=0
DRY_RUN=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-build) SKIP_BUILD=1; shift ;;
    --dry-run) DRY_RUN=1; SKIP_BUILD=1; shift ;;
    -h | --help) sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# FreeBSD's uname says amd64; release file names use x86_64 like the Linux and Windows ones.
release_arch() {
  local arch="$1"
  case "$arch" in
    amd64 | x86_64) echo x86_64 ;;
    arm64 | aarch64) echo aarch64 ;;
    *) echo "unsupported architecture $arch" >&2; return 2 ;;
  esac
  return
}
# The release workflow cross-compiles aarch64 in an x86_64 VM with CARGO_BUILD_TARGET set to
# aarch64-unknown-freebsd (craft-common-actions#39): name the archive after the target.
case "${CARGO_BUILD_TARGET:-}" in
  "") ARCH="$(release_arch "$(uname -m)")" ;;
  aarch64-unknown-freebsd | x86_64-unknown-freebsd) ARCH="$(release_arch "${CARGO_BUILD_TARGET%%-*}")" ;;
  *) echo "CARGO_BUILD_TARGET=$CARGO_BUILD_TARGET is not a supported FreeBSD target" >&2; exit 2 ;;
esac
if [[ "$DRY_RUN" = 0 ]] && [[ "$(uname -s)" != FreeBSD ]]; then
  echo "error: build on FreeBSD (or pass --dry-run to check the tree layout)" >&2
  exit 2
fi
BASENAME="$PKG-$VERSION-freebsd-$ARCH"

echo "==> ArtCraft Launcher $VERSION for FreeBSD $ARCH"

export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
if [[ "$SKIP_BUILD" = 0 ]]; then
  (cd "$ROOT" && cargo build --release --locked -p "$PKG" ${CARGO_BUILD_TARGET:+--target "$CARGO_BUILD_TARGET"})
fi
WORK="$CARGO_TARGET_DIR/freebsd-package"
STAGE="$WORK/$BASENAME"
rm -rf "$WORK"
mkdir -p "$WORK"

# Cargo puts a cross build under target/<triple>/release.
BIN="$CARGO_TARGET_DIR/${CARGO_BUILD_TARGET:+$CARGO_BUILD_TARGET/}release"
OUT_DIR="$DIST"
if [[ "$DRY_RUN" = 1 ]]; then
  # A stub binary, and the tarball stays in the work dir so dist/ only ever holds real builds.
  BIN="$WORK/stub-bin"
  OUT_DIR="$WORK"
  mkdir -p "$BIN"
  printf '#!/bin/sh\necho "%s %s (dry-run stub)"\n' "$PKG" "$VERSION" >"$BIN/$PKG"
  chmod 755 "$BIN/$PKG"
fi

# ---- stage a /usr/local-style tree --------------------------------------------------------------
# FreeBSD's install(1) has no -D: create the directories first.
mkdir -p "$STAGE/bin" "$STAGE/share/applications" "$STAGE/share/metainfo" "$STAGE/share/icons" \
  "$STAGE/share/doc/$PKG"
install -m 755 "$BIN/$PKG" "$STAGE/bin/"
strip "$STAGE/bin/$PKG" 2>/dev/null || true
# The desktop entry, metainfo and icons are the freedesktop files Linux ships.
install -m 644 "$LINUX/$APP_ID.desktop" "$STAGE/share/applications/$APP_ID.desktop"
sed -e "s/@VERSION@/$VERSION/g" -e "s/@DATE@/$ARTCRAFT_LAUNCHER_BUILD_DATE/g" \
  "$LINUX/$APP_ID.metainfo.xml.in" >"$STAGE/share/metainfo/$APP_ID.metainfo.xml"
cp -R "$ROOT/assets/app-icon/hicolor" "$STAGE/share/icons/"
copy_docs "$STAGE/share/doc/$PKG"

for f in "bin/$PKG" "share/applications/$APP_ID.desktop" \
  "share/icons/hicolor/256x256/apps/$APP_ID.png" "share/icons/hicolor/scalable/apps/$APP_ID.svg" \
  "share/doc/$PKG/LICENSE-MIT" "share/doc/$PKG/LICENSE-APACHE"; do
  if [[ ! -e "$STAGE/$f" ]]; then echo "error: $f is missing from the package" >&2; exit 1; fi
done

# ---- .tar.gz ------------------------------------------------------------------------------------
mkdir -p "$OUT_DIR"
tar -C "$WORK" -czf "$OUT_DIR/$BASENAME.tar.gz" "$BASENAME"
echo "wrote $OUT_DIR/$BASENAME.tar.gz"

if [[ "$DRY_RUN" = 1 ]]; then
  echo "==> dry run: tarball contents"
  tar -tzvf "$OUT_DIR/$BASENAME.tar.gz"
fi
echo "==> done"
