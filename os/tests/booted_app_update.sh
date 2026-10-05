#!/usr/bin/env bash
# The system image's apps update, launch and roll back through the app
# registry, like any other app, on a booted and logged-in Tilo: from `app`,
# from `tilo app update`, and from the App Store.
#
#   RUNNER='./os/dev/vm.sh ssh' ./os/tests/booted_app_update.sh   # a booted dev VM
#
# On a tilo-vm slot, RUNNER runs a command in the slot (`tilo-vm slot exec
# --slot S -- sh -c`), and ROOT_RUNNER runs one as root in every mount
# namespace of its session (the compositor's and its user manager's, through
# `tilo-vm vm ssh` and nsenter), since a slot's session sees its own layered
# /usr. The session must be unlocked and accessibility on
# (org.a11y.Status IsEnabled), as on a signed-in seat.
#
# A dev image is the production image plus dev tooling; this needs one piece
# of that tooling, tilo-test-registry, which serves newer versions of the
# installed Settings repacked from its own payload, signed by keys made for
# the run. Everything the check drives is the production path: `app repo
# add` pins the registry's owner key, `app update` / `app rollback` resolve
# offers and hand the signed packages to tilo-install, the App Store updates
# through the app host, and the desktop's palette launches what the store
# now says runs.
#
#   0. Terminal and Files are tapps: the palette opens each from its
#      package's payload.
#   1. `app list --installed` shows Settings as installed, from the image.
#   2. `app update so.tilo.settings` installs the registry's newest version
#      this OS can run: the registry also offers a newer build that requires
#      a system interface version the image does not serve, and it is passed
#      over. The palette opens the update: the launched app's payload is the
#      updated package, not the image's.
#   3. `app rollback` goes back to the registry's previous version, and a
#      second rollback to the image's version.
#   3b. The build for the unserved interface: `tilo-install` refuses it. With
#      the image's declaration replaced (a bind mount) by one serving that
#      version, as a newer OS would, `app update` installs it; with the
#      image's own declaration back, as after an OS rollback, the image's
#      copy runs and launches again, and `app remove` keeps it.
#   4. `tilo app update` updates every app with an update, saying what it
#      does as it goes, within a bound.
#   5. The App Store reads the repositories `app repo` edits (the test
#      registry added there), keeps no list of its own, its Update all
#      updates Settings, and Settings' details show the catalogue's
#      description and version.
#
# The registry runs in the guest on loopback. The command-line steps use a
# scratch repository configuration; the App Store step edits the person's
# own, on a throwaway VM. Exit codes: 0 every step held; 1 one failed.
set -uo pipefail

RUNNER="${RUNNER:-}"
APP="${APP:-so.tilo.settings}"
APP_NAME="${APP_NAME:-Personal Settings}"
STORE_NAME="App Store"
NEWER="9999.0.0"
PREVIOUS="9998.0.0"
UNSERVED="10000.0.0"
INTERFACES="/usr/share/tilo/system-interfaces.toml"
SCRATCH="/tmp/tilo-app-update-check"
ATSPI="/usr/libexec/tilo/installed-atspi-apps.sh"

on() {
  if [ -n "$RUNNER" ]; then $RUNNER "$@"; else bash -c "$*"; fi
}
ok()  { printf 'ok    %s\n' "$*"; }
die() {
  printf 'FAIL  %s\n' "$*" >&2
  on "tail -20 $SCRATCH/registry.log 2>/dev/null" >&2 || true
  cleanup
  exit 1
}
cleanup() {
  on "[ ! -s $SCRATCH/registry.pid ] || kill \$(cat $SCRATCH/registry.pid) 2>/dev/null; true" >/dev/null 2>&1 || true
  restore_interfaces >/dev/null 2>&1 || true
}

# Run as root in the guest, where the subject's commands run: through
# ROOT_RUNNER when set (a dev VM slot's own mount namespace), else sudo, the
# password coming from TILO_PASS when it asks.
as_root() {
  if [ -n "${ROOT_RUNNER:-}" ]; then
    $ROOT_RUNNER "$*"
  else
    on "printf '%s\\n' '${TILO_PASS:-}' | sudo -S -p '' sh -c '$*'"
  fi
}

# Stand in for an OS that serves `versions` of the system interface.
serve_system() {
  on "mkdir -p $SCRATCH && sed 's/^system *=.*/system = [$1]/' $INTERFACES > $SCRATCH/interfaces.toml" \
    || die "cannot write a replacement interface declaration"
  as_root "mountpoint -q $INTERFACES || true; umount $INTERFACES 2>/dev/null; mount --bind $SCRATCH/interfaces.toml $INTERFACES" \
    || die "cannot bind a replacement interface declaration over $INTERFACES"
}

restore_interfaces() {
  as_root "while umount $INTERFACES 2>/dev/null; do :; done; true"
}

# `app` with the scratch repository configuration.
app() { on "TILO_STORE_CONFIG_DIR=$SCRATCH/config TILO_STORE_CACHE_DIR=$SCRATCH/cache app $*"; }

# "<version> <from>" of the installed app.
installed() {
  app list --installed | tr -d '\r' | awk -v id="$APP" '$1 == id { gsub(/[()]/, "", $3); print $2, $3 }'
}

expect_installed() {
  local want="$1" got
  got="$(installed)"
  [ "$got" = "$want" ] || die "expected $APP to be installed as '$want', app list --installed says '$got'"
  ok "$APP is installed as $want"
}

# The package id of the copy that runs, from the store.
running_package() {
  on "tilo-install inspect $APP" | tr -d '\r' | sed -n 's/^package-id *: tapp://p'
}

rollback_to_image() {
  local out
  out="$(app rollback "$APP" 2>&1)" || die "app rollback $APP failed: $out"
  expect_installed "$PREVIOUS user"
  out="$(app rollback "$APP" 2>&1)" || die "the second app rollback $APP failed: $out"
  expect_installed "$image_version image"
  [ "$(running_package)" = "$image_package" ] || die "rolling back did not return to the image's package"
}

# Open an app through the palette; prints "<bus> <pid>".
# A launch of a copy the store no longer holds never presents a window, and
# the client would wait for one indefinitely: bound each try, so the caller
# retries once the desktop has reread the store.
open_app() {
  on "timeout 60 $ATSPI open '$1'" | tr -d '\r' | tail -1
}

# Open Settings through the palette and require that the launched process
# runs `package`'s payload. The desktop rereads the store on change (and at
# most 30 s later), so a launch of the previous copy is retried.
expect_launch() {
  expect_launch_of "$APP_NAME" "$1"
}

# expect_launch for the app the palette calls `name`.
expect_launch_of() {
  local name="$1" package="$2" deadline=$((SECONDS + 90)) opened pid ancestry
  while :; do
    opened="$(open_app "$name")"
    pid="${opened##* }"
    if [[ $pid =~ ^[0-9]+$ ]]; then
      # The launcher binds the payload it was given; its path (named by the
      # package id) is on the sandbox's command line or in its mounts.
      ancestry="$(on "p=$pid; while [ \"\$p\" -gt 1 ]; do tr '\\0' ' ' < /proc/\$p/cmdline; echo; p=\$(awk '{print \$4}' /proc/\$p/stat); done; cat /proc/$pid/mountinfo" 2>/dev/null)"
      on "kill $pid" >/dev/null 2>&1 || true
      if grep -q "$package" <<<"$ancestry"; then
        ok "$name launched from package $package"
        return
      fi
    fi
    [ "$SECONDS" -lt "$deadline" ] || die "$name did not launch from package $package (last open: '$opened')"
    sleep 5
  done
}

on 'echo reachable' | grep -q reachable || { echo "FAIL  cannot run commands on the subject (RUNNER='$RUNNER')" >&2; exit 1; }
on "command -v tilo-test-registry" >/dev/null || die "tilo-test-registry is not installed (a dev image ships it)"

# --- 0. Terminal and Files are tapps ------------------------------------------
for app in "Terminal so.tilo.terminal" "Files so.tilo.file-explorer"; do
  set -- $app
  package="$(on "tilo-install inspect $2" | tr -d '\r' | sed -n 's/^package-id *: tapp://p')"
  [ -n "$package" ] || die "tilo-install inspect $2 names no package id"
  expect_launch_of "$1" "$package"
done

# --- 1. installed from the image ---------------------------------------------
image="$(installed)"
case "$image" in
  *" image") ok "$APP is installed from the system image: $image" ;;
  *) die "app list --installed does not show $APP as an image app: '$image'" ;;
esac
image_version="${image% image}"
image_package="$(running_package)"
[ -n "$image_package" ] || die "tilo-install inspect $APP names no package id"

# --- a test registry offering two newer versions -------------------------------
on "rm -rf $SCRATCH && mkdir -p $SCRATCH/config $SCRATCH/cache" || die "cannot create $SCRATCH"
on "nohup tilo-test-registry --app $APP --version $NEWER --version $PREVIOUS --incompatible $UNSERVED \
      > $SCRATCH/registry.json 2> $SCRATCH/registry.log < /dev/null & echo \$! > $SCRATCH/registry.pid" \
  || die "cannot start tilo-test-registry"
trap cleanup EXIT
for _ in $(seq 1 120); do
  on "test -s $SCRATCH/registry.json" && break
  sleep 1
done
announce="$(on "cat $SCRATCH/registry.json" | tr -d '\r')"
url="$(jq -r .url <<<"$announce")"
owner="$(jq -r .owner_key <<<"$announce")"
[[ $url == http://127.0.0.1:* && $owner =~ ^[0-9a-f]{64}$ ]] || die "tilo-test-registry did not come up: '$announce'"
unserved_needs="$(jq -r --arg v "$UNSERVED" '.interfaces[$v] | join(",")' <<<"$announce")"
[[ $unserved_needs == *system@* ]] || die "the test registry's $UNSERVED requires no system interface: '$unserved_needs'"
ok "test registry at $url offers $NEWER, $PREVIOUS and $UNSERVED (needs $unserved_needs)"
on "grep -q '^system *= *\\[' $INTERFACES" || die "the image does not declare the system interface in $INTERFACES"
ok "the image serves $(on "grep '^system' $INTERFACES" | tr -d '\r')"
app repo add test "$url" --owner-key "$owner" >/dev/null || die "app repo add test failed"
app repo disable "'Tilo main'" >/dev/null || die "could not disable the main repository for this check"

# --- 2. update and launch -------------------------------------------------------
out="$(app update "$APP" 2>&1)" || die "app update $APP failed: $out"
expect_installed "$NEWER user"
updated_package="$(running_package)"
[ -n "$updated_package" ] && [ "$updated_package" != "$image_package" ] \
  || die "the update did not change the package that runs ($updated_package)"
expect_launch "$updated_package"

# --- 3. roll back: the registry's previous version, then the image's ------------
rollback_to_image
expect_launch "$image_package"

# --- 3b. a build for an interface the OS does not serve --------------------------
on "curl -fsS -o $SCRATCH/unserved.tapp $url/$APP-$UNSERVED.tapp" || die "cannot fetch $UNSERVED from the test registry"
publisher="$(jq -r .publisher_key <<<"$announce")"
out="$(on "TILO_STORE_CONFIG_DIR=$SCRATCH/config tilo-install update $APP --from $SCRATCH/unserved.tapp --require-publisher $publisher --allow-self-signed" 2>&1)" \
  && die "tilo-install installed $UNSERVED, which needs $unserved_needs: $out"
grep -q "needs system interfaces this Tilo does not provide" <<<"$out" \
  || die "tilo-install refused $UNSERVED without naming the interface: $out"
ok "tilo-install refuses $UNSERVED: $(grep -o 'system@[0-9]* ([^)]*)' <<<"$out" | head -1)"
expect_installed "$image_version image"

unserved_system="${unserved_needs##*system@}"
unserved_system="${unserved_system%%,*}"
serve_system "1, $unserved_system"
out="$(app update "$APP" 2>&1)" || die "app update $APP on an OS serving system@$unserved_system failed: $out"
expect_installed "$UNSERVED user"
unserved_package="$(running_package)"
ok "on an OS serving system@$unserved_system, app update installs $UNSERVED"

restore_interfaces || die "cannot restore the image's interface declaration"
expect_installed "$image_version image"
[ "$(running_package)" = "$image_package" ] || die "after the OS went back, $UNSERVED still runs"
expect_launch "$image_package"
ok "after the OS went back to system@1, the image's copy runs again over $UNSERVED ($unserved_package)"
out="$(app remove "$APP" 2>&1)" || die "app remove $APP failed: $out"
expect_installed "$image_version image"
expect_launch "$image_package"
ok "removing the update kept the image's copy"

# --- 4. tilo app update ----------------------------------------------------------
out="$(on "TILO_STORE_CONFIG_DIR=$SCRATCH/config TILO_STORE_CACHE_DIR=$SCRATCH/cache timeout 300 tilo app update" 2>&1)" \
  || die "tilo app update failed or exceeded 300 s: $out"
for line in "Checking " "update available" "Updating $APP_NAME $image_version -> $NEWER" "updated 1"; do
  grep -qF "$line" <<<"$out" || die "tilo app update did not report '$line':
$out"
done
expect_installed "$NEWER user"
ok "tilo app update updated $APP and said so as it went"
rollback_to_image

# --- 5. the App Store ------------------------------------------------------------
# The Store reads the person's repositories, the ones `app repo` edits,
# through the app host; its sandbox keeps no list of its own.
on "app repo add test '$url' --owner-key '$owner'" >/dev/null || die "app repo add test failed"
on "app repo disable 'Tilo main'" >/dev/null || die "could not disable the main repository"
opened="$(open_app "$STORE_NAME")"
store_bus="${opened% *}"
store_pid="${opened##* }"
[[ $store_pid =~ ^[0-9]+$ ]] || die "the App Store did not reopen: '$opened'"
on "$ATSPI click '$store_bus' 'Update all' --timeout 120" \
  || die "the App Store offered no Update all: $(on "$ATSPI names '$store_bus'" 2>&1 | head -60)"
deadline=$((SECONDS + 180))
until [ "$(installed)" = "$NEWER user" ]; do
  [ "$SECONDS" -lt "$deadline" ] || die "Update all did not update $APP: $(on "$ATSPI names '$store_bus'" 2>&1 | head -60)"
  sleep 3
done
ok "the App Store's Update all updated $APP to $NEWER"

on "$ATSPI type '$store_bus' '$APP_NAME'" || die "could not search the App Store"
on "$ATSPI click '$store_bus' 'Details' --timeout 60" || die "the App Store showed no Details for $APP_NAME"
details=""
deadline=$((SECONDS + 60))
until grep -q "offered by a test registry" <<<"$details" && grep -q "$NEWER" <<<"$details"; do
  [ "$SECONDS" -lt "$deadline" ] || die "the details of $APP_NAME do not show the catalogue's description and version:
$details"
  sleep 2
  details="$(on "$ATSPI names '$store_bus'" 2>&1)"
done
grep -q "Catalogue details are unavailable" <<<"$details" \
  && die "the App Store still says its catalogue details are unavailable"
ok "the App Store shows $APP_NAME's description and version $NEWER"
on "ls \$HOME/.local/share/tilo-rooms/*/so.tilo.store/.config/tilo-store/registries.toml" >/dev/null 2>&1 \
  && die "the App Store keeps a repository list of its own in its sandbox"
ok "the App Store used the repositories app repo edits, and kept no copy"
on "kill $store_pid" >/dev/null 2>&1 || true
expect_launch "$(running_package)"

ok "$APP updated, launched and rolled back through the registry, the CLI and the App Store"
