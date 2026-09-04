// Thin binary entry point for the qOS graphical greeter — all logic
// lives in the library (mirrors apps/file-explorer/src/main.rs).
//
// Modes:
//   * default — build a `GreeterApp`, hand it to `ui::App`, run the live
//     winit event loop. Launched at the greetd VT under `cage`:
//         cage -s -- /usr/bin/qos-greeter
//     The event-loop proxy is captured via `run_with_setup` so the
//     background auth worker can pump prompts/results back into the UI.
//   * `--shots <PATH> [--scenario <name>] [--viewport <WIDTH>x<HEIGHT>]`
//     — render one login-card state
//     into a PNG at `<PATH>` and exit. No window, no event loop. Headless
//     snapshot for CI / visual inspection (identical output to the live
//     binary). Scenarios: `form`, `hint`, `error`, `authenticating`,
//     `fingerprint`, `starting`, `caps`, `longname`, `personal`, and
//     `personal-no-fingerprint`. Defaults to `form`.

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

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use greeter::GreeterApp;
use greeter::greeter_app::AuthScenario;
use taffy::prelude::*;
use ui::app::UserEvent;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;
use winit::event_loop::EventLoopProxy;

/// Default offscreen viewport for `--shots`.
const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

/// # Errors
///
/// Returns an error if `--shots` rendering fails (see
/// [`render_shot_to_png`]); the live event-loop path does not return.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_shots_arg(&args) {
        let scenario = parse_scenario_arg(&args).unwrap_or(AuthScenario::Form);
        let viewport = parse_viewport_arg(&args).unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        let text_scale = parse_text_scale_arg(&args);
        let widget = GreeterApp::for_shot(scenario);
        render_shot_to_png(
            Box::new(widget),
            viewport,
            text_scale,
            &out,
            shot_credentials(scenario),
        )?;
        return Ok(());
    }

    // Slot the event loop populates once it starts; the greeter reads it to
    // spawn the background auth worker.
    let proxy_slot: Arc<OnceLock<EventLoopProxy<UserEvent>>> = Arc::new(OnceLock::new());
    let widget = GreeterApp::new().with_proxy_slot(proxy_slot.clone());
    let state = ui::State::new();
    let mut app = ui::App::new(state, Box::new(widget));
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
    app.run_with_setup(move |proxy| {
        let _ = proxy_slot.set(proxy);
    });
    Ok(())
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
        // The whole point of the hint state is the *missing* password.
        AuthScenario::Hint => Some(("quinten", "")),
        // Long enough to overrun the field, so the snapshot proves how it
        // degrades rather than leaving it to chance.
        AuthScenario::LongName => Some(("bartholomew.featherstonehaugh-worthington", "hunter2")),
        _ => Some(("quinten", "hunter2")),
    }
}

/// Run one offscreen frame against `widget` at `viewport` and write the
/// resulting RGBA to `out` as a PNG. Mirrors `ui::App`'s
/// `sync → build_layout → compute_layout → apply_layout → draw` cycle.
///
/// `seed` pre-populates the username/password CRDT objects so the
/// non-form screens render representative text. Because the composed
/// children are only created *inside* `build_layout`, we sync a second time
/// afterwards so those children (the `TextInputs`) pull their values from the
/// document — the live App achieves the same via its per-frame sync.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    text_scale: Option<f32>,
    out: &std::path::Path,
    seed: Option<(&str, &str)>,
) -> Result<(), Box<dyn std::error::Error>> {
    use automerge::{ObjType, transaction::Transactable};

    let (w, h) = viewport;
    // Viewport dimensions are screen/image sizes (1280x800 by default),
    // always far under f32's 24-bit mantissa — never enough to lose precision.
    #[allow(
        clippy::cast_precision_loss,
        reason = "viewport dimensions are screen/image sizes, always far under 2^24"
    )]
    let (wf, hf) = (w as f32, h as f32);
    let theme = text_scale.map_or_else(Theme::dark, |scale| Theme::dark().with_text_scale(scale));
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let mut state = ui::State::new();

    if let Some((username, password)) = seed {
        let uobj = state
            .doc
            .put_object(&state.root, "greeter.username", ObjType::Text)?;
        let _ = state.doc.splice_text(&uobj, 0, 0, username);
        let pobj = state
            .doc
            .put_object(&state.root, "greeter.password", ObjType::Text)?;
        let _ = state.doc.splice_text(&pobj, 0, 0, password);
    }

    widget.sync(&mut state.doc, &state.root);

    let child = widget.build_layout(&mut taffy, &theme, &mut node_map);

    // Second sync so the freshly-composed children pull from the doc.
    widget.sync(&mut state.doc, &state.root);

    let root = taffy.new_with_children(
        Style {
            size: Size {
                width: Dimension::length(wf),
                height: Dimension::length(hf),
            },
            ..Default::default()
        },
        &[child],
    )?;

    {
        let measure_map = node_map.clone();
        let tr = &mut text_renderer;
        taffy.compute_layout_with_measure(
            root,
            taffy::Size {
                width: AvailableSpace::Definite(wf),
                height: AvailableSpace::Definite(hf),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )?;
    }
    widget.apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
    );

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("qos-greeter: wrote shot to {}", out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn viewport_accepts_separate_and_equals_forms() {
        assert_eq!(
            parse_viewport_arg(&args(&["qos-greeter", "--viewport", "640x480"])),
            Some((640, 480))
        );
        assert_eq!(
            parse_viewport_arg(&args(&["qos-greeter", "--viewport=360X640"])),
            Some((360, 640))
        );
    }

    #[test]
    fn viewport_rejects_malformed_or_empty_dimensions() {
        for raw in ["", "640", "x480", "640x", "0x480", "640x0", "-1x480"] {
            assert_eq!(
                parse_viewport_arg(&args(&["qos-greeter", "--viewport", raw])),
                None,
                "accepted malformed viewport {raw:?}"
            );
        }
    }

    #[test]
    fn shot_text_scale_accepts_only_bounded_finite_values() {
        assert_eq!(
            parse_text_scale_arg(&args(&["qos-greeter", "--text-scale=1.3"])),
            Some(1.3)
        );
        for value in ["NaN", "0.5", "2.1", "not-a-number"] {
            assert_eq!(
                parse_text_scale_arg(&args(&["qos-greeter", "--text-scale", value])),
                None
            );
        }
    }
}
