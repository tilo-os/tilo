# Guided Folder Copy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver real, verified, resumable one-time folder copying for setup and later folder import.

**Architecture:** A non-privileged migration worker consumes explicit source and destination grants, not the wizard's estimates. It pins directory handles, rejects links and unsupported entries, streams files into new temporary destinations, verifies them, and publishes without overwriting existing content. Durable per-file receipts support restart; receipts are hints to revalidate, never authority to overwrite.

**Tech Stack:** Rust, existing SHA-256/serde/tempfile dependencies, pinned no-follow directory access from the setup scanner work.

**Spec:** `docs/superpowers/specs/2026-09-16-guided-migration-design.md`

## Global Constraints

- Copy preserves original files and does not propagate changes after completion.
- No collaboration sharing or upload permission follows from migration.
- Filesystem enumeration is an estimate, not authority to access or overwrite.
- A source that cannot be read safely produces an explicit incomplete result.
- Every published file must be verified; conflicts preserve both versions.
- This component does not implement two-way synchronization or replace native installation work.

## Prerequisite

The scanner's reviewed no-follow traversal must be finished first. Reuse or extract
its handle-opening primitive without weakening it. Do not copy the vulnerable
pathname-check-then-open pattern. Destination traversal needs the same protection.

### Task 1: Verified copy into a fresh destination

**Files:** Create `apps/imager/src/setup/migration.rs` and tests in that module;
modify `setup/mod.rs`. Extract a shared focused no-follow helper only if needed.

**Interface:** `copy_folder(source: &Path, destination: &Path,
cancelled: &AtomicBool, progress: impl FnMut(CopyProgress)) -> Result<CopyReport, String>`.
Destination is the exact new import directory, not an existing directory to merge
into. `CopyProgress` reports verified files/bytes and current relative path.
`CopyReport` lists verified files and explicit skipped/failed relative paths;
`complete()` is true only when every selected regular file and directory was
handled and the scan completed without interruption or unsupported content.

- [ ] Write a real-filesystem failing test with source `nested/hello.txt` containing
  `hello`, an empty directory, and a fresh destination. Assert destination bytes,
  empty-directory existence, unchanged source bytes, five verified bytes, and
  complete status. The production break caught is a missing or corrupted copy.
- [ ] Write refusal tests for source/destination overlap, existing destination,
  symlink source, replaced destination parent, and cancellation. Assert that
  pre-existing sentinel bytes remain unchanged and no success is reported.
- [ ] Run `cargo test -p imager --lib setup::migration` and record the failures.
- [ ] Pin source and destination parent directory identities; create the exact
  fresh destination with exclusive semantics. Traverse by no-follow handles.
  Never form an unchecked source/destination access from an absolute queued path.
- [ ] Stream each regular file through a bounded buffer into an exclusive temp
  file. Hash bytes during copy, flush/sync, read the destination back and compare
  the digest. Revalidate source identity, length, metadata and content before
  publication; a changed source is incomplete and retryable, not a successful
  snapshot. Use no-clobber publication within the pinned destination directory.
- [ ] Check cancellation between chunks and entries; remove only known temporary
  files owned by this operation. Retain verified output for explicit resumption.
- [ ] Report symlinks, special files, metadata fidelity limitations, unrepresentable
  names, and access errors explicitly. Do not execute imported files. Keep
  timestamps where the filesystem permits; report failures that affect fidelity.
- [ ] Add corruption/source-change tests against the actual copy/publish helpers,
  using real files and controlled operation boundaries rather than a fake copy
  implementation. Run the scoped tests and clippy; commit only this component.

### Task 2: Durable resume and conflict-safe recovery

**Files:** Create `apps/imager/src/setup/migration_journal.rs`; modify migration.rs.

**Interface:** A versioned local `MigrationJournal` records operation identifier,
source/destination identities, relative component paths, verified sizes/digests,
and completed outcomes. No secret or privilege grant is serialized. Expose
`resume_folder(journal_path: &Path, cancelled: &AtomicBool,
progress: impl FnMut(CopyProgress)) -> Result<CopyReport, String>`.

- [ ] Write a failing restart test: copy two real files, interrupt after one is
  durably published, reopen the journal, resume, and verify both destination
  contents and unchanged originals. Already verified output must be revalidated.
- [ ] Add modified-destination, modified-source, malformed-journal, replaced-root,
  path-traversal, and missing-source tests. A changed destination is a conflict;
  it must not be silently replaced based on a receipt.
- [ ] Write journal updates atomically with owner-only permissions and a version
  bound to exact schema decoding. Validate every relative component and path
  length before use. Validate stored root identities against current open handles.
- [ ] On resume, verify existing content against receipts before skipping it.
  Never derive authority from a matching filename, label, or progress counter.
  If a source changed, copy its new version to a separately named fresh output
  or expose a conflict requiring a choice; do not overwrite either version.
- [ ] Test malformed/replaced journals and interruption at publication/journal
  boundaries. Verify an unjournaled output is treated as an existing conflict,
  never erased as cleanup. Run scoped tests, clippy, and commit.

### Task 3: Worker integration and later imports

**Files:** Create `apps/imager/src/setup/migration_worker.rs`; modify setup view and
local setup persistence once their reviewed interfaces exist.

**Interface:** Worker owns cancellation and progress channels and calls the real
copy/resume engine. UI subscribes to outcomes; losing a worker channel produces
an unknown/interrupted state, never completion.

- [ ] Add tests for cancellation, channel disconnection, explicit retry, and
  completion only after a verified report. Exercise the actual worker with temp
  folders. Do not spawn dialogs or privileged helpers in tests.
- [ ] Wire Copy to the worker only after source and target grants are available.
  A full-OS plan before reboot is not a mounted destination; keep its copying
  pending until target-side migration can resolve retained volume identities.
- [ ] Show verified progress and pending folders in setup and later imports.
  Preserve the journal across application restart and require review when roots
  or selections change. Keep Connected unavailable until its separate real sync
  service is delivered.
- [ ] Test restart and retry through the application. Render progress, partial
  failure, conflict, and completion states with real worker results and inspect
  them at compact and normal viewport sizes.

## Follow-on requirements retained

Two-way synchronization still needs persisted baselines, source mount safety,
edit/delete and edit/edit conflicts, recoverable deletions with the 30-day policy,
disconnect/reconnect, case-collision handling, and both-OS validation. Cross-reboot
source mapping needs stable volume identity and native-to-target path translation.
The migration worker cannot infer these from a host pathname. The overall goal
also still requires the internal installer, Desktop installation, boot defaults,
packaging, and platform end-to-end evidence.
