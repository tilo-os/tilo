//! Disk Imager — qOS native app for writing an OS image to a drive, and for
//! installing qOS (CRDT-native, built on the `ui` toolkit).
//!
//! A single guided **wizard** with a six-station step indicator, covering both
//! "write this image to a USB stick" and "install qOS onto a disk":
//!
//!   1. **Source** — pick a local `.iso` / `.img` via the platform's native
//!      file picker, or a known qOS release.
//!   2. **Drive** — choose a target from real platform block-device discovery;
//!      internal disks are hidden behind an explicit opt-in, the **system disk**
//!      needs a second acknowledgement before it can even be selected, and an
//!      honest empty state asks the user to insert a drive.
//!   3. **Customize** (qOS only) — the guided-customization step (hostname,
//!      first user, region, encryption, autologin), shared by the write and
//!      install flows. Plain ISOs skip it. The options double as a headless /
//!      unattended install config.
//!   4. **Confirm** — a read-only review of every choice, an erase warning that
//!      escalates for an internal / system disk, and the typed device-name gate.
//!   5. **Write / Install** — a real, byte-for-byte streaming copy with live
//!      progress, then a read-back **verify** pass. (The caption reads "Write"
//!      for a removable drive, "Install" for a disk install.)
//!   6. **Done** — the verified / failed outcome, with a fresh-start affordance.
//!
//! Writing is doubly gated: the typed confirmation, plus the
//! `QOS_IMAGER_ALLOW_WRITE=1` allow-gate in [`write`]. With the gate off the
//! write is explicitly unavailable; no simulated worker reports fake success.
//!
//! `app_root()` is the live integration point; `app_root_for_shot()` stages
//! only peer-local capture state before returning the same widget. The live binary sets
//! `IMAGER_LIVE=1` so the drive list comes from a real `lsblk` probe and the
//! app starts genuinely blank; `--shots` / `--demo` seed sample content so the
//! screenshot is deterministic offline. Immediately before writing, the live
//! inventory is read again and its stable device identity, size, and system-
//! disk status must still match the confirmed target.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
pub mod customize;
pub mod doc;
mod export_dialog;
pub mod model;
pub mod probe;
pub mod release;
pub mod write;

pub use app::ImagerApp;
pub use customize::CustomizePanel;
pub use model::{
    Action, DiskLayout, InstallMode, InstallProfile, Source, SourceKind, Step, TargetDrive,
    TargetKind,
};

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(ImagerApp::new())
}

/// Build the same widget as [`app_root`], with only peer-local shot state
/// staged outside the collaborative document.
pub fn app_root_for_shot(scenario: &str) -> Box<dyn Widget> {
    let mut app = ImagerApp::new();
    if scenario == "confirm" {
        app.seed_confirmation_for_shot("sdb");
    } else if scenario == "export" {
        app.seed_export_for_shot();
    }
    Box::new(app)
}

/// The ordered `--shots` scenarios: one per meaningful wizard station, named so
/// the offscreen renderer can walk them and the gallery reads top-to-bottom.
pub const SHOT_SCENARIOS: &[&str] = &[
    "source",
    "source-empty",
    "drive",
    "drive-empty",
    "drive-internal",
    "customize",
    "export",
    "confirm",
    "progress",
    "done",
    "done-failed",
];

/// Pose the document for one named `--shots` scenario. Sample content is seeded
/// here via [`ImagerDoc::seed_demo`] — never by the live app — so a screenshot
/// is the only place this state is set. Unknown names fall back to the source
/// step.
pub fn seed_scenario(doc: &mut automerge::AutoCommit, scenario: &str) {
    use crate::doc::ImagerDoc;

    // The empty-drive scenario is the one case with no sample inventory: seed an
    // empty document so the honest "insert a drive" state renders.
    if scenario == "drive-empty" {
        seed_scenario_drive_empty(doc);
        return;
    }
    // The Source step as the live app actually opens it: nothing browsed yet,
    // no release known to be on this machine. This is the state a real run
    // starts in, so it is the one worth looking at.
    if scenario == "source-empty" {
        seed_scenario_source_empty(doc);
        return;
    }
    // Internal-disks-revealed: a USB stick plus the *system* disk, shown with
    // the guard active (the system disk locked, both acknowledgements visible).
    if scenario == "drive-internal" {
        seed_scenario_drive_internal(doc);
        return;
    }

    // Every other scenario builds on the demo document (sample drives + a qOS
    // image + recent history), then poses the wizard station on top.
    ImagerDoc::seed_demo(doc);
    seed_scenario_common(doc, scenario);
}

/// The ISO source shared by the `drive-empty` and `drive` scenarios.
fn demo_iso_source() -> crate::model::Source {
    use crate::model::{Source, SourceKind};
    Source {
        kind: SourceKind::IsoFile,
        path: "/home/user/Downloads/Fedora-Workstation-Live-42.iso".into(),
        name: "Fedora-Workstation-Live-42.iso".into(),
        size: 2_113_929_216,
        label: "Fedora-WS-Live-42-1-1".into(),
        installable: false,
    }
}

fn seed_scenario_drive_empty(doc: &mut automerge::AutoCommit) {
    use crate::doc::ImagerDoc;
    use crate::model::Step;
    ImagerDoc::seed(doc, &[]);
    ImagerDoc::set_source(doc, Some(&demo_iso_source()));
    ImagerDoc::set_selected(doc, None);
    ImagerDoc::set_step(doc, Step::Drive);
    ImagerDoc::set_status(doc, "No removable drives detected");
}

fn seed_scenario_source_empty(doc: &mut automerge::AutoCommit) {
    use crate::doc::ImagerDoc;
    use crate::model::Step;
    ImagerDoc::seed(doc, &[]);
    ImagerDoc::set_step(doc, Step::Source);
}

fn seed_scenario_drive_internal(doc: &mut automerge::AutoCommit) {
    use crate::doc::ImagerDoc;
    use crate::model::{Step, TargetDrive, TargetKind, TargetPartition, sample_source};
    let targets = vec![
        crate::model::sample_targets().remove(0), // the USB stick
        TargetDrive {
            name: "nvme0n1".into(),
            model: "Samsung SSD 990 PRO 1TB".into(),
            stable_id: Some("demo-running-system".into()),
            size: 1_000_204_886_016,
            kind: TargetKind::Nvme,
            tran: Some("nvme".into()),
            removable: false,
            is_system: true, // the running OS boots from this disk
            partitions: vec![
                TargetPartition {
                    name: "nvme0n1p1".into(),
                    size: 536_870_912,
                    fstype: Some("vfat".into()),
                    label: Some("EFI".into()),
                },
                TargetPartition {
                    name: "nvme0n1p2".into(),
                    size: 999_650_000_000,
                    fstype: Some("btrfs".into()),
                    label: Some("qos-root".into()),
                },
            ],
        },
    ];
    ImagerDoc::seed(doc, &targets);
    ImagerDoc::set_source(doc, Some(&sample_source()));
    ImagerDoc::set_selected(doc, None);
    ImagerDoc::set_show_internal(doc, true);
    ImagerDoc::set_step(doc, Step::Drive);
    ImagerDoc::set_status(doc, "1 removable, 2 total");
}

/// Pose the "drive", "customize", "confirm", "progress", "done" and
/// "done-failed" stations (plus the "source"/unknown default) on top of the
/// already-seeded demo document.
fn seed_scenario_common(doc: &mut automerge::AutoCommit, scenario: &str) {
    use crate::doc::ImagerDoc;
    use crate::model::{Action, JobPhase, Progress, Step};

    match scenario {
        "drive" => {
            ImagerDoc::set_source(doc, Some(&demo_iso_source()));
            ImagerDoc::set_selected(doc, Some(0)); // the USB stick
            ImagerDoc::set_step(doc, Step::Drive);
            ImagerDoc::set_status(doc, "1 removable, 2 total");
        }
        "customize" | "export" => {
            ImagerDoc::set_selected(doc, Some(0));
            ImagerDoc::set_action(doc, Action::Flash);
            ImagerDoc::set_step(doc, Step::Customize);
            ImagerDoc::set_status(doc, "Pre-configuring qOS 1.0");
        }
        "confirm" => {
            ImagerDoc::set_selected(doc, Some(0));
            ImagerDoc::set_action(doc, Action::Flash);
            ImagerDoc::set_step(doc, Step::Confirm);
            ImagerDoc::set_status(doc, "Ready to write qOS 1.0 → /dev/sdb");
        }
        "progress" => {
            ImagerDoc::set_selected(doc, Some(0));
            ImagerDoc::set_step(doc, Step::Write);
            ImagerDoc::set_progress(
                doc,
                &Progress {
                    phase: JobPhase::Writing,
                    write_frac: 0.62,
                    verify_frac: 0.0,
                    rate_bps: 176_000_000,
                },
            );
            ImagerDoc::set_status(doc, "Writing qOS 1.0 → /dev/sdb");
        }
        "done" => {
            ImagerDoc::set_selected(doc, Some(0));
            ImagerDoc::set_step(doc, Step::Done);
            ImagerDoc::set_progress(
                doc,
                &Progress {
                    phase: JobPhase::Success,
                    write_frac: 1.0,
                    verify_frac: 1.0,
                    rate_bps: 0,
                },
            );
            ImagerDoc::set_status(doc, "Done — qOS 1.0 written and verified");
        }
        "done-failed" => {
            ImagerDoc::set_selected(doc, Some(0));
            ImagerDoc::set_step(doc, Step::Done);
            ImagerDoc::set_progress(
                doc,
                &Progress {
                    phase: JobPhase::Failed,
                    write_frac: 0.4,
                    verify_frac: 0.0,
                    rate_bps: 0,
                },
            );
            ImagerDoc::set_status(doc, "Write failed — device disappeared mid-write");
        }
        // "source" and any unknown name.
        _ => {
            ImagerDoc::set_selected(doc, None);
            ImagerDoc::set_step(doc, Step::Source);
            ImagerDoc::set_status(doc, "");
        }
    }
}
