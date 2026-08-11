#!/bin/bash
# Build one generation of the composefs+UKI probe image.
#
#   ./os/dev/composefs/build.sh [GENERATION]      # default 1
#
# Produces:
#   localhost/qos-composefs:gen<N>     the image (rootless podman)
#   $QOS_CFS_RUN/gen<N>.cmdline        the command line sealed into the UKI
#   $QOS_CFS_RUN/gen<N>.ukify.json     ukify's --measure report (expected PCR 11)
#
# Signing is on by default against a throwaway key pair generated once into
# $QOS_CFS_RUN/sb. That answers "what does signing look like without bootupd"
# without touching the machine-owner enrolment story: with a UKI there is
# exactly one PE binary to sign and it is signed at build time, not on the
# installed machine.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/common.sh"

GEN="${1:-1}"
IMAGE="$QOS_CFS_IMAGE:gen$GEN"

mkdir -p "$QOS_CFS_RUN"

# The hardening kargs come from the real image, not a copy, so the probe cannot
# drift from what qOS actually ships.
cp -f "$QOS_OS_DIR/usr/lib/bootc/kargs.d/01-hardening.toml" "$HERE/01-hardening.toml"

sb_key="$QOS_CFS_RUN/sb/db.key"
sb_crt="$QOS_CFS_RUN/sb/db.crt"
if [ "$QOS_CFS_SIGN" = "1" ] && [ ! -f "$sb_key" ]; then
  msg "generating a throwaway Secure Boot signing key"
  mkdir -p "$QOS_CFS_RUN/sb"
  openssl req -new -x509 -newkey rsa:2048 -nodes -days 3650 \
    -subj "/CN=qOS composefs spike/" -keyout "$sb_key" -out "$sb_crt" 2>/dev/null
fi

msg "building $IMAGE (generation $GEN, sign=$QOS_CFS_SIGN)"
podman build \
  --build-arg "BASE_REF=$QOS_CFS_BASE_REF" \
  --build-arg "RPMS_REF=$QOS_CFS_RPMS_REF" \
  --build-arg "GENERATION=$GEN" \
  --build-arg "SIGN=$QOS_CFS_SIGN" \
  --build-arg "CFS_DIGEST=${QOS_CFS_DIGEST:-}" \
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
' > "$QOS_CFS_RUN/gen$GEN.uki-info" 2>&1 || true

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
' > "$QOS_CFS_RUN/gen$GEN.cmdline"

msg "sealed command line (generation $GEN):"
sed 's/^/    /' "$QOS_CFS_RUN/gen$GEN.cmdline" >&2; echo >&2
msg "image: $IMAGE"
