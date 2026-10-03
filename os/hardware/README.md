# Hardware profiles

Architecture and hardware model are separate build inputs. The default
`generic` image uses the platform firmware's boot description on either
x86-64 or ARM64. Registered profiles add model-specific firmware and an
optional device tree without changing the common build scripts.

## Defining a hardware profile

Add `os/hardware/profiles/<id>.json`, following the validated schema:

```json
{
  "schema_version": 2,
  "id": "example-board",
  "architecture": "aarch64",
  "devicetree": "vendor/example-board.dtb",
  "system_firmware": ["vendor/display-microcode.bin"],
  "firmware": {"radio.bin": "vendor/example-board/radio.bin"}
}
```

`architecture` is `aarch64` or `x86_64` and must match the native builder and
installed kernel. `devicetree` names a file relative to that kernel's `dtb/`
directory; it is `null` for ACPI or firmware-provided device descriptions.
Only ARM64 profiles may embed a DTB. `system_firmware` lists firmware already
installed from the reviewed distro RPM snapshot that must also be present in
the initramfs. It is separate from board-local OEM inputs and is never copied
from the local firmware artifact. Definitions use the logical kernel-requested
name; the preparer accepts exactly one installed representation: uncompressed,
XZ, or Zstandard. Version 1 definitions remain supported and have an empty
system-firmware list. `firmware` maps local input basenames
to unique paths under `/usr/lib/firmware/`; an empty mapping needs no local
firmware manifest. Paths cannot be absolute, traverse directories, or contain
shell metacharacters. Unknown fields and unsupported architectures fail.
The optional `always_on` (version 2, default `false`) marks a board that is
an always-on host rather than a personal computer; see the Orange Pi below.

The reviewed definition is copied into the immutable rootfs beside the
generated profile. The UKI builder checks both files' ownership and permissions,
their matching identity and architecture, and the exact kernel-specific DTB
path and hash. Adding a board does not add a new branch to the UKI builder.

The first registered profile, `lenovo-83ed`, describes the Lenovo Yoga Slim 7x.
Its kernel must supply `dtb/qcom/x1e80100-lenovo-yoga-slim7x.dtb`.
The retained 7.2.5 RPM's MSM module contains the Adreno 740 firmware selection
`a740_sqe.fw` (the inspected compressed module SHA-256 is
`4d500d42033b9fa3df293dd3f8b71a50067bbb5ad9e9c2ffda16497a45f03267`).
The profile therefore requires the distro-provided `qcom/a740_sqe.fw` and
includes it in the initramfs so GPU initialization does not depend on the
encrypted root already being mounted.

## Local firmware input

For the Lenovo profile, place these six files and `manifest.json` in
`os/hardware/firmware/lenovo-83ed/`. This directory is a local build input;
firmware blobs must not be committed. Preserve the matching Windows driver
package pair when selecting `qccdsp8380.mbn` and `cdsp_dtbs.elf`.

- `qcdxkmsuc8380.mbn`
- `qcvss8380.mbn`
- `qcadsp8380.mbn`
- `adsp_dtbs.elf`
- `qccdsp8380.mbn`
- `cdsp_dtbs.elf`

The manifest has exactly three fields: `schema_version` (integer `1`),
`profile` (`"lenovo-83ed"`), and `files` (an object mapping each of the six
basenames to its lowercase SHA-256). Hashes bind the selected local copies;
they do not prove compatibility or establish a publisher's signature.

### Public firmware gap for Lenovo 83ED

The six-file input above was extracted from this laptop's Windows drivers for
private development. As checked on 2026-09-25, upstream
[linux-firmware's 83ED directory](https://gitlab.com/kernel-firmware/linux-firmware/-/tree/main/qcom/x1e80100/LENOVO/83ED)
and Fedora's
[qcom-firmware file list](https://packages.fedoraproject.org/pkgs/linux-firmware/qcom-firmware/fedora-44-updates.html)
contain five of those six names. Neither lists `cdsp_dtbs.elf` for 83ED.
The five locally extracted files also have different SHA-256 values from the
same-named upstream 83ED files, so a distro package cannot silently replace
the reviewed private six-file set. The upstream
[83ED entries in WHENCE](https://gitlab.com/kernel-firmware/linux-firmware/-/blob/main/WHENCE)
identify the five published files and their licensing references; that does
not establish redistribution rights for the separately extracted Windows
files. The [Linux board device tree](https://github.com/torvalds/linux/blob/master/arch/arm64/boot/dts/qcom/x1e80100-lenovo-yoga-slim7x.dts)
requests the 83ED `cdsp_dtbs.elf` alongside `qccdsp8380.mbn`.

Do not publish a public 83ED image from the extracted six-file set. Before a
public release, obtain an authoritative, redistributable 83ED CDSP DTB source
with its license and notices, confirm the full firmware set against the
target kernel on the device, and build and sign the image in CI. A profile
that omits CDSP firmware would need a separately reviewed device tree and
physical test of the resulting feature loss; this profile still requires all
six files.

## Image build integration

Select the profile when building on native aarch64 Linux, after configuring
the normal pinned build inputs, registry access, generation and owner key:

Set `TILO_TARGET_IMGREF` explicitly to the reviewed update tag or SHA-256 digest
in the signature-required `registry.tilo.so/tilo/tilo` repository. It must identify
a published channel or artifact that retains this board's firmware and device
tree; this guide does not designate a release that has not been published.
Non-generic builds reject a missing reference before building and bind it into
the immutable hardware metadata. The installer uses that reference, permits an
identical override, and rejects a conflicting override before any disk work.
Generic images retain the official `latest` default and trusted overrides.

### CI-built Lenovo install image

`build-image.yml` accepts `hardware_profile=lenovo-83ed` on an explicit
`main`-branch dispatch with `publish=true`. Supply an independently
reviewed `firmware_candidate` OCI digest and `firmware_manifest_sha256`. The
artifact is a native Linux OCI image containing only `/firmware/`: the manifest
and exactly the files named by the selected hardware definition, packaged with
a `FROM scratch` / `COPY` image. CI imports those inert files on the native ARM64 runner,
builds and independently rebuilds the image, closes the composefs/UKI seal by
installing it to a disposable disk, signs its target prediction, and publishes
the signed ARM64 image at
`code.tilo.so/tilo/tilo:lenovo-83ed-<full-main-commit>`.
The image binds that exact immutable tag as its update reference. The workflow
attests the firmware artifact and manifest digest in SLSA provenance and retains
the import receipt. Obtain the final `repo@sha256:...` child digest from the CI
release-child artifact or registry tag and verify its Cosign signature and
provenance before a physical install.

This is an initial, generation-1 board image with an immutable update
reference. It does not join or advance the generic multiarch `latest`/`genN`
release series, and automatic board upgrades are not available until a
separate dense Lenovo generation series is implemented. QEMU's `virt` machine
cannot boot the Lenovo device tree, so CI's board path validates the disk
install and signed image without claiming a physical boot; the generic ARM64
image continues to run the full VM desktop gate. The first laptop boot and
device drivers still require on-device acceptance.

Local driver invocations may instead use `--hardware-profile` with already
staged, verified firmware. Firmware transfer and hash checks establish build
input identity, not hardware functionality or an installable image. A board
image's embedded device tree is not a substitute for a generic VM image.

```sh
TILO_HARDWARE_PROFILE=lenovo-83ed ./os/build.sh
```

The image builder runs `os/dev/prepare-hardware-profile.py` as root after
installing the selected kernel and before dracut builds the initramfs. It
reads the profile's local firmware directory through a build mount. An
explicit profile cannot be combined with `TILO_SKIP_BUILD=1`; the profile
must be included in a newly built image. Set `KERNEL_RPMS_REF` explicitly
when using a separately built native ARM64 kernel artifact.

The preparer validates every input hash and all paths before writing. It
rejects symlink redirects, a missing or malformed DTB, architecture mismatch,
and incomplete manifests. It writes root-owned files with mode `0644`:

- `/usr/lib/firmware/qcom/x1e80100/LENOVO/83ED/` contains the six firmware files.
- `/etc/dracut.conf.d/41-tilo-hardware.conf` includes those exact firmware paths
  plus the profile's reviewed distro-firmware paths in the initramfs, where
  display and input can need them during disk unlock.
- `/usr/lib/tilo/hardware-profile.json` binds the model, aarch64 architecture,
  kernel-specific DTB path, that DTB's SHA-256, and `target_update_ref`. The UKI
  builder validates this immutable image input before embedding the device
  tree and signing.
- `/usr/lib/tilo/hardware-profile-definition.json` holds the reviewed definition
  used to validate that generated profile.

Passing `--profile generic` creates no files and needs no local firmware.
These checks prepare build inputs; a signed image, VM installer checks, and a
live boot on the laptop are still needed to establish bootability. Other
firmware dependencies, including Wi-Fi and Bluetooth, must be checked against
the chosen kernel and image packages.

## Orange Pi 3B v2.1

`orangepi-3b-v2-1` is the Xunlong Orange Pi 3B hardware revision v2.1
(RK3566). The profile embeds `rockchip/rk3566-orangepi-3b-v2.1.dtb` and names
the SD-card drivers the UKI needs before the root is mounted. The Wi-Fi
driver (`mmc_core`, `cfg80211`, `brcmutil`, `brcmfmac`) is there too: the
SDIO radio is enumerated by that same controller before the root is mounted,
and nothing binds it later. So are the HDMI modules (`rockchipdrm`, `dw-hdmi`,
`phy-rockchip-inno-hdmi`, `display-connector`); mainline U-Boot for this board
has no video driver, so the monitor stays dark until Linux drives it.

Fedora's `linux-firmware` does not ship the AP6256 files `brcmfmac` loads.
`os/dev/fetch-orangepi-wifi-firmware.sh` stages Armbian's published
`brcmfmac43456-sdio.{bin,clm_blob,txt}`, pinned by SHA-256 in
`os/external-assets/github-assets.tsv`, into
`os/hardware/firmware/orangepi-3b-v2-1/` with their manifest; the aarch64 dev
lane runs it before building the board image. The SDIO node has no
`brcm,board-type`, so the driver first asks for names carrying the machine's
compatible string, `brcmfmac43456-sdio.xunlong,orangepi-3b-v2.1.*`; the
profile installs both sets. The NVRAM text names Broadcom's sample MAC
`00:90:4c:c5:12:38`; the image installs it without that line, so boards do
not share one address. NetworkManager keeps the permanent MAC for `brcmfmac`
(`os/etc/NetworkManager/conf.d/10-tilo-brcmfmac.conf`): the driver drops the
link when the address is changed. The onboard Ethernet PHY drops the cable
after idle while Energy Efficient Ethernet is on, so a NetworkManager
dispatcher (`os/usr/lib/NetworkManager/dispatcher.d/10-tilo-disable-eee`)
turns EEE off whenever `end0` comes up.

The profile is `"always_on": true`. `os/dev/install-board-layer.sh` gives such
a board image a session instead of a greeter:
`tilo-board-headless-session@<account>.service`, enabled by the disk step for
the account it creates, runs the person's Tilo session headless, and on the
display while a sink is connected (`tilo-seat-session`). greetd and sleep are
masked, chrony accepts the first NTS certificate before the clock can be
checked against it (the board has no battery-backed clock), and
`tilo-boot-health` judges the boot by that session.

The disk puts a 15 MiB unformatted hole at the 1 MiB alignment so the ESP starts at LBA 32768. `idbloader.img` sits in the unpartitioned sectors before that hole, and `u-boot.itb` sits inside the hole. `os/dev/build-orangepi-uboot.sh`
builds mainline U-Boot for `orangepi-3b-rk3566` and
`os/dev/install-rockchip-loader.py` writes `idbloader.img` at LBA 64 and
`u-boot.itb` at LBA 16384. The mask ROM loads that U-Boot, which EFI-boots the
UKI. The first partition must start at or after LBA 32768; a closer partition
is refused before any loader byte is written.

CI builds this profile from an explicit `workflow_dispatch` with
`hardware_profile=orangepi-3b-v2-1` on the native ARM64 runner. The image is
one aarch64 disk at generation 1. It does not enter the generic `latest` /
`genN` series. Dispatch it with publication left off; a signed board channel
is separate from that disk build.

```sh
TILO_BUILD_ARCH=aarch64 TILO_HARDWARE_PROFILE=orangepi-3b-v2-1 \
  TILO_TARGET_IMGREF=registry.tilo.so/tilo/tilo:orangepi-3b-v2-1-<commit> \
  ./os/build.sh --local
```

`TILO_BUILD_ARCH` is only for an x86 builder that emulates aarch64. A native
ARM builder leaves it unset. CI dispatches `hardware_profile=orangepi-3b-v2-1`
with publication left off.

The update-reference binding covers image construction and initial installation.
It does not prove that a later image published at that reference preserves the
profile. A check of staged updates against the installed hardware profile and
physical update/rollback acceptance remain separate work.

## Lenovo wireless verification

The locally inspected 7.2.5 kernel's Lenovo DTB declares Wi-Fi `pci17cb,1107`
and Bluetooth `qcom,wcn7850-bt`; these match the inspected Windows Wi-Fi PCI
identity (`17cb:1107`, subsystem `17aa:e0e9`) and FastConnect 7800 UART device.
Its config enables ath12k, QCA UART Bluetooth, and XZ-compressed firmware loading.
The inspected diagnostic runtime contains `atheros-firmware-20260622-1.fc44`,
including WCN7850 Wi-Fi firmware and `qca/hmtbtfw20.tlv`, `hmtnv20.bin`,
`hmtnv20.b10f`, and `hmtnv20.b112` (stored compressed). This is evidence about
local inputs, not a completed or signed final OS image.

For this Bluetooth compatible, the kernel selects `hmtbtfw<ROM>.tlv` and
`hmtnv<ROM>.b<board>` from controller responses; `hpbtfw21`/`hpnv21` files do
not establish WCN7850 support. Record the actual ROM/board ID and firmware
requests during a live boot before selecting any additional NVM. Likewise,
ath12k board-data selection uses QMI chip/board IDs and an optional variant,
so matching Windows PCI IDs alone do not prove the selected board entry.

The OS explicitly installs `NetworkManager-wifi` and `wireless-regdb` from
the frozen RPM snapshot. Regenerate that snapshot after this package-set
change; the build preflight rejects the previous manifest. Inspect the final
image for the firmware, Wi-Fi plugin, and signed regulatory database, then
verify Wi-Fi association, Bluetooth initialization/pairing, and suspend/resume
on the laptop. The diagnostic runtime and QEMU tests cannot establish those
physical-device results.
