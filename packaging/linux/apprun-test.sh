#!/usr/bin/env bash
# Smoke test for packaging/linux/AppRun: the AppImage's desktop integration writes
# and refreshes the host .desktop entry and icons, is idempotent, honours the opt-out
# and an unwritable HOME, and always launches the app (#593).
#
# Runs in seconds with no build (CI: .github/workflows/packaging-lint.yml).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
APP_ID=ai.storyteller.photocraft

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Fake AppDir with the real AppRun, .desktop template, icons, and a stub binary.
APPDIR="$TMP/PhotoCraft.AppDir"
mkdir -p "$APPDIR/usr/bin"
install -Dm755 "$HERE/AppRun" "$APPDIR/AppRun"
install -Dm644 "$HERE/$APP_ID.desktop" "$APPDIR/usr/share/applications/$APP_ID.desktop"
mkdir -p "$APPDIR/usr/share/icons"
cp -R "$ROOT/assets/app-icon/hicolor" "$APPDIR/usr/share/icons/"
printf '#!/bin/sh\necho "stub: $*"\n' >"$APPDIR/usr/bin/photocraft"
chmod +x "$APPDIR/usr/bin/photocraft"

APPIMAGE="$TMP/photocraft test.AppImage" # spaces on purpose: Exec must stay quoted
: >"$APPIMAGE"
export APPIMAGE
export HOME="$TMP/home"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_CACHE_HOME="$HOME/.cache"
export XDG_STATE_HOME="$HOME/.local/state"
export XDG_DATA_DIRS="$TMP/system-data"
unset DESKTOPINTEGRATION PHOTOCRAFT_NO_DESKTOP_INTEGRATION PHOTOCRAFT_TEST_FAILURE
mkdir -p "$HOME"

# Log cache updates and inject failures without touching the host desktop. These
# wrappers affect only AppRun subprocesses, and forward ordinary calls unchanged.
export PHOTOCRAFT_TEST_CACHE_LOG="$TMP/cache-updates"
: >"$PHOTOCRAFT_TEST_CACHE_LOG"
PHOTOCRAFT_TEST_REAL_SED="$(command -v sed)"
PHOTOCRAFT_TEST_REAL_CMP="$(command -v cmp)"
PHOTOCRAFT_TEST_REAL_RM="$(command -v rm)"
PHOTOCRAFT_TEST_REAL_MKTEMP="$(command -v mktemp)"
export PHOTOCRAFT_TEST_REAL_SED PHOTOCRAFT_TEST_REAL_CMP PHOTOCRAFT_TEST_REAL_RM PHOTOCRAFT_TEST_REAL_MKTEMP
mkdir -p "$TMP/bin"
cat >"$TMP/bin/update-desktop-database" <<'SH'
#!/bin/sh
printf '%s\n' "$*" >>"$PHOTOCRAFT_TEST_CACHE_LOG"
SH
cat >"$TMP/tool-wrapper" <<'SH'
#!/bin/sh
tool="${0##*/}"
case "${PHOTOCRAFT_TEST_FAILURE:-}:$tool" in
  generation:sed | temporary:mktemp) exit 1 ;;
  rendering:sed)
    if [ "${1:-}" = "-e" ]; then
      printf '[Desktop Entry]\n'
      exit 1
    fi ;;
  comparison:cmp)
    [ "${3:-}" != "$PHOTOCRAFT_TEST_DESKTOP" ] || exit 2 ;;
  removal:rm)
    [ "${2:-}" != "$PHOTOCRAFT_TEST_DESKTOP" ] || exit 1 ;;
esac
case "$tool" in
  sed) exec "$PHOTOCRAFT_TEST_REAL_SED" "$@" ;;
  cmp) exec "$PHOTOCRAFT_TEST_REAL_CMP" "$@" ;;
  rm) exec "$PHOTOCRAFT_TEST_REAL_RM" "$@" ;;
  mktemp) exec "$PHOTOCRAFT_TEST_REAL_MKTEMP" "$@" ;;
esac
SH
for tool in sed cmp rm mktemp; do
  install -m755 "$TMP/tool-wrapper" "$TMP/bin/$tool"
done
chmod +x "$TMP/bin/update-desktop-database"
export PATH="$TMP/bin:$PATH"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# 1. Launch works and passes arguments through.
out="$("$APPDIR/AppRun" open file.png)" || fail "AppRun exited non-zero"
[ "$out" = "stub: open file.png" ] || fail "arguments not forwarded: $out"

DESKTOP="$XDG_DATA_HOME/applications/$APP_ID.desktop"
export PHOTOCRAFT_TEST_DESKTOP="$DESKTOP"
[ -f "$DESKTOP" ] || fail "desktop entry not installed"

# 2. Exec rewritten to the AppImage (quoted, spaces intact), TryExec dropped,
#    everything else preserved from the packaged entry.
expected_exec="Exec=\"$(readlink -f "$APPIMAGE")\" %F"
grep -Fxq "$expected_exec" "$DESKTOP" || fail "Exec not rewritten: $(grep '^Exec=' "$DESKTOP")"
grep -q '^TryExec=' "$DESKTOP" && fail "TryExec kept, the entry would be hidden"
grep -Fxq "Icon=$APP_ID" "$DESKTOP" || fail "Icon line lost"
grep -Fxq "X-AppImage-Integrate=false" "$DESKTOP" || fail "X-AppImage-Integrate=false missing (#3623)"
grep -Fxq 'X-PhotoCraft-AppImage-Managed=true' "$DESKTOP" || fail "AppRun ownership marker missing"
grep -q '^X-PhotoCraft-AppImage-Managed=' "$HERE/$APP_ID.desktop" && fail "marker leaked into shared template"
grep -Fxq 'Categories=Graphics;2DGraphics;RasterGraphics;Photography;' "$DESKTOP" || fail "metadata lines lost"
if command -v desktop-file-validate >/dev/null; then
  desktop-file-validate "$DESKTOP" || fail "generated desktop entry is invalid"
fi

# 3. Icons landed under the name the .desktop entry references.
for f in "16x16/apps/$APP_ID.png" "256x256/apps/$APP_ID.png" "scalable/apps/$APP_ID.svg"; do
  [ -f "$XDG_DATA_HOME/icons/hicolor/$f" ] || fail "icon missing: $f"
done

# 4. Idempotent: a second launch must not rewrite anything (inode unchanged).
before="$(stat -c %i "$DESKTOP")"
updates="$(wc -l <"$PHOTOCRAFT_TEST_CACHE_LOG")"
out="$("$APPDIR/AppRun")" || fail "second launch exited non-zero"
[ "$out" = "stub: " ] || fail "second launch output: $out"
after="$(stat -c %i "$DESKTOP")"
[ "$before" = "$after" ] || fail "desktop entry rewritten although unchanged"
[ "$updates" = "$(wc -l <"$PHOTOCRAFT_TEST_CACHE_LOG")" ] || fail "unchanged launch updated cache"

# 5. Moving the AppImage refreshes Exec.
mv "$APPIMAGE" "$TMP/moved.AppImage"
export APPIMAGE="$TMP/moved.AppImage"
"$APPDIR/AppRun" >/dev/null || fail "launch after move exited non-zero"
grep -Fxq "Exec=\"$(readlink -f "$APPIMAGE")\" %F" "$DESKTOP" || fail "Exec not refreshed after move"

# 6. External AppImage integrator (#3623): if another .desktop file in $XDG_DATA_HOME/applications
#    already launches this AppImage (e.g. AppImageLauncher's appimagekit-*.desktop), AppRun removes
#    its own duplicate .desktop entry while keeping the hicolor icons installed.
ext_desktop="$XDG_DATA_HOME/applications/appimagekit-photocraft.desktop"
printf '[Desktop Entry]\nType=Application\nName=PhotoCraft\nExec="%s" %%F\nIcon=%s\n' "$(readlink -f "$APPIMAGE")" "$APP_ID" >"$ext_desktop"
cp "$DESKTOP" "$TMP/managed.desktop"
cp "$ext_desktop" "$TMP/external.desktop"
updates="$(wc -l <"$PHOTOCRAFT_TEST_CACHE_LOG")"
"$APPDIR/AppRun" >/dev/null || fail "launch with external integrator desktop entry exited non-zero"
[ ! -e "$DESKTOP" ] || fail "duplicate desktop entry kept alongside external integrator entry"
[ -f "$ext_desktop" ] || fail "external integrator desktop entry removed"
[ -f "$XDG_DATA_HOME/icons/hicolor/256x256/apps/$APP_ID.png" ] || fail "icons missing with external integrator"
[ "$((updates + 1))" = "$(wc -l <"$PHOTOCRAFT_TEST_CACHE_LOG")" ] || fail "successful removal did not update cache once"
updates="$(wc -l <"$PHOTOCRAFT_TEST_CACHE_LOG")"
"$APPDIR/AppRun" >/dev/null || fail "repeated external launch exited non-zero"
[ ! -e "$DESKTOP" ] || fail "external integration recreated duplicate"
[ "$updates" = "$(wc -l <"$PHOTOCRAFT_TEST_CACHE_LOG")" ] || fail "repeated external launch updated cache"

# Preservation applies only to the external-integration branch; normal launcher
# updates are intentionally left to #2929. Compare all bytes, inode and cache calls.
assert_preserved() {
  cp "$DESKTOP" "$TMP/preserved.desktop"
  local original_inode original_updates
  original_inode="$(stat -c %i "$DESKTOP")"
  original_updates="$(wc -l <"$PHOTOCRAFT_TEST_CACHE_LOG")"
  "$APPDIR/AppRun" >/dev/null || fail "preservation launch exited non-zero"
  "$PHOTOCRAFT_TEST_REAL_CMP" -s "$TMP/preserved.desktop" "$DESKTOP" || fail "launcher modified or removed"
  [ "$original_inode" = "$(stat -c %i "$DESKTOP")" ] || fail "preserved launcher replaced"
  [ "$original_updates" = "$(wc -l <"$PHOTOCRAFT_TEST_CACHE_LOG")" ] || fail "preservation updated cache"
  cmp -s "$TMP/external.desktop" "$ext_desktop" || fail "external launcher modified"
  [ -f "$XDG_DATA_HOME/icons/hicolor/256x256/apps/$APP_ID.png" ] || fail "preservation lost icons"
  if compgen -G "$XDG_DATA_HOME/applications/.$APP_ID.tmp.*" >/dev/null; then
    fail "temporary candidate left behind"
  fi
}

for customization in exec icon tryexec comment deletion; do
  cp "$TMP/managed.desktop" "$DESKTOP"
  case "$customization" in
    exec) sed -i 's/^Exec=/Exec=env WAYLAND_DISPLAY= /; s/ %F$/ --custom %F/' "$DESKTOP" ;;
    icon) sed -i 's/^Icon=.*/Icon=custom-icon/' "$DESKTOP" ;;
    tryexec) printf 'TryExec=/custom/wrapper\n' >>"$DESKTOP" ;;
    comment) printf '# User customization\n' >>"$DESKTOP" ;;
    deletion) sed -i '/^Comment=/d' "$DESKTOP" ;;
  esac
  assert_preserved
  DESKTOPINTEGRATION=AppImageLauncher assert_preserved
done

# Even a byte-identical legacy launcher without the marker has unknown origin.
sed '/^X-PhotoCraft-AppImage-Managed=/d' "$TMP/managed.desktop" >"$DESKTOP"
assert_preserved
DESKTOPINTEGRATION=AppImageLauncher assert_preserved
printf '[Desktop Entry]\nType=Application\nName=Other app\nExec=/custom/wrapper %%F\n' >"$DESKTOP"
assert_preserved

# Links are preserved even when their target matches the generated candidate.
rm -f "$DESKTOP"
ln -s "$TMP/managed.desktop" "$DESKTOP"
assert_preserved
[ -L "$DESKTOP" ] || fail "symlink removed"
DESKTOPINTEGRATION=AppImageLauncher assert_preserved
cmp -s "$TMP/preserved.desktop" "$TMP/managed.desktop" || fail "symlink target modified"
rm -f "$DESKTOP"
ln -s "$TMP/missing.desktop" "$DESKTOP"
"$APPDIR/AppRun" >/dev/null || fail "dangling symlink launch exited non-zero"
[ -L "$DESKTOP" ] || fail "dangling symlink removed"
rm -f "$DESKTOP"

# Generation, comparison, removal and temporary-file failures must retain the
# intact launcher, skip cache updates, clean candidates and still launch the app.
for failure in generation rendering comparison removal temporary; do
  cp "$TMP/managed.desktop" "$DESKTOP"
  rm -f "$XDG_DATA_HOME/icons/hicolor/256x256/apps/$APP_ID.png"
  PHOTOCRAFT_TEST_FAILURE="$failure" assert_preserved
done

# Opt-out also protects an existing intact launcher when an integrator is present.
PHOTOCRAFT_NO_DESKTOP_INTEGRATION=1 DESKTOPINTEGRATION=AppImageLauncher assert_preserved

# A candidate for another path or changed packaged metadata is not proof that an
# old launcher is untouched. Both cases remain conservative without a baseline.
APPIMAGE="$TMP/photocraft test.AppImage"
: >"$APPIMAGE"
DESKTOPINTEGRATION=AppImageLauncher assert_preserved
APPIMAGE="$TMP/moved.AppImage"
src_desktop="$APPDIR/usr/share/applications/$APP_ID.desktop"
cp "$src_desktop" "$TMP/template.desktop"
sed -i 's/^Comment=.*/Comment=Updated package/' "$src_desktop"
assert_preserved
cp "$TMP/template.desktop" "$src_desktop"

# DESKTOPINTEGRATION alone can remove an intact launcher without an external file.
rm -f "$ext_desktop"
cp "$TMP/managed.desktop" "$DESKTOP"
DESKTOPINTEGRATION=AppImageLauncher "$APPDIR/AppRun" >/dev/null || fail "integrator removal exited non-zero"
[ ! -e "$DESKTOP" ] || fail "DESKTOPINTEGRATION kept intact duplicate"

# 7. DESKTOPINTEGRATION set by an external integrator skips creating a second .desktop entry.
rm -rf "$XDG_DATA_HOME"
DESKTOPINTEGRATION=AppImageLauncher "$APPDIR/AppRun" >/dev/null || fail "DESKTOPINTEGRATION launch exited non-zero"
[ ! -e "$DESKTOP" ] || fail "DESKTOPINTEGRATION still wrote duplicate desktop entry"
[ -f "$XDG_DATA_HOME/icons/hicolor/256x256/apps/$APP_ID.png" ] || fail "icons not installed under DESKTOPINTEGRATION"

# 8. Opt-out installs nothing.
rm -rf "$XDG_DATA_HOME"
PHOTOCRAFT_NO_DESKTOP_INTEGRATION=1 "$APPDIR/AppRun" >/dev/null || fail "opt-out launch exited non-zero"
[ ! -e "$XDG_DATA_HOME" ] || fail "opt-out still integrated"

# 9. An unwritable data dir never blocks the launch.
mkdir -p "$TMP/ro" && chmod a-w "$TMP/ro"
out="$(XDG_DATA_HOME="$TMP/ro/data" "$APPDIR/AppRun" out.txt)" || fail "launch blocked by unwritable data dir"
[ "$out" = "stub: out.txt" ] || fail "stub not run against unwritable data dir: $out"
chmod u+w "$TMP/ro"

# 10. No $APPIMAGE (extracted AppDir): launch only, nothing installed.
rm -rf "$XDG_DATA_HOME"
env -u APPIMAGE "$APPDIR/AppRun" >/dev/null || fail "extracted-AppDir launch exited non-zero"
[ ! -e "$XDG_DATA_HOME" ] || fail "integrated without $APPIMAGE"

# 11. A path that would need Exec escaping (here a `$`): launch only, nothing installed.
rm -rf "$XDG_DATA_HOME"
odd="$TMP/odd\$(id).AppImage"
: >"$odd"
out="$(APPIMAGE="$odd" "$APPDIR/AppRun" x)" || fail "launch with an odd path exited non-zero"
[ "$out" = "stub: x" ] || fail "odd path: $out"
[ ! -e "$XDG_DATA_HOME" ] || fail "integrated an AppImage path that needs escaping"

echo "AppRun desktop integration ok"
