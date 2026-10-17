#!/usr/bin/env bash
# Static checks for the boundary between the privileged live installer and the
# system it writes. These catch regressions without requiring a destructive VM
# install; the end-to-end image test should assert the same properties at boot.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
backend="$repo_root/os/usr/libexec/tilo-install"
encryptor="$repo_root/os/usr/libexec/tilo-disk-encrypt"
guided_partitioner="$repo_root/os/usr/libexec/tilo-guided-partition"
containerfile="$repo_root/os/Containerfile.installer"
production_containerfile="$repo_root/os/Containerfile"
builder="$repo_root/os/dev/build-custom-installer.sh"
firstboot_user="$repo_root/os/usr/libexec/tilo-firstboot-user"
autologin_cleanup="$repo_root/os/usr/libexec/tilo-firstboot-autologin-cleanup"
autologin_cleanup_unit="$repo_root/os/usr/lib/systemd/system/tilo-firstboot-autologin-cleanup.service"
profile_extractor="$repo_root/os/usr/libexec/tilo-installer-profile-extract"
installer_app="$repo_root/crates/session/installer/src/installer_app.rs"
installer_disks="$repo_root/crates/session/installer/src/disks.rs"
block_devices_lsblk="$repo_root/crates/system/block-devices/src/lsblk.rs"
installer_timezones="$repo_root/crates/session/installer/src/timezones.rs"
media_profile="$repo_root/crates/session/installer/src/media_profile.rs"
preflight="$repo_root/os/usr/libexec/tilo-verified-boot-install-preflight"
secret_policy="$repo_root/os/usr/libexec/tilo-startup-secret-policy"
prediction_signer="$repo_root/os/dev/sign-target-prediction.sh"
# shellcheck source=lib/containerfile.sh
. "$repo_root/os/tests/lib/containerfile.sh"
os_image_workflow="$repo_root/.forgejo/workflows/build-image.yml"
fail=0

ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

if grep -Fq 'use block_devices::' "$installer_disks" \
   && grep -Fq 'bounded_command::run(' "$block_devices_lsblk" \
   && grep -Fq 'bounded_command::run(' "$installer_timezones" \
   && grep -Fq 'bounded_command::run(' "$installer_app" \
   && ! grep -Eq 'Command::new\("(lsblk|timedatectl|/usr/libexec/tilo-verified-boot-install-preflight)"\)' \
      "$installer_disks" "$block_devices_lsblk" "$installer_timezones" "$installer_app"; then
  ok "installer discovery and verified-boot preflight have bounded subprocesses"
else
  bad "installer trusts an unbounded discovery or verified-boot subprocess"
fi

if grep -Fq 'enum FieldCompletion {' "$installer_app" \
   && grep -Fq 'profile_completion: Option<ProfileCompletion>' "$installer_app" \
   && grep -Fq 'pub repairs: Vec<MediaProfileField>' "$media_profile" \
   && grep -Fq 'repair_summary' "$installer_app" \
   && ! grep -Fq 'preconfigured: bool' "$installer_app"; then
  ok "installer tracks transported setup fields independently and preserves repair routing"
else
  bad "installer profile routing regressed to an all-or-nothing shortcut"
fi

if grep -Fq 'root=tilo-auto' "$repo_root/os/usr/lib/bootc/kargs.d/00-root.toml" \
   && grep -Fq 'rd.luks.options=tpm2-device=auto,fido2-device=auto' \
      "$repo_root/os/usr/lib/bootc/kargs.d/00-root.toml"; then
  ok "live and target UKIs inherit target-independent root and unlock discovery"
else
  bad "sealed UKIs lack target-independent root or hardware-token discovery"
fi

karg_deletes="$(grep -c -- '--karg-delete tilo.install' "$backend" || true)"
if [ "$karg_deletes" -eq 2 ] \
   && grep -Fq 'append systemd.mount-extra=LABEL=TILO_BOOT:/boot:ext4:ro,nosuid,nodev,noexec /boot/tilo-target/tilo-target.efi' "$containerfile" \
   && grep -Fq 'remove tilo.install /boot/tilo-target/tilo-target.efi' "$containerfile" \
   && grep -Fq 'install -m 0644 "$TARGET_VARIANT" "$TARGET_UKI.new"' "$backend" \
   && grep -Fq 'mv -f "$TARGET_UKI.new" "$TARGET_UKI"' "$backend" \
   && grep -Fq 'target UKI lacks the signed TILO_BOOT mount contract' "$backend" \
   && ! grep -Fq 'tilo-uki-cmdline set "tilo.root=' "$backend"; then
  ok "encrypted installs atomically promote a target-independent UKI without per-machine measured bytes"
else
  bad "target-independent UKI handoff is incomplete (legacy deletions: $karg_deletes)"
fi

if grep -Fq 'mkfs.ext4 -q -L TILO_BOOT "$boot_part"' "$encryptor" \
   && grep -Fq -- '--boot-mount-spec "$ENC_BOOT_MOUNT_SPEC"' "$backend" \
   && grep -Fq 'tilo-boot-mount-contract' "$backend" \
   && bash "$repo_root/os/tests/encrypted_boot_mount_contract.sh"; then
  ok "encrypted installs bind the signed TILO_BOOT mount contract to one prepared partition"
else
  bad "encrypted /boot mount contract can be missing, substituted, or ambiguous"
fi

if grep -Fq 'tilo-uki-cmdline remove systemd.show_status' "$containerfile" \
   && grep -Fq 'tilo-uki-cmdline remove rd.udev.log_level' "$containerfile" \
   && grep -Fq 'tilo-uki-cmdline remove loglevel' "$containerfile" \
   && grep -Fq 'tilo-uki-cmdline remove vt.global_cursor_default' "$containerfile" \
   && grep -Fq 'tilo-uki-cmdline show | grep -qw tilo.mode=normal' "$containerfile" \
   && grep -Fq 'tilo-uki-cmdline show | grep -qw tilo.install' "$containerfile" \
   && grep -Fq 'tilo-uki-cmdline show | grep -qw root=tilo-auto' "$containerfile" \
   && ! grep -Eq 'tilo\.(root|luks|unlock)=' "$containerfile"; then
  ok "installer live UKI keeps the bootable root contract without inert placeholders"
else
  bad "installer live UKI root contract or required boot markers are incomplete"
fi

if grep -Fq 'set -- /boot/EFI/Linux/*.efi' "$containerfile" \
   && grep -Fq '[ "$#" -eq 1 ] && [ -f "$1" ]' "$containerfile" \
   && ! grep -Fq 'find /boot/EFI/Linux -type f -name '\''*.efi'\'' -print -quit' "$containerfile"; then
  ok "installer copies the sole live UKI only after rejecting ambiguous boot trees"
else
  bad "installer target UKI selection is traversal-order dependent"
fi

# The signer counts every live UKI, nested bootc directories included, and
# takes the fallback only when exactly one exists (test_target_uki_projection.py
# executes that guard).
if grep -Fq '[ "$n" -eq 1 ] || { echo "explicit target UKI required when multiple live UKIs exist"' "$prediction_signer" \
   && grep -Fq 'done < <(find /boot/EFI/Linux -type f -name "*.efi" -print | sort)' "$prediction_signer"; then
  ok "PCR prediction signing rejects an ambiguous live UKI fallback"
else
  bad "PCR prediction signing can bind the wrong live UKI"
fi

if grep -Fq '"$INSTALLER_CONTEXT/rootfs/usr/share/tilo/greetd-config.toml"' "$builder" \
   && grep -Fq 'COPY rootfs/ /' "$containerfile" \
   && grep -Fq 'install -m 0644 /usr/share/tilo/greetd-config.toml "$GREETD_CONFIG_TMP"' "$backend" \
   && grep -Fq '[initial_session]' "$backend" \
   && grep -Fq 'command = "/usr/libexec/tilo-session --backend drm"' "$backend" \
   && grep -Fq 'firstboot-autologin-pending' "$backend" \
   && grep -Fq 'sync -f "$AUTOLOGIN_PENDING"' "$backend" \
   && grep -Fq 'tilo-firstboot-autologin-cleanup.service' "$containerfile" \
   && grep -Fqx 'After=greetd.service' "$autologin_cleanup_unit" \
   && grep -Fqx 'ConditionPathExists=/etc/tilo/firstboot-autologin-pending' "$autologin_cleanup_unit" \
   && grep -Fq 'greetd-config.toml' "$autologin_cleanup" \
   && grep -Fq 'rm -f -- "$PENDING"' "$autologin_cleanup"; then
  ok "first installed boot reaches OOBE directly, then restores password authentication"
else
  bad "first-boot graphical handoff is missing, unsafe, or can persist past one boot"
fi

if bash "$repo_root/os/tests/firstboot_autologin_cleanup.sh"; then
  ok "autologin cleanup atomically restores the greeter and fails closed on an unsafe marker"
else
  bad "autologin cleanup cannot recover a normal greeter after first boot"
fi

if grep -Fq ': "${TMPDIR:=${TILO_INSTALLER_OUTPUT}/.tmp}"' "$builder" \
   && grep -Fq 'export TMPDIR' "$builder" \
   && grep -Fq 'INSTALLER_SCRATCH="$TMPDIR"' "$builder" \
   && grep -Fq 'mktemp -d "$INSTALLER_SCRATCH/tilo-installer-context.XXXXXX"' "$builder" \
   && ! grep -Fq 'mktemp -d /tmp/tilo-installer-context' "$builder"; then
  ok "large installer staging stays on the output filesystem"
else
  bad "installer staging can exhaust a small system temporary filesystem"
fi

if grep -Fq 'root_gib=$(((image_bytes * 3 + gib - 1) / gib))' "$builder" \
   && grep -Fq '[ "$root_gib" -ge 40 ] || root_gib=40' "$builder" \
   && grep -Fq -- '--root-min "${root_gib}G"' "$builder" \
   && grep -Fq 'TILO_INSTALLER_WORK_DISK=' "$repo_root/os/dev/_common.sh" \
   && grep -Fq "qemu-img create -f qcow2 -F qcow2 -b '\$disk' '\$work_disk'" "$repo_root/os/dev/vm.sh" \
   && grep -Fq '$(virtio_disk "$work_disk" tilo-installer-medium tilo-installer-medium)' "$repo_root/os/dev/vm.sh"; then
  ok "live media is capacity-sized and VM cycles preserve the release artifact"
else
  bad "live-media capacity or pristine-artifact VM isolation regressed"
fi

if grep -Fq 'rm -f "$DEPLOY_ETC/sudoers.d/tilo-installer"' "$backend"; then
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

if python3 "$repo_root/os/tests/test_public_trust_install.py" >/dev/null 2>&1; then
  ok "installed update origin requires immutable binding and an exact mandatory-signature policy"
else
  bad "installer can record an update origin outside the signature-required policy"
fi

if grep -Fq 'TILO_TARGET_IMGREF="$(/usr/libexec/tilo-board-successor' "$backend" \
   && grep -Fq -- 'install-update-origin "${TILO_TARGET_IMGREF:-}")" || exit 1' "$backend" \
   && grep -Fq '{ from = "tilo-board-successor", to = "/usr/libexec/tilo-board-successor", mode = "0755" }' \
      "$repo_root/os/components.toml" \
   && grep -Fq -- '--target-update-ref "$TILO_TARGET_IMGREF"' "$production_containerfile"; then
  ok "installer resolves the image-owned hardware update origin and stops on conflicts"
else
  bad "hardware-profile update-origin selection or runtime helper is missing"
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
   && grep -Fq -- '--expect-boot-partuuid)' "$backend" \
   && grep -Fq -- '--expect-boot-size-bytes)' "$backend" \
   && grep -Fq -- '--expect-efi-partuuid)' "$backend" \
   && grep -Fq -- '--expect-efi-size-bytes)' "$backend" \
   && grep -Fq 'missing stable disk identity (expected WWN or serial)' "$backend" \
   && grep -Fq 'validate_partition_identity "$ROOT_PARTITION"' "$backend" \
   && grep -Fq 'validate_partition_identity "$BOOT_PARTITION"' "$backend" \
   && grep -Fq 'validate_partition_identity "$EFI_PARTITION"' "$backend" \
   && [ "$(grep -c '^[[:space:]]*validate_disk_identity$' "$backend" || true)" -ge 3 ]; then
  ok "reviewed disk and alongside partition identities are revalidated before destructive work"
else
  bad "installer target identity is not bound across discovery and destructive execution"
fi

# The backend resolves its update origin with the compiled hardware-profile
# reader; the fixture runs this checkout's build of it.
cargo build --quiet --manifest-path "$repo_root/Cargo.toml" -p tilo-board-successor \
  || bad "could not build tilo-board-successor"
board_successor="${CARGO_TARGET_DIR:-$repo_root/target}/debug/tilo-board-successor"

installer_identity_fixture() (
  id() { printf 0; }
  local generation
  generation="$(mktemp -d)"
  trap 'rm -rf -- "$generation"' EXIT
  export generation
  export -f id
  # Hardware selection still executes from this checkout. Generation selection
  # is outside this disk-identity fixture; native transport has its own tests.
  python3 - "$backend" "$generation/backend" "$board_successor" <<'PY'
from pathlib import Path
import sys
source = Path(sys.argv[1]).read_text()
selection = '$(/usr/libexec/tilo-installer-prepared-setup-extract selected-generation)'
resolver = '$(/usr/libexec/tilo-board-successor \\\n  install-update-origin'
assert source.count(selection) == 1 and source.count(resolver) == 1
source = source.replace(selection, '$generation')
Path(sys.argv[2]).write_text(source.replace(resolver, resolver.replace('/usr/libexec/tilo-board-successor', sys.argv[3])))
PY
  TILO_INSTALL_TRANSIENT=1 bash "$generation/backend" "$@"
)

identity_error="$(
  installer_identity_fixture \
    --disk /dev/tilo-test-missing --confirm-erase /dev/tilo-test-missing \
    --user identitytest --pin --yes </dev/null 2>&1 || true
)"
stable_error="$(
  installer_identity_fixture \
    --disk /dev/tilo-test-missing --confirm-erase /dev/tilo-test-missing \
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
   && grep -Fq 'native Tilo requires --pin' "$backend" \
   && grep -Fq 'ENROL_ARGS+=(--passphrase)' "$backend" \
   && grep -Fq 'tilo-verified-boot-install-preflight' "$backend" \
   && grep -Fq 'POLICY_MODE=(--tpm-pin)' "$backend" \
   && grep -Fq 'ENROL_ARGS+=(--require-tpm-prediction)' "$backend" \
   && grep -Fq 'tilo-startup-secret-policy' "$backend"; then
  ok "native Tilo requires encryption and permits a short PIN only after exact verified-boot preflight"
else
  bad "native Tilo installer can bypass encryption or the startup secret"
fi

if [ -x "$secret_policy" ] \
   && printf '%s\n' 'correct horse battery' | "$secret_policy" \
   && printf '%s\n' '12345678901234567890' | "$secret_policy" \
   && ! printf '%s\n' 'aaaaaaaaaaaa' | "$secret_policy" 2>/dev/null \
   && ! printf '%s\n' 'password1234' | "$secret_policy" 2>/dev/null \
   && ! printf '%s\n' 'abcabcabcabc' | "$secret_policy" 2>/dev/null \
   && ! printf '%s\n' '123456789012' | "$secret_policy" 2>/dev/null \
   && ! printf '%s\n' 'correct horse' | "$secret_policy" 2>/dev/null \
   && ! printf '%s\n' ' correct horse battery' | "$secret_policy" 2>/dev/null \
   && printf '%s\n' '804927' | "$secret_policy" --tpm-pin \
   && ! printf '%s\n' '123456' | "$secret_policy" --tpm-pin 2>/dev/null \
   && ! printf '%s\n' '111111' | "$secret_policy" --tpm-pin 2>/dev/null; then
  ok "privileged startup-secret policy separates strong passphrases from nontrivial TPM PINs"
else
  bad "privileged startup-secret policy accepts a cheaply guessed disk secret"
fi

if [ -x "$preflight" ] \
   && containerfile_has_copy "$production_containerfile" \
        os/usr/libexec/tilo-verified-boot-install-preflight \
        /usr/libexec/tilo-verified-boot-install-preflight \
   && grep -Fq '"$crypto_bin" verify-rsa-sha256-efi "$pub" "$uki" "$prediction.uki.sig"' "$preflight" \
   && grep -Fq 'efi-signature-count "$uki"' "$preflight" \
   && grep -Fq '"$crypto_bin" verify-pcr-prediction "$pub" "$prediction" "$generation"' "$preflight"; then
  ok "TPM PIN preflight verifies both the exact target UKI and its PCR11 policy"
else
  bad "TPM PIN preflight does not bind the exact target UKI and PCR11 policy"
fi

if [ -x "$prediction_signer" ] \
   && grep -Fq 'systemd-measure "$image"' "$prediction_signer" \
   && grep -Fq 'policy-digest --json=short --bank=sha256 --phase=enter-initrd' "$prediction_signer" \
   && grep -Fq '"$signer" sign-efi' "$prediction_signer" \
   && grep -Fq '"$signer" export-efi-cert' "$prediction_signer" \
   && grep -Fq 'TILO_EFI_SIGNING_CERT_SHA256' "$prediction_signer" \
   && grep -Fq 'verify-efi /work/efi-signing.crt' "$prediction_signer" \
   && grep -Fq 'verify-authenticode-envelope.py' "$prediction_signer" \
   && grep -Fq 'sign-sha256-rsa-pkcs1 "$work/target.efi" "$prediction.uki.sig"' "$prediction_signer" \
   && grep -Fq 'broker signed the prediction with a key other than Tilo' "$prediction_signer" \
   && ! grep -Eq 'PRIVATE[_-]?KEY|private-key' "$prediction_signer" \
   && grep -Fq 'TILO_REQUIRE_PCR_PREDICTION' "$repo_root/os/dev/build-custom-installer.sh" \
   && grep -Fq 'sign-target-prediction.sh" "$TILO_IMAGE"' "$repo_root/os/dev/build-image.sh" \
   && grep -Fq 'if [[ "$publish" == true ]]; then' "$os_image_workflow" \
   && grep -Fq "echo 'TILO_REQUIRE_PCR_PREDICTION=1' >> \"\$GITHUB_ENV\"" "$os_image_workflow" \
   && grep -Fq "echo 'TILO_REQUIRE_PCR_PREDICTION=0' >> \"\$GITHUB_ENV\"" "$os_image_workflow" \
   && grep -Fq 'PROTECTED_PCR_PREDICTION_SIGNER:' "$os_image_workflow" \
   && grep -Fq "github.event.inputs.release == 'true'" "$os_image_workflow" \
   && ! grep -Fq 'TILO_PCR_PREDICTION_SIGNER: ${{ vars.TILO_PCR_PREDICTION_SIGNER }}' "$os_image_workflow" \
   && grep -Fq 'TILO_ALLOW_LOCAL_PCR_SIGNING' "$repo_root/os/dev/build-image.sh" \
   && grep -Fq 'TILO_ALLOW_LOCAL_PCR_SIGNING' "$repo_root/os/dev/build-custom-installer.sh" \
   && grep -Fq 'sign-target-prediction.sh "$repro_image"' "$os_image_workflow" \
   && grep -Fq 'enable-openid-connect: true' "$os_image_workflow" \
   && grep -Fq "TILO_PCR_PREDICTION_SIGNER: \${{ startsWith(github.ref, 'refs/tags/imager-v')" \
      "$repo_root/.forgejo/workflows/release-imager.yml"; then
  ok "release prediction path uses systemd-measure and a broker signer without private-key custody"
else
  bad "release PCR prediction path can bypass authoritative measurement or protected signing"
fi

disk_encrypt="$repo_root/os/usr/libexec/tilo-disk-encrypt"
if grep -Fq 'enrolling the supplied strong passphrase as a LUKS key slot' "$disk_encrypt" \
   && grep -Fq 'verified TPM enrollment prerequisites changed after review' "$disk_encrypt" \
   && ! grep -Fq -- '--tpm2-pcrs=' "$disk_encrypt" \
   && ! grep -Fq 'force_passphrase=' "$disk_encrypt"; then
  ok "disk helper has no dormant PCR-only TPM path and rejects resealing"
else
  bad "disk helper can create a PCR-7-only TPM slot"
fi

if grep -Fq 'tilo-rollback "${rollback_args[@]}" enroll --device "$dev"' "$encryptor" \
   && grep -Fq -- '--require-tpm-prediction' "$encryptor" \
   && grep -Fq 'install -d -m 0755 /usr/share/tilo/pcr-predictions' "$production_containerfile" \
   && grep -Fq 'bash /run/tilo-build-initramfs.sh "$kver"' "$production_containerfile" \
   && grep -Fq -- '--add "crypt systemd-cryptsetup tpm2-tss plymouth tilo-rollback tilo-sibling-unlock"' "$repo_root/os/scripts/build-initramfs.sh" \
   && grep -Fq 'tilo-rollback-initrd-unlock' "$production_containerfile" \
   && grep -Fq 'tilo-root-discover' "$production_containerfile" \
   && grep -Fq 'tilo-root-generator' "$production_containerfile" \
   && ! grep -Fq -- '--tpm2-pcrs=' "$encryptor" \
   && grep -Fq -- '--recovery-key "$dev"' "$encryptor" \
   && grep -Fq -- '--fido2-device=auto "$dev"' "$encryptor"; then
  ok "signed prediction enrollment and same-disk Tilo root unlock ship together and fail closed"
else
  bad "installer TPM enrollment/early-unlock prerequisites are inconsistent"
fi

if grep -Fq 'CONF_TMP="$(mktemp ' "$backend" \
   && grep -Fq 'find "$1/state/deploy"' "$backend" \
   && grep -Fq 'DEPLOY_ETC="$(find_deploy_etc "$MNT")"' "$backend" \
   && grep -Fq 'chmod 0600 "$CONF_TMP"' "$backend" \
   && grep -Fq 'mv -f "$CONF_TMP" "$CONF"' "$backend"; then
  ok "account password hash is created privately and published atomically"
else
  bad "account password hash is briefly exposed or written non-atomically"
fi

if grep -Fq 'systemctl mask sshd.service' "$containerfile" \
   && ! grep -Fq 'quickuser:tilo:wheel' "$builder" \
   && grep -Fq -- '--dev-login requires a non-empty TILO_INSTALLER_DEV_PASSWORD' "$builder" \
   && grep -Fq 'append tilo.install' "$containerfile" \
   && grep -Fq 'show | grep -qw tilo.mode=normal' "$containerfile" \
   && grep -Fq 'show | grep -qw tilo.install' "$containerfile"; then
  ok "release installer has no public debug credential and masks SSH"
else
  bad "release installer still exposes a known debug login or SSH"
fi

if grep -Fq 'StandardInput=file:$INPUT' "$backend" \
   && grep -Fq 'StandardOutput=append:$OUTPUT' "$backend" \
   && grep -Fq 'tail --pid="$RUNNER"' "$backend" \
   && grep -Fq 'tilo-bridge-prepare-$$' "$backend" \
   && grep -Fq 'tilo-bridge-clean-$$' "$backend" \
   && grep -Fq 'installer runtime bridge permissions are unsafe' "$backend" \
   && grep -Fq 'installer runtime bridge has an unexpected SELinux label' "$backend" \
   && grep -Fq 'systemd-run is required for the enforced installer backend' "$backend" \
   && ! grep -Fq 'chcon --reference=/run/tilo-installer/bridge' "$backend" \
   && grep -Fq 'ExecStart=/usr/bin/install -d -m 0700 /run/tilo-installer/bridge' "$repo_root/os/usr/lib/systemd/system/tilo-installer-setup.service" \
   && ! grep -Fq -- '--pipe --collect' "$backend"; then
  ok "privileged handoff keeps secrets out of D-Bus fd passing and journald"
else
  bad "privileged handoff does not use the tested root-only file bridge"
fi

if grep -Fq "refusing to modify pre-existing account" "$firstboot_user" \
   && grep -Fq 'root|bin|daemon|' "$backend" \
   && grep -Fq "stat -c '%u:%a:%h'" "$firstboot_user" \
   && grep -Fq '[a-z_]* ) valid_user=1' "$firstboot_user" \
   && grep -Fq 'valid_sha512_crypt "$hash"' "$firstboot_user" \
   && grep -Fq 'rounds=([1-9][0-9]{3,8})' "$firstboot_user" \
   && grep -Fq -- '-M -e 1970-01-02 -d "$native_home" -p "$lock_value"' "$firstboot_user" \
   && ! grep -Fq 'useradd -m ' "$firstboot_user" \
   && ! grep -Fq -- '-p "$hash"' "$firstboot_user" \
   && grep -Fq '| chpasswd --encrypted' "$firstboot_user"; then
  ok "first boot revalidates credentials and installs the hash through stdin into a locked account"
else
  bad "installer can reset a privileged account, expose its hash in argv, or create an unlocked account"
fi
if bash "$repo_root/os/tests/firstboot_password_hash.sh" >/dev/null; then
  ok "first boot accepts only complete SHA-512 crypt password hashes"
else
  bad "first-boot password hash grammar rejects valid installer output or accepts malformed input"
fi

if ! python3 -B "$repo_root/os/tests/test_installer_firstboot_runtime.py"; then
  bad "installer does not provision firstboot accounts on older signed targets"
fi

firstboot_unit="$repo_root/os/usr/lib/systemd/system/tilo-firstboot-user.service"
for policy in \
  'TimeoutStartSec=infinity' \
  'NoNewPrivileges=yes' \
  'PrivateDevices=yes' \
  'PrivateNetwork=yes' \
  'ProtectKernelTunables=yes' \
  'CapabilityBoundingSet=CAP_CHOWN CAP_DAC_OVERRIDE CAP_FOWNER CAP_AUDIT_WRITE CAP_SETGID CAP_SETUID' \
  'AmbientCapabilities=' \
  'RestrictAddressFamilies=AF_UNIX AF_NETLINK' \
  'SystemCallFilter=@system-service'; do
  grep -Fqx "$policy" "$firstboot_unit" \
    || bad "first-boot account creator sandbox is missing: $policy"
done
if grep -Eq '^SystemCallFilter=~.*(^|[[:space:]])@network-io([[:space:]]|$)' "$firstboot_unit"; then
  bad 'first-boot account creator blocks the libaudit socket syscalls required by useradd'
fi

installer_setup_unit="$repo_root/os/usr/lib/systemd/system/tilo-installer-setup.service"
for policy in \
  'PrivateNetwork=yes' \
  'PrivateIPC=yes' \
  'ProtectHome=yes' \
  'ProtectProc=invisible' \
  'ProtectKernelModules=yes' \
  'ProtectKernelLogs=yes' \
  'RestrictNamespaces=yes' \
  'MemoryDenyWriteExecute=yes' \
  'SystemCallArchitectures=native'; do
  grep -Fqx "$policy" "$installer_setup_unit" \
    || bad "live installer setup sandbox is missing: $policy"
done
if ! python3 -B "$repo_root/os/tests/test_installer_selinux_bridge.py"; then
  bad 'live installer SELinux bridge policy or setup ordering regressed'
fi
if grep -Eq '^(PrivateDevices|ProtectKernelTunables)=' "$installer_setup_unit"; then
  bad 'live installer setup cannot inspect its source disk or load its SELinux policy'
fi

if [ -x "$profile_extractor" ] \
   && grep -Fq 'TILO_INSTALL_PROFILE_V1' "$profile_extractor" \
   && grep -Fq 'sha256sum "$payload"' "$profile_extractor" \
   && grep -Fq 'ExecStart=/usr/libexec/tilo-installer-profile-extract' \
      "$repo_root/os/usr/lib/systemd/system/tilo-installer-setup.service"; then
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

if grep -Fq 'die "setup handoff contains the human root secret"' "$backend" \
   && grep -Fq 'for public in user-identity/pubkey.bin device/cert.bin' "$backend" \
   && grep -Fq 'find "$HANDOFF_VAR/lib/tilo/device/certs"' "$backend"; then
  ok "installed handoff refuses the human root secret and keeps the enrolled identity's public files readable"
else
  bad "installed handoff can copy the human root secret or leave public identity files root-only"
fi

if grep -Fq 'chcon --reference=/etc/greetd/config.toml "$GREETD_CONFIG_TMP"' "$backend"; then
  ok "installed greeter configuration receives an enforcing-compatible /etc label"
else
  bad "installed greeter configuration can retain a transient /run SELinux label"
fi

if grep -Fq 'ConditionKernelCommandLine=!tilo.install' \
      "$repo_root/os/usr/lib/systemd/system/tilo-rollback-harden.service" \
   && grep -Fq 'ConditionKernelCommandLine=!tilo.install' \
      "$repo_root/os/usr/lib/systemd/system/shell-firstboot.service"; then
  ok "disposable live boots cannot consume first-boot state or mutate TPM lockout policy"
else
  bad "live installer can run installed-system first-boot or TPM-hardening services"
fi

if grep -Fq -- '--alongside' "$backend" \
   && grep -Fq 'signature_types="$(wipefs --noheadings --no-act --output TYPE "$ROOT_PARTITION")"' "$backend" \
   && [ "$(grep -c '^[[:space:]]*validate_alongside$' "$backend" || true)" -eq 4 ] \
   && grep -Fq 'signature_types="$(wipefs --noheadings --no-act --output TYPE "$part")"' "$encryptor" \
   && grep -Fq 'root, boot, and EFI partitions are not on the same disk' "$encryptor" \
   && grep -Fq 'boot partition already contains a signature' "$encryptor" \
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

if grep -Fq -- '--preflight-alongside)' "$backend" \
   && grep -Fq '[ "$PREFLIGHT_ALONGSIDE" -eq 1 ]' "$backend" \
   && grep -Fq 'Prepared storage check passed:' "$backend" \
   && grep -A5 -F 'if [ "$PREFLIGHT_ALONGSIDE" -eq 1 ]; then' "$backend" \
      | grep -Fq 'exit 0'; then
  ok "prepared-storage preflight reuses identity/ESP validation and exits before installation"
else
  bad "prepared-storage preflight is missing or can fall through into destructive installation"
fi

if grep -Fq 'prepare-partition "$ROOT_PARTITION" --boot "$BOOT_PARTITION" --efi "$EFI_PARTITION"' "$backend" \
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
   && grep -Fq 'mkfs.vfat -n TILO-EFI "$EFI_PARTITION"' "$guided_partitioner" \
   && grep -Fq 'sfdisk --part-type "$DISK" "$part_number" 7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab' "$backend" \
   && grep -Fq 'validate_partition_identity "$ROOT_PARTITION"' "$backend" \
   && ! grep -Eq 'sfdisk .*--delete|parted .* (move|resize)|resize2fs|ntfsresize' "$guided_partitioner"; then
  ok "guided alongside locks and revalidates the reviewed GPT map before appending only Tilo partitions"
else
  bad "guided alongside can mutate storage outside its reviewed free extent"
fi

# bootc#1929: with two ESPs, stock bootc can write the first (Windows) ESP
# even though the installer mounted a different Tilo ESP under /target.
if python3 - "$backend" "$repo_root/os/dev/build-custom-installer.sh" <<'PY'
from pathlib import Path
import sys

backend = Path(sys.argv[1]).read_text()
builder = Path(sys.argv[2]).read_text()
guard = backend.index('ALONGSIDE_BOOTC=/usr/libexec/tilo-bootc-alongside')
mutation = backend.index('GUIDED_RESULT="$(', guard)
assert guard < mutation
assert '&& [ "$(' in backend[guard:mutation]
assert 'alongside install requires the pinned mounted-ESP bootc helper' in backend[guard:mutation]
assert backend.count('"${ALONGSIDE_BOOTC_PODMAN_ARGS[@]}"') == 2
assert '--entrypoint /run/tilo-bootc-alongside' in backend
assert '0002-select-mounted-esp.patch' in builder
assert 'rootfs/usr/libexec/tilo-bootc-alongside' in builder
PY
then
  ok "alongside install selects a mounted-ESP bootc before any GPT mutation"
else
  bad "alongside install could run unpatched bootc against Windows ESP"
fi

# root=tilo-auto boots exactly one Tilo root per disk: both alongside paths
# refuse to leave a second one, and a staged installer's root is a distinct type.
if grep -Fq 'already holds a Tilo root partition' "$guided_partitioner" \
   && grep -Fq 'already holds another Tilo root' "$backend" \
   && ! grep -Fq 'd4c4daf6-f72c-43ee-b8e3-5ba32550311d' "$guided_partitioner" "$backend"; then
  ok "alongside installs never leave two Tilo roots on one disk"
else
  bad "an alongside install can leave a second Tilo root that makes both unbootable"
fi

if bash "$repo_root/os/tests/live_installer_service_invariants.sh"; then
  ok "durable-identity services stay quiet only on disposable installer media"
else
  bad "a durable-identity service has an incorrect live-installer gate"
fi

exit "$fail"
