//! Image Writer — qOS native app for writing a disk image to a USB drive
//! (CRDT-native, built on the `ui` toolkit).
//!
//! A linear **wizard**, modelled on Fedora Media Writer / balenaEtcher:
//!
//!   1. **Source** — pick a local `.iso` / `.img` via the host XDG portal
//!      file picker (never a bespoke dialog), or a known qOS release.
//!   2. **Drive** — choose a target from the *real* removable block devices
//!      (`lsblk`); internal disks are hidden behind an explicit opt-in, and
//!      an honest empty state asks the user to insert a drive.
//!   3. **Customize** (optional) — pre-configure a qOS image (hostname, first
//!      user, region). Plain ISOs are written as-is and this step is a no-op.
//!      The options double as a HEADLESS / unattended install config.
//!   4. **Write** — a typed device-name confirmation unlocks a real,
//!      byte-for-byte streaming copy with live progress, then a read-back
//!      **verify** pass, then a done / failure screen.
//!
//! Writing is doubly gated: the typed confirmation, plus the
//! `QOS_IMAGER_ALLOW_WRITE=1` allow-gate in [`write`]. With the gate off the
//! same progress UI runs against a simulated worker that touches no device.
//!
//! `app_root()` is the single integration point used by both the live event
//! loop and the `--shots` offscreen renderer. The live binary sets
//! `IMAGER_LIVE=1` so the drive list comes from a real `lsblk` probe;
//! `--shots` leaves it unset and seeds a scenario so the screenshot is
//! deterministic offline.

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

/// The ordered `--shots` scenarios: one per meaningful wizard state, named so
/// the offscreen renderer can walk them and the gallery reads top-to-bottom.
pub const SHOT_SCENARIOS: &[&str] = &[
    "source",
    "drive",
    "drive-empty",
    "customize",
    "write-confirm",
    "progress",
    "verify-done",
];

/// Pose the document for one named `--shots` scenario. Has no effect on the
/// live app (the live binary never calls it), so a screenshot is the only
/// place this state is set. Unknown names fall back to the source step.
pub fn seed_scenario(doc: &mut automerge::AutoCommit, scenario: &str) {
    use crate::doc::ImagerDoc;
    use crate::model::{
        sample_source, sample_targets, Action, JobPhase, Progress, Source, SourceKind, Step,
    };

    let iso_source = || Source {
        kind: SourceKind::IsoFile,
        path: "/home/quinten/Downloads/Fedora-Workstation-Live-42.iso".into(),
        name: "Fedora-Workstation-Live-42.iso".into(),
        size: 2_113_929_216,
        label: "Fedora-WS-Live-42-1-1".into(),
        installable: false,
    };

    match scenario {
        "drive-empty" => {
            // No removable drives present → the honest empty state.
            ImagerDoc::seed(doc, &[]);
            ImagerDoc::set_source(doc, Some(&iso_source()));
            ImagerDoc::set_selected(doc, None);
            ImagerDoc::set_step(doc, Step::Drive);
            ImagerDoc::set_status(doc, "No removable drives detected");
        }
        "drive" => {
            ImagerDoc::seed(doc, &sample_targets());
            ImagerDoc::set_source(doc, Some(&iso_source()));
            ImagerDoc::set_selected(doc, Some(0)); // the USB stick
            ImagerDoc::set_step(doc, Step::Drive);
            ImagerDoc::set_status(doc, "1 removable drive detected");
        }
        "customize" => {
            ImagerDoc::seed(doc, &sample_targets());
            ImagerDoc::set_source(doc, Some(&sample_source())); // installable qOS
            ImagerDoc::set_selected(doc, Some(0));
            ImagerDoc::set_action(doc, Action::Install);
            ImagerDoc::set_step(doc, Step::Customize);
            ImagerDoc::set_status(doc, "Pre-configuring qOS 1.0");
        }
        "write-confirm" => {
            ImagerDoc::seed(doc, &sample_targets());
            ImagerDoc::set_source(doc, Some(&sample_source()));
            ImagerDoc::set_selected(doc, Some(0));
            ImagerDoc::set_action(doc, Action::Install);
            ImagerDoc::set_step(doc, Step::Write);
            ImagerDoc::set_confirm(doc, "sdb"); // armed
            ImagerDoc::set_status(doc, "Ready to write qOS 1.0 → /dev/sdb");
        }
        "progress" => {
            ImagerDoc::seed(doc, &sample_targets());
            ImagerDoc::set_source(doc, Some(&sample_source()));
            ImagerDoc::set_selected(doc, Some(0));
            ImagerDoc::set_step(doc, Step::Write);
            ImagerDoc::set_progress(
                doc,
                &Progress { phase: JobPhase::Writing, write_frac: 0.62, verify_frac: 0.0, rate_bps: 176_000_000 },
            );
            ImagerDoc::set_status(doc, "Writing qOS 1.0 → /dev/sdb");
        }
        "verify-done" => {
            ImagerDoc::seed(doc, &sample_targets());
            ImagerDoc::set_source(doc, Some(&sample_source()));
            ImagerDoc::set_selected(doc, Some(0));
            ImagerDoc::set_step(doc, Step::Write);
            ImagerDoc::set_progress(
                doc,
                &Progress { phase: JobPhase::Success, write_frac: 1.0, verify_frac: 1.0, rate_bps: 0 },
            );
            ImagerDoc::set_status(doc, "Done — qOS 1.0 written and verified");
        }
        // "source" and any unknown name.
        _ => {
            ImagerDoc::seed(doc, &sample_targets());
            ImagerDoc::set_source(doc, Some(&sample_source()));
            ImagerDoc::set_selected(doc, None);
            ImagerDoc::set_step(doc, Step::Source);
            ImagerDoc::set_status(doc, "");
        }
    }
}
