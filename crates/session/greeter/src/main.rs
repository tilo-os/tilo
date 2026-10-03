// Thin binary entry point for the Tilo graphical greeter — all logic
// lives in the library (mirrors apps/file-explorer/src/main.rs).
//
// Modes:
//   * default — build a `GreeterApp`, hand it to `ui::App`, run the live
//     winit event loop. Launched at the greetd VT under `cage`:
//         cage -s -- /usr/bin/tilo-greeter
//     The event-loop proxy is captured via `run_with_setup` so the
//     background auth worker can pump prompts/results back into the UI.
//   * `--shots <PATH> [--scenario <name>] [--viewport <WIDTH>x<HEIGHT>]`
//     — render one login-card state
//     into a PNG at `<PATH>` and exit. No window, no event loop. Headless
//     snapshot for CI / visual inspection (identical output to the live
//     binary). Scenarios: `form`, `hint`, `error`, `authenticating`,
//     `fingerprint`, `starting`, `caps`, `longname`, `personal`, and
//     `personal-no-fingerprint`, and personal-device transient variants.
//     Defaults to `form`. `--time <seconds>` freezes the decorative backdrop;
//     `TILO_SHOTS_THEME=light|dark|high-contrast` selects the review palette.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::sync::{Arc, OnceLock};

use greeter::GreeterApp;
use greeter::greeter_app::AuthScenario;
use ui::app::UserEvent;
use ui::widget::Widget;
use winit::event_loop::EventLoopProxy;

/// Default offscreen viewport for `--shots`.
const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

/// # Errors
///
/// Returns an error if `--shots` rendering fails (see
/// [`render_shot_to_png`]); the live event-loop path does not return.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tilo_startup_metrics::init();
    let _startup_metrics = tilo_startup_metrics::ScopedDumper;
    tilo_startup_metrics::mark("greeter-main-start");
    let _detached = ui::wake::WakeScope::enter_detached();
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_shots_arg(&args) {
        let scenario = parse_scenario_arg(&args).unwrap_or(AuthScenario::Form);
        let viewport = parse_viewport_arg(&args).unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        let text_scale = parse_text_scale_arg(&args);
        let mut widget = GreeterApp::for_shot(scenario);
        widget.set_shot_time(parse_time_arg(&args).unwrap_or(0.0));
        let credentials = shot_credentials(scenario);
        if let Some((_, password)) = credentials {
            widget.seed_shot_password(password);
        }
        render_shot_to_png(
            Box::new(widget),
            viewport,
            text_scale,
            &out,
            credentials.map(|(username, _)| username),
        )?;
        return Ok(());
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
    let proxy = std::sync::Mutex::new(proxy);
    let waker: console_control::server::Waker = Arc::new(move || {
        if let Ok(proxy) = proxy.lock() {
            let _ = proxy.send_event(UserEvent::External(
                greeter::greeter_app::CONSOLE_WAKE.to_owned(),
            ));
        }
    });
    let endpoints = console_control::Endpoint::discover(greeter::greeter_app::CONSOLE_PROVIDER);
    let failed = console.serve_all(&endpoints, &waker);
    for endpoint in &endpoints {
        if let Some((_, error)) = failed.iter().find(|(candidate, _)| candidate == endpoint) {
            tracing::warn!(?endpoint, %error, "greeter console endpoint unavailable");
        } else {
            tracing::info!(?endpoint, "greeter console listening");
        }
    }
}

/// Returns the optional logical-pixel viewport supplied to the headless
/// renderer. Malformed or zero-sized values are ignored, preserving the
/// stable default used by existing visual baselines.
fn parse_viewport_arg(args: &[String]) -> Option<(u32, u32)> {
    let mut iter = args.iter().skip(1);
    let raw = loop {
        let arg = iter.next()?;
        if arg == "--viewport" {
            break iter.next()?.as_str();
        }
        if let Some(rest) = arg.strip_prefix("--viewport=") {
            break rest;
        }
    };
    let (width, height) = raw.split_once(['x', 'X'])?;
    let viewport = (width.parse().ok()?, height.parse().ok()?);
    (viewport.0 > 0 && viewport.1 > 0).then_some(viewport)
}

/// A deterministic animation phase used only by the offscreen renderer.
fn parse_time_arg(args: &[String]) -> Option<f64> {
    let mut iter = args.iter().skip(1);
    let raw = loop {
        let arg = iter.next()?;
        if arg == "--time" {
            break iter.next()?.as_str();
        }
        if let Some(rest) = arg.strip_prefix("--time=") {
            break rest;
        }
    };
    let seconds: f64 = raw.parse().ok()?;
    (seconds.is_finite() && seconds >= 0.0).then_some(seconds)
}

fn parse_text_scale_arg(args: &[String]) -> Option<f32> {
    let mut iter = args.iter().skip(1);
    let raw = loop {
        let arg = iter.next()?;
        if arg == "--text-scale" {
            break iter.next()?.as_str();
        }
        if let Some(rest) = arg.strip_prefix("--text-scale=") {
            break rest;
        }
    };
    let scale: f32 = raw.parse().ok()?;
    (scale.is_finite() && (0.75..=2.0).contains(&scale)).then_some(scale)
}

/// Returns `Some(path)` if `--shots <path>` (or `--shots=<path>`) appears
/// in `args`, otherwise `None`.
fn parse_shots_arg(args: &[String]) -> Option<std::path::PathBuf> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--shots" {
            return iter.next().map(std::path::PathBuf::from);
        }
        if let Some(rest) = arg.strip_prefix("--shots=") {
            return Some(std::path::PathBuf::from(rest));
        }
    }
    None
}

/// Returns the `--scenario <name>` (or `--scenario=<name>`) login-card
/// state for a shot, if present and recognised.
fn parse_scenario_arg(args: &[String]) -> Option<AuthScenario> {
    let mut iter = args.iter().skip(1);
    let raw = loop {
        let arg = iter.next()?;
        if arg == "--scenario" {
            break iter.next()?.clone();
        }
        if let Some(rest) = arg.strip_prefix("--scenario=") {
            break rest.to_string();
        }
    };
    match raw.as_str() {
        "form" => Some(AuthScenario::Form),
        "error" => Some(AuthScenario::Error),
        "authenticating" => Some(AuthScenario::Authenticating),
        "fingerprint" => Some(AuthScenario::Fingerprint),
        "caps" | "capslock" | "caps-lock" => Some(AuthScenario::CapsLock),
        "hint" => Some(AuthScenario::Hint),
        "starting" => Some(AuthScenario::Starting),
        "longname" | "long-name" | "long" => Some(AuthScenario::LongName),
        "personal" => Some(AuthScenario::Personal),
        "personal-error" => Some(AuthScenario::PersonalError),
        "personal-authenticating" => Some(AuthScenario::PersonalAuthenticating),
        "personal-caps" | "personal-caps-lock" => Some(AuthScenario::PersonalCapsLock),
        "personal-hint" => Some(AuthScenario::PersonalHint),
        "personal-starting" => Some(AuthScenario::PersonalStarting),
        "personal-no-fingerprint" | "personal-passcode" => {
            Some(AuthScenario::PersonalNoFingerprint)
        }
        _ => None,
    }
}

/// Credentials to seed for a `--shots` render, as `(username, password)`,
/// or `None` to leave both fields empty.
///
/// Live runs always start empty; these exist only so each snapshot shows a
/// representative frame of the state it is named after.
fn shot_credentials(scenario: AuthScenario) -> Option<(&'static str, &'static str)> {
    match scenario {
        // The first frame anyone ever sees: both fields empty.
        AuthScenario::Form | AuthScenario::Personal | AuthScenario::PersonalNoFingerprint => None,
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
        | AuthScenario::Authenticating
        | AuthScenario::Fingerprint
        | AuthScenario::Starting
        | AuthScenario::PersonalAuthenticating
        | AuthScenario::PersonalStarting => Some(("quinten", "")),
        // Long enough to overrun the field, so the snapshot proves how it
        // degrades rather than leaving it to chance.
        AuthScenario::LongName => Some(("bartholomew.featherstonehaugh-worthington", "hunter2")),
        _ => Some(("quinten", "hunter2")),
    }
}

/// Run one offscreen frame against `widget` at `viewport` and write the
/// resulting RGBA to `out` as a PNG.
///
/// `username` pre-populates the non-secret username CRDT object. The caller
/// seeds representative passcode text through `GreeterApp`'s process-local
/// buffer, never the document. The headless harness runs the live App's
/// per-frame sync, so children composed inside `build_layout` (the
/// `TextInputs`) pull their values from the document before they paint.
fn render_shot_to_png(
    widget: Box<dyn Widget>,
    viewport: (u32, u32),
    text_scale: Option<f32>,
    out: &std::path::Path,
    username: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    use automerge::{ObjType, transaction::Transactable};

    let theme = ui::launch::capture_theme(text_scale);
    let mut state = ui::State::new();
    if let Some(username) = username {
        let uobj = state
            .doc
            .put_object(&state.root, "greeter.username", ObjType::Text)?;
        state.doc.splice_text(&uobj, 0, 0, username)?;
    }
    // The same bounded sync/layout/paint lifecycle as the live host handles
    // responsive composition and applies accessibility scaling to text too.
    let frame = ui::headless::FrameHarness::new_with_theme(state, widget, viewport, theme);
    frame.render_png(out);
    println!("tilo-greeter: wrote shot to {}", out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn cli_arguments_are_bounded_and_additive() {
        // Phase `viewport_accepts_separate_and_equals_forms`.
        {
            assert_eq!(
                parse_viewport_arg(&args(&["tilo-greeter", "--viewport", "640x480"])),
                Some((640, 480))
            );
            assert_eq!(
                parse_viewport_arg(&args(&["tilo-greeter", "--viewport=360X640"])),
                Some((360, 640))
            );
        }
        // Phase `viewport_rejects_malformed_or_empty_dimensions`.
        {
            for raw in ["", "640", "x480", "640x", "0x480", "640x0", "-1x480"] {
                assert_eq!(
                    parse_viewport_arg(&args(&["tilo-greeter", "--viewport", raw])),
                    None,
                    "accepted malformed viewport {raw:?}"
                );
            }
        }
        // Phase `shot_text_scale_accepts_only_bounded_finite_values`.
        {
            assert_eq!(
                parse_text_scale_arg(&args(&["tilo-greeter", "--text-scale=1.3"])),
                Some(1.3)
            );
            for value in ["NaN", "0.5", "2.1", "not-a-number"] {
                assert_eq!(
                    parse_text_scale_arg(&args(&["tilo-greeter", "--text-scale", value])),
                    None
                );
            }
        }
        // Phase `personal_transient_scenarios_are_additive_cli_options`.
        {
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
                assert_eq!(
                    parse_scenario_arg(&args(&["tilo-greeter", "--scenario", name])),
                    Some(expected)
                );
            }
        }
    }

    #[test]
    fn shot_rendering_is_finite_and_retains_no_passcodes() {
        // Phase `animation_phase_is_finite_and_nonnegative`.
        {
            assert_eq!(
                parse_time_arg(&args(&["tilo-greeter", "--time=8"])),
                Some(8.0)
            );
            assert_eq!(
                parse_time_arg(&args(&["tilo-greeter", "--time", "1.5"])),
                Some(1.5)
            );
            for value in ["NaN", "inf", "-1", "not-a-number"] {
                assert_eq!(
                    parse_time_arg(&args(&["tilo-greeter", "--time", value])),
                    None
                );
            }
        }
        // Phase `busy_shots_do_not_retain_representative_passcodes`.
        {
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
}
