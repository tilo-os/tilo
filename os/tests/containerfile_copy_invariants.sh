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
# the build-only residual OpenSSL ABI package gate (removed after it runs),
# the xdg-desktop-portal preference that selects the Tilo backend,
# and the opt-in codec-pack helper overlay (helper, units, keys, polkit rule,
# and the NetworkManager dispatcher hook that starts a deferred install).
# The redundant model-storage oneshot is now absent; systemd provisions the
# model and gateway directories directly without a helper on socket activation.
# Rustls now compiles the fixed CA-bundle path
# into its loader, so the former systemd environment override is absent.
# Tilo build outputs are installed through os/components.toml, so their rows
# name the manifest component and build output instead of a builder /out path;
# every destination is unchanged, and /out/optional/ -> /usr/bin/ is now the
# explicit optional /usr/bin/browser entry it always carried. The VM console
# port udev rule and the generated bash/fish/zsh completions for app and tilo
# (with their store/system aliases) are deliberate additions, as is the
# sandboxed PDF engine helper at /usr/libexec/tilo-pdf-helper, and the
# synced-worktree git defaults (/etc/gitconfig and its snippet) are part of the
# payload, as is the room store volume: its helper, the unit that prepares it
# before fs.service and the loop unit of image-backed stores. The signature
# attachment lookups for the code.tilo.so OS repository and the
# registry.tilo.so owner namespace are staged with the other trust inputs; the
# former code-host lookup is gone. The PipeWire client config behind the
# echo-cancelled microphone for calls is part of the payload. The DDC/CI udev
# rule and its i2c-dev modules-load snippet, which let the session set external
# monitors' brightness, are deliberate additions. So are the cloud account
# service, tilo-fs-cloud.service, and its vault credential helper at
# /usr/libexec/tilo-account-credential. The finished-rootfs UI toolkit gate is
# a deliberate addition. The runtime NetworkManager resolver symlink is
# restored after bootc transient cleanup, before split/ukify seal the rootfs.
# The build-only DNF/RPM clock shim and transaction-state cleaner add their
# explicit rootfs COPY rows; the shim and wrappers are removed before sealing.
# joycond's binary, unit and modules-load snippet (through desktop-binaries)
# and its Joy-Con-scoped udev rule are deliberate additions. So is
# /usr/lib/tilo/web-engine-entitled.conf, the image-owned list of packages
# the launcher lets build a web engine's own renderer sandbox. The
# tilo-identity.service user unit (a one-shot chain-v1 repair) is gone; the
# compositor verifies and repairs identity at startup. The session flight
# recorder and tilo-session-report (through desktop-binaries) are deliberate
# additions: the image's tilo-session now records each login. The
# systemd-boot-random-seed drop-in, which skips the seed refresh on a
# read-only /boot, is a deliberate addition. The rootfs
# takes its build outputs from native-builder-payload, the builder's outputs
# copied unchanged, so it no longer needs the compile stage's intermediate
# cache.
# tilo-esp-write and its unit, the only writer of the read-only ESP, are
# deliberate additions.
# The virtio-only Cage session wrapper is installed as a single executable;
# physical GPU sessions still use the ordinary greeter command.
# tilo-hidepid.service is a deliberate removal: a global hidepid /proc hides
# PID 1 from unprivileged processes and breaks libsystemd unit lookups.
# user-tmpfiles.d/tilo-fs.conf is a deliberate addition: it creates the
# filesystem services' roots, which their PrivateIPC sandbox cannot.
# The user-tmpfiles.d snippets live in /usr/share, the only system path
# systemd-tmpfiles --user reads.
# Sorting
# intentionally ignores instruction order:
# destinations are unique, and the source-stage/source/destination triple defines
# each COPY's installed bytes and path. Modes remain source modes except at the
# explicitly equivalent COPY --chmod=0755 sites checked below. COPY preserves
# ownership, mtimes and symlinks through scratch stages; no RUN mutates them.
# ARM virtio software rendering adds one reviewed LLVM CPU shim payload.
expected_contract=8cb6c8c67d15a5aea1acc735f30a2d85c2ec54784be7fc5cbe0813cedfcfccbf
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
  (stage == "rootfs" || stage == "rootfs-base") && $1 ~ /^(ARG|COPY|RUN|ADD|ENV|LABEL|WORKDIR|USER|EXPOSE|VOLUME|STOPSIGNAL|HEALTHCHECK|SHELL|CMD|ENTRYPOINT)$/ { n++ }
  END { print n+0 }
')"
# The existing image used 123; provider selection adds ARG, COPY and its
# native dynamic-linker validation RUN. Public trust adds four ARGs, two
# additional COPYs and a validation RUN before initramfs generation. External
# AT-SPI validation adds two narrowly scoped COPY instructions.
# Firewall-first NetworkManager ordering adds one explicit drop-in COPY.
# Recording whether the Windows provider is included adds an ARG and a LABEL.
# Synced-worktree git defaults add two: the snippet and the /etc/gitconfig
# that includes it.
# The opt-in codec-pack overlay adds one COPY of its scratch stage.
# The echo-cancel PipeWire config adds one COPY; folding the three
# /usr/lib/systemd/user units into one COPY removes two.
# A build-only resolver placeholder and a final COPY restore the runtime
# symlink outside the cleanup RUN, keeping BuildKit's DNS mount out of /run.
# The fixed-clock build adds an ARG, four COPYs, and an early path assertion.
# Pinning the unpacked base's directory times to the epoch adds one RUN.
# The single virtio Cage wrapper COPY brings the reviewed rootfs to 151.
# The ARM virtual graphics CPU shim adds one COPY, bringing this to 152.
[ "$instructions" -le 152 ] || {
  echo "rootfs layer budget exceeded: $instructions instructions (maximum 152)" >&2
  exit 1
}

# Bulk source-tree overlays are specifically forbidden: they can silently ship
# installer helpers, test fixtures, or future files merely added under os/usr.
if containerfile_instructions "$containerfile" |
   grep -Eq '^COPY( --[^ ]+)* (os/usr/?|os/usr/?) /'; then
  echo 'bulk COPY of os/usr is forbidden' >&2
  exit 1
fi

for overlay in desktop-binaries remote-capture-files rollback-files recovery-files codec-pack-files; do
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
  os/usr/libexec/tilo-store-volume \
  os/usr/libexec/tilo-verified-boot-install-preflight \
  os/usr/libexec/tilo-security-audit \
  os/usr/libexec/tilo-codec-pack \
  os/usr/lib/NetworkManager/dispatcher.d/90-tilo-codec-pack; do
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
