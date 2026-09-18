# Lenovo 83ED development installation acceptance

This checklist scopes the first development installation alongside Windows on
the Lenovo 83ED. It does not replace the general release acceptance contract or
claim that the current image is ready.

## Preparation gates

- Produce the complete native ARM image with the Lenovo profile, matching kernel
  device tree and hash-verified firmware.
- Verify the final image, boot-chain signatures, filesystem seal and installation
  medium. Exercise the actual installer and Windows-preserving layout in a
  disposable environment before using the internal disk.
- Prepare a Windows backup and recovery path, a reviewed space allocation, and
  an explicit plan for Windows encryption and firmware trust.
- Keep installation, partition changes, media overwrite and physical firmware
  changes separate from preparation; perform them only when authorized.

## Essential checks in the installation-medium session

Use the intended installation medium on this laptop. No separate, duplicate
live-boot acceptance session is required. Before starting installation, confirm:

- USB/UEFI boot reaches a usable desktop and installer.
- Display rendering and keyboard/touchpad input support the installation flow.
- Internal storage and existing Windows partitions are correctly identified.
- USB remains usable and networking works through the intended connection.
- The reviewed alongside layout preserves Windows and its recovery/boot path.

Do not record these checks as passed from unit tests, generic VM results or
firmware hashes alone. They require observations from the laptop.

## Deferred hardware checks

Audio, Bluetooth, battery reporting and suspend/resume are not gates for this
first installation. Track their hardware results and fixes in
[issue #49](https://code.podesta.ai/qos/qos/issues/49) once the base system is
usable. Deferral does not imply these functions work.

## Windows encryption choices

Suspending BitLocker protection keeps the volume encrypted but temporarily
disables its normal protector enforcement. This can support planned boot-chain
changes; it is not full decryption or a replacement for a recovery plan.

Turning BitLocker off decrypts the volume. If that option is selected, verify
that decryption has completed before treating encryption as disabled. Retain
the recovery key until then. Neither option replaces a backup or changes the
requirement to preserve Windows during installation.

Choose and perform the encryption change explicitly; preparation must not
silently suspend protection or start decryption.
