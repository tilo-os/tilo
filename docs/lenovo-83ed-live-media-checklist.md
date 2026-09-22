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

Before selecting an install target, record pass/fail for these five items in
the live session:

- display/GPU renders the desktop and installer UI;
- built-in keyboard types and the touchpad moves/clicks;
- the internal NVMe is visible, including the Windows EFI, Windows C:, and
  recovery partitions;
- the installer USB remains visible and readable;
- Wi-Fi associates and transfers a small test file.

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
