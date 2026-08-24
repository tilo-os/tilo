//! Photos — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default              — the live winit event loop via `ui::App`.
//!   * `--open PATH`        — start with that photograph open.
//!   * `--shots PATH`       — render one offscreen frame to a PNG and exit.
//!                            `--scenario NAME` picks the pose (see
//!                            [`photos::SHOT_SCENARIOS`]), `--size WxH` the
//!                            viewport, and `QOS_SHOTS_THEME=light|dark|
//!                            high-contrast` the palette.

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
        std::process::exit(ui::headless::run(spec, photos::app_root(), "dev.qos.photos"));
    }

    let (state, autosave) = ui::persist::open("dev.qos.photos");
    let mut app = photos::PhotosApp::new();
    photos::strings::install();

    // `--open PATH` is the same entry the file chooser uses, so a path from
    // the command line and one from the portal reach the document identically.
    let mut state = state;
    if let Some(path) = flag_value(&args, "--open") {
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
/// sync → build_layout → compute_layout → apply_layout → draw.
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
    let staging = out.parent().unwrap_or(std::path::Path::new(".")).join("photos-shots");
    let mut state = ui::State::new();
    let mut app = photos::PhotosApp::new();
    photos::seed_scenario(&mut app, &mut state.doc, scenario, &staging);

    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut widget: Box<dyn Widget> = Box::new(app);

    // Two passes: the first lays the canvas out so the viewer knows its rect,
    // the second renders with the framing that produced. A single pass would
    // draw the "cropping" scenario before its viewport existed.
    let mut scene = Scene::new();
    for _ in 0..2 {
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

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("photos: wrote {scenario} shot to {}", out.display());
    Ok(())
}
