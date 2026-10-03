//! Code — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//! * default — run the live winit event loop via `ui::App`.
//! * `--shots PATH` — render one offscreen frame to a PNG and exit (headless
//!   screenshot; identical passes to the live app).

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
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
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec, ShotFrame};
use ui::theme::Theme;
use ui::widget::Widget;

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

/// Every `--scenario` name `--shots` knows how to stage. Keeping the list here
/// rather than only in the `match` arms is what lets an unknown name be an
/// error instead of a silently ordinary screenshot.
const SCENARIOS: &[&str] = &[
    "empty",
    "terminal",
    "selection",
    "completion",
    "problems",
    "find",
    "conflict",
    "indexing",
    "close-prompt",
    "status",
];

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn wants_standalone(args: impl Iterator<Item = String>) -> bool {
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" | "--path" => {
                let _ = args.next();
                if arg == "--path" {
                    return true;
                }
            }
            "--shots"
            | "--responsive-matrix"
            | "--native-audit"
            | "--headless"
            | "--sidebar-open" => return true,
            flag if flag.starts_with('-') => {}
            // A positional path opens a project on the standalone widget path.
            _ => return true,
        }
    }
    false
}

/// # Errors
///
/// Returns an error when `--shots` fails to lay out or render the offscreen
/// frame (see [`render_scenario_to_png`]); an unknown `--scenario` name exits
/// the process directly rather than returning an error.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !wants_standalone(std::env::args().skip(1)) {
        ui::framework::run::<code_editor::CodeEditorShell>();
        return Ok(());
    }
    ui::launch::run(
        &LaunchSpec::new("so.tilo.code-editor", code_editor::app_root)
            .live_root_with_args(live_root)
            .shot_handler(render_shot)
            .allowed_flags(&["--sidebar-open"])
            .allowed_live_options(&["--path"])
            .max_positionals(1)
            .scenarios(SCENARIOS)
            .configure_app(configure_app),
    )?;
    Ok(())
}

fn requested_path(args: &LaunchArgs) -> Option<std::path::PathBuf> {
    args.value("--path")
        .map(std::path::PathBuf::from)
        .or_else(|| args.positionals().first().map(std::path::PathBuf::from))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch::LaunchSpec::live_root_with_args"
)]
fn live_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    let path = requested_path(args);
    Ok(code_editor::app_root_at(path.as_deref()))
}

fn configure_app(app: &mut ui::App) {
    code_editor::actions::register(app.action_registry_mut());
}

fn render_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args.shots.as_deref().ok_or_else(|| {
        LaunchError::Operation("code editor screenshot handler called without --shots".into())
    })?;
    let viewport = args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT);
    // The `empty` scenario is the editor as it opens with no project at all.
    let mut app = if args.scenario.as_deref() == Some("empty") {
        code_editor::CodeApp::new()
    } else {
        requested_path(args).map_or_else(code_editor::CodeApp::with_sample, |path| {
            code_editor::CodeApp::open_path(&path)
        })
    };
    app.set_width_hint(f64::from(viewport.0));
    if args.has("--sidebar-open") {
        app.shots_snap_sidebar(true);
    }
    render_scenario_to_png(
        app,
        viewport,
        ui::launch::shot_theme(args),
        out,
        args.scenario.as_deref(),
    )
    .map_err(|error| LaunchError::Operation(error.to_string()))
}

/// Render one offscreen frame of the `CodeApp`, optionally staging a scenario
/// first. Mirrors `ui::App`'s cycle (sync, layout, draw, then the top layer);
/// for the `terminal` scenario it also drives the live PTY and pumps `sync` a
/// few times so real shell output lands in the frame.
///
/// # Errors
/// Returns an error if the offscreen layout cannot be solved.
fn render_scenario_to_png(
    mut app: code_editor::CodeApp,
    viewport: (u32, u32),
    theme: Theme,
    out: &std::path::Path,
    scenario: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut frame = ShotFrame::new(viewport, theme);

    // Pre-sync staging: opening the terminal must happen before the first sync
    // so its grid is initialised and pumped from the start.
    if scenario == Some("terminal") {
        app.shots_open_terminal();
    }
    // Scenarios that need a buffer open before the first sync so its text is
    // seeded and the editor is bound. On a real project this opens a real file
    // from the tree; on the demo tree `main.rs` is already open.
    if matches!(
        scenario,
        Some("find" | "conflict" | "indexing" | "close-prompt")
    ) {
        if let Some(path) = std::env::var_os("TILO_SHOTS_OPEN") {
            app.shots_open_path(std::path::Path::new(&path));
        } else if app.project_root().is_some() {
            app.shots_open_first_file();
        }
    }

    frame.sync(&mut app);
    frame.layout(&mut app)?;

    stage_scenario(&mut app, scenario, &mut frame);

    // The staged scenarios change the layout (the find bar appears), so the
    // tree has to be rebuilt before the final draw or the new surfaces get
    // last frame's geometry.
    if matches!(scenario, Some("find" | "problems")) {
        frame.layout(&mut app)?;
    }

    let mut scene = frame.draw(&mut app);
    frame.draw_overlay(&mut app, &mut scene);
    frame.write_png(&scene, out);
    println!("code_editor: wrote shot to {}", out.display());
    Ok(())
}

/// Stage the requested `--scenario` after the first layout pass: select
/// words, open the find bar, drive the live terminal's PTY, and so on. A
/// `None` scenario (the default cold-start frame) stages nothing.
fn stage_scenario(app: &mut code_editor::CodeApp, scenario: Option<&str>, frame: &mut ShotFrame) {
    match scenario {
        Some("terminal") => {
            // One draw settles the terminal's cell metrics + placement, then we
            // drive the real shell and pump `sync` to collect its output.
            let _ = frame.draw(app);
            app.shots_terminal_input(terminal::demo_session_input());
            for _ in 0..24 {
                frame.sync(app);
                std::thread::sleep(std::time::Duration::from_millis(60));
            }
        }
        Some("selection") => app.shots_select_words(),
        Some("completion") => {
            app.shots_completion_demo(&frame.state.doc);
            frame.sync(app);
        }
        Some("problems") => {
            app.shots_problems(&frame.state.doc);
            frame.sync(app);
        }
        Some("find") => {
            // `TILO_SHOTS_FIND` overrides the query so a shot of a real project
            // can search for something that is actually in it.
            let q = std::env::var("TILO_SHOTS_FIND").unwrap_or_else(|_| "primes".to_string());
            app.shots_open_find(&q, &frame.state.doc);
            frame.sync(app);
        }
        Some("conflict") => app.shots_conflict(),
        Some("close-prompt") => app.shots_close_prompt(),
        Some("status") => {
            app.shots_status();
            frame.sync(app);
        }
        Some("indexing") => {
            app.shots_indexing();
            // The status bar and the Problems panel read the server state
            // during `sync`, not during `draw`. Without this the shot renders
            // the frame from before the state was staged — an "indexing"
            // screenshot with no indexing in it, which would have looked like
            // the feature working.
            frame.sync(app);
        }
        _ => {}
    }
}
