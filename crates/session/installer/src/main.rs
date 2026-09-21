// Thin binary entry point for the Tilo graphical installer — all logic
// lives in the library (mirrors greeter/src/main.rs).
//
// Modes:
//   * default — build an `InstallerApp`, hand it to `ui::App`, run the
//     live winit event loop. Launched off the live media under `cage`:
//         cage -s -- /usr/bin/tilo-installer
//   * `--shots <PATH> [--step <name>] [--viewport <WIDTH>x<HEIGHT>]` — render one screen into a PNG at
//     `<PATH>` and exit. No window, no event loop. Headless snapshot for
//     CI / visual inspection (output identical to the live binary's first
//     frame). `<name>` ∈ {network, disk, account, keyboard, options, options-open, profile-locale-repair,
//     review, install, finish, finish-recovery, failed, failed-recovery} (with `user`/`confirm` accepted as
//     legacy aliases); `options-open` renders the Options step with the
//     time-zone dropdown popped open; `keyboard` renders the keyboard step
//     seeded with a non-default (German) layout so the visual preview shows
//     the remapped keys. Defaults to `disk`.

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
use std::sync::{Arc, Mutex};

use installer::InstallerApp;
use installer::installer_app::{ProxySlot, Step};
use taffy::prelude::*;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

/// Default offscreen viewport for `--shots`.
const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

/// # Errors
///
/// Returns an error when `--shots` is passed and rendering the requested
/// step fails: seeding the offscreen CRDT document, computing the taffy
/// layout, or writing the PNG to `--shots <path>` (see
/// [`render_shot_to_png`]).
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _detached = ui::wake::WakeScope::enter_detached();
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_shots_arg(&args) {
        let name = parse_step_arg(&args).unwrap_or_else(|| "disk".to_string());
        let viewport = parse_viewport_arg(&args).unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        let text_scale = parse_text_scale_arg(&args);
        let Some((widget, seed)) = build_shot(&name) else {
            eprintln!("tilo-installer: unknown --step '{name}'");
            std::process::exit(2);
        };
        render_shot_to_png(widget, viewport, text_scale, &out, seed)?;
        return Ok(());
    }

    let wake_scope = ui::wake::WakeScope::new();
    let _wake_entry = wake_scope.enter();
    // Shared slot the event loop fills with its proxy once running; the
    // install worker thread reads it to stream progress back in.
    let proxy_slot: ProxySlot = Arc::new(Mutex::new(None));
    let widget = InstallerApp::new().with_proxy(proxy_slot.clone());
    let state = ui::State::new();
    let mut app = ui::App::from_scoped_parts(wake_scope, state, Box::new(widget));
    // Hand the keyboard to the first field/row at startup so the wizard
    // is usable without a click...
    app.autofocus_first = true;
    // ...and re-focus the first field of each step after the per-step
    // rebuild (opt-in; leaves shell/greeter behaviour untouched).
    app.autofocus_on_structure_change = true;
    let slot = proxy_slot;
    app.run_with_setup(move |proxy| {
        if let Ok(mut g) = slot.lock() {
            *g = Some(proxy);
        }
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

/// Build the widget for a `--shots --step <name>` render, plus whether to
/// seed sample credentials (so input fields show representative text).
/// Returns `None` for an unrecognised name.
fn build_shot(name: &str) -> Option<(Box<dyn Widget>, bool)> {
    if name == "disk-alongside" {
        let mut app = InstallerApp::for_shot(Step::Disk);
        app.shot_alongside();
        return Some((Box::new(app), false));
    }
    if name == "review-alongside" {
        let mut app = InstallerApp::for_shot(Step::Review);
        app.shot_alongside_review();
        return Some((Box::new(app), true));
    }
    // `options-open` renders the Options step with the time-zone dropdown
    // popped open, capturing the overlay popup state.
    if name == "options-open" {
        let mut app = InstallerApp::for_shot(Step::Options);
        app.shot_open_timezone();
        return Some((Box::new(app), true));
    }
    if name == "profile-locale-repair" {
        let mut app = InstallerApp::for_shot(Step::Options);
        app.shot_locale_repair();
        return Some((Box::new(app), true));
    }

    // `keyboard` seeds a non-default layout so the visual preview clearly
    // shows the remapped keys (y/z swap + ü ö ä for German QWERTZ).
    if name == "keyboard" {
        let mut app = InstallerApp::for_shot(Step::Keyboard);
        app.shot_keymap("German (QWERTZ)");
        return Some((Box::new(app), true));
    }

    if name == "finish-recovery" {
        let mut app = InstallerApp::for_shot(Step::Install);
        app.shot_finished_with_recovery();
        return Some((Box::new(app), true));
    }
    if name == "finish-recovery-saved" {
        let mut app = InstallerApp::for_shot(Step::Install);
        app.shot_finished_with_saved_recovery();
        return Some((Box::new(app), true));
    }
    if name == "failed-recovery" {
        let mut app = InstallerApp::for_shot(Step::Install);
        app.shot_failed_with_recovery();
        return Some((Box::new(app), true));
    }

    let (step, terminal): (Step, Option<bool>) = match name {
        "network" => (Step::Network, None),
        "disk" => (Step::Disk, None),
        "account" | "user" => (Step::Account, None),
        "keyboard-default" => (Step::Keyboard, None),
        "options" => (Step::Options, None),
        "review" | "confirm" => (Step::Review, None),
        "install" => (Step::Install, None),
        "finish" | "done" => (Step::Install, Some(true)),
        "failed" | "fail" => (Step::Install, Some(false)),
        _ => return None,
    };
    let mut app = InstallerApp::for_shot(step);
    match terminal {
        Some(true) => app.shot_finished(),
        Some(false) => app.shot_failed(),
        None => {}
    }
    let seed = !matches!(step, Step::Disk);
    Some((Box::new(app), seed))
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

/// Returns the raw `--step <name>` (or `--step=<name>`) value for a shot,
/// if present. Validation/mapping happens in [`build_shot`].
fn parse_step_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter().skip(1);
    loop {
        let arg = iter.next()?;
        if arg == "--step" {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix("--step=") {
            return Some(rest.to_string());
        }
    }
}

/// Run one offscreen frame against `widget` at `viewport` and write the
/// resulting RGBA to `out` as a PNG. Mirrors `ui::App`'s
/// `sync → build_layout → compute_layout → apply_layout → draw` cycle.
///
/// `seed_creds` pre-populates the username/password CRDT objects so the
/// User/Confirm/Install screens render representative text. Because the
/// composed children are only created *inside* `build_layout`, we sync a
/// second time afterwards so those children (the `TextInputs`) pull their
/// values from the document — the live App achieves the same via its
/// per-frame sync.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    text_scale: Option<f32>,
    out: &std::path::Path,
    seed_creds: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use automerge::{ObjType, transaction::Transactable};

    let (w, h) = viewport;
    let theme = text_scale.map_or_else(Theme::dark, |scale| Theme::dark().with_text_scale(scale));
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let mut state = ui::State::new();

    if seed_creds {
        let uobj = state
            .doc
            .put_object(&state.root, "installer.username", ObjType::Text)?;
        let _ = state.doc.splice_text(&uobj, 0, 0, "ada");
    }

    widget.sync(&mut state.doc, &state.root);

    let child = widget.build_layout(&mut taffy, &theme, &mut node_map);

    // Second sync so the freshly-composed children pull from the doc.
    widget.sync(&mut state.doc, &state.root);

    let root = taffy.new_with_children(
        Style {
            size: Size {
                width: Dimension::length(ui::num::f32_of_u32(w)),
                height: Dimension::length(ui::num::f32_of_u32(h)),
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
                width: AvailableSpace::Definite(ui::num::f32_of_u32(w)),
                height: AvailableSpace::Definite(ui::num::f32_of_u32(h)),
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
    // Top-layer overlay pass (matches `ui::App`): paints an open
    // `Dropdown` list above the rest of the frame instead of letting
    // later widgets occlude it.
    widget.draw_overlay(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
    );

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("tilo-installer: wrote shot to {}", out.display());
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
            parse_viewport_arg(&args(&["tilo-installer", "--viewport", "640x480"])),
            Some((640, 480))
        );
        assert_eq!(
            parse_viewport_arg(&args(&["tilo-installer", "--viewport=360X640"])),
            Some((360, 640))
        );
    }

    #[test]
    fn viewport_rejects_malformed_or_empty_dimensions() {
        for raw in ["", "640", "x480", "640x", "0x480", "640x0", "-1x480"] {
            assert_eq!(
                parse_viewport_arg(&args(&["tilo-installer", "--viewport", raw])),
                None,
                "accepted malformed viewport {raw:?}"
            );
        }
    }

    #[test]
    fn shot_text_scale_accepts_only_bounded_finite_values() {
        assert_eq!(
            parse_text_scale_arg(&args(&["tilo-installer", "--text-scale=1.3"])),
            Some(1.3)
        );
        for value in ["NaN", "0.5", "2.1", "not-a-number"] {
            assert_eq!(
                parse_text_scale_arg(&args(&["tilo-installer", "--text-scale", value])),
                None
            );
        }
    }

    #[test]
    fn recovery_visual_states_are_renderable() {
        for scenario in [
            "finish-recovery",
            "finish-recovery-saved",
            "failed-recovery",
        ] {
            assert!(build_shot(scenario).is_some(), "missing {scenario}");
        }
    }

    #[test]
    fn transported_profile_repair_state_is_renderable() {
        assert!(build_shot("profile-locale-repair").is_some());
    }
}
