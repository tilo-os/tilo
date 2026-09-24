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

Phases, each timed in the receipt (`target/dev-runs/<id>/run.json`):

| Phase | What happens |
|---|---|
| `prepare` | `vm.sh init-cycle`; for an alongside layout, a 40 GiB target with a foreign EFI partition and a Linux sentinel filesystem, hashed |
| `boot-installer` | boot the medium (a throwaway overlay; the medium is never written) with a blank target as `/dev/vdb`; wait for the installer to answer its console |
| `install` | walk the wizard through the installer's published actions: keyboard, network, disk and method, account, options, review, **begin** (destructive: its confirmation phrase is the target), progress, recovery-key confirmation, **power off** |
| `power-off` | the installer's own Finish action stops the VM |
| `verify-alongside` | the preserved extents hash exactly as before |
| `first-boot` | cold-boot the target, type the unlock secret at its boot prompt, wait for the greeter to answer its console, and check it booted the composefs image the installer wrote |

The result (`--json`, and `result.json` in the artifact directory) names the
target, every step the installer went through, the image digest the live
installer booted and the one the first boot runs, and the preserved extents.

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
  `$TILO_RUN/console.sock`. Real hardware has no virtio-serial bus, so the
  channel cannot exist there; nothing listens on a network.

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

- The unlock secret is fixed and public (`scenario-passphrase-2026`, or the
  PIN `804927`); it protects a disposable disk. It reaches the guest only as a
  console argument and on the boot prompt through `vm.sh type -` (stdin), never
  on a command line.
- The recovery key never leaves the guest: the installer's console reports
  whether one exists, was copied and was confirmed saved, never the key, and it
  redacts anything key-shaped from the install log it reports.
- The live installer's serial console is not kept
  (`installer-serial-withheld.txt`).
- Before the run reports anything, every artifact is searched for the unlock
  secret, the backend's recovery-key marker and recovery-key-shaped text; a
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
