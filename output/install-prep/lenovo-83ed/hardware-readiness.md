# Lenovo 83ED live-boot hardware audit

Audit date: 2026-09-22. This is a read-only inventory of the Windows-on-ARM
host and the locally retained Tilo 7.2.5 ARM kernel artifact. It is static
evidence, not a substitute for booting the installation medium on the laptop.
Serial numbers and the machine UUID are intentionally omitted.

## Host identity

- Lenovo Yoga Slim 7 14Q8X9, machine type `83ED`
- Qualcomm Snapdragon X Elite X1E-78-100, 12 cores, ARM64
- 32 GiB RAM
- Firmware `NHCN62WW`, dated 2025-12-02

## Critical devices

| Function | Windows identity | Linux path | Static confidence | Live proof still required |
| --- | --- | --- | --- | --- |
| Internal display/GPU | Adreno X1-85, `ACPI\QCOM0C36`, panel `MONITOR\LEN8AC2`, 2944x1840 | `drm/msm`; DTB declares `qcom,adreno-43050c01`, GMU `x185.1`, and panel `samsung,atna45dc02` | High | Panel lights, correct mode, no corruption; accelerated renderer rather than software rendering |
| Keyboard | `ACPI\QTEC0001`, compatible `PNP0C50` | `i2c-hid`; DTB declares `keyboard@3a` as `hid-over-i2c` | High | Keys work at disk unlock and in the desktop |
| Touchpad | `ACPI\QTEC0002`, compatible `PNP0C50` | `i2c-hid` plus `hid-multitouch`; DTB declares `touchpad@2c` | High | Pointer, click, and basic scrolling work |
| Wi-Fi | FastConnect 7800, PCI `17cb:1107`, subsystem `17aa:e0e9` | `ath12k`; DTB declares `wifi@0` with `pci17cb,1107` | Medium-high | Firmware and board-data load, interface appears, scans and associates |
| Bluetooth | FastConnect 7800 UART, `ACPI\QCOM0C6B`, `QCA_SHB\UART_H4_HMT` | `hci_qca`/`btqca`; DTB declares `qcom,wcn7850-bt` | Medium | Controller firmware/NVM selection and enumeration; optional for initial install |
| Internal storage | Samsung MZAL81T0HDLB, controller PCI `144d:a80d`, 1 TB | generic NVMe; DTB has the laptop NVMe regulator | High | Installer sees the disk without I/O or reset errors |
| USB | host `QCOM0C8C`; several Qualcomm/Synopsys dual-role controllers | `xhci-plat-hcd`, DWC3, Qualcomm USB PHY/type-C drivers; DTB describes three USB-C connectors | Medium-high | Installation USB boots and remains stable |

The retained `tilo-kernel-core-7.2.5-1.aarch64.rpm` contains
`qcom/x1e80100-lenovo-yoga-slim7x.dtb`. Its resolved configuration includes
`CONFIG_DRM_MSM=m`, `CONFIG_ATH12K=m`, `CONFIG_BT_HCIUART=m`,
`CONFIG_BT_QCA=m`, `CONFIG_I2C_HID=y`, `CONFIG_HID_MULTITOUCH=m`, and built-in
NVMe support. The RPM also contains the corresponding MSM DRM, ath12k,
Bluetooth, I2C-HID and multitouch modules.

## Firmware assessment

The exact six board-profile blobs required by the checked-in `lenovo-83ed`
profile are available in the Windows Driver Store:

- `qcdxkmsuc8380.mbn`
- `qcvss8380.mbn`
- `qcadsp8380.mbn`
- `adsp_dtbs.elf`
- `qccdsp8380.mbn`
- `cdsp_dtbs.elf`

Windows identifies Wi-Fi subsystem `17aa:e0e9` as the Lenovo NCM825A variant
and selects `bdwlan_wcn785x_2p0_ncm825.elf`. The Linux ath12k driver instead
selects an entry from `board-2.bin` using QMI chip and board IDs. The PCI ID is
not sufficient by itself, so the actual candidate image was inspected. Its
WCN7850 `board-2.bin` contains an exact entry for PCI `17cb:1107`, subsystem
`17aa:e0e9`, QMI chip ID 2, and QMI board ID 255, plus Lenovo-specific variant
entries. The live-boot log must still confirm the controller reports compatible
QMI IDs and completes firmware start.

The Driver Store also contains `hmtbtfw20.tlv`, `hmtnv20.bin`, and several
board-specific `hmtnv20.b*` files. The controller response during live boot
determines which Bluetooth NVM file Linux requests.

The exact local VM candidate also contains `mesa-dri-drivers`, Mesa EGL/GL and
Vulkan drivers, Xwayland, `NetworkManager-wifi`, `wireless-regdb`,
`linux-firmware`, and `atheros-firmware`. It contains WCN7850 `amss.bin`,
`m3.bin`, and `board-2.bin`, plus `hmtbtfw20.tlv`, `hmtnv20.bin`, and
`hmtnv20.b10f`. This proves the userspace and generic wireless payload is
present; it does not prove that the native kernel can initialize the devices.

## Minimum live-media acceptance

Before installation, the medium is acceptable when all of the following hold:

1. The internal panel reaches a readable console or graphical session at its
   expected orientation and a usable mode.
2. The internal keyboard works, including at the encrypted-volume prompt, and
   the touchpad provides pointer movement and click.
3. The Samsung NVMe and the installation USB remain visible without storage or
   USB reset errors.
4. Wi-Fi firmware loads, NetworkManager lists the adapter, and it can associate
   with an access point and reach a web page.
5. A terminal and browser can be opened. Record `dmesg`, `lspci -nnk`,
   `/sys/kernel/debug/dri/0/state` when available, `glxinfo -B` or
   `eglinfo`, `libinput list-devices`, `nmcli device`, and the firmware load
   messages before installing.

Bluetooth, audio, camera, battery reporting, and suspend/resume are useful
follow-up acceptance checks but do not gate the first dual-boot installation.

## Current gap

The VM candidate exercises architecture-independent installation and userspace
behavior. Native media must additionally be built with the `lenovo-83ed`
profile so that the UKI embeds the exact DTB and the image/initramfs contain the
profile firmware. A generic ARM VM boot cannot prove these physical-device
paths.
