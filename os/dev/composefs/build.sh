#!/bin/bash
# Build one generation of the composefs+UKI probe image.
#
#   ./os/dev/composefs/build.sh [GENERATION]      # default 1
#
# Produces:
#   localhost/tilo-composefs:gen<N>     the image (rootless podman)
#   $TILO_CFS_RUN/gen<N>.cmdline        the command line sealed into the UKI
#   $TILO_CFS_RUN/gen<N>.ukify.json     ukify's --measure report (expected PCR 11)
#
# Signing is on by default against a throwaway key pair generated once into
# $TILO_CFS_RUN/sb. That answers "what does signing look like without bootupd"
# without touching the machine-owner enrolment story: with a UKI there is
# exactly one PE binary to sign and it is signed at build time, not on the
# installed machine.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/common.sh"

GEN="${1:-1}"
IMAGE="$TILO_CFS_IMAGE:gen$GEN"

mkdir -p "$TILO_CFS_RUN"

# Everything the probe shares with the desktop image is copied in at build time, not
# duplicated in this directory, so the probe cannot drift from what Tilo ships.
cp -f "$TILO_OS_DIR/usr/lib/bootc/kargs.d/01-hardening.toml" "$HERE/01-hardening.toml"
# The mode karg is what makes repair mode selectable at all, and the probe is
# where that gets booted. Copied rather than restated for the same reason as the
# hardening set: a probe that carries its own copy proves nothing about the image.
cp -f "$TILO_OS_DIR/usr/lib/bootc/kargs.d/04-mode.toml" "$HERE/04-mode.toml"

# Assembled at the paths they occupy in the image, so the Containerfile needs one
# COPY rather than one per file: a layer commit over this rootfs takes minutes.
TREE="$HERE/stage/tree"
rm -rf "$TREE"
mkdir -p "$TREE/usr/bin" "$TREE/usr/libexec" "$TREE/usr/lib/systemd/system" \
         "$TREE/usr/lib/systemd/system-generators"
for f in usr/libexec/tilo-rollback-common \
         usr/libexec/tilo-rollback-confirm \
         usr/libexec/tilo-rollback-harden \
         usr/libexec/tilo-deployment \
         usr/libexec/tilo-boot-health \
         usr/libexec/tilo-repair \
         usr/lib/systemd/system/tilo-rollback-confirm.service \
         usr/lib/systemd/system/tilo-rollback-confirm.timer \
         usr/lib/systemd/system/tilo-rollback-harden.service \
         usr/lib/systemd/system/tilo-rollback-harden.timer \
         usr/lib/systemd/system/tilo-boot-entries.service \
         usr/lib/systemd/system/tilo-boot-entries.timer \
         usr/lib/systemd/system/tilo-boot-health.service \
         usr/lib/systemd/system/tilo-repair.target \
         usr/lib/systemd/system/tilo-repair@.service \
         usr/lib/systemd/system/tilo-deployment-rollback.service \
         usr/lib/systemd/system-generators/tilo-boot-mode-generator; do
  cp -f "$TILO_OS_DIR/$f" "$TREE/$f"
done
cp -f "$TILO_OS_DIR/tests/rollback-boot-check.sh" "$TREE/usr/libexec/tilo-rollback-boot-check"
cp -f "$TILO_OS_DIR/tests/recovery-boot-check.sh" "$TREE/usr/libexec/tilo-recovery-boot-check"

# The rollback binary. Built here rather than shipped, because the boot-side check
# asserts the tool the image carries — a stale copy would pass a check about a
# version nobody is running.
ROLLBACK_BIN="${ROLLBACK_BIN:-$TILO_REPO/target/release/tilo-rollback}"
[ -x "$ROLLBACK_BIN" ] || die "no tilo-rollback binary at $ROLLBACK_BIN
  Remedy: cargo build -p tilo-rollback --release"
cp -f "$ROLLBACK_BIN" "$TREE/usr/bin/tilo-rollback"
CRYPTO_BIN="${CRYPTO_BIN:-$TILO_REPO/target/release/tilo-crypto}"
[ -x "$CRYPTO_BIN" ] || die "no tilo-crypto binary at $CRYPTO_BIN
  Remedy: cargo build -p tilo-crypto --release"
cp -f "$CRYPTO_BIN" "$TREE/usr/bin/tilo-crypto"
# The repair entry is derived from the installed UKI by rewriting its sealed
# command line in place, so the tool that does that has to be on the machine:
# tilo-esp-write, run under the name tilo-uki-cmdline as the image links it.
ESP_WRITE_BIN="${ESP_WRITE_BIN:-$TILO_REPO/target/release/tilo-esp-write}"
[ -x "$ESP_WRITE_BIN" ] || die "no tilo-esp-write binary at $ESP_WRITE_BIN
  Remedy: cargo build -p tilo-esp-write --release"
cp -f "$ESP_WRITE_BIN" "$TREE/usr/libexec/tilo-esp-write"
ln -sf tilo-esp-write "$TREE/usr/libexec/tilo-uki-cmdline"

sb_key="$TILO_CFS_RUN/sb/db.key"
sb_crt="$TILO_CFS_RUN/sb/db.crt"
if [ "$TILO_CFS_SIGN" = "1" ] && [ ! -f "$sb_key" ]; then
  msg "generating a throwaway Secure Boot signing key"
  mkdir -p "$TILO_CFS_RUN/sb"
  tilo-crypto generate-secure-boot-cert "Tilo composefs spike" "$sb_key" "$sb_crt"
fi

msg "building $IMAGE (generation $GEN, sign=$TILO_CFS_SIGN)"
podman build \
  --build-arg "BASE_REF=$TILO_CFS_BASE_REF" \
  --build-arg "RPMS_REF=$TILO_CFS_RPMS_REF" \
  --build-arg "GENERATION=$GEN" \
  --build-arg "SEAL_GENERATION=${TILO_CFS_SEAL_GENERATION:-1}" \
  --build-arg "SESSION_OK=${TILO_CFS_SESSION_OK:-1}" \
  --build-arg "SIGN=$TILO_CFS_SIGN" \
  --build-arg "CFS_DIGEST=${TILO_CFS_DIGEST:-}" \
  --secret "id=secureboot_key,src=$sb_key" \
  --secret "id=secureboot_cert,src=$sb_crt" \
  --target final \
  -t "$IMAGE" \
  -f "$HERE/Containerfile" "$HERE"

# Pull the evidence off the built image rather than baking it into the rootfs:
# anything added to the rootfs after the digest is computed invalidates it.
msg "extracting the sealed command line"
podman run --rm --entrypoint /bin/bash "$IMAGE" -c '
  set -euo pipefail
  efi=$(ls /boot/EFI/Linux/*.efi | head -1)
  echo "UKI: $efi ($(stat -c %s "$efi") bytes)"
  objcopy -O binary --only-section=.cmdline "$efi" /dev/stdout 2>/dev/null || true
' > "$TILO_CFS_RUN/gen$GEN.uki-info" 2>&1 || true

# objcopy is not in the final image; read the section out with python instead.
podman run --rm --entrypoint /usr/bin/python3 "$IMAGE" -c '
import glob, struct, sys
efi = sorted(glob.glob("/boot/EFI/Linux/*.efi"))[0]
d = open(efi, "rb").read()
pe = struct.unpack_from("<I", d, 0x3c)[0]
nsec = struct.unpack_from("<H", d, pe + 6)[0]
optsz = struct.unpack_from("<H", d, pe + 20)[0]
base = pe + 24 + optsz
out = {}
for i in range(nsec):
    o = base + i * 40
    name = d[o:o+8].rstrip(b"\0").decode()
    vsz = struct.unpack_from("<I", d, o + 8)[0]
    raw = struct.unpack_from("<I", d, o + 20)[0]
    rsz = struct.unpack_from("<I", d, o + 16)[0]
    out[name] = d[raw:raw+min(vsz, rsz)]
sys.stdout.write(out[".cmdline"].rstrip(b"\0").decode())
' > "$TILO_CFS_RUN/gen$GEN.cmdline"

msg "sealed command line (generation $GEN):"
sed 's/^/    /' "$TILO_CFS_RUN/gen$GEN.cmdline" >&2; echo >&2
msg "image: $IMAGE"
