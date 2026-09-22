// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
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
//     a populated web view.
//   * `--shots-live <PATH> [--url <URL>]` (needs the `wpe` feature) —
//     boot the WPE WebKit engine headless, load `URL` over the
//     network, wait for the painted page, and render chrome + live page
//     into a PNG, covering the engine → frame → widget → offscreen-render
//     pipeline without a window.
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

#[cfg(feature = "thin")]
extern crate runtime_dylib;

use std::collections::HashMap;

use browser::BrowserWidget;
use taffy::prelude::*;
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
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

/// # Errors
///
/// Returns an error if a `--shots*` render mode fails to produce its PNG
/// (missing/unreadable target path, offscreen render failure).
pub fn main() {
    // The SHM engine path reads WebKit's frames back on the CPU, yet with a
    // real display WebKit still tries hardware GL for its compositor. On a
    // machine whose GPU EGL is unreliable (e.g. a nested NVIDIA node
    // reporting `driver (null)`) that context can wedge mid-render under
    // load — the page freezes and never recovers while the chrome stays
    // live. Pin WebKit's GL to software so the SHM path is reliable; the
    // zero-copy dmabuf path (opt-in) keeps hardware. Overridable by setting
    // the var yourself. wgpu (the chrome) uses Vulkan, so it's unaffected.
    #[cfg(feature = "wpe")]
    if std::env::var_os("TILO_WEBVIEW_DMABUF").is_none()
        && std::env::var_os("LIBGL_ALWAYS_SOFTWARE").is_none()
    {
        // Safe: called at the very top of `main`, before any thread (or the
        // WebKit WebProcess) that could read the environment concurrently.
        unsafe { std::env::set_var("LIBGL_ALWAYS_SOFTWARE", "1") };
    }

    ui::launch::run_or_exit(
        &LaunchSpec::new_with_args("so.tilo.browser", browser_root)
            .shot_handler(render_shot)
            .allowed_live_options(&[
                "--shots-live",
                "--shots-suggestions",
                "--shots-overlay",
                "--shots-pip-empty",
                "--shots-settings",
                "--shots-history-paused",
                "--shots-loading",
                "--shots-error",
                "--shots-library",
                "--library-section",
                "--library-filter",
                "--download-state",
                "--audit-state",
                "--url",
                "--size",
            ])
            .allowed_flags(&["--probe-pip", "--pip-active"])
            .max_positionals(1)
            .command_handler(command)
            .live_handler(run_live),
    );
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by LaunchSpec's argument-aware root factory"
)]
fn browser_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(Box::new(browser_widget(args)))
}

fn browser_widget(args: &LaunchArgs) -> BrowserWidget {
    let mut widget = launch_target(args.positionals()).map_or_else(browser::app_root, |uri| {
        BrowserWidget::new().with_initial_uri(uri)
    });
    match args.value("--audit-state") {
        Some("error") => widget.debug_show_load_error(),
        Some("history") => widget.debug_show_library("history"),
        Some("bookmarks") => widget.debug_show_library("bookmarks"),
        Some("downloads") => widget.debug_show_library("downloads"),
        Some("extensions") => widget.debug_show_library("extensions"),
        Some("settings") => widget.debug_show_settings(),
        Some("history-paused") => widget.debug_show_history_paused(),
        _ => {}
    }
    widget
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by LaunchSpec::live_handler"
)]
fn run_live(args: &LaunchArgs) -> Result<(), LaunchError> {
    let wake_scope = ui::wake::WakeScope::new();
    let _wake_entry = wake_scope.enter();
    let widget = browser_widget(args);
    let engine = widget.engine_handle();

    // Zero-copy GPU compositing: when the WebView engine exports dmabuf
    // frames (TILO_WEBVIEW_DMABUF=1), install a render-thread hook that
    // blits the imported texture over the content rect. A no-op otherwise.
    #[cfg(feature = "wpe")]
    if std::env::var_os("TILO_WEBVIEW_DMABUF").is_some() {
        ui::render_thread::set_frame_post_processor(Box::new(
            browser::compositor::WebViewCompositor::new(
                engine.clone(),
                widget.content_rect_handle(),
            ),
        ));
    }

    let (state, autosave) = ui::persist::open("so.tilo.browser");
    let mut app =
        ui::App::from_scoped_parts(wake_scope, state, Box::new(widget)).with_autosave(autosave);
    browser::actions::register(app.action_registry_mut());
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

#[allow(
    clippy::too_many_lines,
    reason = "the deterministic screenshot command routing is kept together"
)]
fn command(args: &LaunchArgs) -> Result<bool, LaunchError> {
    if let Some(out) = args.value("--shots-live") {
        let url = args.value("--url").unwrap_or("https://example.org");
        let size = args
            .value("--size")
            .and_then(parse_size)
            .unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        render_live_shot_to_png(
            url,
            size,
            std::path::Path::new(out),
            args.has("--probe-pip"),
            args.text_scale,
        )
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
        return Ok(true);
    }
    if let Some(out) = args.value("--shots-suggestions") {
        render_suggestions_shot(std::path::Path::new(out), args.text_scale);
        return Ok(true);
    }
    if let Some(out) = args.value("--shots-overlay") {
        render_overlay_shot(std::path::Path::new(out), args.text_scale)
            .map_err(|error| LaunchError::Operation(error.to_string()))?;
        return Ok(true);
    }
    if let Some(out) = args.value("--shots-pip-empty") {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_show_empty_pip_feedback();
        let size = args
            .value("--size")
            .and_then(parse_size)
            .unwrap_or((1000, 650));
        widget.debug_set_viewport_width(size.0);
        render_shot_to_png(
            Box::new(widget),
            size,
            std::path::Path::new(out),
            args.text_scale,
        )
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
        return Ok(true);
    }
    if let Some(out) = args.value("--shots-settings") {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_show_settings();
        let size = args
            .value("--size")
            .and_then(parse_size)
            .unwrap_or((900, 700));
        widget.debug_set_viewport_width(size.0);
        render_shot_to_png(
            Box::new(widget),
            size,
            std::path::Path::new(out),
            args.text_scale,
        )
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
        return Ok(true);
    }
    if let Some(out) = args.value("--shots-history-paused") {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_show_history_paused();
        let size = args
            .value("--size")
            .and_then(parse_size)
            .unwrap_or((900, 700));
        widget.debug_set_viewport_width(size.0);
        render_shot_to_png(
            Box::new(widget),
            size,
            std::path::Path::new(out),
            args.text_scale,
        )
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
        return Ok(true);
    }
    if let Some(out) = args.value("--shots-loading") {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_show_loading();
        let size = args
            .value("--size")
            .and_then(parse_size)
            .unwrap_or((900, 620));
        widget.debug_set_viewport_width(size.0);
        render_shot_to_png(
            Box::new(widget),
            size,
            std::path::Path::new(out),
            args.text_scale,
        )
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
        return Ok(true);
    }
    if let Some(out) = args.value("--shots-library") {
        let mut widget = BrowserWidget::new_headless();
        let section = args.value("--library-section").unwrap_or("history");
        widget.debug_show_library(section);
        if let Some(query) = args.value("--library-filter") {
            widget.debug_set_library_filter(query);
        }
        if section == "downloads" {
            widget.debug_show_downloads(args.value("--download-state").unwrap_or("progress"));
        }
        let size = args
            .value("--size")
            .and_then(parse_size)
            .unwrap_or((1000, 700));
        widget.debug_set_viewport_width(size.0);
        render_shot_to_png(
            Box::new(widget),
            size,
            std::path::Path::new(out),
            args.text_scale,
        )
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
        return Ok(true);
    }
    if let Some(out) = args.value("--shots-error") {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_show_load_error();
        let size = args
            .value("--size")
            .and_then(parse_size)
            .unwrap_or((900, 620));
        widget.debug_set_viewport_width(size.0);
        render_shot_to_png(
            Box::new(widget),
            size,
            std::path::Path::new(out),
            args.text_scale,
        )
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
        return Ok(true);
    }
    Ok(false)
}

fn render_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args.shots.as_deref().ok_or(LaunchError::ShotsUnsupported)?;
    let mut widget = BrowserWidget::new_headless();
    if args.has("--pip-active") {
        widget.debug_show_active_pip();
    }
    let viewport = args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT);
    widget.debug_set_viewport_width(viewport.0);
    render_shot_to_png(Box::new(widget), viewport, out, args.text_scale)
        .map_err(|error| LaunchError::Operation(error.to_string()))
}

fn parse_size(value: &str) -> Option<(u32, u32)> {
    let (width, height) = value.split_once(['x', 'X'])?;
    Some((width.trim().parse().ok()?, height.trim().parse().ok()?))
}

fn render_overlay_shot(
    out: &std::path::Path,
    text_scale: Option<f32>,
) -> Result<(), Box<dyn std::error::Error>> {
    use browser::history::HistoryEntry;
    let mut widget = BrowserWidget::new_headless();
    widget.debug_force_suggestions(vec![
        HistoryEntry {
            url: "https://github.com/qos/qos".into(),
            title: "Tilo on code.podesta.ai".into(),
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
    render_shot_to_png(Box::new(widget), (1000, 650), out, text_scale)
}

/// Render the address-suggestion overlay with sample entries, to inspect
/// its rows + selection highlight (it paints in the overlay pass, which
/// the normal shot path doesn't drive).
fn render_suggestions_shot(out: &std::path::Path, text_scale: Option<f32>) {
    use browser::address_overlay::AddressSuggestions;
    use browser::history::HistoryEntry;
    let (w, h) = (900u32, 360u32);
    let theme = shot_theme(text_scale);
    let presence = PresenceCtx::empty();
    let mut text_renderer = TextRenderer::new();
    text_renderer.set_text_scale(theme.text_scale);

    let mut overlay = AddressSuggestions::new();
    overlay.set_suggestions(vec![
        HistoryEntry {
            url: "https://github.com/qos/qos".into(),
            title: "Tilo — code.podesta.ai".into(),
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
}

/// `TILO_SHOTS_THEME=light|high-contrast` renders the shot under that theme so
/// theme-driven chrome can be proven across palettes; defaults to dark.
/// `TILO_SHOTS_REDUCE_MOTION=1` drives a deterministic review override; the
/// live accessibility preferences are honoured too.
fn shot_theme(text_scale: Option<f32>) -> Theme {
    let theme = match std::env::var("TILO_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
    // `--text-scale` is a common launch option.  Browser owns its custom
    // screenshot handlers, so it must thread that value through explicitly
    // rather than silently capturing normal-sized text for an a11y review.
    let theme = match text_scale {
        Some(scale) => theme.with_text_scale(scale),
        None => theme,
    };
    theme.with_reduce_motion(shot_motion_reduced())
}

fn shot_motion_reduced() -> bool {
    [
        "TILO_SHOTS_REDUCE_MOTION",
        "TILO_A11Y_REDUCE_MOTION",
        "TILO_REDUCE_MOTION",
    ]
    .into_iter()
    .filter_map(|key| std::env::var(key).ok())
    .any(|value| {
        let value = value.trim();
        !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
    })
}

/// The resource dispatcher forwards a claimed URI as one opaque positional
/// argument. Browser deliberately accepts only the schemes it declares.
fn launch_target(args: &[String]) -> Option<String> {
    args.iter()
        .find(|arg| {
            arg.split_once(':').is_some_and(|(scheme, rest)| {
                (scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http"))
                    && rest.starts_with("//")
            }) && arg.len() <= 8 * 1024
                && !arg.chars().any(char::is_control)
        })
        .cloned()
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
    probe_pip: bool,
    text_scale: Option<f32>,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::{Duration, Instant};
    use ui::webview::{SharedEngine, WebViewCommand};

    // Dev environments (containers) can't nest WebKit's bubblewrap
    // sandbox; on Tilo the cluster sandbox is the outer boundary anyway.
    unsafe { std::env::set_var("WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS", "1") };

    let engine: SharedEngine = ui::webview::wpe::WpeEngine::new()
        .map_err(|e| format!("WPE engine failed to boot: {e}"))?;
    // headless=true: no filter-loader thread, no auto-navigate — the
    // shot drives navigation itself.
    let mut widget = BrowserWidget::with_engine(engine.clone(), true);
    engine.send(WebViewCommand::Navigate(url.to_string()));

    let (w, h) = viewport;
    let theme = shot_theme(text_scale);
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    text_renderer.set_text_scale(theme.text_scale);

    let mut state = ui::State::new();
    let root_obj = state.root.clone();

    // Pump sync + layout until the engine has painted a frame at the
    // web view's laid-out (physical) size — the first layout pass sends
    // the Resize, subsequent frames arrive at the right dimensions.
    // Then let the page settle briefly so late-loading content paints.
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut sized_frame_seen_at: Option<Instant> = None;
    let mut pip_probe_started = false;
    let mut pip_request = None;
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

        if pip_probe_started && pip_request.is_none() {
            pip_request =
                ui::widget::FloatingSurfaceWidgetExt::take_floating_surface_request(&mut widget);
        }

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
                    if probe_pip && !pip_probe_started {
                        widget.debug_request_picture_in_picture();
                        pip_probe_started = true;
                        continue;
                    }
                    if probe_pip && pip_request.is_none() {
                        continue;
                    }
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
                    renderer.render_png(&scene, vello::peniko::Color::TRANSPARENT, out);
                    if let Some(request) = pip_request.as_ref() {
                        let ui::floating_surface::FloatingSource::AppElement {
                            element_id,
                            region,
                            ..
                        } = &request.source
                        else {
                            return Err(
                                "real browser PiP did not preserve a semantic media element".into(),
                            );
                        };
                        if element_id.trim().is_empty() || region.area() <= 1.0 {
                            return Err("real browser PiP returned empty semantic geometry".into());
                        }
                        let protocol: ui::floating_surface::FloatingSurfaceProtocolRequest =
                            request.into();
                        let sidecar =
                            std::path::PathBuf::from(format!("{}.pip.json", out.display()));
                        std::fs::write(&sidecar, serde_json::to_vec_pretty(&protocol)?)?;
                        println!(
                            "browser: live PiP selected {} at {:.0}×{:.0}; wrote {}",
                            request.title,
                            region.width(),
                            region.height(),
                            sidecar.display()
                        );
                    }
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
    _probe_pip: bool,
    _text_scale: Option<f32>,
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
    text_scale: Option<f32>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let theme = shot_theme(text_scale);
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    text_renderer.set_text_scale(theme.text_scale);
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

#[cfg(test)]
mod resource_claim_tests {
    use super::launch_target;

    #[test]
    fn startup_accepts_only_declared_web_schemes_as_one_value() {
        let uri = "HTTPS://example.test/a --still-data";
        assert_eq!(launch_target(&[uri.into()]).as_deref(), Some(uri));
        for value in [
            "javascript:alert(1)",
            "data:text/plain,x",
            "mailto:a@b",
            "file:///tmp/portal-bypass.html",
            "/tmp/portal-bypass.html",
        ] {
            assert_eq!(launch_target(&[value.into()]), None);
        }
    }
}
