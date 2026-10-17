#!/usr/bin/env bash
# Smoke-test a web entry image before it is published: the entry pages, entry.json with and
# without a home workspace, and the caching, encoding and type headers CDNs and browsers rely
# on. The server runs as a restricted pod runs it: read-only but for its /tmp, uid 10001, no
# capabilities, no_new_privs.
# Given the image a deploy replaces (OUTGOING), it also proves what carry.sh promises: a tab
# still on the outgoing version can fetch every file of it, while the entry pages load only the
# incoming version.
#
# usage: test-image.sh IMAGE [OUTGOING]
# Each is an OCI archive (as tools/ci/publish/oci-build.sh writes it) or a reference buildah
# pulls (docker://…@sha256:…). It runs in the buildah image as root in a micro-VM step (the t
# checks image-web-entry and carry-web-entry), without a daemon: buildah mounts the image and
# the server runs chrooted into it in mount and PID namespaces of its own, sharing the step's
# network, so it is reached on the loopback, one configuration at a time.
set -euo pipefail
usage='usage: test-image.sh IMAGE [OUTGOING]'
. "$(dirname -- "${BASH_SOURCE[0]}")/../../../../tools/ci/publish/lib.sh"
use_buildah web-entry overlay
log=$scratch/web-entry.log
fail() { echo "FAIL: $*" >&2; [[ ! -s $log ]] || tail -20 "$log" >&2; exit 1; }
# An archive path, or a reference as it is.
reference() { if [[ -f $1 ]]; then printf 'oci-archive:%s' "$1"; else printf '%s' "$1"; fi; }
image=$(b pull --quiet "$(reference "${1:?$usage}")")
outgoing=
[[ -z ${2:-} ]] || outgoing=$(b pull --quiet "$(reference "$2")")
json() { python3 -c 'import json, sys; print(json.load(sys.stdin)[sys.argv[1]])' "$1"; }

containers=() server=
cleanup() {
  stop || true
  local c
  for c in "${containers[@]}"; do b rm "$c" >/dev/null 2>&1 || true; done
}
trap cleanup EXIT
# mount_image IMAGE: a container of IMAGE ($container) and its files ($root).
mount_image() {
  container=$(b from --pull=never --quiet "$1")
  containers+=("$container")
  root=$(b mount "$container")
}

[[ $(b inspect --type image --format '{{.OCIv1.Config.User}}' "$image") == 10001 ]] || fail "the image does not run as uid 10001"
mount_image "$image"
mapfile -t image_env < <(b inspect --type image --format '{{range .OCIv1.Config.Env}}{{println .}}{{end}}' "$image" | grep .)
# entry NAME=VALUE…: the image's entrypoint with this environment added to the image's, as a
# restricted pod runs it: the image read-only, a tmpfs at /tmp, uid 10001 (group 0, as the
# runtime gives a user the image doesn't name), no capabilities and no_new_privs. The step's
# root makes the namespaces and the jail, and gives up every privilege on the way in.
entry() {
  unshare --mount --pid --fork -- bash -c '
    set -e
    root=$1 jail=$2
    shift 2
    mkdir -p "$jail"
    mount --bind "$root" "$jail"
    mount -o remount,bind,ro "$jail"
    mount -t tmpfs -o mode=1777,nosuid,nodev tmpfs "$jail/tmp"
    mount -t proc -o nosuid,nodev,noexec proc "$jail/proc"
    mount --rbind /dev "$jail/dev"
    exec setpriv --no-new-privs --bounding-set=-all,+sys_chroot,+setuid,+setgid -- \
      chroot --userspec=10001:0 --groups=0 "$jail" /usr/bin/env -i HOME=/ "$@" /usr/local/bin/tilo-web-entry
  ' entry "$root" "$scratch/jail" "${image_env[@]}" "$@" > "$log" 2>&1
}
# descendants PID: the processes PID started, and theirs, from each process's parent in
# /proc/*/stat (the micro-VM's kernel has no /proc/PID/task/*/children).
descendants() {
  local stat fields pid
  for stat in /proc/[0-9]*/stat; do
    fields=$(cat "$stat" 2>/dev/null) || continue
    fields=${fields##*) }          # after the command name: state, parent, …
    fields=${fields#* }
    [[ ${fields%% *} == "$1" ]] || continue
    pid=${stat#/proc/}
    pid=${pid%/stat}
    echo "$pid"
    descendants "$pid"
  done
}
# stop: end the server start started, with the namespace it runs in.
stop() {
  [[ -n $server ]] || return 0
  local pids
  pids=$(descendants "$server")
  kill -KILL $pids "$server" 2>/dev/null || true
  for _ in $(seq 50); do
    kill -0 "$server" 2>/dev/null || break
    sleep 0.1
  done
  kill -0 "$server" 2>/dev/null && fail "the server did not stop"
  wait "$server" 2>/dev/null || true
  server=
  for _ in $(seq 50); do
    curl -fsS --max-time 2 -o /dev/null http://127.0.0.1:8080/healthz 2>/dev/null || return 0
    sleep 0.1
  done
  fail "the server did not stop"
}
# start NAME=VALUE…: serve with this environment, after stopping the last server.
start() {
  stop
  entry "$@" 2>/dev/null &
  server=$!
  for _ in $(seq 100); do
    curl -fsS --max-time 2 -o /dev/null http://127.0.0.1:8080/healthz 2>/dev/null && return
    kill -0 "$server" 2>/dev/null || fail "the server exited"
    sleep 0.2
  done
  fail "the server did not start"
}
# get PATH [curl args…]: the body; head_of …: the headers, lowercased names.
get() { curl -fsS --max-time 20 "${@:2}" "http://127.0.0.1:8080$1"; }
head_of() { curl -fsS --max-time 20 -o /dev/null -D - "${@:2}" "http://127.0.0.1:8080$1" | tr -d '\r' | lower_names; }
# Portable (BSD sed has no \L): lowercase each header's name, not the status line.
lower_names() { awk '{ i = index($0, ":"); if (i) $0 = tolower(substr($0, 1, i)) substr($0, i + 1); print }'; }

entry TILO_WEB_HOME_NAME=X && fail "a home name without a host must refuse to start"
entry TILO_WEB_ABOUT_COMMUNITY=javascript:x && fail "an About link that is not https must refuse to start"
for bad in http://site.example https://site.example/path "https://a b"; do
  entry "TILO_WEB_EMBEDDER=$bad" && fail "an embedder that is not an https:// origin must refuse to start: $bad"
done

# No home: a bare visit opens the person's own desktop. The server writes nothing but /tmp.
start
[[ $(get /entry.json) == '{}' ]] || fail "without TILO_WEB_HOME_HOST, entry.json must name no home"

# About links: those set are offered, an empty one is not.
start TILO_WEB_ABOUT_SOURCE=https://example.org/source TILO_WEB_ABOUT_NOTIFY=
[[ $(get /entry.json) == '{"about":{"source":"https://example.org/source"}}' ]] \
  || fail "entry.json about: $(get /entry.json)"

# Only the app itself may frame it, and the embedding site when one is set.
start TILO_WEB_EMBEDDER=https://site.example
csp=$(head_of / | grep '^content-security-policy:' || true)
[[ $csp == "content-security-policy: frame-ancestors 'self' https://site.example" ]] \
  || fail "/ must be framable only by itself and its embedder: ${csp:-no header}"
[[ $(get /entry.json) == '{"embedder":"https://site.example"}' ]] \
  || fail "entry.json embedder: $(get /entry.json)"

start TILO_WEB_HOME_HOST=rooms.example TILO_WEB_HOME_NAME=Commons TILO_WEB_HOME_APP=so.tilo.chat
expected='{"home":{"name":"Commons","host":{"origin":"https://rooms.example"},"admission":"https://rooms.example/","app":"so.tilo.chat"}}'
[[ $(get /entry.json) == "$expected" ]] || fail "entry.json: $(get /entry.json)"
grep -q '^cache-control: no-cache' <(head_of /entry.json) || fail "entry.json must revalidate"
csp=$(head_of / | grep '^content-security-policy:' || true)
[[ $csp == "content-security-policy: frame-ancestors 'self'" ]] \
  || fail "without TILO_WEB_EMBEDDER, / must be framable only by itself: ${csp:-no header}"

page=$(get /)
version=$(grep -o "v/[0-9a-f]\{20\}/entry-page.mjs" <<<"$page" | head -1 | cut -d/ -f2)
[[ -n $version ]] || fail "the entry page imports no versioned entry-page.mjs"
grep -q "'../v/$version/entry-page.mjs'" <<<"$(get /app/)" || fail "the app entry points elsewhere"
grep -q '^cache-control: no-cache' <(head_of /) || fail "/ must revalidate"
# Safari's home-screen icon sits beside the entry pages, as their links and
# Safari's own unasked lookup expect.
grep -q '^content-type: image/png' <(head_of /apple-touch-icon.png) || fail "/apple-touch-icon.png must be served as a PNG"
# Every answer pins the host to HTTPS, the entry pages and the versioned
# files alike, and never reaches past it to the workspace hosts beneath.
for path in / /entry.json "/v/$version/entry-page.mjs"; do
  sts=$(head_of "$path" | grep '^strict-transport-security:' || true)
  [[ $sts == 'strict-transport-security: max-age=31536000' ]] || fail "$path must pin HTTPS for this host only: ${sts:-no header}"
done
# One complete, immutable version: version.json names the version the entry
# pages import, and every file that version's build.json lists (bar the two
# entry pages and their icon, which live at the top) is served under
# v/<version>/ with exactly the bytes it records. A missing or substituted file fails here
# rather than in a visitor's tab when an app's module is first fetched.
[[ $(get /version.json | json version) == "$version" ]] || fail "version.json names another version than the entry page imports"
# A page opened before a deploy reads it to learn that its version's files
# are gone (web/bundle-update.mjs): a cached answer would hide the update.
grep -q '^cache-control: no-cache' <(head_of /version.json) || fail "version.json must revalidate"
# verify_version VERSION: every file VERSION's build.json lists is served
# under v/VERSION/ with exactly the bytes it records.
verify_version() {
  local listed served hash name
  listed=$(get "/v/$1/build.json" | python3 -c '
import json, sys
for name, digest in json.load(sys.stdin)["files"].items():
    if name not in ("index.html", "app/index.html", "apple-touch-icon.png"):
        print(digest, name)')
  [[ $(wc -l <<<"$listed") -gt 1 ]] || fail "v/$1/build.json lists no files"
  # -g: font names carry [ ], which curl would otherwise read as a glob.
  served=$(while read -r hash name; do
    printf '%s %s\n' "$(get "/v/$1/$name" -g -H 'Accept-Encoding: identity' | sha256sum | cut -d' ' -f1)" "$name"
  done <<<"$listed")
  [[ $served == "$listed" ]] || fail "v/$1/ differs from its build.json: $(diff <(echo "$listed") <(echo "$served") | head -5)"
}
verify_version "$version"
# The arrival screen's lines, which arrival.mjs fetches beside itself.
for locale in en-US de-DE; do
  grep -q '^arrival-label-tip = ' <<<"$(get "/v/$version/locales/$locale.ftl")" \
    || fail "the bundle carries no locales/$locale.ftl"
done

# Every answer to a plain GET is a whole 200, encoded or not: CDNs never
# cache a 206, so a partial answer to a request without Range sends every
# visitor to the origin.
for path in / "/v/$version/entry-page.mjs" "/v/$version/pkg/tilo_browser_runtime_bg.wasm"; do
  for encoding in zstd br 'gzip, deflate, br, zstd' identity; do
    status=$(head_of "$path" -H "Accept-Encoding: $encoding" | head -1)
    [[ $status == 'HTTP/1.1 200 OK' ]] || fail "$path with Accept-Encoding $encoding: $status"
  done
done

wasm="/v/$version/pkg/tilo_browser_runtime_bg.wasm"
headers=$(head_of "$wasm" -H 'Accept-Encoding: br, zstd')
grep -q '^content-type: application/wasm' <<<"$headers" || fail "wasm type: $headers"
grep -q '^content-encoding: \(zstd\|br\)' <<<"$headers" || fail "wasm is not served precompressed: $headers"
grep -qi '^vary:.*accept-encoding' <<<"$headers" || fail "no Vary: Accept-Encoding: $headers"
grep -q '^cache-control: public, max-age=31536000, immutable' <<<"$headers" || fail "versioned files must be immutable: $headers"
grep -q '^content-encoding:' <(head_of "$wasm") && fail "a client without br/zstd got an encoded body"
grep -q '^content-type: text/javascript' <(head_of "/v/$version/entry-page.mjs") || fail "modules need a JavaScript type"

# A versioned file the image does not have is a 404 no cache may keep: during
# a rolling update a page from a new pod asks an old one for the new
# version's files, and a 404 cached as immutable would stand at that edge for
# a year after the rollout finished.
missing=$(curl -sS --max-time 20 -o /dev/null -D - "http://127.0.0.1:8080/v/$version/pkg/absent.wasm" | tr -d '\r' | lower_names)
[[ $(head -1 <<<"$missing") == 'HTTP/1.1 404 Not Found' ]] || fail "a missing versioned file: $(head -1 <<<"$missing")"
grep -q '^cache-control: no-store' <<<"$missing" || fail "a missing versioned file must not be cached: $missing"

# Which versions the image serves: only its own, or, given the image a deploy
# replaces, its own and that image's own (carry.sh), never more.
serves=$(ls "$root/srv/tilo-web/v" | sort | tr '\n' ' ')
carried=
if [[ -n $outgoing ]]; then
  incoming_root=$root
  mount_image "$outgoing"
  outgoing_root=$root root=$incoming_root
  carried=$(json version < "$outgoing_root/srv/tilo-web/version.json")
  [[ $carried =~ ^[0-9a-f]{20}$ ]] || fail "the outgoing image's version.json names no version: $carried"
  [[ $carried == "$version" ]] && carried=
fi
if [[ -z $carried ]]; then
  [[ $serves == "$version " ]] || fail "the image must serve only v/$version/: $serves"
  echo "web entry image OK (version $version)"
  exit 0
fi
[[ $serves == "$(printf '%s\n' "$version" "$carried" | sort | tr '\n' ' ')" ]] \
  || fail "the image must serve v/$version/ and the outgoing v/$carried/, nothing else: $serves"
label=$(b inspect --type image --format '{{index .OCIv1.Config.Labels "so.tilo.web-entry.carried-version"}}' "$image")
[[ $label == "$carried" ]] || fail "the image is labelled as carrying ${label:-nothing}, not $carried"
# A new tab loads only the incoming version...
for entry in / /app/; do
  grep -q "v/$carried/" <<<"$(get "$entry")" && fail "$entry refers to the outgoing version $carried"
done
# ...and a tab still on the outgoing one opens an app it has not loaded yet:
# its module is a whole, immutable 200, as are all the version's files,
# byte for byte the outgoing image's.
module="/v/$carried/pkg/tilo_browser_runtime.calculator.wasm"
headers=$(head_of "$module" -H 'Accept-Encoding: br, zstd')
[[ $(head -1 <<<"$headers") == 'HTTP/1.1 200 OK' ]] || fail "$module: $(head -1 <<<"$headers")"
grep -q '^cache-control: public, max-age=31536000, immutable' <<<"$headers" || fail "$module must be immutable: $headers"
grep -q '^content-type: application/wasm' <<<"$headers" || fail "$module type: $headers"
verify_version "$carried"
cmp -s <(get "/v/$carried/build.json") "$outgoing_root/srv/tilo-web/v/$carried/build.json" \
  || fail "v/$carried/build.json is not the outgoing image's"
echo "web entry image OK (version $version, carrying $carried)"
