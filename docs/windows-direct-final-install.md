# Windows ARM64 direct final install

This is the no-USB path for placing Tilo's final ESP, plain `/boot`, and
LUKS2 root beside Windows before the first physical Tilo reboot. It keeps the
four reviewed Windows GPT entries and writes only the three new Tilo extents.
The scripts here are guarded backend operations; the production UI and VM
scenario still need to call them as one resumable workflow.

## Boundaries

- `allocate-windows-direct-final-install.py prepare` creates a read-only
  request for the current Windows disk. `execute` requires its exact SHA-256,
  disk ID, matching BitLocker recovery protector, an explicit assertion that
  its 48-digit recovery key has been backed up, suspended BitLocker, no active
  C: pagefile, an exact GPT readback, and sufficient C: headroom. It shrinks
  only the reviewed C: partition and creates exactly a 2 GiB ESP, 1 GiB boot,
  and 32 or 45 GiB root. A resumed run accepts only exact intermediate maps.
- `prepare-windows-direct-source.py template` creates a **sparse VM file**
  with only synthetic Windows GPT entries and the exact free target extent.
  Its synthetic Windows partition contents are empty and must never be
  copied to the physical disk. The file is logically as large as the physical
  disk because the offsets must match; sparse allocation avoids consuming
  that capacity on the host. It also emits `<source.raw>.manifest.json` with
  the four preserved GPT identities and first/last 64 KiB SHA-256 samples of
  each synthetic Windows extent. `verify-template` accepts `--manifest` and
  `--source-raw`, and should run both before and after the WHPX scenario.
- Boot a separately downloaded **CI-built, CI-signed** installer medium in
  WHPX and attach this template as its writable target. Use the guided
  alongside installer flow, then boot and check the installed VM once. The
  installer must produce the ESP, `TILO_BOOT` ext4 partition, and LUKS2 root.
  Shut the VM down before accessing its source file on Windows.
- `prepare-windows-direct-source.py finalize` requires the VM's seven GPT
  entries to retain all four synthetic Windows identities and to match the
  reviewed final geometry and types. It rewrites only protective MBR and GPT
  metadata so the three VM-created PARTUUIDs match the Windows-assigned ones.
  It leaves partition contents, including the CI-signed UKI, untouched.
- `prepare-windows-direct-source.py receipt` checks a digest-pinned CI OCI
  image with `cosign verify` and its SLSA provenance attestation using the
  trusted public key, then hashes each final source extent. This is a local
  byte-integrity receipt. The VM install run must separately establish that
  it installed this exact CI image digest.
- `copy-windows-direct-final-install.py execute` rechecks the CI signature,
  source hashes, both source and physical GPT copies, Windows disk identity,
  suspended BitLocker, the live C: identity, and static Windows partition
  hashes. It copies and reads back only the three reviewed Tilo extents. C:
  remains live, so its changing contents are excluded from the static hash
  comparison; its GPT identity and geometry are checked before and after.

`tools/launch-arm64-disk.ps1` already supports the temporary WHPX probe via
`-DiskPath <CI installer medium> -InstallTargetDisk <sparse source.raw>`.
Never pass `-CreateInstallTarget` for this source file; that would replace its
reviewed GPT. The maintained path should add a Windows ARM64
`tilo-vm scenario install` backend that uses this source-target layout, drives
the guided installer, checks the first VM boot and exact CI image digest, and
returns the VM-installed source receipt without manual guest steps. It must
use WHPX, never TCG. Small component changes should continue through
`tilo-vm` slots without repeating disk installation.

The backend command contract is:

```text
python os/dev/allocate-windows-direct-final-install.py prepare --root-gib 45 --output request.json
python os/dev/allocate-windows-direct-final-install.py execute --request request.json --confirm-request-sha256 <sha256> --confirm-disk-id <disk-id> --confirm-recovery-protector-id <id> --confirm-recovery-key-backed-up --output realized-plan.json
python os/dev/prepare-windows-direct-source.py template --plan realized-plan.json --output source.raw
python os/dev/prepare-windows-direct-source.py verify-template --manifest source.raw.manifest.json --source-raw source.raw
# Run the CI installer in WHPX against source.raw; verify first VM boot; shut it down.
python os/dev/prepare-windows-direct-source.py verify-template --manifest source.raw.manifest.json --source-raw source.raw
python os/dev/prepare-windows-direct-source.py finalize --plan realized-plan.json --source-raw source.raw
python os/dev/prepare-windows-direct-source.py receipt --plan realized-plan.json --source-raw source.raw --ci-image <repo@sha256:digest> --ci-index-manifest ci-index.json --ci-child-manifest ci-arm64-child.json --vm-proof-checked vm-install-proof-checked.json --verify-key keys/cosign.pub --cosign-command-json cosign-runner.json --cosign-key-arg /mnt/c/path/to/keys/cosign.pub --output source-receipt.json
python os/dev/copy-windows-direct-final-install.py execute --plan realized-plan.json --source-raw source.raw --source-receipt source-receipt.json --confirm-disk-id <disk-id> --ci-image <repo@sha256:digest> --ci-index-manifest ci-index.json --ci-child-manifest ci-arm64-child.json --vm-proof-checked vm-install-proof-checked.json --verify-key keys/cosign.pub --cosign-command-json cosign-runner.json --cosign-key-arg /mnt/c/path/to/keys/cosign.pub --output final-readback.json
```

`verify-template` prints a JSON record with `disk_guid` and
`final_partuuids` (`efi`, `boot`, `root`). Before the VM install the latter is
empty; after the install it contains the guest-created GPT identities. The
scenario must compare them to the guest's install proof before `finalize`
rewrites the final PARTUUIDs to the Windows-assigned values.
The receipt and physical copier require the host-checked proof from the WHPX
scenario. They verify that the checked guest image digest equals the unique
Linux ARM64 child of the signed CI index, then independently hash the
installed target UKI by reading its FAT32 path in the source ESP. The provided
index and child JSON files must be exact raw OCI manifest bytes; their SHA256
digests are checked against the CI pin and child descriptor.

On this Windows ARM64 host, Cosign runs in Fedora WSL. Pass
`--cosign-command-json cosign-runner.json` to both `receipt` and `execute`,
where the JSON is an argv array such as
`["wsl.exe","-d","FedoraLinux-42","--exec","/home/xemmerling/.local/bin/cosign"]`.
Also pass `--cosign-key-arg /mnt/c/path/to/cosign.pub` while `--verify-key`
continues to name the same native Windows file for SHA256 binding. The
WSL key path must be the exact `/mnt/<drive>/...` translation of that
resolved Windows path. The command is executed as an argument vector without
a shell.

The allocator command above is an authorized physical mutation and must run
only after the matching BitLocker recovery key has actually been backed up.
The source raw commands act only on a new disposable VM file.

## Remaining integration before physical completion

The physical copier deliberately does not mutate firmware. The final ESP
needs a verified `EFI_LOAD_OPTION` for its Windows-assigned GPT partition and
signed ARM64 loader, followed by `BootNext` while leaving `BootOrder` intact.
The Imager already has a tested EFI load-option encoder and Windows firmware
adapter in `apps/imager/src/setup/staging/efi.rs` and
`apps/imager/src/setup/staging/windows/adapter.rs`; the direct path should
reuse those, bind the new entry to the final copy readback receipt, and reject
an occupied `BootNext`. Until that is integrated and tested, these scripts do
not claim a complete first-boot-ready physical installation.
