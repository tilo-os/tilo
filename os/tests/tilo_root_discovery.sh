#!/bin/bash
set -euo pipefail
repo="$(cd "$(dirname "$0")/../.." && pwd)"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
if [ -n "${TILO_CRYPTO_BIN:-}" ]; then
  crypto="$TILO_CRYPTO_BIN"
else
  cargo build --quiet --manifest-path "$repo/Cargo.toml" -p tilo-crypto
  crypto="$repo/target/debug/tilo-crypto"
fi
mkdir -p "$work/dev/disk/by-partuuid" "$work/run" "$work/bin"
touch "$work/dev/esp" "$work/dev/tilo-root" "$work/dev/linux-root" "$work/dev/removable-root"
ln -s ../../esp "$work/dev/disk/by-partuuid/11111111-2222-3333-4444-555555555555"

cat >"$work/bin/lsblk" <<'EOF'
#!/bin/bash
case "$*" in
  *PKNAME*) echo disk0 ;;
  *NAME,TYPE,PARTTYPE*)
    printf '%s part %s\n' "$TILO_ROOT_DEVROOT/linux-root" 0fc63daf-8483-4772-8e79-3d69d8477de4
    printf '%s part %s\n' "$TILO_ROOT_DEVROOT/tilo-root" 7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab
    if [ "${TILO_ROOT_TEST_DUPLICATE:-0}" = 1 ]; then
      printf '%s part %s\n' "$TILO_ROOT_DEVROOT/tilo-root-2" 7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab
    fi ;;
esac
EOF
cat >"$work/bin/cryptsetup" <<'EOF'
#!/bin/bash
exit 1
EOF
cat >"$work/bin/signature-check" <<'EOF'
#!/bin/bash
echo 'signature 1'
exit 0
EOF
cat >"$work/bin/signature-check-fail" <<'EOF'
#!/bin/bash
exit 1
EOF
chmod +x "$work/bin/"*
env TILO_ROOT_LOADER_PARTUUID=11111111-2222-3333-4444-555555555555 \
  TILO_ROOT_DEVROOT="$work/dev" TILO_ROOT_RUNTIME="$work/run" \
  TILO_ROOT_LSBLK="$work/bin/lsblk" TILO_ROOT_CRYPTSETUP_PROBE="$work/bin/cryptsetup" \
  TILO_ROOT_ALLOW_NONBLOCK=1 "$repo/os/usr/libexec/tilo-root-discover"
test "$(cat "$work/run/tilo-root/source")" = "$work/dev/tilo-root"
test "$(readlink "$work/run/tilo-root/device")" = "$work/dev/tilo-root"
echo 'ok: foreign Linux and removable Tilo roots are ignored; same-disk Tilo root selected'

# Two same-disk Tilo roots are ambiguous and must fail closed.
touch "$work/dev/tilo-root-2"
if env TILO_ROOT_LOADER_PARTUUID=11111111-2222-3333-4444-555555555555 \
  TILO_ROOT_DEVROOT="$work/dev" TILO_ROOT_RUNTIME="$work/run" \
  TILO_ROOT_LSBLK="$work/bin/lsblk" TILO_ROOT_CRYPTSETUP_PROBE="$work/bin/cryptsetup" \
  TILO_ROOT_TEST_DUPLICATE=1 \
  TILO_ROOT_ALLOW_NONBLOCK=1 "$repo/os/usr/libexec/tilo-root-discover" 2>"$work/error"; then
  echo 'duplicate Tilo roots were accepted' >&2; exit 1
fi
if ! grep -q 'expected exactly one Tilo root.*found 2' "$work/error"; then
  cat "$work/error" >&2
  exit 1
fi
echo 'ok: duplicate same-disk Tilo roots fail closed'

# A boot ESP on another disk must never authorize the first disk's Tilo root.
cat >"$work/bin/lsblk" <<'EOF'
#!/bin/bash
case "$*" in
  *PKNAME*) echo removable0 ;;
  *NAME,TYPE,PARTTYPE*)
    printf '%s part %s\n' "$TILO_ROOT_DEVROOT/removable-root" 7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab ;;
esac
EOF
rm -rf "$work/run/tilo-root"
env TILO_ROOT_LOADER_PARTUUID=11111111-2222-3333-4444-555555555555 \
  TILO_ROOT_DEVROOT="$work/dev" TILO_ROOT_RUNTIME="$work/run" \
  TILO_ROOT_LSBLK="$work/bin/lsblk" TILO_ROOT_CRYPTSETUP_PROBE="$work/bin/cryptsetup" \
  TILO_ROOT_ALLOW_NONBLOCK=1 "$repo/os/usr/libexec/tilo-root-discover"
test "$(cat "$work/run/tilo-root/source")" = "$work/dev/removable-root"
echo 'ok: root authority follows the boot ESP physical disk, not disk enumeration order'

printf 'root=tilo-auto quiet\n' >"$work/cmdline"
mkdir -p "$work/generator"
TILO_ROOT_CMDLINE_FILE="$work/cmdline" \
  "$repo/os/usr/lib/systemd/system-generators/tilo-root-generator" "$work/generator"
grep -q 'What=/run/tilo-root/device' "$work/generator/sysroot.mount"
grep -q 'ExecStart=/usr/libexec/tilo-root-discover' "$work/generator/tilo-root-discover.service"
echo 'ok: target-independent initrd units generated'

# The install preflight binds both the PCR policy and the exact target UKI to
# the offline authority. A signed policy beside a substituted UKI is refused.
mkdir -p "$work/preflight/predictions"
printf 'deterministic target UKI bytes' >"$work/preflight/tilo-target.efi"
printf '7\n' >"$work/generation"
"$crypto" generate-tpm-authority-key "$work/private.pem" "$work/public.pem"
prediction="$work/preflight/predictions/generation-7.json"
python3 - "$work/message.bin" <<'PY'
import struct, sys
approved = bytes(range(32))
open(sys.argv[1], "wb").write(approved + struct.pack(">Q", 7))
PY
"$crypto" sign-rsa-sha256 "$work/private.pem" "$work/message.bin" "$work/signature.bin"
python3 - "$work/signature.bin" "$prediction" <<'PY'
import base64, json, sys
signature, output = sys.argv[1:]
approved = bytes(range(32))
json.dump({"generation": 7, "pcr_bank": "sha256", "pcrs": [11],
           "approved_policy": base64.b64encode(approved).decode(),
           "signature": base64.b64encode(open(signature, "rb").read()).decode()}, open(output, "w"))
PY
"$crypto" sign-rsa-sha256 "$work/private.pem" \
  "$work/preflight/tilo-target.efi" "$prediction.uki.sig"
TILO_PREFLIGHT_ROOT="$work/preflight" TILO_PREFLIGHT_PUBLIC_KEY="$work/public.pem" \
  TILO_PREFLIGHT_GENERATION="$work/generation" TILO_PREFLIGHT_SKIP_PLATFORM=1 \
  TILO_PREFLIGHT_CRYPTO="$crypto" \
  TILO_PREFLIGHT_SIGNATURE_CHECK="$work/bin/signature-check" \
  "$repo/os/usr/libexec/tilo-verified-boot-install-preflight"
if TILO_PREFLIGHT_ROOT="$work/preflight" TILO_PREFLIGHT_PUBLIC_KEY="$work/public.pem" \
  TILO_PREFLIGHT_GENERATION="$work/generation" TILO_PREFLIGHT_SKIP_PLATFORM=1 \
  TILO_PREFLIGHT_CRYPTO="$crypto" \
  TILO_PREFLIGHT_SIGNATURE_CHECK="$work/bin/signature-check-fail" \
  "$repo/os/usr/libexec/tilo-verified-boot-install-preflight"; then
  echo 'unsigned target UKI passed verified-boot preflight' >&2; exit 1
fi
echo 'ok: unsigned target UKI is rejected before TPM short-PIN enrollment'
printf 'tamper' >>"$work/preflight/tilo-target.efi"
if TILO_PREFLIGHT_ROOT="$work/preflight" TILO_PREFLIGHT_PUBLIC_KEY="$work/public.pem" \
  TILO_PREFLIGHT_GENERATION="$work/generation" TILO_PREFLIGHT_SKIP_PLATFORM=1 \
  TILO_PREFLIGHT_CRYPTO="$crypto" \
  TILO_PREFLIGHT_SIGNATURE_CHECK="$work/bin/signature-check" \
  "$repo/os/usr/libexec/tilo-verified-boot-install-preflight"; then
  echo 'substituted target UKI passed authority preflight' >&2; exit 1
fi
echo 'ok: authority signature binds the exact deterministic target UKI'

# The updater form binds an explicitly-selected staged UKI to exactly one PCR11
# sidecar; changing the declared PCR set must fail even with an otherwise valid
# authority signature.
printf 'deterministic target UKI bytes' >"$work/preflight/tilo-target.efi"
python3 - "$prediction" <<'PY'
import json, sys
p = sys.argv[1]
d = json.load(open(p))
d["pcrs"] = [7]
json.dump(d, open(p, "w"))
PY
if TILO_PREFLIGHT_PUBLIC_KEY="$work/public.pem" TILO_PREFLIGHT_SKIP_PLATFORM=1 \
  TILO_PREFLIGHT_CRYPTO="$crypto" \
  TILO_PREFLIGHT_SIGNATURE_CHECK="$work/bin/signature-check" \
  "$repo/os/usr/libexec/tilo-verified-boot-install-preflight" \
    --uki "$work/preflight/tilo-target.efi" --prediction "$prediction" --generation 7; then
  echo 'PCR7-only release policy passed exact prediction preflight' >&2; exit 1
fi
echo 'ok: PCR7-only prediction is rejected rather than weakening release authorization'

grep -Fq 'arch: x86_64' "$repo/.forgejo/workflows/release-imager.yml"
grep -Fq 'arch: aarch64' "$repo/.forgejo/workflows/release-imager.yml"
# shellcheck source=lib/containerfile.sh
. "$repo/os/tests/lib/containerfile.sh"
containerfile_has_copy "$repo/os/Containerfile" \
  os/usr/lib/bootc/kargs.d/00-root.toml /usr/lib/bootc/kargs.d/00-root.toml
grep -Fq 'root=tilo-auto' "$repo/os/usr/lib/bootc/kargs.d/00-root.toml"
echo 'ok: amd64 and arm64 release media share the target-independent Tilo root contract'
