#![allow(
    clippy::arithmetic_side_effects,
    clippy::manual_midpoint,
    clippy::needless_borrows_for_generic_args,
    reason = "pixel geometry over bounded fixture canvases"
)]

//! Timeline, media and capture widgets: geometry contracts and evidence.
//!
//! Renders the immersive, hand-laid-out widgets through the real
//! [`FrameHarness`] and offscreen renderer, checks each frame painted
//! something, and asserts the contracts a screenshot cannot: every
//! video-player control reachable through accessibility is at least the
//! minimum pointer target, and all of them sit on one centre line. With
//! `TILO_UI_SCREENSHOT_DIR` set, the frames are also written under
//! `immersive_widget_geometry/` there for a reviewer to open.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Arc;

use accesskit::Role;
use vello::Scene;
use vello::kurbo::Affine;

use tilo_ui::State;
use tilo_ui::headless::FrameHarness;
use tilo_ui::icons::Icon;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::video_player::PlayerSetting;
use tilo_ui::widget::{CaptureMode, CaptureStudio, ScreenshotMarkup, VideoPlayer, Widget};

/// HIG §9.6: minimum pointer target.
const MIN_TARGET: f64 = 32.0;

/// Check the frame painted something and keep it as opt-in evidence.
fn evidence(name: &str, width: u32, height: u32, rgba: &[u8]) {
    crate::support::evidence::assert_painted(name, rgba);
    crate::support::evidence::save_png(
        &format!("immersive_widget_geometry/{name}"),
        width,
        height,
        rgba,
    );
}

fn harness_evidence(harness: &FrameHarness, name: &str, viewport: (u32, u32)) {
    evidence(name, viewport.0, viewport.1, &harness.render_rgba());
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

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn immersive_widgets_render_reachable_geometry_at_every_size() {
    // Case `video_player_controls_are_reachable_targets_on_their_control_rows`.
    {
        for (theme_name, theme, viewport, min_controls) in [
            ("dark", Theme::dark(), (960_u32, 540_u32), 7_usize),
            ("light", Theme::light(), (960, 540), 7),
            // 2× type through the owned-palette convention (the theme carries
            // the scale): the time label yields before any control does.
            // Large text puts every setting under the gear, so the promoted
            // setting no longer appears as a separate seventh bar target.
            ("dark-2x", Theme::dark().with_text_scale(2.0), (960, 540), 6),
            // Narrow and 2×: the setting icons fold into the gear and the label
            // goes entirely, but every control that remains is still a target.
            (
                "dark-2x-narrow",
                Theme::dark().with_text_scale(2.0),
                (320, 240),
                4,
            ),
        ] {
            // Real media (a synthesized engine with an open source) so the
            // transport, seek, mute and volume controls are in the tree, plus a
            // promoted setting so a setting icon and the gear are too.
            let mut engine = media::MediaEngine::new_synthesized();
            engine.open(media::Source::Remote {
                url: "https://x/v.mp4".into(),
            });
            let large_text = theme.text_scale > 1.25;
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
                FrameHarness::new_with_theme(State::new(), Box::new(player), viewport, theme);
            harness_evidence(
                &harness,
                &format!("video-player-controls-{theme_name}"),
                viewport,
            );

            let controls: Vec<(String, accesskit::Rect)> = harness
                .a11y_nodes()
                .into_iter()
                // The player's `ToggleButton` (mute) reaches AccessKit as a
                // `Button` with a toggled state, so three roles cover the bar.
                .filter(|(_, node)| {
                    matches!(node.role(), Role::Button | Role::Slider | Role::ComboBox)
                })
                .filter_map(|(_, node)| {
                    node.bounds()
                        .map(|b| (node.label().unwrap_or_default().to_string(), b))
                })
                .collect();
            // Play/pause, seek, mute, volume, the gear, fullscreen, and at 1x the
            // promoted setting: the bar's whole visible control set is observed.
            assert!(
                controls.len() >= min_controls,
                "{theme_name}: expected at least {min_controls} of the bar's controls in the \
             accessibility tree, got {controls:?}"
            );
            let (_, first_bar_control) = controls
                .iter()
                .find(|(name, _)| name != "Playback position")
                .expect("the bar retains at least one transport control");
            let mid_y = (first_bar_control.y0 + first_bar_control.y1) / 2.0;
            for (name, b) in &controls {
                let (w, h) = (b.x1 - b.x0, b.y1 - b.y0);
                assert!(
                    w >= MIN_TARGET - 1e-6 && h >= MIN_TARGET - 1e-6,
                    "{theme_name}: {name} bounds {b:?} are under the {MIN_TARGET}px minimum target"
                );
                let cy = (b.y0 + b.y1) / 2.0;
                if large_text && name == "Playback position" {
                    assert!(
                        cy < mid_y,
                        "{theme_name}: the large-text timeline centre {cy} must sit above the bar row {mid_y}"
                    );
                    continue;
                }
                assert!(
                    (cy - mid_y).abs() < 1e-6,
                    "{theme_name}: {name} centre {cy} is off the shared centre line {mid_y}"
                );
            }
        }
    }

    // Case `presence_pills_and_edge_tags_render_at_one_and_two_x`.
    // Round two: pointer pills (one an agent), and two off-screen peers whose
    // edge tags must stay inside their mist bands, at 1× and at 2× through the
    // renderer (the host-palette convention) — so the scaled insets and the
    // band-bound tag can be seen, not only asserted.
    {
        use tilo_ui::presence::{PeerCursor, draw_peer_presence};
        use vello::peniko::Color;
        let theme = Theme::dark();
        let (w, h) = (480_u32, 240_u32);
        let viewport = vello::kurbo::Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
        for scale in [1.0_f32, 2.0] {
            let mut tr = TextRenderer::new();
            tr.set_text_scale(scale);
            let peers = vec![
                PeerCursor {
                    pos: (120.0, 90.0),
                    label: "Alice".into(),
                    color: Color::from_rgb8(0xec, 0x48, 0x99),
                    is_agent: false,
                    opacity: 1.0,
                    name_opacity: 1.0,
                    chat: None,
                    reaction: None,
                },
                PeerCursor {
                    pos: (170.0, 150.0),
                    label: "Ada".into(),
                    color: Color::from_rgb8(0x0e, 0xa5, 0xe9),
                    is_agent: true,
                    opacity: 1.0,
                    name_opacity: 1.0,
                    chat: None,
                    reaction: None,
                },
                // Past the right edge, with a name no side band can hold whole.
                PeerCursor {
                    pos: (900.0, 120.0),
                    label: "Charlotte Okonkwo".into(),
                    color: Color::from_rgb8(0xf9, 0x73, 0x16),
                    is_agent: false,
                    opacity: 1.0,
                    name_opacity: 1.0,
                    chat: None,
                    reaction: None,
                },
                // Above the top edge.
                PeerCursor {
                    pos: (300.0, -300.0),
                    label: "Bob".into(),
                    color: Color::from_rgb8(0x10, 0xb9, 0x81),
                    is_agent: false,
                    opacity: 1.0,
                    name_opacity: 1.0,
                    chat: None,
                    reaction: None,
                },
            ];
            let mut scene = Scene::new();
            draw_peer_presence(
                &mut scene,
                Affine::IDENTITY,
                viewport,
                &mut tr,
                &theme,
                &peers,
            );
            let rgba = OffscreenRenderer::new(w, h)
                .render_resilient(&scene, theme.tokens.colors.surfaces.content);
            evidence(&format!("presence-pills-{scale}x"), w, h, &rgba);
        }
    }

    // Case `screenshot_markup_renders_wide_and_narrow`.
    {
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
            harness_evidence(&harness, &format!("screenshot-markup-{name}"), viewport);
        }
    }

    // Case `capture_studio_renders_selector_and_docked_recording_pill`.
    {
        let theme = Theme::dark().with_reduce_motion(true);

        let mut studio = CaptureStudio::default();
        studio.open();
        let harness = FrameHarness::new_with_theme(
            State::new(),
            Box::new(studio),
            (1400, 900),
            theme.clone(),
        );
        harness_evidence(&harness, "capture-studio-selector", (1400, 900));

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
            let rgba = OffscreenRenderer::new(1400, 200)
                .render_resilient(&scene, theme.tokens.colors.surfaces.content);
            evidence(
                &format!("capture-studio-recording-pill-{scale}x"),
                1400,
                200,
                &rgba,
            );
        }
    }
}
