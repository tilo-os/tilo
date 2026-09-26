#!/usr/bin/env bash
# End-to-end test of the codec-pack helper against a real, signed, local
# repository (tools/tests/test_codec_pack_e2e.py): rpmbuild, rpmsign,
# createrepo_c, dnf5, rpmkeys and rpm2archive, all rootless.
#
# Runs directly when those tools are present (the development container has
# them, see dist/dev/Containerfile.fedora44). On a host without them it runs
# inside the existing development distrobox named in .build-container, rather
# than starting a new container. Skips (exit 0) when neither is available.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
test_file=tools/tests/test_codec_pack_e2e.py

have_tools() {
  local tool
  for tool in rpmbuild rpmsign createrepo_c dnf5 gpg rpmkeys rpm2archive python3; do
    command -v "$tool" >/dev/null 2>&1 || return 1
  done
}

if have_tools; then
  cd "$repo"
  exec python3 -m unittest -v "$test_file"
fi

box=""
for marker in "$repo/.build-container" "$repo/../../../.build-container"; do
  if [ -f "$marker" ]; then
    box="$(tr -d '[:space:]' < "$marker")"
    break
  fi
done
if [ -z "$box" ] || ! command -v podman >/dev/null 2>&1 \
  || [ "$(podman container inspect --format '{{.State.Running}}' "$box" 2>/dev/null)" != true ]; then
  echo "codec-pack-e2e: SKIP (no rpm tooling here and no running development container)" >&2
  exit 0
fi
exec podman exec -u "$(id -un)" -w "$repo" "$box" python3 -m unittest -v "$test_file"
