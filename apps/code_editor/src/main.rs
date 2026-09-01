//! Code — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//! * default — run the live winit event loop via `ui::App`.
//! * `--shots PATH` — render one offscreen frame to a PNG and exit (headless
//!   screenshot; identical passes to the live app).

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
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::collections::HashMap;

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
use vello::peniko::Color;

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

/// Every `--scenario` name `--shots` knows how to stage. Keeping the list here
/// rather than only in the `match` arms is what lets an unknown name be an
/// error instead of a silently ordinary screenshot.
const SCENARIOS: &[&str] = &[
    "terminal",
    "selection",
    "completion",
    "find",
    "search",
    "conflict",
    "indexing",
    "goto",
    "close-prompt",
    "status",
];

/// # Errors
/// Returns an error when `--shots` fails to lay out or render the offscreen
/// frame (see [`render_scenario_to_png`]); an unknown `--scenario` name exits
/// the process directly rather than returning an error.
#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    ui::launch::run(
        &LaunchSpec::new("dev.qos.code-editor", code_editor::app_root)
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
    let mut app = requested_path(args).map_or_else(code_editor::CodeApp::with_sample, |path| {
        code_editor::CodeApp::open_path(&path)
    });
    app.set_width_hint(f64::from(viewport.0));
    if args.has("--sidebar-open") {
        app.shots_snap_sidebar(true);
    }
    render_scenario_to_png(app, viewport, out, args.scenario.as_deref())
        .map_err(|error| LaunchError::Operation(error.to_string()))
}

/// Render one offscreen frame of the `CodeApp`, optionally staging a scenario
/// first. Mirrors `ui::App`'s cycle (sync → `build_layout` → `compute_layout` →
/// `apply_layout` → draw); for the `terminal` scenario it also drives the live
/// PTY and pumps `sync` a few times so real shell output lands in the frame.
///
/// # Errors
/// Returns an error if taffy fails to assemble or measure the offscreen
/// layout tree.
fn render_scenario_to_png(
    mut app: code_editor::CodeApp,
    viewport: (u32, u32),
    out: &std::path::Path,
    scenario: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    // `QOS_SHOTS_THEME=light|high-contrast` renders the shot under that theme so
    // theme-driven chrome can be proven across palettes; defaults to dark.
    let theme = match std::env::var("QOS_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut state = ui::State::new();

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
        Some("find" | "conflict" | "indexing" | "goto" | "close-prompt")
    ) {
        if let Some(path) = std::env::var_os("QOS_SHOTS_OPEN") {
            app.shots_open_path(std::path::Path::new(&path));
        } else if app.project_root().is_some() {
            app.shots_open_first_file();
        }
    }

    app.sync(&mut state.doc, &state.root);
    build_and_apply_layout(&mut app, &theme, &mut text_renderer, w, h)?;

    stage_scenario(
        &mut app,
        scenario,
        &mut state,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
    );

    // The staged scenarios change the layout (the find bar appears, the search
    // panel is taller), so the tree has to be rebuilt before the final draw or
    // the new surfaces get last frame's geometry.
    if matches!(scenario, Some("find" | "search")) {
        build_and_apply_layout(&mut app, &theme, &mut text_renderer, w, h)?;
    }

    let scene = draw_frame(&mut app, &mut text_renderer, &theme, &presence, &permission);

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("code_editor: wrote shot to {}", out.display());
    Ok(())
}

/// Build `app`'s widget tree at `w`×`h`, measure it, and apply the result —
/// the `build_layout` → `compute_layout` → `apply_layout` third of the cycle,
/// shared by the initial layout and the rebuild after a scenario changes it.
///
/// # Errors
/// Returns an error if taffy fails to assemble or measure the layout tree.
fn build_and_apply_layout(
    app: &mut code_editor::CodeApp,
    theme: &Theme,
    text_renderer: &mut TextRenderer,
    w: u32,
    h: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = app.build_layout(&mut taffy, theme, &mut node_map);
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
        let tr = &mut *text_renderer;
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
    app.apply_layout(&taffy, child, Point::ORIGIN);
    Ok(())
}

/// Run one full draw pass into a fresh scene.
fn draw_frame(
    app: &mut code_editor::CodeApp,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
    permission: &PermissionContext,
) -> Scene {
    let mut scene = Scene::new();
    app.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        text_renderer,
        theme,
        presence,
        permission,
    );
    scene
}

/// Stage the requested `--scenario` after the first layout pass: select
/// words, open the find bar, drive the live terminal's PTY, and so on. A
/// `None` scenario (the default cold-start frame) stages nothing.
#[allow(
    clippy::too_many_arguments,
    reason = "threads the same handful of already-built shots context (app, state, renderer, theme, presence, permission) to whichever scenario arm needs it; grouping them into a struct would not make the call clearer"
)]
fn stage_scenario(
    app: &mut code_editor::CodeApp,
    scenario: Option<&str>,
    state: &mut ui::State,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
    permission: &PermissionContext,
) {
    match scenario {
        Some("terminal") => {
            // One draw settles the terminal's cell metrics + placement, then we
            // drive the real shell and pump `sync` to collect its output.
            let _ = draw_frame(app, text_renderer, theme, presence, permission);
            app.shots_terminal_input(
                "echo 'qOS embedded terminal (live PTY)'; echo \"shell: $SHELL\"; \
                 echo 'demo output:'; echo '  primes below 20: 2 3 5 7 11 13 17 19'; echo done\n",
            );
            for _ in 0..24 {
                app.sync(&mut state.doc, &state.root);
                std::thread::sleep(std::time::Duration::from_millis(60));
            }
        }
        Some("selection") => app.shots_select_words(),
        Some("completion") => app.shots_completion_demo(&state.doc),
        Some("find") => {
            // `QOS_SHOTS_FIND` overrides the query so a shot of a real project
            // can search for something that is actually in it.
            let q = std::env::var("QOS_SHOTS_FIND").unwrap_or_else(|_| "primes".to_string());
            app.shots_open_find(&q, true, &state.doc);
        }
        Some("search") => {
            let q = std::env::var("QOS_SHOTS_SEARCH").unwrap_or_else(|_| "fn main".to_string());
            app.shots_project_search(&q);
            app.sync(&mut state.doc, &state.root);
        }
        Some("conflict") => app.shots_conflict(),
        Some("goto") => {
            let n = std::env::var("QOS_SHOTS_GOTO").unwrap_or_else(|_| "42".to_string());
            app.shots_open_goto(&n);
        }
        Some("close-prompt") => app.shots_close_prompt(),
        Some("status") => app.shots_status(),
        Some("indexing") => {
            app.shots_indexing();
            // The status bar and the Problems panel read the server state
            // during `sync`, not during `draw`. Without this the shot renders
            // the frame from before the state was staged — an "indexing"
            // screenshot with no indexing in it, which would have looked like
            // the feature working.
            app.sync(&mut state.doc, &state.root);
        }
        _ => {}
    }
}
