#!/usr/bin/env bash
# Fails when the registry owner key t hands release checks (TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY)
# is not the one os/publication.env names. A package or image built with t's value installs it as
# the trust root of system app updates, overriding the key compiled into the CLI, so a stale value
# there makes released devices trust the wrong store owner. Only key prefixes are printed.
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
want=$(sed -n 's/^TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY=//p' "$here/os/publication.env")
[[ $want =~ ^[0-9a-f]{64}$ ]] || { echo "os/publication.env names no 64-hex TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY" >&2; exit 1; }
got=${TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY:-}
[[ -n $got ]] || { echo "t handed this check no TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY" >&2; exit 1; }
if [[ $got != "$want" ]]; then
  echo "t's TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY (${got:0:16}…) is not os/publication.env's (${want:0:16}…): replace the secret in t's server config before a release builds with it" >&2
  exit 1
fi
echo "t's TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY is os/publication.env's (${want:0:16}…)"
