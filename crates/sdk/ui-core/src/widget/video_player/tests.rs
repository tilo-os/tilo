use super::*;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use automerge::AutoCommit;

use crate::synthetic_input::cursor_moved;

use crate::synthetic_input::mouse_press;
use crate::testing::dispatch;
use winit::keyboard::{Key, NamedKey};

#[test]
fn an_engine_attached_when_needed_keeps_the_players_volume_and_is_built_once() {
    let mut vp = VideoPlayer::new();
    vp.set_volume(0.3);
    assert!(vp.engine().is_none(), "a new player starts no engine");
    let mut built = 0;
    vp.ensure_engine(|| {
        built += 1;
        media::MediaEngine::new_synthesized()
    });
    assert!((vp.engine().unwrap().volume() - 0.3).abs() < 1e-6);
    vp.ensure_engine(|| {
        built += 1;
        media::MediaEngine::new_synthesized()
    });
    assert_eq!(built, 1, "an attached engine is kept");

    let mut muted = VideoPlayer::new();
    muted.toggle_mute();
    let engine = muted.ensure_engine(media::MediaEngine::new_synthesized);
    assert!(
        engine.volume().abs() < 1e-6,
        "a muted player's engine is silent"
    );
}

#[test]
fn a_playing_video_shows_its_controls_then_a_progress_line_and_a_paused_one_keeps_them() {
    let mut engine = media::MediaEngine::new_synthesized();
    engine.open(media::Source::Remote {
        url: "v.mp4".into(),
    });
    let mut vp = VideoPlayer::new().with_engine(engine);
    vp.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
    let doc = AutoCommit::new();
    let root = automerge::ROOT;
    vp.sync(&doc, &root);
    draw_once(&mut vp);
    assert!(
        vp.play_button_rect.width() > 0.0,
        "a loaded video that is not playing keeps its controls up"
    );
    assert!(
        !vp.needs_animation_frame(),
        "pinned controls on a paused video ask for no frames"
    );

    vp.engine_mut().unwrap().play();
    vp.sync(&doc, &root);
    draw_once(&mut vp);
    assert!(
        vp.play_button_rect.width() > 0.0,
        "playback starting shows the controls"
    );
    assert_eq!(vp.progress_line_painted, Rect::ZERO);

    vp.seek_to_ms(vp.duration_ms() / 2);
    vp.controls_visible_until = None;
    draw_once(&mut vp);
    assert_eq!(vp.play_button_rect, Rect::ZERO, "the controls hid");
    let line = vp.progress_line_painted;
    assert!(
        (line.y1 - vp.rect.y1).abs() < 1e-9 && line.height() > 0.0,
        "the progress line runs along the picture's bottom edge: {line:?}"
    );
    assert!(
        (line.width() - vp.rect.width() / 2.0).abs() < 2.0,
        "it is filled as far as the video has played: {line:?}"
    );
}

/// The names of the controls the semantics tree lists now.
fn listed_controls(vp: &VideoPlayer) -> Vec<String> {
    vp.accessibility()
        .map(|node| {
            node.children
                .iter()
                .filter_map(|child| child.name.clone())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn controls_that_hid_or_faded_leave_the_tree_with_the_frame_that_hides_them() {
    let mut engine = media::MediaEngine::new_synthesized();
    engine.open(media::Source::Remote {
        url: "v.mp4".into(),
    });
    let mut vp = VideoPlayer::new().with_engine(engine);
    vp.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
    let doc = AutoCommit::new();
    let root = automerge::ROOT;
    vp.engine_mut().unwrap().play();
    vp.sync(&doc, &root);
    draw_once(&mut vp);
    assert!(
        listed_controls(&vp)
            .iter()
            .any(|name| name == "Playback position"),
        "shown controls are listed: {:?}",
        listed_controls(&vp)
    );

    // The auto-hide deadline passes; no frame has been drawn since, so
    // the screen still shows the controls, and the tree with it.
    vp.controls_visible_until = Some(crate::time::now());
    assert!(
        listed_controls(&vp)
            .iter()
            .any(|name| name == "Playback position"),
        "the controls on screen are listed until a frame takes them off"
    );
    draw_once(&mut vp);
    assert_eq!(
        listed_controls(&vp),
        Vec::<String>::new(),
        "the frame that hides them takes them out of the tree"
    );
    assert_eq!(vp.scrub_track_rect, Rect::ZERO, "and nothing is hit there");
    assert!(
        vp.progress_line_painted.height() > 0.0,
        "the progress line shows meanwhile"
    );

    // In the last moments of the fade the controls are all but gone.
    vp.controls_visible_until = crate::time::now().checked_add(Duration::from_millis(5));
    assert!(vp.controls_opacity() < 0.5);
    draw_once(&mut vp);
    assert_eq!(
        listed_controls(&vp),
        Vec::<String>::new(),
        "controls faded past half are not listed"
    );

    vp.show_controls();
    draw_once(&mut vp);
    assert!(
        listed_controls(&vp)
            .iter()
            .any(|name| name == "Playback position"),
        "shown again, they are listed again"
    );
}

#[test]
fn a_reveal_reaches_the_screen_even_when_the_next_frame_comes_after_its_deadline() {
    let mut engine = media::MediaEngine::new_synthesized();
    engine.open(media::Source::Remote {
        url: "v.mp4".into(),
    });
    let mut vp = VideoPlayer::new().with_engine(engine);
    vp.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
    let doc = AutoCommit::new();
    let root = automerge::ROOT;
    vp.engine_mut().unwrap().play();
    vp.sync(&doc, &root);
    // Playback starting shows the controls; once drawn, they hide.
    draw_once(&mut vp);
    vp.controls_visible_until = None;
    draw_once(&mut vp);
    assert_eq!(
        listed_controls(&vp),
        Vec::<String>::new(),
        "hidden while playing"
    );

    // The pointer moves; the page is so busy its next frame comes only
    // after the auto-hide deadline.
    vp.show_controls();
    vp.controls_visible_until = Some(crate::time::now());
    draw_once(&mut vp);
    assert!(
        vp.scrub_track_rect.width() > 0.0
            && listed_controls(&vp)
                .iter()
                .any(|name| name == "Playback position"),
        "that frame still draws the controls the pointer asked for"
    );
    draw_once(&mut vp);
    assert_eq!(
        listed_controls(&vp),
        Vec::<String>::new(),
        "and the frame after hides them"
    );
}

#[test]
fn transport_keys_and_surface_clicks_drive_the_engine() {
    {
        let vp = VideoPlayer::new();
        assert_eq!(vp.state(), PlaybackState::Paused);
        assert!(!vp.is_playing());
    }

    {
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote {
            url: "https://x/v.mp4".into(),
        });
        let mut vp = VideoPlayer::new().with_engine(engine);
        let doc = AutoCommit::new();
        let root = automerge::ROOT;
        vp.sync(&doc, &root);
        assert!(
            vp.latest_frame().is_some(),
            "engine frame pulled into the surface"
        );
        assert!(vp.duration_ms() > 0, "duration mirrored from the engine");
    }

    {
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote {
            url: "https://x/v.mp4".into(),
        });
        let mut vp = VideoPlayer::new().with_engine(engine);
        vp.set_focus(true);
        assert!(!vp.is_playing());
        assert!(vp.handle_key_input(&Key::Named(NamedKey::Space), ElementState::Pressed));
        assert!(vp.is_playing(), "Space started playback via the engine");
        assert!(vp.handle_key_input(&Key::Named(NamedKey::Space), ElementState::Pressed));
        assert!(!vp.is_playing(), "Space paused it again");
    }

    {
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote {
            url: "https://x/v.mp4".into(),
        });
        let mut vp = VideoPlayer::new().with_engine(engine);
        vp.set_focus(true);
        vp.seek_to_ms(20_000);
        assert!(vp.handle_key_input(&Key::Named(NamedKey::ArrowRight), ElementState::Pressed));
        assert_eq!(vp.position_ms(), 25_000, "→ seeks forward 5s");
        assert!(vp.handle_key_input(&Key::Named(NamedKey::ArrowLeft), ElementState::Pressed));
        assert_eq!(vp.position_ms(), 20_000, "← seeks back 5s");
        // The seek propagated to the engine, not just the widget cache.
        assert_eq!(vp.engine().unwrap().position_ms(), 20_000);
    }

    {
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote {
            url: "https://x/v.mp4".into(),
        });
        let mut vp = VideoPlayer::new().with_engine(engine);
        vp.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
        vp.reveal_controls();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        assert!(!vp.is_playing());
        // Top-centre: clear of the controls bar, the scrub track, every button.
        dispatch(&mut vp, &cursor_moved(320.0, 40.0));
        assert!(dispatch(&mut vp, &mouse_press()), "surface click handled");
        assert!(vp.is_playing(), "surface click started playback");
        dispatch(&mut vp, &mouse_press());
        assert!(!vp.is_playing(), "a second surface click paused it");
    }
}

#[test]
fn scrub_track_seeks_where_it_is_painted_and_shows_sponsor_bands() {
    {
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote {
            url: "https://x/v.mp4".into(),
        });
        let mut vp = VideoPlayer::new().with_engine(engine);
        vp.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
        vp.duration_ms = 100_000;
        vp.reveal_controls();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        assert!(vp.scrub_track_rect.width() > 0.0, "scrub track laid out");
        let mid_x = vp.scrub_track_rect.x0 + vp.scrub_track_rect.width() * 0.5;
        let mid_y = vp.scrub_track_rect.center().y;
        dispatch(&mut vp, &cursor_moved(mid_x, mid_y));
        let handled = dispatch(&mut vp, &mouse_press());
        assert!(handled, "scrub press handled");
        let pos = vp.position_ms();
        assert!((45_000..=55_000).contains(&pos), "seeked to ~50%: {pos}ms");
    }

    {
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote {
            url: "https://x/v.mp4".into(),
        });
        let mut vp = VideoPlayer::new().with_engine(engine);
        vp.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
        vp.duration_ms = 100_000;
        vp.reveal_controls();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        let track = vp.scrub_track_rect;
        assert!(track.width() > 0.0);
        let y = track.center().y;
        dispatch(&mut vp, &cursor_moved(track.x0 + track.width() * 0.1, y));
        assert!(
            dispatch(&mut vp, &mouse_press()),
            "press on track begins a scrub"
        );
        assert!(vp.scrubbing, "press started a drag");
        assert!(vp.wants_pointer_capture(), "a scrub owns the pointer");
        let near = vp.position_ms();
        dispatch(&mut vp, &cursor_moved(track.x0 + track.width() * 0.9, y));
        let far = vp.position_ms();
        assert!(
            far > near,
            "dragging right advanced the playhead ({near} → {far})"
        );
        assert!((85_000..=95_000).contains(&far), "drag to ~90%: {far}ms");
    }

    {
        let mut vp = VideoPlayer::new();
        vp.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
        vp.show_controls();
        draw_once(&mut vp);
        let bar = vp.controls_bar_rect(&Theme::dark());
        let pad = f64::from(Theme::dark().tokens.spacing.s2);
        let icon = PLAY_BUTTON_SIZE.min(bar.height() - 8.0);
        // The painted track begins one pad after the play glyph; the hit
        // rect must start at the same x so a click maps to the same fraction
        // the paint shows.
        let painted_x0 = bar.x0 + pad + icon + pad;
        assert!(
            (vp.scrub_track_rect.x0 - painted_x0).abs() < 1e-6,
            "scrub hit rect x0 {} drifted from the painted track {painted_x0}",
            vp.scrub_track_rect.x0
        );
        assert!(
            vp.scrub_track_rect.height() > SCRUB_TRACK_HEIGHT,
            "the hit rect must be taller than the 5px track it targets"
        );
    }

    {
        let mut vp = VideoPlayer::new();
        vp.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
        vp.duration_ms = 100_000;
        vp.reveal_controls();
        let baseline = {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let presence = PresenceCtx::default();
            let mut scene = Scene::new();
            vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            scene.encoding().draw_tags.len()
        };
        vp.set_sponsor_marks(vec![
            (0.2, 0.3, Color::from_rgb8(0, 0xd4, 0)),
            (0.6, 0.72, Color::from_rgb8(0, 0xff, 0xff)),
        ]);
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        assert!(
            scene.encoding().draw_tags.len() > baseline,
            "two sponsor bands add fills to the timeline ({} vs {baseline})",
            scene.encoding().draw_tags.len(),
        );
    }
}

#[test]
fn fullscreen_intent_toggles_and_requests_the_largest_output() {
    {
        let mut vp = VideoPlayer::new();
        vp.set_focus(true);
        assert_eq!(
            FullscreenProvider::fullscreen_intent(&vp),
            None,
            "no opinion until asked"
        );
        assert!(vp.handle_key_input(&Key::Character("f".into()), ElementState::Pressed));
        assert_eq!(FullscreenProvider::fullscreen_intent(&vp), Some(true));
        assert!(vp.handle_key_input(&Key::Character("F".into()), ElementState::Pressed));
        assert_eq!(FullscreenProvider::fullscreen_intent(&vp), Some(false));
    }

    {
        use crate::fullscreen::{FullscreenTarget, FullscreenView};
        let mut vp = VideoPlayer::new();
        assert_eq!(FullscreenProvider::fullscreen_intent(&vp), None);
        assert_eq!(
            FullscreenProvider::fullscreen_params(&vp).target,
            FullscreenTarget::Largest
        );
        vp.toggle_fullscreen();
        assert_eq!(
            FullscreenProvider::fullscreen_intent(&vp),
            Some(true),
            "toggle asks the App to fullscreen"
        );
        let params = FullscreenProvider::fullscreen_params(&vp);
        assert_eq!(
            params.target,
            FullscreenTarget::Largest,
            "immersive video targets the big screen"
        );
        assert_eq!(
            params.view,
            FullscreenView::Mirror,
            "video mirrors the window, not a distinct view"
        );
        vp.set_fullscreen_target(FullscreenTarget::Primary);
        assert_eq!(
            FullscreenProvider::fullscreen_params(&vp).target,
            FullscreenTarget::Primary
        );
    }
}

#[test]
fn media_session_and_engine_events_reach_the_right_side_once() {
    {
        let mut vp = VideoPlayer::new();
        vp.duration_ms = 240_000;
        vp.set_now_playing(NowPlaying {
            track_id: "yt:abc123".into(),
            title: "Runaway".into(),
            artist: "Aurora".into(),
            art_url: "https://img/a.jpg".into(),
        });
        vp.play();
        let snap = vp.mpris_snapshot();
        assert_eq!(
            snap.status,
            crate::mpris::PlaybackStatus::Playing,
            "play → Playing on the bus"
        );
        assert_eq!(snap.title, "Runaway");
        assert_eq!(snap.artist, "Aurora");
        assert_eq!(snap.art_url, "https://img/a.jpg");
        assert_eq!(snap.length_us, 240_000_000, "duration published as µs");
        assert!(
            snap.can_play && snap.can_pause && snap.can_seek,
            "transport controls advertised"
        );
    }

    {
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote {
            url: "https://x/v.mp4".into(),
        });
        let mut vp = VideoPlayer::new().with_engine(engine);
        vp.play();
        assert!(vp.is_playing());
        vp.apply_media_command(crate::mpris::MprisCommand::Pause);
        assert!(!vp.is_playing(), "external Pause paused playback");
        vp.apply_media_command(crate::mpris::MprisCommand::PlayPause);
        assert!(vp.is_playing(), "external PlayPause resumed it");
    }

    {
        let mut vp = VideoPlayer::new();
        vp.record_terminal_media_events([
            media::MediaEvent::Ended,
            media::MediaEvent::MissingDecoder {
                codec: "HEVC".into(),
            },
            media::MediaEvent::Error("decoder stopped".into()),
        ]);
        let events = vp.take_media_events();
        assert!(
            matches!(events.as_slice(), [media::MediaEvent::Ended, media::MediaEvent::MissingDecoder { codec }, media::MediaEvent::Error(message)] if codec == "HEVC" && message == "decoder stopped")
        );
        assert!(
            vp.take_media_events().is_empty(),
            "taking events cannot replay an old terminal status"
        );
    }

    {
        let mut vp = VideoPlayer::new();
        for position in 0..1_000 {
            vp.record_terminal_media_events([
                media::MediaEvent::DurationKnown { ms: 90_000 },
                media::MediaEvent::Buffering { percent: 50 },
                media::MediaEvent::Position { ms: position },
            ]);
        }
        assert!(
            vp.media_events.is_empty(),
            "progress is mirrored by the widget, never buffered for the app"
        );
    }
}

fn draw_once(vp: &mut VideoPlayer) {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
}

fn draw_once_with_theme(vp: &mut VideoPlayer, theme: &Theme) {
    let mut tr = TextRenderer::new();
    tr.set_text_scale(theme.text_scale);
    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    vp.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
}

#[test]
fn over_media_chrome_is_legible_and_dark_in_every_theme() {
    use crate::a11y::{contrast_ratio, relative_luminance};
    for theme in [Theme::dark(), Theme::light()] {
        let name = theme.name.clone();
        let letterbox = theme.tokens.colors.media.letterbox;
        assert!(
            relative_luminance(letterbox) < 0.02,
            "{name}: the letterbox turned grey"
        );
        let panel = crate::chrome::panel_ground(&theme);
        let on_panel = theme.on_ground(panel);
        assert!(
            contrast_ratio(on_panel.text_primary, panel) >= 4.5,
            "{name}: controls ink on the media Panel"
        );
        let mut vp = VideoPlayer::new();
        vp.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
        vp.show_controls();
        draw_once_with_theme(&mut vp, &theme);
        let bar = vp.controls_bar_rect(&theme);
        let inset = f64::from(theme.tokens.spacing.s2);
        assert!(
            (bar.x0 - inset).abs() < 1e-9 && (vp.rect.y1 - bar.y1 - inset).abs() < 1e-9,
            "{name}: the controls float as a Panel inside the picture"
        );
        assert!(
            vp.play_button_rect.width() >= f64::from(theme.tokens.metrics.min_pointer_target),
            "{name}: the play control is a full pointer target"
        );
    }
}

#[test]
fn controls_bar_lays_out_reachable_aligned_targets_at_every_size() {
    {
        let mut regular = VideoPlayer::new();
        regular.rect = Rect::new(0.0, 0.0, 390.0, 844.0);
        regular.reveal_controls();
        regular.set_setting(PlayerSetting::toggle(
            "captions",
            Icon::FileText,
            "Captions",
            2,
            false,
        ));
        let one_x = Theme::dark();
        draw_once_with_theme(&mut regular, &one_x);
        assert_eq!(
            regular.controls_bar_rect(&one_x).height(),
            CONTROLS_BAR_HEIGHT
        );
        assert_eq!(
            regular.scrub_track_rect.center().y,
            regular.play_button_rect.center().y
        );

        let mut accessible = VideoPlayer::new();
        accessible.rect = regular.rect;
        accessible.reveal_controls();
        accessible.set_setting(PlayerSetting::toggle(
            "captions",
            Icon::FileText,
            "Captions",
            2,
            false,
        ));
        let two_x = Theme::dark().with_text_scale(2.0);
        draw_once_with_theme(&mut accessible, &two_x);
        let bar = accessible.controls_bar_rect(&two_x);
        assert_eq!(bar.height(), CONTROLS_BAR_HEIGHT * 2.0);
        assert!(accessible.scrub_track_rect.width() > 350.0);
        assert!(accessible.scrub_track_rect.center().y < accessible.play_button_rect.y0);
        assert!(bar.contains(accessible.play_button_rect.center()));
        assert!(bar.contains(accessible.fullscreen_button_rect.center()));
        assert!(bar.contains(accessible.gear_button_rect.center()));
        assert!(accessible.setting_icon_rects.is_empty());
        assert!(accessible.setting_in_gear("captions"));
    }

    {
        let mut vp = VideoPlayer::new();
        vp.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
        vp.show_controls();
        vp.set_setting(PlayerSetting::toggle(
            "captions",
            Icon::FileText,
            "Captions",
            2,
            false,
        ));
        vp.set_setting(PlayerSetting::choice(
            "resolution",
            Icon::Monitor,
            "Resolution",
            0,
            vec!["1080p".into(), "720p".into()],
            0,
        ));
        draw_once(&mut vp);
        for (name, r) in bar_targets(&vp) {
            assert!(
                r.width() >= MIN_TARGET - 1e-6 && r.height() >= MIN_TARGET - 1e-6,
                "{name} hit rect {r:?} is under the {MIN_TARGET}px minimum target"
            );
        }
    }

    {
        let mut vp = VideoPlayer::new();
        vp.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
        vp.show_controls();
        vp.set_setting(PlayerSetting::toggle(
            "captions",
            Icon::FileText,
            "Captions",
            2,
            false,
        ));
        draw_once(&mut vp);
        let bar = vp.controls_bar_rect(&Theme::dark());
        let mid_y = bar.center().y;
        let targets = bar_targets(&vp);
        for (name, r) in &targets {
            assert!(
                (r.center().y - mid_y).abs() < 1e-6,
                "{name} centre {} is off the bar's centre line {mid_y}",
                r.center().y
            );
        }
        for (i, (a_name, a)) in targets.iter().enumerate() {
            for (b_name, b) in targets.iter().skip(i + 1) {
                let overlap = a.intersect(*b);
                assert!(
                    overlap.is_zero_area(),
                    "{a_name} {a:?} and {b_name} {b:?} claim the same pixels: {overlap:?}"
                );
            }
        }
    }

    // Case `time_label_yields_to_the_scrubber_rather_than_overlapping_controls`.
    // The time label is the one thing in the bar that is not a pointer
    // target, and it used to be the one thing allowed to overlap them: its
    // left edge was clamped to the play button while its right end ran
    // under the volume control. Now it yields — position only, then
    // nothing — so at every width and text scale the scrubber keeps its
    // floor and no control is painted over.
    {
        for (width, scale) in [(300.0, 1.0), (300.0, 2.0), (960.0, 2.0), (420.0, 1.0)] {
            let mut vp = VideoPlayer::new();
            vp.rect = Rect::new(0.0, 0.0, width, 200.0);
            vp.show_controls();
            vp.duration_ms = 2 * 60 * 60 * 1000;
            vp.position_ms = 60 * 60 * 1000 + 8000;
            draw_at_text_scale(&mut vp, scale);
            assert!(
                vp.scrub_track_rect.width() >= MIN_SCRUB_WIDTH - 1e-6,
                "at {width}px / {scale}x the scrubber ({:?}) lost its floor",
                vp.scrub_track_rect
            );
            let label = vp.time_label_rect;
            for (name, r) in bar_targets(&vp) {
                let overlap = label.intersect(r);
                assert!(
                    overlap.is_zero_area(),
                    "at {width}px / {scale}x the time label {label:?} paints over {name} {r:?}"
                );
            }
        }
    }

    // Case `a_wide_bar_paints_the_full_time_label`.
    // The degrade is a last resort: on a bar with room the full
    // "m:ss / m:ss" is painted, exactly as wide as it measures.
    {
        let mut vp = VideoPlayer::new();
        vp.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
        vp.show_controls();
        vp.duration_ms = 120_000;
        vp.position_ms = 8_000;
        draw_once(&mut vp);
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let full = format!(
            "{} / {}",
            format_ms(vp.position_ms),
            format_ms(vp.duration_ms)
        );
        let (tw, _) = tr.measure_style(&full, None, &theme.text(TextRole::Caption));
        assert!(
            (vp.time_label_rect.width() - f64::from(tw)).abs() < 1e-6,
            "label rect {:?} is not the measured full label ({tw})",
            vp.time_label_rect
        );
        assert!(
            (vp.time_label_rect.center().y - vp.controls_bar_rect(&Theme::dark()).center().y).abs()
                < 1e-6,
            "the label left the bar's centre line"
        );
    }

    {
        let mut vp = VideoPlayer::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map = std::collections::HashMap::new();
        let theme = Theme::dark();
        let node = vp.build_layout(&mut taffy, &theme, &mut node_map);
        let style = taffy.style(node).unwrap();
        let default: taffy::Style = taffy::Style::default();
        assert_eq!(
            format!("{:?}", style.padding),
            format!("{:?}", default.padding),
            "no forced internal padding — embeds edge-to-edge"
        );
        assert_eq!(
            format!("{:?}", style.margin),
            format!("{:?}", default.margin),
            "no forced margin"
        );
        assert_eq!(style.flex_grow, 1.0, "the player fills its slot");
    }
}

#[test]
fn settings_surface_by_priority_and_round_trip_through_the_gear() {
    {
        let mut vp = VideoPlayer::new();
        vp.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
        vp.reveal_controls();
        // A high-priority toggle (own icon) + a priority-0 choice (under the gear).
        vp.set_setting(PlayerSetting::toggle(
            "captions",
            Icon::FileText,
            "Captions",
            2,
            false,
        ));
        vp.set_setting(PlayerSetting::choice(
            "resolution",
            Icon::Monitor,
            "Resolution",
            0,
            vec!["1080p".into(), "720p".into(), "480p".into()],
            0,
        ));
        draw_once(&mut vp);
        assert!(
            vp.setting_icon_rect("captions").is_some(),
            "priority-2 setting surfaces as its own bar icon"
        );
        assert!(
            vp.setting_in_gear("resolution"),
            "priority-0 setting lives under the settings gear"
        );
        assert!(
            !vp.setting_in_gear("captions"),
            "the promoted icon is not also in the gear"
        );
    }

    {
        let mut vp = VideoPlayer::new();
        // Narrow bar + many priority settings ⇒ only the top few fit as icons.
        vp.rect = Rect::new(0.0, 0.0, 420.0, 236.0);
        vp.reveal_controls();
        for p in 1..=8 {
            vp.set_setting(PlayerSetting::button(
                format!("act{p}"),
                Icon::Star,
                format!("Action {p}"),
                p,
            ));
        }
        draw_once(&mut vp);
        let icon_prios: Vec<i32> = vp
            .setting_icon_rects
            .iter()
            .map(|(i, _)| vp.settings[*i].priority)
            .collect();
        let overflow_prios: Vec<i32> = vp
            .gear_entries
            .iter()
            .map(|&i| vp.settings[i].priority)
            .collect();
        assert!(!icon_prios.is_empty(), "some settings fit as icons");
        assert!(
            !overflow_prios.is_empty(),
            "the rest overflow into the gear at this width"
        );
        let min_icon = *icon_prios.iter().min().unwrap();
        let max_overflow = *overflow_prios.iter().max().unwrap();
        assert!(
            min_icon >= max_overflow,
            "every shown icon outranks every overflowed setting ({min_icon} >= {max_overflow})"
        );
    }

    {
        let mut vp = VideoPlayer::new();
        vp.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
        vp.reveal_controls();
        vp.set_setting(PlayerSetting::choice(
            "resolution",
            Icon::Monitor,
            "Resolution",
            2, // its own bar icon
            vec!["1080p".into(), "720p".into(), "480p".into()],
            0,
        ));
        draw_once(&mut vp);
        let icon = vp
            .setting_icon_rect("resolution")
            .expect("resolution has a bar icon");
        dispatch(&mut vp, &cursor_moved(icon.center().x, icon.center().y));
        assert!(dispatch(&mut vp, &mouse_press()), "icon click handled");
        assert!(
            vp.menu_open(),
            "clicking the resolution icon opened its option list"
        );
        draw_once(&mut vp);
        let row = vp
            .menu_hit_rects
            .iter()
            .find(|(h, _)| matches!(h, MenuHit::Option(_, 1)))
            .map(|(_, r)| *r)
            .expect("720p option row laid out");
        dispatch(&mut vp, &cursor_moved(row.center().x, row.center().y));
        assert!(dispatch(&mut vp, &mouse_press()), "option click handled");
        let events = vp.take_setting_events();
        assert_eq!(
            events,
            vec![PlayerSettingEvent::Chose {
                id: "resolution".into(),
                index: 1
            }],
            "picking 720p emits a Chose event the host drains"
        );
        match &vp.setting("resolution").unwrap().control {
            PlayerSettingControl::Choice { selected, .. } => assert_eq!(*selected, 1),
            _ => panic!("resolution is a choice"),
        }
        assert!(!vp.menu_open(), "picking an option closes the popover");
    }

    {
        let mut vp = VideoPlayer::new();
        vp.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
        vp.reveal_controls();
        // Priority 0 ⇒ under the gear.
        vp.set_setting(PlayerSetting::toggle(
            "autoplay",
            Icon::Play,
            "Autoplay",
            0,
            false,
        ));
        draw_once(&mut vp);
        let gear = vp.gear_button_rect;
        assert!(gear.width() > 0.0, "gear present for the tucked setting");
        dispatch(&mut vp, &cursor_moved(gear.center().x, gear.center().y));
        assert!(dispatch(&mut vp, &mouse_press()), "gear click handled");
        assert!(vp.menu_open(), "gear opened the menu");
        draw_once(&mut vp);
        let row = vp
            .menu_hit_rects
            .iter()
            .find(|(h, _)| matches!(h, MenuHit::Setting(_)))
            .map(|(_, r)| *r)
            .expect("autoplay row laid out under the gear");
        dispatch(&mut vp, &cursor_moved(row.center().x, row.center().y));
        assert!(dispatch(&mut vp, &mouse_press()), "row click handled");
        let events = vp.take_setting_events();
        assert_eq!(
            events,
            vec![PlayerSettingEvent::Toggled {
                id: "autoplay".into(),
                on: true
            }],
            "toggling the gear row flips it on and emits"
        );
    }
}

#[test]
fn accessibility_exposes_transport_state_and_truthful_controls() {
    {
        use crate::a11y::Role;
        let mut vp = VideoPlayer::new();
        let node = Widget::accessibility(&vp).expect("player is not presentational");
        assert_eq!(node.role, Role::Group);
        assert_eq!(node.name.as_deref(), Some("Video player"));
        assert!(!node.state.busy);
        vp.play();
        assert!(Widget::accessibility(&vp).unwrap().state.busy);
    }

    {
        use crate::a11y::{Action, Role};
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote {
            url: "https://x/v.mp4".into(),
        });
        let mut vp = VideoPlayer::new().with_engine(engine);
        vp.rect = Rect::new(0.0, 0.0, 760.0, 480.0);
        vp.reveal_controls();
        draw_once(&mut vp);

        let node = Widget::accessibility(&vp).unwrap();
        let names: Vec<_> = node
            .children
            .iter()
            .filter_map(|child| child.name.as_deref())
            .collect();
        assert!(names.contains(&"Play"), "{names:?}");
        assert!(names.contains(&"Playback position"), "{names:?}");
        assert!(names.contains(&"Mute"), "{names:?}");
        assert!(names.contains(&"Volume"), "{names:?}");
        assert!(names.contains(&"Enter fullscreen"), "{names:?}");
        assert!(node.children.iter().all(|child| child.bounds.is_some()));
        assert!(node.children.iter().any(|child| child.role == Role::Slider));

        assert!(Widget::perform_semantic_action(
            &mut vp,
            "transport.play-pause",
            &Action::Activate,
            &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert!(vp.is_playing());
        assert!(Widget::perform_semantic_action(
            &mut vp,
            "audio.volume",
            &Action::SetValue("25%".into()),
            &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert!((vp.volume() - 0.25).abs() < 1e-6);
    }

    {
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote {
            url: "https://x/v.mp4".into(),
        });
        let mut vp = VideoPlayer::new().with_engine(engine);
        vp.set_transport_enabled(false);
        vp.toggle_play();
        assert!(!vp.is_playing());
        vp.seek_to_ms(30_000);
        assert_eq!(vp.position_ms(), 0);
        vp.set_volume(0.3);
        assert!((vp.volume() - 0.3).abs() < 1e-6);
    }
}

/// Push mode through a real encoder and decoder. Needs the `realtime`
/// feature of `media` (GStreamer); VP9 in software needs no GPU.
#[cfg(feature = "realtime-video")]
#[test]
fn push_mode_feed_encoded_updates_latest_image() {
    let (w, h) = (64u32, 48u32);
    let mut config = media::EncoderConfig::new(media::StreamCodec::Vp9, w, h, 30);
    config.policy = media::realtime::RealtimePolicy::SoftwareOnly;
    let mut enc = media::RealtimeEncoder::new(&config).expect("VP9 software encoder");
    let rgba = vec![128u8; (w * h * 4) as usize];
    let unit = enc
        .encode(media::InputFrame::Packed(&rgba), std::time::Duration::ZERO)
        .expect("encode grey frame")
        .remove(0);

    let mut vp = VideoPlayer::new();
    assert!(
        vp.feed_encoded(media::StreamCodec::Vp9, &unit.data),
        "decoder consumed the frame"
    );
    let frame = vp.latest_frame().expect("a frame");
    assert_eq!((frame.width, frame.height), (w, h));
}

#[test]
fn surface_paints_pushed_frames_or_a_placeholder() {
    {
        let mut vp = VideoPlayer::new();
        assert!(!vp.feed_frame(&[]));
        assert!(vp.latest_frame().is_none());
    }

    {
        use vello_encoding::DrawTag;
        let mut vp = VideoPlayer::new();
        vp.rect = Rect::new(0.0, 0.0, 320.0, 240.0);
        assert!(vp.latest_frame().is_none());
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        let image_count = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::IMAGE)
            .count();
        assert_eq!(
            image_count, 0,
            "no frame → no image draw in the empty state"
        );
        // Only the letterbox: the app's empty state or error says what is
        // missing, and no glyph is painted under its text.
        assert_eq!(
            scene.encoding().draw_tags.len(),
            1,
            "an empty player paints its letterbox and nothing else"
        );
    }

    {
        use vello_encoding::DrawTag;
        let mut vp = VideoPlayer::new();
        vp.rect = Rect::new(0.0, 0.0, 64.0, 48.0);
        vp.latest_frame = Some(rgba_image(16, 16, Arc::new(vec![255u8; 4 * 16 * 16])));
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        let image_count = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::IMAGE)
            .count();
        assert!(
            image_count >= 1,
            "expected an IMAGE draw, got {image_count}"
        );
    }
}

/// Every pointer target the bar exposes, named, in one place.
fn bar_targets(vp: &VideoPlayer) -> Vec<(String, Rect)> {
    let mut out = vec![
        ("play".to_string(), vp.play_button_rect),
        ("scrub".to_string(), vp.scrub_track_rect),
        ("volume".to_string(), vp.volume_track_rect),
        ("mute".to_string(), vp.mute_button_rect),
        ("fullscreen".to_string(), vp.fullscreen_button_rect),
    ];
    if vp.gear_button_rect.width() > 0.0 {
        out.push(("gear".to_string(), vp.gear_button_rect));
    }
    for (idx, r) in &vp.setting_icon_rects {
        out.push((format!("setting-{idx}"), *r));
    }
    out
}

/// Paint once with the renderer at `scale` (the a11y text-scale
/// convention the `Host` palette source uses), so the label is measured
/// and drawn larger than the theme's type size says.
fn draw_at_text_scale(vp: &mut VideoPlayer, scale: f32) {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    tr.set_text_scale(scale);
    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
}

#[test]
fn controls_ink_and_edges_read_on_both_presets() {
    {
        use crate::offscreen::OffscreenRenderer;
        let theme = Theme::light();
        let mut vp = VideoPlayer::new();
        let (w, h) = (320.0, 120.0);
        vp.rect = Rect::new(0.0, 0.0, w, h);
        vp.show_controls();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a 320x120 test surface"
        )]
        let rgba = OffscreenRenderer::new(w as u32, h as u32)
            .render(&scene, theme.tokens.colors.surfaces.content);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "pixel coordinates inside a 320x120 surface"
        )]
        let sample = |p: Point| {
            let idx = ((p.y as usize) * (w as usize) + p.x as usize) * 4;
            Color::from_rgb8(rgba[idx], rgba[idx + 1], rgba[idx + 2])
        };
        // Inside the paused play triangle: for a glyph box of side `s` the
        // triangle spans x ∈ [cx − 0.5s·½ … cx + 0.7s·½]; (cx − 4, cy) is
        // well inside it at the 30px glyph.
        let c = vp.play_button_rect.center();
        let glyph = sample(Point::new(c.x - 4.0, c.y));
        // A bare patch of the painted scrim: just right of the play target,
        // above the scrub track, inside the bar.
        let bar = vp.controls_bar_rect(&Theme::dark());
        let scrim = sample(Point::new(vp.play_button_rect.x1 + 2.0, bar.y0 + 3.0));
        let ratio = crate::a11y::contrast_ratio(glyph, scrim);
        assert!(
            ratio >= 4.5,
            "play glyph contrast {ratio:.2}:1 against the painted scrim on the light theme \
         (WCAG AA is 4.5:1): near-black text_primary on the dark scrim; ink must be \
         chosen against the scrim"
        );
    }

    // Case `ux1x_controls_bar_and_menu_edges_register_on_both_presets`.
    // The controls bar's top hairline, its unfilled track and the open
    // settings menu keep a **visible derived edge** at 1× on both presets
    // (HIG §9.5). Before, all three wore `border_standard` — one fixed
    // colour per theme, measured against neither theme's scrim, three
    // shades from the dark raised menu — and the light theme's track was
    // `text_dim` (near-black) on a near-black scrim.
    //
    // Writes `$UX1X_F4_OUT/<theme>/video_player.png` plus the harness
    // inspection when that variable is set; asserts regardless.
    {
        use crate::offscreen::OffscreenRenderer;
        const EDGE_MIN: f64 = 0.008;
        let (w, h) = (640.0, 360.0);
        // Evidence first, for both presets, so a failing assertion on one
        // preset still leaves the other's PNG and inspection on disk.
        for theme in [Theme::dark(), Theme::light()] {
            let theme = theme.with_reduce_motion(true);
            if let Some(png) = ux1x_out(&theme, "video_player.png") {
                let harness = crate::headless::FrameHarness::new_with_theme(
                    crate::State::new(),
                    Box::new(ux1x_fixture()),
                    (640, 360),
                    theme.clone(),
                );
                harness.render_png(&png);
                if let Ok(json) = harness.inspection_json() {
                    std::fs::write(png.with_extension("inspection.json"), json)
                        .expect("write inspection");
                }
            }
        }
        for theme in [Theme::dark(), Theme::light()] {
            let theme = theme.with_reduce_motion(true);
            let mut vp = ux1x_fixture();
            vp.rect = Rect::new(0.0, 0.0, w, h);
            let mut tr = TextRenderer::new();
            let presence = PresenceCtx::default();
            let mut scene = Scene::new();
            vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a 640x360 test surface"
            )]
            let rgba = OffscreenRenderer::new(w as u32, h as u32)
                .render(&scene, theme.tokens.colors.surfaces.content);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a 640x360 test surface"
            )]
            let lum = |x: f64, y: f64| lum_at(&rgba, w as u32, x, y);

            // Controls Panel: its edge against its ground just inside it.
            let bar = vp.controls_bar_rect(&theme);
            let edge = lum(bar.x0, bar.center().y);
            let ground = lum(bar.x0 + 4.0, bar.center().y);
            assert!(
                (edge - ground).abs() >= EDGE_MIN,
                "{}: controls Panel edge {edge:.4} vs ground {ground:.4} did not register",
                theme.name
            );

            // Track: the unfilled end (past the 40 % buffered range) against
            // the bare scrim beside it.
            let hit = vp.scrub_track_rect;
            let track = lum(hit.x1 - 3.0, hit.center().y);
            let beside = lum(hit.x1 - 3.0, hit.center().y - 7.0);
            assert!(
                (track - beside).abs() >= EDGE_MIN,
                "{}: unfilled track {track:.4} vs scrim {beside:.4} did not register",
                theme.name
            );

            // Menu: the panel edge (the row hit rects sit 3 px inside the
            // panel) against the panel ground just inside it.
            let first_row = vp
                .menu_hit_rects
                .first()
                .map(|(_, rect)| *rect)
                .expect("the open menu painted rows");
            let panel_x0 = first_row.x0 - 3.0;
            let edge = lum(panel_x0, first_row.center().y);
            let ground = lum(panel_x0 + 2.0, first_row.center().y);
            assert!(
                (edge - ground).abs() >= EDGE_MIN,
                "{}: menu edge {edge:.4} vs panel {ground:.4} did not register",
                theme.name
            );
        }
    }
}

/// Relative luminance of the composited pixel at (`x`, `y`).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "probe coordinates are non-negative pixel positions inside a small test surface"
)]
fn lum_at(rgba: &[u8], w: u32, x: f64, y: f64) -> f64 {
    let (r, g, b, _) =
        crate::offscreen::OffscreenRenderer::sample(rgba, w, x.floor() as u32, y.floor() as u32);
    crate::a11y::relative_luminance(Color::from_rgb8(r, g, b))
}

/// `$UX1X_F4_OUT/<theme>/<name>` when the 1× review asked for evidence.
fn ux1x_out(theme: &Theme, name: &str) -> Option<std::path::PathBuf> {
    let dir =
        std::path::PathBuf::from(std::env::var_os("UX1X_F4_OUT")?).join(theme.name.to_lowercase());
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(name))
}

/// A player with the controls shown, a partly buffered track and the
/// settings menu open on an expanded choice — every surface the bar
/// paints, in one frame.
fn ux1x_fixture() -> VideoPlayer {
    let mut vp = VideoPlayer::new();
    vp.buffering = 40;
    vp.set_setting(PlayerSetting::toggle(
        "captions",
        Icon::FileText,
        "Captions",
        0,
        true,
    ));
    vp.set_setting(PlayerSetting::choice(
        "speed",
        Icon::Settings2,
        "Speed",
        0,
        vec!["0.5×".into(), "1×".into(), "1.5×".into()],
        1,
    ));
    vp.show_controls();
    vp.open_settings_menu(Some("speed"));
    vp
}

/// A page's video shows each new picture as a fresh image the renderer
/// copies from the media element: no pixels pass through the widget,
/// and each picture is one registry entry, so the work per picture is a
/// GPU copy and a repaint of the video's rect.
#[test]
fn element_pictures_reach_the_renderer_without_pixels() {
    let mut vp = VideoPlayer::new();
    vp.rect = Rect::new(0.0, 0.0, 320.0, 180.0);
    let _ = crate::element_pictures::take();
    let element: std::rc::Rc<dyn std::any::Any> = std::rc::Rc::new(());
    const PICTURES: u64 = 12;
    let mut ids = std::collections::BTreeSet::new();
    for serial in 1..=PICTURES {
        vp.dirty = false;
        vp.show_element_picture(media::ElementPicture {
            width: 1920,
            height: 1080,
            serial,
            element: std::rc::Rc::clone(&element),
        });
        assert!(vp.dirty, "a new picture repaints the video");
        let image = vp.latest_frame().expect("the picture's image");
        assert!(image.data.is_empty(), "no pixels are read back");
        assert_eq!((image.width, image.height), (1920, 1080));
        let shown = crate::element_pictures::take();
        assert_eq!(shown.len(), 1, "one copy per picture");
        assert_eq!(shown[0].image.data.id(), image.data.id());
        assert!(ids.insert(image.data.id()), "each picture its own image");
    }
    assert_eq!(ids.len(), PICTURES as usize);
}
