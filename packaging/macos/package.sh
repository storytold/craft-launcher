#!/usr/bin/env bash
# Build, sign and (optionally) notarize the macOS release artifact:
#
#   $DIST/artcraft-launcher-<version>-macos-<arch>.dmg   ArtCraft Launcher.app on a drag-to-Applications DMG
#
# Usage: packaging/macos/package.sh [--arch universal|aarch64|x86_64] [--skip-build]
#
# Signing (env):
#   MACOS_SIGN_IDENTITY   codesign identity (name or SHA-1). Default "-" = ad-hoc (local testing;
#                         Gatekeeper will reject the result on other Macs). In CI, import-cert.sh sets it.
#   MACOS_KEYCHAIN        keychain holding the identity (optional)
# Notarization (env; all three needed, and a real identity):
#   APPLE_ID, APPLE_PASSWORD (app-specific password), APPLE_TEAM_ID
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
HERE="$ROOT/packaging/macos"

# ---- app-specific values ----------------------------------------------------------------------
PKG=artcraft-launcher          # cargo package and binary
APP_NAME="ArtCraft Launcher"   # display name: bundle, executable, icon, DMG volume

ARCH=universal
SKIP_BUILD=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --arch) ARCH="$2"; shift 2 ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    -h | --help) sed -n '2,13p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
case "$ARCH" in
  universal) TARGETS=(aarch64-apple-darwin x86_64-apple-darwin) ;;
  aarch64) TARGETS=(aarch64-apple-darwin) ;;
  x86_64) TARGETS=(x86_64-apple-darwin) ;;
  *) echo "unknown --arch $ARCH" >&2; exit 2 ;;
esac

# Keep in sync with LSMinimumSystemVersion in Info.plist.in.
export MACOSX_DEPLOYMENT_TARGET=11.0
IDENTITY="${MACOS_SIGN_IDENTITY:--}"
SHORT_VERSION="${VERSION%%-*}"
WORK="$CARGO_TARGET_DIR/macos-package"
APP="$WORK/$APP_NAME.app"
DMG="$DIST/$PKG-$VERSION-macos-$ARCH.dmg"

NOTARIZE=0
if [[ "$IDENTITY" = "-" ]]; then
  warn "macOS: ad-hoc signing (no MACOS_SIGN_IDENTITY); artifacts are not notarized"
elif [[ -n "${APPLE_ID:-}" ]] && [[ -n "${APPLE_PASSWORD:-}" ]] && [[ -n "${APPLE_TEAM_ID:-}" ]]; then
  NOTARIZE=1
else
  warn "macOS: APPLE_ID / APPLE_PASSWORD / APPLE_TEAM_ID incomplete; signed but not notarized"
fi

echo "==> $APP_NAME $VERSION for macOS ($ARCH), identity: $IDENTITY, notarize: $NOTARIZE"

# ---- build -------------------------------------------------------------------------------------
if [[ "$SKIP_BUILD" = 0 ]]; then
  args=()
  for t in "${TARGETS[@]}"; do args+=(--target "$t"); done
  (cd "$ROOT" && cargo build --release --locked -p "$PKG" "${args[@]}")
fi

rm -rf "$WORK"
mkdir -p "$WORK/bin"
inputs=()
for t in "${TARGETS[@]}"; do inputs+=("$CARGO_TARGET_DIR/$t/release/$PKG"); done
lipo -create -output "$WORK/bin/$PKG" "${inputs[@]}"
lipo -info "$WORK/bin/$PKG"

# ---- signing helpers ---------------------------------------------------------------------------
sign() {
  # Hardened runtime + secure timestamp for a real identity; ad-hoc can't be timestamped.
  local ts=(--timestamp)
  [[ "$IDENTITY" = "-" ]] && ts=(--timestamp=none)
  local kc=()
  [[ -n "${MACOS_KEYCHAIN:-}" ]] && kc=(--keychain "$MACOS_KEYCHAIN")
  codesign --force --sign "$IDENTITY" ${kc[@]+"${kc[@]}"} "${ts[@]}" "$@"
}

notarize() {
  local file="$1" out id status
  echo "==> notarizing $(basename "$file") (this can take a few minutes)"
  out="$(xcrun notarytool submit "$file" --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" \
    --team-id "$APPLE_TEAM_ID" --wait --timeout 1h --output-format json)" || true
  echo "$out"
  id="$(printf '%s' "$out" | plutil -extract id raw -o - - 2>/dev/null || true)"
  status="$(printf '%s' "$out" | plutil -extract status raw -o - - 2>/dev/null || true)"
  if [[ "$status" != "Accepted" ]]; then
    if [[ -n "$id" ]]; then
      xcrun notarytool log "$id" --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID" || true
    fi
    echo "error: notarization of $(basename "$file") failed (status: ${status:-unknown})" >&2
    exit 1
  fi
}

# `stapler staple` and `stapler validate` both fetch the ticket from Apple's CloudKit, and a dropped
# connection there (NSURLErrorDomain -1005, "Error 68") fails them although notarization worked.
# Both are idempotent, so retry with a pause; a missing or bad ticket still fails every try.
stapler_retry() {
  local action="$1" target="$2" try
  for try in 1 2 3 4; do
    xcrun stapler "$action" "$target" && return 0
    [[ "$try" = 4 ]] && break
    echo "stapler $action failed (try $try of 4); retrying in $((try * 20)) s" >&2
    sleep $((try * 20))
  done
  echo "error: stapler $action $(basename "$target") failed 4 times" >&2
  exit 1
}

# ---- ArtCraft Launcher.app ---------------------------------------------------------------------
echo "==> assembling $APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
# Executable and icon carry the display name (CFBundleExecutable / CFBundleIconFile).
cp "$WORK/bin/$PKG" "$APP/Contents/MacOS/$APP_NAME"
cp "$ROOT/assets/app-icon/$PKG.icns" "$APP/Contents/Resources/$APP_NAME.icns"
cp "$ROOT/NOTICE" "$APP/Contents/Resources/"
sed -e "s/@VERSION@/$VERSION/g" -e "s/@SHORT_VERSION@/$SHORT_VERSION/g" \
  -e "s/@BUILD_SHA@/${ARTCRAFT_LAUNCHER_BUILD_SHA:-unknown}/g" \
  "$HERE/Info.plist.in" >"$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist"
if /usr/libexec/PlistBuddy -c 'Print :LSUIElement' "$APP/Contents/Info.plist" >/dev/null 2>&1; then
  echo "error: Info.plist must not set LSUIElement (the launcher is a normal Dock app)" >&2
  exit 1
fi
printf 'APPL????' >"$APP/Contents/PkgInfo"

# Drop extended attributes picked up during the build (quarantine, FinderInfo) before signing:
# `codesign --verify --strict` rejects them, and nothing after signing may touch the bundle.
xattr -cr "$APP"

# Sign inside-out: nested code first, then the bundle itself (no --deep on the final signature).
# Today the only nested code is the main executable; frameworks/helpers would be signed here too.
sign --options runtime --entitlements "$HERE/entitlements.plist" "$APP/Contents/MacOS/$APP_NAME"
sign --options runtime --entitlements "$HERE/entitlements.plist" "$APP"
codesign --verify --strict --deep --verbose=2 "$APP"

if [[ "$NOTARIZE" = 1 ]]; then
  ditto -c -k --keepParent "$APP" "$WORK/notarize.zip"
  notarize "$WORK/notarize.zip"
  stapler_retry staple "$APP"
  stapler_retry validate "$APP"
  spctl --assess --type execute -vvv "$APP"
fi

# ---- DMG ---------------------------------------------------------------------------------------
echo "==> building $DMG"
STAGE="$WORK/dmg"
mkdir -p "$STAGE"
ditto "$APP" "$STAGE/$APP_NAME.app"
ln -s /Applications "$STAGE/Applications"
# Finder window layout: background, icon size and positions (packaging/macos/dmg/README.md).
mkdir -p "$STAGE/.background"
cp "$HERE/dmg/background.tiff" "$STAGE/.background/background.tiff"
cp "$HERE/dmg/dmg-layout.DS_Store" "$STAGE/.DS_Store"
rm -f "$DMG"
# `hdiutil create -srcfolder` writes the files as they are. `makehybrid -hfs` gave every file a
# com.apple.FinderInfo, which `codesign --verify --strict` rejects as "Finder information, or
# similar detritus" on the app in the image and on a copy installed from it. `create` attaches a
# device while it copies, which fails now and then on CI runners ("Resource busy"), so it gets a
# few tries. verify.sh checks the app inside the finished image with --strict. The volume name has
# no version: .DS_Store finds the background through an alias that includes it.
# Usage: hdiutil_retry <what> <hdiutil args…>; tries five times with growing pauses.
hdiutil_retry() {
  local what="$1" attempt
  shift
  for attempt in 1 2 3 4 5; do
    if hdiutil "$@"; then
      return 0
    fi
    [[ "$attempt" = 5 ]] && { echo "hdiutil $what failed 5 times" >&2; return 1; }
    warn "hdiutil $what failed (attempt $attempt of 5); retrying in $((attempt * 10))s"
    sleep $((attempt * 10))
  done
}
hdiutil_retry create create -srcfolder "$STAGE" -volname "$APP_NAME" -fs HFS+ -format UDZO -imagekey zlib-level=9 -ov "$DMG"
sign "$DMG"
codesign --verify --strict --verbose=2 "$DMG"
if [[ "$NOTARIZE" = 1 ]]; then
  notarize "$DMG"
  stapler_retry staple "$DMG"
  stapler_retry validate "$DMG"
  spctl --assess --type open --context context:primary-signature -vvv "$DMG"
fi

echo "==> done (check the artifact as shipped with packaging/macos/verify.sh --arch $ARCH)"
ls -lh "$DMG"
