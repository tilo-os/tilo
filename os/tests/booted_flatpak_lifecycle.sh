#!/usr/bin/env bash
# A real Flathub app through the whole Flatpak path of a booted, signed-in
# Tilo: found by search, installed from it, opened, and removed from the App
# Store keeping its data and dropping the runtime nothing uses any more; and,
# on a device whose processor Flathub does not build for, an x86_64-only app
# (Steam) found by search as not available for this device instead of
# vanishing.
#
#   RUNNER='tilo-vm slot exec --slot S --timeout 900 -- sh -c' \
#   CONTROL='tilo-vm control --slot S --' \
#   RESTART='tilo-vm slot restart S' \
#     ./os/tests/booted_flatpak_lifecycle.sh
#
# RUNNER runs a shell command as the signed-in person (a throwaway slot made
# with `tilo-vm slot create S --identity`, or a dev VM over SSH); CONTROL runs
# tilo-control against that session (default: tilo-control through RUNNER);
# RESTART, when set, restarts the session so its search reads the Flathub
# metadata this check fetched. The app installs into the person's own Flatpak
# installation, with the Freedesktop runtime it needs (about 250 MB), so run
# it on a throwaway account, never on someone's own.
#
# The app is io.github.mike632t.x11-calc: about 1 MB on the Freedesktop
# runtime, built for x86_64 and aarch64.
#
# Exit codes: 0 every step held; 1 one failed.
set -uo pipefail

RUNNER="${RUNNER:-}"
CONTROL="${CONTROL:-}"
RESTART="${RESTART:-}"
APP="${APP:-io.github.mike632t.x11-calc}"
APP_NAME="${APP_NAME:-x11-calc}"
RUNTIME="${RUNTIME:-org.freedesktop.Platform}"
X86_ONLY="${X86_ONLY:-com.valvesoftware.Steam}"
X86_ONLY_NAME="${X86_ONLY_NAME:-Steam}"
STORE_NAME="App Store"
SCRATCH="/tmp/tilo-flatpak-lifecycle-check"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ATSPI_DIR="$SCRATCH/atspi"
ATSPI="env TILO_ATSPI_CLIENT=$ATSPI_DIR/atspi_external_client.py $ATSPI_DIR/installed-atspi-apps.sh"

on() {
  if [ -n "$RUNNER" ]; then $RUNNER "$*"; else bash -c "$*"; fi
}
control() {
  if [ -n "$CONTROL" ]; then $CONTROL "$@"; else on "tilo-control $*"; fi
}
ok()  { printf 'ok    %s\n' "$*"; }
die() { printf 'FAIL  %s\n' "$*" >&2; exit 1; }

# The rows search lists for QUERY, as `provider<TAB>id<TAB>subtitle`.
search_rows() {
  control actions list "$1" --kind application --json --wait=30000 --limit 50 \
    | python3 -c '
import json, sys
text = sys.stdin.read()
listing = json.loads(text[text.index("{"):text.rindex("}") + 1])
for row in listing["actions"]:
    print("\t".join([row["provider"], row["id"], row.get("subtitle") or ""]))
'
}

user_apps() { on "flatpak list --user --app --columns=application" | tr -d '\r'; }
user_runtimes() { on "flatpak list --user --runtime --columns=application" | tr -d '\r'; }

on 'echo reachable' | grep -q reachable || die "cannot run commands on the subject (RUNNER='$RUNNER')"
on 'command -v flatpak' >/dev/null || die "the subject has no flatpak"
on "flatpak remotes --system --columns=name,options" | grep -q '^flathub' \
  || die "the subject has no Flathub remote"
user_apps | grep -qx "$APP" && die "$APP is already installed for this account: run on a throwaway one"
on "rm -rf $SCRATCH && mkdir -p $SCRATCH" || die "cannot create $SCRATCH"
on "mkdir -m 0700 $ATSPI_DIR && cat > $ATSPI_DIR/atspi_external_client.py" \
  < "$REPO/tools/atspi_external_client.py" || die "cannot copy the AT-SPI client into the subject"
on "cat > $ATSPI_DIR/installed-atspi-apps.sh && chmod 0700 $ATSPI_DIR/*" \
  < "$REPO/os/tests/installed_atspi_apps.sh" || die "cannot copy the AT-SPI wrapper into the subject"

# --- 1. an app with no build for this processor is found, not installed ------
arch="$(on 'uname -m' | tr -d '\r')"
if [ "$arch" != x86_64 ]; then
  # What the hourly tilo-app-update run does: keep Flathub's x86_64 app list.
  on "flatpak update --appstream --noninteractive >/dev/null 2>&1; flatpak remote-ls --system --app --arch=x86_64 --columns=application -- flathub >/dev/null" \
    || die "could not fetch Flathub's x86_64 app list"
  if [ -n "$RESTART" ]; then $RESTART >/dev/null || die "could not restart the session"; fi
  deadline=$((SECONDS + 120))
  until row="$(search_rows "$X86_ONLY_NAME" | grep -P "^store-catalogue\tflathub:$X86_ONLY\t")"; do
    [ "$SECONDS" -lt "$deadline" ] || die "search for '$X86_ONLY_NAME' does not find $X86_ONLY on $arch:
$(search_rows "$X86_ONLY_NAME")"
    sleep 3
  done
  [ "$(cut -f3 <<<"$row")" = "Not available for this device" ] \
    || die "$X86_ONLY's row does not say it is not available for this device: '$row'"
  ok "search finds $X86_ONLY_NAME on $arch and says it is not available for this device"
else
  ok "x86_64 subject: every Flathub app has a build here, nothing to mark unavailable"
fi

# --- 2. installed by its row's Install action, then opened -------------------
deadline=$((SECONDS + 120))
until search_rows "$APP_NAME" | grep -qP "^store-catalogue\tflathub:$APP\t"; do
  [ "$SECONDS" -lt "$deadline" ] || die "search for '$APP_NAME' does not offer $APP"
  sleep 3
done
# Picking the app only opens its Store page; installing is the row's own
# "Install and open" action.
control actions invoke store-catalogue "flathub-install:$APP" --wait=30000 >/dev/null \
  || die "choosing Install and open for $APP in search failed"
started=$SECONDS
deadline=$((SECONDS + 900))
until user_apps | grep -qx "$APP"; do
  [ "$SECONDS" -lt "$deadline" ] || die "$APP was not installed within 900 s"
  sleep 5
done
ok "$APP installed from search into the person's installation in $((SECONDS - started)) s"
user_runtimes | grep -qx "$RUNTIME" || die "$APP was installed without its runtime $RUNTIME in this account"
deadline=$((SECONDS + 120))
until control windows --json | grep -qi "x11-calc\|$APP"; do
  [ "$SECONDS" -lt "$deadline" ] || die "$APP did not open a window after installing: $(control windows --json)"
  sleep 3
done
ok "$APP opened a window once installed"
on "pkill -u \$(id -u) -f '[b]wrap.*$APP'; true" >/dev/null 2>&1
[ "$(search_rows "$APP_NAME" | grep -cP "^store-catalogue\tflathub:$APP\t")" = 0 ] \
  || die "search still offers $APP to install once it is installed"
ok "search no longer offers $APP to install"

# --- 3. removed from the App Store, keeping its data --------------------------
on "mkdir -p \$HOME/.var/app/$APP/data && echo kept > \$HOME/.var/app/$APP/data/tilo-check"
opened="$(on "timeout 60 $ATSPI open '$STORE_NAME'" | tr -d '\r' | tail -1)"
store_bus="${opened% *}"
[[ ${opened##* } =~ ^[0-9]+$ ]] || die "the App Store did not open: '$opened'"
on "$ATSPI type '$store_bus' '$APP_NAME'" || die "could not search the App Store"
on "$ATSPI click '$store_bus' '$APP_NAME' --timeout 60" || die "the App Store did not list $APP_NAME"
on "$ATSPI click '$store_bus' 'Remove' --timeout 60" \
  || die "$APP_NAME's page offers no Remove: $(on "$ATSPI names '$store_bus'" 2>&1 | head -60)"
deadline=$((SECONDS + 300))
while user_apps | grep -qx "$APP"; do
  [ "$SECONDS" -lt "$deadline" ] || die "Remove did not uninstall $APP"
  sleep 3
done
ok "the App Store removed $APP"
[ "$(on "cat \$HOME/.var/app/$APP/data/tilo-check" | tr -d '\r')" = kept ] \
  || die "removing $APP deleted its data"
ok "$APP's data stayed"
if [ -z "$(user_apps)" ]; then
  user_runtimes | grep -qx "$RUNTIME" \
    && die "the runtime $RUNTIME stayed although no app of this account uses it"
  ok "the runtime no app uses any more was removed"
else
  ok "other apps of this account remain, so their runtimes stay"
fi
on "rm -rf \$HOME/.var/app/$APP $SCRATCH"
ok "a Flathub app was found, installed, opened and removed through Tilo's own paths"
