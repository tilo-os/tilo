#!/usr/bin/env bash
# Static checks for the boundary between the privileged live installer and the
# system it writes. These catch regressions without requiring a destructive VM
# install; the end-to-end image test should assert the same properties at boot.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
backend="$repo_root/os/usr/libexec/qos-install"
encryptor="$repo_root/os/usr/libexec/qos-disk-encrypt"
guided_partitioner="$repo_root/os/usr/libexec/qos-guided-partition"
containerfile="$repo_root/os/Containerfile.installer"
production_containerfile="$repo_root/os/Containerfile"
builder="$repo_root/os/dev/build-custom-installer.sh"
firstboot_user="$repo_root/os/usr/libexec/qos-firstboot-user"
profile_extractor="$repo_root/os/usr/libexec/qos-installer-profile-extract"
preflight="$repo_root/os/usr/libexec/qos-verified-boot-install-preflight"
fail=0

ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

unlock_placeholder=qos.unlock=0000000000000000000000000000000000000000000000000000000000000000000000000000
sample_uuid=00000000-0000-0000-0000-000000000000
unlock_options="rd.luks.options=$sample_uuid=tpm2-device=auto,fido2-device=auto"
if [ "${#unlock_placeholder}" -eq "${#unlock_options}" ] \
   && [ "$(grep -Fc 'rd.luks.options=%s=tpm2-device=auto,fido2-device=auto' "$encryptor")" -eq 2 ]; then
  ok "sealed unlock slot exactly fits TPM2 and FIDO2 initramfs discovery options"
else
  bad "encrypted-root producer and sealed UKI hardware-token options disagree"
fi

karg_deletes="$(grep -c -- '--karg-delete qos.install' "$backend" || true)"
if [ "$karg_deletes" -eq 2 ] \
   && grep -Fq 'qos.root=0000000000000000000000000000000000000' "$backend" \
   && grep -Fq 'qos.luks=000000000000000000000000000000000000000000000' "$backend" \
   && grep -Fq 'qos.unlock=0000000000000000000000000000000000000000000000000000000000000000000000000000' "$backend" \
   && grep -Fq 'rd.luks.options=$LUKS_UUID=tpm2-device=auto,fido2-device=auto' "$backend" \
   && grep -Fq 'remove qos.install "$TARGET_UKI"' "$backend"; then
  ok "sealed encrypted installs specialize UUID and hardware-token slots, then remove the live marker"
else
  bad "sealed target UKI handoff is incomplete (legacy deletions: $karg_deletes)"
fi

if grep -Fq 'qos-uki-cmdline remove systemd.show_status' "$containerfile" \
   && grep -Fq 'qos-uki-cmdline remove rd.udev.log_level' "$containerfile" \
   && grep -Fq 'qos-uki-cmdline remove loglevel' "$containerfile" \
   && grep -Fq 'qos-uki-cmdline remove vt.global_cursor_default' "$containerfile" \
   && grep -Fq 'qos-uki-cmdline append qos.unlock=0000000000000000000000000000000000000000000000000000000000000000000000000000' "$containerfile" \
   && grep -Fq 'qos-uki-cmdline show | grep -qw qos.mode=normal' "$containerfile" \
   && grep -Fq 'qos-uki-cmdline show | grep -qw qos.install' "$containerfile"; then
  ok "installer reserves measured UKI cmdline space without losing splash or mode"
else
  bad "installer UKI cmdline headroom or required boot markers are incomplete"
fi

if grep -Fq '"$INSTALLER_CONTEXT/rootfs/usr/share/qos/greetd-config.toml"' "$builder" \
   && grep -Fq 'COPY rootfs/ /' "$containerfile" \
   && grep -Fq 'greetd-config.toml "$DEPLOY_ETC/greetd/config.toml"' "$backend" \
   && grep -Fq '[initial_session]' "$backend" \
   && grep -Fq 'user = "%s"' "$backend"; then
  ok "encrypted startup hands off directly to the installed user's session"
else
  bad "installed system would ask for a second login after disk authentication"
fi

if grep -Fq ': "${TMPDIR:=${QOS_INSTALLER_OUTPUT}/.tmp}"' "$builder" \
   && grep -Fq 'export TMPDIR' "$builder" \
   && grep -Fq 'INSTALLER_SCRATCH="$TMPDIR"' "$builder" \
   && grep -Fq 'mktemp -d "$INSTALLER_SCRATCH/qos-installer-context.XXXXXX"' "$builder" \
   && ! grep -Fq 'mktemp -d /tmp/qos-installer-context' "$builder"; then
  ok "large installer staging stays on the output filesystem"
else
  bad "installer staging can exhaust a small system temporary filesystem"
fi

if grep -Fq 'root_gib=$(((image_bytes * 3 + gib - 1) / gib))' "$builder" \
   && grep -Fq '[ "$root_gib" -ge 40 ] || root_gib=40' "$builder" \
   && grep -Fq -- '--root-min "${root_gib}G"' "$builder" \
   && grep -Fq 'QOS_INSTALLER_WORK_DISK=' "$repo_root/os/dev/_common.sh" \
   && grep -Fq "qemu-img create -f qcow2 -F qcow2 -b '\$disk' '\$work_disk'" "$repo_root/os/dev/vm.sh" \
   && grep -Fq -- '-drive file=$work_disk,format=qcow2,if=virtio' "$repo_root/os/dev/vm.sh"; then
  ok "live media is capacity-sized and VM cycles preserve the release artifact"
else
  bad "live-media capacity or pristine-artifact VM isolation regressed"
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
   && grep -Fq 'lsblk -srpno NAME,TYPE' "$backend" \
   && grep -Fq "could not resolve the live medium's physical disk" "$backend"; then
  ok "live-medium refusal compares stable block identities and fails closed"
else
  bad "live-medium disk guard still relies on path-string equality"
fi

if grep -Fq -- '--confirm-erase)' "$backend" \
   && grep -Fq '[ "$CONFIRM_ERASE" = "$DISK" ]' "$backend" \
   && grep -Fq 'whole-disk install requires --confirm-erase matching --disk' "$backend" \
   && grep -Fq -- '--confirm-erase cannot be used with --alongside' "$backend"; then
  ok "whole-disk erasure requires an exact device confirmation at the privileged boundary"
else
  bad "whole-disk erasure can proceed without an exact privileged confirmation"
fi

if grep -Fq -- '--confirm-root)' "$backend" \
   && grep -Fq '[ "$CONFIRM_ROOT" = "$ROOT_PARTITION" ]' "$backend" \
   && grep -Fq 'alongside install requires --confirm-root matching --root-partition' "$backend"; then
  ok "alongside formatting requires an exact root-partition confirmation at the privileged boundary"
else
  bad "alongside formatting can proceed without an exact privileged root confirmation"
fi

if grep -Fq -- '--expect-disk-size-bytes)' "$backend" \
   && grep -Fq -- '--expect-disk-devnum)' "$backend" \
   && grep -Fq -- '--expect-disk-wwn)' "$backend" \
   && grep -Fq -- '--expect-disk-serial)' "$backend" \
   && grep -Fq -- '--expect-root-partuuid)' "$backend" \
   && grep -Fq -- '--expect-root-size-bytes)' "$backend" \
   && grep -Fq -- '--expect-efi-partuuid)' "$backend" \
   && grep -Fq -- '--expect-efi-size-bytes)' "$backend" \
   && grep -Fq 'missing stable disk identity (expected WWN or serial)' "$backend" \
   && grep -Fq 'validate_partition_identity "$ROOT_PARTITION"' "$backend" \
   && grep -Fq 'validate_partition_identity "$EFI_PARTITION"' "$backend" \
   && [ "$(grep -c '^[[:space:]]*validate_disk_identity$' "$backend" || true)" -eq 2 ]; then
  ok "reviewed disk and alongside partition identities are revalidated before destructive work"
else
  bad "installer target identity is not bound across discovery and destructive execution"
fi

identity_error="$(
  id() { printf 0; }
  export -f id
  QOS_INSTALL_TRANSIENT=1 "$backend" \
    --disk /dev/qos-test-missing --confirm-erase /dev/qos-test-missing \
    --user identitytest --pin --yes </dev/null 2>&1 || true
)"
stable_error="$(
  id() { printf 0; }
  export -f id
  QOS_INSTALL_TRANSIENT=1 "$backend" \
    --disk /dev/qos-test-missing --confirm-erase /dev/qos-test-missing \
    --expect-disk-size-bytes 68719476736 --expect-disk-devnum 252:0 \
    --user identitytest --pin --yes </dev/null 2>&1 || true
)"
if grep -Fq 'missing or invalid --expect-disk-size-bytes' <<<"$identity_error" \
   && grep -Fq 'missing stable disk identity (expected WWN or serial)' <<<"$stable_error"; then
  ok "privileged installer rejects missing discovery identity before touching a block device"
else
  bad "privileged installer does not fail closed when discovery identity is omitted"
fi

if grep -Fq 'ENCRYPT=1' "$backend" \
   && grep -Fq 'native qOS requires --pin' "$backend" \
   && grep -Fq 'ENROL_ARGS+=(--passphrase)' "$backend" \
   && ! grep -Fq 'if verified_tpm; then' "$backend" \
   && ! grep -Fq 'qos-verified-boot-install-preflight' "$backend" \
   && ! grep -Fq 'startup PIN must be 6 to 12 digits' "$backend" \
   && grep -Fq 'disk passphrase must contain at least 12 characters' "$backend"; then
  ok "native qOS requires encryption and a strong passphrase on every machine"
else
  bad "native qOS installer can bypass encryption or the startup secret"
fi

if [ ! -e "$preflight" ] \
   && ! grep -Fq 'COPY os/usr/libexec/qos-verified-boot-install-preflight' "$production_containerfile"; then
  ok "unsupported PCR-7-only TPM PIN preflight is not shipped"
else
  bad "image still ships a TPM PIN preflight without measured qOS-artifact binding"
fi

disk_encrypt="$repo_root/os/usr/libexec/qos-disk-encrypt"
if grep -Fq 'TPM enrollment is disabled until qOS unlock is bound' "$disk_encrypt" \
   && grep -Fq 'enrolling the supplied strong passphrase as a LUKS key slot' "$disk_encrypt" \
   && ! grep -Fq -- '--tpm2-pcrs=' "$disk_encrypt" \
   && ! grep -Fq 'force_passphrase=' "$disk_encrypt"; then
  ok "disk helper has no dormant PCR-only TPM path and rejects resealing"
else
  bad "disk helper can create a PCR-7-only TPM slot"
fi

# Generation-bound TPM enrollment cannot be enabled as a one-line replacement
# for systemd-cryptenroll. The installer changes measured UKI bytes after the
# release image is built; the release carries no matching signed prediction, and
# there is not yet an early-boot consumer for a qos-rollback token. Until all
# three prerequisites arrive together, retain the honest PCR-7 token plus the
# independent recovery/FIDO paths and reject any partial enrollment wiring.
rollback_enrollment_calls="$(
  grep -hE 'qos-rollback[[:space:]]+enroll' "$backend" "$encryptor" 2>/dev/null \
    | grep -vE '^[[:space:]]*#' || true
)"
if [ -z "$rollback_enrollment_calls" ] \
   && grep -Fq 'install -d -m 0755 /usr/share/qos/pcr-predictions' "$production_containerfile" \
   && grep -Fq 'the directory ships empty' "$production_containerfile" \
   && ! grep -Fq -- '--tpm2-pcrs=' "$encryptor" \
   && grep -Fq -- '--recovery-key "$dev"' "$encryptor" \
   && grep -Fq -- '--fido2-device=auto "$dev"' "$encryptor"; then
  ok "PCR11 rollback enrollment stays fail-closed until target predictions and initramfs unlock exist"
else
  bad "installer partially enables generation enrollment without its target prediction/unlock prerequisites"
fi

if grep -Fq 'CONF_TMP="$(mktemp ' "$backend" \
   && grep -Fq 'find "$MNT/state/deploy"' "$backend" \
   && grep -Fq 'chmod 0600 "$CONF_TMP"' "$backend" \
   && grep -Fq 'mv -f "$CONF_TMP" "$CONF"' "$backend"; then
  ok "account password hash is created privately and published atomically"
else
  bad "account password hash is briefly exposed or written non-atomically"
fi

if grep -Fq 'systemctl mask sshd.service' "$containerfile" \
   && ! grep -Fq 'tester:quickdesktop:wheel' "$builder" \
   && grep -Fq -- '--dev-login requires a non-empty QOS_INSTALLER_DEV_PASSWORD' "$builder" \
   && grep -Fq 'append qos.install' "$containerfile" \
   && grep -Fq 'show | grep -qw qos.mode=normal' "$containerfile" \
   && grep -Fq 'show | grep -qw qos.install' "$containerfile"; then
  ok "release installer has no public debug credential and masks SSH"
else
  bad "release installer still exposes a known debug login or SSH"
fi

if grep -Fq 'StandardInput=file:$INPUT' "$backend" \
   && grep -Fq 'StandardOutput=append:$OUTPUT' "$backend" \
   && grep -Fq 'tail --pid="$RUNNER"' "$backend" \
   && grep -Fq 'mktemp -d /run/qos-installer/bridge/run.XXXXXX' "$backend" \
   && grep -Fq 'chcon --reference=/run/qos-installer/bridge "$BRIDGE" "$INPUT" "$OUTPUT"' "$backend" \
   && grep -Fq 'ExecStart=/usr/bin/install -d -m 0700 /run/qos-installer/bridge' "$repo_root/os/usr/lib/systemd/system/qos-installer-setup.service" \
   && ! grep -Fq -- '--pipe --collect' "$backend"; then
  ok "privileged handoff keeps secrets out of D-Bus fd passing and journald"
else
  bad "privileged handoff does not use the tested root-only file bridge"
fi

if grep -Fq "refusing to modify pre-existing account" "$firstboot_user" \
   && grep -Fq 'root|bin|daemon|' "$backend"; then
  ok "installer refuses reserved and pre-existing system accounts"
else
  bad "installer can reset a privileged or pre-existing account"
fi

if [ -x "$profile_extractor" ] \
   && grep -Fq 'QOS_INSTALL_PROFILE_V1' "$profile_extractor" \
   && grep -Fq 'sha256sum "$payload"' "$profile_extractor" \
   && grep -Fq 'ExecStart=/usr/libexec/qos-installer-profile-extract' \
      "$repo_root/os/usr/lib/systemd/system/qos-installer-setup.service"; then
  ok "live installer extracts and verifies the embedded setup profile before greetd"
else
  bad "host setup profile is not securely transported into the live installer"
fi

if grep -Fq "grep -q '^video:' /etc/group" "$repo_root/os/Containerfile" \
   && grep -Fq "grep -q '^input:' /etc/group" "$repo_root/os/Containerfile" \
   && ! grep -Fq 'getent group video' "$repo_root/os/Containerfile"; then
  ok "hardware-access groups are materialised instead of trusting synthesized NSS records"
else
  bad "hardware-access groups must be real /etc/group records for first-boot useradd"
fi

if grep -Fq -- '--full-name' "$backend" \
   && grep -Fq -- '--locale' "$backend" \
   && grep -Fq 'install-user-full-name' "$firstboot_user"; then
  ok "preconfigured identity and locale reach the installed account"
else
  bad "profile identity or locale is dropped before first boot"
fi

if grep -Fq '"$DEPLOY_ETC/systemd/system/sshd.service"' "$backend" \
   && grep -Fq 'multi-user.target.wants/sshd.service' "$backend"; then
  ok "live-only SSH state is removed from the installed target"
else
  bad "live-medium SSH mask/enablement can leak into the target"
fi

if grep -Fq 'chcon --reference=/etc/greetd/config.toml "$DEPLOY_ETC/greetd/config.toml"' "$backend"; then
  ok "installed greeter configuration receives an enforcing-compatible /etc label"
else
  bad "installed greeter configuration can retain a transient /run SELinux label"
fi

if grep -Fq 'ConditionKernelCommandLine=!qos.install' \
      "$repo_root/os/usr/lib/systemd/system/qos-rollback-harden.service" \
   && grep -Fq 'ConditionKernelCommandLine=!qos.install' \
      "$repo_root/os/usr/lib/systemd/system/shell-firstboot.service"; then
  ok "disposable live boots cannot consume first-boot state or mutate TPM lockout policy"
else
  bad "live installer can run installed-system first-boot or TPM-hardening services"
fi

if grep -Fq -- '--alongside' "$backend" \
   && grep -Fq 'signature_types="$(wipefs --noheadings --no-act --output TYPE "$ROOT_PARTITION")"' "$backend" \
   && [ "$(grep -c '^[[:space:]]*validate_alongside$' "$backend" || true)" -eq 3 ] \
   && grep -Fq 'signature_types="$(wipefs --noheadings --no-act --output TYPE "$part")"' "$encryptor" \
   && grep -Fq 'root and EFI partitions are not on the same disk' "$encryptor" \
   && grep -Fq 'MIN_ALONGSIDE_ESP_BYTES=2147483648' "$backend" \
   && grep -Fq 'MIN_ALONGSIDE_ESP_FREE_BYTES=1073741824' "$backend" \
   && grep -Fq 'MIN_ALONGSIDE_ESP_BYTES=2147483648' "$encryptor" \
   && grep -Fq 'MIN_ALONGSIDE_ESP_FREE_BYTES=1073741824' "$encryptor" \
   && grep -Fq 'fsck.fat -n "$EFI_PARTITION"' "$backend" \
   && grep -Fq 'fsck.fat -n "$esp"' "$encryptor" \
   && grep -Fq 'mount -o ro,nosuid,nodev,noexec "$EFI_PARTITION" "$efi_probe"' "$backend" \
   && grep -Fq 'mount -o ro,nosuid,nodev,noexec "$esp" "$esp_probe"' "$encryptor" \
   && grep -Fq 'existing OS partitions are preserved' "$backend" \
   && ! grep -Fq 'mkfs.vfat -n EFI-SYSTEM "$EFI_PARTITION"' "$backend"; then
  ok "alongside install fails closed on signed roots or undersized ESPs and never formats the shared ESP"
else
  bad "alongside install can overwrite an occupied root or shared ESP"
fi

if grep -Fq 'prepare-partition "$ROOT_PARTITION" --efi "$EFI_PARTITION"' "$backend" \
   && grep -Fq 'ROOT_FS_UUID=' "$backend" \
   && grep -Fq 'ENC_CLEANUP_TARGET="$ROOT_PARTITION"' "$backend"; then
  ok "encrypted and unencrypted alongside paths preserve the partition table"
else
  bad "alongside mode is not wired through both encryption paths"
fi

if [ -x "$guided_partitioner" ] \
   && grep -Fq -- '--guided-alongside)' "$backend" \
   && grep -Fq 'flock -x 9' "$guided_partitioner" \
   && grep -Fq 'partition map changed after review' "$guided_partitioner" \
   && grep -Fq 'sfdisk --append --no-reread --lock=no --wipe never --wipe-partitions never' "$guided_partitioner" \
   && grep -Fq 'mkfs.vfat -n QOS-EFI "$EFI_PARTITION"' "$guided_partitioner" \
   && ! grep -Eq 'sfdisk .*--delete|parted .* (move|resize)|resize2fs|ntfsresize' "$guided_partitioner"; then
  ok "guided alongside locks and revalidates the reviewed GPT map before appending only qOS partitions"
else
  bad "guided alongside can mutate storage outside its reviewed free extent"
fi

if bash "$repo_root/os/tests/live_installer_service_invariants.sh"; then
  ok "durable-identity services stay quiet only on disposable installer media"
else
  bad "a durable-identity service has an incorrect live-installer gate"
fi

exit "$fail"
