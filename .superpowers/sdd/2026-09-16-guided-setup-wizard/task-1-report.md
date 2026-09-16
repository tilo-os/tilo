# Task 1 report: setup controller and folder discovery

Implemented the process-local guided-setup controller, point-in-time folder
scanner, and asynchronous native folder chooser. The controller keeps choices
outside collaborative state, defaults to Copy, rejects unsupported experiences,
retains remembered folders while Later exposes no active migration, preserves
per-folder overrides, and moves out of Review whenever a reviewed choice changes.

The scanner counts regular files and checked byte totals, records empty
directories, reports skipped entries and read failures, and uses
`symlink_metadata` so stable symbolic links are reported without traversal. Its
pending work list stores paths rather than open directory iterators, bounding
live directory handles. A completed scan remains an estimate: it supplies no
copy or installation authority, and controller-created selections always set
`safe_sync` to false.

## Public interface for the dependent view

- `SetupController::new(Capabilities) -> SetupController`
- `choose_experience(Experience) -> bool`
- `set_mode(FolderMode)`
- `add_folder(FolderScan, PathBuf) -> Result<(), String>`
- `remove_folder(usize) -> bool`
- `set_folder_mode(usize, FolderMode) -> bool`
- `clear_folder_mode_override(usize) -> bool`
- `next() -> Result<(), String>`
- `back()`
- `plan() -> &SetupPlan`
- `folders() -> &[FolderChoice]`
- public `page: SetupPage` and `default_mode: FolderMode`
- `scan_folder(&Path) -> Result<FolderScan, String>`
- `pick_folder() -> ui::PortalChannel`

`FolderChoice` exposes the retained `FolderScan`, explicit destination, current
mode, and whether that mode is an individual override. `FolderScan` exposes the
source, regular-file count, checked byte estimate, empty-directory paths, and
`SkippedEntry` details. `SetupPage` has `Experience`, `Folders`, and `Review`.

Linux uses `PickRequest::directory("Choose folder", "Choose").send()`. Windows
uses a fixed FolderBrowserDialog PowerShell script on a worker and explicitly
distinguishes Cancel from dialog errors; redirected output is fixed to UTF-8 and
decoded strictly. macOS development uses a fixed AppleScript chooser. Native
paths are never interpolated into either script, and non-Linux native dialogs
honor the shared portal suppression switch before spawning a worker.

## TDD and verification

Red command:

```text
cargo test -p imager --lib setup
```

The initial run failed with the expected missing `SetupController`, `SetupPage`,
`FolderScan`, `SkipReason`, and `scan_folder` symbols. A second interface-focused
red run failed on the not-yet-added retained-folder accessor and strict dialog
stdout result contract.

Green command:

```text
cargo test -p imager --lib setup
```

Result: 27 passed, 0 failed, 133 filtered out.

Lint command:

```text
cargo clippy -p imager --lib --no-default-features --no-deps
```

Result: exit 0. One pre-existing warning remains in `apps/imager/src/model.rs:285`
for `clippy::needless_return`; this task did not change that code. Documentation-
only `# Errors` additions in `setup/model.rs` were authorized to keep the scoped
public API lint-clean.

## Remaining validation and limits

- Only the installed `aarch64-apple-darwin` target was available here. The
  Windows FolderBrowserDialog and Linux portal need compilation and live runtime
  checks on their target platforms; this host result is not evidence for them.
- The macOS stdout decoder is unit-covered, but the live AppleScript chooser and
  its cancellation path were not opened during automated tests.
- Enumeration is neither a filesystem snapshot nor proof that files remain
  readable. Migration must independently validate identities, paths, access,
  contents, totals, and changes before copying anything.
- This component performs no file copying, synchronization, installation, disk
  mutation, or simulated success.
