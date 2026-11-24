# Scenarios: cold boots end to end

Boot components (the installer, the UKI, the bootloader, the initramfs) only
take effect on a cold boot, so `tilo-vm swap` cannot put them into a running
VM. A scenario boots them in a throwaway VM instead. `tilo-vm swap installer`
points here.

A scenario also covers what one device's running session cannot show: a fleet
of devices on one private network, and a home with a server and no tilo.sh.
Those steps go through published actions and `tilo device`, and a capability
that does not exist yet ends its step naming it.

## `tilo-vm scenario install`

```sh
./os/build.sh --local                        # localhost/tilo:latest from this worktree
./os/dev/build-custom-installer.sh           # os/output/installer/qcow2/disk.qcow2
tilo-vm scenario install                     # erase layout, passphrase unlock
tilo-vm scenario install --layout guided-alongside
tilo-vm scenario install --layout prepared-alongside --graphics software
tilo-vm scenario install --dry-run --json    # the plan, nothing booted
```

No account and no dev VM are involved. The scenario runs its own QEMU VM through
`os/dev/vm.sh` (the headless path the release check uses; set `TILO_DISTROBOX=""`
when QEMU is on the host itself) in a named cycle, so its disks, TPM and
firmware state exist for this run only.

For an already-built unsigned ARM development medium, Linux also supports
`TILO_UNVERIFIED_VM_INSTALL=1 TILO_VM_ARCH=aarch64`. This explicit opt-in
attaches the existing VM install-proof port required by that medium. The guest
still requires its embedded development marker, detected virtualization and
passphrase unlock; signed production checks are unchanged. Use only owned
disposable VM targets, never physical disks. This diagnostic does not establish
release signing or current-source acceptance. On an x86 host, select
`TILO_ACCEL=tcg TILO_MEM=4096 TILO_SMP=2 TILO_DISTROBOX=""` explicitly; ARM
software emulation can take substantially longer than native installation.

An Apple silicon Mac runs the same ARM scenario natively under Hypervisor.framework
with Homebrew's `qemu`, `socat`, `tesseract`, `imagemagick` and `util-linux`
(for `setsid`, which is keg-only, so append `/opt/homebrew/opt/util-linux/bin` to
`PATH`). Link Homebrew's `edk2-aarch64-code.fd` and `edk2-arm-vars.fd` into one
directory as `AAVMF_CODE.fd` and `AAVMF_VARS.fd`, then run with
`TILO_ACCEL=hvf TILO_VM_ARCH=aarch64 TILO_DISTROBOX="" TILO_AAVMF_DIRS=<that directory>`
and `--graphics software`. The installer, the encrypted install and the target's
cold boot take minutes, not hours.

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
| `install` | walk the wizard through the installer's published actions: keyboard, network, disk and method, account, options, media (the optional extra formats, skipped), review, **begin** (destructive: its confirmation phrase is the target), progress, recovery-key confirmation, **power off** |
| `power-off` | the installer's own Finish action stops the VM |
| `verify-alongside` | the preserved extents hash exactly as before |
| `first-boot` | cold-boot the target, enter the unlock secret twice at most during early boot, then recognize sign-in through its console or identity setup through a screenshot and OCR |

The result (`--json`, and `result.json` in the artifact directory) names the
target, every step the installer went through, the installed image digest,
the observed first-boot surface, and the preserved extents. Sign-in reports
its composefs digest directly; identity setup proves the target boots but
does not itself report that digest.

### Retained installation preferences

Use `--hostname tilo-desktop --keyboard-layout "German (QWERTZ)" --timezone Europe/Berlin`
for a German keyboard and Berlin time. The keyboard argument is the installer's
published display name. `--locale de_DE.UTF-8` is optional; without it the medium's
language choice stays unchanged. Hostname defaults to `scenario-tilo`; omitted
keyboard, timezone and locale preserve the medium's choices.

These inputs are recorded in `scenario.json`; `--resume` requires the same inputs.
The driver checks their installer state before starting the destructive install,
including when preconfigured media skip a screen. An incompatible skipped screen
fails before installation. Regular identity OOBE remains for the first user boot.

The installed console and desktop receive the selected keyboard. A prebuilt CI
initrd is unchanged: its early encrypted-disk unlock can still use US keys until
the root filesystem is available. Use a layout-neutral passphrase or account for
that first-unlock layout; changing the signed initrd is a separate image operation.

## `tilo-vm scenario four-devices`

One person's four devices used as one computer, on tilo-vm's private network:

```sh
./os/build.sh --local                                    # the x86-64 desktop and laptop disk
TILO_VM_ARCH=aarch64 ./os/build.sh --local                # the aarch64 laptop disk (in a second checkout)
tilo-vm scenario four-devices --arm-disk os/output/qcow2/disk.qcow2
tilo-vm scenario four-devices --dry-run --json            # every step and what it needs
```

It boots an x86-64 desktop (`--2d`, as the measured-boot check does), an aarch64
laptop (under the software emulator when the host is x86-64), and the server
when `--server-disk` names an installed host image. The
guests share a multicast segment on the host's loopback — no bridge, no root
— and the dev image
gives each NIC whose MAC starts `52:54:01:00` an address on it
(`198.18.<x>.<y>`), so nothing needs DHCP. Each guest is also reachable over
its own forwarded SSH port for the scenario's own driving.

The steps prove, in order:

| Step | What it proves | Needs |
|---|---|---|
| `identity` | each device's account holds the one identity the person signed in with | an identity made through published actions (first use is a UI ceremony today) |
| `devices` | every device lists all of them as the person's own | the fleet added by ticket under one identity |
| `second-output` | the x86 desktop drives two virtual outputs | a second display adapter on the desktop VM |
| `outputs` | all three outputs are in the person's shared display view | the `person_displays` view published as an action |
| `arrangement` | the outputs sit in one arrangement the pointer crosses | an action that places a display |
| `window` | a window moves from the desktop's second output to the laptop | an action that moves a window onto another output |
| `server-turn` | the server takes an agent turn after the laptop is suspended | an agent turn on a server with no desktop awake |

Every step goes through published actions and `tilo device`, over the guest's
own login. A step whose capability is not built yet ends `missing`, naming it;
the run reports every step and exits non-zero rather than waiting on a boot or
an invocation that never answers. `--dry-run` prints the same plan without
booting anything.

## `tilo-vm scenario laptop-phone-server`

Laptop and server with no tilo.sh and no internet:

```sh
tilo-vm scenario laptop-phone-server --server-disk /path/to/server-disk.qcow2
tilo-vm scenario laptop-phone-server --dry-run
```

The desktop boots beside the server on the same private network, and the
scenario drops the desktop's default route, so it has the host forward for the
scenario's own login and no way to the internet. The steps prove that the
server serves its own Tilo web UI by default and stops serving it when its
device setting is off; that the desktop is offline; that it signs in to the
server with no tilo.sh; and that it takes the server's update lease and
restarts it through actions. As in `four-devices`, a capability nobody has
built ends its step naming that capability; the server's disk is optional, and
its steps say so when it is absent.

## The console channel

The installer and the greeter run under `cage`, straight from greetd, before
any Tilo compositor exists, so the compositor's control socket never sees them.
They answer the **same** action frames as the desktop, and `state-dump`,
themselves, on two local endpoints:

- `$XDG_RUNTIME_DIR/tilo-console/<app>.sock` inside the guest (0700 directory,
  0600 socket): the local console and assistive tooling running as that user;
- the virtio-serial port `org.tilo.console.0`, which exists only because
  `vm.sh boot-installer` / `boot-target` attach it. Its host end is
  `$TILO_RUN/console.sock` on Linux or a localhost TCP port on Windows.
  Real hardware has no virtio-serial bus, so the channel cannot exist there.

Drive a scenario VM yourself (with `--keep`, or while a run is in progress):

```sh
TILO_VM_CYCLE=<cycle> ./os/dev/vm.sh console          # prints the socket path
tilo-control --socket <that path> raw \
    '{"version":3,"message":{"CatalogueRequest":{"correlation":1,"query":{}}}}'   # its actions; "parent_resource" lists what one holds
tilo-control --socket <that path> state-dump --pretty
os/dev/installer-console.py --socket <that path> invoke next@installer
os/dev/installer-console.py --socket <that path> invoke begin@installer.review --phrase /dev/vdb
```

The console answers action frames, the same ones the desktop answers. Action
ids are `<action>@<resource>`. An action whose contract is destructive takes
its confirmation beside its arguments, a typed phrase or a plain confirmation,
exactly like app actions through the compositor: an `Invoke` frame carries
`"confirmation": {"Phrase": "/dev/vdb"}`, which `installer-console.py invoke
--phrase` writes, and a script written in the headless-drive format names the
same phrase `confirmation_phrase`. A wrong or missing phrase is
`ConfirmationRequired`; a step the wizard is not on is
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

The release check, `os/dev/test-installer-cycle.sh`, drives the same installer
actions over the same console, from a medium customised the way Tilo Imager
writes it, and continues into first-login OOBE and the desktop.

## Qualifying the owner's real devices

The two flows' hardware parts also run on the real machines a person has. These
are manual, with `tilo device` from a development machine that reaches them (the
same story as a scenario, against real hardware); the Orange Pi's and the
Snapdragon laptop's device trees cannot boot QEMU's `virt` machine, which is why
no scenario stands in for them.

Register each device once (`tilo-vm vm add <name> --ssh <user>@<address>`), then
for each:

**Orange Pi 3B** (`os/hardware/profiles/orangepi-3b-v2-1.json`)

- `tilo device install` the board image, then cold-boot it: HDMI shows the
  greeter, the serial console names the same boot, and `tilo device status`
  answers after it starts.
- Plug HDMI into the board's port: the output appears in Settings' displays and
  joins the person's arrangement beside the desktop's two monitors.
- `tilo device update --check`, `tilo device update -y --reboot`, then
  `tilo device rollback -y`; the board boots both directions and its disk still
  unlocks.

**Lenovo 83ED, Snapdragon X Elite** (`os/hardware/profiles/lenovo-83ed.json`)

- Same sequence: install, cold boot with the internal panel and HDMI out both
  lighting up, `tilo device status` after sign-in.
- `tilo device update -y --reboot` and `tilo device rollback -y`; the firmware
  menu lists the deployments and the rollback boots.
- Its outputs join the arrangement like the board's; the internal panel comes
  up at the scale its EDID names.

**Fedora desktop with two monitors**

- Boot the desktop image; both monitors show the greeter and join the
  arrangement; drag a window from one to the other and check the pointer crosses
  only where they meet.
- `tilo device add --capability admin` prints a ticket; `tilo login <ticket>` on
  the board and the laptop joins them to the desktop's identity, and Settings'
  devices lists all of them.
- Suspend the laptop; ask the server (or tilo.sh) for an agent turn and see it
  answer while the laptop is asleep.
