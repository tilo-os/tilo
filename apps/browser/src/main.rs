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
use taffy::prelude::*;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};

/// Default offscreen viewport for `--shots`. Wide enough to show the
/// toolbar row + a sizeable page area.
const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1200, 720);

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // The SHM engine path reads WebKit's frames back on the CPU, yet with a
    // real display WebKit still tries hardware GL for its compositor. On a
    // machine whose GPU EGL is unreliable (e.g. a nested NVIDIA node
    // reporting `driver (null)`) that context can wedge mid-render under
    // load — the page freezes and never recovers while the chrome stays
    // live. Pin WebKit's GL to software so the SHM path is reliable; the
    // zero-copy dmabuf path (opt-in) keeps hardware. Overridable by setting
    // the var yourself. wgpu (the chrome) uses Vulkan, so it's unaffected.
    #[cfg(feature = "wpe")]
    if std::env::var_os("QOS_WEBVIEW_DMABUF").is_none()
        && std::env::var_os("LIBGL_ALWAYS_SOFTWARE").is_none()
    {
        // Safe: called at the very top of `main`, before any thread (or the
        // WebKit WebProcess) that could read the environment concurrently.
        unsafe { std::env::set_var("LIBGL_ALWAYS_SOFTWARE", "1") };
    }

    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_arg_value(&args, "--shots-live") {
        let url =
            parse_arg_value(&args, "--url").unwrap_or_else(|| "https://example.org".to_string());
        // `--size WxH` renders at an arbitrary viewport — used to inspect
        // narrow / mobile widths headlessly.
        let size = parse_arg_value(&args, "--size")
            .and_then(|s| {
                let (w, h) = s.split_once('x')?;
                Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
            })
            .unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        render_live_shot_to_png(&url, size, std::path::Path::new(&out))?;
        return Ok(());
    }
    if let Some(out) = parse_arg_value(&args, "--shots-suggestions") {
        render_suggestions_shot(std::path::Path::new(&out))?;
        return Ok(());
    }
    if let Some(out) = parse_arg_value(&args, "--shots-overlay") {
        // Full chrome + the address suggestion overlay pinned into view,
        // to inspect its live position (under the bar) + frosted look.
        use browser::history::HistoryEntry;
        let mut widget = BrowserWidget::new_headless();
        widget.debug_force_suggestions(vec![
            HistoryEntry {
                url: "https://github.com/qos/qos".into(),
                title: "qOS on code.podesta.ai".into(),
                last_visit: 3,
                visit_count: 12,
            },
            HistoryEntry {
                url: "https://en.wikipedia.org/wiki/Git".into(),
                title: "Git - Wikipedia".into(),
                last_visit: 2,
                visit_count: 4,
            },
            HistoryEntry {
                url: "https://gitlab.com".into(),
                title: "GitLab".into(),
                last_visit: 1,
                visit_count: 2,
            },
        ]);
        render_shot_to_png(Box::new(widget), (1000, 650), std::path::Path::new(&out))?;
        return Ok(());
    }
    if let Some(out) = parse_arg_value(&args, "--shots-settings") {
        // Render the settings pane standalone, to inspect its layout.
        render_shot_to_png(
            Box::new(browser::settings::SettingsPane::new()),
            (900, 700),
            std::path::Path::new(&out),
        )?;
        return Ok(());
    }
    if let Some(out) = parse_shots_arg(&args) {
        let widget = BrowserWidget::new_headless();
        render_shot_to_png(Box::new(widget), DEFAULT_SHOTS_VIEWPORT, &out)?;
        return Ok(());
    }

    let widget = launch_target(&args)
        .map(|uri| BrowserWidget::new().with_initial_uri(uri))
        .unwrap_or_else(BrowserWidget::new);
    let engine = widget.engine_handle();

    // Zero-copy GPU compositing: when the WebView engine exports dmabuf
    // frames (QOS_WEBVIEW_DMABUF=1), install a render-thread hook that
    // blits the imported texture over the content rect. A no-op otherwise.
    #[cfg(feature = "wpe")]
    if std::env::var_os("QOS_WEBVIEW_DMABUF").is_some() {
        ui::render_thread::set_frame_post_processor(Box::new(
            browser::compositor::WebViewCompositor::new(
                engine.clone(),
                widget.content_rect_handle(),
            ),
        ));
    }

    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(spec, Box::new(widget), "dev.qos.browser"));
    }

    let (state, autosave) = ui::persist::open("dev.qos.browser");
    let app = ui::App::new(state, Box::new(widget)).with_autosave(autosave);
    // Wake the (otherwise event-driven) winit loop whenever the engine
    // publishes a frame or a navigation event, so pages animate and
    // finish loading without waiting for user input.
    app.run_with_setup(move |proxy| {
        engine.set_waker(Box::new(move || {
            let _ = proxy.send_event(ui::app::UserEvent::External("webview-wake".into()));
        }));
    });
    Ok(())
}

/// Render the address-suggestion overlay with sample entries, to inspect
/// its rows + selection highlight (it paints in the overlay pass, which
/// the normal shot path doesn't drive).
fn render_suggestions_shot(out: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    use browser::address_overlay::AddressSuggestions;
    use browser::history::HistoryEntry;
    let (w, h) = (900u32, 360u32);
    let theme = shot_theme();
    let presence = PresenceCtx::empty();
    let mut text_renderer = TextRenderer::new();

    let mut overlay = AddressSuggestions::new();
    overlay.set_suggestions(vec![
        HistoryEntry {
            url: "https://github.com/qos/qos".into(),
            title: "qOS — code.podesta.ai".into(),
            last_visit: 3,
            visit_count: 12,
        },
        HistoryEntry {
            url: "https://en.wikipedia.org/wiki/Web_browser".into(),
            title: "Web browser - Wikipedia".into(),
            last_visit: 2,
            visit_count: 5,
        },
        HistoryEntry {
            url: "https://duckduckgo.com/?q=rust".into(),
            title: "rust at DuckDuckGo".into(),
            last_visit: 1,
            visit_count: 3,
        },
    ]);
    overlay.move_selection(1); // highlight the 2nd row
    overlay.set_anchor(Point::new(24.0, 24.0), (w as f32) - 48.0);

    let mut scene = Scene::new();
    overlay.draw_overlay(
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
    renderer.render_png(&scene, theme.bg_app, out);
    println!("browser: wrote suggestions shot to {}", out.display());
    Ok(())
}

/// Returns `Some(path)` if `--shots <path>` (or `--shots=<path>`) appears
/// in `args`, otherwise `None`.
fn parse_shots_arg(args: &[String]) -> Option<std::path::PathBuf> {
    parse_arg_value(args, "--shots").map(std::path::PathBuf::from)
}

/// `QOS_SHOTS_THEME=light|high-contrast` renders the shot under that theme so
/// theme-driven chrome can be proven across palettes; defaults to dark.
fn shot_theme() -> Theme {
    match std::env::var("QOS_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    }
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

/// The resource dispatcher forwards a claimed URI as one opaque positional
/// argument. Browser deliberately accepts only the schemes it declares.
fn launch_target(args: &[String]) -> Option<String> {
    if let Some(uri) = args
        .iter()
        .skip(1)
        .find(|arg| {
            arg.split_once(':').is_some_and(|(scheme, rest)| {
                (scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http"))
                    && rest.starts_with("//")
            }) && arg.len() <= 8 * 1024
                && !arg.chars().any(char::is_control)
        })
        .cloned()
    {
        return Some(uri);
    }
    // File dispatch uses the launcher's existing single positional argument.
    // Convert only an absolute path; relative names and flag values never turn
    // into a WebView navigation by accident.
    let path = args
        .get(1)
        .map(std::path::Path::new)
        .filter(|path| path.is_absolute())?;
    url::Url::from_file_path(path).ok().map(Into::into)
}

#[cfg(test)]
mod resource_claim_tests {
    use super::launch_target;

    #[test]
    fn startup_accepts_only_declared_web_schemes_as_one_value() {
        let uri = "HTTPS://example.test/a --still-data";
        assert_eq!(
            launch_target(&["browser".into(), uri.into()]).as_deref(),
            Some(uri)
        );
        for value in ["javascript:alert(1)", "data:text/plain,x", "mailto:a@b"] {
            assert_eq!(launch_target(&["browser".into(), value.into()]), None);
        }
    }
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
    use ui::webview::{SharedEngine, WebViewCommand};

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
            // Wait for a painted frame at the requested viewport width
            // (edge-to-edge web view, scale 1.0 → frame width ≈ w), so
            // the layout-driven Resize has taken hold rather than the
            // engine's 1024×768 default. Works for any width, wide or
            // narrow.
            let sized = (f.width as i32 - w as i32).abs() <= 2;
            if painted && sized {
                let seen = *sized_frame_seen_at.get_or_insert_with(Instant::now);
                // Settle long enough that a UA-driven reload (the mobile/
                // desktop switch fires one on the first resize) completes
                // over the network before we capture.
                if seen.elapsed() > Duration::from_secs(6) {
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
    let theme = shot_theme();
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
    // Overlay pass (dropdowns, floating panels), as the live App runs it.
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
    // Clear to the app background (as the live App does) so a widget that
    // doesn't paint its own backdrop — e.g. the settings pane — shows its
    // theme-coloured text instead of vanishing on transparent.
    renderer.render_png(&scene, theme.bg_app, out);
    println!("browser: wrote shot to {}", out.display());
    Ok(())
}
