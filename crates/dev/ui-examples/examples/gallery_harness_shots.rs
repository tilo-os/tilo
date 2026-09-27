#![allow(
    clippy::exit,
    reason = "migration stack lint debt is scoped to this target"
)]

//! The component gallery mounted in a [`FrameHarness`], rendered in **dark
//! and light** at 1× on the theme's own background, with the harness's
//! inspection snapshot beside each PNG — the pair the 1× review batch reads:
//! the picture for the eye, the rects for `scripts/ux-bench/usability.py`'s
//! `geometry_audit` (overlap / touching lint).
//!
//! Run:  `cargo run -p ui-examples --example ui-shots -- gallery_harness [OUT_DIR]`
//! Out:  `$GALLERY_HARNESS_OUT` or the first argument (default
//!       `/tmp/gallery_harness`) — `gallery-dark.png`, `gallery-light.png`,
//!       `gallery-dark.inspection.json`, `gallery-light.inspection.json`.
//! Scale: `TILO_SHOTS_TEXT_SCALE=2` renders the large-text variant (the same
//! switch every other capture honours).

use std::path::PathBuf;

use tilo_ui::State;
use tilo_ui::headless::FrameHarness;
use tilo_ui::theme::Theme;
use tilo_ui::widget::Scrollable;

use super::gallery_scene;

/// Viewport tall enough that the whole showcase lays out without a scroll
/// offset, so every card is in the frame and in the inspection.
const VIEWPORT: (u32, u32) = (1200, 2300);

pub fn run(args: &[String]) {
    let out = args
        .first()
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("GALLERY_HARNESS_OUT").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/tmp/gallery_harness"));
    if let Err(error) = std::fs::create_dir_all(&out) {
        eprintln!("gallery_harness: cannot create {}: {error}", out.display());
        std::process::exit(2);
    }
    let environment_scale = std::env::var("TILO_SHOTS_TEXT_SCALE").ok();
    let text_scale = tilo_ui::launch::shot_text_scale(None, environment_scale.as_deref());

    for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        let theme = theme.with_text_scale(text_scale).with_reduce_motion(true);
        let mut state = State::new();
        let root = gallery_scene::build_showcase(&mut state, &theme);
        // The showcase is a tall column; the harness gives it a scroll owner
        // so the layout matches the live `gallery` example.
        let page: Box<dyn tilo_ui::widget::Widget> =
            Box::new(Scrollable::new_vertical().with_child(root));
        let harness = FrameHarness::new_with_theme(state, page, VIEWPORT, theme);
        let png = out.join(format!("gallery-{name}.png"));
        // Composited on the theme's own background, so pixel probes see the
        // real ground, not a transparent canvas.
        harness.render_window_png(&png);
        match harness.inspection_json() {
            Ok(json) => {
                let path = out.join(format!("gallery-{name}.inspection.json"));
                if let Err(error) = std::fs::write(&path, json) {
                    eprintln!("gallery_harness: cannot write {}: {error}", path.display());
                    std::process::exit(2);
                }
            }
            Err(error) => {
                eprintln!("gallery_harness: inspection failed: {error}");
                std::process::exit(2);
            }
        }
        println!("gallery_harness: wrote {}", png.display());
    }
}
