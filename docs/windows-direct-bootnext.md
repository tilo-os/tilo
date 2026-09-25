# Windows ARM64 final boot handoff

After `copy-windows-direct-final-install.py execute` has produced a verified
`final-readback.json`, run the elevated, native Windows ARM64 `imager` executable:

```powershell
imager.exe --tilo-final-bootnext --readback C:\path\to\final-readback.json
```

The command prints a `tilo.windows-direct-armed-boot.v1` JSON receipt. Save
stdout with a file redirect if a durable record is needed. It does **not**
reboot. The next normal firmware boot uses Tilo once via `BootNext`; the
Windows `BootOrder` remains unchanged.

The helper requires the copier's verified final readback, checks the Windows
disk number, GPT disk GUID and size against both Storage metadata and the
on-disk GPT, and identifies the installed ESP by its final PARTUUID and ESP
type. It accepts only the ARM64 systemd-boot path selected by the copier. It
reads that loader from the physical ESP and compares its SHA-256 with the
receipt before any EFI variable write and once more after creating the new
`Boot####` entry. The fallback `\EFI\BOOT\BOOTAA64.EFI` is accepted only if
the primary `\EFI\systemd\systemd-bootaa64.efi` is absent. CI image and VM
proof digests and the copier's physical readback flag are required.

If `BootNext` already exists, the helper stops without replacing it. It
allocates an unused `Boot####` at or above `0100`, writes and reads it back,
then writes and reads back `BootNext`. It never modifies `BootOrder`,
`BootCurrent`, or another boot option. A failure before `BootNext` is written
removes its own new boot option if the entry still matches. If an error occurs
after writing `BootNext`, inspect the printed error and firmware state before
retrying; the helper does not overwrite an occupied `BootNext`.

The file-backed firmware tests exercise option encoding, ordered writes,
readback, occupied `BootNext`, loader mismatch, and disk identity mismatch.
No physical firmware variables or partitions are changed by those tests.
