# Refresh installer helpers on Windows ARM64

This development loop reuses a CI installer disk. Only a small allowlisted set
of scripts crosses the VM channel. The CI base disk, OCI target image and
installed composefs tree are unchanged. Every locally refreshed run is excluded
from physical installation; finish with a clean paired CI-medium run.

The medium must include `tilo-installer-code-refresh.service`. This bootstrap is
built once into development installer media. Older media fail with a missing
channel response; they cannot be refreshed through an arbitrary guest shell.

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
Remove-Item Env:TILO_INSTALLER_CODE_FROM, Env:TILO_INSTALLER_CODE_PAUSE
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
