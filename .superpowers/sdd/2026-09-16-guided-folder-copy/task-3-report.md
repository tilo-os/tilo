# Task 3 report: worker integration and later folder imports

## Result

Implemented a callable later-folder import flow backed by the reviewed copy and
journal engine. The worker owns cancellation and progress delivery, classifies
only a complete verified `CopyReport` as success, and converts a lost event
channel into an interrupted outcome. Cancellation retains checked progress and
retry reopens the existing journal for engine revalidation.

The `--import-folders` route and setup's **Add folders now…** action open the
same real import surface. Users select an actual source folder and destination
parent, review the derived destination, then explicitly start the copy. Existing
operations and their actions appear first on compact screens. The UI reports
checked bytes/files, incomplete work, cancellation, interruption, conflicts,
and completion. Detailed per-path reasons and file-property limitations remain
available behind **Details**. Connected folders remain unavailable.

Full qOS setup does not treat its host-side plan as a mounted destination. Its
review explicitly keeps copying pending until target-side migration can resolve
retained volume identities and obtain real grants after installation.

## Local persistence and safety

Operation records live in private device-local app data rather than the
collaborative document. Native paths are encoded losslessly as Unix bytes or
Windows UTF-16 code units; unsupported platform representations are refused.
Opening state normalizes a formerly running operation to interrupted, but the
index never authorizes overwrite or resume. Retry still depends on the reviewed
journal's root and identity checks.

Private descendants are created through the existing pinned, no-follow
directory machinery. Existing parent permissions are checked and left
unchanged. The UI refuses to start where journal durability is unavailable,
including the current Windows implementation.

## TDD and verification

RED tests were added before each implementation slice:

- Worker tests initially failed to compile because `MigrationJob`,
  `MigrationWorker`, and terminal outcome types did not exist.
- Store tests initially failed because the local store and operation types did
  not exist. Early GREEN attempts exposed macOS `/private` canonicalization and
  APFS filename-fixture assumptions; fixtures were corrected to test the actual
  contract.
- Import tests initially failed because the application flow did not exist.
- Setup integration tests initially failed because the callable import command
  and Full qOS pending-copy semantics did not exist.
- Screenshot-state coverage initially failed because real worker-backed shot
  scenarios did not exist.

Final host evidence:

- `cargo test -p imager --lib`: **244 passed, 0 failed, 1 ignored**.
- `cargo test -p imager setup::import_view::tests --lib`: **4 passed**.
- The worker module has four real-temp-folder tests for verified completion,
  cancellation, channel loss, and explicit journal retry.
- The store module has five tests for interrupted restart, fresh review after
  changed grants, complete issue/disclosure round trips, non-UTF-8 Unix path
  round trips, and unchanged parent permissions/private descendants.
- `cargo build -p imager`: passed before deterministic screenshot rendering.
- `git diff --check` on Task 3 files: passed.
- `cargo clippy -p imager --lib --no-deps -- -D warnings` reports no Task 3
  findings. It remains nonzero on nine pre-existing findings in reviewed files
  outside this task:
  - `model.rs:285` — `needless_return`
  - `migration_durability.rs:48` — `borrow_as_ptr`
  - `migration_durability.rs:57` — `unwrap_used`
  - `migration_journal.rs:80` and `:191` — redundant method-call closures
  - `migration_journal.rs:348` — `unwrap_used`
  - `migration_journal.rs:541` — `unused_self`
  - `migration_journal.rs:838` and `:858` — missing `# Errors` documentation

Native Fedora 44 ARM64 evidence supplied by the root coordinator:

- `cargo test --locked -p imager --lib setup::`: **108 passed, 0 failed,
  2 explicitly ignored**.
- After the visual-review amendments,
  `cargo test --locked -p imager --lib setup::import_view::tests`: **4 passed,
  0 failed**.

The installed Windows Rust target could not compile dependencies on the macOS
host because `ring` requires the MSVC C environment (`assert.h` was missing).
Windows runtime validation remains a release gate; the application visibly
refuses unavailable journal durability instead of falling back to an unsafe
copy.

## Screenshot acceptance

The registered scenarios are `import-progress`, `import-partial`,
`import-conflict`, and `import-complete`. Each is derived from actual worker
events and temporary folders. Progress captures checked file progress; partial
uses an actually skipped symbolic link; conflict modifies a completed
destination then resumes the real journal; completion uses the verified terminal
report. The conflict test additionally proves the original source bytes and the
changed destination bytes are both preserved.

Render commands use:

```text
target/debug/imager --shots /tmp/<name>.png --scenario import-<state> --viewport 640x640
target/debug/imager --shots /tmp/<name>.png --scenario import-<state> --viewport 1180x820
```

All eight renders were inspected at 640×640 and 1180×820. The accepted images
are stored by the root coordinator under
`/Users/xaver/.codex/visualizations/2026/09/16/01a0aaae-f9fd-7760-8e38-cc0eb4ac8ed1/guided-import`.
The compact views keep status and actions above the new-copy form; raw issue and
file-property text is collapsed behind **Details**.

## Files changed

- `apps/imager/src/setup/migration_worker.rs`
- `apps/imager/src/setup/migration_store.rs`
- `apps/imager/src/setup/import_view.rs`
- `apps/imager/src/setup/view.rs`
- `apps/imager/src/setup/mod.rs`
- `apps/imager/src/main.rs`
- `apps/imager/src/lib.rs`
- `apps/imager/Cargo.toml`
- the Imager `dirs 6.0.0` dependency hunk in `Cargo.lock`

## Retained follow-ons

Files → Add folders and target first-login handoff still need their dedicated
typed integration after concurrent Files/shell work settles. Cross-reboot mapping
still requires validated native volume identity and target path translation.
Two-way synchronization, installation executors, and native Windows runtime
evidence remain separate work; this one-time copy does not claim those
capabilities.

## Review fix round 1

The Task 3 review found that the import action existed only on an unreachable
Review page when live installation capabilities were unavailable, and that a
retry kept displaying the preceding attempt's report instead of renewed
progress.

The setup Experience footer now exposes **Add folders now…** independently of
the disabled installation choices. The regression renders the initial 640×640
live-unavailable experience, verifies both install choices remain disabled,
semantically activates the real retained button, and then finds the source
picker on `ImportApp`. The deterministic `setup-experience` shot now uses these
truthful live capability gates.

`OperationRecord::mark_running` now clears attempt-local progress, report,
failure details and limitations only after approval/state checks succeed. The
journal remains the retry authority. New progress therefore reports the new
attempt's counts and current path, while old issues disappear from the active
card. The application restart test now verifies the reset immediately after an
explicit retry and still reaches checked completion.

### RED evidence

- `cargo test -p imager initial_unavailable_experience_can_activate_the_real_import_surface --lib`
  failed with `no addressable widget named "setup.open-imports"`.
- `cargo test -p imager retry_progress_replaces_the_prior_incomplete_attempt --lib`
  failed with the stale report count: `left: 1`, `right: 2`.

### GREEN evidence

- `cargo test -p imager initial_unavailable_experience_can_activate_the_real_import_surface --lib`:
  **1 passed**.
- `cargo test -p imager retry_progress_replaces_the_prior_incomplete_attempt --lib`:
  **1 passed**.
- `cargo test -p imager application_restart_requires_explicit_retry_and_reaches_verified_completion --lib`:
  **1 passed**.
- `cargo test -p imager screenshot_states_come_from_real_worker_events --lib`:
  **1 passed**, including the new real stop → explicit retry → renewed progress
  scenario.
- `cargo test -p imager 'setup::' --lib`: **112 passed, 0 failed, 1 explicitly
  ignored**.

The accepted fix-round renders are:

- `/tmp/qos-task3-fix1-shots/setup-experience-compact.png` — 640×640
- `/tmp/qos-task3-fix1-shots/setup-experience-normal.png` — 1180×820
- `/tmp/qos-task3-fix1-shots/import-retry-progress-compact.png` — 640×640
- `/tmp/qos-task3-fix1-shots/import-retry-progress-normal.png` — 1180×820

The retry screenshot is derived from a real copy, safe stop, explicit journal
retry and renewed worker progress. Visual review confirmed the reachable initial
action and fresh retry counters with no stale issues.

Root-coordinated fix-round evidence:

- Fedora 44 ARM64 `cargo test --locked -p imager --lib setup::`: **109 passed,
  0 failed, 2 explicitly ignored**.
- The exact-source Windows worker/store plus copy/journal `--tests` cfg check:
  **exit 0**. Its harness emitted five expected dead-code warnings because UI
  callers were deliberately excluded. This is compile evidence for the focused
  modules, not a full Windows UI or runtime claim.
- Re-running the focused host Clippy command produced only the same nine
  pre-existing diagnostics listed above and no fix-round findings.
