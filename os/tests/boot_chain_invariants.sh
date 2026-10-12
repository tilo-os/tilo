#!/usr/bin/env bash
# Assert the invariants an encrypted root depends on. Run against a BUILT image:
#
#   podman run --rm --entrypoint /bin/bash localhost/tilo:latest \
#       /usr/libexec/tilo/boot-chain-test.sh
#
# or from the source tree with OS_ROOT pointing at an unpacked rootfs, or with
# `--source` against this repository's os/ tree, which has the units and configs
# but no kernel and no enablement symlinks (checks needing those are skipped;
# the in-image run keeps them). The
# checks exist because the image once shipped without an initramfs while also
# offering `tilo-install --encrypt`, a combination in which the kernel reaches the
# LUKS device and then has no userspace to open it.
#
# Exit codes:
#   0 — every invariant holds.
#   1 — at least one failed; messages go to stderr.

set -uo pipefail

source_tree=0
if [ "${1:-}" = --source ]; then
  source_tree=1
  OS_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fi
OS_ROOT="${OS_ROOT:-/}"
fail=0
ok()   { printf 'ok    %s\n' "$*"; }
bad()  { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

# 1. Exactly one kernel version. A second one means an orphan modules directory
#    survived a kernel swap, and ostree/bootc pick a version rather than fail.
mapfile -t kvers < <(ls -1 "$OS_ROOT/usr/lib/modules" 2>/dev/null)
if [ "$source_tree" = 1 ]; then
  : # the kernel comes from the build
elif [ "${#kvers[@]}" -eq 1 ]; then
  ok "one kernel version: ${kvers[0]}"
else
  bad "expected exactly one /usr/lib/modules entry, found ${#kvers[@]}: ${kvers[*]:-none}"
fi

# 2. That version has a non-empty initramfs. This is the check that would have
#    caught the initrd-less image.
#
#    A sealed composefs image moves the kernel and initramfs out of
#    /usr/lib/modules and into a Unified Kernel Image on the ESP — so an absent
#    initramfs.img there is correct on that layout, and the invariant has to be
#    checked against the UKI instead. Requiring the bare file would fail exactly
#    the images that are built properly.
#    Two layouts: the image build writes /boot/EFI/Linux/<kver>.efi, and
#    `bootc install` renames it after the composefs digest and nests it one deeper
#    as /boot/EFI/Linux/bootc/bootc_composefs-<digest>.efi. An installed machine has
#    the second, so globbing only the first finds no UKI on exactly the system an
#    operator would re-run this on — and it would then report a missing initramfs.
uki=""
for candidate in "$OS_ROOT"/boot/EFI/Linux/*.efi "$OS_ROOT"/boot/EFI/Linux/*/*.efi; do
  [ -f "$candidate" ] && uki="$candidate" && break
done

for kver in "${kvers[@]}"; do
  img="$OS_ROOT/usr/lib/modules/$kver/initramfs.img"
  if [ -s "$img" ]; then
    ok "initramfs present for $kver ($(stat -c %s "$img" 2>/dev/null) bytes)"
  elif [ -n "$uki" ]; then
    # The .initrd section is the initramfs on this layout. objcopy is not in
    # every image, so read the PE section table directly.
    isize="$(python3 - "$uki" <<'EOPY' 2>/dev/null
import struct, sys
d = open(sys.argv[1], "rb").read()
pe = struct.unpack_from("<I", d, 0x3C)[0]
nsec = struct.unpack_from("<H", d, pe + 6)[0]
base = pe + 24 + struct.unpack_from("<H", d, pe + 20)[0]
for i in range(nsec):
    o = base + i * 40
    if d[o:o+8].rstrip(b"\0") == b".initrd":
        print(struct.unpack_from("<I", d, o + 8)[0])
        break
EOPY
)"
    if [ -n "$isize" ] && [ "$isize" -gt 0 ] 2>/dev/null; then
      ok "initramfs is in the UKI for $kver ($uki, .initrd is $isize bytes)"
    else
      bad "$uki has no .initrd section — an encrypted root cannot be opened without one"
    fi
  else
    bad "no initramfs at $img and no UKI under /boot/EFI/Linux — an encrypted root cannot be opened without one"
  fi
done

# 3. Nothing disables initrd generation for future kernel installs, and the
#    ostree layout is not overridden. /etc/kernel/install.conf wins over the
#    base's /usr/lib/kernel/install.conf, so a file here is how the invariant
#    above gets silently undone on the next kernel bump.
conf="$OS_ROOT/etc/kernel/install.conf"
if [ -f "$conf" ]; then
  conf_ok=1
  if grep -qE '^[[:space:]]*initrd_generator[[:space:]]*=[[:space:]]*none' "$conf"; then
    bad "$conf sets initrd_generator=none"; conf_ok=0
  fi
  if grep -qE '^[[:space:]]*layout[[:space:]]*=[[:space:]]*bls' "$conf"; then
    bad "$conf overrides the base's layout=ostree with layout=bls"; conf_ok=0
  fi
  [ "$conf_ok" -eq 1 ] && ok "$conf present and disables nothing"
else
  ok "no /etc/kernel/install.conf override (base layout=ostree applies)"
fi

# 4. The initramfs must be able to open a LUKS volume: dracut has to have pulled
#    in systemd-cryptsetup and the TPM2 stack. Read the image rather than trust
#    the dracut config, since a config drop-in can be added and still not apply.
for kver in "${kvers[@]}"; do
  img="$OS_ROOT/usr/lib/modules/$kver/initramfs.img"
  # Do NOT skip when the bare initramfs is absent: on a sealed composefs image
  # it always is, and skipping would drop the one invariant that says an
  # encrypted root can be opened. The recorded module list is in the rootfs
  # either way, so it answers the question on both layouts.
  if [ ! -s "$img" ] && [ -z "$uki" ]; then
    bad "no initramfs and no UKI for $kver — cannot check the unlock modules"
    continue
  fi
  # Prefer the module list the build recorded from dracut's own output. Reading
  # the image back needs to descend into a squashfs, which needs privileges an
  # unprivileged image builder does not have — there, lsinitrd returns nothing
  # and an initramfs that does carry cryptsetup looks like one that does not.
  mods="$OS_ROOT/usr/lib/tilo/initramfs-modules.txt"
  if [ -s "$mods" ]; then
    for want in crypt systemd-cryptsetup tpm2-tss plymouth tilo-rollback tilo-sibling-unlock; do
      if grep -qx "$want" "$mods"; then
        ok "initramfs includes the $want module"
      else
        bad "initramfs has no $want module — it cannot unlock an encrypted root"
      fi
    done
    for unwanted in network network-legacy network-manager nfs iscsi nvmf nbd fcoe fcoe-uefi nbde clevis clevis-pin-null clevis-pin-sss clevis-pin-tang clevis-pin-tpm2 crypt-ssh; do
      if grep -qx "$unwanted" "$mods"; then
        bad "initramfs carries unsupported network-root module $unwanted"
      fi
    done
    omitted_drivers="$OS_ROOT/usr/lib/tilo/initramfs-omitted-drivers.txt"
    if [ -s "$omitted_drivers" ]; then
      if grep -qx ext4 "$omitted_drivers"; then
        bad "initramfs excludes Ext4, so a supported Ext4 root cannot boot"
      fi
      for unwanted_driver in xfs; do
        if grep -qx "$unwanted_driver" "$omitted_drivers"; then
          ok "initramfs excludes the $unwanted_driver kernel driver"
        else
          bad "initramfs omission policy does not exclude $unwanted_driver"
        fi
      done
    else
      bad "initramfs kernel-driver omission policy is missing"
    fi
  elif [ -s "$img" ] && command -v lsinitrd >/dev/null 2>&1 \
       && [ -n "$(lsinitrd "$img" 2>/dev/null)" ]; then
    listing="$(lsinitrd "$img" 2>/dev/null)"
    for want in systemd-cryptsetup cryptsetup tilo-rollback-initrd-unlock; do
      if grep -q "$want" <<<"$listing"; then
        ok "initramfs carries $want"
      else
        bad "initramfs has no $want — it cannot unlock an encrypted root"
      fi
    done
  else
    bad "cannot tell what is in the initramfs: no $mods, and lsinitrd read nothing"
  fi
done

# 5. The encryption backend must exist wherever the installer can run.
for f in /usr/libexec/tilo-disk-encrypt /usr/libexec/tilo-secureboot; do
  if [ -x "$OS_ROOT$f" ]; then ok "$f present"; else bad "$f missing or not executable"; fi
done

# 6. Exactly one unit grows the root filesystem on the composefs layout.
#    `GrowFileSystem=yes` gives the root partition the GPT attribute that makes
#    systemd-gpt-auto-generator start systemd-growfs-root.service. In Tilo `/`
#    is a composefs overlay, while the block-backed btrfs is mounted at
#    `/sysroot`; masking the stock unit without a replacement leaves the
#    installation's extra space unused.
growfs_unit="$OS_ROOT/usr/lib/systemd/system/tilo-growfs-sysroot.service"
growfs_timer="$OS_ROOT/usr/lib/systemd/system/tilo-growfs-sysroot.timer"
growfs_dropin="$OS_ROOT/usr/lib/systemd/system/systemd-growfs-root.service.d/10-tilo-composefs.conf"
if [ ! -f "$growfs_unit" ]; then
  bad "tilo-growfs-sysroot.service missing — nothing grows the root filesystem"
else
  ours="$(grep -E '^ConditionPathIsMountPoint=' "$growfs_unit" | head -1)"
  exec_line="$(grep -E '^ExecStart=' "$growfs_unit" | head -1)"
  [ "$ours" = "ConditionPathIsMountPoint=/sysroot" ] \
    && ok "tilo growfs is gated on /sysroot" \
    || bad "tilo growfs does not require /sysroot (${ours:-none})"
  [ "$exec_line" = "ExecStart=/usr/bin/sh -c 'mount -o remount,rw /sysroot && exec /usr/lib/systemd/systemd-growfs /sysroot'" ] \
    && ok "tilo growfs targets the block-backed /sysroot" \
    || bad "tilo growfs targets the wrong path (${exec_line:-none})"
fi

if [ ! -f "$growfs_timer" ]; then
  bad "tilo-growfs-sysroot.timer missing — root growth cannot run off the login critical path"
else
  timer_unit="$(grep -E '^Unit=' "$growfs_timer" | head -1)"
  timer_delay="$(grep -E '^OnBootSec=' "$growfs_timer" | head -1)"
  [ "$timer_unit" = "Unit=tilo-growfs-sysroot.service" ] \
    && ok "tilo growfs timer starts the sysroot grow service" \
    || bad "tilo growfs timer starts the wrong unit (${timer_unit:-none})"
  [ "$timer_delay" = "OnBootSec=5s" ] \
    && ok "tilo growfs timer waits for the login I/O burst" \
    || bad "tilo growfs timer has an unexpected delay (${timer_delay:-none})"
fi

if [ ! -f "$growfs_dropin" ]; then
  bad "systemd-growfs-root.service drop-in missing — the stock unit can target the composefs overlay"
else
  theirs="$(grep -E '^ConditionPathIsMountPoint=' "$growfs_dropin" | head -1)"
  [ "$theirs" = "ConditionPathIsMountPoint=!/sysroot" ] \
    && ok "stock growfs is gated away when /sysroot is mounted" \
    || bad "stock growfs condition is not the opposite of the replacement (${theirs:-none})"
fi

if [ -L "$OS_ROOT/etc/systemd/system/systemd-growfs-root.service" ] \
   && [ "$(readlink "$OS_ROOT/etc/systemd/system/systemd-growfs-root.service")" = /dev/null ]; then
  bad "systemd-growfs-root.service is masked, so its composefs drop-in cannot select the layout"
else
  ok "systemd-growfs-root.service is not masked"
fi

# Ask systemctl rather than guessing which wants directory contains the link.
# The path fallback keeps this invariant useful in a minimal unpacked root.
growfs_enabled=""
if [ "$source_tree" = 1 ]; then
  # Enabled by the build (os/Containerfile); os/tests/boot_performance_gate_test.sh
  # checks that it does.
  growfs_enabled=yes
elif command -v systemctl >/dev/null 2>&1; then
  case "$(systemctl --root="$OS_ROOT" is-enabled tilo-growfs-sysroot.timer 2>/dev/null)" in
    enabled|enabled-runtime|static) growfs_enabled=yes ;;
    *) growfs_enabled=no ;;
  esac
elif [ -e "$OS_ROOT/etc/systemd/system/timers.target.wants/tilo-growfs-sysroot.timer" ]; then
  growfs_enabled=yes
else
  growfs_enabled=no
fi
if [ "$growfs_enabled" = yes ]; then
  ok "tilo-growfs-sysroot.timer is enabled"
else
  bad "tilo-growfs-sysroot.timer is not enabled — the filesystem is never grown"
fi

# The kernel has no NFS (os/kernel/config-fragments/tilo.config), so the NFS
# userspace could only fail at every boot. A later package transaction pulling it
# back in (libvirt recommends it) must fail the build, not the boot.
if [ "$source_tree" = 1 ]; then
  :
elif command -v rpm >/dev/null 2>&1 \
     && nfs="$(rpm --root "$OS_ROOT" -qa --qf '%{NAME}\n' 2>/dev/null | grep -Ex 'nfs-utils|nfs(v[34])?-(client|common)-utils|rpcbind' || true)" \
     && [ -n "$nfs" ]; then
  bad "the image ships NFS userspace the kernel cannot serve: $(echo $nfs)"
else
  ok "no NFS userspace"
fi

[ "$fail" -eq 0 ] && printf '\nall boot-chain invariants hold\n'
exit "$fail"
