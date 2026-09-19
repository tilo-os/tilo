# Guided Setup Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the shared setup decision model that the Windows and Linux migration app will use to present valid experiences, folder choices, and storage review.

**Architecture:** Extend the existing Imager with a focused setup module. Capability facts come from platform adapters rather than compile-time guesses; a pure model derives choices and validates review. Keep existing removable-media behavior intact while internal installation is developed.

**Tech Stack:** Rust, existing Imager workspace and UI, native Windows/Linux adapters.

**Spec:** `docs/superpowers/specs/2026-09-16-guided-migration-design.md`

## Global Constraints

- The first release preserves the existing operating system.
- Full qOS means native dual boot.
- Connected folders are synchronized local copies, not symlinks or direct access to original files.
- Folder migration grants no collaboration sharing or cloud-upload permission.
- No support claim follows merely from being Windows/Linux or successfully mounting an image.
- Existing OS remains the persistent boot default.
- This foundation is one delivery component; it does not complete the overall app objective.

## Full objective tracking

Subsequent component plans must deliver native probes and internal boot staging,
host-side space preparation, the complete setup UI, Desktop package installation,
installer handoff/recovery, verified resumable migration, two-way connections,
first-login integration, signed packaging, and Windows/Linux end-to-end evidence.
Do not enable unfinished routes or mark the objective complete after this plan.
Platform feasibility research runs alongside the non-mutating foundation work;
actual boot execution must wait for a validated protocol.

### Task 1: Capability-driven experience and folder review model

**Files:**
- Create: `apps/imager/src/setup/mod.rs`
- Create: `apps/imager/src/setup/model.rs`
- Modify: `apps/imager/src/lib.rs` (export setup module only)

**Interfaces:**
- Produces: `HostOs::{Windows,Linux,Other}`, `Experience::{Desktop,FullOs}`,
  `FolderMode::{Copy,Connected,Later}`.
- Produces: `Capabilities { host: HostOs, desktop_session: bool,
  internal_dual_boot: bool }`, with `supports(Experience) -> bool`.
- Produces: `FolderSelection { source: PathBuf, destination: PathBuf,
  bytes: u64, mode: FolderMode, readable: bool, safe_sync: bool }`.
- Produces: `SetupPlan { experience: Experience, folders: Vec<FolderSelection>,
  system_bytes: u64, staging_bytes: u64, headroom_bytes: u64 }`.
- Produces: `SetupPlan::required_bytes() -> Result<u64, ReviewError>` and
  `SetupPlan::validate(&Capabilities, available_bytes: u64) -> Result<(), ReviewError>`.
- `ReviewError` distinguishes unsupported experience, inaccessible source,
  unavailable connection, invalid/overlapping paths, insufficient space, overflow.
- All types are local non-secret state, not privileged authorization.

- [ ] Write tests first in `model.rs` using the real public model. A Linux desktop
  must require desktop_session evidence; Windows must reject Desktop even when a
  malformed capability input says true. FullOs requires internal_dual_boot and a
  supported host. Copy requires safe readability, Connected requires readable and
  safe_sync, and Later ignores access and consumes no file-copy space.

```rust
#[test]
fn windows_does_not_offer_desktop_from_a_malformed_probe() {
    let caps = Capabilities {
        host: HostOs::Windows, desktop_session: true, internal_dual_boot: false,
    };
    assert!(!caps.supports(Experience::Desktop));
    assert!(!caps.supports(Experience::FullOs));
}
```

- [ ] Add independent literal fixtures for storage totals: system=40, staging=10,
  headroom=5, Copy=7, Connected=11, Later=99 must require 73. Available=72 rejects;
  73 accepts. A u64::MAX input plus 1 returns overflow instead of wrapping.
- [ ] Add path tests: reject relative paths, parent traversal, source/destination
  ancestry overlap, duplicate active selections, overlapping active sources, and
  overlapping active destinations. Component-based comparisons must distinguish
  `/home/a` from `/home/ab`. Later entries do not reserve paths or space. Document
  that platform canonicalization, volume identity, case folding, and filesystem
  revalidation are still required before actual I/O.
- [ ] Run `rustc --edition=2024 --test apps/imager/src/setup/model.rs -o
  /tmp/qos-guided-setup-model-tests` and the resulting binary. First run must fail
  because the requested behavior is missing; then implement and rerun.
- [ ] Implement support rules with an explicit match, not host-only detection:

```rust
match (self.host, experience) {
    (HostOs::Linux, Experience::Desktop) => self.desktop_session,
    (HostOs::Windows | HostOs::Linux, Experience::FullOs) => self.internal_dual_boot,
    _ => false,
}
```

- [ ] Use checked_add for all storage accounting; count both Copy and Connected.
  Validate paths/access before returning a successful review. Return structured
  errors with folder indices where relevant so UI can focus the failing entry.
- [ ] Export `pub mod model;` from setup and `pub mod setup;` from Imager lib.
  Run `cargo test -p imager --lib setup::model` when the workspace build is free.
  Standalone model tests are additional evidence, not replacement for integration.
- [ ] Format only touched Rust files, inspect scoped diff, and commit only this
  task's files with message `feat(imager): model guided setup choices and review`.

## Review and next component

Review the task against capability gating, safe folder semantics, arithmetic,
path handling, and absence of disk mutation. Record full-crate build failures
separately from model test results. The next plan consumes this module in a real
wizard and replaces adapter uncertainty using the feasibility report; do not
invent a successful installation to fill the UI.
