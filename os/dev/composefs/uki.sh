#!/bin/bash
# Build the sealed UKI inside the image build. Runs in the tools stage with the
# split rootfs bind-mounted at /target and its kernel at /kernel/<kver>.
#
# `bootc container ukify` is what makes this worth doing rather than calling
# ukify directly: it computes the composefs digest of /target, reads the extra
# kernel arguments out of /target/usr/lib/bootc/kargs.d, and hands both to
# ukify. So the sealed command line carries the image's own kargs — which is
# how a generation number reaches a *measured* command line.
set -euo pipefail

mkdir -p /out
kver="$(ls /kernel)"

ukify_args=(
  --output "/out/${kver}.efi"
  # Print the PCR 11 values this UKI will produce, per boot phase. This is the
  # host-side prediction the booted machine's PCR 11 is compared against.
  --measure
  --json pretty
)

if [ "${SIGN:-0}" = "1" ]; then
  ukify_args+=(
    --signtool sbsign
    --secureboot-private-key /run/secrets/secureboot_key
    --secureboot-certificate /run/secrets/secureboot_cert
  )
fi

bootc container ukify \
  --rootfs /target \
  --kernel-dir "/kernel/${kver}" \
  -- "${ukify_args[@]}" | tee /out/ukify.json

# The sealed command line, extracted back out of the PE image. Nothing else can
# tell you what a UKI will actually pass to the kernel.
objcopy -O binary --only-section=.cmdline "/out/${kver}.efi" /out/cmdline
printf '%s\n' "${kver}" > /out/kver

ls -l /out
printf 'sealed cmdline: '; cat /out/cmdline; printf '\n'
