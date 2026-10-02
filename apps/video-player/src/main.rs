//! video-player — thin binary entry point. All logic lives in the library.
//!
//! An empty window runs on the app framework (`VideoPlayerShell`). Opening a
//! file (`--open`, a path, or a URL) and capture modes keep the standalone
//! launcher, which is what actually starts that source.

#![cfg_attr(windows, windows_subsystem = "windows")]
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

use std::path::PathBuf;

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::theme::Theme;
use ui::widget::Widget;

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

#[cfg(feature = "thin")]
extern crate runtime_dylib;

/// # Errors
///
/// Returns an error if launching the app or running a `--shots` capture fails.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    if standalone_capture() {
        ui::launch::run(&standalone_launch_spec(live_root))?;
        return Ok(());
    }
    ui::launch::run_or_exit(&video_player::framework_launch_spec());
    Ok(())
}

fn standalone_capture() -> bool {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--shots" | "--responsive-matrix" | "--native-audit" | "--headless" | "--open" => {
                return true;
            }
            "--size" | "--viewport" | "--scenario" | "--data-dir" => {
                let _ = args.next();
            }
            flag if flag.starts_with("--open=") => return true,
            flag if flag.starts_with('-') => {}
            // A positional path or URL is the file this window should play.
            _ => return true,
        }
    }
    false
}

fn standalone_launch_spec(
    root: fn(&LaunchArgs) -> Result<Box<dyn Widget>, LaunchError>,
) -> LaunchSpec {
    LaunchSpec::new_with_args("so.tilo.video-player", root)
        .configure_app(|app| video_player::actions::register(app.action_registry_mut()))
        .shot_handler(render_shot)
        .allowed_live_options(&["--open"])
        .max_positionals(1)
        .scenarios(&[
            "empty",
            "loading",
            "error",
            "native-error",
            "playing",
            "paused",
            "settings",
            "metadata",
            "library",
            "library-focus",
            "recent",
            "shared",
            "fullscreen",
        ])
        .history(&video_player::framework_history::HISTORY)
}

fn requested_path(args: &LaunchArgs) -> Option<PathBuf> {
    args.value("--open").map(PathBuf::from).or_else(|| {
        args.positionals()
            .first()
            .filter(|value| !value.starts_with("http://") && !value.starts_with("https://"))
            .map(PathBuf::from)
    })
}

fn requested_source(args: &LaunchArgs) -> Option<media::Source> {
    if let Some(path) = requested_path(args) {
        return Some(video_player::source_for_path(&path));
    }
    args.positionals()
        .first()
        .map(|url| media::Source::Remote { url: url.clone() })
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by ui::launch::LaunchSpec::new_with_args"
)]
fn live_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(video_player::app_root_source(
        requested_source(args),
        std::env::var("TILO_MEDIA_HANDOFF_TITLE").ok(),
    ))
}

fn render_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args.shots.as_deref().ok_or_else(|| {
        LaunchError::Operation("video-player screenshot handler called without --shots".into())
    })?;
    let viewport = args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT);
    let open = requested_path(args);
    let require_decoded_frame = open.is_some()
        && !matches!(
            args.scenario.as_deref(),
            Some("empty" | "error" | "native-error" | "loading")
        );
    let mut app = video_player::PlayerApp::for_shot(open, args.scenario.as_deref());
    if require_decoded_frame
        && !app
            .player()
            .engine()
            .is_some_and(media::MediaEngine::is_real_decode)
    {
        return Err(LaunchError::Operation(
            "A real video capture requires the GStreamer decoder and runtime".into(),
        ));
    }
    // Real decode prerolls asynchronously; pump until its first picture is
    // ready. The synthesized fallback produces one on the first pass.
    let mut warm = ui::State::new();
    let deadline = ui::time::now() + std::time::Duration::from_secs(5);
    loop {
        app.sync(&mut warm.doc, &warm.root);
        if app.has_media_frame()
            || ui::time::now() >= deadline
            || ui::rt::sleep_thread(std::time::Duration::from_millis(40)).is_err()
        {
            break;
        }
    }
    if require_decoded_frame && !app.has_media_frame() {
        return Err(LaunchError::Operation(
            "The video decoder did not produce a frame for the requested file".into(),
        ));
    }
    render_shot_to_png(Box::new(app), viewport, ui::launch::shot_theme(args), out)
        .map_err(|error| LaunchError::Operation(error.to_string()))
}

/// Render one offscreen frame of `widget` and write it as a PNG.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    theme: Theme,
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // The offscreen renderer bypasses the regular app loop, so explicitly
    // propagate the accessibility motion preference before building the scene.
    widget.set_motion_reduced(theme.reduce_motion);
    ui::launch::render_widget_png(widget.as_mut(), viewport, theme, out)?;
    println!("video-player: wrote shot to {}", out.display());
    Ok(())
}

#[cfg(test)]
mod launch_tests {
    use super::*;

    #[allow(
        clippy::unnecessary_wraps,
        reason = "signature is fixed by the production argument-aware LaunchSpec"
    )]
    fn inert_root(_: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
        Ok(Box::new(video_player::PlayerApp::history_test_root()))
    }

    fn memory_state(_: &'static str) -> (ui::State, ui::persist::DocWriter) {
        (
            ui::State::new(),
            ui::persist::DocWriter::disabled("controlled Video Player launch"),
        )
    }

    #[test]
    fn production_argument_aware_launch_spec_installs_the_root_history_definition() {
        let spec = standalone_launch_spec(inert_root).open_app_global_state(memory_state);
        spec.validate_history_declaration().unwrap();
        let mut app = spec.build_controlled_app_for_test().unwrap();
        let (installed, root) = app.controlled_history_definitions_for_test();
        assert!(std::ptr::eq(
            installed.unwrap(),
            std::ptr::addr_of!(video_player::framework_history::HISTORY)
        ));
        assert!(std::ptr::eq(
            root.unwrap(),
            std::ptr::addr_of!(video_player::framework_history::HISTORY)
        ));
        assert!(
            app.build_a11y_tree_update(1.0)
                .nodes
                .iter()
                .any(|(_, node)| node.label() == Some("History"))
        );
    }
}
