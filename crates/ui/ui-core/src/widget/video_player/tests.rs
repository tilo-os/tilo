use super::*;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use automerge::AutoCommit;

use crate::synthetic_input::cursor_moved;

use crate::synthetic_input::mouse_press;
use crate::testing::dispatch;
use winit::keyboard::{Key, NamedKey};

tilo_test_env::scenario! {
    fn a_video_player_drives_and_follows_its_engine;

    phase transport_keys_and_surface_clicks_drive_the_engine;
    {
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
            vp.draw(&mut crate::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme, presence.draw_env()));
            assert!(!vp.is_playing());
            dispatch(&mut vp, &cursor_moved(320.0, 40.0));
            assert!(dispatch(&mut vp, &mouse_press()), "surface click handled");
            assert!(vp.is_playing(), "surface click started playback");
            dispatch(&mut vp, &mouse_press());
            assert!(!vp.is_playing(), "a second surface click paused it");
        }
    }

    phase scrub_track_seeks_where_it_is_painted_and_shows_sponsor_bands;
    {
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
            vp.draw(&mut crate::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme, presence.draw_env()));
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
            vp.draw(&mut crate::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme, presence.draw_env()));
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
                vp.draw(&mut crate::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme, presence.draw_env()));
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
            vp.draw(&mut crate::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme, presence.draw_env()));
            assert!(
                scene.encoding().draw_tags.len() > baseline,
                "two sponsor bands add fills to the timeline ({} vs {baseline})",
                scene.encoding().draw_tags.len(),
            );
        }
    }

    phase fullscreen_intent_toggles_and_requests_the_largest_output;
    {
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

    phase the_engines_system_controls_hear_what_plays_and_the_player_follows_them;
    {
        let mut vp = VideoPlayer::new().with_engine(media::MediaEngine::new_synthesized());
        vp.set_now_playing(NowPlaying {
            track_id: "yt:abc123".into(),
            title: "Runaway".into(),
            artist: "Aurora".into(),
            art_url: String::new(),
        });
        let described = vp
            .engine()
            .and_then(media::MediaEngine::now_playing)
            .cloned();
        assert_eq!(
            described,
            Some(media::NowPlaying {
                title: "Runaway".into(),
                artist: "Aurora".into(),
                ..media::NowPlaying::default()
            })
        );
        vp.play();
        vp.engine_mut()
            .unwrap()
            .press_now_playing(media::NowPlayingAction::Pause);
        vp.pump_media_controls();
        assert_ne!(
            vp.state,
            PlaybackState::Playing,
            "a pause there pauses here"
        );
    }
}

tilo_test_env::scenario! {
    fn a_video_players_controls_show_hide_and_lay_out;

    phase a_playing_video_shows_its_controls_then_a_progress_line_and_a_paused_one_keeps_them;
    {
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

    phase a_reveal_reaches_the_screen_even_when_the_next_frame_comes_after_its_deadline;
    {
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
        vp.controls_visible_until = None;
        draw_once(&mut vp);
        assert_eq!(
            listed_controls(&vp),
            Vec::<String>::new(),
            "hidden while playing"
        );

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



    phase settings_surface_by_priority_and_round_trip_through_the_gear;
    {
        {
            let mut vp = VideoPlayer::new();
            vp.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
            vp.reveal_controls();
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

    phase accessibility_exposes_transport_state_and_truthful_controls;
    {
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


}

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

fn draw_once(vp: &mut VideoPlayer) {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    vp.draw(&mut crate::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        presence.draw_env(),
    ));
}

/// A German shell plays, mutes, seeks and fullscreens in German: the transport's tooltips and
/// what a reader hears come from the locale catalogue, never from copy compiled into the widget.
/// The source copy still reads in English.
#[test]
fn a_german_shell_hears_the_transport_in_german() {
    use crate::i18n::I18nManager;
    const EN: &str = include_str!("../../../locales/en-US.ftl");
    const DE: &str = include_str!("../../../locales/de-DE.ftl");
    I18nManager::install("en-US", "de-DE", &[("en-US", EN), ("de-DE", DE)]).unwrap();

    let mut engine = media::MediaEngine::new_synthesized();
    engine.open(media::Source::Remote {
        url: "https://x/v.mp4".into(),
    });
    let mut vp = VideoPlayer::new().with_engine(engine);
    vp.rect = Rect::new(0.0, 0.0, 760.0, 480.0);
    vp.reveal_controls();
    draw_once(&mut vp);

    fn hover(vp: &mut VideoPlayer, target: Rect) -> Option<String> {
        dispatch(vp, &cursor_moved(target.center().x, target.center().y));
        Widget::tooltip(vp).map(std::borrow::Cow::into_owned)
    }
    let (play, scrub, mute, volume, fullscreen) = (
        vp.play_button_rect,
        vp.scrub_track_rect,
        vp.mute_button_rect,
        vp.volume_track_rect,
        vp.fullscreen_button_rect,
    );
    assert_eq!(
        hover(&mut vp, play).as_deref(),
        Some("Wiedergeben"),
        "the play control's tooltip speaks German"
    );
    assert_eq!(hover(&mut vp, scrub).as_deref(), Some("Wiedergabeposition"));
    assert_eq!(hover(&mut vp, mute).as_deref(), Some("Stummschalten"));
    assert_eq!(hover(&mut vp, volume).as_deref(), Some("Lautstärke"));
    assert_eq!(
        hover(&mut vp, fullscreen).as_deref(),
        Some("Vollbild öffnen"),
        "entering fullscreen is named, not the state"
    );

    let node = Widget::accessibility(&vp).expect("the player is not presentational");
    assert_eq!(node.name.as_deref(), Some("Videoplayer"));
    assert_eq!(
        node.description.as_deref(),
        Some(
            "Leertaste gibt wieder oder pausiert; Pfeil links und rechts springen; Pfeil hoch und runter ändern die Lautstärke; M schaltet stumm; F schaltet Vollbild"
        ),
        "the keys the player teaches are spoken in German too"
    );
    let names: Vec<&str> = node
        .children
        .iter()
        .filter_map(|child| child.name.as_deref())
        .collect();
    for said in [
        "Wiedergeben",
        "Wiedergabeposition",
        "Stummschalten",
        "Lautstärke",
        "Vollbild öffnen",
    ] {
        assert!(names.contains(&said), "{said} is missing from {names:?}");
    }
    let seek = node
        .children
        .iter()
        .find(|child| child.name.as_deref() == Some("Wiedergabeposition"))
        .expect("the seek control");
    let value = seek.value.as_deref().unwrap_or_default();
    assert!(
        value.contains(" von "),
        "the position reads in German: {value:?}"
    );
    let keys: Vec<&str> = node
        .children
        .iter()
        .filter_map(|child| child.description.as_deref())
        .collect();
    assert!(keys.contains(&"Leertaste"), "{keys:?}");

    vp.play();
    assert_eq!(hover(&mut vp, play).as_deref(), Some("Pausieren"));
    vp.toggle_mute();
    assert_eq!(hover(&mut vp, mute).as_deref(), Some("Ton einschalten"));
    vp.toggle_fullscreen();
    assert_eq!(
        hover(&mut vp, fullscreen).as_deref(),
        Some("Vollbild verlassen")
    );

    I18nManager::init("en-US");
    assert_eq!(
        hover(&mut vp, scrub).as_deref(),
        Some("Playback position"),
        "an English shell reads the source catalogue"
    );
}

#[test]
fn a_live_stream_has_no_place_to_scrub_and_its_jump_goes_to_its_edge() {
    let mut engine = media::MediaEngine::new_synthesized();
    engine.open(media::Source::Remote {
        url: "https://x/live.m3u8".into(),
    });
    let mut vp = VideoPlayer::new().with_engine(engine);
    vp.set_live(true);
    assert!(vp.is_live(), "the player holds a live source as live");

    let doc = AutoCommit::new();
    let root = automerge::ROOT;
    vp.sync(&doc, &root);
    let edge = vp.duration_ms();
    assert!(
        edge > 0,
        "the synthesized stream has a length to stand its edge at"
    );

    vp.seek_to_ms(0);
    vp.sync(&doc, &root);
    assert_eq!(
        vp.position_ms(),
        edge,
        "a jump on a live stream goes to its live edge, not to the start of a recording"
    );
}

tilo_test_env::scenario! {
    fn the_player_offers_speeds_captions_and_their_shortcuts;

    phase choosing_a_speed_or_a_caption_track_reports_the_setting_that_changed;
    {
        let mut vp = VideoPlayer::new().with_engine(media::MediaEngine::new_synthesized());
        vp.set_setting(PlayerSetting::choice(
            "playback-rate",
            Icon::Gauge,
            "Playback speed",
            0,
            vec!["0.5×".into(), "1×".into(), "2×".into()],
            1,
        ));
        vp.set_setting(PlayerSetting::choice(
            "captions",
            Icon::Captions,
            "Captions",
            0,
            vec!["Off".into(), "English".into()],
            0,
        ));
        assert!(vp.step_choice("playback-rate", true));
        assert!(vp.step_choice("captions", true));
        assert_eq!(
            vp.take_setting_events(),
            [
                PlayerSettingEvent::Chose {
                    id: "playback-rate".into(),
                    index: 2
                },
                PlayerSettingEvent::Chose {
                    id: "captions".into(),
                    index: 1
                },
            ]
        );
        assert!(
            !vp.step_choice("playback-rate", true),
            "the fastest speed is the last"
        );
    }

    phase the_keys_step_through_captions_and_speed;
    {
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote {
            url: "https://x/v.mp4".into(),
        });
        let mut vp = VideoPlayer::new().with_engine(engine);
        vp.set_setting(PlayerSetting::choice(
            "playback-rate",
            Icon::Gauge,
            "Playback speed",
            0,
            vec!["0.5×".into(), "1×".into(), "2×".into()],
            1,
        ));
        vp.set_setting(PlayerSetting::choice(
            "captions",
            Icon::Captions,
            "Captions",
            0,
            vec!["Off".into(), "English".into()],
            0,
        ));
        vp.set_focus(true);
        assert!(vp.handle_key_input(&Key::Character("c".into()), ElementState::Pressed));
        assert!(vp.handle_key_input(&Key::Character(">".into()), ElementState::Pressed));
        assert!(vp.handle_key_input(&Key::Character("<".into()), ElementState::Pressed));
        assert_eq!(
            vp.take_setting_events().len(),
            3,
            "c, > and < each chose a setting"
        );
    }

    phase the_playhead_speaks_the_cue_it_is_inside;
    {
        let mut vp = VideoPlayer::new();
        vp.set_captions(Some(
            "WEBVTT\n\n00:00:01.000 --> 00:00:04.000\nHello\n\n00:00:04.000 --> 00:00:06.000\nAgain\n",
        ));
        assert_eq!(vp.captions().len(), 2);
        vp.seek_to_ms(2_000);
        assert_eq!(
            active_cue(vp.captions(), vp.position_ms()).map(|cue| cue.text.as_str()),
            Some("Hello")
        );
        vp.set_captions(None);
        assert!(vp.captions().is_empty(), "off shows no words");
    }
}

/// Over the picture the letterbox stays black and the controls' ink reads on their panel, in
/// the light theme and the dark.
#[test]
fn over_media_chrome_is_legible_and_dark_in_every_theme() {
    use crate::a11y::{contrast_ratio, relative_luminance};
    for theme in [Theme::dark(), Theme::light()] {
        let letterbox = theme.tokens.colors.media.letterbox;
        assert!(
            relative_luminance(letterbox) < 0.02,
            "{}: the letterbox turned grey",
            theme.name
        );
        let panel = crate::chrome::panel_ground(&theme);
        assert!(
            contrast_ratio(theme.on_ground(panel).text_primary, panel) >= 4.5,
            "{}: the controls' ink reads on the media panel",
            theme.name
        );
    }
}
