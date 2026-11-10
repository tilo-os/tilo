#!/usr/bin/env bash
# The front door image before it is published: non-root, one port, Tilo's trust bundle at the
# fixed path its binaries read, a state volume the door owns, no configuration baked in, and a
# door that starts from its environment alone — listening where TILO_DOOR_LISTEN says, answering
# /health, naming the issuer it was given, and keeping the client it registered on the volume,
# with nothing written outside it so a read-only root filesystem serves.
#
# usage: test-image.sh ARCHIVE
# ARCHIVE is an OCI archive as t's image build writes it (the check image-front-door). It runs in
# the buildah image, without a daemon, as root in a micro-VM step.
set -euo pipefail
archive=${1:?usage: test-image.sh ARCHIVE}
. "$(dirname -- "${BASH_SOURCE[0]}")/../../../tools/ci/publish/lib.sh"
use_buildah front-door-test

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
if grep -q '^TILO_DOOR_' <<<"$environment"; then
  die 'the image sets TILO_DOOR_* itself, so a deployment cannot change them'
fi

container=$(b from --pull=never --quiet "$image")
door=
cleanup() {
  [[ -z $door ]] || kill "$door" 2>/dev/null || true
  b umount "$container" >/dev/null 2>&1 || true
  b rm "$container" >/dev/null 2>&1 || true
}
trap cleanup EXIT

root=$(b mount "$container")
[[ -x $root/usr/local/bin/tilo-front-door ]] || die 'no /usr/local/bin/tilo-front-door'
[[ -s $root/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem ]] \
  || die 'the trust bundle is not where every Tilo binary reads it'
[[ $(stat -c '%u:%g %a' "$root/state") == '65532:65532 700' ]] \
  || die "/state is $(stat -c '%u:%g %a' "$root/state"), not 65532:65532 0700"

# Nothing is configured in the image: with no environment the door says what it needs instead of
# starting on a guess.
if refused=$(b run "$container" -- /usr/local/bin/tilo-front-door 2>&1); then
  die 'the door starts with no environment at all'
fi
grep -q 'TILO_DOOR_LISTEN is required' <<< "$refused" \
  || die "the door without an environment said: $refused"

# The volume Kubernetes mounts over /state.
state=$scratch/front-door-state
rm -rf "$state"
mkdir -m 0700 "$state"
chown 65532:65532 "$state"

port=18080
issuer=https://door.image.test
web_entry=https://tilo.example.test
relay=https://relay.example.test
# Every file of the root filesystem, as it stands: what the run below may not change. The
# listing is what the run is compared with, not the files' timestamps against the clock, which
# the archive's own times would answer for.
snapshot() { (cd "$root" && find . -xdev -mindepth 1 -printf '%p %s %T@ %m\n' | sort); }
before=$(snapshot)
b run --user 65532:65532 -v "$state:/state" \
  -e TILO_DOOR_LISTEN=127.0.0.1:$port \
  -e TILO_DOOR_ISSUER=$issuer \
  -e TILO_DOOR_WEB_ENTRY=$web_entry \
  -e TILO_DOOR_RELAY=$relay \
  -e TILO_DOOR_STATE=/state/door.json \
  "$container" -- /usr/local/bin/tilo-front-door &
door=$!

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
  kill -0 "$door" 2>/dev/null || die 'the door exited before answering /health'
  sleep 1
done
[[ -n $answer ]] || die 'the door did not answer /health within 30 seconds'
[[ $(code "$answer") == 200 ]] || die "/health answered: ${answer%%$'\r\n'*}"
[[ $(body "$answer") == '{"ok":true}' ]] || die "/health said: $(body "$answer")"

# The issuer the deployment names is the one agents are given.
answer=$(request GET /.well-known/oauth-authorization-server '') || die 'the door answered no metadata'
grep -Fq "\"issuer\":\"$issuer\"" <<< "$(body "$answer")" \
  || die "the metadata does not name $issuer: $(body "$answer")"
grep -Fq "\"authorization_endpoint\":\"$issuer/authorize\"" <<< "$(body "$answer")" \
  || die 'the metadata names another authorization endpoint'

# A client that registers itself is on the volume, written by the door's own uid: the state the
# deployment names, the volume it mounts, and the user it runs as, at once.
answer=$(request POST /register '{"client_name":"front door image test","redirect_uris":["http://127.0.0.1:9/callback"]}') \
  || die 'the door answered no registration'
[[ $(code "$answer") == 201 ]] || die "registration answered: ${answer%%$'\r\n'*}"
[[ $(stat -c '%u:%g %a' "$state/door.json") == '65532:65532 600' ]] \
  || die "/state/door.json is $(stat -c '%u:%g %a' "$state/door.json"), not 65532:65532 0600"
grep -Fq 'front door image test' "$state/door.json" || die 'the registered client is not on the volume'

# Read-only root filesystem: the run wrote nothing outside the volume. The volume (a bind
# mount) and the files the buildah step puts in the rootfs for its own bookkeeping (`/run`,
# `/dev`, `/proc`, `/sys`, the name, hosts and resolver the chroot gets) are the step's, not the
# image's, and are left out.
kill "$door" 2>/dev/null || true
wait "$door" 2>/dev/null || true
door=
# diff answers 1 for differences, which there always are (the volume): only 2 is its failure.
outside=$({ diff <(printf '%s\n' "$before") <(snapshot) || (( $? == 1 )); } \
  | sed -n 's/^[<>] //p' \
  | awk '$1 !~ /^\.\/(state|run|dev|proc|sys)(\/|$)/ && $1 !~ /^\.\/etc\/(hosts|hostname|resolv\.conf|mtab)$/ { print $1 }' \
  | sort -u)
if [[ -n $outside ]]; then
  die "the run changed $(grep -c . <<< "$outside") paths outside the volume, among them $(head -3 <<< "$outside" | tr '\n' ' '), which a read-only root filesystem refuses"
fi

echo "the front door image runs as 65532, serves $port from TILO_DOOR_LISTEN, names $issuer, and keeps its state on /state"
