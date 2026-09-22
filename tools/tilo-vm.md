# Local ARM VM iteration

`tilo-vm` turns the existing, artifact-specific PowerShell launchers into a
repeatable local debugging loop. It does not copy or rebuild disks. Every
attempt gets a new control directory containing the exact input metadata,
launcher output, assigned ports, append-only events, and captures.

Run a cheap validation before starting QEMU:

```powershell
.\tools\tilo-vm.ps1 --runs-dir C:\tilo-build\runs preflight `
  --launcher .\.qemu\arm64-local\launch-stock-hybrid-gpu-snapshot.ps1 `
  --disk C:\tilo-build\tilo-install-native-test.raw `
  --target C:\tilo-build\tilo-install-target.raw `
  --source-overlay C:\tilo-build\tilo-install-source-cache.qcow2
```

Replace `preflight` with `start` to launch the same inputs. The CLI allocates
QMP and serial ports, passes a fresh state directory to the launcher, runs the
launcher's own `-CheckOnly` contract first, and then records `run.json`. The
receipt is written once; later operations append to `events.jsonl`.

Use the returned run ID for the tight debugging loop:

```powershell
.\tools\tilo-vm.ps1 status 20260922T201500Z-a1b2c3d4
.\tools\tilo-vm.ps1 capture 20260922T201500Z-a1b2c3d4
.\tools\tilo-vm.ps1 stop 20260922T201500Z-a1b2c3d4
```

`capture` asks QEMU for a framebuffer dump and saves a bounded serial tail.
`stop` uses QMP's clean `quit`; `--force` is an explicit fallback limited to
the PID in that run's receipt.

Launcher-specific switches pass through without embedding another launcher in
the CLI:

```powershell
.\tools\tilo-vm.ps1 start --launcher .\path\to\launcher.ps1 `
  --launcher-argument DebugBoot --launcher-argument CpuCount=12
```

The standard parameter names are `StateDirectory`, `QmpPort`, and
`SerialPort`. Override them with `--state-parameter`, `--qmp-parameter`, and
`--serial-parameter`; pass an empty value when a launcher does not implement
one. Disk-like inputs have dedicated flags so their size and modification time
are included in the receipt. The launcher itself remains responsible for
strong artifact hashes and boot contracts.

Keep run directories small and disposable. Large raw disks, qcow2 overlays,
kernels, and initramfs files stay outside them and are referenced by absolute
path. This makes a retry create kilobytes of state instead of another disk
copy.
