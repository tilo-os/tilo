//! Video Editor — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!     (headless screenshot; identical passes to the live app).

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

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec, ShotFrame};
use ui::theme::Theme;
use ui::widget::Widget;

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);
/// Narrow capture viewport — below the bin's collapse breakpoint so `--shots`
/// can exercise the overlay-flap layout. Pass `--narrow` to select it.
const NARROW_SHOTS_VIEWPORT: (u32, u32) = (720, 820);

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    // The live editor runs on the app framework: its project is a hosted
    // record with history, per-author undo and permissions. The scenario
    // captures still stage view state (selections, dialogs, a seeded demo
    // project) directly on a standalone editor, so they keep that path.
    let capture = std::env::args().any(|arg| {
        matches!(
            arg.as_str(),
            "--shots" | "--responsive-matrix" | "--native-audit" | "--headless"
        )
    });
    if !capture {
        ui::framework::run::<video_editor::VideoEditor>();
        return;
    }
    ui::launch::run_or_exit(
        &LaunchSpec::new("so.tilo.video-editor", video_editor::app_root)
            .shot_handler(shot_handler)
            .allowed_flags(&["--empty", "--narrow"])
            .allowed_options(&["--import"])
            .scenarios(video_editor::app::ShotScenario::ALL_NAMES)
            .configure_app(|app| video_editor::actions::register(app.action_registry_mut())),
    );
}

fn shot_handler(args: &LaunchArgs) -> Result<(), LaunchError> {
    render_shot(args).map_err(|error| LaunchError::Operation(error.to_string()))
}

#[allow(
    clippy::disallowed_methods,
    reason = "the one-shot capture handler runs on the main thread before the launcher \
              has spawned any thread, and the capture reads the media directory afterwards"
)]
fn render_shot(args: &LaunchArgs) -> Result<(), Box<dyn std::error::Error>> {
    let out = args.shots.as_deref().ok_or("missing screenshot output")?;
    let theme = ui::launch::shot_theme(args);
    // Point the FUSE-media seam at the bundled sample-media/ dir so the
    // capture resolves the demo project's media to local (hydrated)
    // sources — exercising the hydration branch without a live mount.
    // Honour an existing override if the caller already set one.
    if std::env::var_os("TILO_VE_MEDIA_DIR").is_none() {
        let bundled = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sample-media");
        if bundled.exists() {
            // Safety: set before any threads spawn for the one-shot render.
            unsafe {
                std::env::set_var("TILO_VE_MEDIA_DIR", bundled);
            }
        }
    }
    // Scenario selectors (deterministic single-frame captures):
    //   --narrow             collapsed-bin overlay layout
    //   --scenario filters   clip selected with colour filters + fades
    //   --scenario split     timeline just after a split at the playhead
    //   (none)               the full live editor
    let scenario = args.scenario.as_deref();
    // `--empty` renders the real live app (a blank project on a fresh
    // machine) so the first-run empty state can be screenshotted; every
    // other shot uses the seeded demo build.
    let empty = args.has("--empty");
    // `--import PATH` seeds a real imported clip (probed for real metadata)
    // and, under `--features decode`, warms the pipeline so the capture
    // shows the clip's true decoded frame — not the representative gradient.
    if let Some(path) = args.value("--import") {
        let mut app =
            video_editor::VideoEditorApp::new_import_placed(vec![std::path::PathBuf::from(path)]);
        // Warm up on ONE state so the import + the async-prerolled decoded
        // frame persist into the same document the render then draws from.
        let mut state = ui::State::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
        loop {
            app.sync(&mut state.doc, &state.root);
            if app.has_preview_frame() || std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        render_shot_to_png(
            Box::new(app),
            args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT),
            &theme,
            out,
            state,
        )?;
        return Ok(());
    }
    if empty {
        render_shot_to_png(
            video_editor::app_root(),
            args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT),
            &theme,
            out,
            ui::State::new(),
        )?;
    } else if args.has("--narrow") {
        render_shot_to_png(
            video_editor::app_root_narrow(),
            args.viewport.unwrap_or(NARROW_SHOTS_VIEWPORT),
            &theme,
            out,
            ui::State::new(),
        )?;
    } else {
        let viewport = args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        // Every scenario resolves through the same `ShotScenario` selector the
        // app itself defines, so adding one there is enough to make it
        // capturable — there is no second list here to keep in step.
        match scenario.and_then(video_editor::app::ShotScenario::from_arg) {
            Some(s) => render_shot_to_png(
                Box::new(video_editor::VideoEditorApp::new_for_shot(s)),
                viewport,
                &theme,
                out,
                ui::State::new(),
            )?,
            None => {
                render_shot_to_png(
                    video_editor::app_root_demo(),
                    viewport,
                    &theme,
                    out,
                    ui::State::new(),
                )?;
            }
        }
    }
    Ok(())
}

/// Run one offscreen frame against `widget` and write RGBA to `out` as PNG.
///
/// `state` carries the document: a fresh one for the seeded/demo scenarios, or
/// a **warmed** one (already synced through the import + decode warm-up) so the
/// imported media persists into the rendered frame instead of being wiped by a
/// fresh empty sync.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    theme: &Theme,
    out: &std::path::Path,
    state: ui::State,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut frame = ShotFrame::new(viewport, theme.clone()).with_state(state);
    frame.sync(widget.as_mut());
    frame.layout(widget.as_mut())?;
    let scene = frame.draw(widget.as_mut());
    frame.write_png(&scene, out);
    println!("video_editor: wrote shot to {}", out.display());
    Ok(())
}
