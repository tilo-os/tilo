//! Install Media — qOS native app for creating install media and
//! installing qOS (CRDT-native, built on the `ui` toolkit).
//!
//! The flow runs left-to-right across three columns:
//!
//!   1. **Source** — pick an arbitrary `.iso` from disk, or a qOS image
//!      (local file / known release). Shows name, size and label/type.
//!   2. **Target** — removable USB drives + disks parsed from `lsblk -J -b`,
//!      each with a prominent DESTRUCTIVE warning.
//!   3. **Action / Options** — either WRITE an ISO (raw dd-style + verify),
//!      or INSTALL qOS with structured options (hostname, user, locale,
//!      disk layout, encryption) that double as a HEADLESS/unattended
//!      install config.
//!
//! Writing is gated behind a typed confirmation and a worker seam
//! ([`write`]) that never touches a device in this dev/headless build.
//!
//! `app_root()` is the single integration point used by both the live
//! event loop and the `--shots` offscreen renderer. The live binary sets
//! `IMAGER_LIVE=1` so the target list comes from a real `lsblk` probe;
//! `--shots` leaves it unset so the screenshot is the curated sample.

pub mod app;
pub mod doc;
pub mod model;
pub mod probe;
pub mod write;

pub use app::ImagerApp;
pub use model::{
    Action, DiskLayout, InstallMode, InstallProfile, Source, SourceKind, TargetDrive, TargetKind,
};

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(ImagerApp::new())
}

/// Pose the document for the `--shots` screenshot so the full
/// source → target → options flow is on screen: the qOS image is the
/// source, the install target is selected, and the structured install
/// options panel is open. Has no effect on the live app (the live binary
/// never calls it), so the screenshot is the only place this state is set.
pub fn seed_shots_demo(doc: &mut automerge::AutoCommit) {
    use crate::doc::ImagerDoc;
    use crate::model::{sample_targets, Action};
    ImagerDoc::seed(doc, &sample_targets());
    // Select the NVMe so the destructive "FIXED DISK" warning shows, and
    // open the Install options panel (the richest view).
    ImagerDoc::set_action(doc, Action::Install);
    ImagerDoc::set_selected(doc, Some(1));
    ImagerDoc::set_status(
        doc,
        "Ready — qOS 1.0 → /dev/nvme0n1. Type the device name to arm the write.",
    );
}
