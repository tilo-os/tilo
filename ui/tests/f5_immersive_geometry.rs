//! F5 (timeline / media / capture widgets) geometry evidence and contracts.
//!
//! Renders the immersive, hand-laid-out widgets through the real
//! [`FrameHarness`] and offscreen renderer into
//! `target/ui-review/ui/f5/<tag>/*.png` (`tag` from `F5_SHOT_TAG`, default
//! `after`) so a reviewer can open them, and asserts the contracts a
//! screenshot cannot: every video-player control reachable through
//! accessibility is at least the minimum pointer target, and all of them
//! sit on one centre line.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::PathBuf;
use std::sync::Arc;

use accesskit::Role;
use vello::Scene;
use vello::kurbo::Affine;

use ui::State;
use ui::headless::FrameHarness;
use ui::icons::Icon;
use ui::offscreen::OffscreenRenderer;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::video_player::PlayerSetting;
use ui::widget::{CaptureMode, CaptureStudio, ScreenshotMarkup, VideoPlayer, Widget};

/// HIG §9.6: minimum pointer target.
const MIN_TARGET: f64 = 32.0;

fn out_dir() -> PathBuf {
    let tag = std::env::var("F5_SHOT_TAG").unwrap_or_else(|_| "after".into());
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/ui-review/ui/f5")
        .join(tag);
    std::fs::create_dir_all(&dir).expect("evidence directory");
    dir
}

/// A 640×400 two-tone gradient so the markup editor has a real capture to
/// frame; the content is irrelevant, the geometry around it is the point.
fn capture_rgba(w: u32, h: u32) -> Arc<[u8]> {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            px.push((x * 255 / w) as u8);
            px.push((y * 255 / h) as u8);
            px.push(160);
            px.push(255);
        }
    }
    Arc::from(px)
}

#[test]
fn video_player_controls_are_reachable_targets_on_one_centre_line() {
    let dir = out_dir();
    for (theme_name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        // Real media (a synthesized engine with an open source) so the
        // transport, seek, mute and volume controls are in the tree, plus a
        // promoted setting so a setting icon and the gear are too.
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote {
            url: "https://x/v.mp4".into(),
        });
        let mut player = VideoPlayer::new().with_engine(engine);
        player.set_setting(PlayerSetting::toggle(
            "captions",
            Icon::FileText,
            "Captions",
            2,
            false,
        ));
        player.set_setting(PlayerSetting::choice(
            "resolution",
            Icon::Monitor,
            "Resolution",
            0,
            vec!["1080p".into(), "720p".into()],
            0,
        ));
        player.reveal_controls();
        let harness =
            FrameHarness::new_with_theme(State::new(), Box::new(player), (960, 540), theme);
        harness.render_png(&dir.join(format!("video-player-controls-{theme_name}.png")));

        let controls: Vec<(String, accesskit::Rect)> = harness
            .a11y_nodes()
            .into_iter()
            // The player's `ToggleButton` (mute) reaches AccessKit as a
            // `Button` with a toggled state, so three roles cover the bar.
            .filter(|(_, node)| matches!(node.role(), Role::Button | Role::Slider | Role::ComboBox))
            .filter_map(|(_, node)| {
                node.bounds()
                    .map(|b| (node.label().unwrap_or_default().to_string(), b))
            })
            .collect();
        // Play/pause, seek, mute, volume, the promoted setting, the gear and
        // fullscreen: the bar's whole control set, every one observed.
        assert!(
            controls.len() >= 7,
            "{theme_name}: expected the bar's seven controls in the accessibility tree, got \
             {controls:?}"
        );
        let mid_y = {
            let (_, first) = &controls[0];
            (first.y0 + first.y1) / 2.0
        };
        for (name, b) in &controls {
            let (w, h) = (b.x1 - b.x0, b.y1 - b.y0);
            assert!(
                w >= MIN_TARGET - 1e-6 && h >= MIN_TARGET - 1e-6,
                "{theme_name}: {name} bounds {b:?} are under the {MIN_TARGET}px minimum target"
            );
            let cy = (b.y0 + b.y1) / 2.0;
            assert!(
                (cy - mid_y).abs() < 1e-6,
                "{theme_name}: {name} centre {cy} is off the shared centre line {mid_y}"
            );
        }
    }
}

#[test]
fn screenshot_markup_renders_wide_and_narrow() {
    let dir = out_dir();
    for (name, viewport) in [
        ("wide", (1200_u32, 800_u32)),
        ("narrow", (440_u32, 700_u32)),
    ] {
        let markup = ScreenshotMarkup::new(capture_rgba(640, 400), 640, 400);
        let harness = FrameHarness::new_with_theme(
            State::new(),
            Box::new(markup),
            viewport,
            Theme::dark().with_reduce_motion(true),
        );
        harness.render_png(&dir.join(format!("screenshot-markup-{name}.png")));
    }
}

#[test]
fn capture_studio_renders_selector_and_docked_recording_pill() {
    let dir = out_dir();
    let theme = Theme::dark().with_reduce_motion(true);

    let mut studio = CaptureStudio::default();
    studio.open();
    let harness =
        FrameHarness::new_with_theme(State::new(), Box::new(studio), (1400, 900), theme.clone());
    harness.render_png(&dir.join("capture-studio-selector.png"));

    // The docked recording pill is painted by the shell on top of the
    // frame, not by the widget's own `draw`; paint it the way the shell
    // does, at 1× and at 2× type.
    for scale in [1.0_f32, 2.0] {
        let mut s = CaptureStudio::default();
        let mut taffy = taffy::TaffyTree::new();
        let mut map = std::collections::HashMap::new();
        let node = s.build_layout(&mut taffy, &theme, &mut map);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(1400.0),
                    height: taffy::AvailableSpace::Definite(200.0),
                },
            )
            .expect("layout");
        s.apply_layout(&taffy, node, vello::kurbo::Point::ORIGIN);
        s.open();
        s.set_mode(CaptureMode::Recording);
        s.set_recording_active(true);
        s.set_recording_transition_progress(1000);
        let mut tr = TextRenderer::new();
        tr.set_text_scale(scale);
        let mut scene = Scene::new();
        s.draw_recording_indicator(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        OffscreenRenderer::new(1400, 200).render_png(
            &scene,
            theme.bg_app,
            &dir.join(format!("capture-studio-recording-pill-{scale}x.png")),
        );
    }
}
