#!/usr/bin/env bash
# Static contract for the digest-pinned RPM snapshot. This catches package-set
# drift before a cold image build spends minutes resolving an incomplete repo.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="$repo/.forgejo/workflows/snapshot-rpms.yml"
snapshot="$repo/os/Containerfile.rpms"
asset_manifest="$repo/os/external-assets/github-assets.tsv"
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
  parser_block="$(sed -n '/^[[:space:]]*FILES=(/,/^[[:space:]]*)/p' "$workflow")"
  printf '%s\n' "$parser_block" | grep -Fq "$relative" \
    && ok "$relative contributes packages to the snapshot parser" \
    || bad "$relative consumes frozen RPMs but is absent from the snapshot parser"
  has "$workflow" "- $relative" "$relative changes retrigger the RPM snapshot"
done

# Exercise the workflow's package grammar over its declared sources. Apart
# from proving virtio-win is captured, requiring output from every source keeps
# a syntax change from silently making one whole Containerfile invisible.
mapfile -t parser_sources < <(
  sed -n '/^[[:space:]]*FILES=(/,/^[[:space:]]*)/p' "$workflow" \
    | grep -oE 'os/[A-Za-z0-9_./-]*Containerfile([.][A-Za-z0-9_-]+)?' \
    | sort -u
)

for relative in "${parser_sources[@]}"; do
  [ -f "$repo/$relative" ] || { bad "snapshot parser source $relative exists"; continue; }
  packages="$(awk '
    /(dnf|microdnf) install -y/ {
      blk=$0
      while (blk ~ /\\[[:space:]]*$/) { getline nx; sub(/\\[[:space:]]*$/,"",blk); blk=blk" "nx }
      sub(/.*install -y/,"",blk)
      sub(/&&.*/,"",blk)
      n=split(blk,a," ")
      for (i=1;i<=n;i++) {
        if (a[i] ~ /^-/) continue
        if (a[i] ~ /^[A-Za-z0-9][A-Za-z0-9._+-]*$/) print a[i]
      }
    }
  ' "$repo/$relative" | sort -u)"
  [ -n "$packages" ] \
    && ok "$relative yields packages through the workflow grammar" \
    || bad "$relative has no package captured by the workflow grammar"
done

all_packages="$(awk '
  /(dnf|microdnf) install -y/ {
    blk=$0
    while (blk ~ /\\[[:space:]]*$/) { getline nx; sub(/\\[[:space:]]*$/,"",blk); blk=blk" "nx }
    sub(/.*install -y/,"",blk); sub(/&&.*/,"",blk)
    n=split(blk,a," ")
    for (i=1;i<=n;i++) if (a[i] !~ /^-/ && a[i] ~ /^[A-Za-z0-9][A-Za-z0-9._+-]*$/) print a[i]
  }
' "${parser_sources[@]/#/$repo/}" | sort -u)"
printf '%s\n' "$all_packages" | grep -qx 'virtio-win' \
  && ok 'virtio-win is in the derived frozen package set' \
  || bad 'virtio-win is missing from the derived frozen package set'

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

if grep -Rqs --exclude-dir=output --include='Containerfile*' 'DROP_RPMDB_WAL' \
  "$repo/os" || grep -Fq 'DROP_RPMDB_WAL' "$workflow"; then
  bad 'obsolete DROP_RPMDB_WAL workaround remains'
else
  ok 'obsolete DROP_RPMDB_WAL workaround is absent'
fi

exit "$fail"
