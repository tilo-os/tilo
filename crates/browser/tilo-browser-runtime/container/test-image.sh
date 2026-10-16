#!/usr/bin/env bash
# Smoke-test a web entry image: the entry pages, entry.json with and without
# a home workspace, and the caching, encoding and type headers CDNs and
# browsers rely on. HTTP is spoken from inside the container (the image
# carries curl), so this works when the Docker daemon is remote too. It runs
# as a restricted pod does: read-only, uid 10001, no capabilities, no_new_privs.
# Given the image a deploy replaces (OUTGOING), it also proves what carry.sh
# promises: a tab still on the outgoing version can fetch every file of it,
# while the entry pages load only the incoming version.
# usage: test-image.sh IMAGE [OUTGOING]   (docker or podman as $CONTAINER, default docker)
set -euo pipefail
image=${1:?usage: test-image.sh IMAGE [OUTGOING]}
outgoing=${2:-}
engine=${CONTAINER:-docker}
containers=()
confine=(--read-only --tmpfs /tmp:mode=1777 --user 10001 --cap-drop ALL --security-opt no-new-privileges)
trap 'for c in "${containers[@]}"; do "$engine" logs "$c" 2>&1 | tail -20; "$engine" rm -f "$c" >/dev/null 2>&1; done' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

start() {
  local c
  c=$("$engine" run -d "${confine[@]}" "$@" "$image")
  containers+=("$c")
  for _ in $(seq 50); do
    "$engine" exec "$c" curl -fsS http://127.0.0.1:8080/healthz >/dev/null 2>&1 && { echo "$c"; return; }
    sleep 0.2
  done
  fail "the server did not start"
}
# get CONTAINER PATH [curl args…]: the body; head …: the headers, lowercased names.
get() { "$engine" exec "$1" curl -fsS "${@:3}" "http://127.0.0.1:8080$2"; }
head_of() { "$engine" exec "$1" curl -fsS -o /dev/null -D - "${@:3}" "http://127.0.0.1:8080$2" | tr -d '\r' | lower_names; }
# Portable (BSD sed has no \L): lowercase each header's name, not the status line.
lower_names() { awk '{ i = index($0, ":"); if (i) $0 = tolower(substr($0, 1, i)) substr($0, i + 1); print }'; }

[[ $("$engine" image inspect --format '{{.Config.User}}' "$image") == 10001 ]] || fail "the image does not run as uid 10001"

# No home: a bare visit opens the person's own desktop.
plain=$(start)
[[ $(get "$plain" /entry.json) == '{}' ]] || fail "without TILO_WEB_HOME_HOST, entry.json must name no home"
"$engine" run --rm "${confine[@]}" -e TILO_WEB_HOME_NAME=X "$image" >/dev/null 2>&1 \
  && fail "a home name without a host must refuse to start"

home=$(start -e TILO_WEB_HOME_HOST=rooms.example -e TILO_WEB_HOME_NAME=Commons -e TILO_WEB_HOME_APP=so.tilo.chat)
expected='{"home":{"name":"Commons","host":{"origin":"https://rooms.example"},"admission":"https://rooms.example/","app":"so.tilo.chat"}}'
[[ $(get "$home" /entry.json) == "$expected" ]] || fail "entry.json: $(get "$home" /entry.json)"
grep -q '^cache-control: no-cache' <(head_of "$home" /entry.json) || fail "entry.json must revalidate"

# About links: those set are offered, an empty one is not.
about=$(start -e TILO_WEB_ABOUT_SOURCE=https://example.org/source -e TILO_WEB_ABOUT_NOTIFY=)
[[ $(get "$about" /entry.json) == '{"about":{"source":"https://example.org/source"}}' ]] \
  || fail "entry.json about: $(get "$about" /entry.json)"
"$engine" run --rm "${confine[@]}" -e TILO_WEB_ABOUT_COMMUNITY=javascript:x "$image" >/dev/null 2>&1 \
  && fail "an About link that is not https must refuse to start"

page=$(get "$home" /)
version=$(grep -o "v/[0-9a-f]\{20\}/entry-page.mjs" <<<"$page" | head -1 | cut -d/ -f2)
[[ -n $version ]] || fail "the entry page imports no versioned entry-page.mjs"
grep -q "'../v/$version/entry-page.mjs'" <<<"$(get "$home" /app/)" || fail "the app entry points elsewhere"
grep -q '^cache-control: no-cache' <(head_of "$home" /) || fail "/ must revalidate"
# Every answer pins the host to HTTPS, the entry pages and the versioned
# files alike, and never reaches past it to the workspace hosts beneath.
for path in / /entry.json "/v/$version/entry-page.mjs"; do
  sts=$(head_of "$home" "$path" | grep '^strict-transport-security:' || true)
  [[ $sts == 'strict-transport-security: max-age=31536000' ]] || fail "$path must pin HTTPS for this host only: ${sts:-no header}"
done
# Only the app itself may frame it, and the embedding site when one is set.
csp=$(head_of "$home" / | grep '^content-security-policy:' || true)
[[ $csp == "content-security-policy: frame-ancestors 'self'" ]] \
  || fail "without TILO_WEB_EMBEDDER, / must be framable only by itself: ${csp:-no header}"
embedded=$(start -e TILO_WEB_EMBEDDER=https://site.example)
csp=$(head_of "$embedded" / | grep '^content-security-policy:' || true)
[[ $csp == "content-security-policy: frame-ancestors 'self' https://site.example" ]] \
  || fail "/ must be framable only by itself and its embedder: ${csp:-no header}"
[[ $(get "$embedded" /entry.json) == '{"embedder":"https://site.example"}' ]] \
  || fail "entry.json embedder: $(get "$embedded" /entry.json)"
for bad in http://site.example https://site.example/path "https://a b"; do
  "$engine" run --rm "${confine[@]}" -e TILO_WEB_EMBEDDER="$bad" "$image" >/dev/null 2>&1 \
    && fail "an embedder that is not an https:// origin must refuse to start: $bad"
done
# One complete, immutable version: version.json names the version the entry
# pages import, and every file that version's build.json lists (bar the two
# entry pages, which live at the top) is served under v/<version>/ with
# exactly the bytes it records. A missing or substituted file fails here
# rather than in a visitor's tab when an app's module is first fetched.
[[ $(get "$home" /version.json | jq -r .version) == "$version" ]] || fail "version.json names another version than the entry page imports"
# A page opened before a deploy reads it to learn that its version's files
# are gone (web/bundle-update.mjs): a cached answer would hide the update.
grep -q '^cache-control: no-cache' <(head_of "$home" /version.json) || fail "version.json must revalidate"
# verify_version VERSION: every file VERSION's build.json lists is served
# under v/VERSION/ with exactly the bytes it records.
verify_version() {
  local listed served
  listed=$(get "$home" "/v/$1/build.json" \
    | jq -r '.files | to_entries[] | select(.key != "index.html" and .key != "app/index.html") | "\(.value) \(.key)"')
  [[ $(wc -l <<<"$listed") -gt 1 ]] || fail "v/$1/build.json lists no files"
  # -g: font names carry [ ], which curl would otherwise read as a glob.
  served=$("$engine" exec -i "$home" sh -c '
    while read -r hash name; do
      printf "%s %s\n" "$(curl -fsSg -H "Accept-Encoding: identity" "http://127.0.0.1:8080/v/$1/$name" | sha256sum | cut -d" " -f1)" "$name"
    done' sh "$1" <<<"$listed")
  [[ $served == "$listed" ]] || fail "v/$1/ differs from its build.json: $(diff <(echo "$listed") <(echo "$served") | head -5)"
}
verify_version "$version"
# The arrival screen's lines, which arrival.mjs fetches beside itself.
for locale in en-US de-DE; do
  grep -q '^arrival-label-tip = ' <<<"$(get "$home" "/v/$version/locales/$locale.ftl")" \
    || fail "the bundle carries no locales/$locale.ftl"
done

# Every answer to a plain GET is a whole 200, encoded or not: CDNs never
# cache a 206, so a partial answer to a request without Range sends every
# visitor to the origin.
for path in / "/v/$version/entry-page.mjs" "/v/$version/pkg/tilo_browser_runtime_bg.wasm"; do
  for encoding in zstd br 'gzip, deflate, br, zstd' identity; do
    status=$(head_of "$home" "$path" -H "Accept-Encoding: $encoding" | head -1)
    [[ $status == 'HTTP/1.1 200 OK' ]] || fail "$path with Accept-Encoding $encoding: $status"
  done
done

wasm="/v/$version/pkg/tilo_browser_runtime_bg.wasm"
headers=$(head_of "$home" "$wasm" -H 'Accept-Encoding: br, zstd')
grep -q '^content-type: application/wasm' <<<"$headers" || fail "wasm type: $headers"
grep -q '^content-encoding: \(zstd\|br\)' <<<"$headers" || fail "wasm is not served precompressed: $headers"
grep -qi '^vary:.*accept-encoding' <<<"$headers" || fail "no Vary: Accept-Encoding: $headers"
grep -q '^cache-control: public, max-age=31536000, immutable' <<<"$headers" || fail "versioned files must be immutable: $headers"
grep -q '^content-encoding:' <(head_of "$home" "$wasm") && fail "a client without br/zstd got an encoded body"
grep -q '^content-type: text/javascript' <(head_of "$home" "/v/$version/entry-page.mjs") || fail "modules need a JavaScript type"

# A versioned file the image does not have is a 404 no cache may keep: during
# a rolling update a page from a new pod asks an old one for the new
# version's files, and a 404 cached as immutable would stand at that edge for
# a year after the rollout finished.
missing=$("$engine" exec "$home" curl -sS -o /dev/null -D - "http://127.0.0.1:8080/v/$version/pkg/absent.wasm" | tr -d '\r' | lower_names)
[[ $(head -1 <<<"$missing") == 'HTTP/1.1 404 Not Found' ]] || fail "a missing versioned file: $(head -1 <<<"$missing")"
grep -q '^cache-control: no-store' <<<"$missing" || fail "a missing versioned file must not be cached: $missing"

# Which versions the image serves: only its own, or, given the image a deploy
# replaces, its own and that image's own (carry.sh), never more.
serves=$("$engine" exec "$home" ls /srv/tilo-web/v | sort | tr '\n' ' ')
carried=
if [[ -n $outgoing ]]; then
  carried=$("$engine" run --rm --network none --entrypoint cat "$outgoing" /srv/tilo-web/version.json | jq -r .version)
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
label=$("$engine" image inspect --format '{{index .Config.Labels "so.tilo.web-entry.carried-version"}}' "$image")
[[ $label == "$carried" ]] || fail "the image is labelled as carrying ${label:-nothing}, not $carried"
# A new tab loads only the incoming version...
for entry in / /app/; do
  grep -q "v/$carried/" <<<"$(get "$home" "$entry")" && fail "$entry refers to the outgoing version $carried"
done
# ...and a tab still on the outgoing one opens an app it has not loaded yet:
# its module is a whole, immutable 200, as are all the version's files,
# byte for byte the outgoing image's.
module="/v/$carried/pkg/tilo_browser_runtime.calculator.wasm"
headers=$(head_of "$home" "$module" -H 'Accept-Encoding: br, zstd')
[[ $(head -1 <<<"$headers") == 'HTTP/1.1 200 OK' ]] || fail "$module: $(head -1 <<<"$headers")"
grep -q '^cache-control: public, max-age=31536000, immutable' <<<"$headers" || fail "$module must be immutable: $headers"
grep -q '^content-type: application/wasm' <<<"$headers" || fail "$module type: $headers"
verify_version "$carried"
cmp -s <(get "$home" "/v/$carried/build.json") \
  <("$engine" run --rm --network none --entrypoint cat "$outgoing" "/srv/tilo-web/v/$carried/build.json") \
  || fail "v/$carried/build.json is not the outgoing image's"
echo "web entry image OK (version $version, carrying $carried)"
