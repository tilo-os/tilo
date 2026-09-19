# Guided Setup Wizard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the actual interactive setup screens and native folder selection to Imager, consuming the foundation model while platform installation adapters are developed.

**Architecture:** A process-local controller owns choices and navigates experience, folders, and review. A retained widget composes existing accessible controls. Background folder scanning supplies real file counts and bytes; future installation adapters consume reviewed plans without inheriting authority from UI state.

**Tech Stack:** Rust, Imager, qos-app retained widgets, native Windows folder dialog and Linux directory portal, existing screenshot renderer.

**Spec:** `docs/superpowers/specs/2026-09-16-guided-migration-design.md`

## Global Constraints

- Preserve the existing OS; no replacement option.
- Copy is recommended; Connected means synchronized local copies; Later is accessible.
- No disk mutations, package installation, or fake successful installation in this component.
- Never expose copied folders through collaboration state.
- Overall objective remains incomplete until real installation, migration, synchronization, packaging, and Windows/Linux verification are delivered.
- Ship the entry behind `--setup` until its full installation routes are implemented; preserve existing media-writing entry behavior during development.

### Task 1: Controller and native folder discovery

**Files:** Create `apps/imager/src/setup/controller.rs`, `folders.rs`; modify `setup/mod.rs`.

**Interfaces:** Consume `setup::model::{Capabilities,Experience,FolderMode,FolderSelection,SetupPlan}`.
Produce `SetupController`, `SetupPage::{Experience,Folders,Review}`, with
`new(Capabilities)`, `choose_experience(Experience) -> bool`, `set_mode(FolderMode)`,
`next() -> Result<(), String>`, `back()`, `plan() -> &SetupPlan`, and public read
access to page and default mode. Folder addition consumes a completed scan and
an explicit destination; changing a choice invalidates any review state.

- [ ] Write failing controller tests exercising supported Linux Desktop choice,
  rejected Windows Desktop, Back retaining selections, default Copy, Later
  clearing effective file selection, and review invalidation after a change.

```rust
let mut setup = SetupController::new(Capabilities {
    host: HostOs::Windows, desktop_session: false, internal_dual_boot: false,
});
assert!(!setup.choose_experience(Experience::Desktop));
assert!(setup.next().is_err());
```

- [ ] Implement transitions with explicit matches, no numeric page arithmetic.
  Mode changes apply to selected folders unless individually overridden.
  Later retains optional remembered selections but `plan()` exposes no active
  migration. Selecting a folder never establishes safe_sync by inference.
- [ ] Write filesystem tests using tempfile: nested regular files count exact
  bytes, empty directories survive enumeration, unreadable/missing roots report
  errors, symbolic links are reported as skipped and never traversed. A file
  changing during scanning must not confer installation authority.
- [ ] Implement `scan_folder(&Path) -> Result<FolderScan, String>` with checked
  byte totals and symlink_metadata. It is an estimate only; later migration must
  independently validate identity and contents. Return skipped-entry details.
- [ ] Implement asynchronous `pick_folder() -> ui::PortalChannel`: Linux uses
  `PickRequest::directory("Choose folder", "Choose").send()`, Windows uses
  FolderBrowserDialog via a fixed PowerShell script on a worker, macOS supports
  development with a fixed AppleScript folder chooser. Never interpolate user
  paths into scripts. Respect portal test suppression for native dialogs too.
- [ ] Run `cargo test -p imager --lib setup` for red then green and commit only
  touched files. Add the platform-native dialog runtime checks to remaining
  Windows/Linux validation; compilation on macOS does not prove them.

### Task 2: Retained setup screens and executable entry

**Files:** Create `apps/imager/src/setup/view.rs` and `known_folders.rs`; modify `setup/mod.rs`,
`apps/imager/src/main.rs`, and shot routing in `apps/imager/src/lib.rs` as needed.

**Interfaces:** Consume SetupController and native picker/scan workers.
Produce `SetupApp::new()` for live startup and capability-injected construction
for deterministic rendering. Expose a root `Widget` via `setup::app_root()`.

- [ ] Add failing interaction tests through real widget events or controller
  commands: unavailable experiences cannot proceed; selecting Copy/Connected/
  Later changes review; Back retains settings; a failed worker never appears as
  a completed scan; keyboard activation reaches the same controller commands.
- [ ] Compose screens using existing Column, Row, Text, Button, Scrollable,
  and TextInput controls. Use local command callbacks and state, not Automerge
  file paths. Keep navigation visible at compact viewport sizes and long errors
  in a scrollable body. Use headings, accessible names, and live status text.
- [ ] Experience screen offers Desktop on supported Linux and Full qOS with a
  plain compatibility explanation. Until adapters and catalog evidence exist,
  show the actual unavailable reason. Do not turn model capability flags on just
  because the host OS matches.
- [ ] Folder screen shows the three approved choices, Help me choose guidance,
  familiar Documents/Desktop/Pictures/Downloads suggestions, native Add folder,
  counts/sizes, destination, removal, and per-folder overrides. Resolve suggested
  locations through native known-folder lookup on Windows and `xdg-user-dir` on
  Linux, preserving redirected/localized folders; only suggest existing folders
  and never select them automatically. Run discovery/scanning off the UI thread.
  A missing lookup produces an honest unavailable suggestion, not an invented
  source directory. Isolate known-folder discovery in its own module with parser
  tests for empty, redirected, and whitespace-containing paths.
  Mark Connected unavailable until a working connection backend can attest safe
  source access. Explain the difference without promising live cross-OS sync.
- [ ] Review shows retained OS, experience, folder policies, space estimate,
  initial existing-OS default, and current blockers. Installation action remains
  unavailable until a real executor is wired; no simulated progress or success.
- [ ] Wire `--setup` into allowed flags and `live_root`. Add setup screenshot
  scenarios without invoking native dialogs or machine-changing work. Preserve
  existing Imager shots and default entry.
- [ ] Run controller/widget tests and render actual screenshots at 1180x820 and
  640x640. Inspect images for wrapping, visible actions, selection feedback, and
  overflow; correct defects before committing this component.

## Dependencies and completion boundary

Task 2 consumes Task 1 and may not invent adapters. This component delivers a
real interactive selection/review interface, not the completed migration app.
Next attach native capability probes, package/payload availability, and actual
execution plus resumable persistence; remove the development flag only when the
end-to-end routes work. Preserve all required end-state work in the goal ledger.
