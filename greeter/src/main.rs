// Thin binary entry point for the qOS graphical greeter — all logic
// lives in the library (mirrors apps/file-explorer/src/main.rs).
//
// Modes:
//   * default — build a `GreeterApp`, hand it to `ui::App`, run the live
//     winit event loop. Launched at the greetd VT under `cage`:
//         cage -s -- /usr/bin/qos-greeter
//     The event-loop proxy is captured via `run_with_setup` so the
//     background auth worker can pump prompts/results back into the UI.
//   * `--shots <PATH> [--scenario form|error|authenticating|fingerprint|caps]`
//     — render one login-card state into a PNG at `<PATH>` and exit. No
//     window, no event loop. Headless snapshot for CI / visual inspection
//     (identical output to the live binary). `--scenario` defaults to
//     `form`.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use greeter::greeter_app::AuthScenario;
use greeter::GreeterApp;
use ui::app::UserEvent;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use taffy::prelude::*;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;
use vello::Scene;
use winit::event_loop::EventLoopProxy;

/// Default offscreen viewport for `--shots`.
const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_shots_arg(&args) {
        let scenario = parse_scenario_arg(&args).unwrap_or(AuthScenario::Form);
        // Seed sample credentials for every state except the empty form so
        // the snapshot is representative (live runs start with empty
        // fields).
        let seed = !matches!(scenario, AuthScenario::Form);
        let widget = GreeterApp::for_shot(scenario);
        render_shot_to_png(Box::new(widget), DEFAULT_SHOTS_VIEWPORT, &out, seed)?;
        return Ok(());
    }

    // Slot the event loop populates once it starts; the greeter reads it to
    // spawn the background auth worker.
    let proxy_slot: Arc<OnceLock<EventLoopProxy<UserEvent>>> = Arc::new(OnceLock::new());
    let widget = GreeterApp::new().with_proxy_slot(proxy_slot.clone());
    let state = ui::State::new();
    let mut app = ui::App::new(state, Box::new(widget));
    // Hand the keyboard to the username field at startup so the user can
    // type credentials immediately, with no click required.
    app.autofocus_first = true;
    // Preserve keyboard focus across the structural rebuilds triggered by
    // session-cycle, status, and auth-phase changes — without this the
    // caret/focus is dropped on every status update (deliverable #4). The
    // greeter never raises `take_focus_reset`, so focus is *preserved* at
    // the same focusable ordinal rather than reset to the first field.
    app.autofocus_on_structure_change = true;
    app.run_with_setup(move |proxy| {
        let _ = proxy_slot.set(proxy);
    });
    Ok(())
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
        _ => None,
    }
}

/// Run one offscreen frame against `widget` at `viewport` and write the
/// resulting RGBA to `out` as a PNG. Mirrors `ui::App`'s
/// `sync → build_layout → compute_layout → apply_layout → draw` cycle.
///
/// `seed_creds` pre-populates the username/password CRDT objects so the
/// non-form screens render representative text. Because the composed
/// children are only created *inside* `build_layout`, we sync a second time
/// afterwards so those children (the TextInputs) pull their values from the
/// document — the live App achieves the same via its per-frame sync.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &std::path::Path,
    seed_creds: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use automerge::{transaction::Transactable, ObjType};

    let (w, h) = viewport;
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let mut state = ui::State::new();

    if seed_creds {
        let uobj = state
            .doc
            .put_object(&state.root, "greeter.username", ObjType::Text)?;
        let _ = state.doc.splice_text(&uobj, 0, 0, "quinten");
        let pobj = state
            .doc
            .put_object(&state.root, "greeter.password", ObjType::Text)?;
        let _ = state.doc.splice_text(&pobj, 0, 0, "hunter2");
    }

    widget.sync(&mut state.doc, &state.root);

    let child = widget.build_layout(&mut taffy, &theme, &mut node_map);

    // Second sync so the freshly-composed children pull from the doc.
    widget.sync(&mut state.doc, &state.root);

    let root = taffy.new_with_children(
        Style {
            size: Size {
                width: Dimension::length(w as f32),
                height: Dimension::length(h as f32),
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
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
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
