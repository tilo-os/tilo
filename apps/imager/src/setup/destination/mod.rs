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

pub mod action_button;
pub mod control;
pub mod disk_row;
pub mod effects;
pub mod installer_app;
pub mod keyboard_view;
pub mod seg_choice;
pub mod status_log;
pub mod unattended;

pub use installer_app::{InstallState, InstallerApp, Step};

#[cfg(test)]
pub(crate) fn press(
    control: &mut dyn tilo_ui_base::widget::Widget,
    action: &tilo_ui_base::a11y::Action,
    doc: &mut automerge::AutoCommit,
) {
    assert!(
        control.perform_action(
            action,
            &[],
            &mut tilo_ui_base::widget::Edits::lend(doc),
            &automerge::ROOT
        ),
        "the control refused {action:?}"
    );
    ui::widget::resources::start_pending_control_invocations(
        control,
        &mut tilo_ui_base::widget::Edits::lend(doc),
        &automerge::ROOT,
    );
}

static LIVE_INSTALLER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    if let Some(invocation) = unattended::Invocation::from_args(std::env::args_os().skip(1))? {
        return invocation.run();
    }
    if let Some(planned) = unattended::PlannedInvocation::find() {
        return planned.install();
    }
    LIVE_INSTALLER.store(true, std::sync::atomic::Ordering::Relaxed);
    // The live installer is the device's own program, run before any person has a certificate
    // here: like the greeter, it is no person's window, and serves its console itself.
    crate::hosting::run();
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

/// The installer's window: live on the medium, woken through `wake`, or a scenario's shot.
pub(crate) fn window(
    wake: &ui::WakeHandle,
    opened: crate::hosting::Opened<'_>,
) -> tilo_ui_base::widget::Element {
    #[cfg(test)]
    {
        if let Some(app) = installer_app::host_journeys_tests::armed_window() {
            return Box::new(app);
        }
    }
    match opened {
        crate::hosting::Opened::Live { .. } => {
            let mut app = InstallerApp::new();
            app.attach_live(wake.clone());
            Box::new(app)
        }
        crate::hosting::Opened::Shot(scenario) => Box::new(
            scenario
                .and_then(|name| name.strip_prefix("destination-"))
                .and_then(InstallerApp::for_scenario)
                .unwrap_or_else(|| InstallerApp::for_shot(Step::Keyboard)),
        ),
    }
}
