// Thin binary entry point — all logic lives in the library.
//
// Supports three modes:
//   * default — build a `BrowserWidget`, hand it to `ui::App`, run the
//     live event loop (winit window).
//   * `--shots <PATH>` — render the chrome at a default size into a PNG
//     at `<PATH>` and exit. No window, no network: the embedded test
//     engine pre-renders a deterministic page so the snapshot includes
//     a populated web view, not just empty chrome.
//   * `--shots-live <PATH> [--url <URL>]` (needs the `wpe` feature) —
//     boot the real WPE WebKit engine headless, load `URL` over the
//     network, wait for the painted page, and render chrome + live page
//     into a PNG. End-to-end verification of the engine → frame →
//     widget → offscreen-render pipeline without a window.

use std::collections::HashMap;

use browser::BrowserWidget;
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

/// Default offscreen viewport for `--shots`. Wide enough to show the
/// toolbar row + a sizeable page area.
const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1200, 720);

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_arg_value(&args, "--shots-live") {
        let url = parse_arg_value(&args, "--url")
            .unwrap_or_else(|| "https://example.org".to_string());
        render_live_shot_to_png(&url, DEFAULT_SHOTS_VIEWPORT, std::path::Path::new(&out))?;
        return Ok(());
    }
    if let Some(out) = parse_shots_arg(&args) {
        let widget = BrowserWidget::new_headless();
        render_shot_to_png(Box::new(widget), DEFAULT_SHOTS_VIEWPORT, &out)?;
        return Ok(());
    }

    let widget = BrowserWidget::new();
    let engine = widget.engine_handle();
    let state = ui::State::new();
    let app = ui::App::new(state, Box::new(widget));
    // Wake the (otherwise event-driven) winit loop whenever the engine
    // publishes a frame or a navigation event, so pages animate and
    // finish loading without waiting for user input.
    app.run_with_setup(move |proxy| {
        use ui::webview::WebViewEngine;
        engine.set_waker(Box::new(move || {
            let _ = proxy.send_event(ui::app::UserEvent::External("webview-wake".into()));
        }));
    });
    Ok(())
}

/// Returns `Some(path)` if `--shots <path>` (or `--shots=<path>`) appears
/// in `args`, otherwise `None`.
fn parse_shots_arg(args: &[String]) -> Option<std::path::PathBuf> {
    parse_arg_value(args, "--shots").map(std::path::PathBuf::from)
}

/// Returns the value of `<flag> <value>` or `<flag>=<value>` in `args`.
fn parse_arg_value(args: &[String], flag: &str) -> Option<String> {
    let eq_prefix = format!("{flag}=");
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == flag {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix(&eq_prefix) {
            return Some(rest.to_string());
        }
    }
    None
}

/// `--shots-live`: real engine, real network. Boot WPE, navigate, wait
/// for the painted page at the web view's laid-out size, then render
/// one frame to PNG — the same five-pass cycle as `render_shot_to_png`,
/// with an engine-pumping wait between layout and draw.
#[cfg(feature = "wpe")]
fn render_live_shot_to_png(
    url: &str,
    viewport: (u32, u32),
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::{Duration, Instant};
    use ui::webview::{SharedEngine, WebViewCommand, WebViewEngine};

    // Dev environments (containers) can't nest WebKit's bubblewrap
    // sandbox; on qOS the cluster sandbox is the outer boundary anyway.
    unsafe { std::env::set_var("WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS", "1") };

    let engine: SharedEngine = ui::webview::wpe::WpeEngine::new()
        .map_err(|e| format!("WPE engine failed to boot: {e}"))?;
    // headless=true: no filter-loader thread, no auto-navigate — the
    // shot drives navigation itself.
    let mut widget = BrowserWidget::with_engine(engine.clone(), true);
    engine.send(WebViewCommand::Navigate(url.to_string()));

    let (w, h) = viewport;
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();

    let mut state = ui::State::new();
    let root_obj = state.root.clone();

    // Pump sync + layout until the engine has painted a frame at the
    // web view's laid-out (physical) size — the first layout pass sends
    // the Resize, subsequent frames arrive at the right dimensions.
    // Then let the page settle briefly so late-loading content paints.
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut sized_frame_seen_at: Option<Instant> = None;
    loop {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        widget.sync(&mut state.doc, &root_obj);
        let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
        widget.sync(&mut state.doc, &root_obj);
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

        let frame = engine.frames().lock().unwrap().clone();
        if let Some(f) = frame {
            let painted = f.rgba.chunks_exact(4).any(|px| px[3] != 0);
            // The engine's pre-resize default is 1024px wide; a frame
            // wider than that means the layout-driven Resize took hold.
            if painted && f.width > 1024 {
                let seen = *sized_frame_seen_at.get_or_insert_with(Instant::now);
                if seen.elapsed() > Duration::from_secs(3) {
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
                    println!("browser: wrote live shot of {url} to {}", out.display());
                    return Ok(());
                }
            }
        }
        if Instant::now() > deadline {
            return Err(format!("no sized painted frame from {url} within 45s").into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(not(feature = "wpe"))]
fn render_live_shot_to_png(
    _url: &str,
    _viewport: (u32, u32),
    _out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    Err("--shots-live needs the real engine: rebuild with --features wpe".into())
}

/// Render one offscreen frame against `widget` at `viewport` and write
/// the resulting RGBA to `out` as a PNG. Mirrors `ui::App`'s
/// `sync → build_layout → compute_layout → apply_layout → draw` cycle.
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
    // `BrowserWidget` composes fresh children inside `build_layout`, so
    // they missed the sync above and would draw blank (the address bar
    // would show its placeholder instead of the page URL). Re-sync
    // against the built tree — the same post-build re-sync `ui::App`
    // performs each frame.
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
    println!("browser: wrote shot to {}", out.display());
    Ok(())
}
