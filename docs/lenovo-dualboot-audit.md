# Lenovo 83ED dual-boot evidence matrix

This audit records the boundary between verified installer design and the
hardware facts that only a clean physical boot can establish. It is not an
installation authorization.

| Area | Established evidence | Still required for a claim of readiness |
| --- | --- | --- |
| Lenovo image | The board profile binds the Lenovo 83ED device tree and selected firmware inputs by hash. | A finalized signed Lenovo medium, its raw-media identity, and inspection of the actual image's device tree and firmware bytes. |
| Installation medium | The verified media writer identifies one external removable device, rechecks it, writes raw bytes and verifies a full readback. | A selected physical USB device, a completed accepted write receipt, and a successful Lenovo UEFI boot. |
| Windows disk | The recorded target layout is GPT with a 260 MiB Windows ESP, MSR, C:, and a Windows recovery partition. The Windows ESP is not a Tilo destination. | A fresh elevated snapshot before the physical session, plus post-install comparison of the Windows disk and ESP identity. |
| Alongside geometry | The installer appends only a dedicated 2 GiB Tilo ESP and Tilo root inside one reviewed unallocated extent. It does not resize or format existing partitions. | Final payload fit, retained Windows headroom, and a Windows-created unallocated extent sized for the admitted final requirement. |
| Windows preservation | Static invariants cover stable disk/partition revalidation and reject occupied roots or unsuitable ESPs. Disposable fixtures use preservation sentinels. | A physical install that adds only the reviewed Tilo pair, then cold-boots both Tilo and Windows. |
| BitLocker | The last elevated observation reported C: fully encrypted with protection enabled. | An explicit selected policy, independently available recovery material, and a fresh elevated status. The strict preflight requires full decryption and protection Off. |
| Windows recovery | A recovery partition is present. | Fresh `reagentc /info`, recovery material outside the internal disk, and a successful Windows boot after the UEFI/ESP change. |
| Lenovo display and input | Generic ARM VM runs cover a virtual graphics/input stack only. | The live Lenovo session reaches the usable installer, renders correctly, accepts keyboard input, and completes touchpad pointer/click checks. |
| Storage and USB | Windows has recorded the internal NVMe layout. | Live Linux sees that same NVMe and its Windows partitions, while the installation USB remains present through the session. |
| Wi-Fi | The expected Windows FastConnect adapter and Linux firmware/package inputs are known. | The live Lenovo session detects the adapter, joins the intended network, and transfers data. |
| Recovery and rollback | The image carries designed recovery and rollback paths; static checks cover their required components. | An installer-produced Lenovo target completes the defined cold-boot and recovery/rollback acceptance on hardware. |

## Current hard gates

The historical Windows observation had no unallocated extent, and the last
elevated supported-shrink result was only 7.54 GiB. That is below the old
34 GiB geometry floor even before final payload fit and Windows headroom are
known. Internal free space in C: is neither an unallocated extent nor a
guarantee that Windows can shrink by that amount.

Before the physical alongside procedure, use the
[Windows dual-boot preflight](lenovo-dualboot-preflight.md). It is deliberately
strict: it exits nonzero unless BitLocker is fully off, a safe external medium
is identified, the required unallocated extent exists, and the recorded Windows
ESP identity remains unchanged. The baseline and receipt are operator evidence;
they do not replace the installer backend's own block-device checks.

Generic QEMU acceptance remains necessary but cannot prove Qualcomm display,
touchpad, Wi-Fi, UEFI behavior, real Windows boot, or WinRE. A clean physical
boot is the first evidence that those layers work together.
