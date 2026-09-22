# Lenovo 83ED: Tilo live-media and firmware checklist

This is the operator checklist for the first physical boot. It is intentionally
limited to checks that can block installation. It does not repartition Windows
or approve the installation itself.

## Before rebooting Windows

- Confirm the Tilo medium has been finalized for `lenovo-83ed`, its raw write
  has been verified, and its capacity is at least the authenticated output
  size. A generic ARM VM medium is not a substitute for the Lenovo medium.
- Keep the completed backup and an independently accessible Windows recovery
  method. BitLocker is enabled on this machine; do not delete its protectors or
  decrypt the volume as a shortcut. If the firmware or partition layout is
  changed, suspend BitLocker immediately before that change and resume it only
  after Windows has booted successfully.
- Windows Fast Startup is already disabled (hibernation is off), so no further
  Fast Startup change is needed. Close Windows and all VMs before booting the
  medium.

## Firmware settings and one-time boot

1. Power off fully. Insert the Tilo USB directly into the laptop.
2. Power on and press **F2** (use **Fn+F2** if function keys are in hotkey
   mode) to open Lenovo UEFI setup. Leave boot mode as **UEFI** and leave the
   existing Windows Boot Manager entry intact.
3. Keep USB boot enabled. For the development medium, disable **Secure Boot**
   temporarily because the local development EFI key is not enrolled in the
   factory trust store. Do not erase factory keys or change TPM/security
   ownership. Re-enable Secure Boot after the development session if Windows
   requires it. Production media needs a separately enrolled, production-trusted
   signer; this development medium cannot prove that path.
4. Save and reboot. Press **F12** (or **Fn+F12**) at the Lenovo logo, select the
   USB's **UEFI** entry, and boot it. Do not select Windows Boot Manager.

If the USB does not appear, power off, reconnect it directly (no hub), verify
USB boot is enabled, and retry F12. Do not change storage-controller mode or
delete existing boot entries.

## Required live checks

The retained 7.2.5 kernel and inspected diagnostic runtime provide the expected
Lenovo device tree, Qualcomm display/input drivers, NetworkManager Wi-Fi
plugin, regulatory data, and WCN7850 firmware/board entry for PCI ID
`17cb:1107` with Lenovo subsystem `17aa:e0e9`. The finalized native medium must
be inspected to confirm it retained those inputs. Even then they are static
prerequisites, not evidence that the devices work together. Complete this
short runbook on the laptop before selecting an install target. It writes
diagnostics only to `/run`, which is the live system's volatile memory.

### 1. Reach a root terminal

First confirm that the graphical installer appears and the built-in keyboard
can type. Press **Ctrl+Alt+F2**, choose **5**, and open the root shell. If the
built-in keyboard is unusable, connect a USB keyboard and repeat; this is an
acceptable bring-up fallback but must be recorded as an internal-keyboard
failure. Start a volatile transcript:

```bash
mkdir -p /run/tilo-live-audit
exec > >(tee -a /run/tilo-live-audit/console.txt) 2>&1
date -Is
uname -a
tr '\0' '\n' </proc/device-tree/compatible
cat /proc/device-tree/model
cat /usr/lib/tilo/hardware-profile.json
sha256sum "/usr/lib/modules/$(uname -r)/dtb/qcom/x1e80100-lenovo-yoga-slim7x.dtb"
find /usr/lib/firmware/qcom/x1e80100/LENOVO/83ED -maxdepth 1 -type f \
  -printf '%f %s bytes\n' | sort
```

The compatible/model output must identify the Lenovo Yoga Slim 7x rather than
a generic VM. The hardware profile must name `lenovo-83ed`, bind the running
kernel's DTB, and list all six reviewed Qualcomm firmware files. A usable tty
and root shell prove the recovery path even if the graphical session later
fails. Do not mount, format, repair, or install from this shell.

### 2. Display and GPU

Run:

```bash
for card in /sys/class/drm/card[0-9]*; do
  [ -e "$card" ] || continue
  printf '%s driver=%s\n' "${card##*/}" \
    "$(basename "$(readlink -f "$card/device/driver")")"
done
for output in /sys/class/drm/card[0-9]*-*; do
  [ -r "$output/status" ] || continue
  printf '%s status=%s\n' "${output##*/}" "$(cat "$output/status")"
done
journalctl -b -k --no-pager | grep -Ei \
  'drm|msm|dpu|adreno|gpu|display|simpledrm|qcom.*firmware|firmware.*qcom'
```

Pass requires the internal panel to be connected, the graphical desktop and
installer to render without corruption, and pointer/keyboard interaction to
remain responsive. An `msm` DRM binding is the expected accelerated path.
`simpledrm` is useful as a firmware-framebuffer fallback for the tty, but by
itself does not pass the graphical-desktop/GPU check. If the desktop is blank,
use the tty to retain `/run/tilo-live-audit/console.txt` and the kernel journal;
do not install. An external USB-C display can aid diagnosis but does not replace
proof of the internal panel.

### 3. Keyboard and touchpad

Inventory kernel input devices without reading keystrokes into a file:

```bash
cat /proc/bus/input/devices
for event in /sys/class/input/event*; do
  [ -r "$event/device/name" ] || continue
  printf '%s name=%s driver=%s\n' "${event##*/}" \
    "$(cat "$event/device/name")" \
    "$(basename "$(readlink -f "$event/device/driver")")"
done
journalctl -b -k --no-pager | grep -Ei \
  'QTEC0001|QTEC0002|IDEA4001|i2c.hid|hid.multitouch|keyboard|touchpad|touchscreen'
```

Return with **Ctrl+Alt+F1** (or choose recovery option **7**). Pass the keyboard
by opening global search with **Super+Space**, typing `terminal`, opening it,
and typing a mixed-case line with numbers and punctuation. Pass the touchpad by
moving the pointer, single-clicking, right-clicking, scrolling, and dragging a
window. A USB mouse permits continued bring-up if the touchpad fails, but record
that failure; keyboard-only routes are **Super+Space** for applications and
**Super+Enter** for Terminal.

### 4. Wi-Fi and network fallback

In Terminal, or in the F2 root shell if the desktop network UI is unavailable:

```bash
nmcli -f DEVICE,TYPE,STATE,CONNECTION device status
wifi="$(nmcli -t -f DEVICE,TYPE device status | awk -F: '$2 == "wifi" {print $1; exit}')"
test -n "$wifi" && nmcli radio wifi on
test -n "$wifi" && nmcli device wifi rescan ifname "$wifi"
test -n "$wifi" && nmcli -f IN-USE,SSID,SIGNAL,SECURITY device wifi list ifname "$wifi"
```

Join a test network without putting its password in the transcript or shell
history:

```bash
read -r -p 'SSID: ' SSID
nmcli --ask device wifi connect "$SSID" ifname "$wifi"
nmcli -f GENERAL.STATE,IP4.ADDRESS,IP4.GATEWAY device show "$wifi"
nmcli general connectivity check
getent ahosts chatgpt.com
```

Pass requires an address, DNS resolution, and transferred web content, not
merely an adapter listed as `connected`. If no Wi-Fi interface appears, retain
these diagnostics before rebooting:

```bash
for device in /sys/bus/pci/devices/*; do
  [ "$(cat "$device/vendor" 2>/dev/null)" = 0x17cb ] || continue
  printf '%s vendor=%s device=%s subsystem=%s:%s driver=%s\n' \
    "${device##*/}" "$(cat "$device/vendor")" "$(cat "$device/device")" \
    "$(cat "$device/subsystem_vendor")" "$(cat "$device/subsystem_device")" \
    "$(basename "$(readlink -f "$device/driver")")"
done
journalctl -b -k --no-pager | grep -Ei 'ath12k|wcn7850|mhi|17cb|firmware'
```

For the first iteration, USB-C Ethernet or Android USB tethering is an
acceptable route to a browser: connect it, rerun `nmcli device status`, and
confirm connectivity and DNS. This fallback allows live debugging but does not
turn a failed internal Wi-Fi check into a pass.

### 5. Desktop, browser and ChatGPT

From the desktop, press **Super+Enter** and confirm Terminal opens and accepts
commands with normal interactive latency. Then press **Super+Space**, type
`browser`, open **Browser**, and browse first to `https://example.com` to prove
ordinary HTTPS rendering. Browse to `https://chatgpt.com`, confirm the page
renders and accepts text in its prompt field. Authentication is not required
for hardware acceptance; an existing session may be used if present.

Pass requires visible page content and working keyboard and pointer input.
Starting a process or resolving DNS alone is insufficient. If the Tilo browser
fails, retain its user journal from the root tty:

```bash
journalctl -b --no-pager | grep -Ei 'browser|webkit|cef|shell|greetd|wayland|drm'
```

An independently packaged ARM64 browser may be used only as a diagnostic
fallback. It can prove the graphics/network route but does not pass the shipped
Tilo Browser check.

### 6. Storage identity (read-only)

Finally, from the root tty run only read-only inventory commands:

```bash
lsblk -e7 -o NAME,PATH,MODEL,SERIAL,TRAN,SIZE,TYPE,FSTYPE,PARTLABEL,PARTUUID,MOUNTPOINTS
findmnt -rn -o SOURCE,TARGET,FSTYPE
```

Before selecting an install target, record pass/fail for these five gates:

- internal display uses the accelerated DRM path and renders the desktop and
  installer UI;
- built-in keyboard types and the touchpad moves/clicks (record any USB input
  fallback separately);
- the internal Samsung NVMe is visible, including the Windows EFI, Windows C:,
  and recovery partitions;
- the installer USB remains visible and readable;
- internal Wi-Fi associates and transfers data, or a documented temporary
  wired/tether route reaches the browser while the Wi-Fi failure is retained.

Audio, Bluetooth, battery reporting, and suspend/resume are useful follow-up
checks but are not installation gates for this first bring-up. Record failures
as issues and continue only if the five required checks pass.

## Before the irreversible install action

- Return to Windows for the separately authorized C: shrink if no suitable
  unallocated extent exists. Reboot the medium afterward and reread the complete
  partition table.
- In the installer, select Windows-preserving alongside mode and verify the
  disk model, existing Windows partitions, new Tilo extent, encryption mode,
  and recovery-material destination on the summary screen.
- Stop if the installer proposes deleting or formatting any Windows EFI, MSR,
  C:, or recovery partition. Installation requires a separate reviewed Tilo
  extent; the live boot itself does not.

After installation, remove the USB, cold-boot Tilo, verify the desktop and
browser, then boot Windows once and confirm its normal entry and BitLocker
state. Restore the intended Secure Boot setting only after the development
boot evidence has been recorded.
