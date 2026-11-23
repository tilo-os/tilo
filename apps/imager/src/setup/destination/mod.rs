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
    if let Some(planned) = unattended::PlannedInvocation::find() {
        return planned.install();
    }
    LIVE_INSTALLER.store(true, std::sync::atomic::Ordering::Relaxed);
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = ui::framework::run_helper::<crate::resource::ImagerShell>(&args) {
        #[allow(
            clippy::exit,
            reason = "a helper the installer runs ends the process with its own exit code"
        )]
        std::process::exit(code);
    }
    // The live installer is the device's own program, run before any person has a certificate
    // here: like the greeter, it attaches to no person's runtime, and the device's daemon founds
    // no identity for it.
    ui::launch::run_or_exit(
        &ui::framework::launch_spec::<crate::resource::ImagerShell>().runtime_attachment(|_| false),
    );
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
    #[cfg(test)]
    {
        if let Some(app) = installer_app::host_journeys_tests::armed_window() {
            return crate::window_view::window_view(cx, app, InstallerApp::effect);
        }
    }
    let app = if cx.mode().is_live() {
        let mut app = InstallerApp::new();
        app.attach_live(cx.waker());
        app
    } else {
        InstallerApp::for_shot(Step::Keyboard)
    };
    crate::window_view::window_view(cx, app, InstallerApp::effect)
}
