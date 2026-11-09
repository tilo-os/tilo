#!/usr/bin/env bash
# The Lounge, run and used: the image starts on a fresh volume, reaches Ready,
# lists itself in its public directory, and two people's browsers join it
# through the web entry, chat, share a desk, and see each other present; then
# one shares a document's link and the other opens it full tab.
#
# Everything the deployment does around the image is here, on this machine:
# the entrypoint reads production-shaped TILO_ROOMS_* (this test's own ports
# and addresses), the web entry is served beside it, the host runs the packet
# relay its pages reach their live plane through, and the people are real
# headless Chromium browsers, one each. The programs are the image's own,
# started as the image's entrypoint starts them, in a jail of the image's
# files.
#
# usage: test-integration.sh IMAGE BUNDLE [WORK]
# IMAGE is an OCI archive (as tools/ci/publish/oci-build.sh writes it) or a
# reference buildah pulls (docker://…@sha256:…); BUNDLE is the web desktop
# bundle (tools/ci/publish/web-desktop-bundle.sh bundle). Both are what
# publish-site builds, so the gate runs the artifacts it is about to publish.
# WORK is where it keeps what it found — each page's snapshot and console, the
# site's and the Lounge's logs — which a failing run hands over as artifacts;
# by default a directory of its own under the scratch directory.
# Needs buildah, node, chromium (CHROME or Playwright's cache), curl and a
# Rust toolchain for the image's own tilo-crypto, which mints the site's
# certificate (TILO_CRYPTO_BIN names a built one instead). It runs as root in
# the buildah image in a micro-VM step, without a daemon, and as any user with
# buildah locally.
set -euo pipefail
usage='usage: test-integration.sh IMAGE BUNDLE [WORK]'
. "$(dirname -- "${BASH_SOURCE[0]}")/../../../tools/ci/publish/lib.sh"
use_buildah lounge overlay
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
work=${3:-$scratch/lounge-integration}
rm -rf "$work"
mkdir -p "$work"
log=$work/lounge.log
fail() { echo "FAIL: $*" >&2; [[ ! -s $log ]] || { echo '--- the Lounge said:' >&2; tail -40 "$log" >&2; }; exit 1; }

# An archive path, or a reference as it is.
reference() { if [[ -f $1 ]]; then printf 'oci-archive:%s' "$1"; else printf '%s' "$1"; fi; }
image=$(b pull --quiet "$(reference "${1:?$usage}")")
revision=$(b inspect --type image --format '{{index .OCIv1.Config.Labels "org.opencontainers.image.revision"}}' "$image")

# This test's own addresses, beside whatever already listens on this machine.
# The HTTPS listener is the image's own fixed 9781 (`--web`); the plain
# origin-forwarding listener, the packet relay pages reach the live plane
# through, and the web entry's site take ports of this test's. The site's port
# is fixed so the relay knows the site's origin before the site's workspace
# exists.
http_port=18790
plain_port=19780
live_port=19781
site_port=19782
web_port=9781
relay_origin=https://127.0.0.1:$web_port
site_origin=https://127.0.0.1:$site_port

# The web entry's bundle, laid out as the web entry image lays it out: the
# pages at the top, the modules under their content version.
[[ -f ${2:?$usage}/build.json ]] || fail "no desktop bundle at $2"
python3 -I "$repo_root/crates/browser/tilo-browser-runtime/container/layout.py" "$2" "$work/site"

# A certificate for this machine's loopback, which the site is reached at: the
# page harness ignores certificate errors, as it does for the image's own. The
# image's own tool mints it, so the gate needs no OpenSSL; a build of it may be
# handed in as TILO_CRYPTO_BIN.
crypto=${TILO_CRYPTO_BIN:-}
if [[ -z $crypto ]]; then
  cargo build --quiet --locked --manifest-path "$repo_root/Cargo.toml" -p tilo-crypto \
    --bin tilo-crypto || fail 'could not build the certificate mint'
  crypto=${CARGO_TARGET_DIR:-$repo_root/target}/debug/tilo-crypto
fi
"$crypto" generate-tls-fixture 127.0.0.1 "$work/site.ca.crt" "$work/site.key" "$work/site.crt" \
  || fail 'no certificate for the site'

# The image's own files, mounted once.
containers=() image_pid= site_pid=
container=$(b from --pull=never --quiet "$image")
containers+=("$container")
root=$(b mount "$container")
cleanup() {
  stop_site
  stop_image
  local c
  for c in "${containers[@]}"; do
    b umount "$c" >/dev/null 2>&1 || true
    b rm "$c" >/dev/null 2>&1 || true
  done
  rm -rf "$work/jail"
}
trap cleanup EXIT
# descendants PID: the processes PID started, and theirs, from each process's
# parent in /proc/*/stat (the micro-VM's kernel has no /proc/PID/task/*/children).
descendants() {
  local stat fields pid
  for stat in /proc/[0-9]*/stat; do
    fields=$(cat "$stat" 2>/dev/null) || continue
    fields=${fields##*) }
    fields=${fields#* }
    [[ ${fields%% *} == "$1" ]] || continue
    pid=${stat#/proc/}
    pid=${pid%/stat}
    echo "$pid"
    descendants "$pid"
  done
}
# end PID: PID and everything it started, ending them and proving they ended.
end() {
  local pids
  pids=$(descendants "$1")
  kill -KILL $pids "$1" 2>/dev/null || true
  for _ in $(seq 50); do
    kill -0 "$1" 2>/dev/null || break
    sleep 0.1
  done
  kill -0 "$1" 2>/dev/null && fail "a process of the test did not stop"
  wait "$1" 2>/dev/null || true
}

# state DIR: the volume a start reads, as the deployment mounts it: the
# workspace's directory (room-id, relay state) with the operator's secrets in
# it, owned by the image's user 10001.
state_root=$work/volume
prepare_volume() {
  rm -rf "$state_root"
  mkdir -p "$state_root"
  cp -a "$1"/. "$state_root"/
  mkdir -p "$state_root/secrets"
  # The workspace's owner root, 64 hex characters, as the deployment mounts it:
  # the state carries the one it was sealed with, and a fresh volume takes a
  # throwaway one.
  [[ -f $state_root/secrets/room-root ]] \
    || printf 'a%.0s' $(seq 64) > "$state_root/secrets/room-root"
  # The image's user owns the volume and writes all of it; nothing else is
  # granted, so a secret the state carries stays as owner-only as it was
  # written, which is what the store requires of its key.
  chmod -R u+rwX "$state_root"
  if [[ $EUID == 0 ]]; then chown -R 10001:0 "$state_root"; fi
}

# The image's entrypoint, with this deployment's environment, as a restricted
# pod runs it: the image read-only, a tmpfs at /tmp, uid 10001 (group 0), no
# capabilities and no_new_privs, the state volume writable at /run/tilo-rooms.
start_image() {
  unshare --mount --pid --fork -- bash -c '
    set -e
    root=$1 jail=$2 volume=$3
    shift 3
    mkdir -p "$jail"
    mount --bind "$root" "$jail"
    # The state volume mountpoint, created before the image files are read-only.
    mkdir -p "$jail/run/tilo-rooms"
    mount -o remount,bind,ro "$jail"
    mount --bind "$volume" "$jail/run/tilo-rooms"
    mount -t tmpfs -o mode=1777,nosuid,nodev tmpfs "$jail/tmp"
    mount -t proc -o nosuid,nodev,noexec proc "$jail/proc"
    mount --rbind /dev "$jail/dev"
    exec setpriv --no-new-privs --bounding-set=-all,+sys_chroot,+setuid,+setgid -- \
      chroot --userspec=10001:0 --groups=0 "$jail" /usr/bin/env -i HOME=/ "$@"
  ' lounge "$root" "$work/jail" "$state_root" \
    TILO_SOURCE_REVISION="$revision" \
    TILO_SECRETS_DIR=/run/tilo-rooms/secrets \
    TILO_ROOMS_STATE=/run/tilo-rooms \
    TILO_ROOMS_PERSISTENT=lounge \
    TILO_ROOMS_NAME=Lounge \
    TILO_ROOMS_DESCRIPTION='The Tilo community: chat, notes and a shared desktop' \
    TILO_ROOMS_DEFAULT_ROLE=editor \
    TILO_ROOMS_ADMISSION=open \
    TILO_ROOMS_RELAY_ORIGIN="$relay_origin" \
    TILO_ROOMS_LIVE="127.0.0.1:$live_port" \
    TILO_ROOMS_RELAY_PLAIN="127.0.0.1:$plain_port" \
    TILO_ROOMS_HTTP="127.0.0.1:$http_port" \
    TILO_ROOMS_WEB_ENTRY="$site_origin/" \
    TILO_ROOMS_SITE_ORIGINS="$site_origin" \
    TILO_ROOMS_ROOT_FILE=/run/tilo-rooms/secrets/room-root \
    TILO_ROOMS_ROLES_FILE=/run/tilo-rooms/roles.json \
    TILO_ROOMS_FETCH_PROXY=0 \
    TILO_ROOMS_CHAT_CHANNELS=general \
    /usr/local/bin/tilo-lounge >> "$log" 2>&1 &
  image_pid=$!
}
stop_image() {
  [[ -n $image_pid ]] || return 0
  end "$image_pid"
  image_pid=
}
# Started on a volume: the state this gate has kept so far.
start_on() {
  stop_image
  prepare_volume "$1"
  : > "$log"
  start_image
}
still_running() { kill -0 "$image_pid" 2>/dev/null; }

# Ready: the deployment's readiness probe answers that the relay is up, and
# the image's own revision is what it reports. At most 4 minutes, which a
# first start that seeds its apps fits well inside.
mode=''
wait_ready() {
  for _ in $(seq 1200); do
    mode=$(curl -fsS --max-time 2 "http://127.0.0.1:$http_port/mode" 2>/dev/null) && return 0
    still_running || return 1
    sleep 0.2
  done
  fail 'the Lounge did not reach Ready'
}
# Every record the start reads, reads whole: a sealed write the host leaves
# out of its history, a record it cannot open, or one seeded app that did not
# settle is the state that serves an empty or unloadable app, and no start may
# hand it to people as if it worked.
verify_records_read() {
  local broken
  broken=$(grep -E 'writes of its history that do not merge|invalid automerge input|duplicate seq|does not open: |open the chat.s channels|seed(ing)? .* failed' "$log" || true)
  [[ -z $broken ]] || fail "a record did not read whole: $(head -5 <<< "$broken")"
}
# The directory's answer, read: `POST /peer/v1/directory` on the relay.
directory() { curl -ksS --max-time 20 -X POST "$relay_origin/peer/v1/directory"; }
# page_field FIELD: one value of the one listing, read from the page on stdin.
page_field() {
  python3 -c '
import json, sys
page = json.load(sys.stdin)
listings = page["workspaces"]
if len(listings) != 1:
    sys.exit(f"the directory lists {len(listings)} workspaces, not the Lounge alone: {page}")
print(json.dumps(listings[0][sys.argv[1]]))
' "$1"
}
# The Lounge lists itself: by the name the deployment set, with what it is,
# the role everyone who joins gets, and the door of the workspace `$1` names,
# beside every other listing under the relay's directory.
page=''
verify_directory() {
  page=$(directory) || fail 'the directory did not answer'
  grep -q '"workspaces"' <<< "$page" || fail "the directory answered no page: $page"
  [[ $(page_field name <<< "$page") == '"Lounge"' ]] \
    || fail "the directory names the workspace $(page_field name <<< "$page"), not the deployment's Lounge"
  [[ $(page_field description <<< "$page") == '"The Tilo community: chat, notes and a shared desktop"' ]] \
    || fail "the directory's description is $(page_field description <<< "$page")"
  [[ $(page_field defaultRole <<< "$page") == '"Editor"' ]] \
    || fail "the door grants $(page_field defaultRole <<< "$page"), not the editor role the deployment sets"
  [[ $(page_field door <<< "$page" | tr -d '"') == "directory/$1/" ]] \
    || fail "the door is $(page_field door <<< "$page"), not the workspace's path under the directory"
}

# The web entry, served over TLS with the entry configuration this deployment
# writes: its home is the Lounge, joined through the door the relay lists the
# workspace `$1` names at, and its directory is the relay's.
site_url=''
start_site() {
  stop_site
  : > "$work/site.log"
  node "$here/integration/site.mjs" "$work/site" "$work/site.crt" "$work/site.key" \
    "$relay_origin" Lounge "$relay_origin/peer/v1/directory/$1/" "$site_port" \
    >> "$work/site.log" 2>&1 &
  site_pid=$!
  for _ in $(seq 300); do
    site_url=$(sed -n 's/^{"url": *"\([^"]*\)"}$/\1/p' "$work/site.log" | head -1)
    [[ -n $site_url ]] && break
    kill -0 "$site_pid" 2>/dev/null || break
    sleep 0.1
  done
  [[ $site_url == "$site_origin" ]] \
    || fail "the web entry did not start at $site_origin: $(cat "$work/site.log")"
  echo "ok: the web entry is served at $site_url"
}
stop_site() {
  [[ -n $site_pid ]] || return 0
  end "$site_pid"
  site_pid=
}

# A fresh volume, as the deployment first mounts it: empty, with the operator's
# secrets beside it. The host mints the workspace, keeps its owner root sealed
# in the volume's secret store, and keeps the workspace's id for later starts.
fresh=$work/fresh
mkdir -p "$fresh"
start_on "$fresh"
wait_ready || fail "the image did not start on a fresh volume: $(tail -20 "$log")"
verify_records_read
[[ $mode == '{"relay":"up","revision":"'* ]] || fail "/mode answered $mode"
[[ $mode == *"\"revision\":\"$revision\""* ]] || fail "/mode reports $mode, not the image's revision $revision"
minted=$(cat "$state_root/lounge/room-id" 2>/dev/null) || fail 'the fresh start kept no workspace id'
echo "ok: a fresh volume reaches Ready, /mode at $revision"
verify_directory "$minted"
[[ $(page_field room <<< "$page") == "\"$minted\"" ]] \
  || fail "the directory lists the workspace $(page_field room <<< "$page"), not the $minted this start minted"
echo 'ok: the directory lists the Lounge, joined at its door as an editor'

# The same volume again: what it kept is the workspace it serves, so a restart
# is the same workspace and not a second one beside it.
stop_image
: > "$log"
start_image
wait_ready || fail "the image did not start again on its own volume: $(tail -20 "$log")"
verify_records_read
[[ $(cat "$state_root/lounge/room-id") == "$minted" ]] \
  || fail 'a restart on the same volume served a different workspace'
verify_directory "$minted"
[[ $(page_field room <<< "$page") == "\"$minted\"" ]] \
  || fail 'a restart listed a different workspace'
echo "ok: a restart on the volume serves the same workspace ($minted)"

# The web entry the deployment serves beside the relay: its home is the
# Lounge, joined through the door of the workspace this volume holds.
start_site "$minted"

# Two people: real browsers on the web entry, each its own person, both
# joining the Lounge through its door, chatting, sharing a desk and seeing
# each other present through the live plane their pages join over the host's
# own packet relay. One of them is made someone the workspace lets invite
# while their page is open, invites a third person from the page's own sheet,
# and that person arrives from the link the sheet handed over, with no one
# letting them in.
node "$here/integration/people.mjs" "$site_url" "$relay_origin" "$relay_origin" "$work/people" \
  "$state_root/roles.json" \
  || fail 'two people did not meet in the Lounge'
echo 'ok: two people joined, chatted, shared a desk and saw each other present'
echo 'ok: a person invited from the page arrived from the link as the role it named'

# A shared link: one person opens a document full tab and copies its link
# from the bar's Share; another opens it on a phone-sized page and gets that
# document alone, live, with the first shown present. A run's link opens
# Console on that run.
node "$here/integration/share-link.mjs" "$site_url" "$relay_origin" "$minted" "$work/share-link" \
  || fail 'a shared link did not open the same document live for the second person'
echo 'ok: a shared link opened the one document full tab, live, for the second person'

# The pages that wrote the chat are gone: the image starts again on the same
# volume, and a person who was not here reads what they wrote and writes in it.
# The history is the workspace's — a record's changes name the surface they were
# written with, and a start that did not carry that version is answered by the
# pages that wrote it or by the surfaces the image publishes from the apps'
# history — never by a store beside the workspace.
earlier=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["chat"])' \
  "$work/people/report.json") || fail 'the two people left no chat message behind'
stop_image
: > "$log"
start_image
wait_ready || fail "the image did not start again on the volume people wrote in: $(tail -20 "$log")"
verify_records_read
node "$here/integration/people.mjs" "$site_url" "$relay_origin" "$relay_origin" "$work/returns" \
  "$state_root/roles.json" "$earlier" \
  || fail 'a person joining after the restart did not find the chat the workspace kept'
echo 'ok: a person joining after a restart reads the chat earlier pages wrote and writes in it'

echo "ok: the Lounge image runs, opens a fresh volume, and hosts people ($revision)"
