use super::{MusicApp, Track};
#[cfg(all(test, feature = "audio"))]
use ui::widget::Continuable as _;

impl MusicApp {
    pub(super) fn make_open_file_button() -> ui::widget::Button {
        let resource = tilo_actions::Resource::new("music.library", "music.library", "Library")
            .with_action(crate::actions::open_local_file_spec());
        #[allow(
            clippy::expect_used,
            reason = "the canonical open-file action is statically zero-argument"
        )]
        let binding = tilo_actions::ActionBinding::unit_for(&resource, "open-file")
            .expect("canonical Music picker binding");
        ui::widget::Button::new("music-open-file", "music-open-audio-file", None)
            .content_sized()
            .on_action(binding)
    }

    pub(super) fn is_local_file(&self, id: &str) -> bool {
        self.local_file
            .as_ref()
            .is_some_and(|file| file.track.id == id)
    }

    pub(super) fn begin_resource_playback(
        &mut self,
        invocation: tilo_actions::InvocationId,
        playback: crate::model::Playback,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if self.resource_stream_invocation.is_some() {
            return Err("another Music stream-start operation is still resolving".into());
        }
        let resuming = self.pb.current == playback.current && self.audio.has_stream();
        let frames_before = if resuming {
            self.audio.frames_rendered()
        } else {
            0
        };
        let request = self.apply_resource_playback(playback)?;
        self.register_playback_start(invocation, request, frames_before, resuming)
    }

    /// Cache hits still need decoder evidence. Only an already loaded source
    /// being resumed may reuse prior PCM together with confirmed playback.
    pub(super) fn register_playback_start(
        &mut self,
        invocation: tilo_actions::InvocationId,
        request: Option<u64>,
        frames_before: u64,
        resuming: bool,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if let Some(request_id) = request {
            self.begin_resource_stream_invocation(invocation, request_id);
        } else {
            let request = self
                .next_worker_request_id()
                .ok_or_else(|| "Music playback request counter exhausted".to_string())?;
            self.begin_resource_stream_invocation(invocation, request);
            if let Some((_, _, baseline, _)) = self.resource_stream_invocation.as_mut() {
                *baseline = frames_before;
            }
            if resuming {
                self.resource_stream_resume = self.pb.current.clone();
            }
        }
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }

    pub(super) fn begin_local_file_pick(
        &mut self,
        invocation: tilo_actions::InvocationId,
    ) -> Result<(), String> {
        if self.local_file_pick.is_some() || self.resource_stream_invocation.is_some() {
            return Err("Another Music file selection or playback request is pending".into());
        }
        if !crate::audio::AudioEngine::is_live() {
            return Err("This Music build does not include audio playback".into());
        }
        let request = ui::portal::PickRequest::open("Open audio file", "Open")
            .thread_name("music-open-audio")
            .filter(
                ui::portal::FileFilter::new("Audio files")
                    .mimetype("audio/*")
                    .glob("*.wav")
                    .glob("*.wave")
                    .glob("*.mp3")
                    .glob("*.flac")
                    .glob("*.ogg")
                    .glob("*.oga")
                    .glob("*.opus")
                    .glob("*.m4a")
                    .glob("*.aac")
                    .glob("*.aif")
                    .glob("*.aiff")
                    .glob("*.wma"),
            )
            .all_files();
        let receiver = request.send();
        self.local_file_pick = Some((invocation, receiver));
        self.dirty = true;
        Ok(())
    }

    pub(super) fn drain_local_file_pick(&mut self) {
        let Some((invocation, receiver)) = &self.local_file_pick else {
            return;
        };
        let invocation = *invocation;
        let outcome = match receiver.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                ui::portal::PortalOutcome::Failed("The file picker stopped without a result".into())
            }
        };
        self.local_file_pick = None;
        let error = match outcome {
            ui::portal::PortalOutcome::Opened(path) => {
                match path
                    .to_str()
                    .ok_or_else(|| "The selected path is not valid Unicode".to_string())
                    .and_then(|path| self.open_local_file(path))
                {
                    Ok(request) => {
                        self.begin_resource_stream_invocation(invocation, request);
                        return;
                    }
                    Err(error) => error,
                }
            }
            ui::portal::PortalOutcome::Cancelled => {
                self.resource_stream_results
                    .push(tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some("No audio file was selected".into()),
                    });
                return;
            }
            ui::portal::PortalOutcome::Failed(error) => error,
        };
        self.toast = Some((error.clone(), ui::time::now()));
        self.resource_stream_results
            .push(tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "music.open_file_failed".into(),
                    message: error,
                    details: None,
                    retryable: true,
                },
            });
    }

    /// Open one explicitly requested device-private file. The same operation
    /// serves command-line launches and the exported resource action.
    pub(crate) fn open_local_file(&mut self, source: &str) -> Result<u64, String> {
        if !crate::audio::AudioEngine::is_live() {
            return Err("This Music build does not include audio playback".into());
        }
        let file = crate::local_file::LocalFile::open(source, self.local_file.as_ref())?;
        let id = file.track.id.clone();
        let previous_file = self.local_file.clone();
        let previous_playback = self.pb.clone();
        let previous_playhead = self.playhead_ms;
        let previous_bound_track = self.bound_track.clone();
        let previous_shuffle = self.shuffle_order.clone();
        self.local_file = Some(file);
        self.pb.current = Some(id.clone());
        self.pb.queue = vec![id];
        self.pb.queue_index = 0;
        self.pb.position_ms = 0;
        self.pb.playing = true;
        self.playhead_ms = 0.0;
        self.bound_track.clone_from(&self.pb.current);
        self.shuffle_order.clear();
        // Persistence failure must not start a new decoder behind a failed
        // operation. Restore the old device intent before returning the error.
        if let Err(error) = self.persist_device_profile() {
            self.local_file = previous_file;
            self.pb = previous_playback;
            self.playhead_ms = previous_playhead;
            self.bound_track = previous_bound_track;
            self.shuffle_order = previous_shuffle;
            return Err(error);
        }
        let request = match self.start_local_file() {
            Ok(request) => request,
            Err(error) => {
                self.pb.playing = false;
                self.audio.stop();
                self.playback_error = Some(error.clone());
                return Err(error);
            }
        };
        self.dirty = true;
        Ok(request)
    }

    pub(super) fn start_local_file(&mut self) -> Result<u64, String> {
        let file = self.local_file.as_ref().ok_or("No local file is open")?;
        if !crate::files::is_nonempty_file(&file.path) {
            return Err("Local audio file is unavailable or empty".into());
        }
        let uri = media::source::path_to_file_uri(&file.path).map_err(|error| error.to_string())?;
        let request = self
            .next_worker_request_id()
            .ok_or("Music request identity space is exhausted")?;
        self.supersede_resource_stream_if_needed(request);
        // Retire any previous online resolution before accepting local intent.
        self.active_resolve_request = None;
        self.resolve_in_flight = 0;
        self.playback_error = None;
        self.audio.set_volume(f32::from(self.pb.volume) / 100.0);
        self.audio.load(&uri);
        if self.pb.position_ms > 0 {
            self.audio.seek_ms(self.pb.position_ms);
        }
        if self.pb.playing {
            self.audio.play();
        }
        Ok(request)
    }

    pub(super) fn playback_track(&self, id: &str) -> Option<&Track> {
        self.local_file
            .as_ref()
            .filter(|file| file.track.id == id)
            .map(|file| &file.track)
            .or_else(|| self.lib.track(id))
    }

    pub(super) fn local_file_resource(&self) -> Option<crate::actions::CatalogTrackResource> {
        let file = self.local_file.as_ref()?;
        Some(crate::actions::CatalogTrackResource {
            id: file.track.id.clone(),
            label: file.track.title.clone(),
            album_id: String::new(),
            artist_id: String::new(),
            offline: false,
            liked: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::widget::Widget;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn local_file_intents_are_canonical_and_uris_are_validated() {
        // Phase `raw_path_operation_is_not_remotely_decodable`.
        {
            let mut app = MusicApp::new_for_shot();
            let mut doc = automerge::AutoCommit::new();
            let payload = br#"{"OpenLocalFile":{"source":"private.wav"}}"#;
            assert!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::Effect,
                        crate::actions::MUSIC_OPERATION_TYPE,
                        payload
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .is_err()
            );
            assert!(app.local_file_pick.is_none());
            assert!(app.local_file.is_none());
            assert!(!app.audio.has_stream());
        }

        // Phase `empty_home_shared_button_emits_canonical_picker_intent`.
        {
            use ui::widget::Widget;
            for scale in [1.0, 2.0] {
                let mut app = MusicApp::new_for_shot();
                app.set_offline_for_shot();
                let mut doc = automerge::AutoCommit::new();
                app.sync(&mut doc, &automerge::ROOT);
                app.rect = vello::kurbo::Rect::new(0.0, 0.0, 1000.0, 850.0);
                let theme = ui::theme::Theme::dark().with_text_scale(scale);
                let mut tree = taffy::TaffyTree::<()>::new();
                let node =
                    app.build_layout(&mut tree, &theme, &mut std::collections::HashMap::new());
                let mut text = ui::text_render::TextRenderer::new();
                text.set_text_scale(scale);
                app.render_frame(
                    &mut vello::Scene::new(),
                    vello::kurbo::Affine::IDENTITY,
                    &mut text,
                    &theme,
                );
                let rect = app.open_file_button.get_rect();
                assert!(rect.width() >= 100.0 && rect.height() >= 40.0);
                assert!(app.rect.contains(rect.center()));
                let (_, text_height) = text.measure(
                    &ui::i18n!("music-open-audio-file"),
                    14.0,
                    Some((rect.width() as f32 - 2.0 * theme.padding_standard).max(0.0)),
                );
                assert!(rect.height() >= f64::from(text_height + theme.padding_standard));
                assert_eq!(tree.children(node).unwrap().len(), app.children().len());
                let device_id = unsafe { std::mem::zeroed() };
                let events = [
                    winit::event::WindowEvent::CursorMoved {
                        device_id,
                        position: winit::dpi::PhysicalPosition::new(
                            rect.center().x,
                            rect.center().y,
                        ),
                    },
                    winit::event::WindowEvent::MouseInput {
                        device_id,
                        state: winit::event::ElementState::Pressed,
                        button: winit::event::MouseButton::Left,
                    },
                    winit::event::WindowEvent::MouseInput {
                        device_id,
                        state: winit::event::ElementState::Released,
                        button: winit::event::MouseButton::Left,
                    },
                ];
                for event in events {
                    app.handle_event(
                        &event,
                        &mut ui::widget::EventCtx {
                            scale_factor: 1.0,
                            modifiers: &winit::event::Modifiers::default(),
                            state: &mut doc,
                            path: &automerge::ROOT,
                            handled: &mut false,
                            focus_target: &mut None,
                            node_id: node,
                            taffy: &tree,
                        },
                    );
                }
                let intent = ui::widget::controls::take_control_invocation(&mut app)
                    .expect("visible shared button intent");
                assert_eq!(
                    intent.address,
                    tilo_actions::ActionAddress::new("music.library", "open-file")
                );
                assert!(intent.arguments.is_empty());
                assert!(
                    app.local_file_pick.is_none(),
                    "the control must await broker admission"
                );
            }
        }

        // Phase `home_picker_is_in_host_focus_order_only_while_visible`.
        {
            let mut music = MusicApp::new_for_shot();
            music.set_offline_for_shot();
            let (services, recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);
            recorder.respond_to_pick(ui::portal::PortalOutcome::Cancelled);
            let mut state = ui::State::new();
            music.sync(&mut state.doc, &automerge::ROOT);
            let mut host = ui::App::new(state, Box::new(music));
            crate::actions::register(host.action_registry_mut());
            host.init_layout();
            host.refresh_semantic_tree();
            assert_eq!(host.focusable_order.len(), 1);
            for destination in ["songs", "home"] {
                let resource = crate::actions::view_resource_id(destination);
                let target = ui::accesskit_bridge::resource_action_node_id(&resource, "open");
                host.dispatch_a11y_action(target, &ui::a11y::Action::Activate);
                host.rebuild_structure();
                host.refresh_focus_order();
                host.refresh_semantic_tree();
                let home = destination == "home";
                assert_eq!(host.focusable_order.len(), usize::from(home));
                assert_eq!(
                    host.control_action_coverage()
                        .iter()
                        .any(|entry| entry.binding.control_id == "music-open-file"),
                    home
                );
                assert_eq!(
                    host.semantic_tree().control_paths("music-open-file").len(),
                    usize::from(home)
                );
            }
            #[cfg(feature = "audio")]
            {
                let path = host
                    .semantic_tree()
                    .control_paths("music-open-file")
                    .pop()
                    .unwrap();
                let id = path.last().unwrap().clone();
                assert!(host.invoke_semantic_action(&id, "activate", None).unwrap());
                host.refresh_focus_order(); // consume the recorded cancellation; never open a dialog
                assert_eq!(recorder.pick_requests()[0].title(), "Open audio file");
            }
        }

        // Phase `local_paths_and_file_uris_are_equivalent_and_reject_unsupported_sources`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("音楽 #1.wav");
            wav(&path);
            let first = crate::local_file::LocalFile::open(path.to_str().unwrap(), None).unwrap();
            let uri = media::source::path_to_file_uri(&path).unwrap();
            let same = crate::local_file::LocalFile::open(&uri, Some(&first)).unwrap();
            assert_eq!(first, same);
            assert!(crate::local_file::LocalFile::open("https://example.com/a.wav", None).is_err());
            assert!(crate::local_file::LocalFile::open("music://album/x", None).is_err());
            assert!(
                crate::local_file::LocalFile::open(dir.path().to_str().unwrap(), None).is_err()
            );
            let text = dir.path().join("wrong.txt");
            std::fs::write(&text, b"not audio").unwrap();
            assert!(crate::local_file::LocalFile::open(text.to_str().unwrap(), None).is_err());
        }
    }

    #[test]
    #[cfg(feature = "audio")]
    fn picker_cancel_failure_and_single_pending_request_use_operation_receipts() {
        for cancelled in [true, false] {
            let mut app = MusicApp::new_for_shot();
            let mut doc = automerge::AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
            let before = doc.get_heads();
            let (services, recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);
            recorder.respond_to_pick(if cancelled {
                ui::portal::PortalOutcome::Cancelled
            } else {
                ui::portal::PortalOutcome::Failed("test picker failure".into())
            });
            let invocation = tilo_actions::InvocationId::new();
            let payload =
                serde_json::to_vec(&crate::actions::MusicOperation::ChooseAudioFile).unwrap();
            assert_eq!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        tilo_actions::OperationKind::Effect,
                        crate::actions::MUSIC_OPERATION_TYPE,
                        &payload
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Accepted)
            );
            assert!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::Effect,
                        crate::actions::MUSIC_OPERATION_TYPE,
                        &payload
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .is_err()
            );
            app.sync(&mut doc, &automerge::ROOT);
            let result = app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT);
            if cancelled {
                assert!(matches!(
                    result,
                    Some(tilo_actions::InvocationUpdate::Cancelled { .. })
                ));
            } else {
                assert!(matches!(
                    result,
                    Some(tilo_actions::InvocationUpdate::Failed { .. })
                ));
            }
            assert_eq!(doc.get_heads(), before);
            assert!(app.local_file_pick.is_none());
            assert!(app.local_file.is_none());
            assert!(!app.audio.has_stream());
        }
    }

    #[test]
    #[cfg(feature = "audio")]
    #[serial_test::serial(audio_backend)]
    #[expect(
        clippy::float_cmp,
        reason = "restarting assigns the playhead exactly 0.0"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn local_source_switches_wait_for_decoder_and_retire_old_audio() {
        // Phase `fully_prerolled_finite_source_resume_waits_for_confirmed_playing`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("one-buffer.wav");
            wav(&path);
            let mut bytes = std::fs::read(&path).unwrap();
            bytes.truncate(44 + 16000);
            bytes[4..8].copy_from_slice(&16036u32.to_le_bytes());
            bytes[40..44].copy_from_slice(&16000u32.to_le_bytes());
            std::fs::write(&path, bytes).unwrap();
            let mut app = MusicApp::new_for_shot();
            let mut doc = automerge::AutoCommit::new();
            app.open_local_file(path.to_str().unwrap()).unwrap();
            wait_for_audio(&mut app);
            let first_output = tilo_test_env::hang_deadline();
            while app.audio.frames_rendered() < 2 {
                app.tick();
                assert!(ui::time::now() < first_output);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            app.pb.playing = false;
            app.audio.pause();
            while app.audio.is_playing_real() {
                assert!(ui::time::now() < first_output);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert!(!app.audio.reached_eos());
            assert!(!app.audio.is_playing_confirmed());
            let frames_before = app.audio.frames_rendered();
            assert!(frames_before > 0);
            let mut playback = app.pb.clone();
            playback.playing = true;
            let payload =
                serde_json::to_vec(&crate::actions::MusicOperation::SetPlayback { playback })
                    .unwrap();
            let invocation = tilo_actions::InvocationId::new();
            assert_eq!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        tilo_actions::OperationKind::Effect,
                        crate::actions::MUSIC_OPERATION_TYPE,
                        &payload
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Accepted)
            );
            assert!(
                app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
                    .is_none()
            );
            let deadline = tilo_test_env::hang_deadline();
            loop {
                app.tick();
                if let Some(result) =
                    app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
                {
                    assert!(matches!(
                        result,
                        tilo_actions::InvocationUpdate::Succeeded { .. }
                    ));
                    assert!(app.audio.is_playing_confirmed());
                    assert_eq!(
                        app.audio.frames_rendered(),
                        frames_before,
                        "the finite source already reached the sink during preroll"
                    );
                    assert!(app.resource_stream_resume.is_none());
                    break;
                }
                assert!(ui::time::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            while !app.audio.reached_eos() {
                app.audio.pump();
                assert!(ui::time::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert!(
                !app.audio.is_playing_confirmed(),
                "ended PCM cannot confirm a new resume"
            );
            app.audio.stop();
        }

        // Phase `switching_to_cached_source_uses_new_output_baseline`.
        {
            check_cached_source_switch(false);
            check_cached_source_switch(true);
        }

        // Phase `cached_play_actions_wait_for_decoder_success_or_failure`.
        {
            for queue_item in [false, true] {
                for valid_audio in [false, true] {
                    check_cached_play_action(queue_item, valid_audio);
                }
            }
        }

        // Phase `paused_queue_removal_retires_old_audio_before_resuming_selected_uri`.
        {
            check_replaced_paused_source(false);
        }

        // Phase `continuation_retires_old_audio_before_resuming_selected_uri`.
        {
            check_replaced_paused_source(true);
        }

        // Phase `local_file_row_restart_previous_and_repeat_match_canonical_restart_position`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("restart.wav");
            wav(&path);
            let file = crate::local_file::LocalFile::open(path.to_str().unwrap(), None).unwrap();
            for route in ["row", "previous", "repeat-one", "canonical"] {
                let mut app = MusicApp::new_for_shot();
                let mut doc = automerge::AutoCommit::new();
                app.sync(&mut doc, &automerge::ROOT);
                let id = file.track.id.clone();
                app.local_file = Some(file.clone());
                app.pb.current = Some(id.clone());
                app.pb.queue = vec![id.clone()];
                app.pb.position_ms = 1000;
                app.playhead_ms = 1000.0;
                let mut invocation = None;
                match route {
                    "row" => app.on_play(std::sync::Arc::new(vec![id.clone()]), 0, &mut doc),
                    "previous" => app.on_prev(&mut doc),
                    "repeat-one" => {
                        app.pb.repeat = crate::model::RepeatMode::One;
                        app.pb.playing = true;
                        app.auto_advance();
                    }
                    "canonical" => {
                        let resource_id = crate::actions::queue_item_resource_id(0, &id);
                        let resource =
                            crate::app::MusicApp::publish_resource(&app, &doc, &resource_id)
                                .unwrap();
                        let action = resource
                            .actions
                            .iter()
                            .find(|action| action.id == "play")
                            .unwrap();
                        let encoded = action.build_json(&serde_json::Map::new()).unwrap();
                        let request = tilo_actions::InvocationId::new();
                        assert_eq!(
                            app.start_operation(
                                &ui::widget::ResourceOperation::local(
                                    request,
                                    tilo_actions::OperationKind::Effect,
                                    &encoded.type_id,
                                    &encoded.payload
                                ),
                                &mut doc,
                                &automerge::ROOT
                            )
                            .unwrap(),
                            Some(ui::widget::ResourceOperationStart::Accepted)
                        );
                        invocation = Some(request);
                    }
                    _ => unreachable!(),
                }
                assert_eq!(
                    app.pb.position_ms, 0,
                    "{route} retained the previous saved position"
                );
                assert_eq!(app.playhead_ms, 0.0);
                wait_for_audio(&mut app);
                assert!(
                    app.audio.position_ms().unwrap_or(u64::MAX) < 500,
                    "{route} sought to the old position instead of restarting"
                );
                if let Some(invocation) = invocation {
                    assert!(matches!(
                        app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT),
                        Some(tilo_actions::InvocationUpdate::Succeeded { .. })
                    ));
                }
                app.audio.stop();
            }
        }
    }

    #[cfg(feature = "audio")]
    fn check_cached_play_action(queue_item: bool, valid_audio: bool) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cached.wav");
        wav(&path);
        let bytes = if valid_audio {
            std::fs::read(path).unwrap()
        } else {
            b"Hash-valid cache content is not necessarily decodable audio".to_vec()
        };
        let mut app = MusicApp::new_for_shot();
        app.set_offline_for_shot();
        app.seed_sample_library();
        let mut doc = automerge::AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        let id = app.lib.tracks.keys().next().unwrap().clone();
        app.media_cache
            .install_bytes(&app.media_cache_root, &id, &bytes, 1_000_000)
            .unwrap();
        app.pb.queue = vec![id.clone()];
        let resource_id = if queue_item {
            crate::actions::queue_item_resource_id(0, &id)
        } else {
            crate::actions::track_resource_id(&id)
        };
        let resource = crate::app::MusicApp::publish_resource(&app, &doc, &resource_id).unwrap();
        let action = resource
            .actions
            .iter()
            .find(|action| action.id == "play")
            .unwrap();
        let encoded = action.build_json(&serde_json::Map::new()).unwrap();
        let invocation = tilo_actions::InvocationId::new();
        assert_eq!(
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    invocation,
                    action.kind,
                    &encoded.type_id,
                    &encoded.payload
                ),
                &mut doc,
                &automerge::ROOT
            )
            .unwrap(),
            Some(ui::widget::ResourceOperationStart::Accepted)
        );
        assert_eq!(app.resource_stream_invocation.as_ref().unwrap().2, 0);
        assert!(
            app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
                .is_none()
        );
        let deadline = tilo_test_env::hang_deadline();
        let update = loop {
            app.tick();
            if let Some(update) =
                app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
            {
                break update;
            }
            assert!(ui::time::now() < deadline, "cached decoder never settled");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        if valid_audio {
            assert!(matches!(
                update,
                tilo_actions::InvocationUpdate::Succeeded { .. }
            ));
            assert!(app.audio.frames_rendered() > 0);
            let pcm = app.audio.capture().unwrap().snapshot();
            assert!(pcm.peak > 0, "the cached tone reached the sink: {pcm:?}");
        } else {
            assert!(matches!(
                update,
                tilo_actions::InvocationUpdate::Failed { .. }
            ));
            assert_eq!(app.audio.frames_rendered(), 0);
            assert!(!app.pb.playing);
            assert!(app.playback_error.is_some());
        }
        assert!(
            app.worker.is_none(),
            "cached playback must not use a network resolver"
        );
        app.audio.stop();
    }

    #[cfg(feature = "audio")]
    fn check_cached_source_switch(paused_selection: bool) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cached.wav");
        wav(&path);
        let mut app = MusicApp::new_for_shot();
        app.set_offline_for_shot();
        app.seed_sample_library();
        let mut doc = automerge::AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        app.open_local_file(path.to_str().unwrap()).unwrap();
        wait_for_audio(&mut app);
        assert!(app.audio.frames_rendered() > 0);
        let id = app.lib.tracks.keys().next().unwrap().clone();
        app.media_cache
            .install_bytes(
                &app.media_cache_root,
                &id,
                &std::fs::read(path).unwrap(),
                1_000_000,
            )
            .unwrap();
        let mut next = app.pb.clone();
        next.current = Some(id.clone());
        next.queue = vec![id];
        next.position_ms = 500;
        if paused_selection {
            next.playing = false;
            let pause_payload = serde_json::to_vec(&crate::actions::MusicOperation::SetPlayback {
                playback: next.clone(),
            })
            .unwrap();
            assert!(matches!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::Effect,
                        crate::actions::MUSIC_OPERATION_TYPE,
                        &pause_payload
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
            assert!(!app.audio.has_stream());
            assert!(app.active_resolve_request.is_none());
            assert_eq!(app.pb.position_ms, 500);
        }
        next.playing = true;
        let payload =
            serde_json::to_vec(&crate::actions::MusicOperation::SetPlayback { playback: next })
                .unwrap();
        let invocation = tilo_actions::InvocationId::new();
        assert_eq!(
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    invocation,
                    tilo_actions::OperationKind::Effect,
                    crate::actions::MUSIC_OPERATION_TYPE,
                    &payload
                ),
                &mut doc,
                &automerge::ROOT
            )
            .unwrap(),
            Some(ui::widget::ResourceOperationStart::Accepted)
        );
        assert_eq!(app.resource_stream_invocation.as_ref().unwrap().2, 0);
        wait_for_audio(&mut app);
        let expected_path = app
            .media_cache
            .object_path(&app.media_cache_root, app.pb.current.as_deref().unwrap())
            .unwrap();
        let expected_uri = media::source::path_to_file_uri(&expected_path).unwrap();
        assert_eq!(app.audio.current_uri(), Some(expected_uri.as_str()));
        assert!(app.audio.position_ms().unwrap_or(0) >= 400);
        assert!(matches!(
            app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
        app.audio.stop();
    }

    #[cfg(feature = "audio")]
    #[expect(
        clippy::too_many_lines,
        reason = "one end-to-end scenario: stage, replace the source, then resume, read top to bottom"
    )]
    fn check_replaced_paused_source(continuation: bool) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old-local.wav");
        wav(&path);
        let mut app = MusicApp::new_for_shot();
        app.set_offline_for_shot();
        app.seed_sample_library();
        let mut doc = automerge::AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        app.open_local_file(path.to_str().unwrap()).unwrap();
        wait_for_audio(&mut app);
        let old_id = app.pb.current.clone().unwrap();
        let next_id = app.lib.tracks.keys().next().unwrap().clone();
        app.media_cache
            .install_bytes(
                &app.media_cache_root,
                &next_id,
                &std::fs::read(path).unwrap(),
                1_000_000,
            )
            .unwrap();
        if continuation {
            assert!(
                app.restore_continuation_metadata(
                    &ui::collaboration::ContinuationMetadata {
                        schema_version: 1,
                        bytes: serde_json::to_vec(&serde_json::json!({
                            "track_id": next_id, "position_ms": 500, "paused": true,
                        }))
                        .unwrap(),
                    },
                    &mut doc,
                    &automerge::ROOT
                )
            );
        } else {
            let mut paused = app.pb.clone();
            paused.playing = false;
            paused.position_ms = 500;
            paused.queue.push(next_id.clone());
            app.apply_resource_playback(paused).unwrap();
            let resource_id = crate::actions::queue_item_resource_id(0, &old_id);
            let resource =
                crate::app::MusicApp::publish_resource(&app, &doc, &resource_id).unwrap();
            let remove = resource
                .actions
                .iter()
                .find(|action| action.id == "remove")
                .unwrap();
            let encoded = remove.build_json(&serde_json::Map::new()).unwrap();
            assert!(matches!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::Effect,
                        &encoded.type_id,
                        &encoded.payload
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
        }
        assert!(
            !app.audio.has_stream(),
            "old selected source must be retired"
        );
        assert!(!app.audio.is_playing_real());
        assert!(app.active_resolve_request.is_none());
        assert!(app.resource_stream_resume.is_none());
        assert!(!app.pb.playing);
        assert_eq!(app.pb.current.as_deref(), Some(next_id.as_str()));
        assert_eq!(app.pb.queue, vec![next_id.clone()]);
        assert_eq!(app.pb.queue_index, 0);
        assert_eq!(app.pb.position_ms, if continuation { 500 } else { 0 });
        let player =
            crate::app::MusicApp::publish_resource(&app, &doc, "music.player:player").unwrap();
        let play = player
            .actions
            .iter()
            .find(|action| action.id == "play-pause")
            .unwrap();
        let encoded = play.build_json(&serde_json::Map::new()).unwrap();
        let invocation = tilo_actions::InvocationId::new();
        assert_eq!(
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    invocation,
                    tilo_actions::OperationKind::Effect,
                    &encoded.type_id,
                    &encoded.payload
                ),
                &mut doc,
                &automerge::ROOT
            )
            .unwrap(),
            Some(ui::widget::ResourceOperationStart::Accepted)
        );
        wait_for_audio(&mut app);
        let path = app
            .media_cache
            .object_path(&app.media_cache_root, &next_id)
            .unwrap();
        let uri = media::source::path_to_file_uri(&path).unwrap();
        assert_eq!(app.audio.current_uri(), Some(uri.as_str()));
        if continuation {
            assert!(app.audio.position_ms().unwrap_or(0) >= 400);
        }
        assert!(matches!(
            app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
        app.audio.stop();
    }

    /// Three seconds of a 440 Hz tone, 8 kHz 16-bit mono: silence would not
    /// show that the decoder produced the file's samples.
    fn wav(path: &std::path::Path) {
        let bytes = 48_000u32;
        let mut data = Vec::new();
        data.extend_from_slice(b"RIFF");
        data.extend_from_slice(&(36 + bytes).to_le_bytes());
        data.extend_from_slice(b"WAVEfmt ");
        data.extend_from_slice(&16u32.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&8000u32.to_le_bytes());
        data.extend_from_slice(&16000u32.to_le_bytes());
        data.extend_from_slice(&2u16.to_le_bytes());
        data.extend_from_slice(&16u16.to_le_bytes());
        data.extend_from_slice(b"data");
        data.extend_from_slice(&bytes.to_le_bytes());
        for i in 0..bytes / 2 {
            let phase = f64::from(i) * 440.0 * std::f64::consts::TAU / 8000.0;
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a unit sine scaled to 8000 is well inside i16"
            )]
            let sample = (phase.sin() * 8000.0) as i16;
            data.extend_from_slice(&sample.to_le_bytes());
        }
        std::fs::write(path, data).unwrap();
    }

    /// Drive the app until the file's decoded tone reaches the sink and, when
    /// the app is playing, the sink renders it on the pipeline clock. The
    /// engine captures in tests, so this observes the samples themselves and
    /// needs no sound server; between frames it waits for the next buffer
    /// rather than sleeping a fixed time.
    #[cfg(feature = "audio")]
    fn wait_for_audio(app: &mut MusicApp) {
        let capture = app
            .audio
            .capture()
            .expect("tests capture decoded audio")
            .clone();
        let until = tilo_test_env::hang_deadline();
        // A playing source must also be rendering before the caller acts on
        // it. Unlike a sound server's sink, the capture sink can stall when it
        // is paused while still completing its switch to PLAYING.
        let rendered_before = capture.snapshot().rendered;
        while app.audio.frames_rendered() == 0
            || app.audio.needs_poll()
            || (app.pb.playing && capture.snapshot().rendered == rendered_before)
        {
            app.tick();
            assert!(app.playback_error.is_none(), "{:?}", app.playback_error);
            assert!(ui::time::now() < until, "file decoder did not render PCM");
            capture.wait_for_more(std::time::Duration::from_millis(20));
        }
        let pcm = capture.snapshot();
        assert!(pcm.peak > 0, "the decoded tone reached the sink: {pcm:?}");
        app.tick();
    }

    #[test]
    #[cfg(feature = "audio")]
    #[serial_test::serial(audio_backend)]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn local_file_effects_settle_only_on_real_playback_or_real_failure() {
        // Phase `positional_local_file_uses_live_factory_without_polluting_shared_library`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("local song.wav");
            wav(&path);
            let mut app =
                crate::app_for_launch_source(Some(path.to_str().unwrap()), 0, false).unwrap();
            let mut doc = automerge::AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
            assert!(app.lib.tracks.is_empty());
            assert!(crate::doc::load_library(&doc).tracks.is_empty());
            assert_eq!(app.pb.queue.len(), 1);
            assert!(app.pb.playing);
            assert!(app.worker.is_none());
            wait_for_audio(&mut app);
            assert!(app.current_duration_ms() >= 2900);
            let semantics = app.accessibility().unwrap();
            let player = semantics
                .children
                .iter()
                .find(|node| node.name.as_deref() == Some("Music player"))
                .unwrap();
            assert!(player.value.as_ref().unwrap().starts_with("local song;"));
            assert!(
                crate::app::MusicApp::publish_resource(
                    &app,
                    &doc,
                    crate::actions::LOCAL_FILE_RESOURCE_ID
                )
                .is_some()
            );
            let json = serde_json::to_string(&automerge::AutoSerde::from(&doc)).unwrap();
            assert!(!json.contains("local song"));
            app.on_toggle_play(&mut doc);
            assert!(!app.pb.playing);
            let paused_by = tilo_test_env::hang_deadline();
            while app.audio.is_playing_real() {
                assert!(ui::time::now() < paused_by, "native source did not pause");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let resource = crate::app::MusicApp::publish_resource(
                &app,
                &doc,
                crate::actions::LOCAL_FILE_RESOURCE_ID,
            )
            .unwrap();
            let action = resource
                .actions
                .iter()
                .find(|action| action.id == "play-pause")
                .unwrap();
            let encoded = action.build_json(&serde_json::Map::new()).unwrap();
            let invocation = tilo_actions::InvocationId::new();
            let before = app.audio.frames_rendered();
            assert_eq!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        tilo_actions::OperationKind::Effect,
                        &encoded.type_id,
                        &encoded.payload
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Accepted)
            );
            assert!(
                app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
                    .is_none()
            );
            let deadline = tilo_test_env::hang_deadline();
            loop {
                app.tick();
                if let Some(result) =
                    app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
                {
                    let tilo_actions::InvocationUpdate::Succeeded { output, .. } = result else {
                        panic!("resume did not succeed: {result:?}");
                    };
                    assert_eq!(output.value, Some(serde_json::json!({"state":"playing"})));
                    assert!(app.audio.frames_rendered() >= before);
                    assert!(app.audio.is_playing_confirmed());
                    break;
                }
                assert!(
                    ui::time::now() < deadline,
                    "paused source did not resume output: frames {before} -> {}, playing {}, \
                     confirmed {}, error {:?}",
                    app.audio.frames_rendered(),
                    app.audio.is_playing_real(),
                    app.audio.is_playing_confirmed(),
                    app.playback_error,
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            app.audio.stop();
        }

        // Phase `exported_file_effect_settles_only_after_real_pcm_and_preserves_document`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("effect.wav");
            wav(&path);
            let mut app = MusicApp::new_for_shot();
            let mut doc = automerge::AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
            let before = doc.get_heads();
            let invocation = tilo_actions::InvocationId::new();
            let resources = ui::resources::resources(&app, &doc);
            let action = resources[0]
                .actions
                .iter()
                .find(|action| action.id == "open-file")
                .unwrap();
            assert_eq!(action.kind, tilo_actions::OperationKind::Effect);
            assert_eq!(action.locus, tilo_actions::ExecutionLocus::ClientLocal);
            assert!(action.params().is_empty());
            let (services, recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);
            recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(path));
            let encoded = action.build_json(&serde_json::Map::new()).unwrap();
            let payload = encoded.payload;
            assert_eq!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        tilo_actions::OperationKind::Effect,
                        crate::actions::MUSIC_OPERATION_TYPE,
                        &payload
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Accepted)
            );
            assert!(
                app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
                    .is_none()
            );
            app.sync(&mut doc, &automerge::ROOT);
            wait_for_audio(&mut app);
            assert!(matches!(
                app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
            app.sync(&mut doc, &automerge::ROOT);
            assert_eq!(doc.get_heads(), before);
            assert!(app.lib.tracks.is_empty());
            let profile = crate::storage::DeviceProfile {
                local_file: app.local_file.clone(),
                playback: app.pb.clone(),
                ..Default::default()
            };
            let profile_path = dir.path().join("profile.json");
            crate::storage::save_device_profile_to(&profile_path, &profile).unwrap();
            let restored = crate::storage::load_device_profile_from(&profile_path);
            assert_eq!(restored.local_file, app.local_file);
            assert!(!restored.playback.playing);
            app.audio.stop();
        }

        // Phase `restored_file_resumes_saved_position_and_missing_file_fails_the_effect`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("resume.wav");
            wav(&path);
            let file = crate::local_file::LocalFile::open(path.to_str().unwrap(), None).unwrap();
            let profile = crate::storage::DeviceProfile {
                playback: crate::model::Playback {
                    current: Some(file.track.id.clone()),
                    queue: vec![file.track.id.clone()],
                    position_ms: 1000,
                    ..Default::default()
                },
                local_file: Some(file),
                ..Default::default()
            };
            let profile_path = dir.path().join("profile.json");
            crate::storage::save_device_profile_to(&profile_path, &profile).unwrap();
            for missing in [false, true] {
                if missing {
                    std::fs::remove_file(&path).unwrap();
                }
                let restored = crate::storage::load_device_profile_from(&profile_path);
                let mut app = MusicApp::new_for_shot();
                app.local_file = restored.local_file;
                app.pb = restored.playback;
                let mut doc = automerge::AutoCommit::new();
                app.sync(&mut doc, &automerge::ROOT);
                let source = crate::app::MusicApp::publish_resource(
                    &app,
                    &doc,
                    crate::actions::LOCAL_FILE_RESOURCE_ID,
                )
                .unwrap();
                let play = source
                    .actions
                    .iter()
                    .find(|action| action.id == "play-pause")
                    .unwrap();
                let encoded = play.build_json(&serde_json::Map::new()).unwrap();
                let invocation = tilo_actions::InvocationId::new();
                let started = app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        play.kind,
                        &encoded.type_id,
                        &encoded.payload,
                    ),
                    &mut doc,
                    &automerge::ROOT,
                );
                if missing {
                    assert!(started.is_err());
                    assert!(!app.pb.playing);
                    assert!(!app.audio.has_stream());
                } else {
                    assert_eq!(
                        started.unwrap(),
                        Some(ui::widget::ResourceOperationStart::Accepted)
                    );
                    app.persist_playback(&mut doc);
                    assert_eq!(
                        app.pb.position_ms, 1000,
                        "unknown duration cannot erase the resume point"
                    );
                    wait_for_audio(&mut app);
                    assert!(app.audio.position_ms().unwrap() >= 900);
                    assert!(matches!(
                        app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT),
                        Some(tilo_actions::InvocationUpdate::Succeeded { .. })
                    ));
                    app.audio.stop();
                }
            }
        }

        // Phase `already_decoded_local_source_settles_after_late_invocation_registration`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("short.wav");
            wav(&path);
            let mut app = MusicApp::new_for_shot();
            let mut doc = automerge::AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
            let request = app.open_local_file(path.to_str().unwrap()).unwrap();
            wait_for_audio(&mut app);
            let invocation = tilo_actions::InvocationId::new();
            // A warm native pipeline can deliver its first buffer before the host
            // registers the invocation. New-source evidence must still count.
            app.begin_resource_stream_invocation(invocation, request);
            app.tick();
            assert!(matches!(
                app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
            app.audio.stop();
        }

        // Phase `undecodable_file_fails_the_actual_operation_without_fake_playback`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("corrupt.wav");
            std::fs::write(&path, b"not a WAV stream").unwrap();
            let mut app = MusicApp::new_for_shot();
            let mut doc = automerge::AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
            let invocation = tilo_actions::InvocationId::new();
            let (services, recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);
            recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(path));
            let payload =
                serde_json::to_vec(&crate::actions::MusicOperation::ChooseAudioFile).unwrap();
            assert_eq!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        tilo_actions::OperationKind::Effect,
                        crate::actions::MUSIC_OPERATION_TYPE,
                        &payload
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Accepted)
            );
            let until = tilo_test_env::hang_deadline();
            loop {
                app.sync(&mut doc, &automerge::ROOT);
                app.tick();
                if let Some(update) =
                    app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
                {
                    assert!(matches!(
                        update,
                        tilo_actions::InvocationUpdate::Failed { .. }
                    ));
                    break;
                }
                assert!(
                    ui::time::now() < until,
                    "decoder failure never reached the operation"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(!app.pb.playing);
            assert!(!app.audio.has_stream());
            assert!(app.playback_error.is_some());
        }
    }

    #[test]
    #[cfg(not(feature = "audio"))]
    fn silent_build_explicitly_refuses_file_playback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.wav");
        wav(&path);
        assert!(crate::app_for_launch_source(Some(path.to_str().unwrap()), 0, false).is_err());
    }

    #[test]
    #[cfg(feature = "audio")]
    fn profile_write_failure_does_not_start_the_requested_file() {
        const CHILD: &str = "TILO_TEST_LOCAL_FILE_PROFILE_FAILURE";
        if let Some(path) = std::env::var_os(CHILD) {
            assert_eq!(unsafe { gstreamer::ffi::gst_is_initialized() }, 0);
            let mut app = MusicApp::new_for_shot();
            app.persist_device_profile = true;
            assert!(app.open_local_file(path.to_str().unwrap()).is_err());
            assert!(app.local_file.is_none());
            assert!(app.pb.current.is_none());
            assert!(!app.audio.has_stream());
            assert!(!app.audio.needs_poll());
            assert_eq!(unsafe { gstreamer::ffi::gst_is_initialized() }, 0);
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile-failure.wav");
        wav(&path);
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "app::local_files::tests::profile_write_failure_does_not_start_the_requested_file",
                "--nocapture",
            ])
            .env(CHILD, &path)
            // An existing directory cannot be atomically replaced by a profile file.
            .env("TILO_MUSIC_PROFILE", dir.path())
            .env("XDG_DATA_HOME", dir.path().join("data"))
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .env("XDG_CACHE_HOME", dir.path().join("cache"))
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }
}
