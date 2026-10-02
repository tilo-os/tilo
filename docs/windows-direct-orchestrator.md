# Paired CI Windows direct install

`os/dev/windows-direct-install.py` resumes the Windows ARM64 path from one
reviewed allocation plan. It downloads only the exact Forgejo run artifacts,
verifies signed CI medium identities and OCI provenance, runs the paired WHPX
scenarios, and calls the existing guarded source finalizer and receipt writer.
Physical copy and BootNext are separate explicit commands.

Use native Windows Python, `fgj` authenticated to `code.tilo.so`, `zstd`,
`tilo-vm`, and the `cosign` command described by `cosign_command_json`.
The three manifest files must contain the **raw OCI manifest bytes** of the
exact pinned generic index, its ARM64 child, and the Lenovo child. Do not use
formatted `podman manifest inspect` JSON: the digest checks require the raw
registry bytes. Fetch them with `os/dev/fetch-windows-direct-manifests.py`
after creating the output directory and pinning both image digests:

```powershell
python os/dev/fetch-windows-direct-manifests.py `
  --generic-image 'registry.tilo.so/tilo/tilo@sha256:<signed index digest>' `
  --lenovo-image 'code.tilo.so/tilo/tilo@sha256:<signed Lenovo child digest>' `
  --generic-index-manifest C:\tilo\generic-index-manifest.json `
  --generic-child-manifest C:\tilo\generic-arm64-manifest.json `
  --lenovo-child-manifest C:\tilo\lenovo-arm64-manifest.json
```

The helper uses `skopeo inspect --raw` inside `FedoraLinux-42` WSL, accepts
only official immutable digest refs, verifies each raw byte digest, and selects
the unique Linux ARM64 child from the generic index. It downloads no layers.
Pass `--wsl-distro` if Skopeo is in a different WSL distribution. It rechecks
existing files on resume and never accepts registry credentials in arguments.
The existing `cosign` helper may run in Fedora WSL; pass its
JSON argv file and the exact WSL path of `keys/cosign.pub`.

Create a new empty work directory and an input JSON file outside it:

```json
{
  "schema": "tilo.windows-direct-install.v1",
  "source_revision": "<full lowercase source commit>",
  "imager_run": 12345,
  "generic_os_run": 12340,
  "lenovo_os_run": 12341,
  "generic_image": "registry.tilo.so/tilo/tilo@sha256:<signed index digest>",
  "lenovo_image": "code.tilo.so/tilo/tilo@sha256:<signed Lenovo child digest>",
  "realized_plan": "C:\\tilo\\realized-plan.json",
  "source_raw": "C:\\tilo\\source.raw",
  "generic_index_manifest": "C:\\tilo\\generic-index-manifest.json",
  "generic_child_manifest": "C:\\tilo\\generic-arm64-manifest.json",
  "lenovo_child_manifest": "C:\\tilo\\lenovo-arm64-manifest.json",
  "verify_key": "C:\\tilo\\keys\\cosign.pub",
  "cosign_command_json": "C:\\tilo\\cosign-runner.json",
  "cosign_key_arg": "/mnt/c/tilo/keys/cosign.pub",
  "tilo_vm_exe": "C:\\tilo\\tools\\tilo-vm.exe",
  "username": "installer"
}
```

The run numbers identify a successful paired `release-imager.yml` run and the
two successful `build-image.yml` runs. The wrapper checks their source commits
and binds the signed OCI provenance invocation IDs to the selected OS runs.
The imager identities bind both medium archives, their verified qcow2 bytes,
native CI guest proofs, exact source revision, live base, and one imager run
attempt. The verified qcow2 and signed sidecars are retained in the work
directory; the downloaded ZIPs are removed after verification. There is no
raw media conversion.

Run `prepare` only after the realized plan has been reviewed and allocation
has completed. The `source_raw` path may be new or may name an existing **empty**
sparse template, provided the backend verifies its GPT, preserved samples, and
manifest against this exact realized plan. An installed or partially changed
source is refused without a matching checkpoint. The generic VM uses a
**different** sparse source in the work directory, installs beside synthetic
Windows GPT entries, and completes its first boot. The Lenovo VM uses the
specified source raw and stops after the signed target install. Supply the
retained account and disk secrets as one JSON object on standard input; the
wrapper passes them to `tilo-vm` without logging or storing them:

```powershell
python os/dev/windows-direct-credentials.py |
  python os/dev/windows-direct-install.py --input C:\tilo\input.json --work-dir C:\tilo\run prepare
```

On a Windows account with the existing CurrentUser DPAPI protected install
secrets, use `os/dev/windows-direct-credentials-stored.ps1` in place of the
interactive Python prompt. Pipe it directly into `prepare`; do not save its
output to a plaintext file.

Windows scenarios explicitly use an ext4 root and retain their install record.
If installation completed but first-boot verification failed, rerunning
`prepare` resumes verification of that same encrypted target. It checks the
original inputs, target GPT and guest image proof before booting; it does not
repeat the installer or overwrite a different target. The finished live
installer overlay is discarded before target boot to reclaim its temporary
container store.

For a standalone retained Windows scenario, use the original options and
output directory with `tilo-vm scenario install --resume --keep`. Supply the
original secrets again if the first install used `--credentials-stdin`.

The prompt helper sends only the JSON through the process pipe; it creates no
plaintext file. `prepare` prompts only if a WHPX phase still needs to run.
On success `C:\tilo\run\source-receipt.json` binds both checked WHPX proofs,
signed CI media, OCI signatures and provenance, and final source hashes.
`prepare` can be repeated to resume completed checkpoints. It refuses an
existing uncheckpointed source raw or scenario directory, a changed input,
or a changed checkpoint output. A stopped or partially written source needs
inspection before another attempt.

Only one medium is downloaded and decompressed at a time. The wrapper streams
the `.qcow2.zst` ZIP member through `zstd` directly into the checked qcow2,
then removes both downloaded ZIPs. It deletes the disposable generic VM target
after preserving its checked first-boot proof, before the Lenovo VM install.
It checks available space before media and VM phases and refuses to start
below 20 GiB free.

After reviewing the source receipt and physical disk ID, run the guarded
physical copier explicitly:

```powershell
python os/dev/windows-direct-install.py --input C:\tilo\input.json --work-dir C:\tilo\run copy --confirm-disk-id <reviewed-disk-id>
```

The copier checks the live disk, GPT, BitLocker and source receipt and writes
only the three reviewed Tilo extents. It produces `final-readback.json`.
Only after reviewing that readback, arm a single firmware boot:

```powershell
python os/dev/windows-direct-install.py --input C:\tilo\input.json --work-dir C:\tilo\run bootnext --imager C:\path\to\imager.exe
```

BootNext is intentionally one-shot; the wrapper refuses to invoke it again
after a recorded attempt. If it fails after the firmware write, inspect the
firmware state as described in [Windows-direct BootNext](windows-direct-bootnext.md).
No command here reboots the machine. For allocator prerequisites and the
physical boot boundary, see [Windows ARM64 direct final install](windows-direct-final-install.md).
