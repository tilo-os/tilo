# Snapdragon X laptops: which generation, and what a new one needs

Tilo's first ARM64 board is a Snapdragon X Elite laptop, the Lenovo Yoga Slim
7x (`lenovo-83ed`). The second generation — Snapdragon X2 Elite and X2 Plus,
the 2nd-generation Oryon cores — is coming on new machines, and a person with
one has to be told plainly whether this release runs on their computer.

This page answers three questions and says where each answer comes from:

1. Which generation a machine is (hardware and firmware only, no DT needed).
2. What a Tilo image has to carry before a 2nd-generation machine can boot.
3. Which inputs do not exist upstream yet, so no image can carry them, and how
   to notice when they land.

It names no SoC ids, DTB paths or Adreno generation numbers of unreleased
parts: an id guessed before Qualcomm publishes it is worse than none, because
a profile keyed on a guess fails in a way that looks like a broken laptop. The
rules below are what stays true across whatever ids arrive.

## 1. Identifying the machine, before a device tree exists

A supported-gen-1 laptop runs a board profile (`os/hardware/profiles/*.json`):
an exact model, one architecture, the kernel's DTB for it. Nothing generic is
a substitute — `os/hardware/README.md` says why a generic image cannot stand in
for a board image.

Everything a person can see about their machine before the kernel knows what
it is comes from firmware, in this order:

| Evidence | Where | Says |
|---|---|---|
| CPU model | `/proc/cpuinfo`, "model name" on ARM64 (`MIDR`/`ID_*` registers) | the core generation: 2nd-gen Oryon parts do not report a 1st-gen model |
| Compatible list | `/proc/device-tree/compatible`, one NUL-separated line per string, most specific first | nothing until a board is described; the generic Qualcomm entry is all firmware offers before then |
| Model string | `/proc/device-tree/model` | the machine's marketing name, from firmware |
| SMBIOS | `/sys/class/dmi/id/{sys_vendor,product_name,product_family,board_name}` | the same identity Windows uses; the anchor for a firmware-only match |
| Display controller | `/sys/class/drm/card*/device/driver`, and `modalias` | which SoC generation the display block belongs to |

The rule that follows from this table: **a machine is identified by the identity
its firmware reports, never by which device tree the kernel happened to bind**.
That is what lets a Tilo medium say "this is a Snapdragon X2 machine, Tilo does
not run on it yet" on hardware whose SoC no released kernel describes, instead
of failing somewhere in the boot with a blank screen.

The same identity is what a board profile is keyed on. A 2nd-generation profile
is a new `id`, a new DTB path and a freshly reviewed firmware set; it is never
an edit to `lenovo-83ed`, whose DTB hash, firmware hashes and libinput quirks
describe one machine. Two generations of laptops do not share a profile, even
when one vendor ships both.

### Where that has to be said

The live medium's welcome is the first thing that runs, and it already has to
refuse unsupported hardware rather than offer an install. Two audiences:

- **The person at the machine.** The welcome names the outcome: their computer's
  model and one sentence saying this release does not run on it, no device
  names, no ids.
- **The person keeping the board list.** A refused medium records the whole
  identity table above (plus the kernel version and the initramfs's DTB list)
  into `/run/tilo-live-audit/`, a volatile directory. Adding a board
  begins with that record; "the installer said no" on its own tells no one what
  to add.

A refusal is not a decision to make twice in one boot: once firmware identity
says the machine is not a registered board, the medium stops before it touches
a disk.

## 2. What a 2nd-generation image needs that a 1st-generation one has

The Lenovo profile's own history is the checklist, because every item on it was
a boot that failed first:

- **The DTB.** `qcom/x1e80100-lenovo-yoga-slim7x.dtb` in the running kernel's
  `dtb/` directory, hashed into `/usr/lib/tilo/hardware-profile.json` and sealed
  into the UKI. A kernel without the board's device tree is not "mostly fine":
  nothing binds.
- **GPU firmware, by the names the kernel asks for.** Current MSM kernels ask
  for `gen70500_sqe.fw`, `gen70500_gmu.bin` and an SoC-specific `*_zap.mbn`;
  older ones asked for `a740_sqe.fw`. The profile carries both generations'
  files because a request the initramfs cannot answer stops the boot before the
  encrypted root is opened. A 2nd-generation Adreno will ask for names no
  released linux-firmware carries yet, and the names it asks for are only
  knowable from a kernel that has driven the block.
- **The Qualcomm Windows-driver firmware set** for display, video, audio and
  compute DSPs (`qcdxkmsuc*`, `qcvss*`, `qcadsp*`, `qccdsp*` and the two
  `*_dtbs.elf`), extracted from the machine's own Windows drivers, staged under
  `os/hardware/firmware/<profile>/` with a hash manifest and never committed.
  The public firmware gap for the 83ED set is described in
  `os/hardware/README.md`; a 2nd-generation set starts with the same
  redistribution question and usually ends with the same answer for the CDSP
  DTBs.
- **Radio firmware and board data.** Wi-Fi and Bluetooth come from the distro's
  firmware packages when the module's calibration entry exists; a new module
  needs its `board-2.bin` entry and, for Bluetooth, the ROM/board-specific NVM
  names the controller asks for. These are selected by QMI chip/board ids the
  machine reports at runtime, not by model name, so they are recorded on a
  first boot rather than predicted.
- **libinput quirks for the built-in keyboard**, so disable-while-typing works,
  keyed on the keyboard's own bus/vendor/product/name and the board compatible.
- **Sound**, which arrives last on these machines: the card's driver name and
  topology, the ALSA UCM profile for it, and the DSP firmware the card loads.

`os/tests/board_audio_contract.sh` is the model for the last item: a script
that reads the built image and fails when a board's support is half-installed.
Each item above that can be checked against a built image belongs in that
shape, not in a checklist someone remembers.

## 3. What is missing upstream, and how to notice it arriving

Today no released mainline kernel describes a 2nd-generation SoC: there are no
board DTBs, so no kernel in an image has a device tree to bind, and there is
nothing to put in `os/hardware/profiles/`. Mesa's Turnip must also know the new
Adreno generation before the desktop renders accelerated. Both are outside this
repository, so the triggers below are what this repository watches for:

| Input | Where it lands | What it means here |
|---|---|---|
| SoC and board device trees (`arch/arm64/boot/dts/qcom/`) | `linux-arm-msm`, then a mainline tag, then Fedora's kernel | a DTB path exists for a profile; check the Fedora kernel carries it before naming it |
| IRQ, pinctrl, clock and interconnect changes the new SoC needs | same, as series | the patches this repository carries for the 1st generation may need a 2nd-generation equivalent — `os/kernel/patches.toml` is where that is recorded |
| Adreno generation support in Turnip | Mesa releases, then Fedora's `mesa-vulkan-drivers` | the launcher pins whatever ICD claims the render node (`crates/packaging/launcher/src/gpu.rs`), so no Tilo change is needed once the ICD reports the node |
| firmware names the kernel requests | linux-firmware, then Fedora's `qcom-firmware` | decide per file whether the distro package or a staged local set supplies it |
| A bootloader DMI/ACPI→DT mapping table | the bootloader that ships with the board | if a 2nd-generation machine needs one to describe itself, that bootloader becomes a board input like the Orange Pi's U-Boot, not a Tilo change |

The order matters: a board profile can be reviewed only after its DTB is in a
kernel Tilo ships, and its firmware set only after that kernel has told someone
which files it asks for. A profile written ahead of the device tree is a guess
in the shape of a reviewed input.

## 4. The first boot of an unregistered generation

When a 2nd-generation laptop first runs a Tilo medium, record before changing
anything:

```bash
mkdir -p /run/tilo-live-audit
{
  date -Is
  uname -a
  tr '\0' '\n' </proc/device-tree/compatible
  cat /proc/device-tree/model
  grep -s 'model name' /proc/cpuinfo | head -1
  for id in sys_vendor product_name product_family board_name bios_version; do
    printf '%s=%s\n' "$id" "$(cat "/sys/class/dmi/id/$id" 2>/dev/null)"
  done
  for card in /sys/class/drm/card[0-9]*; do
    [ -e "$card/device" ] || continue
    printf '%s %s %s\n' "${card##*/}" \
      "$(basename "$(readlink -f "$card/device/driver")")" \
      "$(cat "$card/device/modalias" 2>/dev/null)"
  done
  ls "/usr/lib/modules/$(uname -r)/dtb/qcom" 2>/dev/null
} | tee /run/tilo-live-audit/first-boot.txt
```

The DTB listing is the kernel's answer to "is this machine described at all";
the compatible and SMBIOS lines are what a profile would be keyed on; the DRM
drivers name the display generation. Do not install from that shell. The
transcript is the input to the next step, which is waiting for the device tree.
