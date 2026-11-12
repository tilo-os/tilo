#!/usr/bin/env bash
# Production's Caddy, built for t's checks of the local tilo.sh stack: the one
# pin (os/containers/tilo-sh/caddy.env), fetched once here so a check's
# bring-up neither downloads 18 MB nor needs the network. It is the same
# release the tls container and the web entry image run.
#
# usage: tilo-sh-caddy.sh OUT
# OUT gets `caddy`, executable.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
out=${1:?usage: tilo-sh-caddy.sh OUT}
. "$repo_root/os/containers/tilo-sh/caddy.env"
# The pin names the release archive the stack's checks run (x86_64 nodes); an
# architecture it does not cover is a new pin, not a checksum skipped.
[[ $(uname -m) == x86_64 ]] || die "the Caddy pin covers x86_64, not $(uname -m)"
archive="caddy_${CADDY_VERSION}_linux_amd64.tar.gz"
rm -rf "$out"
mkdir -p "$out"
work=$scratch/caddy
rm -rf "$work"
mkdir -p "$work"
curl -fsSL -o "$work/$archive" \
  "https://github.com/caddyserver/caddy/releases/download/v$CADDY_VERSION/$archive" \
  || die "could not fetch $archive"
printf '%s  %s\n' "$CADDY_SHA512" "$work/$archive" | sha512sum -c --quiet - \
  || die "$archive is not release $CADDY_VERSION's"
# The archive's entries are Caddy's release user's (uid 1001), which a job's
# root in a user namespace cannot become: the file's own mode is what matters.
tar --no-same-owner -xzf "$work/$archive" -C "$out" caddy
rm -rf "$work"
"$out/caddy" version | grep -q "v$CADDY_VERSION" || die "$out/caddy is not $CADDY_VERSION"
echo "caddy $CADDY_VERSION: $out/caddy"
