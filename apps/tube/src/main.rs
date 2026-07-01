//! Tube — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`. The root
//!                       app is `online()`: it spawns the fetch worker and pulls
//!                       real YouTube data (home/trending/search/…) via the
//!                       `live` backend, degrading to the bundled sample when
//!                       the network is unreachable.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!                       (headless screenshot; identical passes to the live app).
//!                       Always offline/hermetic — no worker, no network.
//!   * `--probe [Q]`  — construct the real backend, run a live YouTube search
//!                       for `Q` (default "rust programming") + trending, print
//!                       what came back, and exit. Proof that live fetch works;
//!                       requires `--features live` to do anything real.
//!
//! Shots-only flags:
//!   * `--viewport WxH` — render at WxH logical px instead of the default
//!                         1280x800. A sub-`NAV_BREAKPOINT` width exercises the
//!                         responsive overlay-flap layout.
//!   * `--narrow`       — build the narrow variant (rail collapsed to an open
//!                         overlay flap), so the still captures the summoned
//!                         rail + scrim. Use together with a narrow `--viewport`.

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

    // `--probe [query]` — prove the live backend actually fetches real data.
    if let Some(query) = parse_probe_arg(&args) {
        return run_probe(&query);
    }

    if let Some(out) = parse_shots_arg(&args) {
        let viewport = parse_viewport_arg(&args).unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        let narrow = args.iter().any(|a| a == "--narrow");
        let palette = args.iter().any(|a| a == "--palette");
        let root = if palette {
            // Open the command palette pre-seeded so the still captures the
            // Ctrl+K overlay with live results.
            let q = parse_palette_query_arg(&args).unwrap_or_else(|| "rust".to_string());
            Box::new(tube::TubeApp::new().with_palette_open_for_test(&q)) as Box<dyn Widget>
        } else if narrow {
            tube::app_root_narrow_shot()
        } else if let Some(route) = parse_route_arg(&args) {
            tube::app_root_route_shot(route)
        } else {
            tube::app_root()
        };
        render_shot_to_png(root, viewport, &out)?;
        return Ok(());
    }

    let state = ui::State::new();
    // The live binary is `online()`: it reaches out to the real backend and
    // auto-falls-back to the sample if the network is down. (`app_root()`, used
    // by `--shots`, stays offline for hermetic screenshots.)
    let app = ui::App::new(state, Box::new(tube::TubeApp::new().online()));
    app.run();
    Ok(())
}

/// Parse `--probe` / `--probe=<query>` / `--probe <query>`. Returns the query
/// to probe with (default "rust programming") when the flag is present.
fn parse_probe_arg(args: &[String]) -> Option<String> {
    const DEFAULT_Q: &str = "rust programming";
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--probe" {
            return Some(match iter.next() {
                Some(q) if !q.starts_with("--") => q.clone(),
                _ => DEFAULT_Q.to_string(),
            });
        }
        if let Some(rest) = arg.strip_prefix("--probe=") {
            return Some(if rest.is_empty() { DEFAULT_Q.to_string() } else { rest.to_string() });
        }
    }
    None
}

/// The real proof: construct the live backend, hit YouTube, print what came
/// back. Compiled to do real work only under `--features live`.
#[cfg(feature = "live")]
fn run_probe(query: &str) -> Result<(), Box<dyn std::error::Error>> {
    use tube::backend::{BackendResult, YtBackend};

    eprintln!("probe: constructing the live rustypipe backend…");
    let backend = tube::backend::live::RustyPipeBackend::new()
        .map_err(|e| format!("live backend construction failed: {e}"))?;
    println!("probe: backend = {}", backend.name());

    println!("probe: searching YouTube for {query:?} …");
    let (first_id, first_channel) = match backend.search(query) {
        Ok(BackendResult::Videos(vs)) => {
            if vs.is_empty() {
                eprintln!("probe: EMPTY result set — network reached YouTube but no videos parsed.");
                std::process::exit(2);
            }
            println!("probe: {} REAL results from YouTube:", vs.len());
            for (i, v) in vs.iter().take(15).enumerate() {
                println!(
                    "  {:>2}. [{}] {}  — {} ({} views)",
                    i + 1,
                    v.id,
                    v.title,
                    v.channel_name,
                    v.views
                );
            }
            let f = &vs[0];
            (f.id.clone(), f.channel_id.clone())
        }
        Ok(other) => {
            println!("probe: unexpected result variant: {other:?}");
            return Ok(());
        }
        Err(e) => {
            eprintln!("probe: LIVE SEARCH FAILED (network/parse): {e}");
            std::process::exit(2);
        }
    };

    // Video metadata (watch page data path).
    println!("\nprobe: video_details [{first_id}] …");
    match backend.video(&first_id) {
        Ok(BackendResult::Video(v)) => println!(
            "  title={:?} channel={:?} views={} likes={} desc_len={}",
            v.title,
            v.channel_name,
            v.views,
            v.likes,
            v.description.len()
        ),
        Ok(_) => {}
        Err(e) => eprintln!("  video_details failed: {e}"),
    }

    // Real "up next" recommendations (watch-page sidebar data path).
    println!("probe: related [{first_id}] …");
    match backend.related(&first_id) {
        Ok(BackendResult::Videos(vs)) => println!(
            "  {} recommendations; first: {:?}",
            vs.len(),
            vs.first().map(|v| v.title.as_str()).unwrap_or("<none>")
        ),
        Ok(_) => {}
        Err(e) => eprintln!("  related failed: {e}"),
    }

    // Channel page + uploads (channel screen data path).
    println!("probe: channel_videos [{first_channel}] …");
    match backend.channel(&first_channel) {
        Ok(BackendResult::Channel { channel, uploads }) => println!(
            "  {:?} ({} subs, verified={}); {} uploads; first: {:?}",
            channel.name,
            channel.subscribers,
            channel.verified,
            uploads.len(),
            uploads.first().map(|v| v.title.as_str()).unwrap_or("<none>")
        ),
        Ok(_) => {}
        Err(e) => eprintln!("  channel_videos failed: {e}"),
    }

    // Trending exercises the browse feed (the Home/Trending data path).
    println!("probe: trending …");
    match backend.trending() {
        Ok(BackendResult::Videos(vs)) => println!(
            "  {} videos; first: {:?}",
            vs.len(),
            vs.first().map(|v| v.title.as_str()).unwrap_or("<none>")
        ),
        Ok(_) => {}
        Err(e) => eprintln!("  trending failed: {e}"),
    }
    Ok(())
}

/// Non-live builds have no real backend compiled in.
#[cfg(not(feature = "live"))]
fn run_probe(_query: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "probe: this binary was built WITHOUT the `live` feature, so no real \
         backend is compiled in. Rebuild with `--features live` to probe real \
         YouTube data."
    );
    Ok(())
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

/// Parse `--route NAME` (or `--route=NAME`) into a starting [`tube::model::Route`]
/// for a screen-specific shot. Recognises `home`, `trending`, `subs`/
/// `subscriptions`, `history`, `settings`, and `search:<query>`.
fn parse_route_arg(args: &[String]) -> Option<tube::model::Route> {
    use tube::model::Route;
    let mut iter = args.iter().skip(1);
    let spec = loop {
        let arg = iter.next()?;
        if arg == "--route" {
            break iter.next()?.to_string();
        }
        if let Some(rest) = arg.strip_prefix("--route=") {
            break rest.to_string();
        }
    };
    let lower = spec.to_ascii_lowercase();
    Some(match lower.as_str() {
        "home" => Route::Home,
        "trending" => Route::Trending,
        "subs" | "subscriptions" => Route::Subscriptions,
        "history" => Route::History,
        "settings" => Route::Settings,
        _ => {
            if let Some(q) = spec.strip_prefix("search:") {
                Route::Search(q.to_string())
            } else {
                Route::Search(spec)
            }
        }
    })
}

/// Parse `--palette-query <q>` (or `--palette-query=<q>`) — the query to seed
/// the command-palette shot with.
fn parse_palette_query_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--palette-query" {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix("--palette-query=") {
            return Some(rest.to_string());
        }
    }
    None
}

/// Parse `--viewport WxH` (or `--viewport=WxH`) into a (width, height) pair.
fn parse_viewport_arg(args: &[String]) -> Option<(u32, u32)> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        let spec = if arg == "--viewport" {
            iter.next().map(String::as_str)
        } else {
            arg.strip_prefix("--viewport=")
        };
        if let Some(spec) = spec {
            let (w, h) = spec.split_once(['x', 'X'])?;
            return Some((w.trim().parse().ok()?, h.trim().parse().ok()?));
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
    println!("tube: wrote shot to {}", out.display());
    Ok(())
}
