//! Photos — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default              — the live winit event loop via `ui::App`.
//!   * `--open PATH`        — start with that photograph open.
//!   * `--shots PATH`       — render one offscreen frame to a PNG and exit.
//!     `--scenario NAME` picks the pose (see [`photos::SHOT_SCENARIOS`]),
//!     `--size WxH` the viewport, and `QOS_SHOTS_THEME=light|dark|
//!     high-contrast` the palette.

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

use std::collections::HashMap;

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

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let args: Vec<String> = std::env::args().collect();

    if let Some(out) = flag_value(&args, "--shots") {
        let scenario = flag_value(&args, "--scenario").unwrap_or_else(|| "opened".into());
        let viewport = flag_value(&args, "--size")
            .and_then(|s| parse_size(&s))
            .unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        return render_shot(&scenario, viewport, std::path::Path::new(&out));
    }

    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(
            spec,
            photos::app_root(),
            "dev.qos.photos",
        ));
    }

    let (state, autosave) = ui::persist::open("dev.qos.photos");
    let mut app = photos::PhotosApp::new();
    photos::strings::install();

    // The photograph this was started on, however it was named. Same entry as
    // the file chooser's, so a path from a command line, from the portal, and
    // from a sandboxed launch all reach the document identically.
    let mut state = state;
    if let Some(path) = document_argument(&args) {
        app.open_path(&mut state.doc, &path);
    }

    #[allow(unused_mut)]
    let mut app = ui::App::new(state, Box::new(app)).with_autosave(autosave);
    photos::actions::register(app.action_registry_mut());
    // Debug overlays are a development aid; a shipped build is always clean
    // regardless of an inherited QUICK_UI_DEBUG.
    #[cfg(not(debug_assertions))]
    {
        app.debug_mode = false;
    }
    app.run();
    Ok(())
}

/// The document this was asked to open, in either spelling it arrives in.
///
/// `--open PATH` is what a person types and what the portal hands over. A
/// **bare first argument** is what a sandboxed launch delivers: `qos-launcher`
/// forwards the document as the app's `argv[1..]`, because after the
/// entrypoint token everything is opaque bytes to bwrap and there is nowhere
/// for a flag name to survive.
///
/// Reading only the flag was a silent failure with no error anywhere in it.
/// The session staged the picture, granted the app read on it, launched the
/// app with it and logged all three; the app started, ignored its own
/// argument, and came up empty — so a request to crop that photograph reached
/// a window that did not have it, which from outside looks exactly like an
/// agent that did nothing.
fn document_argument(args: &[String]) -> Option<String> {
    if let Some(path) = flag_value(args, "--open") {
        return Some(path);
    }
    // The first token that is not a flag and not a flag's value. A GUI launch
    // carries exactly one argument and nothing else, so this is that argument
    // — the walk is what keeps `--size 1280x800` from being mistaken for a
    // filename on the paths that do carry flags.
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if let Some(name) = arg.strip_prefix("--") {
            if !name.contains('=') {
                // `--flag value`: step over the value too.
                iter.next();
            }
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(arg.clone());
    }
    None
}

/// Value of `--flag VALUE` or `--flag=VALUE`.
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    let mut iter = args.iter().skip(1);
    let eq = format!("{flag}=");
    while let Some(arg) = iter.next() {
        if arg == flag {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix(&eq) {
            return Some(rest.to_string());
        }
    }
    None
}

/// Parse `WxH`.
fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X', '*'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Render one scenario offscreen. Mirrors `ui::App`'s frame exactly:
/// sync → `build_layout` → `compute_layout` → `apply_layout` → draw.
fn render_shot(
    scenario: &str,
    viewport: (u32, u32),
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let theme = match std::env::var("QOS_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
    photos::strings::install();

    // The generated pattern is written beside the output, so a shot run
    // leaves no file anywhere the live app would ever look.
    let staging = out
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("photos-shots");
    let mut state = ui::State::new();
    let mut app = photos::PhotosApp::new();
    photos::seed_scenario(&mut app, &mut state.doc, scenario, &staging);

    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    // Kept concrete rather than boxed as `dyn Widget`: the `cropping`
    // scenario has to zoom the viewer between the two passes, and the viewer
    // only exists once the first pass has synced it into being.
    let mut widget = app;

    // Two passes: the first lays the canvas out so the viewer knows its rect,
    // the second renders with the framing that produced. A single pass would
    // draw the "cropping" scenario before its viewport existed.
    let mut scene = Scene::new();
    for pass in 0..2 {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

        widget.sync(&mut state.doc, &state.root);
        let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
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

        // The viewer now has a rect, so a zoom means something. Framing the
        // crop before the second pass is what makes the `cropping` shot show
        // a photograph being framed rather than merely turned.
        if pass == 0 && scenario == "cropping" {
            widget.set_zoom(photos::CROPPING_SCENARIO_ZOOM);
        }

        scene = Scene::new();
        widget.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut text_renderer,
            &theme,
            &presence,
            &permission,
        );
    }

    // The top layer, exactly as `App` runs it: anything that floats above the
    // ordinary tree paints here so nothing drawn later can occlude it.
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
    println!("photos: wrote {scenario} shot to {}", out.display());
    Ok(())
}
