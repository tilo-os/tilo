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
`next() -> Result<(), String>`, `back()`, `plan() -> &SetupPlan`, and read-only
`page()` / `default_mode()` getters. Folder addition consumes a completed scan and
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
