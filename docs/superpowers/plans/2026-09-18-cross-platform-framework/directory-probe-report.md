# Task 4 bounded Windows directory diagnostic probe

Status: implementation and host verification artifact; **Task 4 provisioning is not complete**. No Windows executable, native identity-mismatch test, VM power cut, storage-loss experiment or architecture qualification was executed by this worker. Production first launch, directory-anchor establishment, wrappers and consumers remain unchanged.

## Delivered scope

The `directory_commit_probe` example is enabled only with atomic-file's default-off `test-support` feature. CLI/report serialization stays in the example; the public diagnostic contract carries ordinary owned strings/numbers, never native handles or a production durability authority. Native calls stay in `src/native/windows/directory_probe.rs`.

Required arguments are `--root`, `--profile local-ntfs`, `--anchor-evidence`, `--report`, `--case`, and `--access`. Root/evidence/report paths must be explicit absolute paths. Evidence and report must be outside the canonical test namespace. The report parent must already exist and have no link/reparse ancestors; the report is created exclusively and never overwrites an existing path. No directory is created by CLI preparation. Controller-owned output directories require ordinary exclusion against concurrent hostile path substitution; this example does not create an output-directory grant or race-proof host filesystem service.

The JSON evidence requires `volume_serial` (u64), `file_id` (exactly 32 hex digits), nonempty `establishment_record`, and nonempty `storage_profile`. Unknown JSON fields, malformed IDs and missing fields fail. The storage description should identify the disposable device/VM, cache setup, and how the controller discarded volatile writes. It is recorded as an **external fixture claim**, not measured hardware evidence or a verified production anchor. The held root's full volume/file identity must equal the supplied identity before any fixture mutation.

Implemented observations:

1. Native `RtlGetVersion` OS/build, native/process architecture, token elevation, all token privilege names/attributes/enabled states. Reject elevated tokens and enabled backup/restore/manage-volume/take-ownership/debug/TCB privileges; never enable a privilege. Existing `Principal::current` also refuses thread impersonation. Query failures remain failures, with requested-query events and terminal errors.
2. Admit the explicitly named drive directory and traverse every root component relative to a held handle with directory/no-follow options. Reject UNC/device paths, volume-root fixtures, traversal and reparse/non-directory objects. Record minimal and writable access opens with full sharing, compare identities, and report writable ancestor access/refusals above the fixture boundary without committing those ancestors. No writable volume handle is opened.
3. Record full held 128-bit identity and volume serial, attributes/reparse tags, owner/DACL SDDL and owner-private admission. Positive native `FileFsDeviceInformation` completion, local disk characteristics, NTFS and persistent ACLs are required. Filesystem flags are recorded without inheriting production regular-file publication's POSIX-rename capability prerequisite. Failed volume queries do not emit default device capabilities as observed facts.
4. Exclusively create a unique `directory-probe-PID-TIMESTAMP` namespace under the held admitted test root. Actual directory requests use `FILE_DIRECTORY_FILE | FILE_WRITE_THROUGH | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT`, full READ/WRITE/DELETE sharing, and creation-time protected owner DACLs. Record access/options/disposition, raw NTSTATUS, IOSB status/information and elapsed time. Successful creates must report FILE_CREATED; opens must report FILE_OPENED. No uncertain fixture is removed, including on errors.
5. Separate `direct`, `staged`, and `collision` candidates. Staged rename uses the existing native `FileRenameInfoEx` buffer with flags zero and a held destination parent, with no descendant opened before the empty-stage rename. Reopen the final name relative to the held parent and compare against the retained source identity. Collision probes both exclusive create and no-replace rename; inspect unchanged held/name identities for both winner and stage. Exact access/collision errors remain in the event log.
6. Resolve `NtFlushBuffersFileEx` at runtime through ntdll, recording export availability before use. Its dynamic function signature is compile-checked against the existing pinned windows-sys binding; no eager call import prevents the missing-export diagnostic. Invoke flags zero twice on each exact retained child and selected minimal/writable parent handle, recording raw status/IOSB, access, repeat and duration. Separately named Win32 `FlushFileBuffers` comparisons never replace native calls or certify durability. Failures are accumulated while the remaining bounded observations run; malformed/pending completion ends the experiment incomplete.
7. `replay` opens a controller-specified chain of 1..16 existing safe components, inspects each object and repeats child/parent barriers on every edge. No creation, rename, AlreadyExists shortcut, writable-ancestor shortcut, or previous success is used as durability evidence. `reparse` and `denied` use one controller-supplied root-relative fixture, record no-follow/access refusal and unchanged identities, and submit no mutating request.
8. Every event is sequence numbered, serialized as JSONL to stdout, and explicitly flushed before continuing. Events after create, rename, child flush, parent flush and candidate completion support externally captured crash cut points. Final report lists events, terminal result, `fully_observed`, `candidate_calls_succeeded`, outstanding prerequisites and `qualification: "unqualified"`. A complete negative flush result may have fully_observed=true and candidate_calls_succeeded=false; missing prerequisite/observation cannot produce a successful experiment. Expected refusal candidates can complete successfully only after observing the intended refusal and unchanged object identities.

The namespace's own creation/flush edge is also recorded. A failed namespace barrier does not mint a durable parent; this diagnostic may continue bounded calls to collect negative operational evidence. Every retained object is named in planned/create/held-object events; identities are included when actually observable. A failed identity query remains missing evidence, never a fabricated ID. Fixture cleanup belongs entirely to the external controller.

## Verification evidence

All cargo invocations used the existing locked dependency graph, `--locked --offline`, the workspace's normal target directories, and no environment override. No cargo fmt, broad shell/UI check, staging or commit was performed.

Host TDD:

- `rustc --edition=2024 --test crates/system/atomic-file/src/directory_probe.rs -o /tmp/directory-probe-contract-tests` followed by that executable: initial RED was 0 passed / 3 failed at assertions rejecting missing prerequisites, unsafe/unbounded replay components, and successful-but-incomplete outcomes. Implemented validation/outcome gating; rerun GREEN 3/3.
- Example regression `controller_evidence_inside_test_namespace_refuses_without_report_effect`: observed RED with 2 passed / 1 failed (the old execution accepted internal evidence and emitted native_subject_unavailable). Added canonical evidence separation/size validation; GREEN 3/3. An initial test path used macOS's `/var` symlink and therefore failed at an unrelated report ancestor check; canonicalizing the fixture exposed the intended failing assertion before the fix. Other CLI parsing/report checks are additional regression coverage, not claims of independent preimplementation native TDD.

Terminal commands/results:

| Command | Actual result |
| --- | --- |
| `cargo build -p atomic-file --example directory_commit_probe --features test-support --locked --offline` | Exit 0; host executable built |
| `cargo test -p atomic-file --example directory_commit_probe --features test-support --locked --offline` | Exit 0; 3 passed, 0 failed/ignored |
| `cargo test -p atomic-file --lib --features test-support --locked --offline` | Exit 0; 23 passed, 0 failed/ignored; 0.07s execution |
| `cargo check -p atomic-file --lib --locked --offline` | Exit 0; ordinary macOS library |
| `cargo check -p atomic-file --lib --target x86_64-pc-windows-msvc --locked --offline` | Exit 0; ordinary Windows library, repeated after diagnostic feature narrowing |
| `cargo check -p atomic-file --tests --example directory_commit_probe --features test-support --target x86_64-pc-windows-msvc --locked --offline` | Exit 0; repeated after complete pending-request ownership and final minimal-mask alignment |

A Python subprocess smoke harness invoked `target/debug/examples/directory_commit_probe` with an explicit `/private/tmp` host-only fixture and external evidence/report:

```json
{"help_exit":0,"missing_args_exit":2,"malformed_evidence_exit":2,"non_windows_exit":1,"existing_report_exit":2,"fixture_entries":0,"fully_observed":false,"qualification":"unqualified"}
```

It asserted malformed evidence created no report; valid non-Windows input emitted `native_subject_unavailable`; every fixture remained empty; and a second invocation preserved the existing report bytes exactly. Host-only harness temporary files were cleaned by the harness. This is parser/refusal evidence only.

A native Windows test, `mismatched_controller_identity_refuses_before_fixture_mutation`, was added and cross-checked. It requires `TILO_DIRECTORY_PROBE_TEST_ROOT` to name an already-provisioned ordinary-user protected owner-private local-NTFS fixture; it must observe the actual anchor mismatch event and no create/rename event. Missing fixture/admission is an assertion failure, never a skip. **It has not executed here.**

Final native test/example recheck after complete pending-request ownership passed with exit 0 (9.33s including lock wait). A subsequent final mask alignment removes FILE_LIST_DIRECTORY from the minimal/writable parent masks while retaining it for created child handles; its final Windows test/example compile passed with exit 0 in 1.18s. This matches the specified minimal traverse/read-attributes/read-control/synchronize authority rather than needlessly requiring ancestor listing access. Parent masks are 0x1200a0 (minimal) and 0x1201a6 (writable); the child mask is 0x1301a7.

## Files and dependency edges

New files:

- `crates/system/atomic-file/examples/directory_commit_probe.rs`
- `crates/system/atomic-file/src/directory_probe.rs`
- `crates/system/atomic-file/src/native/windows/directory_probe.rs`

Narrow wiring: `atomic-file/Cargo.toml`, `src/lib.rs`, `src/native/mod.rs`, `src/native/windows.rs`. Diagnostic `Win32_System_LibraryLoader` and `Win32_System_SystemInformation` features are enabled only by test-support, each corresponding to actual imports. Serde/serde_json are dev dependencies only.

`src/native/windows/acl.rs`: five inserted lines extract the unchanged owner/DACL admission body into `validate_private_security`. Existing production `validate_private` still first enforces the exact regular-file/one-link gate and then executes that same ACL body. No production permission relaxation was made. The ordinary Windows library and feature test targets compile; native Windows ACL regression execution remains unavailable. Unix production implementation is unchanged; the scoped macOS atomic-file library suite passed.

Compared with `task-4-probe-before/Cargo.lock`, the **entire lock diff** is exactly two dependency edges under atomic-file: `serde` and `serde_json`. No versions, checksums, resolutions or unrelated edges changed. No app-state/consumer/production provisioning API or successful Windows directory route was added.

## Native controller invocation

The external disposable controller must provision and independently establish the root, its protected owner DACL, identity and external establishment record before running this program. It must also provision a writable output location outside the filesystem under test, capture stdout outside that filesystem, and run a non-elevated account. The probe does not configure accounts, ACLs, disks, cache policy, or VM lifecycle.

Build on the native Windows architecture:

```powershell
cargo build -p atomic-file --example directory_commit_probe --features test-support --locked --offline
$probeExe = (Resolve-Path target\debug\examples\directory_commit_probe.exe).Path
$probeRoot = 'T:\DisposableStorageProbe\Anchor'
$probeEvidence = 'R:\StorageController\anchor.json'
$probeReports = 'R:\StorageController\reports'
```

These paths identify **controller-provisioned disposable example locations**, not default paths. Substitute the actual explicit fixture locations. `anchor.json` must carry the externally measured u64 `volume_serial`, full 32-hex-digit `file_id` (16 bytes), external establishment-record reference and storage-profile description. The values must not be copied from this document or invented. Report names must be new each run.

Finite create/rename/access matrix (stdout must be captured by the external controller):

```powershell
foreach ($probeCase in @('direct','staged','collision')) {
  foreach ($probeAccess in @('minimal','writable')) {
    & $probeExe --root $probeRoot --profile local-ntfs --anchor-evidence $probeEvidence --report "$probeReports\$probeCase-$probeAccess.json" --case $probeCase --access $probeAccess
    # Preserve LASTEXITCODE and the complete stdout/stderr stream per invocation.
  }
}
```

After a prior run/failure or a controller restart, obtain the exact retained namespace component from its captured `fixture_planned` event. Supply only existing components that the external controller intends to reconcile:

```powershell
$probeNamespace = 'directory-probe-PID-TIMESTAMP' # exact captured component
& $probeExe --root $probeRoot --profile local-ntfs --anchor-evidence $probeEvidence --report "$probeReports\replay-writable-new.json" --case replay --access writable --component $probeNamespace --component final
& $probeExe --root $probeRoot --profile local-ntfs --anchor-evidence $probeEvidence --report "$probeReports\replay-minimal-new.json" --case replay --access minimal --component $probeNamespace --component final
```

Controller-provisioned negative fixtures need names immediately below the anchor. The reparse fixture must be inspectable without following it; the denied fixture must permit minimal observation but deny the writable directory mask. Missing/unobservable fixtures are incomplete results:

```powershell
& $probeExe --root $probeRoot --profile local-ntfs --anchor-evidence $probeEvidence --report "$probeReports\reparse-new.json" --case reparse --access writable --component controller-reparse
& $probeExe --root $probeRoot --profile local-ntfs --anchor-evidence $probeEvidence --report "$probeReports\denied-new.json" --case denied --access writable --component controller-denied
$env:TILO_DIRECTORY_PROBE_TEST_ROOT = $probeRoot
cargo test -p atomic-file --lib --features test-support --locked --offline mismatched_controller_identity_refuses_before_fixture_mutation
```

Exit 0 means this selected diagnostic completed with its requested operations or expected refusals observed. Exit 1 means native subject/prerequisite/observation or candidate failure; inspect fully_observed and raw events. Exit 2 means input/evidence/report I/O failure. Help exits 0. No exit certifies durability. Minimal-access native flush refusal is a design observation, not a reason to widen privileges. Negative cases do not modify controller fixtures.

For power-loss testing the controller must abruptly stop the disposable guest/storage at externally received create/rename/child_flush/parent_flush/candidate_complete events, then independently inspect names/full identities/DACLs. Ordinary process kill tests recovery only. Controller cache-loss assumptions, hardware/filter coverage and production bootstrap remain separate required evidence. This artifact does not add descendant-open rename stress, payload power-loss qualification, faulted-storage recovery, or production provisioning; those require the later native qualification harness/protocol work.

## Self-review and remaining limits

- Successful native status alone is insufficient. IOSB sentinels, exact create/open dispositions, object identity/privacy and subsequent observations gate completion. Volume query failures do not become zero-valued capability evidence.
- All native create input/output addresses are owned for the call. An unexpected STATUS_PENDING retains the complete counted name, UNICODE_STRING, OBJECT_ATTRIBUTES, creation descriptor, output-handle slot, IOSB and duplicated existing parent authority until process exit. Outputs are not read while pending; no unproven returned handle is wrapped/closed, waited on or cancelled. Volume/flush pending output buffers likewise remain allocated. This bounded diagnostic fail-closed retention is not a production asynchronous-I/O strategy.
- Runtime export lookup avoids substituting Win32 flush or silently assuming the ntdll export. All native calls still need actual Windows execution; cross-compilation does not validate access/option compatibility or metadata durability.
- The ACL extraction preserves the production regular-file/link gate and unchanged security checks. The diagnostic directory kind check occurs before shared security admission.
- Reports and event streams distinguish complete negative operations from missing observation. Fixture identity/privacy query failure leaves retained names and an incomplete report; it does not silently delete objects or invent IDs.
- No native runtime qualification, root bootstrap proof, production API, wrapper migration, payload crash matrix or release CI dispatch is claimed. Independent review is requested before treating this probe implementation as accepted.

Primary request references checked: [NtCreateFile](https://learn.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-ntcreatefile) and [NtFlushBuffersFileEx](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/nf-ntifs-ntflushbuffersfileex). They identify the native directory options and normal data/metadata flush scope; they do not turn this unexecuted probe into directory namespace durability qualification.

## Review fix round 1 — observation completeness

Addressed both Important findings in `task-4-probe-review.md`. This round changes only `src/directory_probe.rs` and `src/native/windows/directory_probe.rs`, plus this report. No manifest, lockfile, production ACL helper, ordinary publication path, or external native qualification status changed.

The native directory request now returns a typed completed observation: an opened handle or a real failing native status. Pending/malformed results, input/admission failures and event-sink failures stay in the outer error result. Native refusal is constructed only after recording the call successfully and inspecting its negative returned NTSTATUS; it is not guessed from an `io::Error` kind or Win32 code. Mandatory opens retain a thin adapter that propagates either failure. Ancestor/collision policies consume the typed observation: an incomplete ancestor stops root admission before fixture mutation, and an incomplete collision cannot proceed to stage creation or a fully observed result. Completed ancestor refusals above the anchor and genuine collision refusals retain their existing diagnostic behavior. The denied-fixture branch uses the same typed native result, so an event sink returning Win32 code 5 cannot masquerade as a completed access refusal.

Flush classification rejects every unrecognized positive/informational NTSTATUS, including pending, as incomplete. Native STATUS_SUCCESS still requires initialized successful IOSB completion; real failing NTSTATUS remains a fully observed negative even if Windows left the IOSB sentinels untouched. Native resource-retention behavior is unchanged.

The parent explicitly authorized the directly related reparse extension during this round. The old `inspect(...).kind() == PermissionDenied` catch could also turn a failed SDDL snapshot or event write into apparent refusal evidence. Snapshot acquisition is now separate from intentional reparse admission: full identity/tag/owner-DACL observations and event writes must succeed, then a real observed reparse attribute permits the expected no-follow refusal event. Snapshot/sink errors always propagate. No generic error-kind classifier remains on that path.

Regression seams are the actual private policies called by the Windows ancestor, collision, reparse and flush paths, not test-only copies. Tests exercise those policies and the actual final-outcome mapper. The continuation markers in host tests establish that an incomplete observation stops the caller continuation; they are **host control-flow evidence**, not claims that a native syscall or native filesystem mutation ran.

Exact red/green evidence:

- `cargo test -p atomic-file --lib --features test-support --locked --offline directory_probe::observation`: after extracting the existing policies unchanged and wiring the real native callers, RED exited 101 with 1 passed / 3 failed / 23 filtered. Failures were ancestor continuation/completeness, collision completeness and positive/informational flush completeness. The real-native-refusal preservation case passed.
- Same command after the typed-policy fixes: GREEN exited 0 with 4 passed / 0 failed / 23 filtered. Session `30416` also completed `cargo check -p atomic-file --tests --example directory_commit_probe --features test-support --target x86_64-pc-windows-msvc --locked --offline` with exit 0 (0.96s check, after the host test's shared-lock wait).
- `cargo test -p atomic-file --lib --features test-support --locked --offline directory_probe::observation::tests::incomplete_reparse`: RED in session `32913`, exit 101, 0 passed / 1 failed / 27 filtered. A PermissionDenied snapshot error produced an incorrectly fully observed outcome under the extracted old policy.
- Final `cargo test -p atomic-file --lib --features test-support --locked --offline directory_probe::observation`: GREEN, 5 passed / 0 failed / 0 ignored / 23 filtered (0.49s build; 0.00s execution). Coverage includes incomplete errors with the same raw codes as legitimate native access/collision refusals, real observed reparse versus incomplete snapshot/sink errors, missing IOSB completion, unknown informational statuses, and genuine STATUS_ACCESS_DENIED as a complete negative.
- Final native compile check: `cargo check -p atomic-file --tests --example directory_commit_probe --features test-support --target x86_64-pc-windows-msvc --locked --offline` completed in session `70938` with exit 0 (0.57s). Sessions `30416`, `32913` and `70938` are all terminal; no live build remains.

No broad suite was repeated. Direct rustfmt touched only the two owned source files. Native Windows execution, abrupt storage loss and production anchor qualification remain unavailable and unclaimed.
