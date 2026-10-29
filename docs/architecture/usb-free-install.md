# USB-free Full Tilo installation

Date: 2026-09-22
The implementation contract for installing Full Tilo from the running OS
without a USB drive. The product journey it serves is in
[`docs/guided-setup.md`](../guided-setup.md).

Design decisions:

- Windows and Linux hosts are built together on one shared staging core.
- Windows space comes from a **guided NTFS shrink** of the system volume, after
  explicit review. This replaces the "never resize" rule for the *host-side*
  Imager flow only. The live installer (`tilo-install`) still never resizes a
  filesystem; `docs/install-contract.json` keeps `resizesExistingFilesystems:
  false` for the installer and gains a separate Imager staging entry.
- Linux hosts use existing unallocated space only in this iteration.
- Secure Boot must already be disabled. Imager explains how and can restart into
  firmware settings; it never changes key policy.

## 1. End-to-end flow

```
Imager (Windows or Linux, unprivileged)
  choose Full Tilo → probe host → review plan (shrink, sizes, blockers)
  → download + verify internal bundle (existing worker)
  → consume: launch elevated staging helper with a request file
Elevated staging helper (same Imager binary, --tilo-staging-helper)
  revalidate probe facts → [Windows: shrink C:] → create staging ESP + staging root
  → write both component images → read back + hash → write handoff record
  → create owned Boot#### entry → set BootNext → record every step
Imager
  "Restart to install Tilo" → user restarts
Firmware consumes BootNext → staged live installer (tilo.install, root=tilo-auto → staging root)
  installer reads handoff, revalidates disk/partitions, preselects guided alongside
  in the reviewed free extent → user confirms → install → owned staging entry removed,
  BootNext → installed Tilo, BootOrder restored unchanged (Windows stays default)
Installed Tilo, first boot
  tilo-staging-reclaim: verify staging identities from the reclaim record, delete
  the staging partitions, grow the adjacent Tilo root into them
```

Nothing before the "consume" step writes to disk or firmware. Every privileged
mutation is recorded in the host transaction record *before* it is attempted and
marked done *after* it is read back.

## 2. Disk layout

The plan places everything in one contiguous extent. On Windows it is carved from
the end of the shrunken system volume. On Linux it is an existing unallocated
extent.

```
[ … existing partitions … | C: (shrunk) | target free extent | staging ESP | staging root | … e.g. WinRE … ]
                                         ^ installer's guided   ^ written by Imager
                                           alongside uses this
```

- **Target free extent**: left unallocated. The live installer creates the target
  ESP (2 GiB) and target root inside it with the existing `--guided-alongside`
  mode. It must be ≥ 34 GiB (installer minimum). The default is 64 GiB. The user
  can pick more, up to what the shrink allows.
- **Staging ESP**: GPT type ESP `c12a7328-f81f-11d2-ba4b-00a0c93ec93b`, name
  `Tilo installer boot`, size = bundle `minimum_partition_bytes` of `staging-esp`.
- **Staging root**: GPT type Tilo staging root `d4c4daf6-f72c-43ee-b8e3-5ba32550311d`,
  name `Tilo installer`, size = `minimum_partition_bytes` of `staging-root`
  (≥ 40 GiB, the live root floor). See "Root discovery" below for why it has
  its own type.
- Staging sits immediately *after* the target free extent. The target root ends
  where staging begins, so reclaim can grow the root in place.
- Alignment is 1 MiB for every start and size. Nothing is placed within 1 MiB
  of an existing partition.
- Neither staging partition gets a drive letter (Windows `-NoDefaultDriveLetter`,
  no `AssignDriveLetter`).

### Root discovery

Every Tilo UKI, live or installed, carries `root=tilo-auto`.
`os/usr/libexec/tilo-root-discover` resolves it in the initrd on the physical
disk that holds the ESP systemd-stub booted from (`LoaderDevicePartUUID`), and
each UKI kind selects only its own partition type:

| booted UKI | selects on the boot ESP's disk |
|---|---|
| installed (no `tilo.install`) | exactly one Tilo root (`7a2e8d75-…`); staging roots are never considered |
| live installer (`tilo.install`) | exactly one Tilo staging root (`d4c4daf6-…`) if the disk has any, else exactly one Tilo root (a removable live medium) |

Zero or several candidates of the selected kind fail the boot closed. From the
end of installation until `tilo-staging-reclaim` runs, the disk holds both the
staging root and the new Tilo root. The installed UKI boots the Tilo root. If the
staged installer boots again, for example after a failed attempt that already
created the target partitions, it still boots its own staging root. Neither
case can be ambiguous, so the installer does not have to retype or remove
anything before it reboots. The installer and reclaim identify the staging
root by this type as well as by its GUID and extent. The generic Linux type is
not enough: another distribution beside Tilo may have many partitions of that
type.

Space needed = staging ESP + staging root + target extent. On Windows the shrink
amount is `need − usable unallocated space already after C:`. It is refused if
it would leave C: with less than `max(20 GiB, 15 % of used)` free, or if it
exceeds `Get-PartitionSupportedSize` headroom.

## 3. Internal bundle (schema 2)

Schema 1 is retired: its command-line allow-list predates the tilo→tilo rename and
could never validate a real image. Schema 2 changes the following. The Rust
validator (`apps/imager/src/setup/internal_bundle.rs`) and the Python exporter
(`os/dev/internal-bundle.py`) must stay byte-for-byte in agreement.

- `purpose`: `tilo-internal-installer`. `release.rs` `InternalMetadata.purpose`
  matches it.
- Command-line tokens use the live image's real names: `tilo.install`,
  `tilo.mode=normal`, `tilo.generation=N`, `root=tilo-auto`,
  `rd.luks.options=tpm2-device=auto,fido2-device=auto`. The legacy `tilo.*`
  tokens, device-bound roots (`root=UUID=…`) and the retired `tilo.root=`,
  `tilo.luks=` and `tilo.unlock=` slots are rejected.
- The staging ESP carries the live UKI exactly as built and signed. The
  exporter does not rewrite its command line: the staging root is found
  through `root=tilo-auto` and its partition type (§2, "Root discovery").
  `boot_binding.uki_sha256` is the digest of the release's live UKI, and the
  manifest binds no root locator.
- Component `staging-root`: `byte_size` is the **minimised** btrfs image. The
  exporter shrinks the filesystem with `btrfs filesystem resize` to its minimum
  plus 1 GiB slack, then truncates. `minimum_partition_bytes` stays the live root
  floor (≥ 40 GiB). The live installer grows the filesystem to the partition at
  boot (see §6), so the download is roughly the used size, not 40 GiB.
- Component `staging-esp`: the exported ESP image. `minimum_partition_bytes` ≥
  `byte_size`.
- `boot_binding.efi_path`: the fallback loader path inside the staging ESP
  (`/EFI/BOOT/BOOTX64.EFI` or `/EFI/BOOT/BOOTAA64.EFI`). The exporter copies
  the unchanged live UKI there, so both paths boot identical bytes.

The writer zeroes the first and last 1 MiB of each staging partition before
writing. This prevents stale signatures (for example NTFS remnants from the
shrunk region) from being probed next to the real ones. Bytes beyond `byte_size`
are otherwise left as they are: the grown btrfs treats them as free space.

## 4. Elevated staging helper

Same executable, new mode `--tilo-staging-helper <request.json> <progress.jsonl>`.
Elevation reuses the write helper pattern in `apps/imager/src/write.rs`: `pkexec`
on Linux, `Start-Process -Verb RunAs` on Windows. The helper holds no UI.

### Request (written by the unprivileged UI, read-only to the helper)

```json
{
  "schema": 1,
  "operation_id": "<uuid v4>",
  "action": "stage" | "arm" | "remove",
  "release": { "release_id": "...", "manifest_sha256": "...", "architecture": "x86_64" },
  "manifest_path": "<verified cache path>",
  "component_paths": { "staging-esp": "<path>", "staging-root": "<path>" },
  "plan": { /* §4.2 reviewed plan, exactly as shown to the user */ },
  "plan_sha256": "<sha256 of canonical plan JSON>"
}
```

The native Full Tilo review remains distinct from prepared Linux setup. A
legacy native request may omit `profile` (the helper defaults it to an empty
map), `prepared_setup` and `migration`. The staged live installer then collects
and confirms account and startup choices itself; first-boot account creation
does not require a prepared bundle. This path does not promise migrated files,
accounts or an enrolled identity. Those require their separately verified
handoffs. A claimed prepared profile still requires actual encrypted custody;
it must never be fabricated to enter the native Windows route. The current
prepared-setup staging artifact is Linux-only.

Opening this review records intent, not installability: the native host probe,
architecture-specific internal release catalog and product release gate must
all pass before installation is offered. The review does not enable the gate
or qualify a Windows device.

The helper never trusts the request for authority:

1. It resolves `release` against `bundled_internal_releases()` compiled into the
   same binary. It then hashes the manifest file itself and requires
   `manifest_sha256` to match.
2. It re-probes the host (§5) and requires the probe facts the plan was computed
   from (disk identity, partition map, C: size/offset, free extents, firmware
   facts) to equal the plan's `basis`. Any difference means refusal with
   `plan-stale`, and the UI re-reviews.
3. It streams each component from its path while hashing. A digest mismatch
   means refusal before any partition is created, because the first pass is a
   pure verification read. The second pass writes to disk and hashes again, and
   a readback pass hashes the partition bytes `[0, byte_size)`.

### Plan (`staging::plan::Plan`)

```json
{
  "basis": { "disk": { "guid": "...", "size_bytes": N, "sector_bytes": 512, "id": "<platform id>" },
             "partitions": [ { "guid": "...", "type": "...", "first_lba": N, "last_lba": N } ],
             "system_volume": { "partition_guid": "...", "size_bytes": N, "supported_min_bytes": N },
             "firmware": { "uefi": true, "secure_boot": false, "boot_current": "0001",
                           "boot_order": ["0001","0000"], "boot_next": null } },
  "shrink": null | { "partition_guid": "...", "from_bytes": N, "to_bytes": N },
  "target_extent": { "first_lba": N, "last_lba": N },
  "staging_esp":  { "first_lba": N, "last_lba": N, "partition_guid": "<pre-generated>" },
  "staging_root": { "first_lba": N, "last_lba": N, "partition_guid": "<pre-generated>" },
  "boot_entry": { "description": "Tilo installer", "loader_path": "\\EFI\\BOOT\\BOOTX64.EFI" }
}
```

Partition GUIDs are generated at plan time, so identities are known before
creation. They are recorded before the mutation, and creation is idempotent
under resume.

### Host transaction record

- Windows: `%ProgramData%\Tilo\staging\<operation_id>.json`, with the ACL
  restricted to Administrators and SYSTEM.
- Linux: `/var/lib/tilo-imager/staging/<operation_id>.json`, mode 0600, owned by
  root.
- A copy of the final record goes onto the staging ESP at
  `/EFI/tilo-staging/record.json`.

```json
{ "schema": 1, "operation_id": "...", "plan_sha256": "...", "plan": { ... },
  "release": { ... }, "steps": [
    { "step": "shrink" | "create-esp" | "create-root" | "write-esp" | "write-root"
              | "verify" | "handoff" | "boot-entry" | "boot-next" | "remove-…",
      "state": "intended" | "done" | "failed", "detail": { ... }, "at": "<rfc3339>" } ],
  "original_firmware": { "boot_order": [...], "boot_next": null,
                         "entries": { "0001": "<hex load option>" } },
  "owned_boot_entry": null | { "number": "0004", "load_option_sha256": "..." } }
```

Writes are atomic (temp file, fsync, rename, fsync directory). The helper
refuses to start a new `stage` while any record for the disk has a step in
state `intended`, unless the action is `remove` for that operation.

### Actions

- `stage`: runs the steps in order through `handoff`, then `boot-entry`, then
  `boot-next`. It is resumable: `done` steps are re-verified and skipped, and an
  `intended` step is re-checked against actual disk/firmware state before it is
  retried.
- `arm`: re-verifies staging (identities plus readback digests) and sets
  BootNext again. This is used after a cancelled or failed installer boot.
- `remove`: deletes the owned boot entry (only if its bytes still equal the
  recorded `load_option_sha256`) and clears BootNext if it still points at it.
  It deletes both staging partitions (only if GUID, type and extent match the
  record). On Windows it extends C: back by the recorded shrink when the freed
  space is contiguous. Each step is recorded.

### Firmware adapter (shared encoding, platform I/O)

- `staging::efi` encodes and decodes an `EFI_LOAD_OPTION`: attributes
  `LOAD_OPTION_ACTIVE`, a UTF-16LE description, and a device path of
  `HD(partition number, start LBA, size LBA, GPT partition GUID, sig type 2)` +
  `File(path)` + `End`. It is pure Rust and unit-tested against byte fixtures
  produced by `efibootmgr`.
- Boot number: the lowest free `Boot####` ≥ `0x0100` that is not referenced by
  BootOrder and not present.
- `BootOrder` is **never written**. `BootNext` must be absent before arming,
  unless it equals our own owned entry; otherwise the helper refuses with
  `boot-next-busy`.
- Linux I/O goes through efivarfs (`/sys/firmware/efi/efivars/<Name>-8be4df61-93ca-11d2-aa0d-00e098032b8c`,
  4-byte attribute prefix `NV|BS|RT` = 0x07). Windows I/O goes through
  `GetFirmwareEnvironmentVariableExW` / `SetFirmwareEnvironmentVariableExW`
  after enabling `SeSystemEnvironmentPrivilege`. Every write is read back and
  compared byte-for-byte.

### Partition adapter

- Windows: PowerShell Storage cmdlets run from the elevated helper: `Get-Disk`,
  `Get-Partition`, `Get-PartitionSupportedSize`, `Resize-Partition`,
  `New-Partition -DiskNumber -Offset -Size -GptType -NoDefaultDriveLetter`.
  `New-Partition` cannot choose the partition GUID, so on Windows
  `plan.staging_*.partition_guid` is `null`. The `create-*` step records the
  GUID it reads back, and it records the intended exact offset and size before
  it creates anything. Resume then identifies an interrupted creation by exact
  offset, size and type. Linux pre-generates the GUIDs. Raw writes open
  `\\.\PhysicalDrive<N>`. Each write is aligned and lies entirely within an
  unmounted partition that has no volume.
- Linux: `sfdisk --append` with an explicit start, size, type, name and uuid,
  under `flock` on the disk, followed by `partx -u`. Raw writes go through
  `/dev/disk/by-partuuid/<guid>` opened `O_DIRECT|O_EXCL`.

## 5. Host probe (`staging::probe`)

It is unprivileged where possible. It produces `HostFacts` and a list of blockers
with stable IDs; each blocker has a fluent string and, where one exists, a
remedy action.

| Blocker ID | Condition | Remedy offered |
| --- | --- | --- |
| `not-uefi` | legacy BIOS or CSM boot | none (unsupported) |
| `secure-boot-on` | Secure Boot enabled | explain; "Restart to firmware settings" (Windows `shutdown /r /fw`, Linux `systemctl reboot --firmware-setup`); on Windows with BitLocker, suspend protection for one reboot first |
| `bitlocker-locked` | system volume encrypted and locked, or protection cannot be suspended | explain |
| `not-gpt` | system disk is MBR | none |
| `disk-unsupported` | Storage Spaces, dynamic disk, RAID/Intel RST, removable, or a virtual disk that is not the boot disk | none |
| `hibernation` | hibernate file present or Fast Startup enabled (Windows) | explain; "Turn off Fast Startup" (`powercfg /h off` needs an admin prompt) |
| `insufficient-space` | cannot fit staging + 34 GiB target even after the maximum safe shrink | show numbers |
| `on-battery` | on battery below 50 % | ask to plug in |
| `pending-operation` | an unfinished staging record exists | "Continue" / "Remove Tilo installer" |
| `arch-mismatch` | no bundle for this architecture | none |

`Capabilities.internal_dual_boot` becomes `true` when the probe runs on a
supported platform and finds no blocker other than the ones that have a remedy.
The Full Tilo option then appears, and remedy blockers are shown on Review.

## 6. Live installer internal mode

The live installer booted from staging identifies internal mode by
`LoaderDevicePartUUID` equal to a staging ESP PARTUUID whose
`/EFI/tilo-staging/handoff.json` names that same PARTUUID as `staging_esp`.

Handoff (`/EFI/tilo-staging/handoff.json`, written by the helper after verify):

```json
{ "schema": 1, "operation_id": "...", "host_os": "windows" | "linux",
  "disk_guid": "...", "disk_size_bytes": N,
  "staging_esp": { "partuuid": "...", "first_lba": N, "last_lba": N },
  "staging_root": { "partuuid": "...", "first_lba": N, "last_lba": N },
  "target_extent": { "first_lba": N, "last_lba": N },
  "owned_boot_entry": "0004",
  "original_boot_order": ["0001","0000"],
  "profile": { /* existing media_profile fields: locale, keymap, timezone, account name */ } }
```

The handoff is a **hint, not authority**:

- `tilo-install` gains `--internal-handoff <path>`. In that mode it replaces the
  "live medium's own disk" refusal with checks that all hold together:
  1. the live root and live ESP are exactly the handoff's staging partitions;
  2. the target disk GUID and size match;
  3. only `--guided-alongside` is accepted (whole-disk erase and prepared
     alongside are refused on the live disk);
  4. the guided extent equals `target_extent`, is still unallocated, and does not
     overlap either staging partition.
  The existing whole-disk refusal is unchanged for USB media.
- `crates/session/installer/src/disks.rs` stops disabling the live disk only in
  internal mode, and only offers guided alongside on it. The UI preselects the
  extent and the profile fields, and the user still reviews and confirms.
- The live root grows at boot. `tilo-installer-setup.service` runs
  `btrfs filesystem resize max /sysroot` when the root filesystem is smaller
  than its partition. This is a no-op on USB media whose filesystem already
  fills the partition.
- After a successful install (in internal mode only):
  - `tilo-install-boot-menu firmware-restore` restores BootOrder exactly as today;
  - the owned staging `Boot####` is deleted (if its bytes still match);
  - BootNext is set to the installed target's entry so the next restart
    completes first login;
  - a reclaim record (`/var/lib/tilo/staging-reclaim.json`: disk GUID, both
    staging PARTUUIDs and extents, target root PARTUUID) is written into the
    target.
- Cancel in the installer: "Return to Windows/Linux" reboots. BootNext has
  already been consumed, so the retained OS boots, and Imager then offers "Try
  again" (`arm`) or "Remove Tilo installer" (`remove`).

## 7. Staging reclaim on the installed system

`tilo-staging-reclaim.service` runs once on the installed system, after first
login succeeds. With the reclaim record present and every identity matching
(disk GUID, PARTUUIDs, types, extents, and staging not mounted), it:

1. deletes both staging partitions;
2. if the target root partition ends exactly where the staging ESP began, grows
   it to the end of the freed extent; then grows LUKS (`cryptsetup resize`) and
   btrfs (`btrfs filesystem resize max /`);
3. removes the record.

Any mismatch leaves the partitions untouched and logs why.

## 8. Imager UI and actions

- The Review page for Full Tilo shows:
  - the plan: how much C: shrinks, the Tilo size (a stepper within bounds), and
    the temporary installer space that is returned to Tilo later;
  - the blockers with their remedies;
  - the download card (existing);
  - an **Install** button, enabled only when the download is Ready and nothing
    blocks.
- Install runs the helper `stage`, shows helper progress (`progress.jsonl`: one
  JSON object per line `{step, done_bytes, total_bytes}`), and then offers
  **Restart now** / **Restart later**.
- A pending record at startup shows a resume card: Continue / Try again / Remove.
- Every command is also a resource action in `setup/actions.rs`:
  `setup.internal.set-target-size`, `setup.internal.install`,
  `setup.internal.restart`, `setup.internal.retry`, `setup.internal.remove`,
  `setup.internal.firmware-settings`, `setup.internal.disable-fast-startup`.
  These are in addition to the existing download actions.

## 9. Verification

- Unit tests:
  - plan computation (placement, alignment, shrink bounds, every refusal);
  - EFI load option encode/decode against fixtures;
  - record state machine and resume;
  - handoff validation;
  - schema-2 bundle validation in Rust and Python against the same fixtures.
- Linux loop-device test (root, CI `runner-disk-100g`): stage onto a loop disk
  with a fake efivarfs directory, then check partition map, readback digests,
  load option bytes and `remove`.
- `os/tests`: internal-mode `tilo-install` guards against a loop disk, and
  reclaim against a loop disk.
- VM round trip (`os/dev/test-internal-install-cycle.sh`, KVM runner): an OVMF
  guest with a real retained Fedora on one virtual disk runs `imager
  --tilo-staging-helper` for real, reboots into the staged installer, installs
  guided alongside, reboots into Tilo, confirms reclaim, then cold-boots the
  retained OS with its BootOrder unchanged.
- Windows: the helper's probe and plan run on the Windows CI runner in read-only
  mode. The destructive round trip needs a Windows guest image on a KVM runner.
  Until that exists, Windows support stays **unverified**, and
  `docs/migration-status.md` says so.

The product gate in `setup/view.rs` enables Full Tilo only when all of the
following hold: the probe passes, the catalog contains an internal release for
the host architecture, and the build was produced with `TILO_IMAGER_INTERNAL_INSTALL=1`,
which is set only by release jobs whose round trip passed. Development builds can
opt in with the environment variable `TILO_IMAGER_ALLOW_INTERNAL_INSTALL=1`.
