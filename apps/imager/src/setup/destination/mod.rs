//! The guided setup's destination side: the installer on the live medium, and the
//! scenarios its captures show.

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
pub mod unattended;

pub use installer_app::{InstallState, InstallerApp, Step};

#[cfg(test)]
pub(crate) fn press(
    control: &mut dyn ui::widget::Widget,
    action: &ui::a11y::Action,
    doc: &mut automerge::AutoCommit,
) {
    assert!(
        control.perform_action(
            action,
            &[],
            &mut ui::widget::Edits::lend(doc),
            &automerge::ROOT
        ),
        "the control refused {action:?}"
    );
    ui::widget::controls::start_pending_control_invocations(
        control,
        &mut ui::widget::Edits::lend(doc),
        &automerge::ROOT,
    );
}

static LIVE_INSTALLER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    if let Some(invocation) = unattended::Invocation::from_args(std::env::args_os().skip(1))? {
        return invocation.run();
    }
    LIVE_INSTALLER.store(true, std::sync::atomic::Ordering::Relaxed);
    ui::framework::run::<crate::resource::ImagerShell>();
    Ok(())
}

pub(crate) fn launched() -> bool {
    LIVE_INSTALLER.load(std::sync::atomic::Ordering::Relaxed)
}

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
