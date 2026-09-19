# Internal Installer Boot Proof Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Export an authenticated internal installer bundle and prove a real retained Linux OS can boot the staged qOS installer once from the same disk and cold-boot back with its persistent default unchanged.

**Architecture:** Export partition payloads and explicit boot bindings from the existing real bootc installer image. A disposable-VM harness performs staging in reviewed free space and measures the firmware round trip. This proof does not grant the product app permission to modify physical disks or enable Full OS installation.

**Tech Stack:** Python standard library, existing image/UKI tooling, Rust serde/SHA-256, QEMU/UEFI, native Linux block and EFI tools.

**Spec:** `docs/superpowers/specs/2026-09-16-guided-migration-design.md`; technical evidence and proof requirements in `docs/superpowers/specs/2026-09-16-internal-install-feasibility.md`.

## Global Constraints

- Preserve the existing operating system and its startup default.
- Do not resize existing filesystems or overwrite occupied partitions.
- Keep ordinary removable-media installation's whole-live-disk refusal intact.
- Mounting an image is not proof that its installer can boot internally.
- Do not enable Full OS from architecture or operating-system detection alone.
- Reject Secure Boot enabled, legacy BIOS, ambiguous identities, changed geometry, and conflicting one-time firmware requests before mutation.
- Use a real retained OS and real qOS installer; placeholder loaders and stub installer UI do not satisfy the proof.
- Target formatting, authenticated installation authorization, Windows staging, synchronization, and physical-hardware validation remain required subsequent work.

## File boundaries

`os/dev/internal-bundle.py` owns offline export/inspection and schema validation.
`os/tests/internal_bundle_test.py` owns export/parser fixtures.
`apps/imager/src/setup/internal_bundle.rs` validates embedded catalog selections and component digests without writing devices.
`os/dev/test-internal-boot-cycle.sh` orchestrates a disposable VM and gathers evidence.
`os/dev/internal-boot-guest.sh` performs the narrowly scoped guest staging/firmware operations.
`os/tests/internal_boot_evidence_test.py` tests acceptance of measured evidence.
No changes to `qos-install` target-write policy belong in this plan.

## Task 1: Actual boot-image inspection and versioned export

**Files:** Create `os/dev/internal-bundle.py`, `os/tests/internal_bundle_test.py`; use `os/dev/uki-cmdline.py` and existing build tools without changing their default output.

**Interfaces:** CLI `python3 os/dev/internal-bundle.py export --image <qcow2> --output <fresh-directory> --release <id>` and `inspect --manifest <path>`. Schema1 manifest has purpose `qos-internal-installer`, release_id, architecture (`x86_64`/`aarch64`), firmware `uefi`, secure_boot `disabled`, installer_protocol `1`, and components. Components bind role (`staging-root`/`staging-esp`), relative filename, exact byte_size, sha256, partition type GUID, filesystem type/UUID and minimum partition bytes. Boot binding records the owned EFI executable path, measured UKI command line, and exact root locator. All fields have bounded lengths; unknown fields, unsupported roles and duplicate roles are errors.

- [ ] Define `valid_fixture() -> dict` in the test module from an exported small GPT fixture and assert it passes `validate_manifest(manifest: dict) -> None` before mutating individual fields. Write fixtures for duplicate roles, overlapping source extents, image truncation, path traversal, zero component length, wrong root UUID, architecture mismatch and unrecognized command-line binding. Use real small GPT partition images for export range tests.

```python
def test_component_path_cannot_escape_bundle():
    manifest = valid_fixture()
    manifest["components"][0]["filename"] = "../outside.img"
    with unittest.TestCase().assertRaises(ValueError):
        validate_manifest(manifest)
```

- [ ] Run `python3 -m unittest discover -s os/tests -p internal_bundle_test.py` to establish RED. Implement bounded JSON parsing and range validation with integers checked against actual image size.
- [ ] Convert the caller-supplied qcow2 into an exclusive temporary raw image, attach it read-only, and inspect the actual GPT/filesystem/EFI artifacts. Export only the exact partition extents, never a guessed sparse allocation. Detach only the loop mapping created by this invocation on every exit.
- [ ] Inspect real UKI metadata and root discovery bindings. Accept only an explicitly understood binding that resolves the exported root unambiguously. Do not infer it from filenames, comments, labels or placeholder command-line space. If specialization is needed, use the existing UKI tool on exported copies, record the result, and reject incompatible layouts. Factory Secure Boot compatibility is not claimed.
- [ ] Hash exported bytes and publish the manifest only after every component and boot-binding check succeeds. Output directory must be new; no overwrite/reuse of an existing export. A local export is not automatically a trusted released catalog entry.
- [ ] Build/inspect the real installer with the existing non-stub build path in a disposable Linux builder. Capture tool versions and command-line/root evidence. Run export tests and commit scoped files; absent real image evidence keeps Task1 incomplete.

## Task 2: Host-side bundle validation without device writes

**Files:** Create `apps/imager/src/setup/internal_bundle.rs`; modify setup/mod.rs; add purpose/layout fields to the authenticated release catalog only through typed backward-compatible media variants.

**Interfaces:** `validate_internal_bundle(manifest: &[u8]) -> Result<InternalBundle, String>`; `verify_internal_components(bundle: &InternalBundle, directory: &Path, cancelled: &AtomicBool) -> Result<(), String>`. Consume schema1 from Task1. `InternalBundle` is data, not a device-access or installation grant.

- [ ] Add RED tests for malformed/unknown schemas, media-versus-internal purpose confusion, unknown firmware mode, duplicate roles, escape paths, digest mismatch and missing components.

```rust
#[test]
fn media_payload_cannot_be_used_as_internal_bundle() {
    assert!(validate_internal_bundle(br#"{"schema":1,"purpose":"removable-media"}"#).is_err());
}
```

- [ ] Decode into strict bounded types matching the exporter. Verify component content through pinned regular-file handles; reject links, replacements, wrong lengths and digests. Check cancellation during hashing. Do not run, mount or mark downloaded files executable here.
- [ ] Reuse existing authenticated download transport where appropriate without weakening media verification or treating a checksum fetched beside a file as an independent trust anchor. Checked-in catalogs remain explicitly unreleased until real artifacts are produced and authenticated.
- [ ] Run `cargo test --locked -p imager --lib setup::internal_bundle` and existing release-download tests. Verify the actual Task1 bundle through this code. Commit only reviewed task files.

## Task 3: Disposable retained-OS internal boot round trip

**Files:** Create `os/dev/test-internal-boot-cycle.sh`, `os/dev/internal-boot-guest.sh`, `os/tests/internal_boot_evidence_test.py`; extend `docs/regression/2026-09-16-guided-setup.md` with measured results.

**Interfaces:** Harness requires `--retained-image`, `--bundle`, and `--output` arguments. It makes a disposable clone of the retained image and a private firmware-variable store. Never accept a host block device. Guest staging consumes exact reviewed free GPT extents and verified bundle identities. The Python evidence validator exposes `validate_complete_cycle(evidence: dict) -> bool`, returning false for missing or contradictory proof. Evidence JSON records boot IDs, partition maps/UUIDs, retained partition byte digests, existing EFI file digests, firmware BootOrder/BootNext, owned entry identity, manifest digest and terminal outcome.

- [ ] Define `measured_fixture() -> dict` from a complete evidence sample, asserting all required keys and a passing baseline before removing/mutating one fact. Add evidence-validator RED cases: fake EFI sentinel only, same boot ID claimed twice, changed retained bytes, changed BootOrder, missing installer observation, unsupported firmware and missing return boot.

```python
def test_installer_observation_without_return_boot_is_incomplete():
    evidence = measured_fixture()
    evidence.pop("retained_return_boot")
    assert not validate_complete_cycle(evidence)
```

- [ ] Boot the real retained Linux image first and record its identity/default firmware state. Require an already unallocated region large enough for staging root and ESP; do not shrink or mutate the active developer VM to manufacture space.
- [ ] Through the guest helper, re-read disk identity and geometry immediately before allocating exactly the staging partitions. Refuse any mismatch. Verify all component bytes after writes and confirm retained partition/EFI digests remain unchanged.
- [ ] Create a uniquely owned EFI option on the dedicated staging ESP using `efibootmgr --create-only` (native Fedora efibootmgr18 confirms this option leaves BootOrder untouched). Snapshot/read back firmware state, reject an existing BootNext, and arm only that owned entry. Persistent BootOrder must equal its original bytes; do not temporarily reorder it as a side effect of entry creation.
- [ ] Reboot and prove the actual qOS installer process is running from the staged root on the same disk. Record command line, root identity, boot ID and installer UI observation. Do not start target installation or change the live-disk refusal to make this proof pass.
- [ ] Cold-boot again and prove the retained OS returned through its original persistent default. Re-read firmware state, partition geometry, retained byte hashes and existing EFI hashes. Retain evidence and staging until both observations are complete.
- [ ] Inject invalid component bytes, changed geometry, missing staging data, unexpected BootNext and interruption immediately before/after arming on separate disposable clones. Record actual completed boundaries; never infer rollback from an exit code or automatically replay partition writes.
- [ ] Run evidence-validator tests and the real cycle. A simulated firmware command or fake retained loader cannot substitute for real UEFI and retained-OS boot. Commit harness and measured evidence only after inspection.

## Completion boundary and next required work

A successful proof establishes one explicitly measured Linux/firmware/image combination. It does not enable the user-facing Full OS route. Before that route becomes actionable, implement privileged Linux/Windows host adapters, authenticated reviewed transactions and owner recovery outside target partitions, protected same-disk installation, first-login continuation, one-time onboarding boot, persistent-default choice and native Windows/physical-machine proofs. Preserve the original full Windows/Linux objective while these components are delivered sequentially.
