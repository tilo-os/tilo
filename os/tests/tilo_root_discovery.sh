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
touch "$work/dev/esp" "$work/dev/tilo-root" "$work/dev/tilo-root-2" "$work/dev/linux-root" \
  "$work/dev/staging-root" "$work/dev/staging-root-2" "$work/dev/removable-root"
ln -s ../../esp "$work/dev/disk/by-partuuid/11111111-2222-3333-4444-555555555555"
esp_type=c12a7328-f81f-11d2-ba4b-00a0c93ec93b
tilo_type=7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab
staging_type=d4c4daf6-f72c-43ee-b8e3-5ba32550311d
linux_type=0fc63daf-8483-4772-8e79-3d69d8477de4

# Fixture lsblk: the boot ESP's parent disk is TILO_ROOT_TEST_DISK and its
# partitions are TILO_ROOT_TEST_PARTS, space-separated NAME:PARTTYPE pairs.
cat >"$work/bin/lsblk" <<'EOF'
#!/bin/bash
case "$*" in
  *PKNAME*) echo "$TILO_ROOT_TEST_DISK" ;;
  *NAME,TYPE,PARTTYPE*)
    printf '%s disk\n' "$TILO_ROOT_DEVROOT/$TILO_ROOT_TEST_DISK"
    for part in $TILO_ROOT_TEST_PARTS; do
      printf '%s part %s\n' "$TILO_ROOT_DEVROOT/${part%%:*}" "${part#*:}"
    done ;;
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
printf 'root=tilo-auto rw composefs=abc quiet\n' >"$work/installed-cmdline"
printf 'root=tilo-auto rw composefs=abc quiet tilo.install\n' >"$work/live-cmdline"

# discover KIND DISK PARTS...: resolve with the installed or live command line.
discover() {
  local kind="$1" disk="$2"
  shift 2
  rm -rf "$work/run/tilo-root"
  env TILO_ROOT_LOADER_PARTUUID=11111111-2222-3333-4444-555555555555 \
    TILO_ROOT_CMDLINE_FILE="$work/$kind-cmdline" \
    TILO_ROOT_TEST_DISK="$disk" TILO_ROOT_TEST_PARTS="$*" \
    TILO_ROOT_DEVROOT="$work/dev" TILO_ROOT_RUNTIME="$work/run" \
    TILO_ROOT_LSBLK="$work/bin/lsblk" TILO_ROOT_CRYPTSETUP_PROBE="$work/bin/cryptsetup" \
    TILO_ROOT_ALLOW_NONBLOCK=1 "$repo/os/usr/libexec/tilo-root-discover"
}
# expect_root NAME KIND DISK PARTS...: the resolver selects exactly NAME.
expect_root() {
  local name="$1"
  shift
  discover "$@"
  test "$(cat "$work/run/tilo-root/source")" = "$work/dev/$name"
  test "$(readlink "$work/run/tilo-root/device")" = "$work/dev/$name"
}
# expect_refusal PATTERN KIND DISK PARTS...: the resolver fails closed.
expect_refusal() {
  local pattern="$1"
  shift
  if discover "$@" 2>"$work/error"; then
    echo "resolver accepted: $*" >&2; exit 1
  fi
  if ! grep -q "$pattern" "$work/error"; then
    cat "$work/error" >&2; exit 1
  fi
  test ! -e "$work/run/tilo-root/source"
}

esp="esp:$esp_type"
expect_root tilo-root installed disk0 "$esp" "linux-root:$linux_type" "tilo-root:$tilo_type"
expect_root tilo-root installed disk0 "$esp" "tilo-root:${tilo_type^^}"
echo 'ok: foreign Linux roots are ignored; the same-disk Tilo root is selected'

expect_refusal 'expected exactly one Tilo root.*found 2' installed disk0 \
  "$esp" "tilo-root:$tilo_type" "tilo-root-2:$tilo_type"
echo 'ok: duplicate same-disk Tilo roots fail closed'

# A USB-free install leaves the staging ESP and root beside the new Tilo root
# until the installed system reclaims them. The installed UKI never sees them.
expect_root tilo-root installed disk0 "$esp" "linux-root:$linux_type" \
  "tilo-root:$tilo_type" "staging-esp:$esp_type" "staging-root:$staging_type"
expect_refusal 'expected exactly one Tilo root.*found 0' installed disk0 \
  "$esp" "staging-root:$staging_type"
echo 'ok: the installed UKI never boots a staging root'

# The staged live installer boots its own staging root, also when an earlier
# attempt already created the Tilo root it installs into.
expect_root staging-root live disk0 "$esp" "linux-root:$linux_type" "staging-root:$staging_type"
expect_root staging-root live disk0 "$esp" "tilo-root:$tilo_type" "staging-root:${staging_type^^}"
expect_refusal 'expected exactly one Tilo staging root.*found 2' live disk0 \
  "$esp" "staging-root:$staging_type" "staging-root-2:$staging_type"
echo 'ok: the live installer boots its staging root and refuses two of them'

# A live medium carries its own Tilo root and no staging root.
expect_root removable-root live removable0 "$esp" "removable-root:$tilo_type"
expect_refusal 'expected exactly one Tilo root.*found 0' live disk0 "$esp" "linux-root:$linux_type"
echo 'ok: a live medium boots its own Tilo root'

# A boot ESP on another disk must never authorize the first disk's Tilo root:
# only the partitions of the ESP's parent disk are candidates.
expect_root removable-root installed removable0 "$esp" "removable-root:$tilo_type"
echo 'ok: root authority follows the boot ESP physical disk, not disk enumeration order'

rm -f "$work/installed-cmdline"
expect_refusal 'could not read the kernel command line' installed disk0 "$esp" "tilo-root:$tilo_type"
echo 'ok: an unreadable kernel command line fails closed'

printf 'root=tilo-auto quiet\n' >"$work/cmdline"
: >"$work/initrd-release"
mkdir -p "$work/generator"
TILO_ROOT_CMDLINE_FILE="$work/cmdline" TILO_ROOT_INITRD_RELEASE="$work/initrd-release" \
  "$repo/os/usr/lib/systemd/system-generators/tilo-root-generator" "$work/generator"
grep -q 'What=/run/tilo-root/device' "$work/generator/sysroot.mount"
grep -q 'ExecStart=/usr/libexec/tilo-root-discover' "$work/generator/tilo-root-discover.service"
grep -qx 'RemainAfterExit=yes' "$work/generator/tilo-root-discover.service"
echo 'ok: target-independent initrd units generated'

# The booted system runs its generators with the same command line; the root is
# already mounted there, and the units must not appear.
mkdir -p "$work/generator-booted"
TILO_ROOT_CMDLINE_FILE="$work/cmdline" TILO_ROOT_INITRD_RELEASE="$work/no-initrd-release" \
  "$repo/os/usr/lib/systemd/system-generators/tilo-root-generator" "$work/generator-booted"
[ -z "$(ls -A "$work/generator-booted")" ]
echo 'ok: no root discovery units outside the initrd'

# Exercise the encrypted production path with an attach implementation that
# checks the actual options received by systemd-cryptsetup, not a source grep.
cat >"$work/bin/encrypted" <<'EOF'
#!/bin/bash
exit 0
EOF
cat >"$work/bin/attach" <<'EOF'
#!/bin/bash
[ "$1" = attach ] && [ "$2" = root ] && [ "$4" = - ]
case ",$5," in *,tries=0,*) ;; *) exit 1 ;; esac
case ",$5," in *,timeout=0,*) ;; *) exit 1 ;; esac
case ",$5," in *,password-cache=no,*) ;; *) exit 1 ;; esac
case ",$5," in *,tpm2-device=auto,*) ;; *) exit 1 ;; esac
case ",$5," in *,fido2-device=auto,*) ;; *) exit 1 ;; esac
EOF
chmod +x "$work/bin/encrypted" "$work/bin/attach"
TILO_ROOT_LOADER_PARTUUID=11111111-2222-3333-4444-555555555555 \
TILO_ROOT_CMDLINE_FILE="$work/cmdline" TILO_ROOT_TEST_DISK=disk0 \
TILO_ROOT_TEST_PARTS="$esp tilo-root:$tilo_type" \
TILO_ROOT_DEVROOT="$work/dev" TILO_ROOT_RUNTIME="$work/run" \
TILO_ROOT_LSBLK="$work/bin/lsblk" TILO_ROOT_CRYPTSETUP_PROBE="$work/bin/encrypted" \
TILO_ROOT_CRYPTSETUP="$work/bin/attach" TILO_ROOT_UNLOCK=true \
TILO_ROOT_ALLOW_NONBLOCK=1 "$repo/os/usr/libexec/tilo-root-discover"
test "$(readlink "$work/run/tilo-root/device")" = "$work/dev/mapper/root"
echo 'ok: encrypted root keeps retrying without cached failed passphrases and retains token unlock'

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
