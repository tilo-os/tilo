//! Code — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!                       (headless screenshot; identical passes to the live app).

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

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_shots_arg(&args) {
        // `--shots` renders one offscreen frame. `--viewport WxH` overrides the
        // default desktop viewport so the same binary can capture the narrow
        // (overlay-sidebar) responsive layout. `--sidebar-open` snaps the flap
        // open so a narrow shot shows the summoned sidebar + scrim rather than
        // the closed default a fresh narrow window opens to. `--scenario NAME`
        // stages a specific view (terminal / selection / completion).
        let viewport = parse_viewport_arg(&args).unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        let open_sidebar = args.iter().any(|a| a == "--sidebar-open");
        let collapse_sidebar = args.iter().any(|a| a == "--sidebar-collapsed");
        let scenario = parse_scenario_arg(&args);
        if let Some(name) = scenario.as_deref() {
            // An unknown scenario used to fall through silently and render the
            // plain cold-start frame — a check that cannot observe its subject
            // but reports success. Refuse it instead.
            if !SCENARIOS.contains(&name) {
                eprintln!(
                    "code_editor: unknown scenario '{name}'. Known scenarios: {}",
                    SCENARIOS.join(", ")
                );
                std::process::exit(2);
            }
        }
        // `--shots` with a path captures that real project; without one it
        // captures the bundled demo, which is the only place the demo is used.
        let mut app = match code_editor::parse_path_arg(&args) {
            Some(path) => code_editor::CodeApp::open_path(&path),
            None => code_editor::CodeApp::with_sample(),
        };
        // Seed the width up front so this single offscreen frame resolves the
        // responsive layout (docked vs. overlay) correctly — `build_layout`
        // runs before `apply_layout` would otherwise measure the window.
        app.set_width_hint(viewport.0 as f64);
        if open_sidebar {
            app.shots_snap_sidebar(true);
        }
        if collapse_sidebar {
            // Snap the sidebar shut so a wide/docked shot captures the collapsed
            // state — the editor reclaiming the full width, with the toggle in
            // the tab bar to bring it back.
            app.shots_snap_sidebar(false);
        }
        render_scenario_to_png(app, viewport, &out, scenario.as_deref())?;
        return Ok(());
    }

    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    let open_path = code_editor::parse_path_arg(&args);
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(
            spec,
            code_editor::app_root_at(open_path.as_deref()),
            "dev.qos.code-editor",
        ));
    }

    let (state, autosave) = ui::persist::open("dev.qos.code-editor");
    let mut app = ui::App::new(state, code_editor::app_root_at(open_path.as_deref()))
        .with_autosave(autosave);
    // Register Code's action so the shell's Ctrl+K palette can decode + replay the
    // same buffer-creation the on-screen "New file" command dispatches.
    code_editor::actions::register(app.action_registry_mut());
    app.run();
    Ok(())
}

/// Parse `--viewport WxH` (e.g. `--viewport 520x820`) for `--shots`.
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

/// Parse `--scenario NAME` (terminal / selection / completion).
fn parse_scenario_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--scenario" {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix("--scenario=") {
            return Some(rest.to_string());
        }
    }
    None
}

/// Render one offscreen frame of the `CodeApp`, optionally staging a scenario
/// first. Mirrors `ui::App`'s cycle (sync → build_layout → compute_layout →
/// apply_layout → draw); for the `terminal` scenario it also drives the live
/// PTY and pumps `sync` a few times so real shell output lands in the frame.
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
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

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
        Some("find") | Some("conflict") | Some("indexing") | Some("goto") | Some("close-prompt")
    ) {
        if let Some(path) = std::env::var_os("QOS_SHOTS_OPEN") {
            app.shots_open_path(std::path::Path::new(&path));
        } else if app.project_root().is_some() {
            app.shots_open_first_file();
        }
    }

    app.sync(&mut state.doc, &state.root);

    let child = app.build_layout(&mut taffy, &theme, &mut node_map);
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
    app.apply_layout(&taffy, child, Point::ORIGIN);

    // A helper that runs a full draw pass into a fresh scene.
    let draw = |app: &mut code_editor::CodeApp, tr: &mut TextRenderer| {
        let mut scene = Scene::new();
        app.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            tr,
            &theme,
            &presence,
            &permission,
        );
        scene
    };

    // Post-layout staging.
    match scenario {
        Some("terminal") => {
            // One draw settles the terminal's cell metrics + placement, then we
            // drive the real shell and pump `sync` to collect its output.
            let _ = draw(&mut app, &mut text_renderer);
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
    // The staged scenarios change the layout (the find bar appears, the search
    // panel is taller), so the tree has to be rebuilt before the final draw or
    // the new surfaces get last frame's geometry.
    if matches!(scenario, Some("find") | Some("search")) {
        let mut taffy2: TaffyTree<()> = TaffyTree::new();
        let mut map2: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child2 = app.build_layout(&mut taffy2, &theme, &mut map2);
        let root2 = taffy2.new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(w as f32),
                    height: Dimension::length(h as f32),
                },
                ..Default::default()
            },
            &[child2],
        )?;
        {
            let measure_map = map2.clone();
            let tr = &mut text_renderer;
            taffy2.compute_layout_with_measure(
                root2,
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
        app.apply_layout(&taffy2, child2, Point::ORIGIN);
    }

    let scene = draw(&mut app, &mut text_renderer);

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("code_editor: wrote shot to {}", out.display());
    Ok(())
}
