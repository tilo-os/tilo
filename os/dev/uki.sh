#!/bin/bash
# Build the sealed Unified Kernel Image, inside the image build.
#
# Runs in the `uki` stage of os/Containerfile with the split rootfs bind-mounted
# at /target and its kernel at /kernel/<kver>. Writes /out/<kver>.efi, which the
# final stage copies to /boot/EFI/Linux/.
#
# `bootc container ukify` is what makes this worth doing rather than calling ukify
# directly: it computes the composefs digest of /target, reads the extra kernel
# arguments out of /target/usr/lib/bootc/kargs.d, and hands both to ukify. So the
# sealed command line carries the image's own kargs — the hardening set and
# `qos.generation=` — and that is how a generation number reaches a *measured*
# command line rather than an editable file beside the kernel.
#
# The UKI is NOT signed here. qOS's root of trust is a Platform Key the machine
# generates for itself (docs/security/boot-chain.md), so there is no key at image-build
# time that the target firmware would accept. /usr/libexec/qos-secureboot signs
# the UKI on the installed machine, and qos-secureboot-sign.path already watches
# /boot/EFI/Linux for a newly staged one. Measurement does not depend on the
# signature: systemd-stub extends PCR 11 with the command line whether or not
# anyone signed the PE.
#
# CFS_DIGEST overrides the digest bootc would compute. That exists because
# `bootc container ukify` and `bootc install` do not agree on the digest of the
# same image: install refuses the UKI with
#
#   The UKI has the wrong composefs= parameter (is 'sha512:<A>', should be
#   sha512:<B>)
#
# which is bootc#2334. install's own expected value is stable across a rebuild of
# the UKI — the digest covers the rootfs, and /boot is the one tree excluded from
# it — so sealing the value install asked for converges in two passes.
# os/dev/build-image.sh drives that loop.
set -euo pipefail

target=/target
mkdir -p /out
kver="$(ls /kernel)"
python3 /usr/local/bin/make-boot-splash.py /out/qos-splash.bmp

ukify_args=(
  --output "/out/${kver}.efi"
  # systemd-stub paints this directly from the signed UKI. It replaces the
  # console-to-black gap without adding Plymouth or another boot service to
  # the critical chain.
  --splash /out/qos-splash.bmp
  # Print the PCR 11 values this UKI will produce, per boot phase. This is the
  # host-side prediction a booted machine's PCR 11 is checked against, and the
  # input a signed per-generation prediction is made from.
  --measure
  --json pretty
)

if [ -n "${CFS_DIGEST:-}" ]; then
  cfs_digest="${CFS_DIGEST#sha512:}"
  [[ "$cfs_digest" =~ ^[0-9a-f]{128}$ ]] || {
    echo "CFS_DIGEST must be a SHA-512 digest (128 lowercase hex characters)" >&2
    exit 1
  }
  # Assemble the command line by hand: the kargs the image declares, in kargs.d
  # order, then the digest install told us it wants.
  #
  # Read the kargs as TOML, not as quoted strings in a text file. These drop-ins
  # carry long prose comments, and a quoted phrase inside one is not a kernel
  # argument — grepping for quotes once put the words "table stakes" on the kernel
  # command line, which is the kind of mistake that boots.
  cmdline="$(python3 - "$target" <<'EOPY'
import glob, sys, tomllib
args = []
for path in sorted(glob.glob(f"{sys.argv[1]}/usr/lib/bootc/kargs.d/*.toml")):
    with open(path, "rb") as fh:
        args += tomllib.load(fh).get("kargs", [])
print(" ".join(args))
EOPY
)"
  [ -n "$cmdline" ] || { echo "no kargs found under $target/usr/lib/bootc/kargs.d" >&2; exit 1; }
  cmdline="${cmdline} composefs=${cfs_digest}"
  printf '%s\n' "$cmdline" > /out/cmdline.in
  echo "sealing the composefs digest bootc install asked for (bootc#2334)"
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

# The sealed command line, read back out of the PE image. Nothing else can tell
# you what a UKI will actually pass to the kernel — and this is the artefact the
# whole change exists to produce, so the build prints it rather than asserting it
# from the inputs.
objcopy -O binary --only-section=.cmdline "/out/${kver}.efi" /out/cmdline
printf '%s\n' "${kver}" > /out/kver

# Fail the build here rather than at first boot if the generation did not make it
# into the seal. QOS_SEAL_GENERATION=0 builds the negative control on purpose —
# a UKI that measures perfectly and carries no generation — so that a check
# reporting the generation as measured can be shown to fail when it is absent.
if [ "${QOS_SEAL_GENERATION:-1}" = "1" ]; then
  grep -q 'qos\.generation=[0-9]' /out/cmdline || {
    echo "the sealed command line carries no qos.generation=; rollback protection" >&2
    echo "would have nothing to bind to. Sealed command line was:" >&2
    cat /out/cmdline >&2
    exit 1
  }
else
  echo "NEGATIVE CONTROL: not asserting qos.generation= on the sealed command line"
fi

ls -l /out
printf 'sealed cmdline: '; cat /out/cmdline; printf '\n'
