# ARM64 first-boot iteration

The first boot has two jobs that should run in parallel:

1. boot the exact CI-published image as a black-box acceptance test; and
2. isolate a failure on a disposable copy without rebuilding the complete OS.

The CI image remains the release candidate. A locally modified disk is a diagnostic
fixture only. Once a small replacement proves a fix, put the same change in its
source layer and let CI publish the next candidate. This keeps the proof honest
without making every diagnostic attempt wait for image assembly.

## Keep these artifacts between attempts

Keep one verified OCI pull, one assembled raw disk, one untouched UEFI variable
store, and the serial log from every boot. Make disposable disk/variable-store
copies for experiments. Do not pull or unpack the OCI again unless its manifest
digest changed.

Before starting QEMU, reject broken disk assembly in seconds:

```bash
sudo unshare --mount --propagation private -- \
  python3 os/dev/verify-installed-development-disk.py \
  --preflight-disk /absolute/path/to/disk.raw
```

On the Windows ARM64 host, start the disk and follow its serial evidence with:

```powershell
pwsh -NoProfile -File .\tools\launch-arm64-disk.ps1 -DiskPath <disk.raw>
Get-Content "$env:LOCALAPPDATA\Tilo\qemu-arm64\serial.log" -Wait
```

Reset the variable store only to distinguish persistent UEFI state from disk
content. A normal retry must retain it.

## Fault-isolation matrix

| Last visible stage or signature | Cheapest decisive check | Smallest disposable replacement | Authoritative fix boundary |
| --- | --- | --- | --- |
| Firmware UI; no serial output | Run the disk preflight, then boot once with a fresh development variable store. Inspect the ESP for `EFI/BOOT/BOOTAA64.EFI`, `EFI/systemd/systemd-bootaa64.efi`, the single loader entry, and its selected UKI. | Loader EFI file, loader entry, or variable-store copy. | Loader package/install logic; no application rebuild. |
| systemd-stub splash or `EFI stub` never reaches a kernel message | Read the selected UKI's PE sections and architecture. `verify-installed-development-disk.py --preflight-disk` already requires an ARM64 UKI with the expected sections. | The selected `.efi` UKI on the ESP. | UKI stage only. Reuse the existing kernel, initramfs, and rootfs inputs. |
| Kernel starts, then panics or reports a missing driver before initrd userspace | Keep the full serial log. Compare `.linux`, `.initrd`, `.cmdline`, and optional `.dtb` sections with the published UKI. | One UKI containing the corrected kernel, DTB, or initramfs. | Kernel/initramfs/UKI stage; no desktop or application rebuild. |
| `root=tilo-auto`, `tilo-root-discover`, `sysroot.mount`, or "expected exactly one Tilo root" fails | Inspect the GPT and root type with the disk preflight; run `os/tests/tilo_root_discovery.sh` against the resolver logic; use serial output to distinguish ESP-disk discovery from mount failure. | One UKI, because `tilo-root-generator` and `tilo-root-discover` live in the initramfs. | Initramfs plus UKI only. Disk layout changes require re-running disk assembly, while retaining the OCI pull. |
| UKI starts but reports a composefs digest/deployment/verity mismatch | Run `os/dev/composefs/guest-check.py` in the guest when a shell is reachable. Otherwise compare the UKI `.cmdline` `composefs=` value with the sole `state/deploy/<digest>` directory and selected loader entry offline. | None safely. The root tree and UKI seal are one integrity unit. | Reseal the OCI root and UKI together. Reuse all unchanged OCI/application layers; do not recompile payloads. |
| Root mounted; systemd reaches emergency mode or a required unit fails | Use serial first, then `systemctl --failed`, `systemctl status <unit>`, `journalctl -b -u <unit>`, and `systemd-analyze critical-chain` over SSH or a serial login. | On a disposable disk, place a helper under `/var/lib/tilo/debug/` and an `/etc/systemd/system/<unit>.d/override.conf` that points to it. | The affected rootfs file or unit layer. No kernel, UKI, or application rebuild while diagnosing. The CI candidate must later reseal the root. |
| `greetd.service` fails or no greeter is painted | Check `systemctl status greetd`, `journalctl -b -u greetd`, `/etc/greetd/config.toml`, DRM nodes, and the `cage -> tilo-greeter` process chain. A 2D boot proves firmware/console/greeter; GL is needed for the compositor. | Override `greetd` from `/etc/systemd/system/greetd.service.d/` and run a replacement `cage`, config, or greeter from `/var/lib/tilo/debug/`. | Greeter/config package layer only. |
| Greeter works; login returns to greeter, black-screens, or the desktop exits | Inspect the greetd session log, user journal, `tilo-graphical-session.target`, portal activation environment, and `/usr/libexec/tilo-session`. Run the compositor replacement directly from the authenticated session. | Replacement `tilo-session` or `shell` under `/var/lib/tilo/debug/`, selected by a disposable greetd/session override. | Session/compositor payload layer only. |
| Desktop is visible | Run the guest composefs check, record the greeter and desktop, and retain serial/journal evidence. Then reboot with the same UEFI store to prove selection is persistent. | No replacement. | Candidate is ready for installer-path and hardware acceptance. |

## Evidence gates

Stop at the first failed gate; later failures are usually consequences.

1. **Disk:** the read-only disk preflight passes.
2. **Firmware/loader:** systemd-boot selects the sole expected ARM64 UKI.
3. **Kernel/initramfs:** serial reaches initrd userspace and root discovery.
4. **Root:** `findmnt /` shows composefs and the booted deployment directory
   matches the UKI's `composefs=` argument.
5. **Userspace:** `multi-user.target` completes and required units have useful
   failure logs.
6. **Login:** `greetd.service` is active and the graphical greeter is painted.
7. **Desktop:** authentication starts `tilo-graphical-session.target` and the
   native compositor remains alive.

`os/dev/composefs/guest-check.py` is the post-root proof for UEFI, UKI,
composefs deployment identity, and measured command line. `os/dev/vm.sh` and
`os/dev/record-boot.py` remain the automated greeter/login/desktop evidence
tools on hosts where their QEMU backend is available.

## Rebuild rule

Do not rebuild the complete image to learn which gate failed. Use a stage-local
replacement on a disposable disk, prove the repair at that gate, then rebuild
only the owning source layer. The only unavoidable coupled rebuild is a change
to the immutable composefs root: its digest is sealed into the UKI, so the new
root and UKI must be published together.
