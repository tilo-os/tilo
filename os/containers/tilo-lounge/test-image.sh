#!/usr/bin/env bash
# The Lounge image, before it is published: what the deployment and the
# entrypoint rely on — the user it runs as, its entrypoint, the ports the
# ingress forwards to, the programs starting inside the image, the bot
# programs it installs, and the product font catalog its app workers lay text
# out from — plus the revision label, which is what GET /mode reports at run
# time.
#
# usage: test-image.sh IMAGE
# IMAGE is an OCI archive (as tools/ci/publish/oci-build.sh writes it) or a
# reference buildah pulls (docker://…@sha256:…). It runs in the buildah image
# as root in a micro-VM step, without a daemon.
set -euo pipefail
usage='usage: test-image.sh IMAGE'
. "$(dirname -- "${BASH_SOURCE[0]}")/../../../tools/ci/publish/lib.sh"
use_buildah lounge overlay
fail() { echo "FAIL: $*" >&2; exit 1; }
# An archive path, or a reference as it is.
reference() { if [[ -f $1 ]]; then printf 'oci-archive:%s' "$1"; else printf '%s' "$1"; fi; }
image=$(b pull --quiet "$(reference "${1:?$usage}")")

inspect_image() { b inspect --type image --format "$1" "$image"; }

# A restricted pod runs it as this user, with every capability dropped; the
# files it needs are group-readable through the image's own modes.
user=$(inspect_image '{{.OCIv1.Config.User}}')
[[ $user == 10001 ]] || fail "the image runs as $user, not uid 10001"
entrypoint=$(inspect_image '{{json .OCIv1.Config.Entrypoint}}')
[[ $entrypoint == '["/usr/local/bin/tilo-lounge"]' ]] || fail "the entrypoint is $entrypoint"
# Each answer is read whole before it is searched: under pipefail, `grep -q`
# leaving at its first match can cut off buildah mid-write and fail the check
# on a value that was there.
ports=$(inspect_image '{{json .OCIv1.Config.ExposedPorts}}')
for port in 8790/tcp 9780/tcp 9781/udp; do
  grep -Fq "\"$port\"" <<<"$ports" || fail "port $port is not exposed"
done
revision=$(inspect_image '{{index .OCIv1.Config.Labels "org.opencontainers.image.revision"}}')
[[ $revision =~ ^[0-9a-f]{40}$ ]] || fail "the image's revision label is $revision"
# The label and the environment name the same build: /mode, and every log
# line, report the environment's.
environment=$(inspect_image '{{range .OCIv1.Config.Env}}{{println .}}{{end}}')
grep -Fqx "TILO_SOURCE_REVISION=$revision" <<<"$environment" \
  || fail "TILO_SOURCE_REVISION is not the image's revision"
grep -Fqx 'TILO_SECRETS_DIR=/run/tilo-rooms/secrets' <<<"$environment" \
  || fail "the host is not told where its sealed secrets live"

container=$(b from --pull=never --quiet "$image")
cleanup() { b umount "$container" >/dev/null 2>&1 || true; b rm "$container" >/dev/null 2>&1 || true; }
trap cleanup EXIT
# Each program starts in the image itself, before the container is mounted
# (buildah runs only an unmounted one). A binary that needs a library the image
# does not carry dies in the loader before its main, with "error while loading
# shared libraries: …: cannot open shared object file" on stderr and status
# 127; no file, mode or ELF check sees that, and the deployment found it as a
# crash loop. Both programs are answered from their main: the relay's
# `--version`, and the worker's refusal of a setting it cannot take (of which
# the loader knows nothing).
version=$(b run "$container" -- /usr/local/bin/tilo-relay --version) \
  || fail "tilo-relay does not start in the image"
[[ $version == "tilo-relay "* ]] || fail "tilo-relay --version answered: $version"
worker=0
b run "$container" -- bash -c 'DISCORD_GUILD_ID=not-a-discord-id exec /usr/local/bin/discord-worker' \
  >/dev/null || worker=$?
[[ $worker == 1 ]] || fail "discord-worker does not start in the image (status $worker)"
root=$(b mount "$container")
for program in tilo-relay discord-worker tilo-lounge; do
  [[ -x $root/usr/local/bin/$program ]] || fail "/usr/local/bin/$program is missing or not executable"
done
for program in tilo-relay discord-worker; do
  # The programs are the builder's binaries, not scripts a missing copy left behind.
  [[ $(head -c 4 "$root/usr/local/bin/$program" | od -An -c | tr -d ' ') == '177ELF' ]] \
    || fail "/usr/local/bin/$program is not a program"
done
[[ -f $root/usr/local/share/tilo/bots/discord-bridge.json ]] || fail "the bridge's bot program is missing"
# The catalog must be this build's: a container has no system fonts, so the
# bridge's app components would lay text out in whatever is left.
cmp -s "$root/usr/share/fonts/tilo/manifest.toml" "$repo_root/crates/sdk/ui-core/assets/fonts/manifest.toml" \
  || fail "the installed font catalog is not the one this build pins"

echo "ok: the Lounge image runs as 10001, serves 8790/tcp, 9780/tcp and 9781/udp, and carries the programs, the bot program and the catalog of $revision"
