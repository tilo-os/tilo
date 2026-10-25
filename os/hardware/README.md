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
The optional `firmware_exclude` (version 2) names Fedora firmware RPMs the
board layer removes (`os/dev/install-board-layer.sh`) because the board has no
such hardware. Only packages in `FIRMWARE_PACKAGE_TREES`
(`os/dev/hardware_profile.py`) are accepted, and a package whose firmware
subtree the definition's own `system_firmware` or `firmware` names is refused.
Generic images keep every package. lenovo-83ed drops about 190 MB this way.
The optional `kernel_args` (version 2) lists kernel arguments the board
needs, one unquoted token each. The preparer writes them to
`/usr/lib/bootc/kargs.d/10-hardware-profile.toml`, after the image's own
drop-ins, so they are sealed into the UKI command line and a console named
there is the last `console=`, which the kernel makes `/dev/console`.
The optional `internal_keyboards` (version 2) names built-in I2C or SPI
keyboards libinput must treat as internal; see the Lenovo keyboard section.
The optional `dmi` (version 2) names a firmware-described board's DMI system
identity, `{"sys_vendor": ..., "product_name": ...}`, as `/sys/class/dmi/id`
reports it. It is only for a board with `"devicetree": null` and is refused
beside a device tree, which already names the model; see the handheld
section.
The optional `qcom_snapdragon` (version 2, default `false`) marks a
Snapdragon X laptop, which is the only kind of board whose image installs
Fedora's `qcom-firmware` RPM: that package's Adreno firmware is what those
boards' GPU loads, and a generic image cannot carry it because the same
package also holds firmware for Snapdragon boards Tilo has not looked at.
Both image recipes read the declaration instead of naming profile ids. A
board that sets it must be `aarch64`; see the Snapdragon X Elite section,
where `lenovo-83ed` is the board-model profile and `x1e80100-generic` is the
one for every laptop already described by its own published firmware.

The reviewed definition is copied into the immutable rootfs beside the
generated profile. The UKI builder checks both files' ownership and permissions,
their matching identity and architecture, and the exact kernel-specific DTB
path and hash. Adding a board does not add a new branch to the UKI builder.

The first registered profile, `lenovo-83ed`, describes the Lenovo Yoga Slim 7x.
Its kernel must supply `dtb/qcom/x1e80100-lenovo-yoga-slim7x.dtb`.
Older MSM kernels request `a740_sqe.fw`; the installed 7.2.7 kernel requests
`gen70500_sqe.fw`, `gen70500_gmu.bin`, and the X1E80100-specific
`gen70500_zap.mbn`. The profile includes both generations' distro firmware
in the initramfs. A physical first boot showed that checking only the older
filename allowed the newer GPU's firmware request to fail before the encrypted
root was available. Firmware presence must follow the actual kernel requests.

The 83ED's TPM is Microsoft Pluton. Windows reaches it through a TCG CRB
window that the `qcom.tz.mssecapp` TrustZone app reports and services (ACPI
TPM2 start method 14). Tilo's kernel drives it the same way with
`tpm_qcom_mssec` (`os/kernel/patches/0016` and `0017`), built in. The driver
binds when TrustZone reports a TPM type other than a discrete or Qualcomm
firmware TPM.

`systemd-tpm2-generator` sees the firmware's TPM event log and, by default,
makes `sysinit.target` wait for `/dev/tpm0` and `/dev/tpmrm0` until the 45
second device timeout, once in the initrd and again after switch-root. Before
the driver existed, that held the splash for a minute and a half after the disk
was unlocked on a physical first boot. The built-in driver now creates the
device before systemd starts. The profile still sets `systemd.tpm2_wait=0`, so
a firmware whose mssecapp refuses the TPM costs nothing at boot. Boards whose
TPM driver is a module (an I2C or SPI TPM) still need the wait, so it stays on
everywhere else.

### Every board says what it keeps

A hang leaves nothing behind unless the board was set up to record it, so each
schema-2 definition carries a `crash_record` naming three things:

- `storage` - where the kernel's record of a panic goes. `ramoops` is a region
  the board's device tree reserves, `block` a reserved region on a block
  device (pstore/blk), `efi-variable` firmware variables (which wear, and are
  the last resort), and `none` a board that keeps no record. Only a
  device-tree board can claim `ramoops` or `block`, because it is the device
  tree that reserves the memory.
- `watchdog` - what resets a machine that stops responding: `hardware` for a
  watchdog built into the SoC, `module` for a driver the board installs (which
  must then be in `boot_modules`, so it reaches the initramfs), and `none`.
- `lockup` - what panics first when a CPU stops: `buddy` for ARM64's detector,
  `nmi` for the perf detector x86 uses, and `none` for a board whose stuck CPU
  only logs.

The declaration is held to the board's own kernel arguments: a board may name
a watchdog or a detector only when its command line sets one, and a board that
says it resets nothing or panics from nothing may not carry the arguments that
would. `hardware_profile.py` and its Rust mirror enforce this at image build,
so a board cannot claim a watchdog it never feeds or a detector it never
enables. A board that keeps no record says so rather than leaving it out.
Schema-1 definitions predate the field and are still accepted unchanged.

### Lenovo 83ED crash evidence

The 83ED has reset itself with nothing in the journal: journald writes to disk
every few minutes, so a reset loses the last of the log. The kernel keeps its
console log, and any oops or panic dump, in a 2 MiB ramoops region at
`0xc00000000` that its device tree reserves (`os/kernel/patches/0021`). The
region survives a warm reset. On the next boot `systemd-pstore` moves the
records into the journal of that boot; read them with
`journalctl -b 0 -u systemd-pstore -o verbose`.
`tilo-pstore-console.service` raises the console log level once userspace runs,
because `loglevel=3` would otherwise keep everything below critical out of the
record.

A hang has to become a panic to leave stacks there. The profile's kernel
arguments do that:

- `systemd.watchdog_sec=30` makes systemd feed the SoC's SBSA watchdog.
  Without it the watchdog is never started, and a hard hang freezes the machine
  for good.
- `sbsa_gwdt.action=1` panics at the watchdog's first stage, 30 s without a
  feed. The second stage resets the machine 30 s after that.
- `softlockup_panic=1` and `nmi_watchdog=panic` (the buddy detector, see
  `aarch64.config`) panic on a stuck CPU, which freezes the machine anyway.

A task stuck in uninterruptible sleep for two minutes is only reported, with
its stack, in the journal and the console record. On a working machine that is
usually slow disk or network I/O, and rebooting would lose the work.

A reset that the firmware, hypervisor or PMIC triggers runs no kernel code. It
leaves only the console log, which now has every message up to the reset.

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

## Snapdragon X Elite and X Plus laptops

A Snapdragon X laptop whose vendor firmware already carries a device tree
needs none of the above. Its firmware boots Linux through its own description
of the board, so the kernel reaches it through `efi` (ACPI plus that device
tree) with no DTB for Tilo to embed, and every firmware the SoC asks for
(`qcom/x1e80100/*.mbn`, the ADSP and CDSP blobs under
`qcom/x1e80100/<VENDOR>/<MODEL>/`, the WCN7850's `ath12k` files,
`qcom/gen70500_sqe.fw` and `qcom/gen70500_gmu.bin`) comes from Fedora's
`qcom-firmware`, `atheros-firmware` and `linux-firmware` packages. That is the
shape of the Yoga Slim 7x, the ThinkPad T14s Gen 6, the Surface Laptop 7 and
Pro 11, the Dell XPS 13 9345 and Latitude 7455, the HP OmniBook X 14 and
EliteBook Ultra G1q, the ASUS Vivobook S 15 and Zenbook A14, the Samsung
Galaxy Book4 Edge and the Acer Swift 14 AI.

`x1e80100-generic` is that profile: `qcom_snapdragon` set, `devicetree`
`null`, no board-local firmware, so it needs no OCI firmware artifact and no
extracted Windows drivers. It carries what a board-specific profile does
otherwise:

- the Adreno display firmware both generation names in `system_firmware`, so
the GPU has it before an encrypted root is open;
- the Wi-Fi drivers and firmware the radio needs before switch-root, for
sibling unlock;
- the Lenovo 83ED's crash arguments: the SBSA watchdog fed by systemd, a
panic on the first watchdog stage and on a stuck CPU, and
`systemd.tpm2_wait=0` for Pluton-backed boards.

Its `crash_record` is `efi-variable`: without a device tree of its own the
board reserves no ramoops region, so the kernel's record of a panic goes to
firmware variables, which wear. A machine installed with this profile moves
to its own board profile as soon as it has been inspected on hardware; until
then, a reset on it leaves less behind than on the 83ED.

The battery, the audio DSP topology and the internal keyboard are the halves
this profile cannot supply: the DSP firmware the kernel is told to load,
the `alsa-ucm-conf` machine block that gives the sound card a sink, and
libinput's internal-keyboard quirk all come from a per-model profile
(`internal_keyboards`, see the Lenovo section) once that laptop has been
booted and its firmware names read. A board-model profile is a copy of this
one with those facts filled in, so adding laptops is not a code change.

Neither recipe branches on a profile's id for this: `os/Containerfile` and
`os/Containerfile.board` install `qcom-firmware` when the selected profile
declares `qcom_snapdragon`.

```sh
TILO_HARDWARE_PROFILE=x1e80100-generic TILO_TARGET_IMGREF=<immutable ref> ./os/build.sh
```

What this profile proves is that the image carries the firmware and drivers
those boards are documented to ask for. It does not prove that any one of
them boots: a device tree the firmware supplies, a board-specific audio
topology and a keyboard quirk are physical-device facts, and the board
sections above describe how each is confirmed.

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

The Lenovo board's release is on t (`publish-os-release-lenovo-83ed`, dispatched on stable or a
release tag; `os-board-lenovo-83ed` builds the unsigned dev board image). Its firmware is the
reviewed OCI artifact `TILO_LENOVO_FIRMWARE_REF` with its independently pinned
`TILO_LENOVO_FIRMWARE_MANIFEST_SHA256` (`os/publication.env`): a native Linux OCI image
containing only `/firmware/`, the manifest and exactly the files named by the selected hardware
definition, packaged with a `FROM scratch` / `COPY` image. The release image's layers that read
the hardware profile are built again with it on a native ARM64 node, with those inert files
imported; it is independently rebuilt, its composefs/UKI seal is closed by installing it to a
disposable disk, its target prediction is signed, and the signed ARM64 image is published at
`code.tilo.so/tilo/tilo:lenovo-83ed-<full-main-commit>` and `lenovo-83ed-gen<N>`, with
`lenovo-83ed-latest` moved to it. The image binds that exact immutable tag as its update
reference; installed boards find their successor through `lenovo-83ed-latest`
(docs/security/board-successor-updates.md). Its SLSA provenance binds the board, its hardware
definition, the firmware artifact and manifest digest. Obtain the final `repo@sha256:...` digest
from the registry tag and verify its Cosign signature and provenance before a physical install.

It does not join the generic multiarch `latest`/`genN` release series. QEMU's `virt` machine
cannot boot the Lenovo device tree its UKI carries, so the release validates the disk install
and the signed image without claiming a physical boot; the generic ARM64 image runs the full VM
desktop gate. The first laptop boot and device drivers still require on-device acceptance.

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
and turns its power saving off
(`os/etc/NetworkManager/conf.d/10-tilo-brcmfmac.conf`): the driver drops the
link when the address is changed, and while scanning with power saving on.
The onboard Ethernet PHY drops the cable after idle while Energy Efficient
Ethernet is on, so a NetworkManager
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

The board's debug UART is `ttyS2` at 1500000 baud (the device tree's
`stdout-path`). The image's generic `console=` arguments override that
path, so the profile names it in `kernel_args` as `console=ttyS2,1500000n8`,
with `earlycon` so output starts before the UART driver binds.

The disk puts a 15 MiB unformatted hole at the 1 MiB alignment so the ESP starts at LBA 32768. `idbloader.img` sits in the unpartitioned sectors before that hole, and `u-boot.itb` sits inside the hole. `os/dev/build-orangepi-uboot.sh`
builds mainline U-Boot for `orangepi-3b-rk3566` and
`os/dev/install-rockchip-loader.py` writes `idbloader.img` at LBA 64 and
`u-boot.itb` at LBA 16384. The mask ROM loads that U-Boot, which EFI-boots the
UKI. The first partition must start at or after LBA 32768; a closer partition
is refused before any loader byte is written.

The main development ARM lane builds an unsigned, unvalidated Orange Pi disk
candidate. It is not a release or an accepted device update.

A signed source-pinned board image uses Build / OS's explicit main-branch
`workflow_dispatch` with `release=true` and `hardware_profile=orangepi-3b-v2-1`.
Leave Lenovo firmware inputs empty: Orange Pi firmware and loader inputs are
checksum-pinned in the repository. The native release ARM lane independently
rebuilds, seals and disk-installs the board image, then uses the existing release
signer, SBOM, freshness and provenance checks. Promotion verifies the exact
source, aarch64 profile and hardware-definition hash before publishing
`code.tilo.so/tilo/tilo:orangepi-3b-v2-1-<full-source-commit>` by preserved digest.
Registry/signing access failures stop the lane; no development trust is accepted.
Orange Pi uses the ordinary public Tilo image repository with repository-pinned
firmware. The owner-visibility restriction applies to independently imported Lenovo
firmware; selecting Orange Pi cannot admit those Lenovo firmware inputs.

This is a generation-1 immutable board reference, outside generic `latest` /
`genN`. It neither changes the generic rollback floor nor automatically updates
an installed board. A board DTB cannot boot QEMU virt: disk-install/reproduction
checks are not physical boot qualification. Before activating it on hardware,
verify the existing device's profile/DTB, authenticated update route, runtime
cohort, retained previous deployment and recovery path, then physically test
cold boot, display/input and update/rollback. Preserve device identity and data.
No flash or device update is performed by promotion.

```sh
TILO_BUILD_ARCH=aarch64 TILO_HARDWARE_PROFILE=orangepi-3b-v2-1 \
  TILO_TARGET_IMGREF=registry.tilo.so/tilo/tilo:orangepi-3b-v2-1-<commit> \
  ./os/build.sh --local
```

`TILO_BUILD_ARCH` is only for an x86 builder that emulates aarch64. A native
ARM builder leaves it unset. Local builds do not publish; use the explicit protected release dispatch above
for a signed image.

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

### Wi-Fi before the root is open

Sibling unlock (docs/device-reach.md) can join the owner's unlock Wi-Fi
networks from the initramfs, so the profile carries the radio there. Its
`system_firmware` adds the WCN7850's `ath12k/WCN7850/hw2.0/amss.bin`,
`m3.bin` and `board-2.bin`, and the signed `regulatory.db` and
`regulatory.db.p7s` cfg80211 loads. `board-2.bin` is one container for every
WCN7850 board; it holds the entry ath12k builds for the Yoga's subsystem,
`bus=pci,vendor=17cb,device=1107,subsystem-vendor=17aa,subsystem-device=e0e9,qmi-chip-id=2,qmi-board-id=255`
(the device tree names no calibration variant), and the regulatory data it
also reads. The `ncm865` firmware beside it is for another module and stays out.

`boot_modules` names the drivers udev would otherwise find only after
switch-root: `ath12k` and its PCI front end `ath12k_wifi7`, `mac80211`,
`cfg80211`, the MHI bus and QRTR-over-MHI transport ath12k talks QMI over
(`mhi`, `qrtr`, `qrtr-mhi`, `qmi_helpers`), the WCN7850 power sequencer
(`pwrseq-qcom-wcn`), the PCI power-control driver that asks it to power the
radio before the bus is scanned (`pci-pwrctrl-pwrseq`), and the PCIe PHY
(`phy-qcom-qmp-pcie`). The PCIe controller driver (`CONFIG_PCIE_QCOM`) and
the power-sequencing and PCI power-control cores are built in. Generic images
carry none of this.

`os/scripts/verify-board-initramfs.py` runs after dracut in every board
build and reads the result back with `lsinitrd`: each profile firmware file
and modular boot driver must be there, and when the sibling-unlock unit is,
NetworkManager's Wi-Fi plugin, `wpa_supplicant` and its D-Bus activation file
too. That proves the pieces are present. Only the laptop can show that the
radio powers up, picks that board entry and associates before the root opens.

### Lenovo 83ED Wi-Fi wake policy

The optional version-2 `wifi_wake_on_wlan` field currently admits only numeric
`0` on the exact Lenovo 83ED/aarch64/Yoga Slim 7x definition. The production
profile preparer installs a NetworkManager connection default matching
`driver:ath12k_wifi7_pci` into the selected board image. Generic images and
other boards receive no such policy. No Wi-Fi power-save, autoconnect,
credentials, or unrelated connection settings are changed.

This prevents fresh/default WCN7850 profiles from enabling WoWLAN. It addresses
an observed retained-firmware deep-resume failure (MHI resume followed by
`ESHUTDOWN`), not a proven kernel repair. Physical non-WoW deep resume must
still be tested on the laptop before claiming reconnection is fixed.

As specified by [NetworkManager connection defaults](https://networkmanager.pages.freedesktop.org/NetworkManager/NetworkManager/NetworkManager.conf.html),
an existing explicit `802-11-wireless.wake-on-wlan=0x8` (magic packet) wins over
the generated default. The source did not create that explicit setting; do not
rewrite every saved profile or disable functional wake on unrelated hardware.
For an owner-approved diagnostic correction on this affected device, first
identify its exact active NetworkManager UUID and read only that profile's wake
field plus `iw phy <exact-phy> wowlan show`. Use numeric `0` with
`nmcli connection modify uuid <exact-uuid> 802-11-wireless.wake-on-wlan 0`;
`none` is not accepted by the installed nmcli. Then `iw phy <exact-phy> wowlan
disable` clears the already-applied kernel trigger. Verify both readbacks before
a controlled resume test. These commands require normal device-owner authority;
the generated default neither executes them nor silently migrates explicit
settings. A NetworkManager restart alone cannot repair an already failed MHI
transport.

### Lenovo 83ED keyboard and disable-while-typing

libinput enables a touchpad's disable-while-typing only against a keyboard it
tags internal, and its shipped quirks tag only PS/2 keyboards that way. The
83ED's built-in keyboard is I2C HID (`hid-over-i2c 048D:8987`, bus `0x18`), so
without help libinput never pairs it with the `0488:1054` touchpad and the
Settings toggle has nothing to switch on.

Each `internal_keyboards` entry names the keyboard's bus (`i2c` or `spi`),
uppercase vendor and product ids, exact evdev name and the board's first
device-tree `compatible` string. The preparer writes them, ANDed, into
`/usr/share/libinput/50-tilo-<profile>.quirks` as
`AttrKeyboardIntegration=internal`. Values are literal: no glob characters,
so the section can match only that keyboard on that board. On a board with a
device tree the entry carries that board's `compatible`; on a
firmware-described board it carries `"devicetree_compatible": null` and the
section matches the definition's `dmi` identity instead, through
`MatchDMIModalias=dmi:<sys_vendor>:<product_name>:`. Something must confine
the quirk either way, so an entry on a board with neither is refused.
`/etc/libinput/local-overrides.quirks` stays the administrator's.

On the laptop, `libinput quirks list /dev/input/event<N>` (libinput-utils)
names the attribute for the keyboard node; with stock quirks alone it
prints nothing.

## Handheld PCs

A handheld PC is a firmware-described x86-64 board: it has no device tree,
its panel is mounted in portrait, and its built-in controls are not a
laptop's. Six profiles describe the boards this image targets —
`steamdeck-jupiter` (Steam Deck LCD, Aerith), `steamdeck-galileo` (Steam Deck
OLED, Sephiroth), `rog-ally-rc71l`, `rog-ally-x-rc72l`, `legion-go-8apu1` and
`legion-go-s-8aps1`. Each carries `"devicetree": null`, the DMI identity the
firmware reports, and the panel orientation below.

These are the board's *firmware* description, not a device tree: the kernel's
ACPI tables describe the same devices, so nothing here needs a DTB and the
UKI embeds none. A profile on these boards is what names the model to
model-bound settings — the panel orientation and, for a keyboard whose
`internal_keyboards` entry carries `"devicetree_compatible": null`, the
libinput quirk that pairs it with the touchpad.

### The panel is mounted in portrait

The built-in panel is a portrait panel turned on its side. The kernel has to
be told, because it is the kernel that draws the first console frame and
sets up the scanout before the compositor runs: without it, the splash and
the disk-unlock prompt are drawn sideways and the modesetting driver
advertises the wrong orientation to the compositor, which then turns the
picture a second time. The profile names the connector in `kernel_args`:

```json
"kernel_args": ["video=eDP-1:panel_orientation=right_side_up"]
```

The connector is `eDP-1` because it is the built-in panel, and the value is
the orientation the panel is physically mounted at (its own long edge along
the machine's left). This rides the same sealed-command-line path as every
other profile argument, so it is measured into PCR 11 rather than patched in
at runtime.

The compositor takes the kernel's orientation at face value — a native-portrait
panel therefore needs no second rotation and no copy pass. It only turns the
panel further when the accelerometer reports the device was rotated
(`crates/session/shell/src/compositor/device_rotation.rs`), and that path
already refuses to touch an external monitor.

### Controls

The built-in keyboard and touchscreen are ordinary evdev devices and follow
the same path as a laptop's: libinput, then the compositor, then the focused
surface, with the on-screen keyboard driven by text focus rather than by any
handheld-specific code. The DMI-bound quirk above is what makes the built-in
keyboard pair with the touchpad for disable-while-typing.

The face and shoulder buttons and the sticks are a different matter. The
shell already decodes a physical controller through
`crates/session/shell/src/shell/physical_gamepad.rs`, but that path forwards
the controller to a focused application over the personal-device fabric — it
is for play, not for moving focus around the desktop or dismissing a dialog.
Driving Tilo's own UI from an XI2 gamepad is **not implemented**, and neither
is a handheld-specific power or suspend policy (TDP limits and battery
charge thresholds are firmware- and vendor-specific). Those need a design
decision about which chords the shell owns, so this change does not invent
one.

### Verifying one of these boards

`configure`-time checking is not booting. Before claiming a handheld works,
build its profile image on an x86-64 builder with matching kernel, install
it, and on the device check that the panel is upright from the firmware
handle-off, that the greeter accepts the touchscreen and the built-in
keyboard, and that the on-screen keyboard appears when a text field takes
focus. A board whose DMI strings differ from those in its profile silently
gets none of its model-bound settings, so read `/sys/class/dmi/id/product_name`
on the machine and correct the definition rather than widening it.
