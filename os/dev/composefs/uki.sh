#!/bin/bash
# Build the sealed UKI inside the image build. Runs in the tools stage with the
# split rootfs bind-mounted at /target and its kernel at /kernel/<kver>.
#
# `bootc container ukify` is what makes this worth doing rather than calling
# ukify directly: it computes the composefs digest of /target, reads the extra
# kernel arguments out of /target/usr/lib/bootc/kargs.d, and hands both to
# ukify. So the sealed command line carries the image's own kargs — which is how
# a generation number reaches a *measured* command line.
#
# CFS_DIGEST overrides the digest it would compute. That exists because
# `bootc container ukify` and `bootc install` do not agree on the digest of the
# same image: install refuses the UKI with
#
#   The UKI has the wrong composefs= parameter (is 'sha512:<A>', should be
#   sha512:<B>)
#
# which is bootc#2334. Passing install's own expected value produces a UKI it
# accepts, and whether that value is stable across a rebuild is exactly the
# question — if it is, the seal is workable today with a two-pass build; if it
# moves every time, sealing is circular and blocked upstream.
set -euo pipefail

target=/target
mkdir -p /out
kver="$(ls /kernel)"

ukify_args=(
  --output "/out/${kver}.efi"
  # Print the PCR 11 values this UKI will produce, per boot phase. This is the
  # host-side prediction a booted machine's PCR 11 is compared against.
  --measure
  --json pretty
)

if [ -n "${CFS_DIGEST:-}" ]; then
  # Assemble the command line by hand: the kargs the image declares, in
  # kargs.d order, then the digest install told us it wants.
  # Read the kargs as TOML, not as quoted strings in a text file. These
  # drop-ins carry long prose comments, and a quoted phrase inside one is not a
  # kernel argument — grepping for quotes put the words "table stakes" on the
  # kernel command line, which is the kind of mistake that boots.
  cmdline="$(python3 - "$target" <<'EOPY'
import glob, sys, tomllib
args = []
for path in sorted(glob.glob(f"{sys.argv[1]}/usr/lib/bootc/kargs.d/*.toml")):
    with open(path, "rb") as fh:
        args += tomllib.load(fh).get("kargs", [])
print(" ".join(args))
EOPY
)"
  cmdline="${cmdline} composefs=${CFS_DIGEST#sha512:}"
  printf '%s\n' "$cmdline" > /out/cmdline.in
  echo "sealing an explicitly supplied digest"
  ukify build \
    --linux "/kernel/${kver}/vmlinuz" \
    --initrd "/kernel/${kver}/initramfs.img" \
    --uname "${kver}" \
    --cmdline "@/out/cmdline.in" \
    --os-release "@${target}/usr/lib/os-release" \
    "${ukify_args[@]}" | tee /out/ukify.json
else
  bootc container ukify \
    --rootfs "$target" \
    --kernel-dir "/kernel/${kver}" \
    -- "${ukify_args[@]}" | tee /out/ukify.json
fi

if [ "${SIGN:-0}" = "1" ]; then
  tilo-crypto sign-efi /run/secrets/secureboot_key \
    /run/secrets/secureboot_cert "/out/${kver}.efi" "/out/${kver}.efi.signed"
  tilo-crypto verify-efi /run/secrets/secureboot_cert "/out/${kver}.efi.signed"
  mv -f "/out/${kver}.efi.signed" "/out/${kver}.efi"
fi

# The sealed command line, extracted back out of the PE image. Nothing else can
# tell you what a UKI will actually pass to the kernel.
objcopy -O binary --only-section=.cmdline "/out/${kver}.efi" /out/cmdline
printf '%s\n' "${kver}" > /out/kver

ls -l /out
printf 'sealed cmdline: '; cat /out/cmdline; printf '\n'
