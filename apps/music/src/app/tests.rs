use super::*;
use ui::search::SearchProvider as _;

/// The library's search source as the framework builds it from the
/// resource's entries.
fn library_source(app: &MusicApp) -> impl ui::search::SearchProvider + use<> {
    ui::framework::testing::search_source::<crate::resource::Music>(
        crate::search_type::library_entries(&app.lib),
    )
}
use std::io::Cursor;
use ui::widget::Continuable as _;

fn app_with_doc() -> (MusicApp, AutoCommit) {
    let mut app = MusicApp::new();
    // The behaviour tests exercise playback, queueing and playlists against
    // the bundled catalog, as the screenshot scenarios do. A new library
    // starts empty; see `default_startup_has_empty_library`.
    let mut doc = AutoCommit::new();
    doc::seed(&mut doc, &seed::sample_library());
    app.reload(&doc);
    (app, doc)
}

#[cfg(feature = "audio")]
#[test]
fn private_queue_metadata_restores_unsaved_songs_before_resume() {
    let (mut original, _) = app_with_doc();
    original.pb.queue = vec!["tk_g1".into(), "tk_g2".into()];
    original.pb.current = Some("tk_g1".into());
    original.pb.playing = true;
    let profile = crate::storage::DeviceProfile {
        playback: original.pb.clone(),
        queue_catalogue: original.queue_catalogue(),
        ..Default::default()
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profile.json");
    crate::storage::save_device_profile_to(&path, &profile).unwrap();
    let restored = crate::storage::load_device_profile_from(&path);
    let mut app = MusicApp::from_device_profile(restored, false, String::new(), None);
    let mut doc = AutoCommit::new();
    app.reload(&doc);
    let before = doc.get_heads();
    assert!(!app.pb.playing, "restoration never starts audio");
    assert_eq!(app.pb.queue, original.pb.queue);
    for id in &app.pb.queue {
        assert_eq!(app.playback_track(id), original.playback_track(id));
    }
    let mut requested = app.pb.clone();
    requested.playing = true;
    assert!(app.apply_resource_playback(requested).is_ok());
    assert!(doc::load_library(&doc).tracks.is_empty());
    assert_eq!(doc.get_heads(), before);
    app.audio.stop();
}

#[cfg(feature = "audio")]
#[test]
fn legacy_spotify_queue_resolves_canonical_ids_without_inventing_metadata() {
    let (mut app, mut doc) = app_with_doc();
    let before = doc.get_heads();
    let next = "spotify:track:0DiWol3AO6WpXZgp0goxAV";
    let request = Playback {
        current: Some("tk_g1".into()),
        queue: vec!["tk_g1".into(), next.into()],
        ..Default::default()
    };
    assert!(app.playback_track(next).is_none());
    assert!(app.apply_resource_playback(request.clone()).is_ok());
    assert_eq!(app.pb.queue, request.queue);
    let mut invalid = request;
    invalid.queue.push("/private/not-granted.wav".into());
    assert!(app.apply_resource_playback(invalid).is_err());
    assert!(app.apply_resource_play_queue_item(1, next).is_ok());
    assert!(
        app.playback_track(next).is_none(),
        "the provider must supply metadata"
    );
    assert_eq!(doc.get_heads(), before);
    app.audio.stop();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn track_context_menu_stays_in_bounds_and_dispatches_its_actions() {
    // Phase `track_context_menu_stays_inside_the_app_rect_from_every_corner`.
    // 2026-09-16 alignment review: the track context menu used a hand-rolled
    // "nudge to stay in the app rect" that guessed the menu as
    // 220 × (rows·28 + 24) and laid it out against `Theme::dark()`. It now opens through the
    // shared `ContextMenu::open_anchored`, whose geometry is measured with the
    // frame's renderer and theme and clamped to the viewport — including when
    // the pointer is in the bottom-right corner.
    {
        let (mut app, _doc) = app_with_doc();
        app.rect = Rect::new(0.0, 0.0, 900.0, 600.0);
        // A track from the bundled sample library (see
        // `open_track_menu_lists_expected_actions`).
        let track = String::from("tk_g2");
        assert!(
            app.lib.tracks.contains_key(&track),
            "sample library has tk_g2"
        );
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        for pointer in [
            Point::new(4.0, 4.0),
            Point::new(896.0, 4.0),
            Point::new(4.0, 596.0),
            Point::new(896.0, 596.0),
        ] {
            app.mouse = pointer;
            app.open_track_menu(track.clone(), None, None);
            assert!(app.context.is_open(), "menu opens at {pointer:?}");
            // Settle text metrics the way the paint pass does.
            app.context.layout_within(&mut tr, &theme);
            let menu = app.context.get_rect();
            assert!(
                menu.width() > 0.0 && menu.height() > 0.0,
                "menu has measured geometry at {pointer:?}: {menu:?}"
            );
            assert!(
                app.rect.contains(menu.origin())
                    && app.rect.contains(Point::new(menu.x1 - 0.5, menu.y1 - 0.5)),
                "menu {menu:?} stays inside {:?} for pointer {pointer:?}",
                app.rect
            );
            app.context.close();
        }
    }

    // Phase `open_track_menu_lists_expected_actions`.
    {
        let (mut app, _doc) = app_with_doc();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        // tk_g2 is not liked in the seed, so the toggle row reads "Like".
        app.open_track_menu("tk_g2".into(), None, None);
        assert!(app.context.is_open());
        for label in [
            "Play",
            "Play next",
            "Add to queue",
            "Add to playlist\u{2026}",
            "Go to album",
            "Go to artist",
            "Like",
        ] {
            assert!(menu_index(&app, label).is_some(), "menu has {label}");
        }
        assert!(
            menu_index(&app, "Add to library").is_none(),
            "no duplicate library row"
        );
    }

    // Phase `context_menu_uses_shared_viewport_and_current_large_text_geometry`.
    {
        use ui::widget::Widget as _;

        let (mut app, _doc) = app_with_doc();
        // A short, narrow 200%-scale viewport is the hostile case: the
        // contextual action list cannot fit above or below its anchor. The
        // shared menu must own one in-viewport, internally scrollable surface
        // instead of Music's former guessed clamp.
        app.rect = Rect::new(0.0, 0.0, 390.0, 320.0);
        app.context_text_scale = 2.0;
        app.mouse = Point::new(382.0, 304.0);
        app.open_track_menu("tk_g2".into(), None, None);

        let rect = app.context.get_rect();
        assert!(rect.x0 >= app.rect.x0 && rect.x1 <= app.rect.x1);
        assert!(rect.y0 >= app.rect.y0 && rect.y1 <= app.rect.y1);
        assert_eq!(rect, app.rect, "oversized menu scrolls within the viewport");
    }

    // Phase `menu_add_to_queue_grows_the_queue`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let before = app.pb.queue.len();
        app.open_track_menu("tk_s2".into(), None, None);
        let idx = menu_index(&app, "Add to queue").unwrap() as i64;
        // Simulate the menu callback firing for that row.
        app.pending.store(idx, Ordering::SeqCst);
        app.drain_menu_selection(&mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(
            app.pb.queue.len(),
            before + 1,
            "Add to queue grows the queue"
        );
        assert_eq!(
            app.pb.queue.last().map(std::string::String::as_str),
            Some("tk_s2")
        );
    }

    // Phase `right_click_opens_menu_then_selection_dispatches`.
    {
        // The full production path: draw to register row-menu hits, right-click
        // to open, then a left-click on the "Play next" row dispatches it.
        let (mut app, mut doc) = app_with_doc();
        app.view = View::Songs;
        // Render one frame so row-menu hits + geometry exist.
        draw_once(&mut app, &mut doc);
        // A track row is its own right-click target.
        let p = track_row(&app, "tk_g2").center();
        send(&mut app, &cursor_at(p), &mut doc);
        let handled = send(&mut app, &press(MouseButton::Right), &mut doc);
        assert!(handled, "right-click on a row is consumed");
        assert!(app.context.is_open(), "right-click opens the context menu");
    }

    // Phase `the_songs_table_is_the_shared_table_with_a_row_per_track`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.view = View::Songs;
        draw_once(&mut app, &mut doc);
        let table = app.track_tables.first().expect("the songs table");
        assert_eq!(table.table.rows().len(), app.lib.tracks.len());
        assert!(
            table.table.row_rect(0).is_some(),
            "its first row is in view"
        );
    }
}

/// A track row is the shared table's: a press plays it from the table's
/// queue, and its actions are the toolkit's buttons under the pointer.
#[test]
fn track_rows_play_and_act_through_the_shared_table() {
    // Phase `a_press_on_a_row_plays_it_from_the_tables_queue`.
    let (mut app, mut doc) = app_with_doc();
    app.view = View::Songs;
    draw_once(&mut app, &mut doc);
    draw_once(&mut app, &mut doc);
    click_track(&mut app, &mut doc, "tk_g2");
    assert_eq!(app.pb.current.as_deref(), Some("tk_g2"));
    assert_eq!(
        app.pb.queue.len(),
        app.lib.tracks.len(),
        "the whole list is queued behind it"
    );

    // Phase `the_queue_rows_reorder_and_remove_with_their_buttons`.
    app.view = View::Queue;
    draw_once(&mut app, &mut doc);
    draw_once(&mut app, &mut doc);
    let next = app.pb.queue[app.pb.queue_index + 1].clone();
    let after = app.pb.queue[app.pb.queue_index + 2].clone();
    let down = track_control(&mut app, &mut doc, &next, "down", draw_once);
    hosted_click(&mut app, &mut doc, down.center());
    assert_eq!(
        app.pb.queue[app.pb.queue_index + 1],
        after,
        "Move down swaps the row with the one below"
    );
    assert_eq!(
        app.pb.current.as_deref(),
        Some("tk_g2"),
        "playback is untouched"
    );
    let before = app.pb.queue.len();
    draw_once(&mut app, &mut doc);
    let remove = track_control(&mut app, &mut doc, &after, "remove", draw_once);
    hosted_click(&mut app, &mut doc, remove.center());
    assert_eq!(
        app.pb.queue.len(),
        before - 1,
        "Remove takes the row out of the queue"
    );
}

/// A cover is the shared media card: a press on it opens what it names, and
/// the Play action over its art plays it.
#[test]
fn cover_cards_open_and_play_through_the_shared_grid() {
    let (mut app, mut doc) = app_with_doc();
    app.view = View::Albums;
    draw_once(&mut app, &mut doc);
    let album = app
        .lib
        .albums
        .keys()
        .next()
        .cloned()
        .expect("a sample album");
    let card = app
        .media_grids
        .iter()
        .find_map(|grid| grid.card_rect(&album))
        .expect("the album has a card");
    assert!(card.width() > 0.0 && app.body_rect.contains(card.center()));

    // Phase `the_play_action_over_the_art_plays_the_album`.
    let art = Point::new(card.center().x, card.y0 + card.width() / 2.0);
    send(&mut app, &cursor_at(art), &mut doc);
    send(&mut app, &press(MouseButton::Left), &mut doc);
    send(&mut app, &released(MouseButton::Left), &mut doc);
    assert_eq!(app.view, View::Albums, "Play does not leave the page");
    let playing = app.pb.current.clone().expect("the album plays");
    assert_eq!(
        app.lib
            .tracks
            .get(&playing)
            .map(|track| track.album_id.clone()),
        Some(album.clone())
    );

    // Phase `a_press_on_the_card_opens_the_album`.
    let title = Point::new(card.x0 + 8.0, card.y1 - 6.0);
    send(&mut app, &cursor_at(title), &mut doc);
    send(&mut app, &press(MouseButton::Left), &mut doc);
    send(&mut app, &released(MouseButton::Left), &mut doc);
    assert_eq!(app.view, View::Album(album));
}

/// Test and shot constructors own their caches and never read the real profile;
/// startup adopts only the host document; the seed is honest.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn constructors_isolate_caches_profiles_and_never_fabricate_content() {
    // Phase `test_instances_keep_cache_mutations_in_separate_owned_directories`.
    {
        let first = MusicApp::new();
        let second = MusicApp::new();
        assert_ne!(first.media_cache_root, second.media_cache_root);
        assert!(!first.persist_device_profile);
        let root = first.media_cache_root.clone();
        std::fs::write(root.join("test-owned-media"), b"media").unwrap();
        assert!(!second.media_cache_root.join("test-owned-media").exists());
        drop(first);
        assert!(!root.exists());
    }

    // Phase `shot_constructor_never_reads_tilo_music_profile`.
    {
        let path = std::env::temp_dir().join(format!(
            "tilo-music-shot-profile-{}-{}.json",
            std::process::id(),
            ui::time::now().elapsed().as_nanos()
        ));
        let mut profile = crate::storage::DeviceProfile::default();
        profile.playback.current = Some("sentinel-track".into());
        profile.playback.queue = vec!["sentinel-track".into()];
        profile.playback.position_ms = 42;
        crate::storage::save_device_profile_to(&path, &profile).unwrap();

        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_MUSIC_PROFILE", &path);
        let app = MusicApp::without_profile();
        drop(env);
        let _ = std::fs::remove_file(path);

        assert!(app.pb.current.is_none());
        assert!(app.device_cache.is_empty());
        assert!(!app.persist_device_profile);
    }

    // Phase `seeds_and_loads_library`.
    {
        let (app, _doc) = app_with_doc();
        assert!(app.lib.total_tracks() >= 20);
        // A fresh launch is idle: nothing is playing until the user starts it.
        assert!(
            app.pb.current.is_none(),
            "no fabricated now-playing track on startup"
        );
        assert!(!app.pb.playing, "transport is not playing on startup");
    }

    // Phase `default_startup_adopts_only_the_host_document_not_a_relay_file`.
    {
        // Migration guarantee: the GUI never loaded the removed relay file.
        // The live app receives its canonical document from the platform; with
        // a new record's empty library it must not fabricate a catalog, a
        // playlist, or an app-owned library store.
        let mut app = MusicApp::new();
        let mut doc = AutoCommit::new();
        doc::seed(&mut doc, &Library::default());
        app.reload(&doc);
        assert_eq!(
            app.lib.total_tracks(),
            0,
            "no fabricated tracks on a fresh launch"
        );
        assert!(
            app.lib.playlists.is_empty(),
            "no fabricated playlists on a fresh launch"
        );
        assert!(
            app.pb.current.is_none(),
            "nothing playing on a fresh launch"
        );
        // The platform document is still initialised, just empty; a later host
        // sync can adopt shared state without re-seeding.
        assert!(
            doc::is_seeded(&doc),
            "the empty library still initialises the doc structure"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn picked_covers_normalise_to_portable_values() {
    // Phase `picked_cover_normalizes_to_a_portable_renderable_action_value`.
    {
        let mut output = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(&mut output, image::ImageFormat::Png)
            .unwrap();
        let source = output.into_inner();
        let portable = portable_cover_data_url(&source).expect("small PNG normalizes");
        let action = crate::actions::set_cover_action("pl_test", &portable);
        let crate::actions::MusicAction::SetPlaylistCover { cover, .. } = action else {
            unreachable!("cover builder returns a cover action");
        };
        assert!(crate::actions::portable_cover_value(&cover));
        assert!(!cover.contains("file:"));
        assert!(!cover.contains("/tmp/"));

        let decoded = embedded_cover_bytes(&cover).expect("renderer accepts portable value");
        assert!(
            image::load_from_memory(&decoded).is_ok(),
            "the image cache receives a decodable PNG"
        );
        assert!(embedded_cover_bytes("file:///tmp/cover.png").is_none());
        assert!(embedded_cover_bytes("/tmp/cover.png").is_none());
    }

    // Phase `completed_portal_cover_write_contains_no_device_local_path`.
    {
        let (mut app, mut doc) = app_with_doc();
        let playlist = app.lib.playlists[0].id.clone();
        let mut output = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(&mut output, image::ImageFormat::Png)
            .unwrap();
        let source = output.into_inner();
        let portable = portable_cover_data_url(&source).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        sender.send(Ok(portable)).unwrap();
        app.cover_embed_for = Some(playlist.clone());
        app.cover_embed = Some(receiver);

        app.drain_cover_embed(&mut ui::widget::Edits::lend(&mut doc));
        let persisted = doc::load_library(&doc)
            .playlist(&playlist)
            .unwrap()
            .cover
            .clone();
        assert!(persisted.starts_with("data:image/png;base64,"));
        assert!(!persisted.contains("file:"));
        assert!(!persisted.contains("/tmp/"));
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn continuation_and_media_handoff_restore_known_tracks_locally() {
    // Phase `continuation_round_trip_reopens_a_known_track_paused_at_its_position`.
    {
        let (mut sender, source) = app_with_doc();
        sender.pb.current = Some("tk_g1".into());
        sender.pb.playing = true;
        sender.playhead_ms = 12_345.0;
        let metadata = sender
            .continuation_metadata(&source, &automerge::ROOT)
            .expect("a selected library track has a portable continuation");

        let (mut receiver, mut target) = app_with_doc();
        assert!(receiver.restore_continuation_metadata(
            &metadata,
            &mut ui::widget::Edits::lend(&mut target),
            &automerge::ROOT,
        ));
        assert_eq!(receiver.pb.current.as_deref(), Some("tk_g1"));
        assert_eq!(receiver.pb.position_ms, 12_345);
        assert!(
            !receiver.pb.playing,
            "continuation restore must never autoplay"
        );
    }

    // Phase `media_handoff_resolves_synced_track_and_restores_transport`.
    {
        let (mut app, mut doc) = app_with_doc();
        let arguments = [crate::search_type::SearchHit::track_link("tk_g1")];
        app.open_launch(
            &ui::framework::LaunchInput::new(&arguments, None).with_handoff(Some(
                &ui::application::PlaybackHandoff {
                    position_ms: 12_345,
                    resume_playing: true,
                    title: None,
                },
            )),
        );
        let before = (
            doc.base_heads(),
            automerge::transaction::Transactable::pending_ops(&doc),
        );
        app.sync(&doc, &automerge::ROOT);
        assert_eq!(
            (
                doc.base_heads(),
                automerge::transaction::Transactable::pending_ops(&doc)
            ),
            before
        );
        assert!(
            app.pb.current.is_none(),
            "sync leaves the pending handoff for authored settlement"
        );
        assert!(app.pending_media_handoff.is_some());
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
        assert_eq!(app.pb.current.as_deref(), Some("tk_g1"));
        assert_eq!(app.pb.position_ms, 12_345);
        assert_eq!(app.playhead_ms, 12_345.0);
        assert!(app.pb.playing);
        assert!(app.pending_media_handoff.is_none());
    }

    // Phase `media_handoff_waits_for_its_synced_resource`.
    {
        let mut app = MusicApp::without_profile();
        let arguments = [crate::search_type::SearchHit::track_link(
            "not-hydrated-yet",
        )];
        app.open_launch(
            &ui::framework::LaunchInput::new(&arguments, None).with_handoff(Some(
                &ui::application::PlaybackHandoff {
                    position_ms: 42,
                    resume_playing: true,
                    title: None,
                },
            )),
        );
        let mut doc = AutoCommit::new();
        app.sync(&doc, &automerge::ROOT);
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
        assert!(app.pb.current.is_none());
        assert!(app.pending_media_handoff.is_some());
    }

    // Phase `shared_library_sync_never_adopts_remote_playback`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.sync(&doc, &automerge::ROOT);
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
        let before = app.pb.clone();
        // A peer can still edit a collaborative playlist. It refreshes the
        // library but must leave this device's transport untouched.
        app.perform_action(
            &mut ui::widget::Edits::lend(&mut doc),
            &crate::actions::create_playlist_action("pl_peer_library", "Peer library edit"),
        );
        app.sync(&doc, &automerge::ROOT);
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
        assert_eq!(app.pb, before, "transport remains device-local");
        assert!(
            app.lib.playlist("pl_peer_library").is_some(),
            "collaborative library edits still arrive"
        );
    }
}

#[test]
#[cfg(feature = "audio")]
#[serial_test::serial(audio_backend)]
fn metadata_duration_does_not_cut_off_real_audio_before_eos() {
    let (mut app, _) = app_with_doc();
    app.lib.tracks.get_mut("tk_g1").unwrap().duration_s = 1;
    app.pb.queue = vec!["tk_g1".into()];
    app.pb.current = Some("tk_g1".into());
    app.pb.playing = true;

    // The real two-second PCM source outlasts its one-second catalogue entry.
    // Unit-test output captures decoded PCM without opening a sound device.
    let scratch = tempfile::tempdir().unwrap();
    let path = scratch.path().join("longer-than-metadata.wav");
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&32036u32.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&8000u32.to_le_bytes());
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&32000u32.to_le_bytes());
    wav.resize(32044, 0);
    std::fs::write(&path, wav).unwrap();
    app.audio
        .load(&media::source::path_to_file_uri(&path).unwrap());
    app.audio.play();

    let deadline = tilo_test_env::hang_deadline();
    let mut furthest_position = 0.0_f64;
    while app.pb.playing {
        app.tick();
        assert!(app.playback_error.is_none(), "{:?}", app.playback_error);
        furthest_position = furthest_position.max(app.playhead_ms);
        assert!(
            ui::time::now() < deadline,
            "the real source must reach EOS: {}",
            app.audio.diagnostics()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        furthest_position >= 1800.0,
        "metadata must not stop the two-second decoder at {furthest_position}ms"
    );
    assert_eq!(app.pb.queue_index, 0);
}

#[test]
#[cfg(feature = "audio")]
#[serial_test::serial(audio_backend)]
fn media_handoff_waits_for_decoder_but_preserves_paused_preroll() {
    let (mut app, _) = app_with_doc();
    let title = format!("handoff-init-{}", uuid::Uuid::new_v4());
    app.lib
        .tracks
        .get_mut("tk_g1")
        .unwrap()
        .title
        .clone_from(&title);
    app.pb.current = Some("tk_g1".into());
    app.pb.playing = false;
    app.pb.volume = 50;
    // A real, silent PCM file avoids both network access and audible noise.
    let scratch = tempfile::tempdir().unwrap();
    let path = scratch.path().join("paused.wav");
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&16036u32.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&8000u32.to_le_bytes());
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.resize(16044, 0);
    std::fs::write(&path, wav).unwrap();
    app.audio
        .load(&media::source::path_to_file_uri(&path).unwrap());
    app.audio.pause();
    app.publish_handoff();
    assert!(
        media::handoff::playback_sessions()
            .iter()
            .all(|session| session.title != title)
    );
    assert!(
        app.needs_animation_frame(),
        "paused initialization needs ticks"
    );

    let deadline = tilo_test_env::hang_deadline();
    while app.audio.frames_rendered() == 0 || app.audio.needs_poll() {
        app.tick();
        assert!(app.playback_error.is_none(), "{:?}", app.playback_error);
        assert!(
            ui::time::now() < deadline,
            "paused decoder failed to preroll: {}",
            app.audio.diagnostics()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!app.audio.is_playing_real());
    app.publish_handoff();
    let session = media::handoff::playback_sessions()
        .into_iter()
        .find(|session| session.title == title)
        .expect("a decoded paused source can acknowledge handoff");
    assert!(!session.playing);
    app.audio.stop();
    app.publish_handoff();
    assert!(
        media::handoff::playback_sessions()
            .iter()
            .all(|session| session.title != title)
    );
}

// ---- synthetic winit events (drive `handle_event` like the real loop) --
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::NamedKey;

/// Where the row of track `id` stands in the page's track tables.
fn track_row(app: &MusicApp, id: &str) -> Rect {
    app.track_tables
        .iter()
        .find_map(|slot| {
            let index = slot.table.rows().iter().position(|row| row.id == id)?;
            slot.table.row_rect(index)
        })
        .unwrap_or_else(|| panic!("track {id} has a row in view"))
}

/// Bring the pointer over the row of track `id` so it shows its actions,
/// redraw with `draw`, and return where its control `name` stands.
fn track_control(
    app: &mut MusicApp,
    doc: &mut AutoCommit,
    id: &str,
    name: &str,
    draw: impl Fn(&mut MusicApp, &mut AutoCommit),
) -> Rect {
    let row = track_row(app, id);
    send(
        app,
        &cursor_at(Point::new(row.x0 + row.width() * 0.3, row.center().y)),
        doc,
    );
    draw(app, doc);
    app.track_tables
        .iter()
        .find_map(|slot| {
            let index = slot.table.rows().iter().position(|row| row.id == id)?;
            slot.table.control_rect(index, name)
        })
        .unwrap_or_else(|| panic!("track {id} shows its {name} control under the pointer"))
}

/// Press and release the left button on the row of track `id`.
fn click_track(app: &mut MusicApp, doc: &mut AutoCommit, id: &str) {
    let row = track_row(app, id);
    send(
        app,
        &cursor_at(Point::new(row.x0 + row.width() * 0.3, row.center().y)),
        doc,
    );
    send(app, &press(MouseButton::Left), doc);
    send(app, &released(MouseButton::Left), doc);
}

fn cursor_at(p: Point) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(p.x, p.y),
    }
}
fn press(button: MouseButton) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state: ElementState::Pressed,
        button,
    }
}
/// Drive one event through the production `handle_event` path.
/// Send a row-control click through the same custody and action runner as
/// a live window, retaining the actual Music root and canonical document.
pub(super) fn hosted_click(app: &mut MusicApp, doc: &mut AutoCommit, at: Point) {
    let mut state = ui::State::new();
    state.adopt_doc(std::mem::take(doc));
    let origin = app.get_rect().origin();
    let owned = std::mem::take(app);
    let mut frame = ui::headless::FrameHarness::for_root(state, owned, (1280, 800), Theme::dark());
    let hosted_origin = frame.root::<MusicApp>().get_rect().origin();
    frame.click_at(
        at.x + hosted_origin.x - origin.x,
        at.y + hosted_origin.y - origin.y,
    );
    frame.relayout();
    let (owned, state) = frame.root_and_state_mut::<MusicApp>();
    *app = std::mem::take(owned);
    *doc = std::mem::take(&mut state.doc);
}

fn send(app: &mut MusicApp, ev: &WindowEvent, doc: &mut AutoCommit) -> bool {
    send_with(app, ev, winit::event::Modifiers::default(), doc)
}

/// Drive one event through `handle_event` with explicit modifiers held.
fn send_with(
    app: &mut MusicApp,
    ev: &WindowEvent,
    mods: winit::event::Modifiers,
    doc: &mut AutoCommit,
) -> bool {
    let mut taffy = TaffyTree::<()>::new();
    let node = taffy.new_leaf(Style::default()).unwrap();
    let mut handled = false;
    let mut focus = None;
    let root = automerge::ROOT;
    app.handle_event(
        ev,
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &mods,
            state: ui::widget::Edits::lend(doc),
            path: &root,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: node,
            taffy: &taffy,
        },
    );
    handled
}

fn released(button: MouseButton) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state: ElementState::Released,
        button,
    }
}

fn wheel(lines_y: f32) -> WindowEvent {
    WindowEvent::MouseWheel {
        device_id: winit::event::DeviceId::dummy(),
        delta: winit::event::MouseScrollDelta::LineDelta(0.0, lines_y),
        phase: winit::event::TouchPhase::Moved,
    }
}

/// A character keypress event (logical key + committed text). like the other app event-path tests.
fn char_key(c: &str) -> WindowEvent {
    use winit::keyboard::Key;
    ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        Key::Character(c.into()),
        Some(c.into()),
        ElementState::Pressed,
    ))
}

/// A named keypress event (Enter, Escape, Space, arrows …).
fn named_key(named: winit::keyboard::NamedKey) -> WindowEvent {
    use winit::keyboard::Key;
    ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        Key::Named(named),
        None,
        ElementState::Pressed,
    ))
}

fn shift_mods() -> winit::event::Modifiers {
    winit::event::Modifiers::from(winit::keyboard::ModifiersState::SHIFT)
}

/// The captured click rect (center) for the first hit matching `action`,
/// so tests can click a control the real draw pass registered.
fn hit_center(app: &MusicApp, action: &Action) -> Option<Point> {
    app.hits
        .iter()
        .find(|h| &h.action == action)
        .map(|h| h.rect.center())
}

#[test]
fn home_source_picker_keyboard_activation_opens_choices_without_sign_in() {
    use ui::framework::testing::TestApp;
    let mut app = TestApp::<crate::resource::MusicShell>::new().unwrap();
    app.harness()
        .relayout_themed((360, 900), ui::Theme::high_contrast().with_text_scale(2.0))
        .unwrap();
    let before = app.read_document(|doc| doc.clone().get_heads());
    app.harness()
        .perform_semantic_named("music-sources", None, &ui::a11y::Action::Focus)
        .unwrap();
    app.harness().key("Enter");
    assert_eq!(app.surface_mut::<MusicApp>().menu_actions.len(), 2);
    assert!(
        app.surface_mut::<MusicApp>().source_invocation.is_none(),
        "opening the source menu never starts OAuth"
    );
    assert!(app.surface_mut::<MusicApp>().spotify_jobs.is_empty());
    app.harness().key("Escape");
    assert_eq!(app.read_document(|doc| doc.clone().get_heads()), before);
}

#[test]
fn spotify_keyboard_creation_and_escape_use_accessible_shared_controls() {
    use ui::framework::testing::TestApp;
    for scale in [1.0, 2.0] {
        let mut app = TestApp::<crate::resource::MusicShell>::new().unwrap();
        app.surface_mut::<MusicApp>().snap_spotify("spotify-home");
        app.surface_mut::<MusicApp>().on_nav(View::Playlists);
        app.harness()
            .relayout_themed(
                (360, 900),
                ui::Theme::high_contrast().with_text_scale(scale),
            )
            .unwrap();
        assert_eq!(app.surface_mut::<MusicApp>().source_widget_mode(), 3);
        let accessible = app.harness().focus_access_nodes();
        let label = ui::i18n!("music-spotify-create-playlist");
        assert!(
            accessible
                .iter()
                .any(|node| node.name.as_deref() == Some(label.as_str())
                    && node.state.focusable
                    && !node.state.disabled),
            "creation remains keyboard accessible in Playlists: {accessible:?}"
        );
        let before = app.read_document(|doc| doc.clone().get_heads());
        app.harness()
            .perform_semantic_named(
                "music-spotify-create-playlist",
                None,
                &ui::a11y::Action::Focus,
            )
            .unwrap();
        app.harness().key("Enter");
        assert!(
            app.surface_mut::<MusicApp>().name_prompt.is_some(),
            "host keyboard activation invokes the published effect and opens the shared dialog"
        );
        app.harness().key("Escape");
        assert!(app.surface_mut::<MusicApp>().name_prompt.is_none());
        assert_eq!(
            app.read_document(|doc| doc.clone().get_heads()),
            before,
            "cancelling never creates a local or provider playlist"
        );
        assert!(app.surface_mut::<MusicApp>().spotify_jobs.is_empty());
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn transport_play_pause_seek_and_repeat_stay_local_and_honest() {
    // Phase `toggle_play_stays_local`.
    {
        let (mut app, mut doc) = app_with_doc();
        // Play/pause needs a loaded track (see `play_with_nothing_loaded_is_noop`).
        app.pb.current = Some("tk_g1".into());
        let was = app.pb.playing;
        let heads = doc.get_heads();
        app.dispatch(Action::TogglePlay, &mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(app.pb.playing, !was);
        assert_eq!(doc.get_heads(), heads, "transport does not sync");
    }

    // Phase `play_with_nothing_loaded_is_noop`.
    {
        let (mut app, mut doc) = app_with_doc();
        // Nothing queued, nothing current, stopped.
        assert!(app.pb.current.is_none());
        assert!(app.pb.queue.is_empty());
        assert!(!app.pb.playing);
        // Hitting play with an empty queue / no current track must do nothing —
        // never flip to a fabricated "playing" state.
        app.dispatch(Action::TogglePlay, &mut ui::widget::Edits::lend(&mut doc));
        assert!(!app.pb.playing, "play with nothing loaded stays stopped");
        assert!(app.pb.current.is_none());
    }

    // Phase `play_action_sets_queue_and_current`.
    {
        let (mut app, mut doc) = app_with_doc();
        let queue: Vec<String> = vec!["tk_g1".into(), "tk_g2".into(), "tk_g3".into()];
        app.dispatch(
            Action::Play {
                queue: queue.clone().into(),
                index: 1,
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(app.pb.current.as_deref(), Some("tk_g2"));
        assert_eq!(app.pb.queue, queue);
        assert!(app.pb.playing);
    }

    // Phase `absent_audio_source_does_not_advance_a_decorative_playhead`.
    {
        let (mut app, _doc) = app_with_doc();
        app.pb.current = Some("tk_g1".into());
        app.pb.playing = true;
        app.playhead_ms = 42_000.0;
        app.tick();
        assert_eq!(
            app.playhead_ms, 42_000.0,
            "no source means no fake progress"
        );
    }

    // Phase `next_advances_and_wraps_with_repeat_all`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Play {
                queue: vec!["a".into(), "b".into()].into(),
                index: 1,
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.pb.repeat = RepeatMode::All;
        app.dispatch(Action::Next, &mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(app.pb.queue_index, 0);
    }

    // Phase `settled_paused_library_does_not_request_redundant_redraws`.
    {
        use ui::widget::Widget as _;

        let (mut app, _doc) = app_with_doc();
        app.pb.playing = false;
        app.toast = None;
        app.live_search_pending = None;
        app.resolve_in_flight = 0;
        app.downloads.clear();
        app.mark_drawn();
        assert!(!app.needs_redraw(), "settled UI does not redraw at vsync");
        assert!(
            !app.needs_animation_frame(),
            "no playback or work in flight means no frame pump"
        );
        app.dirty = true;
        assert!(app.needs_redraw(), "a real visual mutation still repaints");
    }

    // Phase `repeat_one_restarts_on_auto_advance`.
    {
        let (mut app, _doc) = app_with_doc();
        app.pb.queue = vec!["tk_g1".into(), "tk_g2".into()];
        app.pb.queue_index = 0;
        app.pb.current = Some("tk_g1".into());
        app.pb.repeat = RepeatMode::One;
        app.playhead_ms = 5000.0;
        app.advance(true);
        assert_eq!(app.pb.queue_index, 0);
        assert_eq!(app.playhead_ms, 0.0);
    }

    // Phase `seek_sets_playhead`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.pb.current = Some("tk_g1".into()); // 215s
        app.dispatch(
            Action::SeekFrac(0.5),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!((app.playhead_ms - 215_000.0 * 0.5).abs() < 1.0);
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn request_identities_keep_stale_responses_from_winning() {
    // Phase `stale_resolve_failure_cannot_stop_a_newer_a_to_b_to_a_intent`.
    {
        let (mut app, _doc) = app_with_doc();
        // The second A has a distinct request id from the first A. This is the
        // race a plain track-id comparison gets wrong.
        app.pb.current = Some("tk_g1".into());
        app.pb.playing = true;
        app.active_resolve_request = Some(3);
        app.resolve_in_flight = 1;

        app.apply_resolve_failure("tk_g1", 1, "old A failed");
        assert!(app.pb.playing, "the obsolete A failure is ignored");
        assert_eq!(app.active_resolve_request, Some(3));
        assert_eq!(app.resolve_in_flight, 1);

        app.apply_resolve_failure("tk_g1", 3, "new A failed");
        assert!(!app.pb.playing, "the active failure settles transport");
        assert_eq!(app.resolve_in_flight, 0);
        assert!(
            app.playback_error
                .as_deref()
                .is_some_and(|message| message.contains("new A failed")),
            "the current failure remains visible beyond a transient worker note"
        );
    }

    // Phase `exhausted_request_identity_space_fails_closed_without_aliasing_max`.
    {
        let (mut app, _doc) = app_with_doc();
        app.next_request_id = u64::MAX;
        assert_eq!(
            app.next_worker_request_id(),
            None,
            "MAX is never reused as a new request identity"
        );
        assert_eq!(app.next_request_id, u64::MAX, "allocator never wraps");

        // Search has a visible settled failure rather than a pending spinner
        // when no fresh identity can be allocated.
        "anything".clone_into(&mut app.search_query);
        app.request_live_search();
        assert!(app.live_search_pending.is_none());
        assert_eq!(app.active_search_request, None);
        assert!(app.search_error.is_some());
    }

    // Phase `stale_search_response_cannot_replace_the_current_query`.
    {
        let (mut app, _doc) = app_with_doc();
        "new query".clone_into(&mut app.search_query);
        app.active_search_request = Some(8);
        let old = SearchBundle {
            tracks: vec![TrackHit {
                video_id: "old-result".into(),
                title: "Old result".into(),
                artist: "Artist".into(),
                artist_id: Some("artist".into()),
                album: Some("Album".into()),
                album_id: Some("album".into()),
                duration_s: 1,
                cover_url: String::new(),
            }],
            ..SearchBundle::default()
        };
        app.apply_search_results("old query", 7, &old);
        assert!(
            app.live_bundle.is_none(),
            "old query cannot populate results"
        );

        app.apply_search_results("new query", 8, &old);
        assert_eq!(
            app.live_bundle.as_ref().map(|(query, _)| query.as_str()),
            Some("new query"),
            "only the current query and request identity may update the view"
        );
    }

    // Phase `current_search_failure_settles_loading_and_exposes_retry_state`.
    {
        let (mut app, _doc) = app_with_doc();
        "unavailable query".clone_into(&mut app.search_query);
        app.active_search_request = Some(4);
        app.live_search_pending = Some("unavailable query".into());
        app.apply_search_failure("unavailable query", 4, "network unavailable");

        assert!(app.live_search_pending.is_none(), "failure ends busy state");
        assert!(app.active_search_request.is_none());
        assert_eq!(
            app.search_error.as_ref().map(|(query, _)| query.as_str()),
            Some("unavailable query")
        );
        app.mark_drawn();
        assert!(
            !app.needs_animation_frame(),
            "a settled search error never leaves an idle redraw loop"
        );
    }

    // Phase `offline_home_fetch_reports_unavailable_and_stops_the_skeleton`.
    // A Home fetch that cannot be served must settle on the honest empty state
    // rather than shimmer a loading skeleton (and spin frames) forever.
    //
    // The source is pinned to the bundled offline backend, whose `home()`
    // errors, so `HomeUnavailable` is this test's subject rather than a
    // coincidence. It used to rely on the build having no live backend — but
    // `live` is a *default* feature, so an ordinary build talks to real
    // `YouTube` Music: on a connected machine Home *loads*, `home_failed` never
    // flips, and the test cannot reach its subject at all. What it observed was
    // the host's network, and it reported a working Home fetch as a failure.
    {
        // The real default startup: an empty library, network allowed.
        let mut app = MusicApp::new();
        let mut doc = AutoCommit::new();
        doc::seed(&mut doc, &Library::default());
        app.reload(&doc);
        assert!(
            app.lib.tracks.is_empty(),
            "default startup has an empty library"
        );

        // Kick the real Home fetch through the worker + backend, with the source
        // pinned so the outcome is the app's and not the network's.
        app.use_worker(crate::stream::StreamWorker::spawn_offline());
        app.ensure_home();
        assert!(app.home_requested);
        assert!(
            app.home_is_loading(),
            "a Home request in flight reads as loading"
        );

        // Pump the worker until it reports Home unavailable (the offline backend
        // errors on `home()`), exercising stream.rs → drain end to end.
        let mut settled = false;
        for _ in 0..300 {
            app.drain_worker();
            if app.home_failed {
                settled = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(settled, "the offline Home fetch reports unavailable");
        assert!(
            !app.home_is_loading(),
            "Home stops loading — no endless skeleton / frame spin"
        );
        assert!(
            !app.needs_animation_frame(),
            "an unavailable, idle Home requests no more frames"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn assistive_routes_use_the_same_reducers_and_actions() {
    // Phase `accessibility_transport_routes_to_the_same_playback_reducer`.
    {
        use ui::a11y::Action as A11yAction;
        use ui::widget::Widget as _;

        let (mut app, mut doc) = app_with_doc();
        app.pb.current = Some("tk_g1".into());
        let tree = app.accessibility().expect("music semantics");
        assert_eq!(tree.name.as_deref(), Some("Music"));
        let player = tree
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Music player"))
            .expect("transport has its own semantic group");
        assert!(
            player
                .children
                .iter()
                .any(|node| node.action_target.as_deref() == Some("music.toggle-play"))
        );
        assert!(app.perform_semantic_action(
            "music.toggle-play",
            &A11yAction::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert!(
            app.pb.playing,
            "semantic action uses the live transport path"
        );
    }

    // Phase `empty_home_is_honest_and_its_search_action_is_accessible`.
    {
        use ui::a11y::Action as A11yAction;
        use ui::widget::Widget as _;

        let mut app = MusicApp::new();
        app.disconnect();
        let mut doc = AutoCommit::new();
        draw_once(&mut app, &mut doc);

        assert!(app.lib.tracks.is_empty());
        assert!(app.lib.albums.is_empty());
        assert!(app.lib.artists.is_empty());
        assert!(app.lib.playlists.is_empty());
        assert!(
            app.hits.iter().any(|hit| hit.action == Action::OpenSearch),
            "the empty-state Search button is a real action"
        );

        let tree = app.accessibility().expect("music semantics");
        let page = tree
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Home"))
            .expect("active page is exposed");
        let empty_library = page
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Empty music library"))
            .expect("empty state is separate from transport semantics");
        let search = empty_library
            .children
            .iter()
            .find(|node| node.action_target.as_deref() == Some("music.open-search"))
            .expect("empty-state Search button has an accessible action");
        assert_eq!(search.name.as_deref(), Some("Search music"));
        assert!(app.perform_semantic_action(
            "music.open-search",
            &A11yAction::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert!(
            app.search_opener.is_pending(),
            "it opens the platform search"
        );
    }

    // Phase `active_music_page_is_bounded_and_rows_route_to_real_actions`.
    {
        use ui::a11y::Action as A11yAction;
        use ui::widget::Widget as _;

        let (mut app, mut doc) = app_with_doc();
        app.view = View::Songs;
        let tree = app.accessibility().expect("music semantics");
        let page = tree
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Songs"))
            .expect("active Songs page is exposed");
        let list = page
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Songs items"))
            .expect("song rows are exposed");
        assert!(!list.children.is_empty());
        assert!(list.children.len() <= render::ACCESSIBLE_PAGE_LIMIT);
        let play = list.children[0]
            .children
            .iter()
            .find_map(|node| node.action_target.as_deref())
            .expect("track row has a semantic play action")
            .to_string();
        assert!(app.perform_semantic_action(
            &play,
            &A11yAction::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert!(
            app.pb.current.is_some(),
            "semantic play uses the playback reducer"
        );
    }

    // Phase `playlist_semantics_expose_stable_edit_actions`.
    {
        use ui::a11y::Action as A11yAction;
        use ui::widget::Widget as _;

        let (mut app, mut doc) = app_with_doc();
        let playlist = app.lib.playlists[0].clone();
        let first = playlist.track_ids[0].clone();
        app.view = View::Playlist(playlist.id.clone());
        let tree = app.accessibility().expect("music semantics");
        let page = tree
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some(playlist.name.as_str()))
            .expect("playlist page");
        let list = page
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some(format!("{} items", playlist.name).as_str()))
            .expect("playlist rows");
        let row = &list.children[0];
        assert!(row.key.is_some(), "membership identity survives reordering");
        let move_down = row
            .children
            .iter()
            .find(|node| {
                node.action_target
                    .as_deref()
                    .is_some_and(|target| target.starts_with("music.move-down-playlist-track."))
            })
            .and_then(|node| node.action_target.clone())
            .expect("move down action");
        assert!(app.perform_semantic_action(
            &move_down,
            &A11yAction::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(app.lib.playlist(&playlist.id).unwrap().track_ids[1], first);
    }

    // Phase `assistive_search_edits_and_navigation_use_normal_routes`.
    {
        use ui::a11y::Action as A11yAction;
        use ui::widget::Widget as _;

        let (mut app, mut doc) = app_with_doc();
        assert!(app.perform_semantic_action(
            "music.open-search",
            &A11yAction::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert!(
            app.search_opener.is_pending(),
            "the assistive search action opens the platform search"
        );
        assert!(
            ui::widget::controls::ActionControls::take_control_invocation(&mut app).is_some(),
            "the request leaves through the app's control intents"
        );

        assert!(app.perform_semantic_action(
            "music.nav.albums",
            &A11yAction::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(app.view, View::Albums);
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn offline_downloads_and_media_cache_are_honest_and_device_local() {
    // Phase `offline_download_request_is_honest_and_device_local`.
    {
        let (mut app, mut doc) = app_with_doc();
        assert_eq!(app.cache_state("tk_g2"), CacheState::Streamed);
        let heads = doc.get_heads();
        let keep = crate::actions::effect_payload(&crate::actions::MusicEffect::KeepOffline {
            tracks: vec!["tk_g2".into()],
        });
        assert!(matches!(
            app.start_effect(ui::tilo_actions::InvocationId::new(), &keep)
                .expect("request queues a real resolver/download lifecycle"),
            ui::framework::Effected::Pending
        ));
        assert!(matches!(
            app.cache_state("tk_g2"),
            CacheState::Downloading(_)
        ));
        assert_eq!(doc.get_heads(), heads);
    }

    // Phase `offline_effect_refuses_keep_but_batch_evicts_legacy_metadata_once`.
    {
        let (mut app, _doc) = app_with_doc();
        app.device_cache.insert("tk_g1".into(), CacheState::Cached);
        app.device_cache.insert("tk_g2".into(), CacheState::Cached);
        let keep = crate::actions::effect_payload(&crate::actions::MusicEffect::KeepOffline {
            tracks: vec!["tk_g1".into(), "tk_g2".into()],
        });
        assert!(
            app.start_effect(ui::tilo_actions::InvocationId::new(), &keep)
                .is_ok()
        );
        let evict = crate::actions::effect_payload(&crate::actions::MusicEffect::RemoveOffline {
            tracks: vec!["tk_g1".into(), "tk_g2".into()],
        });
        assert!(matches!(
            app.start_effect(ui::tilo_actions::InvocationId::new(), &evict)
                .unwrap(),
            ui::framework::Effected::Done(_)
        ));
        assert!(
            app.device_cache.is_empty(),
            "one operation evicts the full batch"
        );
    }

    // Phase `verified_media_cache_reopens_after_restart_without_a_resolver`.
    {
        let (mut app, _doc) = app_with_doc();
        let root = std::env::temp_dir().join(format!(
            "tilo-music-offline-playback-音 # %-{}",
            std::process::id()
        ));
        app.media_cache_root = root.clone();
        app.media_cache
            .install_bytes(&root, "tk_g1", b"durable media bytes", 1024)
            .expect("install complete local media");
        // Model a restart: reload the manifest/object pair into a fresh local
        // cache projection, then start the known track with no worker at all.
        app.media_cache = crate::storage::load_media_cache_from(&root);
        app.device_cache.insert("tk_g1".into(), CacheState::Cached);
        app.pb.current = Some("tk_g1".into());
        app.pb.queue = vec!["tk_g1".into()];
        app.pb.playing = true;
        assert_eq!(
            app.start_stream("tk_g1"),
            None,
            "no network resolver is queued"
        );
        assert!(
            app.audio
                .current_uri()
                .is_some_and(|uri| uri.starts_with("file://")),
            "the audio engine reads the durable local object"
        );
        let file_path = url::Url::parse(app.audio.current_uri().expect("cached URI"))
            .expect("valid URL")
            .to_file_path()
            .expect("native file path");
        assert!(file_path.starts_with(&root));
        assert_eq!(
            std::fs::read(file_path).expect("cached bytes"),
            b"durable media bytes"
        );
        app.audio.stop();
        drop(app);
        std::fs::remove_dir_all(root).expect("release cached audio files");
    }

    // Phase `downloads_rows_expose_direct_cancel_remove_and_retry_actions`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.view = View::Downloads;
        app.update_device_cache("tk_g1", CacheState::Downloading(0.32));
        app.update_device_cache("tk_g2", CacheState::Cached);
        app.offline_errors
            .insert("tk_g3".into(), "network ended".into());
        draw_once(&mut app, &mut doc);

        // Each row's one action, under the pointer, is the toolkit's button.
        for (track, name) in [
            ("tk_g1", "Cancel download"),
            ("tk_g2", "Remove download"),
            ("tk_g3", "network ended"),
        ] {
            let control = track_control(&mut app, &mut doc, track, "offline", draw_once);
            send(&mut app, &cursor_at(control.center()), &mut doc);
            assert_eq!(ui::widget::Widget::tooltip(&app), Some(name), "{track}");
        }
        // Pressing the failed row's control retries it.
        let retry = track_control(&mut app, &mut doc, "tk_g3", "offline", draw_once);
        hosted_click(&mut app, &mut doc, retry.center());
        assert!(
            !app.offline_errors.contains_key("tk_g3"),
            "Retry clears the failure"
        );
    }

    // Phase `large_text_download_failure_exposes_full_hover_and_assistive_recovery`.
    {
        use ui::a11y::Action as A11yAction;
        use ui::widget::Widget as _;

        let (mut app, mut doc) = app_with_doc();
        app.view = View::Downloads;
        app.offline_errors
            .insert("tk_g3".into(), "connection reset while downloading".into());
        draw_once_large_text(&mut app, &mut doc);

        let retry = track_control(&mut app, &mut doc, "tk_g3", "offline", draw_once_large_text);
        send(&mut app, &cursor_at(retry.center()), &mut doc);
        assert_eq!(
            app.tooltip(),
            Some("connection reset while downloading"),
            "hover exposes the unelided transport failure"
        );

        let tree = app.accessibility().expect("music semantics");
        let page = tree
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Downloads"))
            .expect("Downloads page");
        let list = page
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Downloads items"))
            .expect("Downloads list");
        let failed = list
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Slow Tide"))
            .expect("failed track is retained in assistive output");
        assert!(
            failed.description.as_deref().is_some_and(
                |description| description.contains("connection reset while downloading")
            ),
            "assistive description contains the complete failure"
        );
        let retry = failed
            .children
            .iter()
            .find_map(|node| node.action_target.as_deref())
            .expect("failed track has an assistive Retry action")
            .to_string();
        assert!(app.perform_semantic_action(
            &retry,
            &A11yAction::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert!(!app.offline_errors.contains_key("tk_g3"));
    }

    // Phase `prefetch_only_runs_on_a_stable_connection`.
    {
        let (mut app, _doc) = app_with_doc();
        app.playback_loaded = true;
        app.pb.queue = vec!["tk_g1".into(), "tk_a1".into()]; // tk_a1 has bundled lyrics
        app.pb.queue_index = 0;
        app.pb.current = Some("tk_g1".into());
        // Metered → suppressed.
        app.force_metered = Some(true);
        assert!(
            !app.maybe_prefetch(),
            "prefetch is suppressed on a metered link"
        );
        assert!(app.prefetched_for.is_none());
        assert!(!app.lyrics_cache.contains_key("tk_a1"));
        // Stable → runs, warming the next queued track's lyrics into the cache.
        app.force_metered = Some(false);
        assert!(app.maybe_prefetch(), "prefetch runs on a stable connection");
        assert_eq!(app.prefetched_for.as_deref(), Some("tk_g1"));
        assert!(
            app.lyrics_cache.contains_key("tk_a1"),
            "the next queued track's lyrics are prefetched into the shared cache"
        );
        // Idempotent within the same current track.
        assert!(
            !app.maybe_prefetch(),
            "prefetch fires once per track change"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn sidebar_docks_overlays_collapses_and_follows_navigation() {
    // Phase `nav_and_back`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Nav(View::Albums),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(app.view, View::Albums);
        app.dispatch(
            Action::Nav(View::Album("al_glasslight".into())),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(app.view, View::Album("al_glasslight".into()));
        app.dispatch(Action::Back, &mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(app.view, View::Albums);
    }

    // Phase `layout_docks_when_wide_overlays_when_narrow`.
    {
        let mut app = MusicApp::new();
        sized(&mut app, 1280.0);
        assert_eq!(app.sidebar_layout(), SplitLayout::Docked);
        sized(&mut app, 480.0);
        assert_eq!(app.sidebar_layout(), SplitLayout::Overlay);
        // Boundary is inclusive at the drawer's breakpoint → docked.
        let breakpoint = app.drawer.resolved_breakpoint();
        sized(&mut app, breakpoint);
        assert_eq!(app.sidebar_layout(), SplitLayout::Docked);
        sized(&mut app, breakpoint - 1.0);
        assert_eq!(app.sidebar_layout(), SplitLayout::Overlay);
    }

    // Phase `sidebar_open_docked_closed_overlay_by_default`.
    {
        let mut app = MusicApp::new();
        sized(&mut app, 1280.0);
        assert!(app.drawer.is_open(), "wide: docked + open");
        assert!(
            app.content_inset() > 0.0,
            "wide content is inset by the sidebar"
        );

        sized(&mut app, 480.0);
        assert!(!app.drawer.is_open(), "narrow: overlay + closed");
        assert_eq!(app.content_inset(), 0.0, "narrow content is full-width");
    }

    // Phase `hamburger_toggles_overlay_sidebar`.
    {
        let (mut app, mut doc) = app_with_doc();
        sized(&mut app, 480.0); // overlay
        assert!(!app.drawer.is_open());
        app.dispatch(
            Action::ToggleSidebar,
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(app.drawer.is_open(), "hamburger opens the flap");
        app.dispatch(
            Action::ToggleSidebar,
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(!app.drawer.is_open(), "toggling again closes it");
    }

    // Phase `navigating_dismisses_overlay_sidebar`.
    {
        let (mut app, mut doc) = app_with_doc();
        sized(&mut app, 480.0); // overlay
        app.drawer.open();
        assert!(app.drawer.is_open());
        // Selecting a destination from the open flap closes it.
        app.dispatch(
            Action::Nav(View::Albums),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(app.view, View::Albums);
        assert!(!app.drawer.is_open(), "selection dismisses the flap");
    }

    // Phase `navigating_keeps_docked_sidebar`.
    {
        let (mut app, mut doc) = app_with_doc();
        sized(&mut app, 1280.0); // docked
        app.dispatch(
            Action::Nav(View::Albums),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(app.drawer.is_open(), "docked sidebar persists across nav");
    }

    // Phase `close_action_dismisses_sidebar`.
    {
        let (mut app, mut doc) = app_with_doc();
        sized(&mut app, 480.0);
        app.drawer.open();
        app.dispatch(Action::CloseSidebar, &mut ui::widget::Edits::lend(&mut doc));
        assert!(!app.drawer.is_open());
    }

    // Phase `docked_collapse_toggles_and_reexpands`.
    {
        let (mut app, mut doc) = app_with_doc();
        sized(&mut app, 1280.0); // docked, open by default
        assert!(app.drawer.is_open());
        // Collapse via the (docked) toggle.
        app.dispatch(
            Action::ToggleSidebar,
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(!app.drawer.is_open(), "docked sidebar collapses");
        // Once collapsed, the toggle moves to the content's header so the
        // sidebar can be re-expanded.
        assert!(
            app.toggle_in_content(),
            "collapsed docked sidebar offers a re-expand affordance"
        );
        app.dispatch(
            Action::ToggleSidebar,
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(app.drawer.is_open(), "toggling again re-expands it");
    }

    // Phase `collapsed_docked_sidebar_clears_to_zero_width`.
    // Collapsing the docked sidebar clears it: after a settled collapse the
    // resolved sidebar rect has zero width (nothing left to paint), so no
    // stale sidebar pixels remain under the content.
    {
        let (mut app, mut doc) = app_with_doc();
        sized(&mut app, 1280.0);
        assert!(
            app.sidebar_rect().width() > 100.0,
            "open docked sidebar has width"
        );
        app.dispatch(
            Action::ToggleSidebar,
            &mut ui::widget::Edits::lend(&mut doc),
        );
        // Settle the collapse animation.
        app.drawer.snap(false);
        app.drawer.resolve(app.rect);
        assert!(
            app.sidebar_rect().width() < 0.5,
            "collapsed sidebar clears to zero width"
        );
        assert_eq!(
            app.content_inset(),
            0.0,
            "collapsed content reclaims the full width"
        );
    }
}

// ---- text truncation --------------------------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn large_text_rows_elide_and_keep_one_identity() {
    // Phase `elide_renders_nothing_when_no_glyph_fits`.
    {
        let mut tr = TextRenderer::new();
        // A width far too narrow for any character + ellipsis: render nothing,
        // never a stray "…" that would overflow into the next column.
        let style = Theme::dark().text(ui::tokens::TextRole::Label);
        let out = elide(&mut tr, "Some Long Album Title", &style, 2.0);
        assert!(
            out.is_empty(),
            "too-narrow column renders no text, got {out:?}"
        );
        assert!(
            !out.contains('\u{2026}'),
            "no lone ellipsis when nothing fits"
        );

        // A genuinely truncating (but non-degenerate) width keeps some glyphs
        // and appends the ellipsis, and the result stays within the budget.
        let mid_w = 90.0;
        let mid = elide(&mut tr, "Some Long Album Title", &style, mid_w);
        assert!(
            mid.ends_with('\u{2026}'),
            "a truncation still shows the ellipsis: {mid:?}"
        );
        assert!(
            mid.chars().count() > 1,
            "at least one real glyph precedes it: {mid:?}"
        );
        assert!(
            f64::from(tr.measure_style(&mid, None, &style).0) <= mid_w,
            "the elided string fits inside the column budget",
        );

        // Fits outright → returned unchanged, no ellipsis.
        let whole = elide(&mut tr, "OK", &style, 500.0);
        assert_eq!(whole, "OK");
    }

    // Phase `large_text_header_actions_stay_in_the_header`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.view = View::Browse;
        draw_once_large_text(&mut app, &mut doc);
        for (name, rect) in [
            ("search", app.search_button.get_rect()),
            ("open file", app.open_file_button.get_rect()),
        ] {
            assert!(
                rect.width() > 0.0 && rect.height() >= 28.0,
                "large-text {name} keeps a real target: {rect:?}"
            );
            assert!(
                rect.y1 <= app.body_rect.y0,
                "the {name} action sits in the header, not over the page"
            );
        }
    }

    // Phase `dense_track_rows_share_one_queue_snapshot`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.view = View::Songs;
        draw_at(&mut app, &mut doc, 1_280.0, 800.0);
        let table = app.track_tables.first().expect("the songs table");
        assert!(table.queue.len() > 1 && table.table.rows().len() == table.queue.len());
        let before = Arc::clone(&table.queue);
        draw_at(&mut app, &mut doc, 1_280.0, 800.0);
        assert!(
            Arc::ptr_eq(&before, &app.track_tables[0].queue),
            "the rows share one queue, kept across frames while it is the same"
        );
    }

    // Phase `large_text_entity_page_deduplicates_local_and_resolved_track_identity`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.snap_album_page();
        draw_once_large_text(&mut app, &mut doc);
        let queue = &app
            .track_tables
            .first()
            .expect("album exposes playable rows")
            .queue;
        assert_eq!(
            queue.len(),
            5,
            "live-style and local identities for the same recording render once"
        );
    }
}

// ---- platform search ---------------------------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn browse_and_the_live_catalogue_answer_platform_search() {
    // Phase `browse_cards_open_platform_search_for_their_genre`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.disconnect(); // hermetic
        app.playback_loaded = true;
        app.view = View::Browse;
        draw_once(&mut app, &mut doc);
        // A genre card is the shared card: a press on it opens search.
        let genre = app
            .lib
            .artists
            .values()
            .map(|artist| artist.genre.clone())
            .filter(|genre| !genre.is_empty())
            .min()
            .expect("a sample genre");
        let card = page_widget(&app, &format!("music.genre.{genre}"));
        send(&mut app, &cursor_at(card.center()), &mut doc);
        send(&mut app, &press(MouseButton::Left), &mut doc);
        send(&mut app, &released(MouseButton::Left), &mut doc);
        assert_eq!(
            app.view,
            View::Browse,
            "the app draws no results of its own"
        );
        let invocation = ui::widget::controls::ActionControls::take_control_invocation(&mut app)
            .expect("the card opens the platform search");
        assert!(
            invocation
                .arguments
                .values()
                .any(|value| value.as_str() == Some(genre.as_str())),
            "the search opens prefilled with {genre:?}: {:?}",
            invocation.arguments
        );
    }

    // Phase `the_app_draws_no_search_field_and_leaves_the_slash_key_alone`.
    {
        let (mut app, mut doc) = app_with_doc();
        let before = app.view.clone();
        send(&mut app, &char_key("/"), &mut doc);
        assert_eq!(app.view, before, "`/` belongs to the platform search");
        assert!(!app.search_opener.is_pending());
        draw_once(&mut app, &mut doc);
        let search = app.search_button.get_rect();
        assert!(
            search.width() > 0.0 && search.y1 <= app.body_rect.y0,
            "the header carries the search launcher: {search:?}"
        );
    }

    // Phase `live_catalogue_pages_arrive_after_the_worker_answers`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.playback_loaded = true;
        let query = ui::search::SearchQuery::new("daft punk");
        ui::framework::ResourceSurface::<crate::resource::Music, crate::actions::MusicEffect>::search_query(&mut app, &query);
        assert_eq!(
            app.pending_catalogue_query.as_deref(),
            Some("daft punk"),
            "the canonical query is delivered to the worker queue"
        );
        app.disconnect();
        app.pending_catalogue_query = None;
        app.search_query = "daft punk".into();
        app.active_search_request = Some(41);
        app.apply_search_results("daft punk", 40, &sample_bundle());
        assert!(app.live_bundle.is_none(), "a stale worker reply is ignored");
        app.apply_search_results("daft punk", 41, &sample_bundle());
        let provider = ui::framework::testing::search_source::<crate::resource::Music>(
            app.live_search_entries(),
        );
        let ready = ui::search::SearchProvider::search_page(&provider, &query, None, 100, 100);
        assert!(
            app.active_search_request.is_none(),
            "the matching worker reply completes the request"
        );
        let song = ready
            .results
            .iter()
            .find(|row| row.title == "One More Time")
            .expect("a song row");
        assert!(matches!(
            song.row,
            Some(ui::search::SearchRow::Media { .. })
        ));
        let album = ready
            .results
            .iter()
            .find(|row| row.kind == "Albums")
            .expect("an album row");

        // Activating a hit plays the song, or opens the album's page.
        invoke_hit(&mut app, song, &mut doc);
        assert_eq!(app.pb.current.as_deref(), Some("yt:vid_one"));
        invoke_hit(&mut app, album, &mut doc);
        assert_eq!(app.view, View::Album("MPREdiscovery".into()));
    }
}

#[test]
fn a_live_catalogue_like_uses_the_admitted_effect_and_one_undo() {
    let mut app = ui::framework::testing::TestApp::<crate::MusicShell>::new().unwrap();
    app.with_widget(|surface: &mut MusicApp| {
        surface.disconnect();
        surface.search_query = "daft punk".into();
        surface.active_search_request = Some(41);
        surface.apply_search_results("daft punk", 41, &sample_bundle());
    })
    .expect("the production surface accepts the actual worker reply");
    let providers = ui::search::capability::search_providers(app.harness().root_widget());
    assert_eq!(providers.len(), 1, "the platform owns Music search");
    let hits = providers[0].search("One More Time");
    let song = hits
        .iter()
        .find(|hit| hit.title == "One More Time")
        .expect("the real catalogue reply is in platform search");
    let row = song.action_address().unwrap().resource;
    assert_eq!(
        row.as_str(),
        crate::actions::track_resource_id("yt:vid_one")
    );
    assert!(!app.state().tracks.contains_key("yt:vid_one"));
    app.call_on(row.as_str(), "toggle-like", serde_json::json!({}))
        .unwrap();
    assert!(
        app.state().tracks.contains_key("yt:vid_one"),
        "actual song metadata is imported"
    );
    assert!(app.state().is_liked("yt:vid_one"));
    assert!(app.undo(), "the admitted effect is one authored undo step");
    assert!(!app.state().is_liked("yt:vid_one"));
    assert!(
        !app.state().tracks.contains_key("yt:vid_one"),
        "undo restores the entire effect"
    );
}

// ---- responsive sidebar -----------------------------------------------

/// Set the app's allocated width directly (normally done by `apply_layout`)
/// and resolve the sidebar drawer against it, as a real frame would.
fn sized(app: &mut MusicApp, w: f64) {
    app.rect = Rect::new(0.0, 0.0, w, 800.0);
    app.drawer.resolve(app.rect);
}

// ---- 1. sidebar collapse ----------------------------------------------

// ---- 2. context menu --------------------------------------------------

/// Open a track's context menu, returning the action index for a label.
fn menu_index(app: &MusicApp, label: &str) -> Option<usize> {
    app.context.items().iter().enumerate().find_map(|(i, it)| {
        if it.label() == Some(label) {
            Some(i)
        } else {
            None
        }
    })
}

// ---- 3. add album to library ------------------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn library_edits_like_save_follow_and_playlists_round_trip() {
    // Phase `toggle_save_album_round_trips`.
    {
        let (mut app, mut doc) = app_with_doc();
        assert!(!app.lib.is_album_saved("al_strata"));
        app.dispatch(
            Action::ToggleSaveAlbum("al_strata".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(app.lib.is_album_saved("al_strata"));
        assert!(doc::load_library(&doc).is_album_saved("al_strata"));
        app.dispatch(
            Action::ToggleSaveAlbum("al_strata".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(!app.lib.is_album_saved("al_strata"));
    }

    // Phase `toggle_like_round_trips_and_persists`.
    {
        let (mut app, mut doc) = app_with_doc();
        assert!(!app.lib.is_liked("tk_g2"));
        app.dispatch(
            Action::ToggleLike("tk_g2".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(
            app.lib.is_liked("tk_g2"),
            "like is reflected in the snapshot"
        );
        assert!(
            doc::load_library(&doc).is_liked("tk_g2"),
            "like persists to the doc"
        );
        // Liked view surfaces it.
        assert!(app.lib.liked_ids().contains(&"tk_g2".to_string()));
        app.dispatch(
            Action::ToggleLike("tk_g2".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(!app.lib.is_liked("tk_g2"));
    }

    // Phase `create_playlist_navigates_to_it`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.name_input = "Roadtrip".into();
        let before = app.lib.playlists.len();
        app.dispatch(
            Action::CreatePlaylist,
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(app.lib.playlists.len(), before + 1);
        assert!(
            matches!(app.view, View::Playlist(_)),
            "creating opens the new playlist"
        );
        let created = app.lib.playlists.iter().any(|p| p.name == "Roadtrip");
        assert!(created);
    }

    // Phase `add_remove_reorder_playlist_tracks`.
    {
        let (mut app, mut doc) = app_with_doc();
        // pl_morning seeds [tk_p1, tk_r1, tk_g2, tk_r3].
        app.dispatch(
            Action::AddToPlaylist {
                playlist: "pl_morning".into(),
                track: "tk_h2".into(),
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        let pl = app.lib.playlist("pl_morning").unwrap().clone();
        assert_eq!(
            pl.track_ids.last().map(std::string::String::as_str),
            Some("tk_h2")
        );
        app.dispatch(
            Action::RemoveFromPlaylist {
                playlist: "pl_morning".into(),
                track: "tk_g2".into(),
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(
            !app.lib
                .playlist("pl_morning")
                .unwrap()
                .track_ids
                .iter()
                .any(|t| t == "tk_g2")
        );
        app.dispatch(
            Action::MovePlaylistItem {
                playlist: "pl_morning".into(),
                from: 0,
                to: 1,
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(
            app.lib.playlist("pl_morning").unwrap().track_ids[1],
            "tk_p1"
        );
    }

    // Phase `rename_and_delete_playlist_actions`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.name_input = "Concentration".into();
        app.dispatch(
            Action::CommitRename("pl_focus".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(app.lib.playlist("pl_focus").unwrap().name, "Concentration");
        app.dispatch(
            Action::DeletePlaylist("pl_focus".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(app.lib.playlist("pl_focus").is_none());
    }

    // Phase `name_prompt_typing_and_enter_creates_playlist`.
    {
        let (mut app, mut doc) = app_with_doc();
        // A real rect so the shared dialog lays out and its embedded field can
        // take keystrokes (keyboard is forwarded only after `place_content`).
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.dispatch(
            Action::BeginNewPlaylist,
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(matches!(app.name_prompt, Some(NamePrompt::NewPlaylist)));
        // Typing routes through the shared ModalDialog into the real TextInput.
        for ch in ["J", "a", "z", "z"] {
            send(&mut app, &char_key(ch), &mut doc);
        }
        assert_eq!(
            app.name_dialog.as_ref().unwrap().dialog.content().value(),
            "Jazz",
            "keystrokes land in the dialog's embedded TextInput",
        );
        send(&mut app, &named_key(NamedKey::Enter), &mut doc);
        assert!(
            app.name_prompt.is_none(),
            "Enter commits and closes the prompt"
        );
        assert!(
            app.name_dialog.is_none(),
            "the dialog is torn down after commit"
        );
        let pl = app
            .lib
            .playlists
            .iter()
            .find(|p| p.name == "Jazz")
            .expect("Enter on the name prompt created the playlist");
        assert!(
            matches!(&app.view, View::Playlist(id) if id == &pl.id),
            "opens the freshly-created playlist"
        );
    }

    // Phase `add_to_playlist_new_playlist_includes_target_track`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.open_add_to_playlist_menu("tk_s2");
        let idx = menu_index(&app, "New playlist\u{2026}")
            .expect("picker offers a New playlist row") as i64;
        app.pending.store(idx, Ordering::SeqCst);
        app.drain_menu_selection(&mut ui::widget::Edits::lend(&mut doc));
        assert!(
            matches!(app.name_prompt, Some(NamePrompt::NewPlaylist)),
            "opens the name prompt"
        );
        assert_eq!(
            app.new_playlist_track.as_deref(),
            Some("tk_s2"),
            "remembers the track being added"
        );
        app.name_input = "Late Night".into();
        app.dispatch(
            Action::CreatePlaylist,
            &mut ui::widget::Edits::lend(&mut doc),
        );
        let pl = app
            .lib
            .playlists
            .iter()
            .find(|p| p.name == "Late Night")
            .expect("the playlist was created");
        assert!(
            pl.track_ids.contains(&"tk_s2".to_string()),
            "the target track was added to the new playlist"
        );
        // And it round-trips through the document, not just the snapshot.
        let saved = doc::load_library(&doc);
        let spl = saved
            .playlists
            .iter()
            .find(|p| p.name == "Late Night")
            .unwrap();
        assert!(
            spl.track_ids.contains(&"tk_s2".to_string()),
            "the add persists to the doc"
        );
    }

    // Phase `plain_new_playlist_is_empty`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::BeginNewPlaylist,
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.name_input = "Blank".into();
        app.dispatch(
            Action::CreatePlaylist,
            &mut ui::widget::Edits::lend(&mut doc),
        );
        let pl = app
            .lib
            .playlists
            .iter()
            .find(|p| p.name == "Blank")
            .unwrap();
        assert!(
            pl.track_ids.is_empty(),
            "a plain create (no target track) makes an empty playlist"
        );
    }

    // Phase `saved_album_syncs_via_doc`.
    {
        let (mut app, mut doc) = app_with_doc();
        let before = doc::load_library(&doc).is_album_saved("al_glasslight");
        app.dispatch(
            Action::ToggleSaveAlbum("al_glasslight".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        let after = doc::load_library(&doc).is_album_saved("al_glasslight");
        assert_ne!(
            before, after,
            "toggling flips the saved-album state in the synced doc"
        );
        let mut peer = AutoCommit::new();
        crate::sync::converge(&mut doc, &mut peer);
        assert_eq!(
            doc::load_library(&peer).is_album_saved("al_glasslight"),
            after,
            "the saved-album state syncs to another device via the CRDT"
        );
    }

    // Phase `following_an_artist_toggles_and_persists`.
    {
        let (mut app, mut doc) = app_with_doc();
        let artist = app.lib.artists.keys().next().cloned().unwrap();
        assert!(!app.lib.is_artist_followed(&artist));
        app.dispatch(
            Action::ToggleFollow(artist.clone()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(
            app.lib.is_artist_followed(&artist),
            "Follow adds the artist"
        );
        // Persisted to the CRDT — a fresh load from the document sees it.
        assert!(
            doc::load_library(&doc).followed_artists.contains(&artist),
            "follow round-trips through the document",
        );
        app.dispatch(
            Action::ToggleFollow(artist.clone()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(
            !app.lib.is_artist_followed(&artist),
            "toggling again unfollows"
        );
    }
}

// ---- 4. like toggle ---------------------------------------------------

// ---- 5. queue ops -----------------------------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn queue_edits_insert_remove_reorder_and_refuse_stale_rows() {
    // Phase `play_next_inserts_after_current`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Play {
                queue: vec!["a".into(), "b".into(), "c".into()].into(),
                index: 0,
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(app.pb.queue_index, 0);
        app.dispatch(
            Action::PlayNext("x".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(
            app.pb.queue,
            vec!["a", "x", "b", "c"],
            "play-next sits right after current"
        );
    }

    // Phase `remove_from_queue_drops_entry`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Play {
                queue: vec!["a".into(), "b".into(), "c".into()].into(),
                index: 1,
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        // Remove the entry before current → index shifts down, current stays "b".
        app.dispatch(
            Action::RemoveFromQueue(0),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(app.pb.queue, vec!["b", "c"]);
        assert_eq!(app.pb.current.as_deref(), Some("b"));
        assert_eq!(app.pb.queue_index, 0);
    }

    // Phase `move_queue_item_reorders_and_keeps_current`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Play {
                queue: vec!["a".into(), "b".into(), "c".into()].into(),
                index: 0,
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.dispatch(
            Action::MoveQueueItem { from: 2, to: 1 },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(app.pb.queue, vec!["a", "c", "b"]);
        assert_eq!(
            app.pb.current.as_deref(),
            Some("a"),
            "current track unchanged by a reorder below it"
        );
    }

    // Phase `canonical_queue_item_operation_rejects_a_stale_duplicate_row`.
    {
        let (mut app, doc) = app_with_doc();
        app.pb.queue = vec!["tk_g1".into(), "tk_g1".into(), "tk_g2".into()];
        app.pb.queue_index = 0;
        app.pb.current = Some("tk_g1".into());
        let resources = app.palette_tree(&doc);
        let queue = resources[0]
            .children
            .iter()
            .find(|resource| resource.id.as_str() == crate::actions::QUEUE_RESOURCE_ID)
            .expect("the local queue is published canonically");
        assert_eq!(queue.children.len(), 3);
        let duplicate = &queue.children[1];
        let remove = duplicate
            .actions
            .iter()
            .find(|action| action.id == "remove")
            .expect("second duplicate has its own remove operation");
        let payload = remove
            .build_json(&serde_json::Map::new())
            .expect("queue removal takes no arguments")
            .payload;
        assert!(matches!(
            app.start_effect(ui::tilo_actions::InvocationId::new(), &payload)
                .expect("the observed queue row is removable"),
            ui::framework::Effected::Done(_)
        ));
        assert_eq!(app.pb.queue, vec!["tk_g1", "tk_g2"]);
        assert!(
            app.start_effect(ui::tilo_actions::InvocationId::new(), &payload)
                .is_err(),
            "the old second-occurrence operation cannot remove a recycled row"
        );
    }
}

// ---- 6. playlist management -------------------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the test intentionally exercises the complete palette-to-resource-operation route"
)]
fn global_search_and_canonical_views_route_without_mutation() {
    // Phase `global_search_deep_links_route_to_the_real_view_and_play_path`.
    {
        use crate::search_type::SearchHit;
        let (mut app, mut doc) = app_with_doc();

        // Real ids straight from the seeded library.
        let album = app.lib.albums.keys().next().unwrap().clone();
        let artist = app.lib.artists.keys().next().unwrap().clone();
        let playlist = app.lib.playlists.first().unwrap().id.clone();
        let track = app.lib.tracks.keys().next().unwrap().clone();

        // An album deep-link opens the album page.
        assert!(app.open_deep_link(
            &SearchHit::album_link(&album),
            &mut ui::widget::Edits::lend(&mut doc)
        ));
        assert_eq!(app.view, View::Album(album));

        // An artist deep-link opens the artist page.
        assert!(app.open_deep_link(
            &SearchHit::artist_link(&artist),
            &mut ui::widget::Edits::lend(&mut doc)
        ));
        assert_eq!(app.view, View::Artist(artist));

        // A playlist deep-link opens the playlist page.
        assert!(app.open_deep_link(
            &SearchHit::playlist_link(&playlist),
            &mut ui::widget::Edits::lend(&mut doc)
        ));
        assert_eq!(app.view, View::Playlist(playlist));

        // A track deep-link actually plays that track through the real play path.
        assert!(app.open_deep_link(
            &SearchHit::track_link(&track),
            &mut ui::widget::Edits::lend(&mut doc)
        ));
        assert_eq!(app.pb.current.as_deref(), Some(track.as_str()));
        assert!(app.pb.queue.contains(&track));

        // A hostile / unknown link is rejected and changes nothing.
        let view_before = app.view.clone();
        assert!(!app.open_deep_link(
            "https://evil/track/x",
            &mut ui::widget::Edits::lend(&mut doc)
        ));
        assert_eq!(app.view, view_before);
    }

    // Phase `ctrl_k_search_provider_is_registered_and_activation_plays_and_navigates`.
    {
        // Exercise the real palette broker seam: a result address resolves in
        // the exported resource tree, queues a typed operation, and never uses
        // the legacy app callback.
        let (mut app, mut doc) = app_with_doc();
        let track = app.lib.tracks.values().next().unwrap().clone();
        let resources = app.palette_tree(&doc);
        let album = app.lib.albums.values().next().unwrap().clone();
        let artist_for_resolution = app.lib.artists.values().next().unwrap().clone();
        let playlist = app.lib.playlists.first().unwrap().clone();
        let provider = library_source(&app);
        for (query, expected) in [
            (
                track.title.as_str(),
                crate::actions::play_track_address(&track.id),
            ),
            (
                album.title.as_str(),
                crate::actions::open_album_address(&album.id),
            ),
            (
                artist_for_resolution.name.as_str(),
                crate::actions::open_artist_address(&artist_for_resolution.id),
            ),
            (
                playlist.name.as_str(),
                crate::actions::open_playlist_address(&playlist.id),
            ),
        ] {
            let hit = provider
                .search(query)
                .into_iter()
                .find(|hit| hit.action_address().as_ref() == Some(&expected))
                .expect("each searchable Music entity has its canonical hit");
            let address = hit.action_address().expect("canonical search address");
            assert!(
                tilo_actions::door::resolve(&resources, &address).is_some(),
                "{address:?} resolves against the live Music resource tree"
            );
        }
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.set_providers(vec![Box::new(library_source(&app))]);
        palette.open();
        palette.set_query(&track.title);
        let row = palette
            .result_labels()
            .iter()
            .position(|label| label == &track.title)
            .expect("song search row is present");
        palette.set_cursor(row);
        palette.activate_selected();
        let operation = palette.take_operation().expect("play action queued");
        assert!(palette.take_navigation().is_none());
        assert_eq!(operation.kind, tilo_actions::OperationKind::Effect);
        let heads = doc.get_heads();
        let invocation = tilo_actions::InvocationId::new();
        assert!(matches!(
            app.start_effect(
                invocation,
                &operation
                    .encoded()
                    .expect("test requires prepared encoding")
                    .payload
            )
            .expect("local playback operation starts"),
            ui::framework::Effected::Pending
        ));
        assert_eq!(
            app.pb.current.as_deref(),
            Some(track.id.as_str()),
            "the brokered effect plays the selected track"
        );
        assert!(app.pb.queue.contains(&track.id));
        assert_eq!(doc.get_heads(), heads, "playback is device-local");

        // Navigation follows the same path but is explicitly a View operation.
        let heads = doc.get_heads();
        let artist = app.lib.artists.values().next().unwrap().clone();
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.set_providers(vec![Box::new(library_source(&app))]);
        palette.open();
        palette.set_query(&artist.name);
        let row = palette
            .result_labels()
            .iter()
            .position(|label| label == &artist.name)
            .expect("artist search row is present");
        palette.set_cursor(row);
        palette.activate_selected();
        let operation = palette.take_operation().expect("artist view queued");
        assert_eq!(operation.kind, tilo_actions::OperationKind::View);
        assert!(matches!(
            app.start_effect(
                ui::tilo_actions::InvocationId::new(),
                &operation
                    .encoded()
                    .expect("test requires prepared encoding")
                    .payload
            )
            .unwrap(),
            ui::framework::Effected::Done(_)
        ));
        assert_eq!(
            app.view,
            View::Artist(artist.id),
            "brokered view opens the artist page"
        );
        assert_eq!(doc.get_heads(), heads, "navigation is presentation-only");
    }

    // Phase `a_changed_library_is_searchable_once_it_is_reloaded`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.perform_action(
            &mut ui::widget::Edits::lend(&mut doc),
            &crate::actions::create_playlist_action("pl_provider_refresh", "Refresh proof"),
        );
        app.reload_library(&doc);
        assert!(
            library_source(&app)
                .search("Refresh proof")
                .iter()
                .any(|result| {
                    result.action_address()
                        == Some(crate::actions::open_playlist_address("pl_provider_refresh"))
                })
        );
    }

    // Phase `membership_open_operation_reveals_the_current_row_and_refuses_stale_rows`.
    {
        let (mut app, mut doc) = app_with_doc();
        let playlist = app.lib.playlists.first().expect("seed playlist").clone();
        let track = playlist.track_ids.first().expect("seed membership").clone();
        let resource_id = crate::actions::playlist_track_resource_id(&playlist.id, &track);
        let resource =
            MusicApp::publish_resource(&app, &resource_id).expect("membership resolves lazily");
        assert!(
            resource
                .actions
                .iter()
                .any(|action| { action.id == "open" && action.label == "Open in playlist" })
        );

        let payload = crate::actions::effect_payload(&crate::actions::MusicEffect::Open {
            place: crate::actions::Place::Membership {
                playlist: playlist.id.clone(),
                track: track.clone(),
            },
        });
        assert!(matches!(
            app.start_effect(ui::tilo_actions::InvocationId::new(), &payload)
                .expect("current membership opens"),
            ui::framework::Effected::Done(_)
        ));
        assert_eq!(app.view, View::Playlist(playlist.id.clone()));
        assert_eq!(
            app.revealed_membership,
            Some((playlist.id.clone(), track.clone()))
        );

        app.perform_action(
            &mut ui::widget::Edits::lend(&mut doc),
            &crate::actions::remove_track_action(&playlist.id, &track),
        );
        app.reload_library(&doc);
        let error = app
            .start_effect(ui::tilo_actions::InvocationId::new(), &payload)
            .expect_err("stale membership must refuse to open");
        assert!(error.contains("membership is no longer present"));
    }

    // Phase `canonical_top_level_view_operation_routes_without_mutating_the_document`.
    {
        let (mut app, mut doc) = app_with_doc();
        let resource_id = crate::actions::view_resource_id("downloads");
        let resource = MusicApp::publish_resource(&app, &resource_id)
            .expect("published destination resolves at the authority door");
        let open = resource
            .actions
            .iter()
            .find(|action| action.id == "open")
            .expect("view resource exposes open");
        let operation = open
            .build_json(&serde_json::Map::new())
            .expect("open destination takes no arguments");
        let heads = doc.get_heads();
        assert!(matches!(
            app.start_effect(ui::tilo_actions::InvocationId::new(), &operation.payload)
                .expect("known canonical destination routes"),
            ui::framework::Effected::Done(_)
        ));
        assert_eq!(app.view, View::Downloads);
        assert_eq!(doc.get_heads(), heads, "view navigation remains local");

        let forged = crate::actions::effect_payload(&crate::actions::MusicEffect::Open {
            place: crate::actions::Place::View("../../unsafe-local-path".into()),
        });
        assert!(
            app.start_effect(ui::tilo_actions::InvocationId::new(), &forged)
                .is_err()
        );
    }

    // Phase `every_published_top_level_destination_has_a_local_router_target`.
    {
        for &(id, _) in crate::actions::NAVIGATION_VIEWS {
            assert!(
                MusicApp::top_level_view(id).is_some(),
                "published destination {id:?} must route locally"
            );
        }
        assert!(MusicApp::top_level_view("not-a-view").is_none());
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn stream_operations_settle_exactly_once_for_their_request() {
    // Phase `changed_track_playback_effect_starts_exactly_one_stream_request`.
    {
        let (mut app, _doc) = app_with_doc();
        app.disconnect();
        let payload = crate::actions::effect_payload(&crate::actions::MusicEffect::Play {
            track: "tk_g1".into(),
            queued: None,
        });
        let before = app.next_request_id;
        let invocation = tilo_actions::InvocationId::new();
        assert!(matches!(
            app.start_effect(invocation, &payload).unwrap(),
            ui::framework::Effected::Pending
        ));
        assert_eq!(
            app.next_request_id,
            before + 1,
            "a changed track queues one resolve request, never a duplicate stream"
        );
        assert_eq!(app.resolve_in_flight, 1);
    }

    // Phase `stream_operation_reports_exact_success_failure_and_ignores_stale_completion`.
    {
        let (mut app, _doc) = app_with_doc();
        app.disconnect();
        let payload = crate::actions::effect_payload(&crate::actions::MusicEffect::Play {
            track: "tk_g1".into(),
            queued: None,
        });
        let invocation = tilo_actions::InvocationId::new();
        assert!(matches!(
            app.start_effect(invocation, &payload).unwrap(),
            ui::framework::Effected::Pending
        ));
        let request_id = app.active_resolve_request.unwrap();
        let competing = crate::actions::effect_payload(&crate::actions::MusicEffect::Play {
            track: "tk_g2".into(),
            queued: None,
        });
        assert!(
            app.start_effect(tilo_actions::InvocationId::new(), &competing)
                .is_err(),
            "concurrent stream start is refused deterministically"
        );
        app.apply_resolve_failure("tk_g1", request_id + 1, "stale failure");
        assert!(
            app.take_effect_result(invocation).is_none(),
            "a stale worker result cannot settle this invocation"
        );
        app.apply_resolve_failure("tk_g1", request_id, "backend unavailable");
        assert!(matches!(
            app.take_effect_result(invocation),
            Some(tilo_actions::InvocationUpdate::Failed { .. })
        ));
    }

    // Phase `stream_operation_times_out_once_and_late_resolution_is_ignored`.
    {
        let (mut app, _doc) = app_with_doc();
        app.disconnect();
        let payload = crate::actions::effect_payload(&crate::actions::MusicEffect::Play {
            track: "tk_g1".into(),
            queued: None,
        });
        let invocation = tilo_actions::InvocationId::new();
        app.start_effect(invocation, &payload).unwrap();
        let (_, request_id, _, deadline) =
            app.resource_stream_invocation.expect("pending host stream");

        app.expire_resource_stream_if_due(deadline + Duration::from_millis(1));
        assert_eq!(
            app.active_resolve_request, None,
            "timeout retires request authority"
        );
        let Some(tilo_actions::InvocationUpdate::Failed { error, .. }) =
            app.take_effect_result(invocation)
        else {
            panic!("timeout must settle the accepted operation");
        };
        assert_eq!(error.code, "music.stream_start_timed_out");
        assert!(
            app.take_effect_result(invocation).is_none(),
            "a terminal operation update is consumed exactly once"
        );

        app.apply_resolved_stream(
            "tk_g1",
            request_id,
            &crate::backend::ResolvedStream {
                video_id: "yt_tk_g1".into(),
                url: "https://example.invalid/late-audio".into(),
                mime: "audio/mp4".into(),
                codec: "mp4a".into(),
                format: "m4a".into(),
                bitrate: 128_000,
                itag: 140,
                size_bytes: 1,
                duration_ms: 1,
                fallback_url: None,
            },
        );
        assert!(
            app.take_effect_result(invocation).is_none(),
            "a late resolver result cannot overwrite a timeout terminal result"
        );
    }

    // Phase `local_next_supersedes_pending_host_stream_and_late_result_stays_stale`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.disconnect();
        let payload = crate::actions::effect_payload(&crate::actions::MusicEffect::Play {
            track: "tk_g1".into(),
            queued: None,
        });
        let invocation = tilo_actions::InvocationId::new();
        app.start_effect(invocation, &payload).unwrap();
        assert!(matches!(
            app.start_effect(
                tilo_actions::InvocationId::new(),
                &crate::actions::effect_payload(&crate::actions::MusicEffect::AddToQueue {
                    track: "tk_g2".into(),
                }),
            ),
            Ok(ui::framework::Effected::Done(_))
        ));
        let (_, first_request, _, _) = app.resource_stream_invocation.expect("pending host stream");

        // This is the normal in-window/media-key next-track route, not a
        // second host operation. User input wins over a slow external caller.
        app.on_next(&mut ui::widget::Edits::lend(&mut doc));
        let second_request = app.active_resolve_request.expect("new local intent");
        assert_ne!(first_request, second_request);
        assert_eq!(app.pb.current.as_deref(), Some("tk_g2"));

        app.apply_resolved_stream(
            "tk_g1",
            first_request,
            &crate::backend::ResolvedStream {
                video_id: "yt_tk_g1".into(),
                url: "https://example.invalid/late-audio".into(),
                mime: "audio/mp4".into(),
                codec: "mp4a".into(),
                format: "m4a".into(),
                bitrate: 128_000,
                itag: 140,
                size_bytes: 1,
                duration_ms: 1,
                fallback_url: None,
            },
        );
        assert_eq!(app.active_resolve_request, Some(second_request));
        let Some(tilo_actions::InvocationUpdate::Failed { error, .. }) =
            app.take_effect_result(invocation)
        else {
            panic!("local supersession must settle the old host operation");
        };
        assert_eq!(error.code, "music.stream_start_superseded");
        assert!(
            app.take_effect_result(invocation).is_none(),
            "supersession terminal update is delivered exactly once"
        );
    }

    // Phase `stream_operation_reports_success_only_for_its_bound_request`.
    {
        let (mut app, _doc) = app_with_doc();
        app.disconnect();
        let payload = crate::actions::effect_payload(&crate::actions::MusicEffect::Play {
            track: "tk_g1".into(),
            queued: None,
        });
        let invocation = tilo_actions::InvocationId::new();
        let _ = app.start_effect(invocation, &payload).unwrap();
        let request_id = app.active_resolve_request.unwrap();
        let stream = crate::backend::ResolvedStream {
            video_id: "yt_tk_g1".into(),
            url: "https://example.invalid/audio".into(),
            mime: "audio/mp4".into(),
            codec: "mp4a".into(),
            format: "m4a".into(),
            bitrate: 128_000,
            itag: 140,
            size_bytes: 1,
            duration_ms: 1,
            fallback_url: None,
        };
        app.apply_resolved_stream("tk_g1", request_id, &stream);
        #[cfg(not(feature = "audio"))]
        let update = app
            .take_effect_result(invocation)
            .expect("a silent build settles as resolved, never as playing");
        #[cfg(feature = "audio")]
        {
            assert!(
                app.take_effect_result(invocation).is_none(),
                "resolver success alone is not audio-start success"
            );
            // Exercise the same settlement branch `tick` uses after its real
            // `frames_rendered()` observation. The value is deliberately
            // greater than the invocation's captured baseline.
            app.settle_resource_stream_after_rendered_frames(u64::MAX);
        }
        #[cfg(feature = "audio")]
        let update = app
            .take_effect_result(invocation)
            .expect("rendered PCM settles the audio invocation as playing");

        let tilo_actions::InvocationUpdate::Succeeded { output, .. } = update else {
            panic!("the bound resolver completion must succeed");
        };
        #[cfg(not(feature = "audio"))]
        assert_eq!(
            output.value,
            Some(serde_json::json!({ "state": "resolved" })),
            "a no-audio build never labels URL resolution as actual playback"
        );
        #[cfg(feature = "audio")]
        assert_eq!(
            output.value,
            Some(serde_json::json!({ "state": "playing" })),
            "only post-render settlement may report actual playback"
        );
        let play = crate::actions::catalog_track_resource(&crate::actions::CatalogTrackResource {
            id: "tk_g1".into(),
            label: "Track one".into(),
            album_id: String::new(),
            artist_id: String::new(),
            artist: String::new(),
            offline: false,
            liked: false,
        })
        .actions
        .into_iter()
        .find(|action| action.id == "play")
        .expect("play resource action");
        assert_eq!(
            play.contract.execution,
            tilo_actions::ExecutionMode::Background { cancellable: false },
            "playing a track answers when its audio starts"
        );
        play.contract
            .result
            .validate(&output)
            .expect("the actual stream completion must match its published result");
    }

    // Phase `play_drives_the_audio_pipeline_path`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Play {
                queue: vec!["tk_g1".into()].into(),
                index: 0,
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        // The transport started the stream: the engine recorded a load (the
        // stub records the resolved/intended URI; the live backend set the
        // playbin uri). Offline the source resolver returns an error so no URI
        // is loaded — but with the `streaming`/`audio` features the path runs.
        // Either way `start_stream` was invoked and play state is set.
        assert!(app.pb.playing);
        // Volume changes hit the engine.
        app.dispatch(
            Action::VolumeFrac(0.5),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(
            (app.audio.volume() - 0.5).abs() < 1e-6,
            "volume reaches the audio engine"
        );
    }
}

// ---- 7. real audio path -----------------------------------------------

// ---- 8. real live streaming (network; `--features live` only) ---------

/// End-to-end through the *real app widget*: submit a YouTube-Music search,
/// let the worker return real hits, then play the top hit and confirm the
/// app resolved + loaded a real `googlevideo.com` audio URL into the engine.
/// Network-dependent, so gated behind the `live` feature and exercised by
/// the nightly live-integration step rather than every deterministic shard.
#[cfg(feature = "live")]
#[test]
#[ignore = "queries third-party music services; nightly runs it explicitly"]
fn live_search_and_stream_resolve_through_the_app() {
    let (mut app, mut doc) = app_with_doc();
    // The render proof below is about the system sink, not a capture.
    #[cfg(feature = "audio")]
    {
        app.audio = crate::audio::AudioEngine::with_output(crate::audio::AudioOutput::System);
    }
    app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);

    let first = live_search_top_result(&mut app);
    let _url = live_play_and_resolve_stream(&mut app, &mut doc, &first);

    // …and, when this build can actually reach a sound sink, prove the real
    // stream *renders* — decoded PCM buffers reach a real audio sink and the
    // transport advances off the stream clock. This is the end-to-end "a real
    // YouTube track plays sound" proof: the engine stages the resolved
    // (rangeable) source to a temp file with bounded reqwest ranges and plays
    // that.
    #[cfg(feature = "audio")]
    live_audio_render_proof(&mut app);
}

/// Submit a real YouTube-Music search, pump the worker until real hits
/// land, and confirm the top hit is a real, injected, streamable library
/// track. Returns its id. Part of
/// [`live_search_and_stream_resolve_through_the_app`].
#[cfg(feature = "live")]
fn live_search_top_result(app: &mut MusicApp) -> String {
    use std::time::Duration;

    // Submit a real search (spawns the worker + sends the request).
    "aurora runaway".clone_into(&mut app.search_query);
    app.request_live_search();
    assert!(app.worker.is_some(), "search spawned the streaming worker");

    // Pump until real results land (bounded ~8s).
    let mut ids = Vec::new();
    for _ in 0..800 {
        app.drain_worker();
        if let Some((q, r)) = &app.live_results
            && q == "aurora runaway"
            && !r.is_empty()
        {
            ids = r.clone();
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !ids.is_empty(),
        "live search returned real tracks into the app"
    );

    // The top result is a real, injected, streamable library track.
    let first = ids[0].clone();
    app.inject_live_hits();
    assert!(
        super::is_live_track(&first),
        "live track keyed `yt:<videoId>`"
    );
    assert!(
        app.lib.track(&first).is_some(),
        "live track merged into the library"
    );
    first
}

/// Play `first` and pump the worker until the app resolves + loads a
/// real rangeable audio URL into the engine. Returns the loaded URL.
/// Part of [`live_search_and_stream_resolve_through_the_app`].
#[cfg(feature = "live")]
fn live_play_and_resolve_stream(app: &mut MusicApp, doc: &mut AutoCommit, first: &str) -> String {
    use std::time::Duration;

    // Play it → the app requests a stream resolution on the worker.
    app.dispatch(
        Action::Play {
            queue: vec![first.to_string()].into(),
            index: 0,
        },
        &mut ui::widget::Edits::lend(doc),
    );
    assert_eq!(app.pb.current.as_deref(), Some(first));
    assert!(
        app.resolve_in_flight >= 1,
        "play requested a stream resolve"
    );

    // Pump until the resolved URL is loaded into the audio engine.
    let mut loaded_url = None;
    // The resolved audio URL is a fully-rangeable source: an Invidious
    // `local` proxy (`…/latest_version?…&itag=…&local=true`) that serves the
    // whole track over arbitrary byte ranges, or the direct googlevideo URL
    // as a fallback. Either carries an `itag`.
    let is_streamable = |u: &str| {
        u.contains("itag=") && (u.contains("local=true") || u.contains("googlevideo.com"))
    };
    for _ in 0..2000 {
        app.drain_worker();
        if let Some(uri) = app.audio.current_uri()
            && is_streamable(uri)
        {
            loaded_url = Some(uri.to_string());
            break;
        }
        if app.resolve_in_flight == 0 && app.audio.current_uri().is_none() {
            break; // resolve finished without a URL — fail below
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let uri_metadata = app
        .audio
        .current_uri()
        .and_then(|uri| url::Url::parse(uri).ok())
        .map(|uri| {
            (
                uri.scheme().to_owned(),
                uri.host_str().map(str::to_owned),
                uri.query_pairs()
                    .map(|(key, _)| key.into_owned())
                    .collect::<Vec<_>>(),
            )
        });
    assert!(
        loaded_url.is_some(),
        "app resolved + loaded a rangeable audio URL; current={:?}, source={:?}, uri_metadata={uri_metadata:?}, request={:?}, error={:?}",
        app.pb.current,
        app.lib.track(first).map(|t| &t.yt_id),
        app.active_resolve_request,
        app.playback_error
    );
    let url = loaded_url.unwrap();
    assert!(
        is_streamable(&url),
        "resolved a rangeable, itag-bearing audio URL"
    );
    url
}

/// When this build can reach a real sound sink, prove the resolved
/// stream actually renders: decoded PCM buffers reach a real audio sink
/// and the transport advances off the stream clock. Part of
/// [`live_search_and_stream_resolve_through_the_app`].
#[cfg(all(feature = "live", feature = "audio"))]
fn live_audio_render_proof(app: &mut MusicApp) {
    use std::time::Duration;

    if audio_server_present() && decode_plugins_present() {
        // Drive the real frame loop until decoded PCM reaches the sink *and*
        // the transport clock has advanced off it (both prove genuine, not
        // faked, playback of the real stream).
        let mut frames = 0u64;
        for _ in 0..2000 {
            app.drain_worker();
            app.tick();
            frames = app.audio.frames_rendered();
            if frames > 0 && app.playhead_ms > 0.0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            frames > 0,
            "real resolved stream rendered PCM buffers to the sink"
        );
        assert_eq!(
            app.audio.audio_sink_is_real(),
            Some(true),
            "the rendered stream reached a real audio sink, not a fakesink"
        );
        assert!(
            app.playhead_ms > 0.0,
            "the playhead advanced with real playback"
        );
        eprintln!(
            "[live audio] PLAYING: sink={:?} frames={frames} playhead={:.0}ms",
            app.audio.actual_audio_sink_name(),
            app.playhead_ms
        );

        // …and it keeps *advancing*: sample the transport clock over the next
        // few seconds and require it to have moved meaningfully forward. This
        // is the "position advances over time" proof — a running playhead
        // driven by the real pipeline PTS, not a single one-shot nudge or a
        // frozen 0:00. The pipeline position and the frame counter must both
        // grow. Each sample is printed so the advance is visible in the log.
        let p0 = app.playhead_ms;
        let f0 = app.audio.frames_rendered();
        let mut advanced = false;
        let mut last_print = ui::time::now();
        for _ in 0..400 {
            app.tick();
            if last_print.elapsed() >= Duration::from_millis(500) {
                eprintln!(
                    "[live audio] tick: playhead={:.0}ms frames={}",
                    app.playhead_ms,
                    app.audio.frames_rendered()
                );
                last_print = ui::time::now();
            }
            if app.playhead_ms > p0 + 250.0 && app.audio.frames_rendered() > f0 {
                advanced = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        eprintln!(
            "[live audio] ADVANCED: {:.0}ms -> {:.0}ms (frames {} -> {})",
            p0,
            app.playhead_ms,
            f0,
            app.audio.frames_rendered()
        );
        assert!(
            advanced,
            "the real playhead kept advancing (from {p0:.0}ms) while frames kept flowing"
        );
    } else {
        eprintln!("[live audio test] no audio server / decode plugins — skipped the render-proof");
    }
}

/// Whether a running PipeWire/Pulse server is reachable (mirrors the audio
/// module's check) so the render-proof above only runs where a sink exists.
#[cfg(all(feature = "live", feature = "audio"))]
fn audio_server_present() -> bool {
    let run = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from);
    run.is_some_and(|d| d.join("pipewire-0").exists() || d.join("pulse/native").exists())
        || std::env::var_os("PULSE_SERVER").is_some()
}

/// Whether the `GStreamer` decode chain for a staged `YouTube` audio stream (an
/// m4a/AAC file: `filesrc ! qtdemux ! avdec_aac/faad ! audioconvert !
/// audioresample ! autoaudiosink`) is installed. The render-proof skips
/// honestly when it isn't — a stock build without `gstreamer1-libav` can't
/// decode AAC, so there'd be no frames through no fault of the code.
#[cfg(all(feature = "live", feature = "audio"))]
fn decode_plugins_present() -> bool {
    let _ = gstreamer::init();
    let base = [
        "filesrc",
        "qtdemux",
        "audioconvert",
        "audioresample",
        "autoaudiosink",
    ];
    let have_base = base
        .iter()
        .all(|n| gstreamer::ElementFactory::find(n).is_some());
    let have_aac = gstreamer::ElementFactory::find("avdec_aac").is_some()
        || gstreamer::ElementFactory::find("faad").is_some();
    have_base && have_aac
}

/// **Full-track proof** (`--features live,audio`; requires network access):
/// resolve a real `YouTube` track, stage it, and prove what the staged bytes
/// actually support.
///
/// The Rust-native resolver asks `InnerTube` for an `ANDROID_VR` URL because that client is
/// meant to serve arbitrary byte ranges over the whole file, where
/// rustypipe's iOS/TV URL is SABR-gated to its first ~1 minute. Whether the
/// source honours that on the day is the source's decision, and the two
/// outcomes are different claims — so this reads the staging byte counts and
/// proves the one that happened:
///
/// * **the source served the whole file** — seek to ~88 s and require the
///   transport to advance past **90 s** with decoded PCM still reaching the
///   sink, which a stream truncated at ~1 minute could never do;
/// * **the source refused a range partway** — require the app to be honest
///   about it rather than pretend: the partial track still renders real PCM
///   to a real sink, `staging_complete()` reports the shortfall, and when the
///   staged bytes run out the transport ends the track instead of freezing
///   while still claiming to play.
///
/// Neither branch is a skip and both assert the production path. What is
/// ruled out is the shape this replaces, which read `staging_complete()` and
/// a `duration_ms()` over two minutes as proof of a whole track: the download
/// thread set "done" when it *stopped*, refused range included, and the
/// duration comes from the MP4 header, so a ~1-minute stub satisfied both and
/// the proof passed on exactly the stream it existed to rule out. It then
/// failed 15 s later at the seek, blaming the seek. Skips honestly only when
/// an audio server or decode plugins are absent (never a fakesink, never a
/// weakened assertion).
#[cfg(all(feature = "live", feature = "audio"))]
#[test]
// Both branches above still need the track to arrive: this streams a real
// one end to end through InnerTube and a live audio server, so it depends on a
// third party being reachable and on that video still existing. It
// self-skips when the tools are missing, which made it pass on CI and fail
// on a developer machine that has them but cannot reach the network — the
// environment decided the result, not the code.
//
// Opt in with `cargo nextest run -p music --run-ignored all -E
// 'test(full_track_streams_whole_file_or_reports_the_source_truncation)'`
// when changing the stream resolver; that is when it has something to say.
#[ignore = "streams a real track over the network; opt in with --run-ignored"]
#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end proof covers complete playback and explicit source truncation"
)]
fn full_track_streams_whole_file_or_reports_the_source_truncation() {
    use std::time::Duration;
    if !audio_server_present() || !decode_plugins_present() {
        eprintln!("[full-track] skipped — no audio server / decode plugins");
        return;
    }

    let (mut app, mut doc) = app_with_doc();
    // Real sound through the system sink is what this proves.
    app.audio = crate::audio::AudioEngine::with_output(crate::audio::AudioOutput::System);
    app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    "rick astley never gonna give you up".clone_into(&mut app.search_query);
    app.request_live_search();
    let mut ids = Vec::new();
    for _ in 0..1000 {
        app.drain_worker();
        if let Some((_, r)) = &app.live_results
            && !r.is_empty()
        {
            ids = r.clone();
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!ids.is_empty(), "live search returned real tracks");
    let first = ids[0].clone();
    app.inject_live_hits();
    app.dispatch(
        Action::Play {
            queue: vec![first].into(),
            index: 0,
        },
        &mut ui::widget::Edits::lend(&mut doc),
    );

    // Wait until the resolved stream renders PCM and the staging download has
    // stopped fetching — whether it finished the file or the source cut it
    // off. Waiting on "complete" instead would spin the whole budget out
    // against a range-gated source that is never going to finish.
    let mut frames = 0u64;
    for _ in 0..6000 {
        app.drain_worker();
        app.tick();
        frames = app.audio.frames_rendered();
        if frames > 0 && app.audio.staging_finished() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(frames > 0, "the resolved stream rendered PCM to the sink");
    assert!(
        app.audio.staging_finished(),
        "the staging download reached a terminal state rather than hanging"
    );
    // Real sound out of a real sink — never a fakesink quietly swallowing it.
    assert_eq!(
        app.audio.audio_sink_is_real(),
        Some(true),
        "decoded PCM went to a real audio sink ({:?}), not a fakesink",
        app.audio.actual_audio_sink_name()
    );

    // What the resolver *found*: a multi-minute track. This is a property of
    // the resolved media's header, not of the bytes on disk — a truncated
    // fetch of the same URL reports the same duration — so it says the
    // resolver picked a full song, and nothing about how much of it landed.
    let dur = app.audio.duration_ms().unwrap_or(0);
    assert!(
        dur > 120_000,
        "the resolver returned a full song ({dur}ms > 2min), not a short stub"
    );

    let (got, want) = app.audio.staged_progress().unwrap_or((0, None));
    let whole_file = app.audio.staging_complete();
    eprintln!(
        "[full-track] duration={dur}ms frames={frames} staged={got}/{} complete={whole_file}",
        want.map_or_else(|| "?".into(), |w| w.to_string())
    );

    if !whole_file {
        // The source refused a range partway. Prove the app is honest about
        // it instead of pretending the rest is there.
        let want = want.expect("a refused range is only detectable against a declared length");
        assert!(
            got < want,
            "an incomplete staging must report fewer bytes than declared ({got} of {want})"
        );

        // Seek past the staged bytes. The pipeline runs out of media, and the
        // transport must *end the track* — not sit at `playing` with a frozen
        // playhead, which is what the truncation used to look like to a user.
        app.audio.seek_ms(88_000);
        let mut settled = false;
        for _ in 0..1500 {
            app.tick();
            if !app.pb.playing {
                settled = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        eprintln!(
            "[full-track] source served {got} of {want} bytes — transport ended the track \
                 rather than freezing (playing={})",
            app.pb.playing
        );
        assert!(
            settled,
            "running past the staged bytes ended the track; the transport still claimed to \
                 be playing at playhead {:.0}ms with no media behind it",
            app.playhead_ms
        );
        assert!(
            !app.audio.has_stream(),
            "the exhausted pipeline was torn down rather than left loaded"
        );
        return;
    }

    // Seek to ~88 s. Then let the flushing seek land: drive ticks until the
    // *real* pipeline position reflects the seek (~88 s). Reaching 88 s is
    // itself decisive — a stream truncated at ~1 minute has no bytes there and
    // could never position at 88 s.
    app.audio.seek_ms(88_000);
    let mut landed_ms = 0u64;
    for _ in 0..1500 {
        app.tick();
        landed_ms = app.audio.position_ms().unwrap_or(0);
        if landed_ms >= 86_000 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    eprintln!("[full-track] seek to 88s landed at position={landed_ms}ms");
    assert!(
        landed_ms >= 86_000,
        "seek to 88s landed in the late part of the full track (position {landed_ms}ms) — \
             a 1-minute-truncated stream could never reach here"
    );

    // …and playback *continues* past 90 s with decoded PCM still flowing.
    let f0 = app.audio.frames_rendered();
    let mut past = false;
    for _ in 0..3000 {
        app.tick();
        if app.playhead_ms > 90_000.0 && app.audio.frames_rendered() > f0 {
            past = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    eprintln!(
        "[full-track] advanced past cutoff: playhead={:.0}ms frames {}->{}",
        app.playhead_ms,
        f0,
        app.audio.frames_rendered()
    );
    assert!(
        past,
        "playback advanced past 90s (playhead {:.0}ms) while decoding — the full track \
             streams past the 1-minute cutoff",
        app.playhead_ms
    );
}

// ---- 9. lyrics --------------------------------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn lyrics_load_bundled_sync_and_balance_the_wide_layout() {
    // Phase `ensure_lyrics_loads_bundled_for_current`.
    {
        let (mut app, _doc) = app_with_doc();
        app.pb.current = Some("tk_g1".into());
        app.lyrics_for = None;
        app.ensure_lyrics();
        assert!(
            !app.lyrics.is_empty(),
            "bundled lyrics load for the current track"
        );
        assert!(app.lyrics_synced, "tk_g1 has synced lyrics");
    }

    // Phase `lyrics_view_loads_synced_and_highlight_moves_with_playhead`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.disconnect();
        app.playback_loaded = true; // don't let sync clobber the staged track
        app.pb.current = Some("tk_g1".into());
        app.pb.queue = vec!["tk_g1".into()];
        app.view = View::Lyrics;
        app.playhead_ms = 40_000.0;
        draw_at(&mut app, &mut doc, 1000.0, 800.0);
        assert!(
            !app.lyrics.is_empty(),
            "synced lyrics loaded for the current track"
        );
        assert!(app.lyrics_synced, "tk_g1 lyrics are time-synced");
        // The highlighted line (last timestamp <= playhead) advances with time.
        let l = crate::lyrics::Lyrics {
            lines: app.lyrics.clone(),
            synced: true,
        };
        let early = l.current_line(10_000).expect("a line at 10s");
        let late = l.current_line(50_000).expect("a line at 50s");
        assert!(
            late > early,
            "the highlighted current line moves forward in time"
        );
    }

    // Phase `lyrics_are_one_readable_column_on_the_gutter`.
    {
        // Wide: one column on the content gutter, bounded to a readable
        // width, not a centred island beside a repeat of the now-playing art.
        let body = Rect::new(280.0, 57.0, 1280.0, 716.0);
        let column = render::lyrics_column(body, 640.0, 24.0);
        assert!(
            (column.x0 - body.x0 - 24.0).abs() < 0.5,
            "lyrics start on the gutter"
        );
        assert!(
            column.width() <= 640.0 + 0.5,
            "lyrics keep a readable measure"
        );
        assert!(column.y0 >= body.y0 && column.y1 <= body.y1);
        // Narrow: the column takes the width inside the gutters.
        let narrow = Rect::new(0.0, 57.0, 500.0, 500.0);
        let nc = render::lyrics_column(narrow, 640.0, 24.0);
        assert!((nc.x1 - (narrow.x1 - 24.0)).abs() < 0.5);
        // Synced lyrics hold the current line a third of the way down;
        // plain lyrics read from the top.
        assert!(render::lyrics_scroll(None, 36.0, 600.0).abs() < 1e-9);
        let first = render::lyrics_scroll(Some(0), 36.0, 600.0);
        assert!(
            (-first - 200.0).abs() < 0.5,
            "the first line sits a third down"
        );
        let tenth = render::lyrics_scroll(Some(10), 36.0, 600.0);
        assert!((10.0 * 36.0 - tenth - 200.0).abs() < 0.5);
    }
}

// ---- shared draw helper -----------------------------------------------

/// Render one offscreen frame through the production draw path so hit
/// lists + geometry are populated (no GPU; just the Vello scene encode).
fn draw_once(app: &mut MusicApp, doc: &mut AutoCommit) {
    use ui::text_render::TextRenderer;
    use ui::theme::Theme;
    // Lay the root out to the test viewport first.
    app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    app.settle(&mut ui::widget::Edits::lend(doc), &automerge::ROOT);
    app.sync(doc, &automerge::ROOT);
    app.settle(&mut ui::widget::Edits::lend(doc), &automerge::ROOT);
    app.sync(doc, &automerge::ROOT);
    let mut tr = TextRenderer::new();
    let theme = Theme::dark();
    let mut scene = Scene::new();
    app.render_frame(&mut scene, Affine::IDENTITY, &mut tr, &theme);
}

fn draw_once_large_text(app: &mut MusicApp, doc: &mut AutoCommit) {
    use ui::text_render::TextRenderer;
    use ui::theme::Theme;
    app.rect = Rect::new(0.0, 0.0, 390.0, 844.0);
    app.settle(&mut ui::widget::Edits::lend(doc), &automerge::ROOT);
    app.sync(doc, &automerge::ROOT);
    app.settle(&mut ui::widget::Edits::lend(doc), &automerge::ROOT);
    app.sync(doc, &automerge::ROOT);
    let mut tr = TextRenderer::new();
    let theme = Theme::dark().with_text_scale(2.0);
    let mut scene = Scene::new();
    app.render_frame(&mut scene, Affine::IDENTITY, &mut tr, &theme);
}

// ---- 10. shuffle actually reorders playback ----------------------------

/// Build a longer play queue of real seeded tracks (index 0 = current).
fn shuffle_queue() -> Vec<String> {
    [
        "tk_g1", "tk_g2", "tk_g3", "tk_g4", "tk_g5", "tk_p1", "tk_p2", "tk_p3",
    ]
    .iter()
    .map(std::string::ToString::to_string)
    .collect()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn shuffle_permutes_visits_every_track_and_restores_order() {
    // Phase `enabling_shuffle_builds_a_permutation_current_first`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.shuffle_rng = 0xD1CE_5EED_1234_5678; // deterministic order for the assert
        let q = shuffle_queue();
        app.dispatch(
            Action::Play {
                queue: q.clone().into(),
                index: 0,
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.dispatch(
            Action::ToggleShuffle,
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(app.pb.shuffle);
        assert_eq!(app.shuffle_order.len(), q.len());
        let mut sorted = app.shuffle_order.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            (0..q.len()).collect::<Vec<usize>>(),
            "shuffle order is a permutation of the queue"
        );
        assert_eq!(
            app.shuffle_order[0], 0,
            "the currently-playing track stays first"
        );
        assert_ne!(
            app.shuffle_order,
            (0..q.len()).collect::<Vec<usize>>(),
            "the upcoming order is genuinely reshuffled, not the identity"
        );
    }

    // Phase `shuffled_advance_visits_every_track_then_stops`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.shuffle_rng = 0xABCD_1234_9876_5432;
        let q: Vec<String> = ["a", "b", "c", "d", "e", "f"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        app.dispatch(
            Action::Play {
                queue: q.clone().into(),
                index: 0,
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.dispatch(
            Action::ToggleShuffle,
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.pb.repeat = RepeatMode::Off;
        let order = app.shuffle_order.clone();
        let mut visited = vec![app.pb.queue_index];
        for _ in 0..(q.len() - 1) {
            app.dispatch(Action::Next, &mut ui::widget::Edits::lend(&mut doc));
            visited.push(app.pb.queue_index);
        }
        // The production Next path (advance) consults the shuffled order, not the
        // linear queue: proof the dead shuffle button now drives playback.
        assert_eq!(visited, order, "Next follows the shuffled permutation");
        let mut vs = visited.clone();
        vs.sort_unstable();
        assert_eq!(
            vs,
            (0..q.len()).collect::<Vec<usize>>(),
            "every queued track is visited exactly once"
        );
        let last = app.pb.current.clone();
        app.dispatch(Action::Next, &mut ui::widget::Edits::lend(&mut doc));
        assert!(
            !app.pb.playing,
            "advancing past the final shuffled track stops (repeat off)"
        );
        assert_eq!(app.pb.current, last, "playback stays on the final track");
    }

    // Phase `shuffled_repeat_all_keeps_playing_after_the_last_track`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.shuffle_rng = 0x0F0F_1234_5678_9ABC;
        let q: Vec<String> = ["a", "b", "c", "d"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        app.dispatch(
            Action::Play {
                queue: q.clone().into(),
                index: 0,
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.dispatch(
            Action::ToggleShuffle,
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.pb.repeat = RepeatMode::All;
        for _ in 0..q.len() {
            app.dispatch(Action::Next, &mut ui::widget::Edits::lend(&mut doc));
        }
        assert!(
            app.pb.playing,
            "a repeating shuffled queue laps instead of stopping"
        );
    }

    // Phase `disabling_shuffle_returns_to_linear_order`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.shuffle_rng = 0x5;
        let q: Vec<String> = ["a", "b", "c", "d"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        app.dispatch(
            Action::Play {
                queue: q.into(),
                index: 0,
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.dispatch(
            Action::ToggleShuffle,
            &mut ui::widget::Edits::lend(&mut doc),
        ); // on
        assert!(!app.shuffle_order.is_empty());
        app.dispatch(
            Action::ToggleShuffle,
            &mut ui::widget::Edits::lend(&mut doc),
        ); // off
        assert!(!app.pb.shuffle);
        assert!(
            app.shuffle_order.is_empty(),
            "disabling shuffle drops the shuffled order"
        );
        app.dispatch(Action::Next, &mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(
            app.pb.queue_index, 1,
            "linear advance resumes after disabling shuffle"
        );
        assert_eq!(app.pb.current.as_deref(), Some("b"));
    }

    // Phase `s_key_toggles_shuffle_via_event_path`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Play {
                queue: vec!["a".into(), "b".into(), "c".into()].into(),
                index: 0,
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(!app.pb.shuffle);
        assert!(send(&mut app, &char_key("s"), &mut doc), "'s' is consumed");
        assert!(app.pb.shuffle, "'s' enables shuffle");
        assert_eq!(app.shuffle_order.len(), 3, "'s' builds the shuffled order");
        send(&mut app, &char_key("s"), &mut doc);
        assert!(!app.pb.shuffle, "'s' again disables it");
    }
}

// ---- 11. keyboard transport shortcuts (via the real event path) --------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn keyboard_and_pointer_transport_controls_drive_playback() {
    // Phase `space_key_toggles_play_via_event_path`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Play {
                queue: vec!["a".into()].into(),
                index: 0,
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(app.pb.playing);
        send(&mut app, &named_key(NamedKey::Space), &mut doc);
        assert!(!app.pb.playing, "space pauses");
        send(&mut app, &named_key(NamedKey::Space), &mut doc);
        assert!(app.pb.playing, "space resumes");
    }

    // Phase `n_and_p_keys_change_track`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Play {
                queue: vec!["a".into(), "b".into(), "c".into()].into(),
                index: 0,
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        send(&mut app, &char_key("n"), &mut doc);
        assert_eq!(app.pb.queue_index, 1, "'n' plays the next track");
        send(&mut app, &char_key("p"), &mut doc);
        assert_eq!(app.pb.queue_index, 0, "'p' plays the previous track");
    }

    // Phase `arrow_keys_seek_and_shift_skips`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Play {
                queue: vec!["tk_g1".into(), "tk_g2".into()].into(),
                index: 0,
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.playhead_ms = 10_000.0; // tk_g1 is 215s long
        send(&mut app, &named_key(NamedKey::ArrowRight), &mut doc);
        assert!(
            (app.playhead_ms - 15_000.0).abs() < 1.0,
            "right arrow seeks +5s"
        );
        send(&mut app, &named_key(NamedKey::ArrowLeft), &mut doc);
        assert!(
            (app.playhead_ms - 10_000.0).abs() < 1.0,
            "left arrow seeks -5s"
        );
        send_with(
            &mut app,
            &named_key(NamedKey::ArrowRight),
            shift_mods(),
            &mut doc,
        );
        assert_eq!(app.pb.queue_index, 1, "shift+right skips to the next track");
        send_with(
            &mut app,
            &named_key(NamedKey::ArrowLeft),
            shift_mods(),
            &mut doc,
        );
        assert_eq!(
            app.pb.queue_index, 0,
            "shift+left skips to the previous track"
        );
    }

    // Phase `dragging_the_scrubber_seeks`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.playback_loaded = true;
        app.pb.queue = vec!["tk_g1".into()];
        app.pb.current = Some("tk_g1".into());
        app.pb.queue_index = 0;
        draw_once(&mut app, &mut doc); // registers the scrubber rect at real geometry
        let sr = app.scrubber_rect;
        assert!(sr.width() > 10.0, "the full transport drew a scrubber bar");
        send(
            &mut app,
            &cursor_at(Point::new(sr.x0 + 4.0, sr.center().y)),
            &mut doc,
        );
        send(&mut app, &press(MouseButton::Left), &mut doc);
        assert_eq!(
            app.drag,
            Some(Drag::Scrubber),
            "pressing the bar begins a scrub drag"
        );
        let near_start = app.playhead_ms;
        send(
            &mut app,
            &cursor_at(Point::new(sr.x1 - 4.0, sr.center().y)),
            &mut doc,
        );
        let near_end = app.playhead_ms;
        send(&mut app, &released(MouseButton::Left), &mut doc);
        assert!(app.drag.is_none(), "releasing ends the drag");
        assert!(
            near_end > near_start + 100_000.0,
            "dragging right seeks forward ({near_start} -> {near_end})"
        );
    }

    // Phase `dragging_the_volume_bar_sets_volume`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.playback_loaded = true;
        draw_once(&mut app, &mut doc);
        let vr = app.volume_rect;
        assert!(vr.width() > 10.0, "the full transport drew a volume bar");
        send(
            &mut app,
            &cursor_at(Point::new(vr.x0 + 2.0, vr.center().y)),
            &mut doc,
        );
        send(&mut app, &press(MouseButton::Left), &mut doc);
        assert_eq!(app.drag, Some(Drag::Volume));
        let low = app.pb.volume;
        send(
            &mut app,
            &cursor_at(Point::new(vr.x1 - 2.0, vr.center().y)),
            &mut doc,
        );
        let high = app.pb.volume;
        send(&mut app, &released(MouseButton::Left), &mut doc);
        assert!(
            high > low,
            "dragging the volume bar right raises volume ({low} -> {high})"
        );
        assert!(high >= 95, "the far right is near max volume");
        assert!(
            (app.audio.volume() - f32::from(app.pb.volume) / 100.0).abs() < 1e-6,
            "the audio engine tracks the dragged volume"
        );
    }

    // Phase `wheel_scrolls_and_clamps_to_content`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.view = View::Songs;
        // The second frame lays the page out beside the scroll bar's lane.
        draw_once(&mut app, &mut doc);
        draw_once(&mut app, &mut doc);
        let max = (app.content_height - app.body_rect.height()).max(0.0);
        assert!(
            max > 0.0,
            "the songs list overflows, so there is room to scroll"
        );
        // The wheel scrolls what is under the pointer.
        let over_body = app.body_rect.center();
        send(&mut app, &cursor_at(over_body), &mut doc);
        send(&mut app, &wheel(-3.0), &mut doc);
        assert!(app.scroll_y() > 0.0, "wheel down scrolls the content");
        assert!(app.scroll_y() <= max);
        send(&mut app, &wheel(-1000.0), &mut doc);
        assert_eq!(
            app.scroll_y(),
            max,
            "a big scroll clamps at the content bottom"
        );
        send(&mut app, &wheel(1000.0), &mut doc);
        assert_eq!(app.scroll_y(), 0.0, "scrolling back up clamps at the top");
    }

    // Phase `toggle_mute_silences_and_restores_volume`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.pb.volume = 76;
        app.audio.set_volume(0.76);
        app.dispatch(Action::ToggleMute, &mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(app.pb.volume, 0, "mute drops the volume to zero");
        assert!(
            app.audio.volume().abs() < 1e-6,
            "the audio engine is silenced"
        );
        app.dispatch(Action::ToggleMute, &mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(app.pb.volume, 76, "unmute restores the pre-mute level");
        assert!((app.audio.volume() - 0.76).abs() < 1e-6);
    }

    // Phase `speaker_icon_click_toggles_mute`.
    {
        let (mut app, mut doc) = app_with_doc();
        draw_once(&mut app, &mut doc);
        assert!(app.pb.volume > 0);
        let c =
            hit_center(&app, &Action::ToggleMute).expect("the speaker icon is a wired mute toggle");
        send(&mut app, &cursor_at(c), &mut doc);
        send(&mut app, &press(MouseButton::Left), &mut doc);
        assert_eq!(app.pb.volume, 0, "clicking the speaker mutes");
    }

    // Phase `swiping_the_bar_left_plays_next`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Play {
                queue: vec!["tk_g1".into(), "tk_g2".into(), "tk_g3".into()].into(),
                index: 0,
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.disconnect();
        draw_at(&mut app, &mut doc, 520.0, 800.0); // compact bar
        let bar = app.nowplaying_rect();
        // Start on the bar's empty title area (left of the transport buttons).
        let start = Point::new(bar.x0 + bar.width() * 0.35, bar.center().y);
        let endp = Point::new(start.x - 130.0, start.y);
        send(&mut app, &cursor_at(start), &mut doc);
        send(&mut app, &press(MouseButton::Left), &mut doc);
        send(&mut app, &cursor_at(endp), &mut doc);
        send(&mut app, &released(MouseButton::Left), &mut doc);
        assert_eq!(
            app.pb.queue_index, 1,
            "a leftward swipe skips to the next track"
        );
        assert_eq!(app.pb.current.as_deref(), Some("tk_g2"));
    }
}

// ---- 12. drag-to-seek / drag-volume gestures ---------------------------

// ---- 13. mouse-wheel scroll + clamp ------------------------------------

// ---- 14. mute toggle (speaker icon) ------------------------------------

// ---- 15. search typing + name-prompt keyboard flow ---------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn queue_view_and_volume_controls_are_responsive() {
    // Phase `queue_toggle_opens_then_restores_previous_view`.
    // Toggling the queue view off returns the user to exactly where they were
    // (view + scroll), not a reset.
    {
        let (mut app, mut doc) = app_with_doc();
        app.view = View::Albums;
        app.set_scroll_y(137.0);

        app.dispatch(Action::ToggleQueue, &mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(app.view, View::Queue, "toggling opens the queue view");
        assert_eq!(app.scroll_y(), 0.0, "the queue view starts at the top");

        app.dispatch(Action::ToggleQueue, &mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(
            app.view,
            View::Albums,
            "toggling off returns to the prior view"
        );
        assert_eq!(
            app.scroll_y(),
            137.0,
            "and restores the prior scroll position"
        );
    }

    // Phase `queue_button_hover_opens_preview`.
    // Hovering the now-playing bar's queue button opens the up-next preview,
    // which lists the next few queued songs.
    {
        let (mut app, mut doc) = app_with_doc();
        // Keep the in-memory queue authoritative across the draw's `sync` (which
        // would otherwise re-adopt the seeded playback snapshot).
        app.playback_loaded = true;
        app.pb.queue = vec![
            "tk_g1".into(),
            "tk_a1".into(),
            "tk_h1".into(),
            "tk_p1".into(),
        ];
        app.pb.current = Some("tk_g1".into());
        app.pb.queue_index = 0;
        draw_once(&mut app, &mut doc); // registers the queue button rect
        assert!(
            app.queue_btn_rect.width() > 0.0,
            "the wide transport drew a queue button"
        );

        // The preview lists the upcoming (not-current) tracks.
        assert_eq!(
            app.queue_preview_tracks(),
            vec![
                "tk_a1".to_string(),
                "tk_h1".to_string(),
                "tk_p1".to_string()
            ],
        );

        // Not hovering yet → closed; hovering the button → open.
        app.mouse = Point::new(-10.0, -10.0);
        assert!(!app.queue_preview_active(), "no preview when not hovering");
        app.mouse = app.queue_btn_rect.center();
        assert!(
            app.queue_preview_active(),
            "hovering the queue button opens the preview"
        );
    }

    // Phase `volume_control_is_responsive_by_bar_width`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.disconnect();
        app.playback_loaded = true;
        app.pb.current = Some("tk_g1".into());
        app.pb.queue = vec!["tk_g1".into()];
        // Wide bar (docked sidebar, ~1044px content): inline slider.
        draw_at(&mut app, &mut doc, 1280.0, 800.0);
        assert!(
            app.volume_rect.width() > 0.0,
            "wide bar shows an inline volume slider"
        );
        assert_eq!(
            app.volume_btn_rect,
            Rect::ZERO,
            "wide bar has no collapsed button"
        );
        // Mid bar (~764px content): slider collapses to a button.
        draw_at(&mut app, &mut doc, 1000.0, 800.0);
        assert_eq!(
            app.volume_rect,
            Rect::ZERO,
            "mid bar drops the inline slider"
        );
        assert!(
            app.volume_btn_rect.width() > 0.0,
            "mid bar shows a volume button"
        );
        // Very small (compact bar): volume stays a button with its popover.
        draw_at(&mut app, &mut doc, 520.0, 800.0);
        assert_eq!(app.volume_rect, Rect::ZERO);
        assert!(
            app.volume_btn_rect.width() > 0.0,
            "the compact bar keeps the volume button"
        );
        // The rest of the wide bar's controls live in its More menu.
        let more = app
            .hits
            .iter()
            .find_map(|hit| match hit.action {
                Action::OpenPlayerMenu(rect) => Some(rect),
                _ => None,
            })
            .expect("the compact bar offers a More menu");
        app.dispatch(
            Action::OpenPlayerMenu(more),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(app.context.is_open(), "More opens the player menu");
        let labels: Vec<&str> = app
            .context
            .items()
            .iter()
            .filter_map(ui::widget::context_menu::MenuItem::label)
            .collect();
        for label in ["Shuffle", "Repeat all", "Repeat one", "Lyrics", "Queue"] {
            assert!(
                labels.contains(&label),
                "the More menu offers {label}: {labels:?}"
            );
        }
        assert!(
            labels.contains(&"Like") || labels.contains(&"Unlike"),
            "the More menu offers like: {labels:?}"
        );
    }

    // Phase `volume_button_opens_a_slider_popover`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.disconnect();
        app.playback_loaded = true;
        app.pb.current = Some("tk_g1".into());
        app.pb.queue = vec!["tk_g1".into()];
        draw_at(&mut app, &mut doc, 1000.0, 800.0); // collapsed-to-button width
        assert!(app.volume_btn_rect.width() > 0.0);
        app.dispatch(
            Action::ToggleVolumePopover,
            &mut ui::widget::Edits::lend(&mut doc),
        );
        draw_at(&mut app, &mut doc, 1000.0, 800.0);
        assert!(
            app.volume_popover_open,
            "the volume button opens the popover"
        );
        assert!(
            app.volume_popover_slider.width() > 0.0,
            "the popover hosts a volume slider"
        );
        let (panel, button) = (app.volume_popover_rect, app.volume_btn_rect);
        assert!(
            panel.y1 < button.y0 && (panel.x1 - button.x1).abs() < 0.5,
            "the popover opens above the button, on its trailing edge ({panel:?} over {button:?})"
        );
        assert!(
            panel.x0 >= 0.0 && panel.x1 <= 1000.0,
            "the popover stays in the window: {panel:?}"
        );
        let (track, slider) = (app.volume_rect, app.volume_popover_slider);
        assert!(
            track.width() > 0.0
                && track.x0 >= slider.x0
                && track.x1 <= slider.x1
                && (track.y0 - slider.y0).abs() < 0.5
                && (track.y1 - slider.y1).abs() < 0.5,
            "the popover slider is the active volume drag target ({track:?} in {slider:?})"
        );
    }

    // Phase `compact_bar_renders_progress_below_the_song`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.disconnect();
        app.playback_loaded = true;
        app.pb.current = Some("tk_g1".into());
        app.pb.queue = vec!["tk_g1".into()];
        draw_at(&mut app, &mut doc, 520.0, 800.0); // compact
        assert!(
            app.scrubber_rect.y0 > app.nowplaying_rect().center().y,
            "the progress bar runs along the bottom of the bar, under the song"
        );
        assert!(
            app.scrubber_rect.width() > 300.0,
            "the progress spans the bar width"
        );
    }

    // Phase `compact_scrubber_is_grabbable_not_a_hairline`.
    {
        // At narrow width the scrub bar must be a real, grabbable track — not a
        // 1-2px hairline glued to the window edge.
        let (mut app, mut doc) = app_with_doc();
        app.disconnect();
        app.playback_loaded = true;
        app.pb.current = Some("tk_g1".into());
        app.pb.queue = vec!["tk_g1".into()];
        draw_at(&mut app, &mut doc, 520.0, 800.0); // compact bar
        let sr = app.scrubber_rect;
        assert!(
            sr.height() >= 16.0,
            "the narrow scrub bar has a grabbable hit height (was {})",
            sr.height()
        );
        assert!(sr.width() > 300.0, "the scrub bar spans the bar width");
        // Its top clears the transport row, so a play/pause press never seeks.
        assert!(
            sr.y0 > app.rect.y1 - app.bar_h / 2.0,
            "the scrub strip stays below the transport buttons"
        );
    }

    // Phase `songs_row_columns_share_the_width_without_a_dead_gap`.
    {
        // A wide row: the title column is as wide as its widest line (but a
        // readable column at least), the album follows one column gap
        // later, so no cavern opens between them; the actions and duration
        // keep their own trailing columns.
        let (left, right, gap, min_text) = (304.0, 1256.0, 24.0, 160.0);
        let widths = render::ColumnWidths {
            index: 24.0,
            duration: 32.0,
            action: 28.0,
            actions: 2,
            title: 210.0,
            gap,
            min_text,
        };
        let cols = render::track_columns(left, right, widths, true);
        let (album_x, album_x1) = cols.album.expect("a wide row shows the album");
        assert!(
            (album_x - cols.title_x1 - gap).abs() < 0.5,
            "album follows the title"
        );
        assert!(
            (cols.title_x1 - cols.title_x - 210.0).abs() < 0.5,
            "the title column fits its widest line"
        );
        let short = render::track_columns(
            left,
            right,
            render::ColumnWidths {
                title: 90.0,
                ..widths
            },
            true,
        );
        assert!(
            (short.title_x1 - short.title_x - min_text).abs() < 0.5,
            "short titles keep a readable column"
        );
        let long = render::track_columns(
            left,
            right,
            render::ColumnWidths {
                title: 2000.0,
                ..widths
            },
            true,
        );
        let (long_album, _) = long
            .album
            .expect("long titles still leave the album its half");
        assert!(long_album - long.title_x1 >= gap - 0.5);
        assert!(long.title_x1 - long.title_x <= (long.actions_x - gap - long.title_x) / 2.0 + 0.5);
        assert!(album_x1 - album_x >= min_text);
        assert!(
            (cols.actions_x - album_x1 - gap).abs() < 0.5,
            "the actions follow the album"
        );
        assert!(cols.actions_x < right);

        // Too narrow for two readable columns: the title keeps the width.
        let narrow = render::track_columns(0.0, 420.0, widths, true);
        assert!(narrow.album.is_none());
        assert!((narrow.actions_x - narrow.title_x1 - gap).abs() < 0.5);
    }
}

// ---- 16. "Add to playlist… → New playlist…" keeps the target track -----

// ---- 18. per-view render smoke test ------------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn every_view_renders_and_now_playing_falls_back_to_album_art() {
    // Phase `every_view_renders_without_panic`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.disconnect(); // hermetic: no worker / cover fetches
        app.playback_loaded = true;
        app.pb.queue = vec!["tk_g1".into(), "tk_g2".into()];
        app.pb.current = Some("tk_g1".into());
        app.pb.queue_index = 0;
        let views = [
            View::Home,
            View::Songs,
            View::Albums,
            View::Artists,
            View::Playlists,
            View::Downloads,
            View::Liked,
            View::Browse,
            View::Queue,
            View::Lyrics,
            View::Album("al_glasslight".into()),
            View::Artist("ar_aurora".into()),
            View::Playlist("pl_focus".into()),
        ];
        for v in views {
            app.view = v.clone();
            draw_once(&mut app, &mut doc);
            assert!(app.content_height > 0.0, "{v:?} produced a frame");
            assert!(!app.hits.is_empty(), "{v:?} registered interactive hits");
        }
    }

    // Phase `now_playing_uses_album_cover_when_track_art_missing`.
    {
        let (mut app, _doc) = app_with_doc();
        // An album-track hit often omits per-track art; only the album cover is
        // known — the now-playing bar must use it, not a procedural placeholder.
        app.art_urls
            .insert("al_x".into(), "http://cover/al_x.jpg".into());
        assert_eq!(
            app.best_cover_id("tk_x", "al_x"),
            "al_x",
            "falls back to album art"
        );
        // Track art wins when present.
        app.art_urls
            .insert("tk_x".into(), "http://cover/tk_x.jpg".into());
        assert_eq!(app.best_cover_id("tk_x", "al_x"), "tk_x");
        // With neither, the (procedural) track id is used.
        assert_eq!(app.best_cover_id("tk_none", "al_none"), "tk_none");
    }
}

// ---- 19. real audio / playback state ----------------------------------

/// Render one full frame at a specific viewport size (drives the same
/// passes the live app + `--shots` do), so responsive layout + captured
/// interaction rects can be asserted.
fn draw_at(app: &mut MusicApp, doc: &mut AutoCommit, w: f64, h: f64) {
    use ui::text_render::TextRenderer;
    use ui::theme::Theme;
    app.rect = Rect::new(0.0, 0.0, w, h);
    app.settle(&mut ui::widget::Edits::lend(doc), &automerge::ROOT);
    app.sync(doc, &automerge::ROOT);
    app.settle(&mut ui::widget::Edits::lend(doc), &automerge::ROOT);
    app.sync(doc, &automerge::ROOT);
    let mut tr = TextRenderer::new();
    let theme = Theme::dark();
    let mut scene = Scene::new();
    app.render_frame(&mut scene, Affine::IDENTITY, &mut tr, &theme);
}

// ---- 20. lyrics: synced load + moving highlight ------------------------

// ---- 21. favourites sync via the CRDT document ------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn canonical_actions_converge_through_the_platform_document() {
    // Phase `canonical_action_host_commit_converges_through_the_platform_document`.
    {
        let (mut app, mut doc) = app_with_doc();
        assert!(!doc::load_library(&doc).is_liked("tk_g2"));
        // Like via the normal action pipeline: the framework dispatcher
        // pointer controls and Ctrl+K share, not a Music-owned transport or
        // a side-channel mutation.
        app.dispatch(
            Action::ToggleLike("tk_g2".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(
            doc::load_library(&doc).is_liked("tk_g2"),
            "the canonical action changed the host-owned app document"
        );
        assert_eq!(
            last_action_label(&mut doc).as_deref(),
            Some("Like song"),
            "the like is a recorded, attributed action the platform replicates"
        );
        // Model the platform's document replication boundary with an exact
        // Automerge message exchange. Music neither opens a socket nor mints a
        // pairing token; it only reads/writes this canonical document.
        let mut peer = AutoCommit::new();
        crate::sync::converge(&mut doc, &mut peer);
        assert!(
            doc::load_library(&peer).is_liked("tk_g2"),
            "the committed canonical action reaches the peer document"
        );
    }

    // Phase `like_heart_click_dispatches_the_action_the_palette_builds`.
    // Single source of truth on the real click path: clicking a song row's like
    // heart dispatches the byte-identical absolute `EncodedAction` the Ctrl+K
    // like spec builds for that track — the on-screen affordance and the
    // palette are one action, not two code paths. Proven end-to-end through the
    // production `handle_event` path.
    {
        let (mut app, mut doc) = app_with_doc();
        app.view = View::Songs;
        draw_once(&mut app, &mut doc);
        // The like control of a song's row, shown under the pointer.
        let id = String::from("tk_g2");
        let center = track_control(&mut app, &mut doc, &id, "like", draw_once).center();
        // The sample library seeds some tracks as already-liked, so assert the
        // membership *flips* rather than assuming a fixed direction.
        let before = doc::load_library(&doc).is_liked(&id);

        hosted_click(&mut app, &mut doc, center);

        assert_eq!(
            doc::load_library(&doc).is_liked(&id),
            !before,
            "and it actually toggled the track's liked state in the doc"
        );
        assert_eq!(
            last_action_label(&mut doc).as_deref(),
            Some(if before { "Unlike song" } else { "Like song" }),
            "the click applied the palette's like action"
        );
    }
}

/// The history label of the last action applied to `doc`.
fn last_action_label(doc: &mut AutoCommit) -> Option<String> {
    let change = doc.get_last_local_change()?;
    let message = change.message()?;
    tilo_actions::ActionContext::from_change_message(message).map(|context| context.action_label)
}

// ---- 22. responsive bottom bar: volume + swipe ------------------------

// ---- 23. prefetch on a stable connection ------------------------------

// ---- 24. multi-type search, filter tabs, artist + album pages ---------

use crate::backend::{AlbumHit, AlbumKind, ArtistAlbum};

fn th(video: &str, title: &str) -> TrackHit {
    TrackHit {
        video_id: video.into(),
        title: title.into(),
        artist: "Daft Punk".into(),
        artist_id: Some("UCdaftpunk".into()),
        album: Some("Discovery".into()),
        album_id: Some("MPREdiscovery".into()),
        duration_s: 300,
        cover_url: String::new(),
    }
}

/// A realistic multi-type search response: a top-result artist plus songs,
/// an album, an artist and a playlist — exactly what one live search yields.
fn sample_bundle() -> SearchBundle {
    SearchBundle {
        top: Some(SearchTop::Artist(ArtistHit {
            id: "UCdaftpunk".into(),
            name: "Daft Punk".into(),
            subscriber_count: 5_000_000,
            avatar_url: String::new(),
        })),
        tracks: vec![
            th("vid_one", "One More Time"),
            th("vid_two", "Digital Love"),
        ],
        albums: vec![AlbumHit {
            id: "MPREdiscovery".into(),
            title: "Discovery".into(),
            artist: "Daft Punk".into(),
            artist_id: Some("UCdaftpunk".into()),
            year: 2001,
            kind: AlbumKind::Album,
            cover_url: String::new(),
        }],
        artists: vec![ArtistHit {
            id: "UCdaftpunk".into(),
            name: "Daft Punk".into(),
            subscriber_count: 5_000_000,
            avatar_url: String::new(),
        }],
        playlists: vec![crate::backend::PlaylistHit {
            id: "PLdaftmix".into(),
            title: "This Is Daft Punk".into(),
            subtitle: "YouTube Music".into(),
            track_count: 50,
            cover_url: String::new(),
        }],
    }
}

/// An app that has taken in a multi-type live search reply, offline (no
/// worker, no network) so the render path is exercised hermetically.
fn app_with_live_results() -> (MusicApp, AutoCommit) {
    let (mut app, doc) = app_with_doc();
    app.disconnect();
    app.playback_loaded = true;
    app.ingest_search_results("daft punk", sample_bundle());
    (app, doc)
}

/// Invoke the published action search `hit` names, as the palette does: on
/// the row a search provider resolves, else on the app's published tree.
fn invoke_hit(app: &mut MusicApp, hit: &ui::search::SearchResult, _doc: &mut AutoCommit) {
    use ui::tilo_resource::Effect;
    let address = hit
        .action_address()
        .expect("a hit invokes a published action");
    let row = app
        .device_row(address.resource.as_str())
        .map(ui::framework::LiveRow::into_resource)
        .or_else(|| app.publish_resource(address.resource.as_str()))
        .expect("the actual authority door resolves the hit's row");
    let rows = [row];
    let spec = tilo_actions::door::resolve(&rows, &address)
        .expect("the hit's published action resolves on its actual row");
    let encoded = ui::testing::action_contract::spec_encoding(spec, None);
    let effect = crate::actions::MusicEffect::from_payload(&encoded.payload)
        .expect("the published row names the typed music effect");
    app.begin_effect(tilo_actions::InvocationId::new(), effect)
        .expect("the actual effect runs");
}

/// Whether track `id` is a row of one of the page's track tables.
fn has_play_of(app: &MusicApp, id: &str) -> bool {
    app.track_tables.iter().any(|slot| {
        slot.queue.iter().any(|queued| queued == id)
            && slot.table.rows().iter().any(|row| row.id == id)
    })
}
/// Where the page's embedded widget with test id `id` stands.
fn page_widget(app: &MusicApp, id: &str) -> Rect {
    app.page_parts
        .iter()
        .find_map(|part| part.find(id))
        .unwrap_or_else(|| panic!("the page shows {id}"))
}

#[test]
fn recommendations_shared_input_and_dismiss_controls_record_canonical_actions() {
    let (mut app, mut doc) = app_with_doc();
    app.disconnect();
    app.view = View::Recommendations;
    draw_at(&mut app, &mut doc, 1280.0, 2400.0);
    let toggle = page_widget(&app, "music-recommendations-input-likes");
    hosted_click(&mut app, &mut doc, toggle.center());
    assert!(!doc::load_library(&doc).recommendations.likes);
    assert_eq!(
        last_action_label(&mut doc).as_deref(),
        Some(ui::i18n!("music-recommendations-change-inputs").as_str())
    );
    app.dispatch(
        Action::Recommendation(crate::actions::MusicAction::SetRecommendationInput {
            input: crate::recommendations::Input::Likes,
            enabled: true,
        }),
        &mut ui::widget::Edits::lend(&mut doc),
    );
    draw_at(&mut app, &mut doc, 1280.0, 2400.0);
    let pick = crate::recommendations::suggestions(&app.lib, 1)
        .pop()
        .unwrap();
    let dismiss = page_widget(
        &app,
        &format!("music-recommendations-dismiss-{}", pick.track),
    );
    hosted_click(&mut app, &mut doc, dismiss.center());
    assert!(
        doc::load_library(&doc)
            .recommendations
            .dismissed
            .contains(&pick.track)
    );
    assert_eq!(
        last_action_label(&mut doc).as_deref(),
        Some(ui::i18n!("music-recommendations-dismiss").as_str())
    );
}

fn has_nav(app: &MusicApp, view: View) -> bool {
    let want = Action::Nav(view);
    app.hits.iter().any(|h| h.action == want)
        || app.media_grids.iter().any(|grid| grid.offers(&want))
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn artist_and_album_pages_navigate_and_play() {
    // Phase `artist_page_from_search_result_navigates_and_plays_a_top_track`.
    {
        let (mut app, mut doc) = app_with_live_results();
        // Activating the artist's search hit opens the artist page.
        let rows = crate::live_search::rows_of(&sample_bundle());

        let artist = rows
            .iter()
            .find(|row| {
                row.action_address() == Some(crate::actions::open_artist_address("UCdaftpunk"))
            })
            .expect("the artist's hit");
        invoke_hit(&mut app, artist, &mut doc);
        assert_eq!(app.view, View::Artist("UCdaftpunk".into()));
        // Supply the real artist page (as the worker would) and render it.
        let page = ArtistPage {
            id: "UCdaftpunk".into(),
            name: "Daft Punk".into(),
            subscriber_count: 5_000_000,
            description: "French electronic music duo formed in Paris in 1993.".into(),
            wikipedia_url: "https://en.wikipedia.org/wiki/Daft_Punk".into(),
            header_url: String::new(),
            tracks: vec![th("top_track", "Harder Better Faster Stronger")],
            albums: vec![
                ArtistAlbum {
                    id: "MPREdiscovery".into(),
                    title: "Discovery".into(),
                    year: 2001,
                    kind: AlbumKind::Album,
                    cover_url: String::new(),
                },
                ArtistAlbum {
                    id: "MPREsingle".into(),
                    title: "Da Funk".into(),
                    year: 1995,
                    kind: AlbumKind::Single,
                    cover_url: String::new(),
                },
            ],
            provider_followed: None,
            similar: vec![ArtistHit {
                id: "UCjustice".into(),
                name: "Justice".into(),
                subscriber_count: 1_000_000,
                avatar_url: String::new(),
            }],
        };
        app.absorb_artist(&page);
        app.artist_pages.insert("UCdaftpunk".into(), page);
        // Tall enough that the whole page is on screen: rows scrolled out of
        // view register no clicks.
        draw_at(&mut app, &mut doc, 1280.0, 2400.0);
        // The full artist page surfaces a Follow control, related artists, and the
        // album + single as separate discography entries.
        let follow = page_widget(&app, "music.page.Follow");
        assert!(follow.width() > 0.0, "the artist page has a Follow control");
        assert!(
            has_nav(&app, View::Artist("UCjustice".into())),
            "related artists are navigable"
        );
        assert!(
            has_nav(&app, View::Album("MPREsingle".into())),
            "the single appears in the discography"
        );
        // Click a top track → it plays.
        let top_id = "yt:top_track".to_string();
        click_track(&mut app, &mut doc, &top_id);
        assert_eq!(
            app.pb.current.as_deref(),
            Some("yt:top_track"),
            "clicking a top track plays it"
        );
        assert!(app.pb.queue.contains(&top_id));
    }

    // Phase `album_page_lists_ordered_tracks_and_plays_the_album`.
    {
        let (mut app, mut doc) = app_with_doc();
        app.disconnect();
        app.playback_loaded = true;
        let page = AlbumPage {
            id: "MPREdiscovery".into(),
            title: "Discovery".into(),
            artist_id: "UCdaftpunk".into(),
            artist_name: "Daft Punk".into(),
            year: 2001,
            cover_url: String::new(),
            tracks: vec![
                th("al_one", "One More Time"),
                th("al_two", "Aerodynamic"),
                th("al_three", "Digital Love"),
            ],
        };
        app.absorb_album(&page);
        app.album_pages.insert("MPREdiscovery".into(), page);
        app.view = View::Album("MPREdiscovery".into());
        draw_once(&mut app, &mut doc);
        // The album's full ordered track list renders — every track is playable.
        for vid in ["al_one", "al_two", "al_three"] {
            assert!(
                has_play_of(&app, &format!("yt:{vid}")),
                "album track {vid} is a playable row"
            );
        }
        // The album header's Play plays the whole album in track order.
        let play = page_widget(&app, "music.page.Play");
        send(&mut app, &cursor_at(play.center()), &mut doc);
        send(&mut app, &press(MouseButton::Left), &mut doc);
        send(&mut app, &released(MouseButton::Left), &mut doc);
        assert_eq!(
            app.pb.queue,
            ["yt:al_one", "yt:al_two", "yt:al_three"],
            "the album header plays the whole album in order"
        );
        assert_eq!(app.pb.current.as_deref(), Some("yt:al_one"));
    }
}

#[test]
fn a_launch_link_plays_from_the_start_or_opens_its_page() {
    // A plain `music://track/` launch opens the track paused at the start once
    // it is in the library.
    let (mut app, mut doc) = app_with_doc();
    let arguments = [crate::search_type::SearchHit::track_link("tk_g2")];
    app.open_launch(&ui::framework::LaunchInput::new(&arguments, None));
    app.sync(&doc, &automerge::ROOT);
    app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
    app.sync(&doc, &automerge::ROOT);
    assert_eq!(app.pb.current.as_deref(), Some("tk_g2"));
    assert_eq!(app.pb.position_ms, 0);
    assert!(!app.pb.playing);

    let (mut app, _doc) = app_with_doc();
    let arguments = [crate::search_type::SearchHit::album_link("al_glasslight")];
    app.open_launch(&ui::framework::LaunchInput::new(&arguments, None));
    assert_eq!(app.view, View::Album("al_glasslight".into()));
    assert!(app.pb.current.is_none(), "opening a page plays nothing");
}

#[test]
fn live_album_tracks_are_discoverable_and_queue_through_the_resource_door() {
    let mut app = ui::framework::testing::TestApp::<crate::MusicShell>::new().unwrap();
    let track = "spotify:track:4uLU6hMCjMI75M1A2tKUQC";
    let surface = app.surface_mut::<MusicApp>();
    surface.absorb_album(&crate::backend::AlbumPage {
        id: "spotify:album:2w3BTVsZ1rb9LRsgkoWJbG".into(),
        tracks: vec![crate::backend::TrackHit {
            video_id: track.into(),
            title: "An admitted catalogue track".into(),
            artist: "Catalogue artist".into(),
            artist_id: None,
            album: Some("Catalogue album".into()),
            album_id: Some("spotify:album:2w3BTVsZ1rb9LRsgkoWJbG".into()),
            duration_s: 213,
            cover_url: String::new(),
        }],
        ..Default::default()
    });
    app.settle();
    let address = crate::actions::track_resource_id(track);
    let published = app.agent_surface().to_string();
    assert!(published.contains(&address), "{published}");
    assert!(published.contains("Play track"), "{published}");
    assert!(!app.state().tracks.contains_key(track));
    app.effect_on(
        &address,
        &crate::actions::MusicEffect::AddToQueue {
            track: track.into(),
        },
    )
    .expect("a live album track exposes its bound queue action");
    assert!(
        !app.state().tracks.contains_key(track),
        "queueing is device-local"
    );
    assert!(
        app.agent_surface()
            .to_string()
            .contains(&crate::actions::queue_item_resource_id(0, track))
    );
}

#[test]
fn playback_status_reports_observations_without_mutating_the_library() {
    let mut app = ui::framework::testing::TestApp::<crate::MusicShell>::new().unwrap();
    let before = app.read_document(|doc| doc.clone().get_heads());
    let track = "spotify:track:4uLU6hMCjMI75M1A2tKUQC";
    app.surface_mut::<MusicApp>()
        .absorb_album(&crate::backend::AlbumPage {
            id: "spotify:album:2w3BTVsZ1rb9LRsgkoWJbG".into(),
            tracks: vec![crate::backend::TrackHit {
                video_id: track.into(),
                title: "Observed queue fixture".into(),
                artist: "Fixture artist".into(),
                artist_id: None,
                album: Some("Fixture album".into()),
                album_id: Some("spotify:album:2w3BTVsZ1rb9LRsgkoWJbG".into()),
                duration_s: 213,
                cover_url: String::new(),
            }],
            ..Default::default()
        });
    app.settle();
    app.effect_on(
        &crate::actions::track_resource_id(track),
        &crate::actions::MusicEffect::AddToQueue {
            track: track.into(),
        },
    )
    .unwrap();
    app.effect(&crate::actions::MusicEffect::SetVolume { volume: 37 })
        .unwrap();
    app.effect_on(
        "music.player:player",
        &crate::actions::MusicEffect::ToggleShuffle,
    )
    .unwrap();
    app.effect_on(
        "music.player:player",
        &crate::actions::MusicEffect::CycleRepeat,
    )
    .unwrap();
    let output = app
        .effect_on(
            "music.player:player",
            &crate::actions::MusicEffect::PlaybackStatus,
        )
        .expect("read-only playback observations are available on the player");
    let observed = output
        .value
        .expect("the declared result contains observations");
    assert_eq!(observed["confirmed_playing"], false);
    assert_eq!(observed["rendered_buffers"], 0);
    assert!(observed["sink"].is_null());
    assert!(observed["spotify_stage"].is_null());
    assert_eq!(observed["queue"], serde_json::json!([track]));
    assert_eq!(observed["queue_index"], 0);
    assert_eq!(observed["volume"], 37);
    assert_eq!(observed["shuffle"], true);
    assert_eq!(observed["repeat"], "all");
    assert_eq!(app.read_document(|doc| doc.clone().get_heads()), before);
}

#[test]
fn playback_failure_does_not_advertise_retry_of_an_at_most_once_invocation() {
    use ui::tilo_resource::Effect as _;
    let (mut app, _) = app_with_doc();
    let invocation = tilo_actions::InvocationId::new();
    app.pb.current = Some("tk_g1".into());
    app.active_resolve_request = Some(42);
    app.begin_resource_stream_invocation(invocation, 42);
    app.apply_resolve_failure("tk_g1", 42, "Spotify did not confirm Premium eligibility");
    let Some(tilo_actions::InvocationUpdate::Failed { error, .. }) =
        app.take_effect_result(invocation)
    else {
        panic!("the actual resolver failure must settle its pending invocation");
    };
    let contract = crate::actions::MusicEffect::Play {
        track: "tk_g1".into(),
        queued: None,
    }
    .bound()
    .contract;
    assert_eq!(contract.retry, tilo_actions::RetryPolicy::AtMostOnce);
    assert!(!error.retryable);
    assert!(error.message.contains("Premium eligibility"));
}
