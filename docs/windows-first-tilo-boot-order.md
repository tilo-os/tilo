# Enlist Tilo in the firmware boot menu after the first boot

The Windows direct install creates an active Tilo `Boot####` option and uses
`BootNext` to start it once. Windows stays the persistent default. An option
absent from `BootOrder` may not appear in every firmware's picker, so enlist it
after proving the installed Tilo boots and its desktop is usable.

In Tilo, first check the account login, disk unlock, and
`systemctl status tilo-boot-health.service`. Read the Tilo ESP PARTUUID and
ARM64 loader path from `final-readback.json`, and the four-digit boot number
from the `tilo.windows-direct-armed-boot.v1` handoff receipt. Then run:

```sh
sudo /usr/libexec/tilo-boot-order enlist \
  --esp-partuuid 11111111-2222-3333-4444-555555555555 \
  --boot-id 0100 \
  --loader '\EFI\systemd\systemd-bootaa64.efi'
sudo efibootmgr -v
```

Replace the example GUID and boot number with the receipt values. The helper
requires the named entry to be the currently booted, active Tilo option, bound
to the expected ESP and exact ARM64 loader. It checks systemd-boot's own
loaded-ESP report. It refuses an occupied `BootNext`, duplicate `BootOrder`,
ambiguous/missing options, or a non-Windows first option. It appends Tilo at
the end of the existing `BootOrder`, so Windows remains the default. Repeating
the command is a no-op. If it refuses or readback fails, inspect
`efibootmgr -v` before changing firmware manually. Installation never runs
this command automatically.
