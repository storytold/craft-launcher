#!/usr/bin/env bash
# Smoke test for packaging/linux/AppRun: the AppImage's desktop integration writes
# and refreshes the host .desktop entry and icons, is idempotent, honours the opt-out
# and an unwritable HOME, and always launches the app.
#
# Runs in seconds with no build (CI: .github/workflows/packaging-lint.yml).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
APP_ID=ai.storyteller.artcraft-launcher

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Fake AppDir with the real AppRun, .desktop template, icons, and a stub binary.
APPDIR="$TMP/ArtCraftLauncher.AppDir"
mkdir -p "$APPDIR/usr/bin"
install -Dm755 "$HERE/AppRun" "$APPDIR/AppRun"
install -Dm644 "$HERE/$APP_ID.desktop" "$APPDIR/usr/share/applications/$APP_ID.desktop"
mkdir -p "$APPDIR/usr/share/icons"
cp -R "$ROOT/assets/app-icon/hicolor" "$APPDIR/usr/share/icons/"
printf '#!/bin/sh\necho "stub: $*"\n' >"$APPDIR/usr/bin/artcraft-launcher"
chmod +x "$APPDIR/usr/bin/artcraft-launcher"

APPIMAGE="$TMP/artcraft-launcher test.AppImage" # spaces on purpose: Exec must stay quoted
: >"$APPIMAGE"
export APPIMAGE
export HOME="$TMP/home"
export XDG_DATA_HOME="$HOME/.local/share"
mkdir -p "$HOME"
unset DESKTOPINTEGRATION ARTCRAFT_LAUNCHER_NO_DESKTOP_INTEGRATION

# Record cache refreshes without needing a desktop or GTK installation.
export REAL_CACHE_UPDATER
REAL_CACHE_UPDATER="$(command -v gtk-update-icon-cache || true)"
mkdir -p "$TMP/tools"
cat >"$TMP/tools/gtk-update-icon-cache" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$CACHE_CALLS"
if [ -n "$REAL_CACHE_UPDATER" ]; then
  "$REAL_CACHE_UPDATER" "$@"
fi
EOF
chmod +x "$TMP/tools/gtk-update-icon-cache"
export PATH="$TMP/tools:$PATH" CACHE_CALLS="$TMP/cache-calls"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# 1. Launch works and passes arguments through.
out="$("$APPDIR/AppRun" open file.png)" || fail "AppRun exited non-zero"
[[ "$out" = "stub: open file.png" ]] || fail "arguments not forwarded: $out"

DESKTOP="$XDG_DATA_HOME/applications/$APP_ID.desktop"
[[ -f "$DESKTOP" ]] || fail "desktop entry not installed"

# 2. Exec rewritten to the AppImage (quoted, spaces intact), TryExec dropped,
#    everything else preserved from the packaged entry.
expected_exec="Exec=\"$(readlink -f "$APPIMAGE")\""
grep -Fxq "$expected_exec" "$DESKTOP" || fail "Exec not rewritten: $(grep '^Exec=' "$DESKTOP")"
grep -q '^TryExec=' "$DESKTOP" && fail "TryExec kept, the entry would be hidden"
grep -Fxq "Icon=$APP_ID" "$DESKTOP" || fail "Icon line lost"
grep -Fxq 'Categories=Utility;PackageManager;' "$DESKTOP" || fail "metadata lines lost"
if command -v desktop-file-validate >/dev/null; then
  desktop-file-validate "$DESKTOP" || fail "generated desktop entry is invalid"
fi

# 3. Icons landed under the name the .desktop entry references.
for f in "16x16/apps/$APP_ID.png" "256x256/apps/$APP_ID.png" "scalable/apps/$APP_ID.svg"; do
  [[ -f "$XDG_DATA_HOME/icons/hicolor/$f" ]] || fail "icon missing: $f"
done
[[ -f "$CACHE_CALLS" ]] || fail "installed icons did not refresh the icon cache"
[[ "$(wc -l <"$CACHE_CALLS")" -eq 1 ]] || fail "icon cache refreshed more than once"
grep -Fxq -- "-f -t $XDG_DATA_HOME/icons/hicolor" "$CACHE_CALLS" || fail "wrong icon cache arguments"
if [[ -n "$REAL_CACHE_UPDATER" ]]; then
  [[ -f "$XDG_DATA_HOME/icons/hicolor/icon-theme.cache" ]] || fail "GTK did not build the icon cache"
fi

# 4. Idempotent: a second launch must not rewrite anything (inode unchanged).
before="$(stat -c %i "$DESKTOP")"
out="$("$APPDIR/AppRun")" || fail "second launch exited non-zero"
[[ "$out" = "stub: " ]] || fail "second launch output: $out"
after="$(stat -c %i "$DESKTOP")"
[[ "$before" = "$after" ]] || fail "desktop entry rewritten although unchanged"
[[ "$(wc -l <"$CACHE_CALLS")" -eq 1 ]] || fail "unchanged icons refreshed the cache"

# 5. Moving the AppImage refreshes Exec.
mv "$APPIMAGE" "$TMP/moved.AppImage"
export APPIMAGE="$TMP/moved.AppImage"
"$APPDIR/AppRun" >/dev/null || fail "launch after move exited non-zero"
grep -Fxq "Exec=\"$(readlink -f "$APPIMAGE")\"" "$DESKTOP" || fail "Exec not refreshed after move"
cp "$DESKTOP" "$TMP/generated-entry"

# 6. Opt-out installs nothing.
rm -rf "$XDG_DATA_HOME"
ARTCRAFT_LAUNCHER_NO_DESKTOP_INTEGRATION=1 "$APPDIR/AppRun" >/dev/null || fail "opt-out launch exited non-zero"
[[ ! -e "$XDG_DATA_HOME" ]] || fail "opt-out still integrated"

# 7. An unwritable data dir never blocks the launch.
mkdir -p "$TMP/ro" && chmod a-w "$TMP/ro"
out="$(XDG_DATA_HOME="$TMP/ro/data" "$APPDIR/AppRun" out.txt)" || fail "launch blocked by unwritable data dir"
[[ "$out" = "stub: out.txt" ]] || fail "stub not run against unwritable data dir: $out"
chmod u+w "$TMP/ro"

# 8. No $APPIMAGE (extracted AppDir): launch only, nothing installed.
rm -rf "$XDG_DATA_HOME"
env -u APPIMAGE "$APPDIR/AppRun" >/dev/null || fail "extracted-AppDir launch exited non-zero"
[[ ! -e "$XDG_DATA_HOME" ]] || fail "integrated without $APPIMAGE"

# 9. A path that would need Exec escaping (here a `$`): launch only, nothing installed.
rm -rf "$XDG_DATA_HOME"
odd="$TMP/odd\$(id).AppImage"
: >"$odd"
out="$(APPIMAGE="$odd" "$APPDIR/AppRun" x)" || fail "launch with an odd path exited non-zero"
[[ "$out" = "stub: x" ]] || fail "odd path: $out"
[[ ! -e "$XDG_DATA_HOME" ]] || fail "integrated an AppImage path that needs escaping"

# 10. Gear Lever's environment flag suppresses integration, including icon writes.
out="$(DESKTOPINTEGRATION=1 "$APPDIR/AppRun" managed)" || fail "manager launch failed"
[[ "$out" = "stub: managed" ]] || fail "manager launch lost arguments"
[[ ! -e "$XDG_DATA_HOME" ]] || fail "manager-integrated AppImage integrated again"

# 11. A manager entry under the native id keeps TryExec and its launch environment.
mkdir -p "$(dirname "$DESKTOP")"
cp "$TMP/generated-entry" "$DESKTOP"
cat >>"$DESKTOP" <<EOF
TryExec=$APPIMAGE
X-GearLever-Managed=true
EOF
cp "$DESKTOP" "$TMP/manager-entry"
"$APPDIR/AppRun" >/dev/null || fail "launch with existing manager entry failed"
cmp -s "$DESKTOP" "$TMP/manager-entry" || fail "manager metadata overwritten"

# 12. Preserve a launcher's entry customized by its user, even after a move.
sed 's/^Exec=/Exec=env DESKTOPINTEGRATION=1 /' "$TMP/generated-entry" >"$DESKTOP"
"$APPDIR/AppRun" >/dev/null || fail "launch with custom Exec failed"
grep -Fq 'Exec=env DESKTOPINTEGRATION=1' "$DESKTOP" || fail "custom launch environment overwritten"

# 12a. A plain quoted absolute wrapper is a customization, too.
wrapper="$TMP/custom launcher wrapper"
printf '#!/bin/sh\nexit 0\n' >"$wrapper"
sed "s|^Exec=.*|Exec=\"$wrapper\"|" "$TMP/generated-entry" >"$DESKTOP"
cp "$DESKTOP" "$TMP/custom-wrapper-entry"
"$APPDIR/AppRun" >/dev/null || fail "launch with quoted wrapper failed"
cmp -s "$DESKTOP" "$TMP/custom-wrapper-entry" || fail "quoted absolute wrapper overwritten"

# 12b. Unmarked legacy entries cannot establish ownership, even with a plain Exec.
legacy="$TMP/legacy.AppImage"
: >"$legacy"
sed -e '/^X-ArtCraft-Launcher-Generated-Exec=/d' -e "s|^Exec=.*|Exec=\"$legacy\"|" "$TMP/generated-entry" >"$DESKTOP"
cp "$DESKTOP" "$TMP/legacy-entry"
"$APPDIR/AppRun" >/dev/null || fail "launch with legacy entry failed"
cmp -s "$DESKTOP" "$TMP/legacy-entry" || fail "ambiguous legacy entry overwritten"

# 12c. Duplicate executable or ownership keys are ambiguous and stay untouched.
for key in Exec X-ArtCraft-Launcher-Generated-Exec; do
  cp "$TMP/generated-entry" "$DESKTOP"
  grep "^$key=" "$TMP/generated-entry" >>"$DESKTOP"
  cp "$DESKTOP" "$TMP/duplicate-entry"
  "$APPDIR/AppRun" >/dev/null || fail "launch with duplicate $key failed"
  cmp -s "$DESKTOP" "$TMP/duplicate-entry" || fail "duplicate $key entry overwritten"
done

# The uncustomized entry refreshed after the move keeps a matching ownership marker.
grep -Fxq "X-ArtCraft-Launcher-Generated-Exec=\"$(readlink -f "$APPIMAGE")\"" "$TMP/generated-entry" || fail "generated executable marker missing or stale"

# 13. Refresh a changed icon, and tolerate a cache updater that fails.
printf '#!/bin/sh\nexit 1\n' >"$TMP/tools/gtk-update-icon-cache"
icon="16x16/apps/$APP_ID.png"
printf 'stale icon' >"$XDG_DATA_HOME/icons/hicolor/$icon"
out="$("$APPDIR/AppRun" cache-failure)" || fail "cache failure blocked launch"
[[ "$out" = "stub: cache-failure" ]] || fail "cache failure lost arguments"
cmp -s "$APPDIR/usr/share/icons/hicolor/$icon" "$XDG_DATA_HOME/icons/hicolor/$icon" || fail "changed icon not refreshed"

# 14. A symlink to someone else's entry must not be replaced.
rm "$DESKTOP"
ln -s "$TMP/manager-entry" "$DESKTOP"
"$APPDIR/AppRun" >/dev/null || fail "launch with desktop symlink failed"
[[ -L "$DESKTOP" ]] || fail "desktop symlink replaced"

echo "AppRun desktop integration ok"
