//! Maps — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!                       (headless screenshot; identical passes to the live app).
//!                       `--viewport WxH` sets the frame size and `--nav-closed`
//!                       forces the overlay drawer shut — both affect only the
//!                       screenshot path, never the live app.

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

/// Parsed screenshot request.
struct ShotArgs {
    out: std::path::PathBuf,
    viewport: (u32, u32),
    nav_closed: bool,
    empty: bool,
    panel: Option<maps::Panel>,
    /// Demo the typeable "From" field mid-edit with its geocoded dropdown.
    edit_from: bool,
    /// Demo the Transit picker anchored to an "Arrive by" time.
    transit_arrive: bool,
}

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(shot) = parse_shots(&args) {
        // Headless: no event loop and no network — use an offline app so the
        // tile worker is never spawned and nothing blocks on I/O. The default
        // offline app renders the bundled sample city; `--empty` mirrors the
        // live first-run state (empty model, plain basemap).
        let mut app = if shot.empty {
            maps::MapsApp::offline_empty()
        } else {
            maps::MapsApp::offline()
        };
        if shot.nav_closed {
            app.set_nav_open(false);
        }
        if let Some(panel) = shot.panel {
            app.set_active_panel(panel);
        }
        if shot.edit_from {
            app.set_demo_edit_origin();
        }
        if shot.transit_arrive {
            app.set_demo_transit_arrive();
        }
        render_shot_to_png(Box::new(app), shot.viewport, &shot.out)?;
        return Ok(());
    }

    let state = ui::State::new();
    let app = ui::App::new(state, maps::app_root());
    app.run();
    Ok(())
}

fn parse_shots(args: &[String]) -> Option<ShotArgs> {
    let mut out: Option<std::path::PathBuf> = None;
    let mut viewport = DEFAULT_SHOTS_VIEWPORT;
    let mut nav_closed = false;
    let mut empty = false;
    let mut panel: Option<maps::Panel> = None;
    let mut edit_from = false;
    let mut transit_arrive = false;
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--shots" {
            out = iter.next().map(std::path::PathBuf::from);
        } else if let Some(rest) = arg.strip_prefix("--shots=") {
            out = Some(std::path::PathBuf::from(rest));
        } else if arg == "--viewport" {
            if let Some(v) = iter.next().and_then(|s| parse_viewport(s)) {
                viewport = v;
            }
        } else if let Some(rest) = arg.strip_prefix("--viewport=") {
            if let Some(v) = parse_viewport(rest) {
                viewport = v;
            }
        } else if arg == "--nav-closed" {
            nav_closed = true;
        } else if arg == "--empty" {
            empty = true;
        } else if arg == "--edit-from" {
            edit_from = true;
        } else if arg == "--transit-arrive" {
            transit_arrive = true;
        } else if arg == "--panel" {
            panel = iter.next().and_then(|s| parse_panel(s));
        } else if let Some(rest) = arg.strip_prefix("--panel=") {
            panel = parse_panel(rest);
        }
    }
    out.map(|out| ShotArgs {
        out,
        viewport,
        nav_closed,
        empty,
        panel,
        edit_from,
        transit_arrive,
    })
}

/// Parse a `--panel` value into a [`maps::Panel`].
fn parse_panel(s: &str) -> Option<maps::Panel> {
    match s.trim().to_ascii_lowercase().as_str() {
        "explore" => Some(maps::Panel::Explore),
        "directions" => Some(maps::Panel::Directions),
        "transit" => Some(maps::Panel::Transit),
        _ => None,
    }
}

/// Parse a `WxH` (or `WXH`) viewport string like `560x720`.
fn parse_viewport(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Run one offscreen frame against `widget` and write RGBA to `out` as PNG.
/// Mirrors `ui::App`'s cycle: sync → build_layout → compute_layout → apply_layout → draw.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let theme = Theme::dark();
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
    println!("maps: wrote shot to {}", out.display());
    Ok(())
}
