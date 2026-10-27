//! Music's playback across a continuation or media handoff: known tracks
//! restore locally, real audio plays past its catalogue duration, and a
//! handoff waits for the decoder while keeping a paused preroll.

use super::*;

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
            Library::paths().playlists.insert_as(
                &ui::tilo_resource::Id::from_raw("pl_peer_library"),
                0,
                &crate::model::Playlist::named("pl_peer_library", "Peer library edit"),
            ),
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
        // Read the decoder right before the tick that may find its EOS and tear
        // it down: the playhead a tick leaves is as old as the gap between
        // ticks, which a loaded machine stretches past the end of the source.
        if let Some(position) = app.audio.position_ms() {
            furthest_position = furthest_position.max(ui::num::f64_of_u64(position));
        }
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
