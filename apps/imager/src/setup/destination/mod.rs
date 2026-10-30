//! The guided setup's destination side: the installer on the live medium.
//!
//! Run under `cage` (a kiosk wlroots compositor) off the live medium, it
//! walks a person through the wizard with a persistent progress breadcrumb:
//!
//!   1. **Keyboard** — pick a layout before typing any secrets.
//!   2. **Network** — optionally connect through `NetworkManager`; offline is valid.
//!   3. **Accounts** — optionally sign in to online accounts, privately.
//!   4. **Disk** — pick a target disk, or free space beside another system.
//!   5. **Account** — choose a username and a confirmed password.
//!   6. **Options** — filesystem, hostname, time zone.
//!   7. **Media** — explicitly choose whether to download more formats.
//!   8. **Review** — every choice, the compatibility report and the erase warning.
//!   9. **Install** — the privileged backend on a worker thread, streaming
//!      progress into a live status log, then Finish (Reboot / Power off)
//!      or a failure screen.
//!
//! What it reads from and does to the machine is its platform half,
//! `imager_backend::destination`. Every step is also a set of published
//! effects ([`effects`]), and the wizard's own controls run those effects:
//! one implementation behind a click, a screen reader, the Ctrl+K palette, a
//! headless scenario and the console. The destructive `begin` asks for the
//! exact object it formats as a typed phrase. On the live medium the console
//! answers the `tilo-control` action protocol; nothing about it can reveal a
//! passphrase or the recovery key.
//!
//! Modules:
//! * [`installer_app`] — the [`InstallerApp`] surface and wizard state
//!   machine, its published effects and its console.
//! * [`effects`] — the one effect vocabulary every door invokes.
//! * [`control`] — what a control does when used: queue its effect.
//! * [`unattended`] — an installation from a private recipe, through the
//!   same effects.
//! * [`status_log`], [`disk_row`], [`step_indicator`], [`action_button`],
//!   [`seg_choice`], [`keyboard_view`] — the wizard's widgets.
//! * [`accounts_view`], [`accounts_hints`] — the private account form and
//!   the hints read from the old system.
//! * [`hardware_report`] — the compatibility report Review shows.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod accounts_hints;
pub mod accounts_view;
pub mod action_button;
pub mod control;
pub mod disk_row;
pub mod effects;
pub mod hardware_report;
pub mod installer_app;
pub mod keyboard_view;
pub mod seg_choice;
pub mod status_log;
pub mod step_indicator;
pub mod unattended;

pub use installer_app::{InstallState, InstallerApp, Step};

/// Layout of a full-width wizard row that keeps its `height` instead of
/// shrinking when the column runs short.
fn full_width_row(height: f32) -> taffy::Style {
    taffy::Style {
        size: taffy::Size {
            width: taffy::Dimension::percent(1.0),
            height: taffy::Dimension::length(height),
        },
        flex_shrink: 0.0,
        ..Default::default()
    }
}

/// Whether this process is the installer on the live medium.
static LIVE_INSTALLER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Run the installer on the live medium (`tilo-installer`): an unattended
/// installation from a private recipe when the command line names one,
/// otherwise the imager app showing its destination side.
///
/// # Errors
///
/// Returns an error for a malformed unattended command line; the window
/// does not return.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    if let Some(invocation) = unattended::Invocation::from_args(std::env::args_os().skip(1))? {
        return invocation.run();
    }
    LIVE_INSTALLER.store(true, std::sync::atomic::Ordering::Relaxed);
    ui::framework::run::<crate::resource::ImagerShell>();
    Ok(())
}

/// Whether this process was started as the installer on the live medium.
pub(crate) fn launched() -> bool {
    LIVE_INSTALLER.load(std::sync::atomic::Ordering::Relaxed)
}

/// The installer's screenshot scenarios, one per step and terminal state.
pub(crate) const SCENARIOS: &[&str] = &[
    "destination-keyboard",
    "destination-keyboard-default",
    "destination-network",
    "destination-disk",
    "destination-disk-alongside",
    "destination-account",
    "destination-options",
    "destination-options-open",
    "destination-profile-locale-repair",
    "destination-media",
    "destination-media-chosen",
    "destination-review",
    "destination-review-alongside",
    "destination-install",
    "destination-finish",
    "destination-finish-recovery",
    "destination-finish-recovery-saved",
    "destination-failed",
    "destination-failed-recovery",
];

/// The installer's window: the wizard live, probing this computer, or a
/// sample one that a screenshot scenario stages.
pub(crate) fn view(cx: &mut ui::framework::ViewCx<crate::resource::Imager>) -> ui::widget::Element {
    let app = if cx.mode().is_live() {
        let mut app = InstallerApp::new();
        app.attach_live(cx.waker());
        app
    } else {
        InstallerApp::for_shot(Step::Keyboard)
    };
    crate::window_view::window_view(cx, app, InstallerApp::effect)
}
