//! Disk Imager — qOS native app for writing an OS image to a drive, and for
//! installing qOS (CRDT-native, built on the `ui` toolkit).
//!
//! A single guided **wizard** with a six-station step indicator, covering both
//! "write this image to a USB stick" and "install qOS onto a disk":
//!
//!   1. **Source** — pick a local `.iso` / `.img` via the host XDG portal
//!      file picker (never a bespoke dialog), or a known qOS release.
//!   2. **Drive** — choose the target from the *real* block devices (`lsblk`);
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
//! same progress UI runs against a simulated worker that touches no device.
//!
//! `app_root()` is the single integration point used by both the live event
//! loop and the `--shots` offscreen renderer. The live binary sets
//! `IMAGER_LIVE=1` so the drive list comes from a real `lsblk` probe and the
//! app starts genuinely blank; `--shots` / `--demo` seed sample content so the
//! screenshot is deterministic offline.

pub mod actions;
pub mod app;
pub mod doc;
pub mod model;
pub mod probe;
pub mod write;

pub use app::ImagerApp;
pub use model::{
    Action, DiskLayout, InstallMode, InstallProfile, Source, SourceKind, Step, TargetDrive,
    TargetKind,
};

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(ImagerApp::new())
}

/// The ordered `--shots` scenarios: one per meaningful wizard station, named so
/// the offscreen renderer can walk them and the gallery reads top-to-bottom.
pub const SHOT_SCENARIOS: &[&str] = &[
    "source",
    "drive",
    "drive-empty",
    "customize",
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
    use crate::model::{Action, JobPhase, Progress, Source, SourceKind, Step};

    let iso_source = || Source {
        kind: SourceKind::IsoFile,
        path: "/home/quinten/Downloads/Fedora-Workstation-Live-42.iso".into(),
        name: "Fedora-Workstation-Live-42.iso".into(),
        size: 2_113_929_216,
        label: "Fedora-WS-Live-42-1-1".into(),
        installable: false,
    };

    // The empty-drive scenario is the one case with no sample inventory: seed an
    // empty document so the honest "insert a drive" state renders.
    if scenario == "drive-empty" {
        ImagerDoc::seed(doc, &[]);
        ImagerDoc::set_source(doc, Some(&iso_source()));
        ImagerDoc::set_selected(doc, None);
        ImagerDoc::set_step(doc, Step::Drive);
        ImagerDoc::set_status(doc, "No removable drives detected");
        return;
    }

    // Every other scenario builds on the demo document (sample drives + a qOS
    // image + recent history), then poses the wizard station on top.
    ImagerDoc::seed_demo(doc);

    match scenario {
        "drive" => {
            ImagerDoc::set_source(doc, Some(&iso_source()));
            ImagerDoc::set_selected(doc, Some(0)); // the USB stick
            ImagerDoc::set_step(doc, Step::Drive);
            ImagerDoc::set_status(doc, "1 removable, 2 total");
        }
        "customize" => {
            ImagerDoc::set_selected(doc, Some(0));
            ImagerDoc::set_action(doc, Action::Flash);
            ImagerDoc::set_step(doc, Step::Customize);
            ImagerDoc::set_status(doc, "Pre-configuring qOS 1.0");
        }
        "confirm" => {
            ImagerDoc::set_selected(doc, Some(0));
            ImagerDoc::set_action(doc, Action::Flash);
            ImagerDoc::set_step(doc, Step::Confirm);
            ImagerDoc::set_confirm(doc, "sdb"); // armed
            ImagerDoc::set_status(doc, "Ready to write qOS 1.0 → /dev/sdb");
        }
        "progress" => {
            ImagerDoc::set_selected(doc, Some(0));
            ImagerDoc::set_step(doc, Step::Write);
            ImagerDoc::set_progress(
                doc,
                &Progress { phase: JobPhase::Writing, write_frac: 0.62, verify_frac: 0.0, rate_bps: 176_000_000 },
            );
            ImagerDoc::set_status(doc, "Writing qOS 1.0 → /dev/sdb");
        }
        "done" => {
            ImagerDoc::set_selected(doc, Some(0));
            ImagerDoc::set_step(doc, Step::Done);
            ImagerDoc::set_progress(
                doc,
                &Progress { phase: JobPhase::Success, write_frac: 1.0, verify_frac: 1.0, rate_bps: 0 },
            );
            ImagerDoc::set_status(doc, "Done — qOS 1.0 written and verified");
        }
        "done-failed" => {
            ImagerDoc::set_selected(doc, Some(0));
            ImagerDoc::set_step(doc, Step::Done);
            ImagerDoc::set_progress(
                doc,
                &Progress { phase: JobPhase::Failed, write_frac: 0.4, verify_frac: 0.0, rate_bps: 0 },
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
