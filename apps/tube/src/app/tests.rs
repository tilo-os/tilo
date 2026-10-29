//! The window's suite: what the shell builds for each screen, what the
//! controls do to the model and the shared player, and what the window says
//! when the network, the library or the video itself refuses.

use super::*;
use crate::api::{Request, Update, Worker};
use crate::model::Category;
use automerge::{AutoCommit, ReadDoc};
use ui::tilo_resource::Resource as _;

fn fresh() -> (TubeApp, AutoCommit) {
    (TubeApp::sample(), AutoCommit::new())
}

#[test]
fn the_demo_page_shows_the_sample_captions_over_the_picture() {
    let app = TubeApp::sample();
    app.tube().stage_scenario("play-demo");
    let model = app.model_for_test();
    assert_eq!(model.captions_of, crate::sample::SAMPLE_CAPTIONED_VIDEO);
    assert!(!model.captions.is_empty(), "the still shows captions");
    assert!(
        crate::captions::active_cue(&model.captions, 45_000).is_some(),
        "the playhead the still shows is inside the first line"
    );
    assert_eq!(model.caption_tracks.len(), 2);
    assert_eq!(model.settings.caption_language.as_deref(), Some("en"));
}

#[test]
fn a_half_watched_video_offers_where_it_was_left() {
    let model = crate::sample::model();
    assert_eq!(
        model.resume_at_ms("vid_black_holes"),
        Some(246_000),
        "the sample history remembers the minute it stopped at"
    );
    assert_eq!(
        model.resume_at_ms(crate::sample::SAMPLE_CAPTIONED_VIDEO),
        None,
        "a video never left part way has nothing to continue"
    );
}

/// A fresh app whose player uses the deterministic **synthesized** media
/// engine, so the transport-wiring tests behave identically whether or not
/// the `media` (real `GStreamer`) backend is compiled in — a real pipeline
/// can't preroll/seek the fake test URLs, so its position would stay 0.
fn fresh_synth() -> (TubeApp, AutoCommit) {
    let (app, doc) = fresh();
    app.tube()
        .player
        .borrow_mut()
        .set_engine(media::MediaEngine::new_synthesized());
    (app, doc)
}

#[test]
fn player_quality_choice_survives_settings_refresh_and_reopen() {
    use crate::model::Settings;
    use crate::resource::TubeShell;
    use ui::framework::testing::TestApp;
    use ui::widget::video_player::PlayerSettingEvent;

    let mut app = TestApp::<TubeShell>::capture(Some("watch")).unwrap();
    let preferences = app.settings::<Settings>();
    let index = QUALITIES
        .iter()
        .position(|quality| *quality != preferences.get().preferred_quality)
        .unwrap();
    let quality = QUALITIES[index];
    app.surface_mut::<TubeApp>()
        .tube()
        .apply_player_setting_event(PlayerSettingEvent::Chose {
            id: "resolution".into(),
            index,
        });
    assert_eq!(preferences.get().preferred_quality, quality);
    let surface = app.surface_mut::<TubeApp>();
    surface.tube().refresh_preferences();
    assert_eq!(surface.tube().model.settings.preferred_quality, quality);
    let app = app.reopen().unwrap();
    assert_eq!(app.settings::<Settings>().get().preferred_quality, quality);
}

#[test]
fn a_worker_result_wakes_the_window() {
    // The network answers while nobody touches Tube: the result wakes
    // the window, which shows it without waiting for input or an
    // animation.
    let mut scope = ui::wake::WakeScope::new();
    let (woke, wakes) = std::sync::mpsc::channel();
    scope
        .bind(move |event| {
            let _ = woke.send(event.requires_sync());
        })
        .unwrap();
    let mut app = TubeApp::sample();
    app.tube().wake_with(scope.handle());
    let results = app.script_worker_for_test();
    assert!(wakes.try_recv().is_err(), "nothing has arrived yet");
    results
        .send(Update::Status {
            live: true,
            label: "live".into(),
        })
        .unwrap();
    assert_eq!(
        wakes.try_recv(),
        Ok(true),
        "the result woke the window to sync"
    );
    assert!(Widget::take_structure_dirty(&mut app), "and it folds in");
}

#[test]
fn a_window_with_its_fetch_worker_asks_for_no_idle_syncs() {
    // The worker lives as long as the window and wakes it per result;
    // an idle sync for as long as it existed was a busy loop in a
    // collaborative window.
    let mut app = TubeApp::sample();
    let _results = app.script_worker_for_test();
    assert!(!Widget::needs_idle_sync(&app));
}

#[test]
fn a_staged_catalogue_retires_the_live_worker_and_fetches_nothing() {
    // A live window has spawned its fetch worker by its first frame.
    let app = TubeApp::new().online();
    app.tube().worker = Some(Worker::spawn(ui::WakeHandle::detached()));
    app.stage_model_for_test(crate::sample::model());
    assert!(
        app.tube().worker.is_none(),
        "a staged catalogue must not keep the live worker, whose late results rebuild the page"
    );
    app.tube().open_video("vid_rust_async".into());
    assert!(
        matches!(&app.tube().model.route, Route::Watch(id) if id == "vid_rust_async"),
        "the staged watch page opens"
    );
    assert!(
        app.tube().worker.is_none(),
        "an offline watch page asks the network for no SponsorBlock or DeArrow branding"
    );
}

#[test]
fn a_fresh_share_notice_replaces_the_previous_toast() {
    let (mut app, _doc) = fresh();
    app.tube().model.route = Route::Watch("vid_donut".into());
    app.shell.toast("Previous notice", ToastKind::Info);
    app.tube()
        .perform(TubeEffect::Share {
            video: "vid_donut".into(),
        })
        .unwrap();
    app.show_new_notices();
    assert!(
        app.shell
            .toast_text()
            .is_some_and(|text| text.contains("Link copied") && text.contains("vid_donut")),
        "the fresh share notice replaces the previous one"
    );
}

#[test]
fn controls_without_a_document_change_act_on_this_window() {
    // Opening a video shows it, starts it from the top and records it in
    // the watch history.
    {
        let app = TubeApp::sample();
        app.tube().apply(Action::OpenVideo("vid_donut".into()));
        assert_eq!(app.tube().model.route, Route::Watch("vid_donut".into()));
        assert_eq!(
            app.tube().model.history.first().map(String::as_str),
            Some("vid_donut")
        );
        assert_eq!(app.tube().model.playhead, 0.0);
        app.tube()
            .perform(TubeEffect::ForgetWatched {
                video: "vid_donut".into(),
            })
            .unwrap();
        assert!(!app.tube().model.history.iter().any(|id| id == "vid_donut"));
        app.tube().perform(TubeEffect::ClearHistory).unwrap();
        assert!(app.tube().model.history.is_empty());
    }
    {
        let app = TubeApp::sample();
        app.tube().apply(Action::OpenVideo("vid_donut".into()));
        app.tube().apply(Action::Share("vid_donut".into()));
        assert!(!app.tube().model.is_liked("vid_donut"));
        assert!(
            app.tube()
                .model
                .notices
                .iter()
                .any(|notice| notice.contains("youtu.be/vid_donut"))
        );
    }
    // A screen an effect names opens; one it does not is refused.
    {
        let app = TubeApp::sample();
        app.tube()
            .perform(TubeEffect::ShowScreen {
                screen: "settings".into(),
            })
            .unwrap();
        assert_eq!(app.tube().model.route, Route::Settings);
        assert!(
            app.tube()
                .perform(TubeEffect::ShowScreen {
                    screen: "attic".into()
                })
                .is_err()
        );
        assert!(
            app.tube()
                .perform(TubeEffect::OpenVideo {
                    video: "no-such-video".into()
                })
                .is_err()
        );
    }
    // What the platform search was asked for opens on YouTube's results
    // for it; a blank search is refused.
    {
        let app = TubeApp::sample();
        app.tube().follow_palette_query("  veritasium ");
        let entries = app.tube().search_entries_of_model();
        let row = entries
            .iter()
            .find_map(|entry| {
                entry
                    .anchor
                    .as_deref()
                    .filter(|anchor| anchor.starts_with("tube.search:"))
            })
            .expect("the search results offer to search YouTube");
        let effect = crate::search_type::opened(row).expect("the row opens");
        app.tube().perform(effect).unwrap();
        assert_eq!(app.tube().model.route, Route::Search("veritasium".into()));
        assert!(
            app.tube()
                .perform(TubeEffect::Search { query: " ".into() })
                .is_err()
        );
    }
    {
        let (app, _) = fresh_synth();
        assert_eq!(FullscreenProvider::fullscreen_intent(&app), None);
        app.tube().player.borrow_mut().toggle_fullscreen();
        assert_eq!(FullscreenProvider::fullscreen_intent(&app), Some(true));
    }
}

#[test]
fn an_emptied_library_shows_empty() {
    let app = TubeApp::sample();
    let mut library = Library::default();
    library.likes.insert("vid_donut".into());
    library.queue.insert("vid_donut".into());
    library.comment_likes.insert("vid_donut#1".into());
    app.tube().present_library(&library);
    assert!(app.tube().model.is_liked("vid_donut") && app.tube().model.is_queued("vid_donut"));
    assert!(app.tube().model.comment_likes.contains("vid_donut#1"));

    library.likes.remove(&"vid_donut".into());
    library.queue.remove(&"vid_donut".into());
    app.tube().present_library(&library);
    assert!(
        app.tube().model.liked.is_empty(),
        "an emptied library is shown"
    );
    assert!(app.tube().model.queue.is_empty());
}

// --- media-engine transport wiring (the real playback path) ---

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn resolved_streams_drive_the_playback_engine() {
    {
        // The worker's `StreamResolved` path (factored into `load_resolved_stream`)
        // must open the rangeable URL into the shared player's media engine and
        // start it — the exact code path the live app runs.
        let (app, _doc) = fresh_synth();
        app.tube().model.route = Route::Watch("vid_donut".into());
        let ok = app.tube().load_resolved_stream(
            "vid_donut".into(),
            "https://inv.example/latest_version?id=vid_donut&itag=18&local=true".into(),
            true,
            "360p",
        );
        assert!(ok, "the resolved stream loaded for the open watch video");
        assert!(
            app.tube().player.borrow().is_playing(),
            "the engine is playing the stream"
        );
        let src = app
            .tube()
            .player
            .borrow()
            .engine()
            .and_then(|e| e.source().cloned());
        match src {
            Some(media::Source::Remote { url }) => {
                assert!(
                    url.contains("local=true"),
                    "opened the rangeable proxy URL: {url}"
                );
            }
            other => panic!("expected a remote source on the engine, got {other:?}"),
        }
    }
    {
        // A late resolution for a since-closed video must not hijack the engine.
        let (app, _doc) = fresh_synth();
        app.tube().model.route = Route::Watch("vid_donut".into());
        let ok = app.tube().load_resolved_stream(
            "some_other_id".into(),
            "https://inv.example/x?local=true".into(),
            true,
            "360p",
        );
        assert!(!ok, "a stream for a non-open video is dropped");
        assert!(
            app.tube()
                .player
                .borrow()
                .engine()
                .and_then(|e| e.source())
                .is_none()
        );
    }
    {
        let (app, _) = fresh_synth();
        app.tube().model.route = Route::Watch("vid_donut".into());
        app.tube().load_resolved_stream(
            "vid_donut".into(),
            "https://inv.example/x?local=true".into(),
            true,
            "360p",
        );
        assert!(app.tube().player.borrow().is_playing());
        app.tube().apply(Action::TogglePlay);
        assert!(
            !app.tube().player.borrow().is_playing(),
            "TogglePlay paused the engine"
        );
        assert!(
            !app.tube().model.playing,
            "the model mirrors the paused engine"
        );
        app.tube().apply(Action::TogglePlay);
        assert!(
            app.tube().player.borrow().is_playing(),
            "TogglePlay resumed the engine"
        );
        assert!(
            app.tube().model.playing,
            "the model mirrors the playing engine"
        );
    }
    {
        // The resolve/buffer must start when the watch page OPENS — not on a
        // later Play click. `ensure_playback` (run right after the open action
        // drains, and on the boot-into-watch sync) arms it; `playing_id`
        // latching to the opened video proves the `PlayVideo` resolve request
        // went out, so playback is loading before any Play is pressed.
        let (app, _) = fresh_synth();
        app.tube().online = true; // online so `ensure_playback` actually resolves
        assert!(
            app.tube().playing_id.is_none(),
            "nothing armed before opening"
        );
        app.tube().apply(Action::OpenVideo("vid_donut".into()));
        app.tube().ensure_playback();
        assert_eq!(
            app.tube().playing_id.as_deref(),
            Some("vid_donut"),
            "opening a watch page arms stream resolution (no Play click needed)"
        );
    }
    {
        // While the stream is still resolving (armed but no source loaded) the
        // loop must keep pumping so `StreamResolved` folds in. Once a source is
        // loaded and the video is PAUSED, the app must let the event loop idle
        // rather than spin at 60fps forever — the needless busy-loop that made
        // scrolling on a paused-but-loaded watch page feel heavy.
        let (app, _doc) = fresh_synth();
        app.tube().online = true;
        app.tube().model.route = Route::Watch("vid_donut".into());
        app.tube().playing_id = Some("vid_donut".into());

        // Resolving: no source loaded yet → keep pumping.
        app.tube().player_video = None;
        assert!(
            app.needs_animation_frame(),
            "while the stream is resolving the loop keeps pumping so it folds in"
        );

        // Loaded + paused → the loop may idle (the paused player asks for none).
        let ok = app.tube().load_resolved_stream(
            "vid_donut".into(),
            "https://inv.example/x?local=true".into(),
            true,
            "360p",
        );
        assert!(
            ok && app.tube().player_video.as_deref() == Some("vid_donut"),
            "source loaded"
        );
        app.tube().player.borrow_mut().pause();
        app.tube().model.playing = false;
        assert!(
            !app.tube().player.borrow().is_playing(),
            "precondition: paused"
        );
        assert!(
            !app.needs_animation_frame(),
            "a paused, loaded watch page lets the event loop idle"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn sponsor_segments_and_autoplay_steer_the_engine() {
    {
        // Auto-skip runs against the real playhead and must seek the engine (not
        // just the model). vid_rust_async has a Sponsor segment 1440..1570.
        let (app, _doc) = fresh_synth();
        app.tube().model.route = Route::Watch("vid_rust_async".into());
        app.tube().load_resolved_stream(
            "vid_rust_async".into(),
            "https://inv.example/x?local=true".into(),
            true,
            "360p",
        );
        app.tube().model.playhead = 1500.0;
        let skipped = app.tube().apply_auto_skip();
        assert!(skipped, "a skip happened");
        assert!(
            app.tube().model.playhead >= 1570.0,
            "model playhead jumped past the segment"
        );
        // The seek reached the engine (its position advanced off the start). The
        // synthesized test engine caps its duration, so the exact target clamps —
        // proving it moved is the honest real-path assertion.
        let engine_pos = app
            .tube()
            .player
            .borrow()
            .engine()
            .map_or(0, media::MediaEngine::position_ms);
        assert!(
            engine_pos > 0,
            "the auto-skip seeked the engine (pos={engine_pos})"
        );
        assert!(
            app.tube()
                .model
                .notices
                .iter()
                .any(|n| n.contains("Sponsor"))
        );
    }
    {
        let (app, _) = fresh();
        // vid_rust_async has a Sponsor segment 1440..1570.
        app.tube().apply(Action::OpenVideo("vid_rust_async".into()));
        let dur = f64::from(app.tube().model.video("vid_rust_async").unwrap().duration);
        // Seek to ~1500s (inside the sponsor segment).
        app.tube().apply(Action::Seek(1500.0 / dur));
        // Auto-skip should jump the playhead to the segment end (1570).
        assert!(
            app.tube().model.playhead >= 1570.0,
            "playhead was {}",
            app.tube().model.playhead
        );
        assert!(
            app.tube()
                .model
                .notices
                .iter()
                .any(|l| l.contains("Sponsor"))
        );
    }
    {
        // Autoplay is on by default; seeking to the very end must advance to
        // the first "up next" recommendation — the real code path that
        // consumes the autoplay flag.
        let (app, _) = fresh();
        assert!(app.tube().model.settings.autoplay, "autoplay defaults on");
        app.tube().apply(Action::OpenVideo("vid_rust_async".into()));
        app.tube().apply(Action::Seek(1.0));
        match &app.route_for_test() {
            Route::Watch(id) => assert_ne!(
                id, "vid_rust_async",
                "autoplay advanced to a different video"
            ),
            other => panic!("expected a watch route, got {other:?}"),
        }
    }
    {
        let (app, _) = fresh();
        app.tube().model.settings.autoplay = false;
        app.tube().apply(Action::OpenVideo("vid_rust_async".into()));
        app.tube().apply(Action::Seek(1.0));
        assert_eq!(
            app.route_for_test(),
            Route::Watch("vid_rust_async".into()),
            "with autoplay off, reaching the end stays on the same video"
        );
    }
    {
        use crate::model::SkipMode;
        let (app, _) = fresh();
        assert_eq!(
            app.tube().model.settings.mode_of(Category::Intro),
            SkipMode::Skip
        );
        app.tube()
            .perform(TubeEffect::SetSegmentHandling {
                category: Category::Intro.api_key().into(),
                mode: SkipMode::Show.key().into(),
            })
            .unwrap();
        assert_eq!(
            app.tube().model.settings.mode_of(Category::Intro),
            SkipMode::Show
        );
        app.tube()
            .perform(TubeEffect::SetSegmentHandling {
                category: Category::Intro.api_key().into(),
                mode: SkipMode::Disabled.key().into(),
            })
            .unwrap();
        assert_eq!(
            app.tube().model.settings.mode_of(Category::Intro),
            SkipMode::Disabled
        );
    }
    {
        use crate::model::SkipMode;
        let (app, _doc) = fresh_synth();
        app.tube().model.route = Route::Watch("vid_rust_async".into());
        app.tube().load_resolved_stream(
            "vid_rust_async".into(),
            "https://inv.example/x?local=true".into(),
            true,
            "360p",
        );
        app.tube()
            .model
            .settings
            .categories
            .insert(Category::Sponsor.api_key().to_string(), SkipMode::Show);
        app.tube().model.playhead = 1500.0;
        assert!(
            !app.tube().apply_auto_skip(),
            "Show mode must NOT auto-skip"
        );
        assert_eq!(
            app.tube().model.playhead,
            1500.0,
            "playhead untouched by auto-skip"
        );
        app.tube().perform(TubeEffect::SkipSegment).unwrap();
        assert!(
            app.tube().model.playhead >= 1570.0,
            "manual skip advanced the playhead"
        );
    }
    {
        use crate::model::SkipMode;
        let (mut app, _doc) = fresh_synth();
        app.tube().model.route = Route::Watch("vid_rust_async".into());
        app.tube().load_resolved_stream(
            "vid_rust_async".into(),
            "https://inv.example/x?local=true".into(),
            true,
            "360p",
        );
        app.tube()
            .model
            .settings
            .categories
            .insert(Category::Sponsor.api_key().to_string(), SkipMode::Show);
        app.tube().model.playhead = 1500.0;
        let theme = Theme::dark();
        let _ = app.build_screen(&theme);
        assert!(
            app.tube()
                .player
                .borrow()
                .setting(screens::SKIP_SETTING)
                .is_some(),
            "inside a Show-mode segment the player offers Skip"
        );
        app.tube().apply_player_setting_event(
            ui::widget::video_player::PlayerSettingEvent::Activated {
                id: screens::SKIP_SETTING.into(),
            },
        );
        assert!(
            app.tube().model.playhead >= 1570.0,
            "Skip jumps past the segment"
        );
        app.show_new_notices();
        assert!(
            app.shell.toast_text().is_some(),
            "the skip is confirmed by a toast"
        );
        app.shell.dismiss_toast();
        app.show_new_notices();
        assert!(app.shell.toast_text().is_none(), "a notice is toasted once");
        let _ = app.build_screen(&theme);
        assert!(
            app.tube()
                .player
                .borrow()
                .setting(screens::SKIP_SETTING)
                .is_none(),
            "past the segment the Skip command is gone"
        );
    }
    {
        use crate::model::{Category, SkipMode};
        // The on-video "Skip" overlay is settings-driven: it appears only for a
        // Show-mode segment the playhead is inside, never for Auto-skip (which
        // jumps on its own) or Off, and never when SponsorBlock is disabled.
        let (app, _doc) = fresh();
        app.tube().model.route = Route::Watch("vid_rust_async".into()); // Sponsor 1440..1570
        let key = Category::Sponsor.api_key().to_string();

        app.tube()
            .model
            .settings
            .categories
            .insert(key.clone(), SkipMode::Skip);
        app.tube().model.playhead = 1500.0;
        assert_eq!(
            app.tube().current_show_skip_end(),
            None,
            "auto-skip shows no manual overlay"
        );

        app.tube()
            .model
            .settings
            .categories
            .insert(key, SkipMode::Show);
        assert_eq!(
            app.tube().current_show_skip_end(),
            Some(1570.0),
            "show-mode offers a Skip to the end"
        );

        app.tube().model.playhead = 100.0;
        assert_eq!(
            app.tube().current_show_skip_end(),
            None,
            "no overlay outside the segment"
        );

        app.tube().model.playhead = 1500.0;
        app.tube().model.settings.sponsorblock_enabled = false;
        assert_eq!(
            app.tube().current_show_skip_end(),
            None,
            "master off suppresses the overlay"
        );
    }
}

/// Seeking and skipping a segment shown with a button are actions, so an
/// agent or a screen reader reaches them as a click does.
#[test]
#[expect(
    clippy::float_cmp,
    reason = "the playhead is assigned the exact target under test"
)]
fn seeking_and_skipping_a_segment_are_actions() {
    use crate::model::SkipMode;
    let (app, _doc) = fresh_synth();
    assert!(
        app.tube()
            .perform(TubeEffect::Seek { seconds: 10.0 })
            .is_err(),
        "with no video open there is nothing to seek in"
    );
    let statuses = |app: &TubeApp| {
        let tube = app.tube();
        crate::rows::effect_statuses(&tube.model, tube.device_sync_view())
    };
    let no_video = ui::framework::EffectStatus::unavailable(
        TubeEffect::SEEK,
        "no_video",
        "Open a video to seek in it",
    );
    let no_segment = ui::framework::EffectStatus::unavailable(
        TubeEffect::SKIP_SEGMENT,
        "no_segment",
        "No segment to skip is playing",
    );
    assert!(statuses(&app).contains(&no_video));

    // vid_rust_async has a Sponsor segment 1440..1570.
    app.tube().model.route = Route::Watch("vid_rust_async".into());
    app.tube().load_resolved_stream(
        "vid_rust_async".into(),
        "https://inv.example/x?local=true".into(),
        true,
        "360p",
    );
    let sponsor = Category::Sponsor.api_key().to_string();
    app.tube()
        .model
        .settings
        .categories
        .insert(sponsor.clone(), SkipMode::Show);
    let offers_skip = |app: &TubeApp| {
        let tube = app.tube();
        crate::rows::live_rows(&tube.model, tube.device_sync_view())
            .into_iter()
            .find(|row| row.id() == "tube.current:vid_rust_async")
            .expect("the open video is published")
            .into_resource()
            .actions
            .iter()
            .any(|action| action.id == TubeEffect::SKIP_SEGMENT.id)
    };
    assert!(!offers_skip(&app), "no segment plays at the start");
    assert!(statuses(&app).contains(&no_segment));
    assert!(!statuses(&app).contains(&no_video), "a video is open");

    app.tube()
        .perform(TubeEffect::Seek { seconds: 1500.0 })
        .unwrap();
    assert_eq!(
        app.tube().model.playhead,
        1500.0,
        "a segment shown with a button waits for it"
    );
    assert!(offers_skip(&app), "the open video offers to skip it");
    assert!(!statuses(&app).contains(&no_segment));

    app.tube().perform(TubeEffect::SkipSegment).unwrap();
    assert_eq!(app.tube().model.playhead, 1570.0, "Skip lands at its end");
    assert!(!offers_skip(&app), "past the segment there is none to skip");
    assert!(
        app.tube().perform(TubeEffect::SkipSegment).is_err(),
        "skipping with no segment playing says so"
    );

    app.tube()
        .model
        .settings
        .categories
        .insert(sponsor, SkipMode::Skip);
    app.tube()
        .perform(TubeEffect::Seek { seconds: 1500.0 })
        .unwrap();
    assert!(
        app.tube().model.playhead >= 1570.0,
        "seeking into a segment set to skip skips it, as playback does"
    );
    assert!(
        app.tube()
            .perform(TubeEffect::Seek { seconds: -1.0 })
            .is_err(),
        "a moment before the start is refused"
    );
}

#[test]
fn library_and_queries_keep_shared_and_local_state_apart() {
    {
        let (mut app, mut doc) = fresh();
        app.tube().model.route = Route::Watch("vid_donut".into());
        app.tube().model.playhead = 42.0;
        ui::framework::testing::dispatcher::<Library>("tube.library")
            .submit_now(
                &mut ui::widget::Edits::lend(&mut doc),
                Library::like("vid_donut", true),
            )
            .expect("structured library action applies");
        app.tube().present_library(&Library::read(&doc).unwrap());
        Widget::sync(&mut app, &doc, &automerge::ROOT);
        assert!(app.tube().model.liked.iter().any(|id| id == "vid_donut"));
        assert!(
            doc.get(&automerge::ROOT, "tube_state")
                .expect("document read")
                .is_none(),
            "route, playback and fetched catalogue are never mirrored into collaboration"
        );
    }
    {
        let (mut first, doc) = fresh();
        first.tube().search_query.set("rust async".into());
        Widget::sync(&mut first, &doc, &automerge::ROOT);

        assert!(
            doc.get(&automerge::ROOT, "search_query")
                .expect("document read")
                .is_none()
        );

        let mut second = TubeApp::sample();
        Widget::sync(&mut second, &doc, &automerge::ROOT);
        assert!(second.tube().search_query.get().is_empty());
        assert_eq!(first.tube().search_query.get(), "rust async");

        // A document holding a search key must not hand its value to a new
        // app merely because a collaborator's client emitted it.
        let mut legacy_doc = AutoCommit::new();
        ui::tilo_resource::write_key(
            &mut legacy_doc,
            &automerge::ROOT,
            "search_query",
            &ui::tilo_resource::Atom("remote query".into()),
        )
        .expect("legacy search seed");
        let mut third = TubeApp::sample();
        Widget::sync(&mut third, &legacy_doc, &automerge::ROOT);
        assert!(third.tube().search_query.get().is_empty());
    }
}

// --- navigation rail ---

#[test]
fn the_rail_adapts_and_navigates_through_published_actions() {
    {
        let app = TubeApp::sample().with_route_shot(Route::Trending);
        assert_eq!(app.tube().model.route, Route::Trending);
        assert_eq!(app.tube().nav.current(), &Route::Trending);
        assert!(
            !app.can_go_back_for_test(),
            "a direct screenshot route has no fabricated prior destination"
        );
    }
    {
        let mut app = ui::prelude::TestApp::<crate::resource::TubeShell>::new().unwrap();
        app.effect(&TubeEffect::ShowScreen {
            screen: "settings".into(),
        })
        .unwrap();
        app.settle();
        app.with_surface::<TubeApp, _>(|viewer| {
            assert_eq!(viewer.tube().model.route, Route::Settings);
            assert!(viewer.can_go_back_for_test(), "Back returns to Home");
        })
        .expect("the viewer is mounted");
    }
    // The shared shell docks the rail while the grid keeps its minimum
    // width beside it; narrower, the rail is a screen of its own. The
    // sidebar starts as its rail (a short flat set of places), so it
    // stays docked down to the rail beside the grid's minimum.
    {
        use ui::widget::SidebarPlacement::{Docked, Screen};
        for (width, placement) in [(1280, Docked), (800, Docked), (420, Docked), (340, Screen)] {
            let mut framed = ui::headless::Framed::new(
                ui::State::new(),
                TubeApp::sample(),
                (width, 800),
                Theme::dark(),
            );
            assert_eq!(
                framed.shell.geometry().sidebar,
                placement,
                "the shared shell adapts the rail at {width} px"
            );
            if placement == Screen && !framed.sidebar_open_for_test() {
                ui::headless::Framed::harness_mut(&mut framed)
                    .press_window_slot(ui::window_toolbar::ToolbarEvent::ToggleSidebar);
            }
            assert!(
                framed.sidebar_open_for_test(),
                "the window bar's sidebar toggle reaches the rail at {width} px"
            );
        }
    }
}

// --- no fabricated content + loading states ---

#[test]
fn framework_launch_mounts_the_live_app() {
    // The installed binary launches through the framework, so the surface
    // it mounts must be the online app: no bundled sample catalogue, and
    // a first `sync` that fetches the real feed.
    let app = crate::resource::TubeShell::window(ui::framework::RunMode::Live);
    assert!(
        app.tube().online,
        "the framework launch must reach the live backend"
    );
    assert!(
        app.tube().model.videos.is_empty() && app.tube().model.channels.is_empty(),
        "the framework launch must not show the bundled sample catalogue"
    );
    assert!(
        app.tube().model.feed_loading,
        "cold Home shows a loading skeleton"
    );
}

#[test]
fn online_catalogue_starts_empty_and_tracks_requests() {
    {
        // The real (online) app must NOT present the bundled sample catalogue as
        // if it were real data — it starts empty and shows a loading skeleton.
        let app = TubeApp::new().online();
        let tube = app.tube();
        assert!(
            tube.model.videos.is_empty(),
            "online start must seed no sample videos"
        );
        assert!(
            tube.model.channels.is_empty(),
            "online start must seed no sample channels"
        );
        assert!(tube.model.subscriptions.is_empty());
        assert!(
            tube.model.feed_loading,
            "cold Home shows a loading skeleton, not a blank page"
        );
    }
    {
        let app = TubeApp::new();
        let mut tube = app.tube();
        tube.model.search_query = "rust".into();
        tube.model.search_loading = true;
        tube.request_rejected(
            &Request::Search {
                query: "rust".into(),
            },
            false,
        );
        assert!(
            !tube.model.search_loading,
            "a refused search must not leave the results waiting"
        );
        assert!(
            tube.model
                .search_error
                .as_deref()
                .is_some_and(|message| message.contains("try again"))
        );

        tube.model.feed_loading = true;
        tube.request_rejected(&Request::Feed, false);
        assert!(
            !tube.model.feed_loading,
            "do not leave Home loading forever"
        );
        assert!(
            tube.model
                .feed_error
                .as_deref()
                .is_some_and(|message| message.contains("try again"))
        );
    }
    {
        let app = TubeApp::sample();
        let mut tube = app.tube();
        tube.online = true;
        tube.kicked_off = true;
        tube.search_query.set("rust".into());
        tube.apply(Action::SubmitSearch);
        assert!(
            tube.worker.is_some(),
            "the submitted query reaches the background worker"
        );
        assert!(
            tube.model.search_loading,
            "the outstanding result is represented as loading"
        );
        assert_eq!(tube.model.search_query, "rust");
        assert_eq!(tube.model.route, Route::Search("rust".into()));
    }
    {
        // The watch page's creator avatar + subscriber count come from a channel
        // fetch kicked off when we only hold a name stub. Prove the gate: a bare
        // `ensure_channel_stub` record has no avatar and 0 subscribers (so it
        // qualifies), while a fetched channel would not.
        let mut m = crate::model::TubeModel::default();
        m.ensure_channel_stub("c1", "Chan");
        let stub = m.channel("c1").expect("stub inserted");
        assert!(
            stub.avatar_url.is_empty() && stub.subscribers == 0,
            "a name stub must be recognised as needing an avatar/subs fetch",
        );
    }
}

#[test]
fn an_image_still_loading_queues_no_rebuild() {
    // Thumbnails / avatars / banners resolve their decoded image LAZILY in
    // their own `sync` and repaint via `needs_redraw` when it lands, so an
    // in-flight image load must NOT make the app report a structural
    // rebuild (a whole-tree rebuild + layout re-solve on every one of
    // ~160 image completions would make the grids churn).
    use std::sync::{Arc, Mutex};
    // A fetcher held until the case ends, so a load is in flight
    // (pending ≥ 1) throughout.
    let (release, held) = std::sync::mpsc::channel::<()>();
    let held = Mutex::new(held);
    let fetcher: ui::image_store::Fetcher = Arc::new(move |_: &str| {
        let _ = held
            .lock()
            .unwrap()
            .recv_timeout(tilo_test_env::hang_bound());
        None
    });
    let store = ui::ImageStore::new(&fetcher);
    let _ = store.get("http://x/a.png");
    assert!(store.pending() >= 1, "a load is in flight");

    let mut app = TubeApp::sample().with_image_store(store.clone());
    let doc = AutoCommit::new();
    // The first frame builds the shell; image loads arrive after it.
    let _ = Widget::take_structure_dirty(&mut app);
    app.sync(&doc, &automerge::ROOT);
    assert!(
        !Widget::take_structure_dirty(&mut app),
        "an in-flight image load must NOT queue a whole-tree rebuild — images \
     fold in lazily via sync + redraw, not via a structural rebuild",
    );
    assert!(store.pending() >= 1, "the load was still in flight");
    drop(release);
}

#[test]
fn unreachable_sources_settle_pages_without_claiming_offline() {
    use crate::api::Unreached;
    // Case `unreachable_sources_are_not_reported_as_offline`: in a page
    // the sources refuse cross-origin reads and the host has no fetch
    // proxy. The device is online, so Home and search say what is wrong
    // instead of "You're offline".
    {
        let mut app = TubeApp::new().online();
        let results = app.script_worker_for_test();
        let mut tube = app.tube();
        tube.model.search_query = "rust".into();
        tube.model.search_loading = true;
        results.send(Update::Unreached(Unreached::NoRelay)).unwrap();
        assert!(tube.pump());
        assert!(!tube.model.feed_loading, "Home stops loading");
        assert!(!tube.model.search_loading, "search stops loading");
        assert!(
            !tube.model.data_source.label.contains("offline"),
            "an online device is not offline: {:?}",
            tube.model.data_source
        );
        assert_eq!(tube.model.sources_unreached, Some(Unreached::NoRelay));
        assert!(t("tube-sources-no-relay").contains("fetch proxy"));
        assert_eq!(
            tube.model.search_error.as_deref(),
            Some(t("tube-unreached-title").as_str())
        );

        let failed = Unreached::Failed("HTTP 403".into());
        results.send(Update::Unreached(failed.clone())).unwrap();
        assert!(tube.pump());
        assert_eq!(tube.model.sources_unreached, Some(failed));
        assert!(!tube.model.data_source.label.contains("offline"));

        // Only a device without a network shows the offline state.
        results.send(Update::Unreached(Unreached::Offline)).unwrap();
        assert!(tube.pump());
        assert!(tube.model.data_source.label.contains("offline"));
        assert_eq!(
            tube.model.sources_unreached, None,
            "Home shows the offline state"
        );
        assert_eq!(tube.model.feed_error, None);
    }
    // Case `a_video_youtube_refuses_says_why_on_its_page_and_tries_again`:
    // only the page still open on it, in YouTube's own words.
    {
        let mut app = TubeApp::new().online();
        let video = String::from("vid_donut");
        let results = app.script_worker_for_test();
        let mut tube = app.tube();
        tube.navigate_to(Route::Watch(video.clone()));
        results
            .send(Update::Unplayable {
                video: "another".into(),
                said: None,
            })
            .unwrap();
        tube.pump();
        assert_eq!(tube.model.unplayable(&video), None);
        let bot_check = "Sign in to confirm you're not a bot";
        results
            .send(Update::Unplayable {
                video: video.clone(),
                said: Some(bot_check.into()),
            })
            .unwrap();
        assert!(tube.pump());
        assert_eq!(
            tube.model
                .unplayable(&video)
                .and_then(|u| u.said.as_deref()),
            Some(bot_check)
        );
        assert!(!tube.model.playing);
        tube.apply(Action::RetryPlayback);
        assert_eq!(tube.model.unplayable(&video), None, "the page asks anew");
        assert_eq!(tube.playing_id.as_deref(), Some(video.as_str()));
    }
    {
        let app = TubeApp::new().online();
        let mut tube = app.tube();
        tube.navigate_to(Route::Watch("vid_donut".into()));
        tube.model.playing = true;
        assert!(!tube.player_stopped(&[media::MediaEvent::Error("network error".into())]));
        assert!(tube.model.notices.is_empty(), "{:?}", tube.model.notices);
        assert!(tube.player_stopped(&[
            media::MediaEvent::LimitReached,
            media::MediaEvent::Error("the host streamed as much as it allows for now".into()),
        ]));
        assert_eq!(
            tube.model.notices.first().map(String::as_str),
            Some("Playback limit reached on this host. Try again later.")
        );
        assert!(!tube.model.playing);
    }
    {
        let app = TubeApp::new();
        let mut tube = app.tube();
        tube.model.feed_loading = true;
        tube.request_rejected(&Request::Feed, true);
        assert!(!tube.model.data_source.label.contains("offline"));
        assert!(
            tube.model
                .feed_error
                .as_deref()
                .is_some_and(|message| message.contains("stopped"))
        );
    }
}

#[test]
fn youtube_answers_the_platform_search_as_it_is_typed_and_stale_answers_drop() {
    let mut app = TubeApp::new().online();
    let results = app.script_worker_for_test();
    let found = |id: &str, title: &str| crate::model::Video {
        id: id.into(),
        title: title.into(),
        dearrow_title: None,
        channel_name: "Cat channel".into(),
        ..crate::sample::model().videos[0].clone()
    };
    app.tube().follow_palette_query("ca");
    assert!(app.tube().suggestions_pending(), "YouTube is asked");
    app.tube().follow_palette_query("cats");
    results
        .send(Update::Suggestions {
            query: "ca".into(),
            videos: vec![found("yt_old", "Cars of the year")],
        })
        .unwrap();
    app.tube().pump();
    assert!(
        app.tube().suggestions_pending(),
        "an answer for an older query is not the newer one's"
    );
    results
        .send(Update::Suggestions {
            query: "cats".into(),
            videos: vec![
                found("yt_cat_1", "Funniest kittens compilation"),
                found("yt_cat_2", "Big cat rescue"),
            ],
        })
        .unwrap();
    app.tube().pump();
    assert!(!app.tube().suggestions_pending(), "the latest answer is in");

    let provider = ui::framework::testing::search_source::<crate::resource::Library>(
        app.tube().search_entries_of_model(),
    );
    let hits = ui::search::SearchProvider::search(&provider, "cats");
    let youtube: Vec<_> = hits
        .iter()
        .filter(|hit| hit.action_id.starts_with("tube."))
        .map(|hit| hit.action_id.as_str())
        .collect();
    assert_eq!(
        youtube.get(..3),
        Some(
            &[
                "tube.search:cats",
                "tube.video:yt_cat_1",
                "tube.video:yt_cat_2"
            ][..]
        ),
        "Enter still searches; YouTube's videos follow in its order: {hits:#?}"
    );
    assert!(
        hits.iter().all(|hit| hit.action_id != "tube.video:yt_old"),
        "the stale answer never shows"
    );
    let effect = crate::search_type::opened("tube.video:yt_cat_2").expect("the row opens");
    app.tube().perform(effect).unwrap();
    assert_eq!(app.tube().model.route, Route::Watch("yt_cat_2".into()));

    app.tube().follow_palette_query("  ");
    assert!(
        !app.tube().suggestions_pending(),
        "an empty query asks nothing"
    );
    assert!(
        app.tube()
            .search_entries_of_model()
            .iter()
            .all(|entry| entry.group.as_deref() != Some("YouTube")),
        "and offers nothing from YouTube"
    );
}
