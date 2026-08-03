#!/usr/bin/env bash
# Assert the invariants an encrypted root depends on. Run against a BUILT image:
#
#   podman run --rm --entrypoint /bin/bash localhost/qos:latest \
#       /usr/libexec/qos/boot-chain-test.sh
#
# or from the source tree with OS_ROOT pointing at an unpacked rootfs. The
# checks exist because the image once shipped without an initramfs while also
# offering `qos-install --encrypt`, a combination in which the kernel reaches the
# LUKS device and then has no userspace to open it.
#
# Exit codes:
#   0 — every invariant holds.
#   1 — at least one failed; messages go to stderr.

set -uo pipefail

OS_ROOT="${OS_ROOT:-/}"
fail=0
ok()   { printf 'ok    %s\n' "$*"; }
bad()  { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

# 1. Exactly one kernel version. A second one means an orphan modules directory
#    survived a kernel swap, and ostree/bootc pick a version rather than fail.
mapfile -t kvers < <(ls -1 "$OS_ROOT/usr/lib/modules" 2>/dev/null)
if [ "${#kvers[@]}" -eq 1 ]; then
  ok "one kernel version: ${kvers[0]}"
else
  bad "expected exactly one /usr/lib/modules entry, found ${#kvers[@]}: ${kvers[*]:-none}"
fi

# 2. That version has a non-empty initramfs. This is the check that would have
#    caught the initrd-less image.
for kver in "${kvers[@]}"; do
  img="$OS_ROOT/usr/lib/modules/$kver/initramfs.img"
  if [ -s "$img" ]; then
    ok "initramfs present for $kver ($(stat -c %s "$img" 2>/dev/null) bytes)"
  else
    bad "no initramfs at $img — an encrypted root cannot be opened without one"
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
  [ -s "$img" ] || continue
  # Prefer the module list the build recorded from dracut's own output. Reading
  # the image back needs to descend into a squashfs, which needs privileges an
  # unprivileged image builder does not have — there, lsinitrd returns nothing
  # and an initramfs that does carry cryptsetup looks like one that does not.
  mods="$OS_ROOT/usr/lib/qos/initramfs-modules.txt"
  if [ -s "$mods" ]; then
    for want in crypt systemd-cryptsetup tpm2-tss; do
      if grep -qx "$want" "$mods"; then
        ok "initramfs includes the $want module"
      else
        bad "initramfs has no $want module — it cannot unlock an encrypted root"
      fi
    done
  elif command -v lsinitrd >/dev/null 2>&1 && [ -n "$(lsinitrd "$img" 2>/dev/null)" ]; then
    listing="$(lsinitrd "$img" 2>/dev/null)"
    for want in systemd-cryptsetup cryptsetup; do
      if printf '%s' "$listing" | grep -q "$want"; then
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
for f in /usr/libexec/qos-disk-encrypt /usr/libexec/qos-secureboot; do
  if [ -x "$OS_ROOT$f" ]; then ok "$f present"; else bad "$f missing or not executable"; fi
done

[ "$fail" -eq 0 ] && printf '\nall boot-chain invariants hold\n'
exit "$fail"
