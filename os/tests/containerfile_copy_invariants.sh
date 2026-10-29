#!/usr/bin/env bash
# Rules for the rootfs COPYs: unique destinations, a layer budget, no bulk or
# mutating overlays, and the modes and component inputs the image relies on.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
containerfile="$repo/os/Containerfile"
# shellcheck source=lib/containerfile.sh
. "$here/lib/containerfile.sh"

# A unique destination means no later COPY can mask an earlier payload at the
# same path. Directory-content copies remain
# single-source entries and were intentionally not consolidated.
# Two directory-content copies share /usr/ as their destination, with disjoint
# trees: the Windows provider's (provider.json only; its payload is the
# windows-compat sysext), and the assembled
# runtime tree
# (/usr/lib/tilo-runtime, /usr/share/fonts/tilo, /usr/share/tilo/font-catalogs),
# which must be one COPY so its hard links stay one layer's. The signed records
# of the runtimes and of the app store (/signatures) land in the trees the
# COPYs before them wrote, at paths those leave out.
duplicates="$(containerfile_expanded_rootfs_copy_contract "$containerfile" |
  awk -F '\t' '$1 == "runtime-image-payload" && $2 == "/image/usr/" && $3 == "/usr/" { next }
    $2 ~ /^\/signatures\// && ($1 == "runtime-image-payload" || $1 == "app-store") { next }
    { count[$3]++ } END { for (path in count) if (count[path] > 1) print path }')"
[ -z "$duplicates" ] || {
  printf 'rootfs copy destinations are no longer unique:\n%s\n' "$duplicates" >&2
  exit 1
}

instructions="$(containerfile_instructions "$containerfile" | awk -v stages=" $(containerfile_image_stages "$containerfile") " '
  $1 == "FROM" { stage=$NF; next }
  stage != "" && index(stages, " " stage " ") && $1 ~ /^(ARG|COPY|RUN|ADD|ENV|LABEL|WORKDIR|USER|EXPOSE|VOLUME|STOPSIGNAL|HEALTHCHECK|SHELL|CMD|ENTRYPOINT)$/ { n++ }
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
# The opt-in extension overlay adds one COPY of its scratch stage.
# The echo-cancel PipeWire config adds one COPY; folding the three
# /usr/lib/systemd/user units into one COPY removes two.
# A build-only resolver placeholder and a final COPY restore the runtime
# symlink outside the cleanup RUN, keeping BuildKit's DNS mount out of /run.
# The fixed-clock build adds an ARG, four COPYs, and an early path assertion.
# Pinning the unpacked base's directory times to the epoch adds one RUN.
# The single virtio Cage wrapper COPY brings the reviewed rootfs to 151.
# The ARM virtual graphics CPU shim adds one COPY, bringing this to 152.
# Redeclaring KERNEL_RPMS_REF before the kernel-swap RUN so Buildah hashes the
# kernel image identity into that layer's cache key adds one ARG (153).
# Redeclaring RPMS_REF before the first snapshot install, for the same reason,
# adds one more (154).
# NetworkManager's Wake-on-LAN drop-in adds one COPY (155).
# The offline predefined Flathub descriptor adds one reviewed COPY (156).
# The image is built as layer actions (os/image/layers.toml): the count is every
# stage of their pieces, the seal's included, 151 of them.
# The signed records of the runtimes and of the app store, each copied apart
# from the bytes they sign, add two COPYs (158).
# Declaring TILO_RUST_LTO in the rootfs piece, which tells the initramfs build whether the
# greeter's size budget is enforced or reported, adds one ARG (159).
[ "$instructions" -le 159 ] || {
  echo "rootfs layer budget exceeded: $instructions instructions (maximum 159)" >&2
  exit 1
}

# Bulk source-tree overlays are specifically forbidden: they can silently ship
# installer helpers, test fixtures, or future files merely added under os/usr.
if containerfile_instructions "$containerfile" |
   grep -Eq '^COPY( --[^ ]+)* (os/usr/?|os/usr/?) /'; then
  echo 'bulk COPY of os/usr is forbidden' >&2
  exit 1
fi

for overlay in desktop-binaries remote-capture-files rollback-files recovery-files extension-files; do
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
  && { echo 'COPY ownership override is not allowed' >&2; exit 1; } \
  || true

for path in \
  os/scripts/finalize-rpmdb.sh \
  os/usr/libexec/tilo-disk-encrypt \
  os/usr/libexec/tilo-store-volume \
  os/usr/libexec/tilo-verified-boot-install-preflight \
  os/usr/libexec/tilo-security-audit \
  os/usr/lib/NetworkManager/dispatcher.d/90-tilo-extension; do
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

# Component metadata must actually enter the assembly stage: a manifest-only
# dry run cannot prove that the build context supplies the installed bytes.
for path in \
  usr/lib/tilo/node-relay.toml \
  usr/lib/sysusers.d/tilo-relay.conf \
  usr/lib/systemd/system/tilo-node-relay@.service \
  usr/lib/systemd/system/tilo-node-session-key@.service \
  usr/lib/systemd/system/tilo-node-session@.service \
  usr/lib/systemd/system/tilo-node-session@.path; do
  containerfile_copy_contract "$containerfile" component-files |
    awk -F '\t' -v source="os/$path" -v destination="/tilo-components/$path" '
      $1 == "context" && $2 == source && $3 == destination { n++ }
      END { exit n != 1 }' || {
        echo "relay component input is not staged exactly once: $path" >&2
        exit 1
      }
done
python3 "$repo/os/scripts/install-components.py" relay --dry-run --format json |
  python3 -c 'import json,sys
rows = [row for row in json.load(sys.stdin) if (row["from"] or "").startswith("@manifest/")]
assert len(rows) == 6, rows
for row in rows:
    assert row["from"] == "@manifest" + row["to"], row
    assert row["mode"] == "0644" and not row["optional"], row'

echo "rootfs COPY rules hold; $instructions-instruction layer budget satisfied"

# The shell's startup companion is now installed by the canonical component
# installer rather than a direct COPY --chmod. Observe that actual image path
# and its mandatory executable-mode plan instead of accepting a staging COPY.
containerfile_installs_output "$containerfile" '@manifest/usr/libexec/tilo-session' /usr/libexec/tilo-session
python3 "$repo/os/scripts/install-components.py" shell --dry-run --format json |
  python3 -c 'import json,sys
rows = [row for row in json.load(sys.stdin) if row["to"] == "/usr/libexec/tilo-session"]
assert len(rows) == 1, "shell must install exactly one startup wrapper"
row = rows[0]
assert row["from"] == "@manifest/usr/libexec/tilo-session", row
assert row["mode"] == "0755" and not row["optional"], row'
