#![allow(
    clippy::field_reassign_with_default,
    clippy::redundant_field_names,
    clippy::too_many_lines,
    reason = "migration stack lint debt is scoped to this target"
)]
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
//   * `--shots <PATH> [--scenario <NAME>]` — render the chrome at a default
//     size into a PNG at `<PATH>` and exit. No window, no network: the
//     embedded test engine pre-renders a deterministic page so the
//     snapshot includes a populated web view, not just empty chrome. A
//     scenario stages one state (settings, library sections, an open
//     suggestion list, a failed load).
//   * `--shots-live <PATH> [--url <URL>]` —
//     boot the real Chromium engine headless, load `URL` over the
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

mod live_probe;

#[cfg(feature = "thin")]
extern crate runtime_dylib;

use browser::browsing_mode::BrowsingMode;
use browser::{BrowserWidget, BrowserWindow};
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec, ShotFrame};
use ui::widget::Widget;

/// Default offscreen viewport for `--shots`. Wide enough to show the
/// toolbar row + a sizeable page area.
const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1200, 720);

/// # Errors
///
/// Returns an error if a `--shots*` render mode fails to produce its PNG
/// (missing/unreadable target path, offscreen render failure).
pub fn main() {
    // The live handler installs the engine's frame waker. The framework
    // shell never wakes the window, so a page would sit until some other
    // event.
    ui::launch::run_or_exit(&launch_spec());
}

fn launch_spec() -> LaunchSpec {
    LaunchSpec::new_with_args("so.tilo.browser", browser_root)
        .runtime_attachment(|args| !launch_browsing_mode(args).is_private())
        .scenarios(SCENARIOS)
        .shot_handler(render_shot)
        .open_state_with_args(open_browser_state)
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
            "--app-root",
        ])
        .allowed_flags(&["--probe-pip"])
        .allowed_live_flags(&["--private"])
        .max_positionals(1)
        .command_handler(command)
        .live_handler(run_live)
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by LaunchSpec's argument-aware root factory"
)]
fn browser_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(Box::new(browser_window(args)?))
}

#[derive(Clone, Debug)]
struct LaunchPolicy {
    mode: BrowsingMode,
    root: Option<std::path::PathBuf>,
}

static LAUNCH_POLICY: std::sync::OnceLock<Result<LaunchPolicy, String>> =
    std::sync::OnceLock::new();

fn requested_browsing_mode(
    explicit_private: bool,
    bundle_fallback: Option<&std::ffi::OsStr>,
) -> BrowsingMode {
    if explicit_private || bundle_fallback.is_some_and(|value| value == "1") {
        BrowsingMode::Private
    } else {
        BrowsingMode::Normal
    }
}

fn launch_browsing_mode(args: &LaunchArgs) -> BrowsingMode {
    requested_browsing_mode(
        args.has("--private"),
        std::env::var_os("TILO_BROWSER_PRIVATE").as_deref(),
    )
}

fn prepare_launch(args: &LaunchArgs) -> Result<&'static LaunchPolicy, String> {
    LAUNCH_POLICY
        .get_or_init(|| {
            let mode = launch_browsing_mode(args);
            if mode.is_private() {
                let root = browser::storage::allocate_private_root()?;
                let root_text = root
                    .to_str()
                    .ok_or_else(|| "Private browser root is not valid UTF-8".to_string())?;
                if let Err(error) = browser::storage::configure(Some(root_text)) {
                    if browser::storage::is_owned_private_root(&root) {
                        let _ = std::fs::remove_dir_all(&root);
                    }
                    return Err(error);
                }
                Ok(LaunchPolicy {
                    mode,
                    root: Some(root),
                })
            } else {
                browser::storage::configure(args.value("--app-root"))?;
                Ok(LaunchPolicy {
                    mode,
                    root: browser::storage::explicit_root().map(std::path::Path::to_owned),
                })
            }
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// One window, one page. A launch without a page to show starts in the
/// address field.
fn browser_window(args: &LaunchArgs) -> Result<BrowserWindow, LaunchError> {
    let policy = prepare_launch(args).map_err(LaunchError::Operation)?;
    if let Some(root) = policy.root.as_deref()
        && policy.mode.is_private()
    {
        tracing::info!(private_root = %root.display(), "Private browser root allocated");
    }
    let focus_address = launch_target(args.positionals()).is_none();
    Ok(BrowserWindow::new(
        browser_widget(args, policy.mode),
        focus_address,
    ))
}

fn open_browser_state(id: &'static str, args: &LaunchArgs) -> (ui::State, ui::persist::DocWriter) {
    let policy = match prepare_launch(args) {
        Ok(policy) => policy,
        Err(error) => return (ui::State::new(), ui::persist::DocWriter::disabled(error)),
    };
    if policy.mode.is_private() {
        return (
            ui::State::new(),
            ui::persist::DocWriter::disabled("Private window state is memory-only"),
        );
    }
    if let Some(root) = browser::storage::explicit_root() {
        let opened = ui::persist::DocStore::open_canonical_at(id, &root.join("document"));
        return match opened {
            Ok(mut store) => {
                let mut doc = store.load_or_new();
                if let Some(error) = store.refusal() {
                    return (
                        ui::State::from_doc(doc),
                        ui::persist::DocWriter::disabled(error.to_string()),
                    );
                }
                let mut autosave = store.writer();
                autosave.seed(&mut doc);
                (ui::State::from_doc(doc), autosave)
            }
            Err(error) => (
                ui::State::new(),
                ui::persist::DocWriter::disabled(error.to_string()),
            ),
        };
    }
    ui::persist::open(id)
}

fn browser_widget(args: &LaunchArgs, mode: BrowsingMode) -> BrowserWidget {
    let mut widget = launch_target(args.positionals()).map_or_else(
        || BrowserWidget::new_with_mode(mode),
        |uri| BrowserWidget::new_with_mode(mode).with_initial_uri(uri),
    );
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
    let mode = prepare_launch(args).map_err(LaunchError::Operation)?.mode;
    let mut engine = None;
    let mut make_root = || -> Result<Box<dyn Widget>, LaunchError> {
        let widget = browser_window(args)?;
        engine = Some(widget.engine_handle());
        Ok(Box::new(widget))
    };
    let mut app = if mode.is_private() {
        // Private browsing keeps its explicit local-only construction path.
        let wake_scope = ui::wake::WakeScope::new();
        let _entry = wake_scope.enter();
        let root = make_root()?;
        let (state, autosave) = open_browser_state("so.tilo.browser", args);
        ui::App::from_scoped_parts_local(wake_scope, state, root).with_autosave(autosave)
    } else {
        launch_spec()
            .prepare_native(args)?
            .build_root(&mut make_root)?
    };
    browser::actions::register(app.action_registry_mut());
    let engine = engine.ok_or_else(|| {
        LaunchError::Operation("browser factory did not retain its engine".into())
    })?;
    // Wake the (otherwise event-driven) winit loop whenever the engine
    // publishes a frame or a navigation event, so pages animate and
    // finish loading without waiting for user input.
    app.run_with_setup(move |proxy| {
        engine.set_waker(Box::new(move || {
            let _ = proxy.send_event(ui::app::UserEvent::External("webview-wake".into()));
        }));
    });
    // Every Browser tab of this profile browses through one Chromium host.
    // When it is this process's, the window closing must not take the
    // other tabs' pages down with it.
    ui::webview::chromium::serve_shared_until_idle();
    Ok(())
}

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
            args,
        )
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
        return Ok(true);
    }
    if let Some(out) = args.value("--shots-suggestions") {
        render_suggestions_shot(std::path::Path::new(out), args)
            .map_err(|error| LaunchError::Operation(error.to_string()))?;
        return Ok(true);
    }
    if let Some(out) = args.value("--shots-overlay") {
        render_overlay_shot(std::path::Path::new(out), args)
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
        render_shot_to_png(Box::new(widget), size, std::path::Path::new(out), args)
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
        render_shot_to_png(Box::new(widget), size, std::path::Path::new(out), args)
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
        render_shot_to_png(Box::new(widget), size, std::path::Path::new(out), args)
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
        render_shot_to_png(Box::new(widget), size, std::path::Path::new(out), args)
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
        render_shot_to_png(Box::new(widget), size, std::path::Path::new(out), args)
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
        render_shot_to_png(Box::new(widget), size, std::path::Path::new(out), args)
            .map_err(|error| LaunchError::Operation(error.to_string()))?;
        return Ok(true);
    }
    Ok(false)
}

/// The states `--shots --scenario` can stage.
const SCENARIOS: &[&str] = &[
    "overlay",
    "settings",
    "history",
    "bookmarks",
    "extensions",
    "downloads",
    "downloads-completed",
    "downloads-failed",
    "downloads-empty",
    "history-paused",
    "loading",
    "error",
    "pip-empty",
    "pip-active",
    "menu",
    "site-protection",
    "site-information",
    "passwords",
    "addresses",
    "find",
    "permission",
    "download-prompt",
    "private",
    "private-settings",
];

/// What a `--shots` capture renders for `--scenario`.
enum Staged {
    /// A widget that is drawn as it is.
    Widget(Box<dyn Widget>),
    /// The browser with its overflow menu opened by a press on the control,
    /// as a person opens it.
    OverflowMenu(Box<BrowserWidget>),
}

/// The widget a `--shots` capture renders for `--scenario`. Without one it is
/// the browser on its default page.
fn scenario_widget(scenario: Option<&str>, viewport: (u32, u32)) -> Result<Staged, LaunchError> {
    let mut widget = BrowserWidget::new_capture(
        if scenario.is_some_and(|name| name.starts_with("private")) {
            BrowsingMode::Private
        } else {
            BrowsingMode::Normal
        },
    );
    widget.debug_set_viewport_width(viewport.0);
    let mut menu = false;
    match scenario {
        None | Some("private") => {}
        Some("overlay") => {
            widget.debug_type_address("git");
            widget.debug_force_suggestions(suggestion_fixture());
        }
        Some("settings" | "private-settings") => widget.debug_show_settings(),
        Some("history") => widget.debug_show_library("history"),
        Some("bookmarks") => widget.debug_show_library("bookmarks"),
        Some("extensions") => widget.debug_show_library("extensions"),
        Some("downloads") => {
            widget.debug_show_library("downloads");
            widget.debug_show_downloads("progress");
        }
        Some("downloads-completed") => {
            widget.debug_show_library("downloads");
            widget.debug_show_downloads("completed");
        }
        Some("downloads-failed") => {
            widget.debug_show_library("downloads");
            widget.debug_show_downloads("error");
        }
        Some("downloads-empty") => {
            widget.debug_show_library("downloads");
            widget.debug_show_downloads("empty");
        }
        Some("history-paused") => widget.debug_show_history_paused(),
        Some("loading") => widget.debug_show_loading(),
        Some("error") => widget.debug_show_load_error(),
        Some("pip-empty") => widget.debug_show_empty_pip_feedback(),
        Some("pip-active") => {
            widget.debug_show_active_pip();
            menu = true;
        }
        Some("menu") => menu = true,
        Some("site-protection") => widget.debug_show_site_panel(false),
        Some("site-information") => widget.debug_show_site_panel(true),
        Some("passwords") => widget.debug_show_autofill(false),
        Some("addresses") => widget.debug_show_autofill(true),
        Some("find") => widget.debug_show_find(),
        Some("permission") => widget.debug_request_permission(),
        Some("download-prompt") => widget.debug_request_download(),
        Some(other) => {
            return Err(LaunchError::Scenario(format!("unknown scenario {other:?}")));
        }
    }
    Ok(if menu {
        Staged::OverflowMenu(Box::new(widget))
    } else {
        Staged::Widget(Box::new(widget))
    })
}

/// What the address bar offers while someone types `git`.
fn suggestion_fixture() -> Vec<browser::history::HistoryEntry> {
    use browser::history::HistoryEntry;
    vec![
        HistoryEntry {
            url: "https://code.tilo.so/tilo/tilo".into(),
            title: "tilo/tilo: Tilo source code".into(),
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
    ]
}

fn render_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args.shots.as_deref().ok_or(LaunchError::ShotsUnsupported)?;
    let viewport = args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT);
    let staged = scenario_widget(args.scenario.as_deref(), viewport)?;
    match staged {
        Staged::Widget(widget) => render_shot_to_png(widget, viewport, out, args),
        Staged::OverflowMenu(widget) => render_menu_shot(*widget, viewport, out, args),
    }
    .map_err(|error| LaunchError::Operation(error.to_string()))
}

fn parse_size(value: &str) -> Option<(u32, u32)> {
    let (width, height) = value.split_once(['x', 'X'])?;
    Some((width.trim().parse().ok()?, height.trim().parse().ok()?))
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

/// `--shots-live`: real engine, real network. Boot Chromium, navigate, wait
/// for the painted page at the web view's laid-out size, then render
/// one frame to PNG — the same five-pass cycle as `render_shot_to_png`,
/// with an engine-pumping wait between layout and draw.
fn render_live_shot_to_png(
    url: &str,
    viewport: (u32, u32),
    out: &std::path::Path,
    probe_pip: bool,
    args: &LaunchArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    use ui::time::{Duration, Instant};
    use ui::webview::{SharedEngine, WebViewCommand};

    let real_engine: SharedEngine = ui::webview::chromium::ChromiumEngine::new()
        .map_err(|e| format!("Chromium engine failed to boot: {e}"))?;
    // The same page drawn from the paint stream, as a web client sees it.
    let real_engine: SharedEngine = if std::env::var("TILO_BROWSER_PAINT").as_deref() == Ok("1") {
        ui::webview::paint::PaintEngine::new(real_engine)
    } else {
        real_engine
    };
    let probe = live_probe::LiveProbe::new(real_engine);
    let engine: SharedEngine = probe.clone();
    // Keep capture chrome/session data isolated while using real maintained
    // protection and the same WebViewEngine event/render pipeline as the app.
    let mut widget = BrowserWidget::with_engine(engine.clone(), true);
    browser::blocker::spawn_filter_loader(engine.clone());
    engine.send(WebViewCommand::SetUserScripts(std::sync::Arc::new(
        browser::blocker::user_scripts(),
    )));
    engine.send(WebViewCommand::Navigate(url.to_string()));

    let (w, _) = viewport;
    let mut frame =
        ShotFrame::new(viewport, ui::launch::shot_theme(args)).with_resync_after_build();

    // Pump sync + layout until the engine has painted a frame at the
    // web view's laid-out (physical) size — the first layout pass sends
    // the Resize, subsequent frames arrive at the right dimensions.
    // Then let the page settle briefly so late-loading content paints.
    let deadline = ui::time::now() + Duration::from_secs(45);
    let mut sized_frame_seen_at: Option<Instant> = None;
    let mut pip_probe_started = false;
    let mut pip_request = None;
    loop {
        frame.sync(&mut widget);
        frame.layout(&mut widget)?;

        if pip_probe_started && pip_request.is_none() {
            pip_request =
                ui::widget::FloatingSurfaceWidgetExt::take_floating_surface_request(&mut widget);
        }

        let page = probe.state();
        if let Some(error) = &page.error {
            return Err(format!("{}: {error}", page.uri).into());
        }
        let painted_frame = engine
            .frames()
            .lock()
            .map_err(|_| "Capture frame store is unavailable")?
            .clone();
        if let Some(f) = painted_frame {
            let painted = f
                .rgba
                .chunks_exact(4)
                .any(|px| px.last().is_some_and(|alpha| *alpha != 0));
            // Wait for a painted frame at the requested viewport width
            // (edge-to-edge web view, scale 1.0 → frame width ≈ w), so
            // the layout-driven Resize has taken hold rather than the
            // engine's 1024×768 default. Works for any width, wide or
            // narrow.
            let sized = (f.width as i32 - w as i32).abs() <= 2;
            if painted && sized && page.loaded && page.uri.starts_with("http") {
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
                    let mut scene = frame.draw(&mut widget);
                    frame.draw_overlay(&mut widget, &mut scene);
                    return write_live_capture(&frame, &scene, out, pip_request.as_ref(), &page);
                }
            }
        }
        if ui::time::now() > deadline {
            return Err(format!("no sized painted frame from {url} within 45s").into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The suggestion overlay in its real Browser window, using the same fixture
/// and draw/overlay passes as the scenario gallery.
fn render_suggestions_shot(
    out: &std::path::Path,
    args: &LaunchArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    render_overlay_shot(out, args)
}

fn render_overlay_shot(
    out: &std::path::Path,
    args: &LaunchArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let viewport = args
        .value("--size")
        .and_then(parse_size)
        .unwrap_or((1000, 650));
    let Staged::Widget(widget) = scenario_widget(Some("overlay"), viewport)? else {
        return Err("the suggestion scenario did not produce a Browser window".into());
    };
    render_shot_to_png(widget, viewport, out, args)
}

/// Render one offscreen frame against `widget` at `viewport` and write
/// the resulting RGBA to `out` as a PNG.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &std::path::Path,
    args: &LaunchArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let theme = ui::launch::shot_theme(args);
    // `BrowserWidget` composes fresh children inside `build_layout`, so they
    // would miss the first sync and draw blank (the address bar would show
    // its placeholder instead of the page URL); re-sync against the built
    // tree, as `ui::App` does each frame.
    let mut frame = ShotFrame::new(viewport, theme).with_resync_after_build();
    frame.sync(widget.as_mut());
    frame.layout(widget.as_mut())?;
    // The window's own size is known to the widget only after the first
    // layout; a live window lays out again with it, as the capture does.
    frame.sync(widget.as_mut());
    frame.layout(widget.as_mut())?;
    let mut scene = frame.draw(widget.as_mut());
    // Overlay pass (dropdowns, floating panels), as the live App runs it.
    frame.draw_overlay(widget.as_mut(), &mut scene);
    frame.write_png(&scene, out);
    println!("browser: wrote shot to {}", out.display());
    Ok(())
}

/// Render the browser with its overflow menu open. The menu opens the way it
/// does for a person: a press on the control after the first layout. The
/// frame is drawn as it stands, without another layout, which would rebuild
/// the toolbar and close the menu.
fn render_menu_shot(
    mut widget: BrowserWidget,
    viewport: (u32, u32),
    out: &std::path::Path,
    args: &LaunchArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    use winit::event::{DeviceId, ElementState, Modifiers, MouseButton, WindowEvent};

    let mut frame =
        ShotFrame::new(viewport, ui::launch::shot_theme(args)).with_resync_after_build();
    frame.sync(&mut widget);
    frame.layout(&mut widget)?;
    frame.sync(&mut widget);
    frame.layout(&mut widget)?;
    let (x, y) = widget
        .debug_overflow_center()
        .ok_or("the toolbar has no overflow control")?;
    let press = [
        WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(x, y),
        },
        WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
        WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        },
    ];
    let (state, taffy, node) = frame
        .layout_parts()
        .ok_or("the frame has no layout to press in")?;
    let root = state.root.clone();
    for event in &press {
        let mut handled = false;
        let mut focus = None;
        widget.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: &mut state.doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy,
            },
        );
    }
    let mut scene = frame.draw(&mut widget);
    frame.draw_overlay(&mut widget, &mut scene);
    frame.write_png(&scene, out);
    println!("browser: wrote shot to {}", out.display());
    Ok(())
}

fn write_live_capture(
    frame: &ShotFrame,
    scene: &vello::Scene,
    out: &std::path::Path,
    pip_request: Option<&ui::floating_surface::FloatingSurfaceRequest>,
    page: &live_probe::PageState,
) -> Result<(), Box<dyn std::error::Error>> {
    frame.write_png(scene, out);
    if let Some(request) = pip_request {
        let ui::floating_surface::FloatingSource::AppElement {
            element_id, region, ..
        } = &request.source
        else {
            return Err("real browser PiP did not preserve a semantic media element".into());
        };
        if element_id.trim().is_empty() || region.area() <= 1.0 {
            return Err("real browser PiP returned empty semantic geometry".into());
        }
        let protocol: ui::floating_surface::FloatingSurfaceProtocolRequest = request.into();
        let sidecar = std::path::PathBuf::from(format!("{}.pip.json", out.display()));
        std::fs::write(&sidecar, serde_json::to_vec_pretty(&protocol)?)?;
        println!(
            "browser: live PiP selected {} at {:.0}x{:.0}; wrote {}",
            request.title,
            region.width(),
            region.height(),
            sidecar.display()
        );
    }
    std::fs::write(
        format!("{}.page.json", out.display()),
        serde_json::to_vec_pretty(page)?,
    )?;
    println!(
        "browser: wrote live Chromium shot of {} to {}",
        page.uri,
        out.display()
    );
    Ok(())
}

#[cfg(test)]
mod resource_claim_tests {
    use super::{launch_target, requested_browsing_mode};
    use browser::browsing_mode::BrowsingMode;

    /// Startup resource claims.
    #[test]
    fn startup_claims_private_flag_and_one_web_scheme_value() {
        // Phase `explicit_private_flag_wins_and_environment_one_is_bundle_fallback`.
        {
            assert_eq!(
                requested_browsing_mode(true, Some(std::ffi::OsStr::new("0"))),
                BrowsingMode::Private
            );
            assert_eq!(
                requested_browsing_mode(false, Some(std::ffi::OsStr::new("1"))),
                BrowsingMode::Private
            );
            for fallback in [
                None,
                Some(std::ffi::OsStr::new("0")),
                Some(std::ffi::OsStr::new("true")),
            ] {
                assert_eq!(
                    requested_browsing_mode(false, fallback),
                    BrowsingMode::Normal
                );
            }
        }

        // Phase `startup_accepts_only_declared_web_schemes_as_one_value`.
        {
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
}
