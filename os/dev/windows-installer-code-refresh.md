# Refresh installer helpers on Windows ARM64

This development loop reuses a CI installer disk. Only a small allowlisted set
of scripts crosses the VM channel. The CI base disk, OCI target image and
installed composefs tree are unchanged. Every locally refreshed run is excluded
from physical installation; finish with a clean paired CI-medium run.

New media include `tilo-installer-code-refresh.service`. For older CI media,
seed a tiny VM-only overlay once with the local bootstrap below.

## Bootstrap older CI media locally

Use WSL root with a private mount namespace. The host needs QEMU image/NBD
tools, mtools, util-linux and the filesystem driver for the CI root. This
creates a NEW overlay; the verified CI base and its UKI/composefs are unchanged:

```sh
sudo unshare -m --propagation private python3 os/dev/bootstrap-installer-qcow2.py \
  --source /mnt/c/VMs/ci-dev-installer.qcow2 \
  --verified /mnt/c/VMs/ci-dev-installer.qcow2.verified.json \
  --output /mnt/c/VMs/local-bootstrap.qcow2 --checkout "$PWD"
```

In PowerShell, set `TILO_INSTALLER_BOOTSTRAP_DISK=C:\VMs\local-bootstrap.qcow2`
alongside the two code-refresh opt-ins below. Still pass the ORIGINAL verified
CI disk to `--medium`. The backend verifies the tiny overlay hash, its backing
chain and CI download receipt before booting it. The frozen bootstrap lives in
the selected deployment's writable `/etc`; it starts in PID1's mount namespace
after installer setup and binds helpers with their original SELinux labels.
It never copies over immutable `/usr` or changes the kernel, UKI or target OCI.

To reuse an already imported target after a failed scenario, stop that scenario
with `stop-cache` (successful guest poweroff required) and add `--cached-source /mnt/c/.../scenario-install-.../installer-work.qcow2`.
If its backing filenames are Windows absolute paths, also pass
`--host-qemu-img /mnt/c/.../qemu-img.exe`. Filenames are translated only in
temporary NBD image options; original files are unchanged. Such a cache is
explicitly recorded as UNVERIFIED scenario work, never as a CI artifact.
Dirty ext4 journal replay is contained in the new overlay: pristine bootstrap
is bounded to 64 MiB and cached bootstrap to 128 MiB.

Bootstrap acknowledgment includes actual read-only transient backend execution,
SELinux enforcing state, domain and executed helper hash. Optional original CI
boot-input fixtures run the current validator against the original kernel/initrd,
real UKI sections and profile-pinned DTB without importing or repairing Podman
storage. This evidence is for local development only.

## Start a paused development scenario

From the source checkout, in PowerShell:

```powershell
$env:TILO_INSTALLER_CODE_FROM = (Get-Location).Path
$env:TILO_INSTALLER_CODE_PAUSE = '1'
tilo-vm scenario install --medium C:\VMs\ci-dev-installer.qcow2 --graphics software --keep --out C:\VMs\installer-code-test
```

The maintained Windows backend boots a throwaway WHPX overlay and sends the
current helper bundle before the installer UI starts. The scenario pauses before
its installer actions and prints the path of `continue-install`.

## Refresh changed scripts

In another PowerShell terminal, read `scenario.json` in the artifact directory.
The cycle name identifies its directory under `storage\vm-cycles`. Refresh it:

```powershell
python -X utf8 os\dev\refresh-installer-code.py --source . --scenario C:\VMs\installer-code-test\storage\vm-cycles\scenario-install-YYYYMMDDTHHMMSSZ-PID
```

The host records the source revision, dirty state and each file's content hash
before sending anything. The guest returns the same receipt. The packet is at
most 2 MiB, each file at most 512 KiB. Unchanged generations are idempotent.
Only the fixed helper allowlist is accepted; the proof emitter and refresh
bootstrap cannot be replaced through this channel.

Continue the normal published installer action driver:

```powershell
New-Item -ItemType File C:\VMs\installer-code-test\continue-install
```

Refresh is refused while any installer backend command holds the shared lock.
A failed install stops its VM using the existing scenario lifecycle. Start the
next scenario from the same CI medium, passing the edited checkout again; no
medium build or download is needed. Remove the old continuation marker or use a
fresh artifact directory. At most 32 code generations are retained per guest.

Disable the development options for the final clean CI run:

```powershell
Remove-Item Env:TILO_INSTALLER_CODE_FROM, Env:TILO_INSTALLER_CODE_PAUSE, Env:TILO_INSTALLER_BOOTSTRAP_DISK -ErrorAction SilentlyContinue
```

Host code-channel and complete refresh-history evidence are bound into the
checked VM proof independently of guest claims. Windows-direct physical
admission rejects active channels, any refresh history, omitted host evidence,
or guest override markers.

Python 3 must be on PATH, or set `TILO_PYTHON` to its executable. Helpers must use LF line endings. Hot refresh is allowed only during the explicit paused window; creating `continue-install` permanently closes that window before the first installer action. The guest independently checks active installer units/processes and the backend lock. Files are staged together and new bind mounts are rolled back on failure; a failed rollback stops the disposable VM greeter. Proof emitters, verification preflight and refresh bootstrap are excluded from refresh.

Privileged integration proof is an explicit fail-fast command (ordinary unit tests have no skip gates):

```sh
sudo unshare -m --propagation private python3 os/tests/probe_installer_code_refresh_mounts.py
```

## Read-only original CI boot-input diagnostics

A stopped scenario cache is unverified development data. A forced QEMU stop can
leave imported layers incomplete even when the manifest is present. Do not repair
or reimport that cache to test a pure helper: use the original verified CI boot
inputs on a clean generic bootstrap instead.

```powershell
python -X utf8 os/dev/make-installer-probe-fixture.py --root C:/VMs/original-ci-boot-inputs --output C:/VMs/boot-inputs.tar --target-digest sha256:EXACT_OCI_DIGEST
$env:TILO_INSTALLER_PROBE_UKI = 'C:/VMs/boot-inputs.tar'
```

The optional fixture requires the paused local bootstrap opt-ins. QEMU exposes it
as a read-only virtio block device; the original medium and fixture stay unchanged.
The frozen probe verifies the archive manifest, file hashes, path/size bounds and
block-device read-only status, then runs the actual refreshed UKI validator against
those bytes in private `/run` storage. It compares the real EFI kernel/initrd/DTB
sections with the split payloads and profile DTB pin. The archive is bounded to 512
MiB and 16 regular files. This is operator-supplied diagnostic provenance, not CI
or physical admission authority. The host records its SHA256 before transport and
requires the guest result to agree; host refresh history permanently excludes the
scenario from physical admission.

The transient backend receipt also inventories `ukify`, both conventional ukify
paths, `objcopy`, and the Python `pefile`/`cryptography` modules. This observes the
actual installer runtime without installing packages or touching a target disk.

To exercise sealed UKI promotion as well, supply all four optional fixture flags:
`--promotion-origin`, `--promotion-loader`, `--composefs-digest`, and
`--update-origin`. The first two files contain the intended bootc origin and BLS
entry; use the exact original CI EFI and its split payloads in the input tree.
The probe constructs private scratch deployment/ESP directories in `/run`, calls
the refreshed promotion helper, and verifies the selected OCI origin and loader
are unchanged. It compares every PE byte except the `.cmdline` payload and its
virtual size. Its receipt records both UKI hashes and explicitly identifies
`scratch-bootc-format-origin`; this does not test an installed root or make the
scenario physically admissible. The same scratch probe runs after every paused
warm refresh, avoiding another medium build, download, or OCI import.

For future reusable cache capture, invoke `vm-windows.ps1 stop-cache` with the
scenario's existing transport environment. It requests guest poweroff and waits
for owned QEMU exit. If poweroff fails, it leaves the VM running and refuses the
capture; it never force-kills a VM while claiming clean cache capture. Ordinary
`stop` still ends disposable scenarios and does not attest cache durability.

Clear `TILO_INSTALLER_PROBE_UKI` along with the code/bootstrap options before a
clean CI proof or physical installation.
