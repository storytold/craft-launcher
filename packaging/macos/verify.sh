#!/usr/bin/env bash
# Check the macOS release artifact exactly as users download it:
#
#   $DIST/artcraft-launcher-<version>-macos-<arch>.dmg   signed, notarized, ticket stapled; the
#                                                       app inside is Developer ID signed with the
#                                                       hardened runtime and carries every arch
#
# Usage: packaging/macos/verify.sh [--arch universal|aarch64|x86_64]
#
# With a real signing identity and notarization credentials in the environment (the same
# MACOS_SIGN_IDENTITY / APPLE_ID / APPLE_PASSWORD / APPLE_TEAM_ID that package.sh uses), every
# check is strict and a failure exits non-zero. Without them (local ad-hoc builds, or a release run
# whose secrets are missing) only the signature's integrity is checked and the rest is a warning,
# matching package.sh. Nothing here reads or prints a secret's value; it only tests whether one is set.
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"

PKG=artcraft-launcher
APP_NAME="ArtCraft Launcher"

ARCH=universal
while [[ $# -gt 0 ]]; do
  case "$1" in
    --arch) ARCH="$2"; shift 2 ;;
    -h | --help) sed -n '2,14p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
case "$ARCH" in
  universal) WANT_ARCHS="x86_64 arm64" ;;
  aarch64) WANT_ARCHS="arm64" ;;
  x86_64) WANT_ARCHS="x86_64" ;;
  *) echo "unknown --arch $ARCH" >&2; exit 2 ;;
esac

DMG="$DIST/$PKG-$VERSION-macos-$ARCH.dmg"
IDENTITY="${MACOS_SIGN_IDENTITY:--}"
STRICT=0
if [[ "$IDENTITY" != "-" ]] && [[ -n "${APPLE_ID:-}" ]] && [[ -n "${APPLE_PASSWORD:-}" ]] && [[ -n "${APPLE_TEAM_ID:-}" ]]; then
  STRICT=1
fi

FAILED=0
fail() {
  if [[ "$STRICT" = 1 ]]; then
    echo "error: $*" >&2
    FAILED=1
  else
    warn "$* (expected without signing secrets)"
  fi
}

# spctl's online ticket lookup can lag a few seconds behind notarytool's "Accepted".
assess() {
  local out attempt tries=1
  [[ "$STRICT" = 1 ]] && tries=5
  for attempt in $(seq "$tries"); do
    if out="$(spctl "$@" 2>&1)"; then
      printf '%s\n' "$out"
      return 0
    fi
    [[ "$attempt" = "$tries" ]] || sleep 10
  done
  printf '%s\n' "$out"
  return 1
}

if [[ ! -f "$DMG" ]]; then
  echo "error: missing $DMG" >&2
  exit 1
fi

WORK="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/$PKG-verify.XXXXXX")"
MNT="$WORK/mnt"
cleanup() {
  hdiutil detach "$MNT" -quiet >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT

# ---- DMG ---------------------------------------------------------------------------------------
echo "==> $(basename "$DMG")"
codesign --verify --strict --verbose=2 "$DMG"
if [[ "$STRICT" = 1 ]]; then
  xcrun stapler validate "$DMG" || fail "no notarization ticket stapled to the DMG"
  assess --assess --type open --context context:primary-signature -vv "$DMG" \
    || fail "Gatekeeper rejects the DMG"
fi

# ---- the app inside ----------------------------------------------------------------------------
mkdir -p "$MNT"
hdiutil attach "$DMG" -readonly -nobrowse -noautoopen -mountpoint "$MNT" -quiet
APP="$MNT/$APP_NAME.app"
EXE="$APP/Contents/MacOS/$APP_NAME"
if [[ ! -x "$EXE" ]]; then
  echo "error: $(basename "$DMG") has no $APP_NAME.app/Contents/MacOS/$APP_NAME" >&2
  exit 1
fi
echo "==> $APP_NAME.app"

archs="$(lipo -archs "$EXE")"
for a in $WANT_ARCHS; do
  case " $archs " in
    *" $a "*) ;;
    *) echo "error: $APP_NAME lacks $a (has: $archs)" >&2; exit 1 ;;
  esac
done

if /usr/libexec/PlistBuddy -c 'Print :LSUIElement' "$APP/Contents/Info.plist" >/dev/null 2>&1; then
  echo "error: Info.plist sets LSUIElement; the launcher must stay a normal Dock app" >&2
  exit 1
fi

# Integrity first: this must hold even for ad-hoc builds, and with --strict, on the app in the
# image and on a copy installed from it with ditto. A valid DMG signature says nothing about the
# bundle inside: the old makehybrid image gave every file a com.apple.FinderInfo, which strict
# mode rejects as "detritus".
codesign --verify --strict --deep --verbose=2 "$APP"
ditto "$APP" "$WORK/copy/$APP_NAME.app"
codesign --verify --strict --deep --verbose=2 "$WORK/copy/$APP_NAME.app"
details="$(codesign -d --verbose=2 "$APP" 2>&1)"
printf '%s\n' "$details" | grep -E '^(Identifier|Format|CodeDirectory|Authority|Timestamp|TeamIdentifier)[= ]' || true

printf '%s\n' "$details" | grep -Eq '^CodeDirectory .*flags=0x[0-9a-f]+\([^)]*runtime' \
  || fail "$APP_NAME.app is not signed with the hardened runtime"
printf '%s\n' "$details" | grep -q '^Authority=Developer ID Application:' \
  || fail "$APP_NAME.app is not signed with a Developer ID Application identity"
printf '%s\n' "$details" | grep -q '^Timestamp=' \
  || fail "$APP_NAME.app signature has no secure timestamp"
if [[ -n "${APPLE_TEAM_ID:-}" ]]; then
  printf '%s\n' "$details" | grep -qx "TeamIdentifier=$APPLE_TEAM_ID" \
    || fail "$APP_NAME.app is signed by a different team"
fi
if [[ "$STRICT" = 1 ]]; then
  xcrun stapler validate "$APP" || fail "no notarization ticket stapled to $APP_NAME.app"
  if gk="$(assess --assess --type execute -vv "$APP")"; then
    printf '%s\n' "$gk"
    printf '%s\n' "$gk" | grep -q 'source=Notarized Developer ID' \
      || fail "Gatekeeper accepts $APP_NAME.app but not as notarized"
  else
    printf '%s\n' "$gk"
    fail "Gatekeeper rejects $APP_NAME.app (not notarized?)"
  fi
fi

if [[ "$FAILED" != 0 ]]; then
  echo "error: macOS artifacts failed verification" >&2
  exit 1
fi

if [[ "$STRICT" = 1 ]]; then
  result="signed (Developer ID, hardened runtime), notarized; DMG and app stapled"
else
  result="signature intact; Developer ID and notarization not required (no signing secrets)"
fi
echo "==> verified: $result"
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  echo "- macOS \`$(basename "$DMG")\`: $result" >>"$GITHUB_STEP_SUMMARY"
fi
