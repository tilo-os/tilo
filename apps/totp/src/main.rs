//! Authenticator — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default            — run the live winit event loop via `ui::App`.
//!   * `--shots PATH`     — render one offscreen frame to a PNG and exit
//!                           (headless screenshot; identical passes to the live app).
//!   * `--screen add`     — pair with `--shots` to capture the Add-account
//!                           screen instead of the list.
//!   * `--viewport WxH`   — pair with `--shots` to render at a specific size
//!                           (default 1280x800) so responsive widths can be
//!                           captured. Shots-only; the live window sizes itself.
//!
//! The `--shots` renderer uses a deterministic sample vault so the frame is
//! populated and reproducible. The live event loop never fabricates sample
//! accounts — it shows exactly what the user has saved.

use std::collections::HashMap;

use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_shots_arg(&args) {
        let widget = match parse_screen_arg(&args).as_deref() {
            Some("add") => totp::app_root_add_screen(),
            Some("add-advanced") => totp::app_root_add_advanced_screen(),
            Some("add-link") => totp::app_root_add_link_screen(),
            Some("add-picker") => totp::app_root_add_picker_screen(),
            Some("empty") => totp::app_root_empty(),
            _ => totp::app_root_demo(),
        };
        let viewport = parse_viewport_arg(&args).unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        render_shot_to_png(widget, viewport, &out)?;
        return Ok(());
    }

    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(spec, totp::app_root(), "dev.qos.totp"));
    }

    let (state, autosave) = ui::persist::open("dev.qos.totp");
    let mut app = ui::App::new(state, totp::app_root()).with_autosave(autosave);
    // Register the Authenticator's actions so the shell's Ctrl+K palette can
    // decode + replay the same account actions the on-screen affordances dispatch.
    totp::actions::register(app.action_registry_mut());
    app.run();
    Ok(())
}

/// Read the `--screen <name>` flag (e.g. `--screen add`), if present.
fn parse_screen_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--screen" {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix("--screen=") {
            return Some(rest.to_string());
        }
    }
    None
}

/// Read the `--viewport WxH` flag (e.g. `--viewport 560x720`), if present.
/// Shots-only: lets the offscreen renderer capture narrow / small widths.
fn parse_viewport_arg(args: &[String]) -> Option<(u32, u32)> {
    let mut iter = args.iter().skip(1);
    let raw = loop {
        let arg = iter.next()?;
        if arg == "--viewport" {
            break iter.next()?.clone();
        }
        if let Some(rest) = arg.strip_prefix("--viewport=") {
            break rest.to_string();
        }
    };
    let (w, h) = raw.split_once(['x', 'X', '*'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

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

/// Run one offscreen frame against `widget` and write RGBA to `out` as PNG.
/// Mirrors `ui::App`'s cycle: sync → build_layout → compute_layout → apply_layout → draw.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let theme = match std::env::var("QOS_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let mut state = ui::State::new();
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
    println!("totp: wrote shot to {}", out.display());
    Ok(())
}
