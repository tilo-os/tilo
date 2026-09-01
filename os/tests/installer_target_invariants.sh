#!/usr/bin/env bash
# Static checks for the boundary between the privileged live installer and the
# system it writes. These catch regressions without requiring a destructive VM
# install; the end-to-end image test should assert the same properties at boot.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
backend="$repo_root/os/usr/libexec/qos-install"
encryptor="$repo_root/os/usr/libexec/qos-disk-encrypt"
containerfile="$repo_root/os/Containerfile.installer"
builder="$repo_root/os/dev/build-custom-installer.sh"
firstboot_user="$repo_root/os/usr/libexec/qos-firstboot-user"
fail=0

ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

karg_deletes="$(grep -c -- '--karg-delete qos.installer=1' "$backend" || true)"
if [ "$karg_deletes" -eq 3 ]; then
  ok "encrypted, erase, and alongside installs remove qos.installer=1"
else
  bad "expected three target karg deletions, found $karg_deletes"
fi

if grep -Fq 'COPY etc/greetd/config.toml /usr/share/qos/greetd-config.toml' "$containerfile" \
   && grep -Fq 'greetd-config.toml "$DEPLOY_ETC/greetd/config.toml"' "$backend"; then
  ok "normal greetd configuration is restored in the target"
else
  bad "normal greetd configuration is not bundled and restored"
fi

if grep -Fq 'rm -f "$DEPLOY_ETC/sudoers.d/qos-installer"' "$backend"; then
  ok "live-installer sudo privilege is removed from the target"
else
  bad "target does not remove the live-installer sudo privilege"
fi

if grep -Fq -- '-uid 0 -links 1 ! -perm /077 -print0' "$backend" \
   && grep -Fq 'install -m 0600 "$PROFILE"' "$backend" \
   && grep -Fq "sed -i '/^[[:space:]]*permissions=/d'" "$backend"; then
  ok "safe live Wi-Fi profiles become private machine connections on first boot"
else
  bad "installer Wi-Fi profiles are not safely preserved for first boot"
fi

if grep -Fq 'code.sandbox.example/qos/qos:*|code.sandbox.example/qos/qos@sha256:*' "$backend"; then
  ok "installed update origin is restricted to the signature-required repository"
else
  bad "installer can record an update origin outside the signature-required policy"
fi

if grep -Fq 'TARGET_MAJ_MIN=' "$backend" \
   && grep -Fq 'LIVE_MAJ_MIN=' "$backend" \
   && grep -Fq 'lsblk -spno NAME,TYPE' "$backend" \
   && grep -Fq "could not resolve the live medium's physical disk" "$backend"; then
  ok "live-medium refusal compares stable block identities and fails closed"
else
  bad "live-medium disk guard still relies on path-string equality"
fi

if grep -Fq 'CONF_TMP="$(mktemp ' "$backend" \
   && grep -Fq 'chmod 0600 "$CONF_TMP"' "$backend" \
   && grep -Fq 'mv -f "$CONF_TMP" "$CONF"' "$backend"; then
  ok "account password hash is created privately and published atomically"
else
  bad "account password hash is briefly exposed or written non-atomically"
fi

if grep -Fq 'systemctl mask sshd.service' "$containerfile" \
   && ! grep -Fq 'tester:quickdesktop:wheel' "$builder" \
   && grep -Fq -- '--dev-login requires a non-empty QOS_INSTALLER_DEV_PASSWORD' "$builder"; then
  ok "release installer has no public debug credential and masks SSH"
else
  bad "release installer still exposes a known debug login or SSH"
fi

if grep -Fq "refusing to modify pre-existing account" "$firstboot_user" \
   && grep -Fq 'root|bin|daemon|' "$backend"; then
  ok "installer refuses reserved and pre-existing system accounts"
else
  bad "installer can reset a privileged or pre-existing account"
fi

if grep -Fq '"$DEPLOY_ETC/systemd/system/sshd.service"' "$backend" \
   && grep -Fq 'multi-user.target.wants/sshd.service' "$backend"; then
  ok "live-only SSH state is removed from the installed target"
else
  bad "live-medium SSH mask/enablement can leak into the target"
fi

if grep -Fq -- '--alongside' "$backend" \
   && grep -Fq 'signature_types="$(wipefs --noheadings --no-act --output TYPE "$ROOT_PARTITION")"' "$backend" \
   && [ "$(grep -c '^[[:space:]]*validate_alongside$' "$backend" || true)" -eq 3 ] \
   && grep -Fq 'signature_types="$(wipefs --noheadings --no-act --output TYPE "$part")"' "$encryptor" \
   && grep -Fq 'root and EFI partitions are not on the same disk' "$encryptor" \
   && grep -Fq 'existing OS partitions are preserved' "$backend" \
   && ! grep -Fq 'mkfs.vfat -n EFI-SYSTEM "$EFI_PARTITION"' "$backend"; then
  ok "alongside install fails closed on signed roots and never formats the shared ESP"
else
  bad "alongside install can overwrite an occupied root or shared ESP"
fi

if grep -Fq 'prepare-partition "$ROOT_PARTITION" --efi "$EFI_PARTITION"' "$backend" \
   && grep -Fq 'ROOT_FS_UUID=' "$backend"; then
  ok "encrypted and unencrypted alongside paths preserve the partition table"
else
  bad "alongside mode is not wired through both encryption paths"
fi

exit "$fail"
