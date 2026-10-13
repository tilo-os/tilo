#!/usr/bin/env bash
# Smoke-test a web entry image: the entry pages, entry.json with and without
# a home workspace, and the caching, encoding and type headers CDNs and
# browsers rely on. HTTP is spoken from inside the container (the image
# carries curl), so this works when the Docker daemon is remote too. It runs
# as a restricted pod does: read-only, uid 10001, no capabilities, no_new_privs.
# usage: test-image.sh IMAGE   (docker or podman as $CONTAINER, default docker)
set -euo pipefail
image=${1:?usage: test-image.sh IMAGE}
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
head_of() { "$engine" exec "$1" curl -fsS -o /dev/null -D - "${@:3}" "http://127.0.0.1:8080$2" | tr -d '\r' | sed 's/^\([^:]*\):/\L\1:/'; }

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

page=$(get "$home" /)
version=$(grep -o "v/[0-9a-f]\{20\}/entry-page.mjs" <<<"$page" | head -1 | cut -d/ -f2)
[[ -n $version ]] || fail "the entry page imports no versioned entry-page.mjs"
grep -q "'../v/$version/entry-page.mjs'" <<<"$(get "$home" /app/)" || fail "the app entry points elsewhere"
grep -q '^cache-control: no-cache' <(head_of "$home" /) || fail "/ must revalidate"

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
echo "web entry image OK (version $version)"
