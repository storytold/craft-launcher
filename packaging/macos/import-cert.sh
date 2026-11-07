#!/usr/bin/env bash
# CI only: import the Developer ID certificate into a temporary keychain so codesign can use it.
#
# Inputs (env): APPLE_CERTIFICATE (base64 .p12), APPLE_CERTIFICATE_PASSWORD, KEYCHAIN_PASSWORD
# (optional; random if unset), RUNNER_TEMP (or TMPDIR).
# Outputs: MACOS_SIGN_IDENTITY (SHA-1 of the identity) and MACOS_KEYCHAIN, appended to
# $GITHUB_ENV when running in Actions, otherwise printed.
#
# Missing certificate: prints a warning and exits 0, so package.sh falls back to ad-hoc signing.
set -euo pipefail
umask 077
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"

if [[ -z "${APPLE_CERTIFICATE:-}" ]]; then
  warn "APPLE_CERTIFICATE is not set: macOS builds will be ad-hoc signed and not notarized"
  exit 0
fi

TMP="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
# Create a private directory we own, so a predictable name in a shared, world-writable temp root
# can't be pre-created by another local user to capture the decoded signing bundle (CWE-377). The
# keychain lives here too; later signing/notarization steps read it through $MACOS_KEYCHAIN and the
# runner discards RUNNER_TEMP when the job ends.
WORK="$(mktemp -d "$TMP/artcraft-launcher-signing.XXXXXXXX")"
KEYCHAIN="$WORK/artcraft-launcher-signing.keychain-db"
CERT="$WORK/artcraft-launcher-signing.p12"
KC_PASS="${KEYCHAIN_PASSWORD:-$(openssl rand -hex 24)}"

# The decoded .p12 is removed as soon as it is imported; the keychain must outlive this script so
# codesign can use it in a later step, so it is left in the private directory rather than deleted.
trap 'rm -f "$CERT"' EXIT
printf '%s' "$APPLE_CERTIFICATE" | base64 --decode >"$CERT"
security create-keychain -p "$KC_PASS" "$KEYCHAIN"
security set-keychain-settings -lut 21600 "$KEYCHAIN"
security unlock-keychain -p "$KC_PASS" "$KEYCHAIN"
security import "$CERT" -P "${APPLE_CERTIFICATE_PASSWORD:-}" -T /usr/bin/codesign -T /usr/bin/security -t cert -f pkcs12 -k "$KEYCHAIN"
rm -f "$CERT"
# Let codesign use the key without a UI prompt.
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$KC_PASS" "$KEYCHAIN" >/dev/null
# Put it on the search list (keeping the login keychain) so codesign and notarytool find it.
# shellcheck disable=SC2046
security list-keychains -d user -s "$KEYCHAIN" $(security list-keychains -d user | tr -d '"')

IDENTITY="$(security find-identity -v -p codesigning "$KEYCHAIN" | awk '/Developer ID Application/ { print $2; exit }')"
if [[ -z "$IDENTITY" ]]; then
  security find-identity -v -p codesigning "$KEYCHAIN" >&2
  echo "error: no 'Developer ID Application' identity in APPLE_CERTIFICATE" >&2
  exit 1
fi
echo "Imported signing identity $IDENTITY"
if [[ -n "${GITHUB_ENV:-}" ]]; then
  { echo "MACOS_SIGN_IDENTITY=$IDENTITY"; echo "MACOS_KEYCHAIN=$KEYCHAIN"; } >>"$GITHUB_ENV"
else
  echo "MACOS_SIGN_IDENTITY=$IDENTITY"
  echo "MACOS_KEYCHAIN=$KEYCHAIN"
fi
