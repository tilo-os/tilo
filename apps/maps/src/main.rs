//! Maps — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!     (headless screenshot; identical passes to the live app).
//!     `--viewport WxH` sets the frame size and `--nav-closed`
//!     forces the overlay drawer shut — both affect only the
//!     screenshot path, never the live app. Scenario flags
//!     (`--pois`, `--search`, `--offline`, `--panel …`, …)
//!     stage a specific surface for the shot.

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
        reason = "a test that unwraps is stating an invariant"
    )
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

/// Parsed screenshot request.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, orthogonal --shots scenario flag (not a state \
              machine); a real CLI accepts them in any combination"
)]
struct ShotArgs {
    out: std::path::PathBuf,
    viewport: (u32, u32),
    nav_closed: bool,
    empty: bool,
    panel: Option<maps::Panel>,
    /// Select Transit travel mode inside Directions (`--panel transit`).
    transit_mode: bool,
    /// Demo the typeable "From" field mid-edit with its geocoded dropdown.
    edit_from: bool,
    /// Demo the Transit picker anchored to an "Arrive by" time.
    transit_arrive: bool,
    /// Demo the floating search-results overlay (proximity-ordered).
    search: bool,
    /// Demo the live POI marker layer with an open info card.
    pois: bool,
    /// Turn the real-time traffic overlay on (sample congestion, or the honest
    /// "No traffic data" state when combined with `--empty`).
    traffic: bool,
    /// Demo the offline-maps overlay with saved, manageable downloaded areas.
    offline: bool,
    /// Demo the ⋯ overflow menu expanded (attribution / offline / licenses).
    menu: bool,
}

/// # Errors
///
/// Returns `Err` if the screenshot renderer fails to write its output file.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // Install the rustls crypto provider before any HTTPS fetch. The tile worker
    // and the geocoding/routing client also call this, but doing it here at
    // startup keeps the provider set for the whole process regardless of which
    // request runs first. See `net::ensure_crypto_provider` for why it's needed.
    maps::net::ensure_crypto_provider();

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
        if shot.transit_mode {
            app.set_travel_mode(maps::TravelMode::Transit);
        }
        if shot.edit_from {
            app.set_demo_edit_origin();
        }
        if shot.transit_arrive {
            app.set_demo_transit_arrive();
        }
        if shot.search {
            app.set_demo_search();
        }
        if shot.pois {
            app.set_demo_pois();
        }
        if shot.traffic {
            app.set_demo_traffic();
        }
        if shot.offline {
            app.set_demo_offline();
        }
        if shot.menu {
            app.set_demo_overflow();
        }
        render_shot_to_png(Box::new(app), shot.viewport, &shot.out)?;
        return Ok(());
    }

    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(spec, maps::app_root(), "dev.qos.maps"));
    }

    let (state, autosave) = ui::persist::open("dev.qos.maps");
    let mut app = ui::App::new(state, maps::app_root()).with_autosave(autosave);
    // Register Maps' actions so the shell's Ctrl+K palette can decode + replay the
    // same saved-place / route / offline-region actions the on-screen affordances
    // dispatch.
    maps::actions::register(app.action_registry_mut());
    app.run();
    Ok(())
}

fn parse_shots(args: &[String]) -> Option<ShotArgs> {
    let mut out: Option<std::path::PathBuf> = None;
    let mut viewport = DEFAULT_SHOTS_VIEWPORT;
    let mut nav_closed = false;
    let mut empty = false;
    let mut panel: Option<maps::Panel> = None;
    let mut transit_mode = false;
    let mut edit_from = false;
    let mut transit_arrive = false;
    let mut search = false;
    let mut pois = false;
    let mut traffic = false;
    let mut offline = false;
    let mut menu = false;
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
        } else if arg == "--search" {
            search = true;
        } else if arg == "--pois" {
            pois = true;
        } else if arg == "--traffic" {
            traffic = true;
        } else if arg == "--offline" {
            offline = true;
        } else if arg == "--menu" {
            menu = true;
        } else if arg == "--panel" {
            if let Some(s) = iter.next() {
                apply_panel(s, &mut panel, &mut transit_mode);
            }
        } else if let Some(rest) = arg.strip_prefix("--panel=") {
            apply_panel(rest, &mut panel, &mut transit_mode);
        }
    }
    out.map(|out| ShotArgs {
        out,
        viewport,
        nav_closed,
        empty,
        panel,
        transit_mode,
        edit_from,
        transit_arrive,
        search,
        pois,
        traffic,
        offline,
        menu,
    })
}

/// Resolve a `--panel` value. `transit` is not a panel — it opens Directions in
/// Transit travel mode, mirroring how the live app models it.
fn apply_panel(s: &str, panel: &mut Option<maps::Panel>, transit_mode: &mut bool) {
    match s.trim().to_ascii_lowercase().as_str() {
        "explore" => *panel = Some(maps::Panel::Explore),
        "directions" => *panel = Some(maps::Panel::Directions),
        "transit" => {
            *panel = Some(maps::Panel::Directions);
            *transit_mode = true;
        }
        _ => {}
    }
}

/// Parse a `WxH` (or `WXH`) viewport string like `560x720`.
fn parse_viewport(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Run one offscreen frame against `widget` and write RGBA to `out` as PNG.
/// Mirrors `ui::App`'s cycle: sync → `build_layout` → `compute_layout` → `apply_layout` → draw.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    // `QOS_SHOTS_THEME=light|high-contrast` renders the shot under that theme
    // so the same frame can be graded in both light and dark (default dark).
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
    println!("maps: wrote shot to {}", out.display());
    Ok(())
}
