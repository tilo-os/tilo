#!/usr/bin/env bash
# The API server image before it is published: non-root, one port, Tilo's trust bundle at the
# fixed path its binaries read, a state volume the server owns, no configuration baked in, and a
# server that starts from its environment alone — listening where TILO_API_LISTEN says, answering
# /health, naming the issuer it was given, and keeping the client it registered on the volume,
# with nothing written outside it so a read-only root filesystem serves.
#
# usage: test-image.sh ARCHIVE
# ARCHIVE is an OCI archive as t's image build writes it (the check image-api). It runs in
# the buildah image, without a daemon, as root in a micro-VM step.
set -euo pipefail
archive=${1:?usage: test-image.sh ARCHIVE}
. "$(dirname -- "${BASH_SOURCE[0]}")/../../../tools/ci/publish/lib.sh"
use_buildah api-test

image=$(b pull --quiet "oci-archive:$archive")
inspect_image() { b inspect --type image --format "$1" "$image"; }
user=$(inspect_image '{{.OCIv1.Config.User}}')
[[ $user == 65532:65532 ]] || die "the image runs as $user, not uid 65532"
# Each answer is read whole before it is searched: under pipefail, `grep -q` leaving at its
# first match can cut off buildah mid-write and fail the check on a value that was there.
ports=$(inspect_image '{{json .OCIv1.Config.ExposedPorts}}')
grep -Fq '"8080/tcp"' <<<"$ports" || die 'the image does not expose 8080/tcp'
# Nothing of the deployment is baked in: a changed value is a changed environment, not an image.
environment=$(inspect_image '{{range .OCIv1.Config.Env}}{{println .}}{{end}}')
if grep -q '^TILO_API_' <<<"$environment"; then
  die 'the image sets TILO_API_* itself, so a deployment cannot change them'
fi

container=$(b from --pull=never --quiet "$image")
server=
cleanup() {
  [[ -z $server ]] || kill "$server" 2>/dev/null || true
  b umount "$container" >/dev/null 2>&1 || true
  b rm "$container" >/dev/null 2>&1 || true
}
trap cleanup EXIT

root=$(b mount "$container")
# Every Tilo image is Fedora, the OS included: the server trusts the same
# certificates and links the same libraries as the relay beside it.
b run "$container" -- /usr/bin/busybox grep -qx 'ID=fedora' /etc/os-release \
  || die 'the runtime is not a Fedora base'
[[ -x $root/usr/local/bin/tilo-api ]] || die 'no /usr/local/bin/tilo-api'
[[ -s $root/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem ]] \
  || die 'the trust bundle is not where every Tilo binary reads it'
[[ $(stat -c '%u:%g %a' "$root/state") == '65532:65532 700' ]] \
  || die "/state is $(stat -c '%u:%g %a' "$root/state"), not 65532:65532 0700"

# Nothing is configured in the image: with no environment the server says what it needs instead of
# starting on a guess.
if refused=$(b run "$container" -- /usr/local/bin/tilo-api 2>&1); then
  die 'the server starts with no environment at all'
fi
grep -q 'TILO_API_LISTEN is required' <<< "$refused" \
  || die "the server without an environment said: $refused"

# The volume Kubernetes mounts over /state.
state=$scratch/api-state
rm -rf "$state"
mkdir -m 0700 "$state"
chown 65532:65532 "$state"

port=18080
issuer=https://api.image.test
web_entry=https://tilo.example.test
relay=https://relay.example.test
# Every file of the root filesystem, as it stands: what the run below may not change. The
# listing is what the run is compared with, not the files' timestamps against the clock, which
# the archive's own times would answer for.
snapshot() { (cd "$root" && find . -xdev -mindepth 1 -printf '%p %s %T@ %m\n' | sort); }
before=$(snapshot)
b run --user 65532:65532 -v "$state:/state" \
  -e TILO_API_LISTEN=127.0.0.1:$port \
  -e TILO_API_ORIGIN=$issuer \
  -e TILO_API_WEB_ENTRY=$web_entry \
  -e TILO_API_HOST=$relay \
  -e TILO_API_STATE=/state/api.json \
  "$container" -- /usr/local/bin/tilo-api &
server=$!

request() {
  exec 3<>"/dev/tcp/127.0.0.1/$port" || return 1
  printf '%s %s HTTP/1.0\r\nHost: 127.0.0.1:%s\r\nContent-Type: application/json\r\nContent-Length: %s\r\nConnection: close\r\n\r\n%s' \
    "$1" "$2" "$port" "${#3}" "$3" >&3
  timeout 5 cat <&3
  exec 3<&- 3>&-
}
# The status code of an answer, as its first line carries it (`HTTP/1.0 200 OK`).
code() { read -r _ answer_code _ <<< "${1%%$'\r\n'*}"; printf '%s' "$answer_code"; }
# The body of an answer.
body() { printf '%s' "${1##*$'\r\n\r\n'}"; }

answer=
for _ in $(seq 1 30); do
  if answer=$(request GET /health '' 2>/dev/null); then break; fi
  kill -0 "$server" 2>/dev/null || die 'the server exited before answering /health'
  sleep 1
done
[[ -n $answer ]] || die 'the server did not answer /health within 30 seconds'
[[ $(code "$answer") == 200 ]] || die "/health answered: ${answer%%$'\r\n'*}"
# The server answers for itself, and says what the site answered for its sign-in page: here the
# site does not resolve, so it names the page it could not reach.
health=$(body "$answer")
[[ $health == '{"ok":true,"sign_in":'* ]] || die "/health said: $health"
[[ $health == *'/sign-in'* ]] || die "/health named no sign-in page: $health"

# The issuer the deployment names is the one agents are given.
answer=$(request GET /.well-known/oauth-authorization-server '') || die 'the server answered no metadata'
grep -Fq "\"issuer\":\"$issuer\"" <<< "$(body "$answer")" \
  || die "the metadata does not name $issuer: $(body "$answer")"
grep -Fq "\"authorization_endpoint\":\"$issuer/authorize\"" <<< "$(body "$answer")" \
  || die 'the metadata names another authorization endpoint'

# A client that registers itself is on the volume, written by the server's own uid: the state the
# deployment names, the volume it mounts, and the user it runs as, at once.
answer=$(request POST /register '{"client_name":"API server image test","redirect_uris":["http://127.0.0.1:9/callback"]}') \
  || die 'the server answered no registration'
[[ $(code "$answer") == 201 ]] || die "registration answered: ${answer%%$'\r\n'*}"
[[ $(stat -c '%u:%g %a' "$state/api.json") == '65532:65532 600' ]] \
  || die "/state/api.json is $(stat -c '%u:%g %a' "$state/api.json"), not 65532:65532 0600"
grep -Fq 'API server image test' "$state/api.json" || die 'the registered client is not on the volume'

# Read-only root filesystem: the run wrote nothing outside the volume. The volume (a bind
# mount) and the files the buildah step puts in the rootfs for its own bookkeeping (`/run`,
# `/dev`, `/proc`, `/sys`, the name, hosts and resolver the chroot gets) are the step's, not the
# image's, and are left out.
kill "$server" 2>/dev/null || true
wait "$server" 2>/dev/null || true
server=
# diff answers 1 for differences, which there always are (the volume): only 2 is its failure.
outside=$({ diff <(printf '%s\n' "$before") <(snapshot) || (( $? == 1 )); } \
  | sed -n 's/^[<>] //p' \
  | awk '$1 !~ /^\.\/(state|run|dev|proc|sys)(\/|$)/ && $1 !~ /^\.\/etc\/(hosts|hostname|resolv\.conf|mtab)$/ { print $1 }' \
  | sort -u)
if [[ -n $outside ]]; then
  die "the run changed $(grep -c . <<< "$outside") paths outside the volume, among them $(head -3 <<< "$outside" | tr '\n' ' '), which a read-only root filesystem refuses"
fi

echo "the API server image runs as 65532, serves $port from TILO_API_LISTEN, names $issuer, and keeps its state on /state"
