#!/usr/bin/env bash
# The digest-pinned Fedora bootc base (os/base-snapshot.env: BASE_REF) present in
# registry.tilo.so before the RPM snapshot builds FROM it: os/dev/ensure-base-mirror.sh copies
# BASE_SOURCE across, digests preserved, when the mirror lacks the pinned digest, and fails when
# upstream no longer serves it. Fedora rebuilds the upstream tag daily and collects the manifest
# it replaces, so a bumped BASE_REF has to reach the mirror while upstream still has it.
# Where the mirror has the pin already (anyone may pull the base) this passes without logging in;
# copying logs in as `oidc` with an identity token from t (`id_token = ["registry.tilo.so"]`),
# which only trusted runs get.
#
# usage: base-mirror.sh
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"
# shellcheck disable=SC1091
. os/base-snapshot.env
[[ ${BASE_REF:-} =~ ^registry\.tilo\.so/tilo/fedora-bootc@sha256:[0-9a-f]{64}$ ]] \
  || die 'BASE_REF is not a digest-pinned fedora-bootc'
if skopeo inspect --raw "docker://$BASE_REF" >/dev/null 2>&1; then
  printf 'mirrored already: %s\n' "$BASE_REF"
  exit 0
fi
auth=$scratch/base-mirror-auth.json
password=$(id_token registry.tilo.so)
printf '%s' "$password" | skopeo login --authfile "$auth" -u oidc --password-stdin registry.tilo.so >/dev/null
REGISTRY_AUTH_FILE=$auth bash os/dev/ensure-base-mirror.sh
