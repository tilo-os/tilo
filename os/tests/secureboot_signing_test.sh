#!/usr/bin/env bash
# Prove overlapping update/shutdown signing hooks serialize and converge.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
secureboot="$repo/os/usr/libexec/tilo-secureboot"
grep -Fq 'tilo-crypto durable-rename "$signed_output" "$a"' "$secureboot"
# The read-only ESP is written only by tilo-esp-write@secureboot-sign.
grep -Fq 'esp_install_signed "$signed_output" "${a#"$ESP"/}"' "$secureboot"
grep -Fq 'tilo-crypto durable-rename "$temporary" "$output"' "$secureboot"
grep -Fq 'exec 8<"$ESP"' "$secureboot"
if grep -Eq '^[[:space:]]*sync([[:space:]]|$)' "$secureboot"; then
  printf 'Secure Boot signing still performs a system-wide sync\n' >&2
  exit 1
fi
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
export TILO_SB_BOOT_ID_FILE="$work/boot-id"
printf '11111111-1111-1111-1111-111111111111\n' >"$TILO_SB_BOOT_ID_FILE"
mkdir -p "$work/bin" "$work/keys" "$work/boot/EFI/systemd" "$work/run"
chmod 0700 "$work/keys"
printf 'test db key\n' >"$work/keys/db.key"
printf 'test db certificate\n' >"$work/keys/db.crt"
chmod 0600 "$work/keys/db.key"
printf 'unsigned fixture\n' >"$work/boot/EFI/systemd/systemd-bootx64.efi"
printf 'untrusted predictable sidecar\n' \
  >"$work/boot/EFI/systemd/systemd-bootx64.efi.tilo-signed"

cat >"$work/bin/tilo-crypto" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  generate-secure-boot-cert)
    printf '%s\n' "$2" >"$3"
    printf '%s\n' "$2" >"$4"
    chmod 0600 "$3"
    ;;
  key-matches-certificate)
    cmp -s "$2" "$3"
    ;;
  certificate-esl)
    cp "$2" "$3"
    ;;
  firmware-read-esl)
    if [[ ${TILO_TEST_MISSING_KEK:-0} == 1 && $3 == KEK ]]; then
      exit 1
    fi
    if [[ -n ${TILO_TEST_FIRMWARE_DIR:-} && -s $TILO_TEST_FIRMWARE_DIR/$3.esl ]]; then
      cp "$TILO_TEST_FIRMWARE_DIR/$3.esl" "$4"
      exit 0
    fi
    printf 'factory-%s\n' "$3" >"$4"
    ;;
  firmware-write-esl)
    if [[ $3 == PK ]]; then
      [[ $# == 6 && -f $5 && -f $6 ]] || exit 64
    fi
    if [[ ${TILO_TEST_CORRUPT_DB:-0} == 1 && $3 == db ]]; then
      printf 'firmware-dropped-owner-certificate\n' >"$TILO_TEST_FIRMWARE_DIR/$3.esl"
    else
      cp "$4" "$TILO_TEST_FIRMWARE_DIR/$3.esl"
      if [[ ${TILO_TEST_RETAIN_EXTRA_DB:-0} == 1 && $3 == db ]]; then
        printf 'firmware-retained-extra-boot-authority\n' >>"$TILO_TEST_FIRMWARE_DIR/$3.esl"
      fi
    fi
    if [[ $3 == PK ]]; then
      printf '\6\0\0\0\0' >"$TILO_SB_EFIVARS/SetupMode-8be4df61-93ca-11d2-aa0d-00e098032b8c"
    fi
    ;;
  verify-efi-signature-list)
    ! grep -Fq 'malformed-vendor-esl' "$3" && grep -Fqx -- "$(cat "$2")" "$3"
    ;;
  verify-exclusive-efi-signature-list)
    [[ $(wc -l <"$3") -eq 1 ]] && grep -Fqx -- "$(cat "$2")" "$3"
    ;;
  verify-efi)
    grep -q '^Tilo-test-signature$' "$3"
    ;;
  sign-efi)
    sleep 0.2
    [ ! -e "$5" ] || exit 73
    cp "$4" "$5"
    printf 'Tilo-test-signature\n' >>"$5"
    ;;
  durable-rename)
    [[ ${TILO_TEST_DURABLE_RENAME_FAIL:-0} != 1 ]] || exit 1
    mv -- "$2" "$3"
    if [[ ${TILO_TEST_CORRUPT_DURABLE_RENAME:-0} == 1 ]]; then
      printf 'corrupt published artifact\n' >"$3"
    fi
    ;;
  durable-unlink)
    rm -- "$2"
    ;;
  *) exit 64 ;;
esac
EOF
chmod +x "$work/bin/tilo-crypto"
cat >"$work/bin/systemctl" <<'EOF'
#!/usr/bin/env bash
if [[ ${TILO_TEST_SYSTEMCTL_FAIL_START:-0} == 1 && $1 == start ]]; then exit 1; fi
[[ $1 == is-enabled || $1 == daemon-reload || $1 == restart || $1 == start ]]
EOF
chmod +x "$work/bin/systemctl"
cat >"$work/bin/systemd-creds" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ $1 == encrypt || $1 == decrypt ]]
if [[ -n ${TILO_TEST_CREDS_LOG:-} ]]; then printf '%s\n' "$1" >>"$TILO_TEST_CREDS_LOG"; fi
cp -- "${@: -2:1}" "${@: -1}"
EOF
chmod +x "$work/bin/systemd-creds"

# The ESP writer runs for real against the fixture ESP; a corrupted
# publication is modelled after it wrote.
mkdir -p "$work/plans/secureboot-sign"
chmod 0700 "$work/plans/secureboot-sign"
cat >"$work/bin/esp-write" <<EOF
#!/usr/bin/env bash
set -euo pipefail
dst="\$(sed -n 's/.*"dst": "\([^"]*\)".*/\1/p' "$work/plans/secureboot-sign/plan.json")"
TILO_ESP="\$TILO_SB_ESP" TILO_ESP_PLAN_ROOT="$work/plans" TILO_ESP_PLAN_OWNER="\$(id -u)" \\
  TILO_ESP_WRITE_NO_REMOUNT=1 "$repo/os/usr/libexec/tilo-esp-write" secureboot-sign
if [[ \${TILO_TEST_CORRUPT_DURABLE_RENAME:-0} == 1 ]]; then
  printf 'corrupt published artifact\n' >"\$TILO_SB_ESP/\$dst"
fi
EOF
chmod +x "$work/bin/esp-write"

run_sign() {
  PATH="$work/bin:$PATH" \
    TILO_SB_KEYDIR="$work/keys" \
    TILO_SB_BOOT="$work/boot" \
    TILO_SB_ESP="$work/boot" \
    TILO_SB_LOCK="$work/run/sign.lock" \
    TILO_ESP_PLAN_DIR="$work/plans/secureboot-sign" \
    TILO_ESP_WRITE_CMD="$work/bin/esp-write" \
    "$repo/os/usr/libexec/tilo-secureboot" sign
}

run_sign >"$work/first.log" & first=$!
run_sign >"$work/second.log" & second=$!
wait "$first"
wait "$second"

target="$work/boot/EFI/systemd/systemd-bootx64.efi"
[ "$(grep -c '^Tilo-test-signature$' "$target")" -eq 1 ]
grep -Fq 'signed 1 artefact(s)' "$work/first.log" "$work/second.log"
grep -Fq '1 already signed' "$work/first.log" "$work/second.log"
grep -Fqx 'untrusted predictable sidecar' "$target.tilo-signed"

# Verification must cover the final pathname, not only the temporary output.
# Model storage/publisher corruption after rename and require a failed pass.
cp "$target" "$work/valid-signed.efi"
printf 'unsigned fixture\n' >"$target"
if TILO_TEST_CORRUPT_DURABLE_RENAME=1 run_sign \
    >"$work/corrupt-publish.out" 2>"$work/corrupt-publish.err"; then
  echo 'Secure Boot signer accepted a corrupt durably published artefact' >&2
  exit 1
fi
grep -Fq 'installed artefact did not verify after durable publication' \
  "$work/corrupt-publish.err"
cp "$work/valid-signed.efi" "$target"

# A missing/unmounted ESP must not turn the update or shutdown signer into a
# successful no-op. Otherwise an enrolled machine can acknowledge an unsigned
# deployment and discover the broken chain only after firmware refuses boot.
mkdir "$work/empty-esp"
if PATH="$work/bin:$PATH" \
    TILO_SB_KEYDIR="$work/keys" \
    TILO_SB_BOOT="$work/empty-esp" \
    TILO_SB_ESP="$work/empty-esp" \
    "$repo/os/usr/libexec/tilo-secureboot" sign \
      >"$work/empty-sign.out" 2>"$work/empty-sign.err"; then
  echo 'Secure Boot signer reported success without a managed boot artefact' >&2
  exit 1
fi
grep -Fq 'found no managed boot artefacts' "$work/empty-sign.err"

chmod 0644 "$work/keys/db.key"
if run_sign >/dev/null 2>&1; then
  echo 'Secure Boot signing accepted a group/world-readable db key' >&2
  exit 1
fi
chmod 0600 "$work/keys/db.key"
ln "$work/keys/db.key" "$work/db-key-hardlink"
if run_sign >/dev/null 2>&1; then
  echo 'Secure Boot signing accepted a multiply-linked db key' >&2
  exit 1
fi
rm "$work/db-key-hardlink"

cp "$work/keys/db.key" "$work/keys/db.key.valid"
truncate -s 65537 "$work/keys/db.key"
if run_sign >/dev/null 2>&1; then
  echo 'Secure Boot signing accepted oversized private-key input' >&2
  exit 1
fi
mv "$work/keys/db.key.valid" "$work/keys/db.key"
chmod 0600 "$work/keys/db.key"

cp "$work/keys/db.crt" "$work/keys/db.crt.valid"
truncate -s 65537 "$work/keys/db.crt"
if run_sign >/dev/null 2>&1; then
  echo 'Secure Boot signing accepted oversized certificate input' >&2
  exit 1
fi
mv "$work/keys/db.crt.valid" "$work/keys/db.crt"
chmod 0644 "$work/keys/db.crt"
ln "$work/keys/db.crt" "$work/db-cert-hardlink"
if run_sign >/dev/null 2>&1; then
  echo 'Secure Boot signing accepted a multiply-linked trust anchor' >&2
  exit 1
fi
rm "$work/db-cert-hardlink"

chmod 0666 "$target"
if run_sign >/dev/null 2>&1; then
  echo 'Secure Boot signing accepted a group/world-writable boot artefact' >&2
  exit 1
fi
chmod 0644 "$target"
ln "$target" "$work/boot/EFI/systemd/systemd-boot-alias.efi"
if run_sign >/dev/null 2>&1; then
  echo 'Secure Boot signing accepted a multiply-linked boot artefact' >&2
  exit 1
fi
rm "$work/boot/EFI/systemd/systemd-boot-alias.efi"
chmod 0777 "$work/boot/EFI/systemd"
if run_sign >/dev/null 2>&1; then
  echo 'Secure Boot signing accepted a writable artefact parent directory' >&2
  exit 1
fi
chmod 0755 "$work/boot/EFI/systemd"

# The online db key must not become a recursive general-purpose EFI signing
# oracle. Only Tilo/bootc UKIs and OSTree deployment kernels are in scope.
mkdir -p "$work/boot/EFI/Linux/bootc" \
  "$work/boot/EFI/Linux/unmanaged.efi.extra.d" \
  "$work/boot/ostree/deployment" "$work/boot/unmanaged"
printf 'top-level uki\n' >"$work/boot/EFI/Linux/tilo.efi"
printf 'bootc uki\n' >"$work/boot/EFI/Linux/bootc/staged.efi"
printf 'ostree kernel\n' >"$work/boot/ostree/deployment/vmlinuz-test"
printf 'untrusted addon\n' \
  >"$work/boot/EFI/Linux/unmanaged.efi.extra.d/evil.addon.efi"
printf 'untrusted kernel\n' >"$work/boot/unmanaged/vmlinuz-evil"
run_sign >"$work/managed-layout.log"
grep -Fq 'signed 3 artefact(s)' "$work/managed-layout.log"
for managed in \
  "$work/boot/EFI/Linux/tilo.efi" \
  "$work/boot/EFI/Linux/bootc/staged.efi" \
  "$work/boot/ostree/deployment/vmlinuz-test"; do
  grep -Fq 'Tilo-test-signature' "$managed"
done
grep -Fqx 'untrusted addon' \
  "$work/boot/EFI/Linux/unmanaged.efi.extra.d/evil.addon.efi"
grep -Fqx 'untrusted kernel' "$work/boot/unmanaged/vmlinuz-evil"

generated="$work/generated"
PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$generated" \
  "$repo/os/usr/libexec/tilo-secureboot" generate-keys test >/dev/null
PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$generated" \
  "$repo/os/usr/libexec/tilo-secureboot" generate-keys test >/dev/null

chmod 0755 "$generated"
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$generated" \
    "$repo/os/usr/libexec/tilo-secureboot" sign >/dev/null 2>&1; then
  echo 'Secure Boot signing accepted a group/world-accessible key store' >&2
  exit 1
fi
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$generated" \
    "$repo/os/usr/libexec/tilo-secureboot" generate-keys test >/dev/null 2>&1; then
  echo 'Secure Boot key generation accepted an existing insecure key store' >&2
  exit 1
fi
chmod 0700 "$generated"

symlinked="$work/symlinked-keys"
ln -s "$generated" "$symlinked"
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$symlinked" \
    "$repo/os/usr/libexec/tilo-secureboot" generate-keys test >/dev/null 2>&1; then
  echo 'Secure Boot key generation accepted a symlinked key store' >&2
  exit 1
fi

rm -f "$generated/PK.crt"
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$generated" \
    "$repo/os/usr/libexec/tilo-secureboot" generate-keys test >/dev/null 2>&1; then
  echo 'incomplete Secure Boot keyset was silently accepted' >&2
  exit 1
fi
printf 'mismatched certificate\n' >"$generated/PK.crt"
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$generated" \
    "$repo/os/usr/libexec/tilo-secureboot" generate-keys test >/dev/null 2>&1; then
  echo 'mismatched Secure Boot private key and certificate were accepted' >&2
  exit 1
fi

captured="$work/captured"
PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" \
  "$repo/os/usr/libexec/tilo-secureboot" capture-vendor-keys >/dev/null
test -s "$captured/vendor-db.esl" -a -s "$captured/vendor-KEK.esl"
durability_failed="$work/durability-failed-capture"
if PATH="$work/bin:$PATH" TILO_TEST_DURABLE_RENAME_FAIL=1 \
  TILO_SB_KEYDIR="$durability_failed" \
  "$repo/os/usr/libexec/tilo-secureboot" capture-vendor-keys >/dev/null 2>&1; then
  echo 'factory-key capture reported success after durable publication failed' >&2
  exit 1
fi
test ! -e "$durability_failed/vendor-db.esl"
partial="$work/partial-capture"
if PATH="$work/bin:$PATH" TILO_TEST_MISSING_KEK=1 TILO_SB_KEYDIR="$partial" \
    "$repo/os/usr/libexec/tilo-secureboot" capture-vendor-keys >/dev/null 2>&1; then
  echo 'partial factory-key capture was reported as complete' >&2
  exit 1
fi
test -s "$partial/vendor-db.esl"

firmware="$work/firmware"
efivars="$work/efivars"
mkdir -p "$firmware" "$efivars"
printf '\6\0\0\0\1' >"$efivars/SetupMode-8be4df61-93ca-11d2-aa0d-00e098032b8c"
malformed_efivars="$work/malformed-efivars"
mkdir "$malformed_efivars"
printf '\0\0\0\0\1' >"$malformed_efivars/SetupMode-8be4df61-93ca-11d2-aa0d-00e098032b8c"
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" TILO_SB_EFIVARS="$malformed_efivars" \
    "$repo/os/usr/libexec/tilo-secureboot" status >/dev/null 2>&1; then
  echo 'Secure Boot state accepted an efivar with unauthentic attributes' >&2
  exit 1
fi
PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" TILO_SB_BOOT="$work/boot" \
  TILO_SB_ESP="$work/boot" TILO_SB_EFIVARS="$efivars" \
  TILO_TEST_FIRMWARE_DIR="$firmware" \
  "$repo/os/usr/libexec/tilo-secureboot" generate-keys test >/dev/null
cp "$captured/PK.esl" "$captured/PK.esl.valid"
printf 'untrusted additional platform authority\n' >>"$captured/PK.esl"
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" TILO_SB_BOOT="$work/boot" \
  TILO_SB_ESP="$work/boot" TILO_SB_EFIVARS="$efivars" \
  TILO_TEST_FIRMWARE_DIR="$firmware" \
  "$repo/os/usr/libexec/tilo-secureboot" enroll --yes >/dev/null 2>&1; then
  echo 'Secure Boot enrollment accepted an additional PK authority' >&2
  exit 1
fi
test ! -e "$firmware/db.esl" -a ! -e "$firmware/KEK.esl" -a ! -e "$firmware/PK.esl"
mv "$captured/PK.esl.valid" "$captured/PK.esl"
chmod 0644 "$captured/PK.esl"
cp "$captured/vendor-db.esl" "$captured/vendor-db.esl.valid"
truncate -s 1048570 "$captured/vendor-db.esl"
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" TILO_SB_BOOT="$work/boot" \
  TILO_SB_ESP="$work/boot" TILO_SB_EFIVARS="$efivars" \
  TILO_TEST_FIRMWARE_DIR="$firmware" \
  "$repo/os/usr/libexec/tilo-secureboot" enroll --yes >/dev/null 2>&1; then
  echo 'Secure Boot enrollment accepted an oversized merged vendor db' >&2
  exit 1
fi
test ! -e "$firmware/db.esl" -a ! -e "$firmware/KEK.esl" -a ! -e "$firmware/PK.esl"
mv "$captured/vendor-db.esl.valid" "$captured/vendor-db.esl"
chmod 0644 "$captured/vendor-db.esl"
cp "$captured/vendor-KEK.esl" "$captured/vendor-KEK.esl.valid"
printf 'malformed-vendor-esl\n' >>"$captured/vendor-KEK.esl"
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" TILO_SB_BOOT="$work/boot" \
  TILO_SB_ESP="$work/boot" TILO_SB_EFIVARS="$efivars" \
  TILO_TEST_FIRMWARE_DIR="$firmware" \
  "$repo/os/usr/libexec/tilo-secureboot" enroll --yes >/dev/null 2>&1; then
  echo 'Secure Boot enrollment accepted a malformed merged vendor KEK' >&2
  exit 1
fi
test ! -e "$firmware/db.esl" -a ! -e "$firmware/KEK.esl" -a ! -e "$firmware/PK.esl"
mv "$captured/vendor-KEK.esl.valid" "$captured/vendor-KEK.esl"
chmod 0644 "$captured/vendor-KEK.esl"
cp "$captured/db.esl" "$captured/db.esl.valid"
truncate -s 1048577 "$captured/db.esl"
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" TILO_SB_BOOT="$work/boot" \
  TILO_SB_ESP="$work/boot" TILO_SB_EFIVARS="$efivars" \
  TILO_TEST_FIRMWARE_DIR="$firmware" \
  "$repo/os/usr/libexec/tilo-secureboot" enroll --yes >/dev/null 2>&1; then
  echo 'Secure Boot enrollment accepted an oversized EFI signature list' >&2
  exit 1
fi
mv "$captured/db.esl.valid" "$captured/db.esl"
chmod 0644 "$captured/db.esl"
# Holding the firmware-directory inode lock must keep the complete db/KEK/PK
# transaction from starting. This covers separate invocations with different
# key-store paths as well as the ordinary same-store case.
exec 6<"$efivars"
flock -x 6
PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" TILO_SB_BOOT="$work/boot" \
  TILO_SB_ESP="$work/boot" TILO_SB_EFIVARS="$efivars" \
  TILO_TEST_FIRMWARE_DIR="$firmware" \
  "$repo/os/usr/libexec/tilo-secureboot" enroll --yes >/dev/null &
enroll_pid=$!
sleep 0.1
kill -0 "$enroll_pid"
test ! -e "$firmware/db.esl" -a ! -e "$firmware/KEK.esl" -a ! -e "$firmware/PK.esl"
flock -u 6
wait "$enroll_pid"
exec 6<&-
for variable in db KEK PK; do test -s "$firmware/$variable.esl"; done
[[ $(od -An -tu1 -j4 -N1 "$efivars/SetupMode-8be4df61-93ca-11d2-aa0d-00e098032b8c" | tr -d ' \n') == 0 ]]

vendor_captured="$work/vendor-captured"
cp -a "$captured" "$vendor_captured"
printf '22222222-2222-2222-2222-222222222222\n' >"$TILO_SB_BOOT_ID_FILE"
printf '\6\0\0\0\1' >"$efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c"
PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$vendor_captured" TILO_SB_BOOT="$work/boot" \
  TILO_SB_ESP="$work/boot" TILO_SB_EFIVARS="$efivars" \
  TILO_TEST_FIRMWARE_DIR="$firmware" \
  "$repo/os/usr/libexec/tilo-secureboot" retire-enrollment-keys --yes >/dev/null
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$vendor_captured" TILO_SB_TPM_DEVICE=/dev/null \
  TILO_SB_EFIVARS="$efivars" TILO_TEST_FIRMWARE_DIR="$firmware" \
  TILO_SB_RUNTIME_DIR="$work/vendor-seal-run" \
  TILO_SB_SYSTEMD_DIR="$work/vendor-systemd" \
  "$repo/os/usr/libexec/tilo-secureboot" seal-db-key --yes >/dev/null 2>&1; then
  echo 'PCR7-only db sealing accepted additional firmware boot authorities' >&2
  exit 1
fi
test -s "$vendor_captured/db.key" -a ! -e "$vendor_captured/db.key.cred"

merged_firmware="$work/merged-firmware"
merged_efivars="$work/merged-efivars"
mkdir -p "$merged_firmware" "$merged_efivars"
printf '\6\0\0\0\1' >"$merged_efivars/SetupMode-8be4df61-93ca-11d2-aa0d-00e098032b8c"
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" TILO_SB_BOOT="$work/boot" \
    TILO_SB_ESP="$work/boot" TILO_SB_EFIVARS="$merged_efivars" \
    TILO_TEST_FIRMWARE_DIR="$merged_firmware" TILO_TEST_RETAIN_EXTRA_DB=1 \
    "$repo/os/usr/libexec/tilo-secureboot" enroll --yes --owner-only-db \
      >/dev/null 2>&1; then
  echo 'owner-exclusive enrollment accepted firmware-retained db authority' >&2
  exit 1
fi
test ! -e "$merged_firmware/KEK.esl" -a ! -e "$merged_firmware/PK.esl"

# The explicit owner-only profile keeps captured vendor KEK for authenticated
# firmware updates but excludes vendor db boot authority, which is required
# before a PCR7-only credential can safely hold the online db key.
printf '\6\0\0\0\1' >"$efivars/SetupMode-8be4df61-93ca-11d2-aa0d-00e098032b8c"
PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" TILO_SB_BOOT="$work/boot" \
  TILO_SB_ESP="$work/boot" TILO_SB_EFIVARS="$efivars" \
  TILO_TEST_FIRMWARE_DIR="$firmware" \
  "$repo/os/usr/libexec/tilo-secureboot" enroll --yes --owner-only-db >/dev/null
cmp -s "$captured/db.esl" "$firmware/db.esl"
grep -Fqx -- "$(cat "$captured/KEK.esl")" "$firmware/KEK.esl"
grep -Fqx -- "$(cat "$captured/vendor-KEK.esl")" "$firmware/KEK.esl"
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" TILO_SB_BOOT="$work/boot" \
  TILO_SB_ESP="$work/boot" TILO_SB_EFIVARS="$efivars" \
  TILO_TEST_FIRMWARE_DIR="$firmware" \
  "$repo/os/usr/libexec/tilo-secureboot" retire-enrollment-keys --yes >/dev/null 2>&1; then
  echo 'Secure Boot enrollment keys retired without a verified reboot boundary' >&2
  exit 1
fi
printf '33333333-3333-3333-3333-333333333333\n' >"$TILO_SB_BOOT_ID_FILE"
ln "$captured/enrollment-boot-id" "$work/enrollment-boot-id-hardlink"
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" TILO_SB_BOOT="$work/boot" \
  TILO_SB_ESP="$work/boot" TILO_SB_EFIVARS="$efivars" \
  TILO_TEST_FIRMWARE_DIR="$firmware" \
  "$repo/os/usr/libexec/tilo-secureboot" retire-enrollment-keys --yes >/dev/null 2>&1; then
  echo 'Secure Boot key retirement accepted aliased enrollment reboot evidence' >&2
  exit 1
fi
rm "$work/enrollment-boot-id-hardlink"

bad_firmware="$work/bad-firmware"
bad_efivars="$work/bad-efivars"
mkdir -p "$bad_firmware" "$bad_efivars"
printf '\6\0\0\0\1' >"$bad_efivars/SetupMode-8be4df61-93ca-11d2-aa0d-00e098032b8c"
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" TILO_SB_BOOT="$work/boot" \
    TILO_SB_ESP="$work/boot" TILO_SB_EFIVARS="$bad_efivars" \
    TILO_TEST_FIRMWARE_DIR="$bad_firmware" TILO_TEST_CORRUPT_DB=1 \
    "$repo/os/usr/libexec/tilo-secureboot" enroll --yes >/dev/null 2>&1; then
  echo 'Secure Boot enrollment accepted a firmware db that dropped the owner certificate' >&2
  exit 1
fi
test ! -e "$bad_firmware/KEK.esl" -a ! -e "$bad_firmware/PK.esl"
printf '44444444-4444-4444-4444-444444444444\n' >"$TILO_SB_BOOT_ID_FILE"
PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" TILO_SB_BOOT="$work/boot" \
  TILO_SB_ESP="$work/boot" TILO_SB_EFIVARS="$efivars" \
  TILO_TEST_FIRMWARE_DIR="$firmware" \
  "$repo/os/usr/libexec/tilo-secureboot" retire-enrollment-keys --yes >/dev/null
test ! -e "$captured/PK.key" -a ! -e "$captured/KEK.key"
test -e "$captured/db.key" -a -e "$captured/PK.crt" -a -e "$captured/KEK.crt"
# Retirement is idempotent, but it still re-verifies the enrolled firmware
# certificates and the remaining db signing authority on every invocation.
PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" TILO_SB_BOOT="$work/boot" \
  TILO_SB_ESP="$work/boot" TILO_SB_EFIVARS="$efivars" \
  TILO_TEST_FIRMWARE_DIR="$firmware" \
  "$repo/os/usr/libexec/tilo-secureboot" retire-enrollment-keys --yes >/dev/null
ln "$captured/enrollment-keys-retired" "$work/retirement-marker-hardlink"
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" \
  "$repo/os/usr/libexec/tilo-secureboot" generate-keys test >/dev/null 2>&1; then
  echo 'key generation accepted an aliased enrollment-key retirement marker' >&2
  exit 1
fi
rm "$work/retirement-marker-hardlink"
printf 'unexpected extra state\n' >>"$captured/enrollment-keys-retired"
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" \
  "$repo/os/usr/libexec/tilo-secureboot" generate-keys test >/dev/null 2>&1; then
  echo 'key generation accepted a malformed enrollment-key retirement marker' >&2
  exit 1
fi
printf 'PK+KEK retired after verified enrollment\n' >"$captured/enrollment-keys-retired"
printf '\6\0\0\0\1' >"$efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c"
# Exercise the supported PCR7-only custody shape established above.
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" TILO_SB_TPM_DEVICE=/dev/null \
  TILO_SB_EFIVARS="$efivars" TILO_TEST_FIRMWARE_DIR="$firmware" \
  TILO_SB_RUNTIME_DIR="$work/seal-run" TILO_SB_SYSTEMD_DIR="$work/systemd" \
  TILO_TEST_SYSTEMCTL_FAIL_START=1 \
  "$repo/os/usr/libexec/tilo-secureboot" seal-db-key --yes \
    >/dev/null 2>"$work/seal-interrupted.err"; then
  echo 'db sealing removed plaintext before a credential-backed service activation' >&2
  exit 1
fi
if ! test -s "$captured/db.key" -a -s "$captured/db.key.cred"; then
  cat "$work/seal-interrupted.err" >&2
  exit 1
fi
# Resume the interrupted ceremony using the already verified sealed credential.
PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" TILO_SB_TPM_DEVICE=/dev/null \
  TILO_SB_EFIVARS="$efivars" TILO_TEST_FIRMWARE_DIR="$firmware" \
  TILO_SB_RUNTIME_DIR="$work/seal-run" TILO_SB_SYSTEMD_DIR="$work/systemd" \
  "$repo/os/usr/libexec/tilo-secureboot" seal-db-key --yes >/dev/null
test ! -e "$captured/db.key" -a -s "$captured/db.key.cred"
grep -Fq "LoadCredentialEncrypted=db.key:$captured/db.key.cred" \
  "$work/systemd/tilo-secureboot-sign.service.d/30-tilo-sealed-db-key.conf"
if grep -Fq 'LoadCredentialEncrypted=' \
  "$work/systemd/tilo-secureboot-sign-shutdown.service.d/30-tilo-sealed-db-key.conf"; then
  echo 'shutdown signer would keep the unsealed db key resident for all uptime' >&2
  exit 1
fi
test -e "$work/systemd/tilo-secureboot-sign.path.d/30-tilo-sealed-db-key.conf"
ln "$captured/db.key.cred" "$work/db-credential-hardlink"
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" \
  "$repo/os/usr/libexec/tilo-secureboot" generate-keys test >/dev/null 2>&1; then
  echo 'key generation accepted a multiply-linked TPM credential' >&2
  exit 1
fi
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" TILO_SB_BOOT="$work/boot" \
  TILO_SB_ESP="$work/boot" TILO_SB_TPM_DEVICE=/dev/null \
  TILO_SB_RUNTIME_DIR="$work/seal-run" \
  "$repo/os/usr/libexec/tilo-secureboot" sign-at-shutdown >/dev/null 2>&1; then
  echo 'shutdown signing accepted a multiply-linked TPM credential' >&2
  exit 1
fi
rm "$work/db-credential-hardlink"
chmod 0640 "$captured/db.key.cred"
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" \
  "$repo/os/usr/libexec/tilo-secureboot" generate-keys test >/dev/null 2>&1; then
  echo 'key generation accepted an exposed TPM credential' >&2
  exit 1
fi
chmod 0600 "$captured/db.key.cred"
creds_log="$work/shutdown-creds.log"
PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" TILO_SB_BOOT="$work/boot" \
  TILO_SB_ESP="$work/boot" TILO_SB_TPM_DEVICE=/dev/null \
  TILO_SB_RUNTIME_DIR="$work/seal-run" TILO_TEST_CREDS_LOG="$creds_log" \
  "$repo/os/usr/libexec/tilo-secureboot" sign-at-shutdown >/dev/null
grep -Fqx decrypt "$creds_log"
if compgen -G "$work/seal-run/.shutdown-db-key.*" >/dev/null; then
  echo 'shutdown signing left an unsealed db key in the runtime directory' >&2
  exit 1
fi
cp "$captured/db.key.cred" "$work/db.key.cred.good"
printf 'wrong shutdown key\n' >"$captured/db.key.cred"
if PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" TILO_SB_BOOT="$work/boot" \
  TILO_SB_ESP="$work/boot" TILO_SB_TPM_DEVICE=/dev/null \
  TILO_SB_RUNTIME_DIR="$work/seal-run" \
  "$repo/os/usr/libexec/tilo-secureboot" sign-at-shutdown >/dev/null 2>&1; then
  echo 'shutdown signing accepted an unsealed key that does not match db.crt' >&2
  exit 1
fi
if compgen -G "$work/seal-run/.shutdown-db-key.*" >/dev/null; then
  echo 'failed shutdown validation left plaintext key material behind' >&2
  exit 1
fi
mv "$work/db.key.cred.good" "$captured/db.key.cred"
PATH="$work/bin:$PATH" TILO_SB_KEYDIR="$captured" \
  "$repo/os/usr/libexec/tilo-secureboot" generate-keys test >"$work/sealed-generate.log"
grep -Fq 'db keyset is TPM-sealed, keeping it' "$work/sealed-generate.log"
printf 'ok: concurrent Secure Boot hooks serialize and produce one verified artefact\n'
printf 'ok: predictable sidecars cannot block or redirect Secure Boot signing\n'
printf 'ok: Secure Boot signer ignores EFI and kernel payloads outside managed layouts\n'
printf 'ok: Secure Boot key generation rejects partial and mismatched keysets\n'
printf 'ok: Secure Boot operations reject insecure and symlinked key stores\n'
printf 'ok: Secure Boot capture refuses to call a partial factory trust backup complete\n'
printf 'ok: Secure Boot enrollment verifies firmware read-back and exits setup mode\n'
printf 'ok: Secure Boot enrollment serializes the complete firmware policy transaction\n'
printf 'ok: Secure Boot enrollment stops before KEK/PK on corrupt db read-back\n'
printf 'ok: verified enrollment authority can be retired while db signing remains\n'
printf 'ok: key generation preserves a TPM-sealed db identity without recreating plaintext\n'
printf 'ok: TPM db sealing resumes safely and retires plaintext only after service activation\n'
printf 'ok: shutdown signing unseals just in time and retires its runtime plaintext\n'
