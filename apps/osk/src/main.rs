//! Thin binary entry point for the Tilo on-screen keyboard.
//!
//! Two modes, mirroring the other `ui` apps (e.g. file-explorer):
//!   * default — build an [`OskWidget`], hand it to `ui::App`, run the live
//!     winit event loop.
//!   * `--shots <PATH>` — render the keyboard at a default size into a PNG at
//!     `<PATH>` and exit. No window, no event loop. Used for headless
//!     screenshot tests and CI snapshots.
//!
//! ## Tilo integration
//!
//! The session compositor supervises this signed service when the persisted
//! accessibility preference is enabled. It gives the service a dedicated,
//! unexported Wayland socket. Only connections accepted on that socket can see
//! `input-method-v2` or `virtual-keyboard-v1`; copying the app-id on the public
//! display grants no input authority.
//!
//! The main xdg-toplevel announces the stable [`osk::KEYBOARD_APP_ID`]. The
//! compositor requires both that id and the private-socket credential before it
//! promotes the surface out of the workspace tree into a bottom floating
//! overlay. Input-method activation makes the initially empty keyboard visible;
//! leaving the focused text field suppresses it again. Pointer focus may enter
//! the keyboard, but its taps deliberately do not take keyboard focus from the
//! edited field.
//!
//! Typed and swiped text follows the protocol-clean `commit_string` path. A
//! narrowly gated virtual keyboard handles Backspace because winit's current
//! `text-input-v3` client does not consume `delete_surrounding_text`. On other
//! compositors, the binary may use its standalone virtual-keyboard fallback;
//! Tilo never advertises that privileged global on its public display.
//!
//! Swipe decoding and prediction are live: the real FUTO model is selected
//! when its runtime assets are available, otherwise the in-tree geometric
//! decoder provides the same candidate UI.

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
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::sync::{Arc, Mutex};

use osk::InputPurpose;
use osk::OskWidget;
use osk::imclient::{ImState, InputMethodClient, try_input_method};
use osk::inject::default_injector;
use osk::input_purpose_name;
use ui::app::UserEvent;
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::widget::Widget;

/// Default offscreen viewport for `--shots`. Phone-portrait-ish width with
/// just enough height for the toolbar + suggestion bar + four key rows
/// (48 + 40 + 4×56 = 312, rounded up).
const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (720, 360);

/// # Errors
///
/// Returns an error when `--input-method` is explicitly requested but the
/// compositor does not advertise `zwp_input_method_manager_v2`, or when
/// `--shots` fails to render (taffy layout failure, e.g. the tree exceeds its
/// node capacity).
#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    ui::launch::run_or_exit(
        &LaunchSpec::new_with_args("so.tilo.osk", shot_root)
            .shot_root(shot_root)
            .shot_viewport(|_| DEFAULT_SHOTS_VIEWPORT)
            .allowed_live_flags(&["--input-method", "--no-input-method"])
            .allowed_live_options(&["--purpose"])
            .disable_headless()
            .live_handler(run_live),
    );
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by LaunchSpec's argument-aware root factory"
)]
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    let mut widget = OskWidget::new();
    if let Some(purpose) = parse_purpose_arg(args) {
        widget.set_purpose(purpose);
    }
    Ok(Box::new(widget))
}

fn run_live(args: &LaunchArgs) -> Result<(), LaunchError> {
    let purpose = parse_purpose_arg(args);

    // --- pick the input path. Prefer auto-show via input-method-v2 when the
    // compositor supports it: the keyboard appears on text focus, follows the
    // field's content purpose, hides on blur, and commits text via the IM. Fall
    // back to the always-visible keyboard + virtual-keyboard injection
    // otherwise. ---
    let im_pref = parse_input_method_pref(args);
    let im = match im_pref {
        // Force the always-visible path; never try the input method.
        InputMethodPref::Off => None,
        // Try; fail loudly if explicitly requested but unavailable.
        InputMethodPref::Require => match try_input_method() {
            Some(im) => Some(im),
            None => {
                return Err(LaunchError::Operation(
                    "osk: --input-method requested but the compositor does not advertise \
                     zwp_input_method_manager_v2"
                        .into(),
                ));
            }
        },
        // Default: use it if present, otherwise degrade silently.
        InputMethodPref::Auto => try_input_method(),
    };

    if let Some(im) = im {
        tracing::info!("osk: auto-show via input-method-v2");
        run_auto_show(im, purpose);
    } else {
        run_always_visible(purpose);
    }
    Ok(())
}

/// The always-visible keyboard: it fills its surface and types into the focused
/// app via `virtual-keyboard-v1` (or a no-op when that protocol is absent). This
/// is the path when there is no input method to drive auto-show.
fn run_always_visible(purpose: Option<InputPurpose>) {
    let wake_scope = ui::wake::WakeScope::new();
    let _wake_entry = wake_scope.enter();
    let mut injector = default_injector();
    tracing::info!("osk: text-injection backend = {}", injector.backend_name());
    let mut widget = OskWidget::new().on_key(move |action| {
        injector.key_action(&action);
    });
    if let Some(p) = purpose {
        widget.set_purpose(p);
    }

    // Production uses a wlr-layer-shell overlay; this launch path uses a plain
    // `ui::App` window. `ui::App::run()` owns the event loop, and `on_key`
    // forwards each committed key synchronously.
    // Deliberately no `--headless-drive` hook. Every key this widget commits
    // is forwarded to `injector`, which types into whatever application holds
    // focus in the live session — driving it from a test harness would inject
    // keystrokes into the user's desktop.

    let (state, autosave) = ui::persist::open("so.tilo.osk");
    let app = ui::App::from_scoped_parts(wake_scope, state, Box::new(widget))
        .with_autosave(autosave)
        .with_window_app_id(osk::KEYBOARD_APP_ID);
    app.run();
}

/// The auto-show keyboard, driven by an `input-method-v2` client.
///
/// The keyboard starts hidden. A background thread non-blockingly pumps the
/// input-method connection; each focus event is folded through an [`ImState`]
/// reducer into a minimal show/hide/purpose command, which it sends to the
/// winit loop via the `EventLoopProxy` as a [`UserEvent::External`] string the
/// widget understands (`osk:show`, `osk:hide`, `osk:purpose:<name>`).
/// Typed/swiped text is committed back to the focused app via the same input
/// method (`commit_string`).
fn run_auto_show(im: InputMethodClient, purpose: Option<InputPurpose>) {
    let wake_scope = ui::wake::WakeScope::new();
    let _wake_entry = wake_scope.enter();
    // The IM client is shared: the winit thread's `on_key` callback commits
    // typed text through it, while the background thread reads focus events.
    let im = Arc::new(Mutex::new(im));

    let im_for_keys = Arc::clone(&im);
    // `text-input-v3` in winit intentionally does not implement
    // delete_surrounding_text yet. Keep the compositor-gated virtual keyboard
    // only for non-inserting editing keys such as Backspace; printable text
    // stays on the input-method commit path.
    let mut editing_keys = default_injector();
    let mut widget = OskWidget::new().hidden().on_key(move |action| {
        if matches!(action, osk::KeyAction::Backspace) {
            editing_keys.key_action(&action);
            return;
        }
        // Turn the committed action into text and send it via the input method.
        // State-only actions (shift/symbols/layout) and Backspace produce no
        // commit string; the guarded virtual keyboard path above handles the
        // latter until winit consumes delete_surrounding_text.
        if let Some(text) = osk::inject::action_symbols(&action)
            .into_iter()
            .map(symbol_to_commit_text)
            .collect::<Option<String>>()
            && !text.is_empty()
            && let Ok(mut im) = im_for_keys.lock()
        {
            im.commit_text(&text);
        }
    });
    // A `--purpose` override still applies as the initial layout; the field's
    // content_type will replace it once focus lands.
    if let Some(p) = purpose {
        widget.set_purpose(p);
    }

    let state = ui::State::new();
    let app = ui::App::from_scoped_parts(wake_scope, state, Box::new(widget))
        .with_window_app_id(osk::KEYBOARD_APP_ID);

    // Spawn the input-method reader once the event loop proxy exists, so it can
    // push show/hide/purpose commands into the loop.
    let im_for_reader = Arc::clone(&im);
    app.run_with_setup(move |proxy| {
        std::thread::spawn(move || {
            let mut model = ImState::new();
            loop {
                // Poll without blocking, then release the lock immediately so the
                // winit thread's `on_key` callback can take it to commit typed
                // text. (Blocking on the socket while holding the lock would
                // deadlock against that callback.) Focus changes are rare and
                // not latency-critical, so a short sleep between polls is fine.
                let events = {
                    let Ok(mut im) = im_for_reader.lock() else {
                        break;
                    };
                    im.poll()
                };
                for ev in events {
                    let t = model.reduce(ev);
                    // Purpose first, so the first mapped frame already has
                    // the correct layout instead of flashing the text layout.
                    if let Some(p) = t.purpose_changed {
                        let cmd = format!("osk:purpose:{}", input_purpose_name(p));
                        if proxy.send_event(UserEvent::External(cmd)).is_err() {
                            return;
                        }
                    }
                    if let Some(visible) = t.visibility_changed {
                        let cmd = if visible { "osk:show" } else { "osk:hide" };
                        if proxy
                            .send_event(UserEvent::External(cmd.to_string()))
                            .is_err()
                        {
                            return; // event loop gone
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(16));
            }
        });
    });
}

/// The text a single [`osk::Symbol`] inserts when committed through the input
/// method, or `None` for symbols that aren't a plain inserted character (so the
/// whole commit is skipped — e.g. Backspace, handled by the guarded virtual
/// keyboard fallback).
fn symbol_to_commit_text(sym: osk::Symbol) -> Option<char> {
    match sym {
        osk::Symbol::Unicode(c) => Some(c),
        osk::Symbol::Space => Some(' '),
        osk::Symbol::Return => Some('\n'),
        osk::Symbol::Tab => Some('\t'),
        osk::Symbol::BackSpace => None,
    }
}

/// How the binary should treat the input method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputMethodPref {
    /// Use the input method if the compositor offers it, else fall back.
    Auto,
    /// Require it — error out if the compositor doesn't offer it.
    Require,
    /// Never use it — always-visible keyboard.
    Off,
}

/// Read the input-method preference from the args. `--no-input-method` forces it
/// off; `--input-method` requires it; absence means auto-detect.
fn parse_input_method_pref(args: &LaunchArgs) -> InputMethodPref {
    if args.has("--no-input-method") {
        InputMethodPref::Off
    } else if args.has("--input-method") {
        InputMethodPref::Require
    } else {
        InputMethodPref::Auto
    }
}

/// Returns the [`InputPurpose`] requested by `--purpose <kind>` (or
/// `--purpose=<kind>`), or `None` when absent. An unrecognised kind is ignored
/// (the keyboard falls back to plain text) so a typo doesn't abort launch.
fn parse_purpose_arg(args: &LaunchArgs) -> Option<InputPurpose> {
    args.value("--purpose").and_then(purpose_from_str)
}

/// Map a `--purpose` value to an [`InputPurpose`]. Accepts both the short
/// spellings a user is likely to type and the enum-ish names.
fn purpose_from_str(s: &str) -> Option<InputPurpose> {
    Some(match s.to_ascii_lowercase().as_str() {
        "text" => InputPurpose::Text,
        "number" | "numeric" => InputPurpose::Number,
        "decimal" => InputPurpose::Decimal,
        "phone" | "tel" | "telephone" => InputPurpose::Telephone,
        "url" => InputPurpose::Url,
        "email" => InputPurpose::Email,
        "pin" => InputPurpose::Pin,
        "password" | "pass" => InputPurpose::Password,
        "date" => InputPurpose::Date,
        "time" => InputPurpose::Time,
        _ => return None,
    })
}
