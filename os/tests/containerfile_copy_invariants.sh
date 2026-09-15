#!/usr/bin/env bash
# Static proof that rootfs COPY consolidation changes layer shape, not payload.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
containerfile="$repo/os/Containerfile"
# shellcheck source=lib/containerfile.sh
. "$here/lib/containerfile.sh"

# Captured from the 178 effective rootfs copy mappings after rebasing the
# consolidation over main's app-host, native AT-SPI, and VirtIO media additions.
# Compared with the original 173-entry contract, these are exactly five added
# mappings and no removals. Sorting intentionally ignores instruction order:
# destinations are unique, and the source-stage/source/destination triple defines
# each COPY's installed bytes and path. Modes remain source modes except at the
# explicitly equivalent COPY --chmod=0755 sites checked below. COPY preserves
# ownership, mtimes and symlinks through scratch stages; no RUN mutates them.
expected_contract=9b90479025b4e597a048d5df0205df1de39d393ce35595d3a0c48cea095159ad
actual_contract="$(containerfile_expanded_rootfs_copy_contract "$containerfile" |
  LC_ALL=C sort | sha256sum | awk '{print $1}')"
[ "$actual_contract" = "$expected_contract" ] || {
  echo "rootfs copy contract changed: expected $expected_contract, got $actual_contract" >&2
  exit 1
}

# A unique destination makes the sorted contract sufficient: no later COPY can
# mask an earlier payload at the same path. Directory-content copies remain
# single-source entries and were intentionally not consolidated.
duplicates="$(containerfile_expanded_rootfs_copy_contract "$containerfile" |
  awk -F '\t' '{ count[$3]++ } END { for (path in count) if (count[path] > 1) print path }')"
[ -z "$duplicates" ] || {
  printf 'rootfs copy destinations are no longer unique:\n%s\n' "$duplicates" >&2
  exit 1
}

instructions="$(containerfile_instructions "$containerfile" | awk '
  $1 == "FROM" { stage=$NF; next }
  stage == "rootfs" && $1 ~ /^(ARG|COPY|RUN|ADD|ENV|LABEL|WORKDIR|USER|EXPOSE|VOLUME|STOPSIGNAL|HEALTHCHECK|SHELL|CMD|ENTRYPOINT)$/ { n++ }
  END { print n+0 }
')"
[ "$instructions" -le 120 ] || {
  echo "rootfs layer budget exceeded: $instructions instructions (maximum 120)" >&2
  exit 1
}

# Bulk source-tree overlays are specifically forbidden: they can silently ship
# installer helpers, test fixtures, or future files merely added under os/usr.
if containerfile_instructions "$containerfile" |
   grep -Eq '^COPY( --[^ ]+)* (os/usr/?|os/usr/?) /'; then
  echo 'bulk COPY of os/usr is forbidden' >&2
  exit 1
fi

for overlay in desktop-binaries rollback-files recovery-files; do
  [ "$(containerfile_copy_contract "$containerfile" rootfs |
       awk -F '\t' -v s="$overlay" '$1==s && $2=="/" && $3=="/"{n++} END{print n+0}')" -eq 1 ] \
    || { echo "$overlay is not applied to rootfs exactly once" >&2; exit 1; }

  # Scratch overlays may only arrange COPY payload. A RUN could alter hashes,
  # ownership, capabilities, labels, or symlink targets behind the manifest.
  bad_instruction="$(containerfile_instructions "$containerfile" | awk -v wanted="$overlay" '
    $1 == "FROM" { stage=$NF; next }
    stage == wanted && $1 !~ /^(COPY|#)$/ && NF { print; exit }
  ')"
  [ -z "$bad_instruction" ] \
    || { echo "$overlay mutates payload: $bad_instruction" >&2; exit 1; }
done

containerfile_instructions "$containerfile" |
  grep -Eq '^COPY .*--chown(=| )' \
  && { echo 'COPY ownership override requires updating the payload proof' >&2; exit 1; } \
  || true

for path in \
  os/scripts/finalize-rpmdb.sh \
  os/usr/libexec/qos-session \
  os/usr/libexec/qos-disk-encrypt \
  os/usr/libexec/qos-verified-boot-install-preflight \
  os/usr/libexec/qos-security-audit; do
  containerfile_instructions "$containerfile" |
    awk -v path="$path" '
      $1 == "COPY" {
        chmod=0; source=0
        for (i=2; i<NF; i++) {
          if ($i == "--chmod=0755") chmod=1
          if ($i == path) source=1
        }
        if (chmod && source) found=1
      }
      END { exit !found }
    ' \
    || { echo "$path lost its enforced 0755 installed mode" >&2; exit 1; }
done

echo "rootfs COPY contract unchanged; $instructions-instruction layer budget satisfied"
