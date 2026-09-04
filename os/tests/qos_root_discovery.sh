#!/bin/bash
set -euo pipefail
repo="$(cd "$(dirname "$0")/../.." && pwd)"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
mkdir -p "$work/dev/disk/by-partuuid" "$work/run" "$work/bin"
touch "$work/dev/esp" "$work/dev/qos-root" "$work/dev/linux-root" "$work/dev/removable-root"
ln -s ../../esp "$work/dev/disk/by-partuuid/11111111-2222-3333-4444-555555555555"

cat >"$work/bin/lsblk" <<'EOF'
#!/bin/bash
case "$*" in
  *PKNAME*) echo disk0 ;;
  *NAME,TYPE,PARTTYPE*)
    printf '%s part %s\n' "$QOS_ROOT_DEVROOT/linux-root" 0fc63daf-8483-4772-8e79-3d69d8477de4
    printf '%s part %s\n' "$QOS_ROOT_DEVROOT/qos-root" 7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab ;;
esac
EOF
cat >"$work/bin/cryptsetup" <<'EOF'
#!/bin/bash
exit 1
EOF
cat >"$work/bin/sbverify" <<'EOF'
#!/bin/bash
echo 'signature 1'
exit 0
EOF
cat >"$work/bin/sbverify-fail" <<'EOF'
#!/bin/bash
# Match the surprising behavior of sbsigntools 0.9.5: it exits successfully
# for an unsigned image, so callers must inspect the listing as well.
echo 'No signature table present'
exit 0
EOF
chmod +x "$work/bin/"*
env QOS_ROOT_LOADER_PARTUUID=11111111-2222-3333-4444-555555555555 \
  QOS_ROOT_DEVROOT="$work/dev" QOS_ROOT_RUNTIME="$work/run" \
  QOS_ROOT_LSBLK="$work/bin/lsblk" QOS_ROOT_CRYPTSETUP_PROBE="$work/bin/cryptsetup" \
  QOS_ROOT_ALLOW_NONBLOCK=1 "$repo/os/usr/libexec/qos-root-discover"
test "$(cat "$work/run/qos-root/source")" = "$work/dev/qos-root"
test "$(readlink "$work/run/qos-root/device")" = "$work/dev/qos-root"
echo 'ok: foreign Linux and removable qOS roots are ignored; same-disk qOS root selected'

# Two same-disk qOS roots are ambiguous and must fail closed.
cat >>"$work/bin/lsblk" <<'EOF'
printf '%s part %s\n' "$QOS_ROOT_DEVROOT/qos-root-2" 7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab
EOF
touch "$work/dev/qos-root-2"
if env QOS_ROOT_LOADER_PARTUUID=11111111-2222-3333-4444-555555555555 \
  QOS_ROOT_DEVROOT="$work/dev" QOS_ROOT_RUNTIME="$work/run" \
  QOS_ROOT_LSBLK="$work/bin/lsblk" QOS_ROOT_CRYPTSETUP_PROBE="$work/bin/cryptsetup" \
  QOS_ROOT_ALLOW_NONBLOCK=1 "$repo/os/usr/libexec/qos-root-discover" 2>"$work/error"; then
  echo 'duplicate qOS roots were accepted' >&2; exit 1
fi
grep -q 'expected exactly one qOS root.*found 2' "$work/error"
echo 'ok: duplicate same-disk qOS roots fail closed'

# A boot ESP on another disk must never authorize the first disk's qOS root.
cat >"$work/bin/lsblk" <<'EOF'
#!/bin/bash
case "$*" in
  *PKNAME*) echo removable0 ;;
  *NAME,TYPE,PARTTYPE*)
    printf '%s part %s\n' "$QOS_ROOT_DEVROOT/removable-root" 7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab ;;
esac
EOF
rm -rf "$work/run/qos-root"
env QOS_ROOT_LOADER_PARTUUID=11111111-2222-3333-4444-555555555555 \
  QOS_ROOT_DEVROOT="$work/dev" QOS_ROOT_RUNTIME="$work/run" \
  QOS_ROOT_LSBLK="$work/bin/lsblk" QOS_ROOT_CRYPTSETUP_PROBE="$work/bin/cryptsetup" \
  QOS_ROOT_ALLOW_NONBLOCK=1 "$repo/os/usr/libexec/qos-root-discover"
test "$(cat "$work/run/qos-root/source")" = "$work/dev/removable-root"
echo 'ok: root authority follows the boot ESP physical disk, not disk enumeration order'

printf 'root=qos-auto quiet\n' >"$work/cmdline"
mkdir -p "$work/generator"
QOS_ROOT_CMDLINE_FILE="$work/cmdline" \
  "$repo/os/usr/lib/systemd/system-generators/qos-root-generator" "$work/generator"
grep -q 'What=/run/qos-root/device' "$work/generator/sysroot.mount"
grep -q 'ExecStart=/usr/libexec/qos-root-discover' "$work/generator/qos-root-discover.service"
echo 'ok: target-independent initrd units generated'

# The install preflight binds both the PCR policy and the exact target UKI to
# the offline authority. A signed policy beside a substituted UKI is refused.
mkdir -p "$work/preflight/predictions"
printf 'deterministic target UKI bytes' >"$work/preflight/qos-target.efi"
printf '7\n' >"$work/generation"
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 \
  -out "$work/private.pem" >/dev/null 2>&1
openssl pkey -in "$work/private.pem" -pubout -out "$work/public.pem" >/dev/null 2>&1
prediction="$work/preflight/predictions/generation-7.json"
python3 - "$work/private.pem" "$prediction" <<'PY'
import base64, json, struct, subprocess, sys, tempfile
key, output = sys.argv[1:]
approved = bytes(range(32))
with tempfile.TemporaryDirectory() as td:
    msg, sig = td + "/msg", td + "/sig"
    open(msg, "wb").write(approved + struct.pack(">Q", 7))
    subprocess.run(["openssl", "dgst", "-sha256", "-sign", key,
                    "-out", sig, msg], check=True)
    signature = open(sig, "rb").read()
json.dump({"generation": 7, "pcr_bank": "sha256", "pcrs": [11],
           "approved_policy": base64.b64encode(approved).decode(),
           "signature": base64.b64encode(signature).decode()}, open(output, "w"))
PY
openssl dgst -sha256 -sign "$work/private.pem" -out "$prediction.uki.sig" \
  "$work/preflight/qos-target.efi"
QOS_PREFLIGHT_ROOT="$work/preflight" QOS_PREFLIGHT_PUBLIC_KEY="$work/public.pem" \
  QOS_PREFLIGHT_GENERATION="$work/generation" QOS_PREFLIGHT_SKIP_PLATFORM=1 \
  QOS_PREFLIGHT_SBVERIFY="$work/bin/sbverify" \
  "$repo/os/usr/libexec/qos-verified-boot-install-preflight"
if QOS_PREFLIGHT_ROOT="$work/preflight" QOS_PREFLIGHT_PUBLIC_KEY="$work/public.pem" \
  QOS_PREFLIGHT_GENERATION="$work/generation" QOS_PREFLIGHT_SKIP_PLATFORM=1 \
  QOS_PREFLIGHT_SBVERIFY="$work/bin/sbverify-fail" \
  "$repo/os/usr/libexec/qos-verified-boot-install-preflight"; then
  echo 'unsigned target UKI passed verified-boot preflight' >&2; exit 1
fi
echo 'ok: unsigned target UKI is rejected before TPM short-PIN enrollment'
printf 'tamper' >>"$work/preflight/qos-target.efi"
if QOS_PREFLIGHT_ROOT="$work/preflight" QOS_PREFLIGHT_PUBLIC_KEY="$work/public.pem" \
  QOS_PREFLIGHT_GENERATION="$work/generation" QOS_PREFLIGHT_SKIP_PLATFORM=1 \
  QOS_PREFLIGHT_SBVERIFY="$work/bin/sbverify" \
  "$repo/os/usr/libexec/qos-verified-boot-install-preflight"; then
  echo 'substituted target UKI passed authority preflight' >&2; exit 1
fi
echo 'ok: authority signature binds the exact deterministic target UKI'

# The updater form binds an explicitly-selected staged UKI to exactly one PCR11
# sidecar; changing the declared PCR set must fail even with an otherwise valid
# authority signature.
printf 'deterministic target UKI bytes' >"$work/preflight/qos-target.efi"
python3 - "$prediction" <<'PY'
import json, sys
p = sys.argv[1]
d = json.load(open(p))
d["pcrs"] = [7]
json.dump(d, open(p, "w"))
PY
if QOS_PREFLIGHT_PUBLIC_KEY="$work/public.pem" QOS_PREFLIGHT_SKIP_PLATFORM=1 \
  QOS_PREFLIGHT_SBVERIFY="$work/bin/sbverify" \
  "$repo/os/usr/libexec/qos-verified-boot-install-preflight" \
    --uki "$work/preflight/qos-target.efi" --prediction "$prediction" --generation 7; then
  echo 'PCR7-only release policy passed exact prediction preflight' >&2; exit 1
fi
echo 'ok: PCR7-only prediction is rejected rather than weakening release authorization'

grep -Fq 'arch: x86_64' "$repo/.forgejo/workflows/release-imager.yml"
grep -Fq 'arch: aarch64' "$repo/.forgejo/workflows/release-imager.yml"
# shellcheck source=lib/containerfile.sh
. "$repo/os/tests/lib/containerfile.sh"
containerfile_has_copy "$repo/os/Containerfile" \
  os/usr/lib/bootc/kargs.d/00-root.toml /usr/lib/bootc/kargs.d/00-root.toml
grep -Fq 'root=qos-auto' "$repo/os/usr/lib/bootc/kargs.d/00-root.toml"
echo 'ok: amd64 and arm64 release media share the target-independent qOS root contract'
