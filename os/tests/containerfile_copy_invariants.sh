#!/usr/bin/env bash
# Static proof that rootfs COPY consolidation changes layer shape, not payload.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
containerfile="$repo/os/Containerfile"
# shellcheck source=lib/containerfile.sh
. "$here/lib/containerfile.sh"

# Public trust now stages the selected keys, independent tracked references,
# and three validation tools under /run before installing validated bytes.
# test_public_trust_install.py proves default keys and development provenance;
# the contract below covers these explicit staging inputs plus existing payload.
# Captured from the effective rootfs copy mappings. The current change
# deliberately removes the obsolete root-owned shell-firstboot unit and adds
# non-blocking boot timers, a background-resource policy, and two model-service
# activation sockets, the staged-image owner trust gate, and the deferred
# BootPack status-projection timer, the physical-disk BFQ policy, the
# root-owned device-key initializer, the off-critical-path TPM hardening timer,
# the authenticated Chrony policy used by the image-trust freshness floor, the
# zero-delay firewall-first network target that keeps both services off the
# login chain without exposing an unfiltered interface, and
# the packaged-app external AT-SPI client and installed-app accessibility gate,
# the build-only residual OpenSSL ABI package gate (removed after it runs).
# The redundant model-storage oneshot is now absent; systemd provisions the
# model and gateway directories directly without a helper on socket activation.
# Rustls now compiles the fixed CA-bundle path
# into its loader, so the former systemd environment override is absent.
# Tilo build outputs are installed through os/components.toml, so their rows
# name the manifest component and build output instead of a builder /out path;
# every destination is unchanged, and /out/optional/ -> /usr/bin/ is now the
# explicit optional /usr/bin/browser entry it always carried. Sorting
# intentionally ignores instruction order:
# destinations are unique, and the source-stage/source/destination triple defines
# each COPY's installed bytes and path. Modes remain source modes except at the
# explicitly equivalent COPY --chmod=0755 sites checked below. COPY preserves
# ownership, mtimes and symlinks through scratch stages; no RUN mutates them.
expected_contract=a6d4f81db6a9e3bbe3fc02403cbeba82ebe948778befd52a8437b9909039e2b0
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
# The existing image used 123; provider selection adds ARG, COPY and its
# native dynamic-linker validation RUN. Public trust adds four ARGs, two
# additional COPYs and a validation RUN before initramfs generation. External
# AT-SPI validation adds two narrowly scoped COPY instructions.
# Firewall-first NetworkManager ordering adds one explicit drop-in COPY.
# Keep the current 137-instruction bound exact and reviewed.
[ "$instructions" -le 137 ] || {
  echo "rootfs layer budget exceeded: $instructions instructions (maximum 137)" >&2
  exit 1
}

# Bulk source-tree overlays are specifically forbidden: they can silently ship
# installer helpers, test fixtures, or future files merely added under os/usr.
if containerfile_instructions "$containerfile" |
   grep -Eq '^COPY( --[^ ]+)* (os/usr/?|os/usr/?) /'; then
  echo 'bulk COPY of os/usr is forbidden' >&2
  exit 1
fi

for overlay in desktop-binaries remote-capture-files rollback-files recovery-files; do
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
  os/usr/libexec/tilo-session \
  os/usr/libexec/tilo-disk-encrypt \
  os/usr/libexec/tilo-verified-boot-install-preflight \
  os/usr/libexec/tilo-security-audit; do
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
