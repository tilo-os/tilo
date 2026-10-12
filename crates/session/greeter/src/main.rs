// Thin binary entry point for the Tilo graphical greeter — all logic
// lives in the library (mirrors apps/file-explorer/src/main.rs).
//
// Modes:
//   * default — build a `GreeterApp`, hand it to `ui::App`, run the live
//     winit event loop. Launched at the greetd VT under `cage`:
//         cage -s -- /usr/bin/tilo-greeter
//     The event-loop proxy is captured via `run_with_setup` so the
//     background auth worker can pump prompts/results back into the UI.
//   * `--shots <PATH> [--scenario <name>] [--direction split|poster]` plus
//     the standard capture options (`--viewport`, `--text-scale`, `--theme`,
//     …, see `ui::launch::LaunchArgs`) — render one login-card state
//     into a PNG at `<PATH>` and exit. No window, no event loop. Headless
//     snapshot for CI / visual inspection (identical output to the live
//     binary). `--scenario` names one of `SCENARIOS` and defaults to `form`.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::sync::{Arc, OnceLock};

use greeter::GreeterApp;
use greeter::greeter_app::AuthScenario;
use ui::app::UserEvent;
use winit::event_loop::EventLoopProxy;

/// Default offscreen viewport for `--shots`.
const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

/// # Errors
///
/// Returns an error for a malformed command line or an unknown `--scenario`;
/// the live event-loop path does not return.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tilo_startup_metrics::init();
    let _startup_metrics = tilo_startup_metrics::ScopedDumper;
    tilo_startup_metrics::mark("greeter-main-start");
    let _detached = ui::wake::WakeScope::enter_detached();
    tracing_subscriber::fmt::init();

    let args = ui::launch::LaunchArgs::from_env(&["--direction"])?;
    if let Some(out) = &args.shots {
        let scenario = match args.scenario.as_deref() {
            None => AuthScenario::Form,
            Some(name) => scenario_named(name)?,
        };
        let mut widget = GreeterApp::for_shot(scenario);
        match args.value("--direction") {
            Some("split") => widget.set_direction(greeter::greeter_app::Direction::Split),
            Some("poster") => widget.set_direction(greeter::greeter_app::Direction::Poster),
            _ => {}
        }
        if let Some((username, password)) = shot_credentials(scenario) {
            widget.seed_shot_username(username);
            widget.seed_shot_password(password);
        }
        ui::clock::freeze_for_capture();
        let viewport = args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        let mut frame = ui::headless::FrameHarness::new_with_theme(
            ui::State::new(),
            Box::new(widget),
            viewport,
            ui::launch::shot_theme(&args),
        );
        frame.settle_for_capture(ui::launch::CAPTURE_SETTLE_BOUND);
        frame.render_png(out);
        println!("tilo-greeter: wrote shot to {}", out.display());
        return Ok(());
    }

    // Configure the kiosk output before winit observes its logical size.
    // Session and sign-in use the same physical panel-DPI policy.
    if let Err(error) = greeter::output_scale::configure() {
        tracing::warn!(%error, "greeter output scale remains compositor-controlled");
    }

    let wake_scope = ui::wake::WakeScope::new();
    let _wake_entry = wake_scope.enter();
    // Slot the event loop populates once it starts; the greeter reads it to
    // spawn the background auth worker.
    let proxy_slot: Arc<OnceLock<EventLoopProxy<UserEvent>>> = Arc::new(OnceLock::new());
    let console = console_control::ConsoleServer::new();
    let widget = GreeterApp::new()
        .with_proxy_slot(proxy_slot.clone())
        .with_console(console.clone());
    let state = ui::State::new();
    let mut app = ui::App::from_scoped_parts(wake_scope, state, Box::new(widget))
        .without_native_document_chrome();
    tilo_startup_metrics::mark("greeter-app-ready");
    // On a personal machine with one unambiguous interactive account, the
    // greeter hides the redundant username field and this lands on Passcode.
    // Zero/multiple candidates retain the two-field fallback and autofocus
    // lands on Username. Either way, typing can begin without a click.
    app.autofocus_first = true;
    // Preserve keyboard focus across the structural rebuilds triggered by
    // session, status, and Caps-Lock changes — without this the caret and
    // focus are dropped on every status update. Those rebuilds leave
    // `take_focus_reset` clear, so focus is *preserved* at the same
    // focusable ordinal; only an auth-phase transition raises it, because
    // that one enables/disables the inputs and so changes which widgets are
    // focusable at all.
    app.autofocus_on_structure_change = true;
    tilo_startup_metrics::mark("greeter-run-start");
    app.run_with_setup(move |proxy| {
        serve_console(&console, proxy.clone());
        let _ = proxy_slot.set(proxy);
    });
    Ok(())
}

/// Start the console endpoints that exist here: the greeter user's private
/// runtime socket, and the virtio port only a hypervisor can attach.
fn serve_console(console: &console_control::ConsoleServer, proxy: EventLoopProxy<UserEvent>) {
    let waker: console_control::server::Waker = Arc::new(move || {
        let _ = proxy.send_event(UserEvent::External(
            greeter::greeter_app::CONSOLE_WAKE.to_owned(),
        ));
    });
    console.serve_discovered(greeter::greeter_app::CONSOLE_PROVIDER, &waker);
}

/// Every `--scenario` name and the login-card state it captures.
const SCENARIOS: [(&str, AuthScenario); 22] = [
    ("form", AuthScenario::Form),
    ("error", AuthScenario::Error),
    ("authenticating", AuthScenario::Authenticating),
    ("fingerprint", AuthScenario::Fingerprint),
    ("caps", AuthScenario::CapsLock),
    ("hint", AuthScenario::Hint),
    ("starting", AuthScenario::Starting),
    ("longname", AuthScenario::LongName),
    ("personal", AuthScenario::Personal),
    ("personal-error", AuthScenario::PersonalError),
    (
        "personal-authenticating",
        AuthScenario::PersonalAuthenticating,
    ),
    ("personal-caps", AuthScenario::PersonalCapsLock),
    ("personal-hint", AuthScenario::PersonalHint),
    ("personal-starting", AuthScenario::PersonalStarting),
    (
        "personal-no-fingerprint",
        AuthScenario::PersonalNoFingerprint,
    ),
    ("personal-locked", AuthScenario::PersonalLocked),
    (
        "personal-session-failed",
        AuthScenario::PersonalSessionFailed,
    ),
    ("personal-service-down", AuthScenario::PersonalServiceDown),
    (
        "personal-passcode-expired",
        AuthScenario::PersonalPasscodeExpired,
    ),
    ("personal-sessions", AuthScenario::PersonalSessions),
    (
        "personal-repeated-error",
        AuthScenario::PersonalRepeatedError,
    ),
    ("accounts", AuthScenario::Accounts),
];

/// The login-card state a `--scenario` name captures; an unknown name's error
/// lists the known ones.
fn scenario_named(name: &str) -> Result<AuthScenario, String> {
    SCENARIOS
        .iter()
        .find(|(known, _)| *known == name)
        .map(|&(_, scenario)| scenario)
        .ok_or_else(|| {
            let known: Vec<&str> = SCENARIOS.iter().map(|(known, _)| *known).collect();
            format!("unknown --scenario {name:?}; one of: {}", known.join(", "))
        })
}

/// Credentials to seed for a `--shots` render, as `(username, password)`,
/// or `None` to leave both fields empty.
///
/// Live runs always start empty; these exist only so each snapshot shows a
/// representative frame of the state it is named after.
fn shot_credentials(scenario: AuthScenario) -> Option<(&'static str, &'static str)> {
    match scenario {
        // The first frame anyone ever sees: both fields empty.
        AuthScenario::Form
        | AuthScenario::Personal
        | AuthScenario::PersonalNoFingerprint
        | AuthScenario::PersonalSessions
        | AuthScenario::Accounts => None,
        // Hint/error states deliberately show the missing passcode. Busy
        // states do too, for a more important reason: the live submit path
        // moves the passcode into a zeroizing worker value before entering
        // either busy phase, while fingerprint submission scrubs any typed
        // passcode outright. Keep visual evidence honest; it must never imply
        // that a secret remains in the disabled field after submission.
        AuthScenario::Hint
        | AuthScenario::Error
        | AuthScenario::PersonalHint
        | AuthScenario::PersonalError
        | AuthScenario::PersonalRepeatedError
        | AuthScenario::PersonalLocked
        | AuthScenario::PersonalSessionFailed
        | AuthScenario::PersonalServiceDown
        | AuthScenario::PersonalPasscodeExpired
        | AuthScenario::Authenticating
        | AuthScenario::Fingerprint
        | AuthScenario::Starting
        | AuthScenario::PersonalAuthenticating
        | AuthScenario::PersonalStarting => Some(("ada", "")),
        // Long enough to overrun the field, so the snapshot proves how it
        // degrades rather than leaving it to chance.
        AuthScenario::LongName => Some(("bartholomew.featherstonehaugh-worthington", "hunter2")),
        _ => Some(("ada", "hunter2")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personal_transient_scenarios_are_capturable() {
        for (name, expected) in [
            ("personal-error", AuthScenario::PersonalError),
            (
                "personal-authenticating",
                AuthScenario::PersonalAuthenticating,
            ),
            ("personal-caps", AuthScenario::PersonalCapsLock),
            ("personal-hint", AuthScenario::PersonalHint),
            ("personal-starting", AuthScenario::PersonalStarting),
        ] {
            assert_eq!(scenario_named(name), Ok(expected));
        }
    }

    #[test]
    fn busy_shots_do_not_retain_representative_passcodes() {
        for scenario in [
            AuthScenario::Authenticating,
            AuthScenario::Fingerprint,
            AuthScenario::Starting,
            AuthScenario::PersonalAuthenticating,
            AuthScenario::PersonalStarting,
        ] {
            assert_eq!(
                shot_credentials(scenario).map(|(_, passcode)| passcode),
                Some(""),
                "{scenario:?} must mirror the live path after secret handoff"
            );
        }
    }
}
