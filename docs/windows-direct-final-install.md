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
  When `Get-PartitionSupportedSize` rejects the reviewed target, `execute`
  queries DiskPart and requests the same whole-MiB shrink as both `desired`
  and `minimum`. It rechecks disk identity, GPT, BitLocker, C: identity and
  free space before that command, then requires the exact GPT map and NTFS
  size change before creating any Tilo partition. A failed or partial shrink
  stops the workflow for inspection.
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
  alongside installer flow. For the Lenovo board target, verify the completed
  install and stop before a QEMU virt first boot; separately run the generic
  signed medium through install **and** first boot. The installer must produce
  the ESP, `TILO_BOOT` ext4 partition, and LUKS2 root. Shut the Lenovo VM
  down before accessing its source file on Windows.
- `prepare-windows-direct-source.py finalize` requires the VM's seven GPT
  entries to retain all four synthetic Windows identities and to match the
  reviewed final geometry and types. It rewrites only protective MBR and GPT
  metadata so the three VM-created PARTUUIDs match the Windows-assigned ones.
  It leaves partition contents, including the CI-signed UKI, untouched.
- `prepare-windows-direct-source.py receipt` checks a digest-pinned CI OCI
  image with `cosign verify` and parses its SLSA provenance attestation using
  the trusted public key. It requires the signed digest, official builder and
  source, and the Lenovo board profile and firmware inputs for a physical
  install. It binds the host-checked VM guest proof to that signed digest,
  reads the installed UKI and ARM64 loader independently from the source FAT32
  ESP, then hashes each final source extent. For a Lenovo physical source it
  also requires two separately signed CI installer medium identities: the
  Lenovo install-only WHPX proof and a generic full first-boot WHPX proof.
  The media must share CI checkout and signed live base; each has its own
  signed CI run identity, and each checked
  host proof must bind its own medium's SHA-256. The generic ARM64 child has
  its own signature and SLSA attestation in addition to its signed index.
- `copy-windows-direct-final-install.py execute` rechecks the CI signature,
  source hashes, both source and physical GPT copies, Windows disk identity,
  suspended BitLocker, the live C: identity, and static Windows partition
  hashes. It copies and reads back only the three reviewed Tilo extents. C:
  remains live, so its changing contents are excluded from the static hash
  comparison; its GPT identity and geometry are checked before and after.

The maintained Windows ARM64 `tilo-vm scenario install` path uses WHPX and
attaches the reviewed sparse source raw as its target. It drives the guided
installer and writes a checked guest proof beside the scenario result. The
Lenovo target uses `--skip-target-boot` because QEMU virt cannot establish its
board-specific first boot. This source proof is marked `first_boot_verified:
false`; a separate generic CI-signed target must complete the full installer
and first-boot cycle before physical copying. Keep the resulting
`vm-install-proof-checked.json` for the source receipt. Small component
changes can continue through `tilo-vm` slots without repeating disk
installation.

The backend command contract is:

```text
python os/dev/allocate-windows-direct-final-install.py prepare --root-gib 45 --output request.json
python os/dev/allocate-windows-direct-final-install.py execute --request request.json --confirm-request-sha256 <sha256> --confirm-disk-id <disk-id> --confirm-recovery-protector-id <id> --confirm-recovery-key-backed-up --output realized-plan.json
python os/dev/prepare-windows-direct-source.py template --plan realized-plan.json --output source.raw
python os/dev/prepare-windows-direct-source.py verify-template --manifest source.raw.manifest.json --source-raw source.raw
tilo-vm scenario install --medium C:\path\to\ci-installer.qcow2 --layout guided-alongside --target-disk source.raw --encrypt passphrase --graphics software --skip-target-boot --username <account-name> --credentials-stdin
python os/dev/prepare-windows-direct-source.py verify-template --manifest source.raw.manifest.json --source-raw source.raw
python os/dev/prepare-windows-direct-source.py finalize --plan realized-plan.json --source-raw source.raw
python os/dev/prepare-windows-direct-source.py receipt --plan realized-plan.json --source-raw source.raw --ci-image registry.tilo.so/tilo/tilo@sha256:<digest> --ci-index-manifest ci-arm64-manifest.json --ci-child-manifest ci-arm64-manifest.json --vm-proof-checked vm-install-proof-checked.json --verify-key keys/cosign.pub --cosign-command-json cosign-runner.json --cosign-key-arg /mnt/c/path/to/keys/cosign.pub --output source-receipt.json
python os/dev/copy-windows-direct-final-install.py execute --plan realized-plan.json --source-raw source.raw --source-receipt source-receipt.json --confirm-disk-id <disk-id> --ci-image registry.tilo.so/tilo/tilo@sha256:<digest> --ci-index-manifest ci-arm64-manifest.json --ci-child-manifest ci-arm64-manifest.json --vm-proof-checked vm-install-proof-checked.json --verify-key keys/cosign.pub --cosign-command-json cosign-runner.json --cosign-key-arg /mnt/c/path/to/keys/cosign.pub --output final-readback.json
```

If all three Tilo partitions were allocated in an earlier session but the
reviewed request or realized plan was lost, recover them read-only from the
live seven-partition GPT before starting the VM scenario:

```text
python os/dev/allocate-windows-direct-final-install.py recover --root-gib 45 --request-output recovered-request.json --plan-output realized-plan.json
```

`recover` independently reads the physical GPT, reconstructs the original
Windows-only geometry in memory, and requires it to reproduce the exact live
ESP, boot, and root extents through the normal allocation validator. It never
shrinks, creates, or formats a partition.

The Lenovo `receipt` and `execute` commands additionally require the same
eight arguments: `--board-medium-identity`, `--board-ci-guest-proof`,
`--generic-ci-image` (pinned generic multiarch index),
`--generic-index-manifest`, `--generic-child-manifest`,
`--generic-vm-proof-checked`, `--generic-medium-identity`, and
`--generic-ci-guest-proof`. The identity paths are the CI signed
`*.whpx-source.json` files, with adjacent `.sig` and `.bundle` files. The
native guest proofs are the raw CI cycle `guest-install-proof.json` files;
the generic checked VM proof must report a completed first boot. Both
commands verify the signatures again and bind all results into the source
receipt before a physical copy can open the disk for writing.

For a retained account, pass one JSON object on standard input with distinct
`account_password` and `disk_passphrase` fields. Keep the values out of the
command line and logs. The VM scenario rejects an account name without
`--credentials-stdin` and refuses to include either secret in its receipt.

For a guest image built with the VM install-proof helper, the WHPX scenario
attaches a dedicated named virtio serial port, visible in the guest at
`/dev/virtio-ports/org.tilo.vm-install-proof`. After `tilo-install` has closed
the LUKS target successfully, it emits one
`TILO_VM_INSTALL_PROOF_V2=<compact JSON>` line to that port for signed offline
targets. No backend logs
or secrets are sent to this channel. The JSON schema is
`tilo.vm-install-image-proof.v2`; it contains `architecture` (`arm64`),
separate booted and installed image and composefs digests, selected signed
target and installed update origin, `target_uki_sha256`, `disk_guid`,
and `partuuids` with `efi`, `boot`, and `root` fields. The guest derives those
from `bootc status`, the installed deployment `.origin`, the promoted target
UKI, and `blkid`; it refuses any mismatch. The scenario compares the installed
digest to the separately verified CI target digest and the GPT identities
to the VM source **before** `finalize` rewrites PARTUUIDs. It must also check
the UKI in the source ESP against this proof and the CI-signed boot binding.
The port is the VM-only opt-in signal; the backend also checks
`systemd-detect-virt --vm`. The line appears only after successful backend
completion. No supplied `--ci-image` argument alone establishes this
connection.

The guest image digest fields are platform-specific OCI manifest digests
(`sha256:` plus 64 lowercase hex), as reported by `bootc status` and the
installed `.origin`. The booted live medium and installed signed target can
have different digests. The CI pin may instead name a multiarch index. In
that case, the host must verify the signed index at its exact digest, parse
its OCI descriptors, select exactly one `linux/arm64` child manifest, and
compare the guest digest to that child. If the CI pin names a single image
manifest, compare it directly. The composefs digest is 128 lowercase hex
without a prefix; the UKI hash is 64 lowercase hex without a prefix; GPT IDs
are lowercase hyphenated UUIDs.

`verify-template` prints a JSON record with `disk_guid` and
`final_partuuids` (`efi`, `boot`, `root`). Before the VM install the latter is
empty; after the install it contains the guest-created GPT identities. The
scenario must compare them to the guest's install proof before `finalize`
rewrites the final PARTUUIDs to the Windows-assigned values.
The receipt and physical copier require the host-checked proof from the WHPX
scenario. For this Lenovo physical install, the CI image is a single ARM64
manifest, so pass its exact raw OCI manifest bytes to both `--ci-index-manifest`
and `--ci-child-manifest`. The first option means the top-level OCI object,
which can also be an index for synthetic VM tests. The copier refuses a
generic index for physical installation. Both commands compare the checked
guest image digest to the signed manifest digest, then independently hash the
installed target UKI and ARM64 EFI loader from the source FAT32 ESP. The
final readback emits `final_esp_partuuid`, `loader_path`, and `loader_sha256`
for the guarded BootNext helper to verify against the physical ESP.

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

## Firmware handoff

The physical copier deliberately does not mutate firmware. After it writes
and verifies `final-readback.json`, run the guarded Windows-native Imager
helper described in [Windows-direct BootNext](windows-direct-bootnext.md):

```text
imager.exe --tilo-final-bootnext --readback C:\path\to\final-readback.json
```

The helper verifies the final ESP, loader, and firmware readback, creates a
Tilo boot option for that ESP, and arms it for one boot. It refuses an occupied
`BootNext` and leaves the persistent Windows-first `BootOrder` unchanged.
Do not restart until the host install receipt and initial account and disk
unlock secrets have been saved. The development-signed Lenovo image requires
Secure Boot to be disabled in firmware for the first Tilo boot; keep the
Windows BitLocker recovery key available while changing that setting.

After the first successful physical Tilo boot, follow
[Windows-first firmware boot order](windows-first-tilo-boot-order.md) to add
the verified Tilo entry after Windows in the persistent picker order.
