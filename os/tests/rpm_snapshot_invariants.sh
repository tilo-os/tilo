#!/usr/bin/env bash
# Static contract for the digest-pinned RPM snapshot. This catches package-set
# drift before a cold image build spends minutes resolving an incomplete repo.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="$repo/.forgejo/workflows/snapshot-rpms.yml"
snapshot="$repo/os/Containerfile.rpms"
image_workflow="$repo/.forgejo/workflows/build-image.yml"
build_script="$repo/os/dev/build-image.sh"
asset_manifest="$repo/os/external-assets/github-assets.tsv"
derive="$repo/os/dev/derive-rpm-package-set.py"
fail=0

ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }
has() {
  local file="$1" needle="$2" message="$3"
  grep -Fq -- "$needle" "$file" && ok "$message" || bad "$message"
}

# Every image which consumes the frozen repository must contribute its package
# blocks to the workflow parser and retrigger the snapshot when it changes.
mapfile -t frozen_consumers < <(
  grep -lE 'from=rpms|enablerepo=frozen|RPMS_REF' \
    "$repo/os/Containerfile" \
    "$repo/os/Containerfile.host" \
    "$repo/os/containers/qos-relay/Containerfile" \
    "$repo/os/dev/composefs/Containerfile" \
    "$repo/os/kernel/Containerfile"
)

for file in "${frozen_consumers[@]}"; do
  relative="${file#"$repo/"}"
  "$derive" --sources | grep -Fxq "$relative" \
    && ok "$relative contributes packages to the snapshot parser" \
    || bad "$relative consumes frozen RPMs but is absent from the snapshot parser"
  has "$workflow" "- $relative" "$relative changes retrigger the RPM snapshot"
done

# Exercise the workflow's package grammar over its declared sources. Apart
# from proving virtio-win is captured, requiring output from every source keeps
# a syntax change from silently making one whole Containerfile invisible.
mapfile -t parser_sources < <("$derive" --sources)

for relative in "${parser_sources[@]}"; do
  [ -f "$repo/$relative" ] || { bad "snapshot parser source $relative exists"; continue; }
  packages="$(PYTHONDONTWRITEBYTECODE=1 python3 - "$derive" "$repo/$relative" <<'PY'
import importlib.util, pathlib, sys
spec = importlib.util.spec_from_file_location("rpm_package_set", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
print("\n".join(sorted(set(module.packages(pathlib.Path(sys.argv[2]))))))
PY
)"
  [ -n "$packages" ] \
    && ok "$relative yields packages through the workflow grammar" \
    || bad "$relative has no package captured by the workflow grammar"
done

all_packages="$("$derive")"
for package in wine wine-dxvk gamescope qemu-kvm libvirt-client \
  libvirt-daemon-kvm libvirt-daemon-config-nwfilter swtpm swtpm-tools \
  edk2-ovmf virtio-win freerdp xwayland-satellite virt-viewer xorriso \
  genisoimage udftools; do
  printf '%s\n' "$all_packages" | grep -Fxq "$package" \
    && ok "$package is in the derived frozen package set" \
    || bad "$package is missing from the derived frozen package set"
done

has "$workflow" './os/dev/derive-rpm-package-set.py' \
  'snapshot workflow uses the invariant-tested package parser'
has "$workflow" '- os/dev/derive-rpm-package-set.py' \
  'package parser changes retrigger the snapshot'

has "$workflow" '- os/external-assets/github-assets.tsv' \
  'pinned external asset changes retrigger the snapshot'
has "$workflow" './os/dev/cache-github-assets.sh' \
  'snapshot materializes checksum-verified external inputs'
has "$workflow" 'github-assets=os/external-assets/cache' \
  'snapshot receives only the materialized external-input cache'
has "$snapshot" '/qos-assets/virtio-win-0.1.302-1.noarch.rpm' \
  'snapshot resolver installs the content-pinned virtio RPM'
has "$asset_manifest" $'2f58eea024e0221a873032761dc3ed3262409cd80860b7b27d3d5a0f206616c9\tvirtio-win-0.1.302-1.noarch.rpm\t' \
  'virtio RPM has a reviewed SHA-256 pin'
if grep -Eq 'fedorapeople.org/.*/(stable|latest)/' "$asset_manifest" "$snapshot"; then
  bad 'virtio RPM uses a moving upstream repository'
else
  ok 'virtio RPM uses an immutable archived version URL'
fi

packages_line="$(grep -F 'dnf install -y --downloadonly' "$snapshot" || true)"
if [ -n "$packages_line" ] \
  && [[ "$packages_line" == *'${PACKAGES}'* ]] \
  && [[ "$packages_line" != *'--skip-unavailable'* ]]; then
  ok 'snapshot transaction resolves the complete package set without skipping unavailable RPMs'
else
  bad 'snapshot transaction can omit unavailable packages or does not consume PACKAGES'
fi

first_instruction="$(grep -m1 -E '^(ARG|FROM) ' "$snapshot")"
[ "$first_instruction" = 'ARG BASE_REF' ] \
  && ok 'BASE_REF is globally declared before the first snapshot stage' \
  || bad 'BASE_REF must be declared before the first FROM'
has "$snapshot" '/repo/qos-direct-package-specs.txt' \
  'snapshot publishes its direct package manifest'
has "$snapshot" 'dnf install -y --downloadonly --setopt=reposdir=/repo ${PACKAGES}' \
  'snapshot proves the frozen repo resolves the complete transaction offline'
has "$image_workflow" './os/dev/verify-rpm-snapshot-package-set.sh docker "$RPMS_REF"' \
  'production CI rejects a stale frozen package manifest before building'
has "$build_script" 'verify-rpm-snapshot-package-set.sh" podman "$RPMS_REF"' \
  'local production builds reject a stale frozen package manifest'

if grep -Rqs --exclude-dir=output --include='Containerfile*' 'DROP_RPMDB_WAL' \
  "$repo/os" || grep -Fq 'DROP_RPMDB_WAL' "$workflow"; then
  bad 'obsolete DROP_RPMDB_WAL workaround remains'
else
  ok 'obsolete DROP_RPMDB_WAL workaround is absent'
fi

exit "$fail"
