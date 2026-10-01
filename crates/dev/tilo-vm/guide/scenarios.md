# Scenarios: boot components end to end

Boot components (the installer, the UKI, the bootloader, the initramfs) only
take effect on a cold boot, so `tilo-vm swap` cannot put them into a running
VM. A scenario boots them in a throwaway VM instead. `tilo-vm swap installer`
points here.

## `tilo-vm scenario install`

```sh
./os/build.sh --local                        # localhost/tilo:latest from this worktree
./os/dev/build-custom-installer.sh           # os/output/installer/qcow2/disk.qcow2
tilo-vm scenario install                     # erase layout, passphrase unlock
tilo-vm scenario install --layout guided-alongside
tilo-vm scenario install --layout prepared-alongside --graphics software
tilo-vm scenario install --dry-run --json    # the plan, nothing booted
```

No slot and no dev VM are involved. The scenario runs its own QEMU VM through
`os/dev/vm.sh` (the headless path the release gate uses; set `TILO_DISTROBOX=""`
when QEMU is on the host itself) in a named cycle, so its disks, TPM and
firmware state exist for this run only.

On Windows ARM64, use the native `tilo-vm.exe` and the WHPX transport:

```powershell
tilo-vm scenario install --medium C:\path\to\installer.qcow2 --layout erase --encrypt passphrase --graphics software
```

For the guarded Windows-direct source workflow, first create a **disposable
file** from the reviewed allocation plan. The helper creates sparse synthetic
Windows partitions; it never reads Windows partition contents into the VM.

```powershell
python os/dev/prepare-windows-direct-source.py template --plan C:\path\to\realized-plan.json --output C:\VMs\tilo-source.raw
tilo-vm scenario install --medium C:\path\to\installer.qcow2 --layout guided-alongside --target-disk C:\VMs\tilo-source.raw --encrypt passphrase --graphics software
```

A Lenovo board image can require firmware and a DTB that QEMU virt does not
provide. For a reviewed Windows source raw, `--skip-target-boot` stops after
the encrypted install and guest proof, then verifies the preserved GPT after
the VM stops. Its checked proof states `first_boot_verified: false`. This
mode is an install write check; a separate generic CI-signed target must run
the full installer and first-boot cycle before physical copying.

The scenario verifies the template manifest before installation, after the
installer powers off, and, unless target boot is explicitly skipped, after
the installed system's first boot. It retains
the external raw file even without `--keep`; it only cleans its own VM cycle.
On Windows it also requires a fresh proof from the guest on a dedicated
virtio-serial port. The scenario checks that the guest installed the image it
booted, that its composefs digest matches the installer, and
that its disk and partition identities match the source raw's verified GPT.
The checked proof is saved as `vm-install-proof-checked.json` in the scenario
artifacts; the private installer serial log is still withheld. The
source-preparation helper must be present in this worktree. This test does
not itself verify the CI signature or authorize writing a physical disk;
the Windows-direct receipt must bind the proof to a signed CI image and the
source ESP's UKI before the physical copy.

`os/dev/vm-windows.ps1` uses `qemu-system-aarch64.exe` with `-accel whpx`
and `-cpu host`. It creates a copy-on-write installer overlay and a sparse
40 GiB QCOW2 target for erase, or attaches the reviewed external sparse raw
target for guided alongside; the medium remains unchanged. Set `TILO_QEMU_SYSTEM`,
`TILO_QEMU_IMG`, `TILO_EFI_CODE`, and `TILO_EFI_VARS` if these are outside the
QEMU installation. The two boots share the target and UEFI variable store.
The virtio console and QMP endpoints bind to localhost TCP ports in the run
plan. Windows supports erase and a supplied guided-alongside raw target with
passphrase unlock and software graphics. Install Tesseract with English data
for first-boot screen recognition (`pacman -S mingw-w64-clang-aarch64-tesseract-ocr
mingw-w64-clang-aarch64-tesseract-data-eng` in the Clang ARM64 MSYS2 shell).
The first boot may show identity setup
instead of sign-in; this is recorded as a screenshot and OCR observation. Its
composefs identity is inferred from the installer proof and the VM's sole
target disk, whereas the sign-in console reports that identity directly.
Built-in alongside fixtures need
Linux filesystem creation tools, and TPM PIN needs a persistent software TPM.
The default `--graphics gl`
must therefore be overridden on Windows.

Phases, each timed in the receipt (`target/dev-runs/<id>/run.json`):

| Phase | What happens |
|---|---|
| `prepare` | `vm.sh init-cycle`; for an alongside layout, a 40 GiB target with a foreign EFI partition and a Linux sentinel filesystem, hashed |
| `boot-installer` | boot the medium (a throwaway overlay; the medium is never written) with a blank target as `/dev/vdb`; wait for the installer to answer its console |
| `install` | walk the wizard through the installer's published actions: keyboard, network, disk and method, account, options, review, **begin** (destructive: its confirmation phrase is the target), progress, recovery-key confirmation, **power off** |
| `power-off` | the installer's own Finish action stops the VM |
| `verify-alongside` | the preserved extents hash exactly as before |
| `first-boot` | cold-boot the target, enter the unlock secret twice at most during early boot, then recognize sign-in through its console or identity setup through a screenshot and OCR |

The result (`--json`, and `result.json` in the artifact directory) names the
target, every step the installer went through, the installed image digest,
the observed first-boot surface, and the preserved extents. Sign-in reports
its composefs digest directly; identity setup proves the target boots but
does not itself report that digest.

## The console channel

The installer and the greeter run under `cage`, straight from greetd, before
any Tilo compositor exists, so the compositor's control socket never sees them.
They answer the **same** semantic half of the `tilo-control` protocol
themselves — `actions list`, `actions invoke`, `state-dump` — on two local
endpoints:

- `$XDG_RUNTIME_DIR/tilo-console/<app>.sock` inside the guest (0700 directory,
  0600 socket): the local console and assistive tooling running as that user;
- the virtio-serial port `org.tilo.console.0`, which exists only because
  `vm.sh boot-installer` / `boot-target` attach it. Its host end is
  `$TILO_RUN/console.sock` on Linux or a localhost TCP port on Windows.
  Real hardware has no virtio-serial bus, so the channel cannot exist there.

Drive a scenario VM yourself (with `--keep`, or while a run is in progress):

```sh
TILO_VM_CYCLE=<cycle> ./os/dev/vm.sh console          # prints the socket path
tilo-control --socket <that path> actions list        # the whole wizard, with why each step is or is not available
tilo-control --socket <that path> state-dump --pretty
tilo-control --socket <that path> actions invoke installer next@installer
tilo-control --socket <that path> actions invoke installer begin@installer.review \
    --args-json '{"confirmation_phrase":"/dev/vdb"}'
```

Action ids are `<action>@<resource>`. An action whose contract is destructive
takes its confirmation as an argument — `confirmation_phrase` for a typed
phrase, `confirm: true` for a plain confirmation — exactly like app actions
through the compositor. A wrong or missing phrase is
`action_confirmation_required`; a step the wizard is not on is
`action_unavailable` with the reason the screen would give.

## Secrets

- Disposable scenarios use a fixed public unlock secret
  (`scenario-passphrase-2026`, or the PIN `804927`). For a retained install,
  pass `--username bob --credentials-stdin` and pipe exactly one UTF-8 JSON
  object: `{"account_password":"...","disk_passphrase":"..."}`. Both values
  must be distinct, lowercase ASCII letters and digits. Generate long random
  values (at least 24 characters each). This alphabet is exactly typable at
  the VM's first-boot unlock prompt. `--credentials-stdin` requires
  `--username` and is refused with `--dry-run`. Supply the JSON from a protected
  launcher or password store; never place either value in the command line.
  The supplied disk secret unlocks the installed system; the account secret
  logs into it. Both reach the guest through its published actions and the disk
  secret reaches the boot prompt through `vm.sh type -` (stdin).
- The recovery key never leaves the guest: the installer's console reports
  whether one exists, was copied and was confirmed saved, never the key, and it
  redacts anything key-shaped from the install log it reports.
- The live installer's serial console is not kept
  (`installer-serial-withheld.txt`).
- Before the run reports anything, every artifact and retained VM runtime log
  is searched for both supplied secrets, the backend's recovery-key marker and
  recovery-key-shaped text; a
  file that matches is emptied and the run fails.

## When it fails

The error names the phase and the installer's own reason, the artifact
directory (`installer-state-*.json`, `install.json`, `greeter-state.json`,
`target-serial.log`) and, for installer phases, the console command to inspect
it live. `--keep` keeps the target disk and runtime directory; `--encrypt
tpm-pin` needs a firmware store that trusts the image (`TILO_SECBOOT_KEYS`),
otherwise the installer offers only a passphrase and the run fails rather than
installing with a weaker factor than asked.

The release gate, `os/dev/test-installer-cycle.sh`, drives the same installer
actions over the same console, from a medium customised the way Tilo Imager
writes it, and continues into first-login OOBE and the desktop.
