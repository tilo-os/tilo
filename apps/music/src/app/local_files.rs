use super::{MusicApp, Track};
#[cfg(test)]
#[cfg(feature = "audio")]
use ui::widget::Continuable as _;
use ui::widget::toast::ToastKind;

impl MusicApp {
    /// The invocation of the library's Open audio file, as the window bar's
    /// slot runs it through the window's controls.
    pub(super) fn open_file_invocation() -> Option<tilo_actions::ControlInvocation> {
        let resource = tilo_actions::Resource::new("music.library", "music.library", "Library")
            .with_action(ui::tilo_resource::Effect::bound(
                &crate::actions::MusicEffect::OpenFile,
            ));
        tilo_actions::ActionBinding::unit_for(&resource, "open-file")
            .ok()?
            .invoke(())
            .ok()
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
    ) -> Result<ui::framework::Effected, String> {
        self.supersede_pending_stream_start();
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
    ) -> Result<ui::framework::Effected, String> {
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
        Ok(ui::framework::Effected::Pending)
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
        // Where files have no paths (a browser page) the chooser hands over
        // the file's contents instead.
        let pick = if tilo_paths::availability(tilo_paths::Feature::NativePaths)
            == tilo_paths::Availability::Available
        {
            super::LocalFilePick::Path(request.send())
        } else {
            super::LocalFilePick::Contents(request.send_contents())
        };
        self.local_file_pick = Some((invocation, pick));
        self.dirty = true;
        Ok(())
    }

    pub(super) fn drain_local_file_pick(&mut self) {
        enum Chosen {
            Path(String),
            Contents(ui::portal::PickedContents),
        }
        let Some((invocation, pick)) = &self.local_file_pick else {
            return;
        };
        let invocation = *invocation;
        let stopped = || Err("The file picker stopped without a result".to_string());
        // What was chosen, nothing (`None`), or why it could not be.
        let chosen: Option<Result<Chosen, String>> = match pick {
            super::LocalFilePick::Path(receiver) => match receiver.try_recv() {
                Ok(ui::portal::PortalOutcome::Opened(path)) => Some(
                    path.to_str()
                        .map(|path| Chosen::Path(path.to_owned()))
                        .ok_or_else(|| "The selected path is not valid Unicode".to_string()),
                ),
                Ok(ui::portal::PortalOutcome::Cancelled) => None,
                Ok(ui::portal::PortalOutcome::Failed(error)) => Some(Err(error)),
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(stopped()),
            },
            super::LocalFilePick::Contents(receiver) => match receiver.try_recv() {
                Ok(ui::portal::ContentsOutcome::Picked(files)) => files
                    .into_iter()
                    .next()
                    .map(|file| Ok(Chosen::Contents(file))),
                Ok(ui::portal::ContentsOutcome::Cancelled) => None,
                Ok(ui::portal::ContentsOutcome::Failed(error)) => Some(Err(error)),
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(stopped()),
            },
        };
        self.local_file_pick = None;
        let opened = chosen.map(|chosen| {
            chosen.and_then(|chosen| match chosen {
                Chosen::Path(path) => self.open_local_file(&path),
                Chosen::Contents(file) => self.open_local_contents(&file),
            })
        });
        let error = match opened {
            Some(Ok(request)) => {
                self.begin_resource_stream_invocation(invocation, request);
                return;
            }
            Some(Err(error)) => error,
            None => {
                self.resource_stream_results
                    .push(tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some("No audio file was selected".into()),
                    });
                return;
            }
        };
        self.notify(ToastKind::Error, error.clone());
        self.resource_stream_results
            .push(tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "music.open_file_failed".into(),
                    message: error,
                    details: None,
                    retryable: false,
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
        self.play_local_file(file)
    }

    /// Open a file the host handed over as contents: from where it is when
    /// it has a path, else from a page-local copy of its bytes.
    pub(crate) fn open_local_contents(
        &mut self,
        file: &ui::portal::PickedContents,
    ) -> Result<u64, String> {
        if let Some(location) = file.location.as_ref().and_then(|path| path.to_str()) {
            return self.open_local_file(location);
        }
        crate::local_file::check_supported(std::path::Path::new(&file.name))?;
        if file.bytes.is_empty() {
            return Err("Choose a non-empty audio file".into());
        }
        if !crate::audio::AudioEngine::is_live() {
            return Err("This Music build does not include audio playback".into());
        }
        let source = media::Source::from_contents(&file.name, &file.bytes)
            .map_err(|error| format!("Cannot open audio file: {error}"))?;
        match source {
            media::Source::Remote { url } => {
                let file = crate::local_file::LocalFile::from_page_url(&file.name, url)?;
                self.play_local_file(file)
            }
            staged => {
                let path = staged
                    .abs_path()
                    .ok_or("Cannot open audio file: it has no place on this device")?;
                let path = path
                    .to_str()
                    .ok_or("Cannot open audio file: its place is not valid Unicode")?;
                let mut local = crate::local_file::LocalFile::open(path, self.local_file.as_ref())?;
                // Named by what the person chose, not the staged copy.
                local.track.title = crate::local_file::file_title(std::path::Path::new(&file.name));
                self.play_local_file(local)
            }
        }
    }

    /// The open file to reopen next time. One playing from a page-local
    /// copy of its bytes is gone with the page, so it is not kept.
    pub(super) fn kept_local_file(&self) -> Option<crate::local_file::LocalFile> {
        self.local_file
            .clone()
            .filter(|file| file.page_url.is_none())
    }

    fn play_local_file(&mut self, file: crate::local_file::LocalFile) -> Result<u64, String> {
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

    pub(super) fn observe_local_duration(&mut self) {
        if let Some(file) = self
            .local_file
            .as_mut()
            .filter(|file| self.pb.current.as_deref() == Some(&file.track.id))
            && let Some(duration) = self.audio.duration_ms().filter(|duration| *duration > 0)
        {
            file.observed_duration_ms = Some(duration);
        }
    }

    pub(super) fn start_local_file(&mut self) -> Result<u64, String> {
        let file = self.local_file.as_ref().ok_or("No local file is open")?;
        if self.pb.playing
            && file
                .observed_duration_ms
                .is_some_and(|duration| self.pb.position_ms >= duration)
        {
            self.pb.position_ms = 0;
        }
        self.playhead_ms = self.pb.position_ms as f64;
        let uri = if let Some(url) = &file.page_url {
            url.clone()
        } else {
            if !crate::files::is_nonempty_file(&file.path) {
                return Err("Local audio file is unavailable or empty".into());
            }
            media::source::path_to_file_uri(&file.path).map_err(|error| error.to_string())?
        };
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
    #[cfg(feature = "audio")]
    use ui::widget::Widget;

    #[cfg(feature = "audio")]
    #[test]
    fn picker_failure_settles_the_at_most_once_effect_without_retry() {
        use ui::tilo_resource::Effect as _;
        let mut app = MusicApp::without_profile();
        let mut doc = automerge::AutoCommit::new();
        app.sync(&doc, &automerge::ROOT);
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        let before = doc.get_heads();
        let invocation = tilo_actions::InvocationId::new();
        let effect = crate::actions::MusicEffect::OpenFile;
        assert_eq!(
            effect.bound().contract.retry,
            tilo_actions::RetryPolicy::AtMostOnce
        );
        let action = crate::actions::MusicEffect::specs()
            .into_iter()
            .find(|action| action.id == "open-file")
            .unwrap();
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        recorder.respond_to_pick(ui::portal::PortalOutcome::Failed(
            "Fixture file access denied".into(),
        ));
        let payload = action.build_json(&serde_json::Map::new()).unwrap().payload;
        assert!(matches!(
            app.start_effect(invocation, &payload).unwrap(),
            ui::framework::Effected::Pending
        ));
        app.drain_local_file_pick();
        let Some(tilo_actions::InvocationUpdate::Failed {
            invocation: completed,
            error,
        }) = app.take_effect_result(invocation)
        else {
            panic!("the actual picker failure must settle its pending invocation");
        };
        assert_eq!(completed, invocation);
        assert_eq!(error.code, "music.open_file_failed");
        assert_eq!(error.message, "Fixture file access denied");
        assert!(!error.retryable);
        assert!(app.local_file_pick.is_none());
        assert!(app.local_file.is_none());
        assert!(!app.audio.has_stream());
        assert_eq!(doc.get_heads(), before);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn local_file_intents_are_canonical_and_uris_are_validated() {
        {
            let mut app = MusicApp::without_profile();
            let _doc = automerge::AutoCommit::new();
            let payload = br#"{"OpenLocalFile":{"source":"private.wav"}}"#;
            assert!(
                app.start_effect(tilo_actions::InvocationId::new(), payload)
                    .is_err()
            );
            assert!(app.local_file_pick.is_none());
            assert!(app.local_file.is_none());
            assert!(!app.audio.has_stream());
        }

        {
            let mut app = MusicApp::without_profile();
            app.disconnect();
            let mut doc = automerge::AutoCommit::new();
            app.sync(&doc, &automerge::ROOT);
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            let toolbar = ui::window_toolbar::declared().expect("the window declares its slots");
            assert!(
                toolbar
                    .actions
                    .iter()
                    .any(|action| action.id == super::super::window_bar::OPEN_FILE),
                "Home offers Open audio file in the window's bar: {toolbar:?}"
            );
            ui::window_toolbar::deliver(ui::window_toolbar::ToolbarEvent::Action {
                id: super::super::window_bar::OPEN_FILE.into(),
            });
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            let intent = ui::widget::controls::take_control_invocation(&mut app)
                .expect("the bar's slot queues the picker's intent");
            assert_eq!(intent.address.resource, "music.library");
            assert_eq!(
                intent.address.action,
                ui::tilo_resource::Effect::id(&crate::actions::MusicEffect::OpenFile)
            );
            assert!(intent.arguments.is_empty());
            assert!(
                app.local_file_pick.is_none(),
                "the control must await broker admission"
            );
        }

        {
            let (services, recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);
            recorder.respond_to_pick(ui::portal::PortalOutcome::Cancelled);
            let mut app = ui::framework::testing::TestApp::<crate::MusicShell>::new().unwrap();
            let home_focus = app.harness().focus_order();
            assert!(!home_focus.is_empty(), "Home source controls are focusable");
            let offers_open_file = || {
                ui::window_toolbar::declared().is_some_and(|toolbar| {
                    toolbar
                        .actions
                        .iter()
                        .any(|action| action.id == super::super::window_bar::OPEN_FILE)
                })
            };
            assert!(
                offers_open_file(),
                "Home offers its file picker in the window's bar"
            );
            app.call_on(
                &crate::actions::view_resource_id("songs"),
                "open",
                serde_json::json!({}),
            )
            .unwrap();
            app.settle();
            assert!(!offers_open_file(), "leaving Home removes the picker");
            assert!(
                app.harness().focus_order().len() < home_focus.len(),
                "the source controls leave focus order with Home"
            );
            assert!(
                app.harness().a11y_node_named("Back").is_none(),
                "a sidebar section is a destination, not a step to go back from"
            );
            app.call_on(
                &crate::actions::view_resource_id("home"),
                "open",
                serde_json::json!({}),
            )
            .unwrap();
            app.settle();
            assert_eq!(
                app.harness().focus_order().len(),
                home_focus.len(),
                "returning Home restores the picker"
            );
            assert!(app.harness().a11y_node_named("Back").is_none());
            assert!(offers_open_file(), "returning Home restores the picker");
            #[cfg(feature = "audio")]
            {
                app.call("open-file", serde_json::json!({})).unwrap();
                app.settle();
                assert_eq!(recorder.pick_requests()[0].title(), "Open audio file");
            }
        }

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
            let mut app = MusicApp::without_profile();
            let mut doc = automerge::AutoCommit::new();
            app.sync(&doc, &automerge::ROOT);
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            let before = doc.get_heads();
            let (services, recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);
            recorder.respond_to_pick(if cancelled {
                ui::portal::PortalOutcome::Cancelled
            } else {
                ui::portal::PortalOutcome::Failed("test picker failure".into())
            });
            let invocation = tilo_actions::InvocationId::new();
            let payload = crate::actions::effect_payload(&crate::actions::MusicEffect::OpenFile);
            assert!(matches!(
                app.start_effect(invocation, &payload).unwrap(),
                ui::framework::Effected::Pending
            ));
            assert!(
                app.start_effect(tilo_actions::InvocationId::new(), &payload)
                    .is_err()
            );
            app.sync(&doc, &automerge::ROOT);
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            let result = app.take_effect_result(invocation);
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

    #[cfg(feature = "audio")]
    #[test]
    #[serial_test::serial(audio_backend)]
    #[expect(
        clippy::float_cmp,
        reason = "EOF and replay assign exact integer-millisecond playhead values"
    )]
    fn local_decoder_duration_survives_eos_and_replay_rewinds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("finished local song.wav");
        wav(&path);
        let mut app = MusicApp::without_profile();
        app.open_local_file(path.to_str().unwrap()).unwrap();
        wait_for_audio(&mut app);
        let duration = app.current_duration_ms();
        assert!(duration >= 2900);
        let deadline = tilo_test_env::hang_deadline();
        while app.pb.playing {
            app.tick();
            assert!(
                ui::time::now() < deadline,
                "local decoder did not reach EOS"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!app.audio.has_stream());
        assert!(app.playback_error.is_none());
        assert_eq!(
            app.current_duration_ms(),
            duration,
            "the visible local duration must survive real decoder teardown"
        );
        assert_eq!(app.playhead_ms, duration as f64);
        let ui::framework::Effected::Done(status) = app
            .begin_effect(
                tilo_actions::InvocationId::new(),
                crate::actions::MusicEffect::PlaybackStatus,
            )
            .unwrap()
        else {
            panic!("playback inspection must return the actual stopped state");
        };
        let status = status.value.unwrap();
        assert_eq!(status["duration_ms"], duration);
        assert_eq!(
            status["position_ms"], duration,
            "actual local EOS must retain its observed end position after decoder teardown"
        );
        assert_eq!(status["confirmed_playing"], false);
        assert_eq!(status["requested_playing"], false);
        // The self-managed sink retains its actual decoded-buffer history at
        // EOS; a stopped source must not erase that evidence or report playing.
        assert_eq!(status["rendered_counter_unit"], "sink_buffers");
        assert!(status["rendered_buffers"].as_u64().unwrap() > 0);
        let resource =
            crate::app::MusicApp::publish_resource(&app, crate::actions::LOCAL_FILE_RESOURCE_ID)
                .unwrap();
        let action = resource
            .actions
            .iter()
            .find(|a| a.id == "play-pause")
            .unwrap();
        let encoded = action.build_json(&serde_json::Map::new()).unwrap();
        let invocation = tilo_actions::InvocationId::new();
        assert!(matches!(
            app.start_effect(invocation, &encoded.payload).unwrap(),
            ui::framework::Effected::Pending
        ));
        assert_eq!(
            app.pb.position_ms, 0,
            "replay must not seek to the ended position"
        );
        assert_eq!(app.playhead_ms, 0.0);
        let deadline = tilo_test_env::hang_deadline();
        loop {
            app.tick();
            if let Some(update) = app.take_effect_result(invocation) {
                let tilo_actions::InvocationUpdate::Succeeded { output, .. } = update else {
                    panic!("real replay did not succeed: {update:?}");
                };
                assert_eq!(output.value, Some(serde_json::json!({"state":"playing"})));
                action.contract.result.validate(&output).unwrap();
                assert!(app.audio.frames_rendered() > 0);
                assert!(app.audio.is_playing_confirmed());
                break;
            }
            assert!(ui::time::now() < deadline, "replay produced no actual PCM");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        app.audio.stop();
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
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("one-buffer.wav");
            wav(&path);
            let mut bytes = std::fs::read(&path).unwrap();
            bytes.truncate(44 + 16000);
            bytes[4..8].copy_from_slice(&16036u32.to_le_bytes());
            bytes[40..44].copy_from_slice(&16000u32.to_le_bytes());
            std::fs::write(&path, bytes).unwrap();
            let mut app = MusicApp::without_profile();
            let _doc = automerge::AutoCommit::new();
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
            assert!(!app.pb.playing);
            let payload = crate::actions::effect_payload(&crate::actions::MusicEffect::PlayPause);
            let invocation = tilo_actions::InvocationId::new();
            assert!(matches!(
                app.start_effect(invocation, &payload).unwrap(),
                ui::framework::Effected::Pending
            ));
            assert!(app.take_effect_result(invocation).is_none());
            let deadline = tilo_test_env::hang_deadline();
            loop {
                app.tick();
                if let Some(result) = app.take_effect_result(invocation) {
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

        {
            check_replaced_paused_source(false);
        }

        {
            check_replaced_paused_source(true);
        }

        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("restart.wav");
            wav(&path);
            let file = crate::local_file::LocalFile::open(path.to_str().unwrap(), None).unwrap();
            for route in ["row", "previous", "repeat-one", "canonical"] {
                let mut app = MusicApp::without_profile();
                let mut doc = automerge::AutoCommit::new();
                app.sync(&doc, &automerge::ROOT);
                app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
                app.sync(&doc, &automerge::ROOT);
                let id = file.track.id.clone();
                app.local_file = Some(file.clone());
                app.pb.current = Some(id.clone());
                app.pb.queue = vec![id.clone()];
                app.pb.position_ms = 1000;
                app.playhead_ms = 1000.0;
                let mut invocation = None;
                match route {
                    "row" => app.on_play(
                        std::sync::Arc::new(vec![id.clone()]),
                        0,
                        &mut ui::widget::Edits::lend(&mut doc),
                    ),
                    "previous" => app.on_prev(&mut ui::widget::Edits::lend(&mut doc)),
                    "repeat-one" => {
                        app.pb.repeat = crate::model::RepeatMode::One;
                        app.pb.playing = true;
                        app.auto_advance();
                    }
                    "canonical" => {
                        let resource_id = crate::actions::queue_item_resource_id(0, &id);
                        let resource =
                            crate::app::MusicApp::publish_resource(&app, &resource_id).unwrap();
                        let action = resource
                            .actions
                            .iter()
                            .find(|action| action.id == "play")
                            .unwrap();
                        let encoded = action.build_json(&serde_json::Map::new()).unwrap();
                        let request = tilo_actions::InvocationId::new();
                        assert!(matches!(
                            app.start_effect(request, &encoded.payload).unwrap(),
                            ui::framework::Effected::Pending
                        ));
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
                        app.take_effect_result(invocation),
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
        let mut app = MusicApp::without_profile();
        app.disconnect();
        let mut doc = crate::seed::sample_document();
        app.sync(&doc, &automerge::ROOT);
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
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
        let resource = crate::app::MusicApp::publish_resource(&app, &resource_id).unwrap();
        let action = resource
            .actions
            .iter()
            .find(|action| action.id == "play")
            .unwrap();
        let encoded = action.build_json(&serde_json::Map::new()).unwrap();
        let invocation = tilo_actions::InvocationId::new();
        assert!(matches!(
            app.start_effect(invocation, &encoded.payload).unwrap(),
            ui::framework::Effected::Pending
        ));
        assert_eq!(app.resource_stream_invocation.as_ref().unwrap().2, 0);
        assert!(app.take_effect_result(invocation).is_none());
        let deadline = tilo_test_env::hang_deadline();
        let update = loop {
            app.tick();
            if let Some(update) = app.take_effect_result(invocation) {
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
        let mut app = MusicApp::without_profile();
        app.disconnect();
        let mut doc = crate::seed::sample_document();
        app.sync(&doc, &automerge::ROOT);
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
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
        if paused_selection {
            // A paused selection waits for its source until it plays.
            let pause = crate::actions::effect_payload(&crate::actions::MusicEffect::PlayPause);
            assert!(matches!(
                app.start_effect(tilo_actions::InvocationId::new(), &pause)
                    .unwrap(),
                ui::framework::Effected::Done(_)
            ));
            assert!(!app.pb.playing);
        }
        let payload = crate::actions::effect_payload(&crate::actions::MusicEffect::Play {
            track: id,
            queued: None,
        });
        let invocation = tilo_actions::InvocationId::new();
        assert!(matches!(
            app.start_effect(invocation, &payload).unwrap(),
            ui::framework::Effected::Pending
        ));
        assert_eq!(app.resource_stream_invocation.as_ref().unwrap().2, 0);
        wait_for_audio(&mut app);
        let expected_path = app
            .media_cache
            .object_path(&app.media_cache_root, app.pb.current.as_deref().unwrap())
            .unwrap();
        let expected_uri = media::source::path_to_file_uri(&expected_path).unwrap();
        assert_eq!(app.audio.current_uri(), Some(expected_uri.as_str()));
        assert!(matches!(
            app.take_effect_result(invocation),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
        app.audio.stop();
    }

    #[cfg(feature = "audio")]
    fn check_replaced_paused_source(continuation: bool) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old-local.wav");
        wav(&path);
        let mut app = MusicApp::without_profile();
        app.disconnect();
        let mut doc = crate::seed::sample_document();
        app.sync(&doc, &automerge::ROOT);
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
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
                        schema_version: ui::collaboration::semantic_schema::MEDIA_POSITION_V1,
                        bytes: serde_json::to_vec(&serde_json::json!({
                            "track_id": next_id, "position_ms": 500, "paused": true,
                        }))
                        .unwrap(),
                    },
                    &mut ui::widget::Edits::lend(&mut doc),
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
            let resource = crate::app::MusicApp::publish_resource(&app, &resource_id).unwrap();
            let remove = resource
                .actions
                .iter()
                .find(|action| action.id == "remove")
                .unwrap();
            let encoded = remove.build_json(&serde_json::Map::new()).unwrap();
            assert!(matches!(
                app.start_effect(tilo_actions::InvocationId::new(), &encoded.payload)
                    .unwrap(),
                ui::framework::Effected::Done(_)
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
        let player = crate::app::MusicApp::publish_resource(&app, "music.player:player").unwrap();
        let play = player
            .actions
            .iter()
            .find(|action| action.id == "play-pause")
            .unwrap();
        let encoded = play.build_json(&serde_json::Map::new()).unwrap();
        let invocation = tilo_actions::InvocationId::new();
        assert!(matches!(
            app.start_effect(invocation, &encoded.payload).unwrap(),
            ui::framework::Effected::Pending
        ));
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
            app.take_effect_result(invocation),
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
        // First PCM can arrive before the asynchronous Playing transition.
        // Keep driving production tick until the active decoder confirms it,
        // within the original deadline. The capture sink can also stall when
        // paused while still completing its switch to Playing.
        let rendered_before = capture.snapshot().rendered;
        while app.audio.frames_rendered() == 0
            || app.audio.needs_poll()
            || (app.pb.playing && !app.audio.is_playing_confirmed())
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
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("local song.wav");
            wav(&path);
            let mut app = MusicApp::without_profile();
            let arguments = [path.to_str().unwrap().to_owned()];
            app.open_launch(&ui::framework::LaunchInput::new(&arguments, None));
            assert!(app.playback_error.is_none());
            let mut doc = crate::seed::empty_document();
            app.sync(&doc, &automerge::ROOT);
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            assert!(app.lib.tracks.is_empty());
            assert!(crate::seed::library_of(&doc).tracks.is_empty());
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
                    crate::actions::LOCAL_FILE_RESOURCE_ID
                )
                .is_some()
            );
            let json = serde_json::to_string(&automerge::AutoSerde::from(&doc)).unwrap();
            assert!(!json.contains("local song"));
            app.on_toggle_play(&mut ui::widget::Edits::lend(&mut doc));
            assert!(!app.pb.playing);
            let paused_by = tilo_test_env::hang_deadline();
            while app.audio.is_playing_real() {
                assert!(ui::time::now() < paused_by, "native source did not pause");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let resource = crate::app::MusicApp::publish_resource(
                &app,
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
            assert!(matches!(
                app.start_effect(invocation, &encoded.payload).unwrap(),
                ui::framework::Effected::Pending
            ));
            assert!(app.take_effect_result(invocation).is_none());
            let deadline = tilo_test_env::hang_deadline();
            loop {
                app.tick();
                if let Some(result) = app.take_effect_result(invocation) {
                    let tilo_actions::InvocationUpdate::Succeeded { output, .. } = result else {
                        panic!("resume did not succeed: {result:?}");
                    };
                    assert_eq!(output.value, Some(serde_json::json!({"state":"playing"})));
                    action
                        .contract
                        .result
                        .validate(&output)
                        .expect("real resumed PCM completion must match the published result");
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

        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("effect.wav");
            wav(&path);
            let mut app = MusicApp::without_profile();
            let mut doc = automerge::AutoCommit::new();
            app.sync(&doc, &automerge::ROOT);
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            let before = doc.get_heads();
            let invocation = tilo_actions::InvocationId::new();
            let library = <crate::actions::MusicEffect as ui::tilo_resource::Effect>::specs();
            let action = library
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
            assert!(matches!(
                app.start_effect(invocation, &payload).unwrap(),
                ui::framework::Effected::Pending
            ));
            assert!(app.take_effect_result(invocation).is_none());
            app.sync(&doc, &automerge::ROOT);
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            wait_for_audio(&mut app);
            assert!(matches!(
                app.take_effect_result(invocation),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
            app.sync(&doc, &automerge::ROOT);
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            assert_eq!(doc.get_heads(), before);
            assert!(app.lib.tracks.is_empty());
            let profile = crate::storage::DeviceProfile {
                spotify_catalogue: false,
                catalogue_connected: false,
                local_file: app.local_file.clone(),
                playback: app.pb.clone(),
                queue_catalogue: Vec::new(),
            };
            let profile_path = dir.path().join("profile.json");
            crate::storage::save_device_profile_to(&profile_path, &profile).unwrap();
            let restored = crate::storage::load_device_profile_from(&profile_path);
            assert_eq!(restored.local_file, app.local_file);
            assert!(!restored.playback.playing);
            app.audio.stop();
        }

        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("resume.wav");
            wav(&path);
            let file = crate::local_file::LocalFile::open(path.to_str().unwrap(), None).unwrap();
            let profile = crate::storage::DeviceProfile {
                spotify_catalogue: false,
                playback: crate::model::Playback {
                    current: Some(file.track.id.clone()),
                    queue: vec![file.track.id.clone()],
                    position_ms: 1000,
                    ..Default::default()
                },
                local_file: Some(file),
                queue_catalogue: Vec::new(),
                catalogue_connected: false,
            };
            let profile_path = dir.path().join("profile.json");
            crate::storage::save_device_profile_to(&profile_path, &profile).unwrap();
            for missing in [false, true] {
                if missing {
                    std::fs::remove_file(&path).unwrap();
                }
                let restored = crate::storage::load_device_profile_from(&profile_path);
                let mut app = MusicApp::without_profile();
                app.local_file = restored.local_file;
                app.pb = restored.playback;
                let mut doc = automerge::AutoCommit::new();
                app.sync(&doc, &automerge::ROOT);
                app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
                app.sync(&doc, &automerge::ROOT);
                let source = crate::app::MusicApp::publish_resource(
                    &app,
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
                let started = app.start_effect(invocation, &encoded.payload);
                if missing {
                    assert!(started.is_err());
                    assert!(!app.pb.playing);
                    assert!(!app.audio.has_stream());
                } else {
                    assert!(matches!(started.unwrap(), ui::framework::Effected::Pending));
                    app.persist_playback(&mut ui::widget::Edits::lend(&mut doc));
                    assert_eq!(
                        app.pb.position_ms, 1000,
                        "unknown duration cannot erase the resume point"
                    );
                    wait_for_audio(&mut app);
                    assert!(app.audio.position_ms().unwrap() >= 900);
                    assert!(matches!(
                        app.take_effect_result(invocation),
                        Some(tilo_actions::InvocationUpdate::Succeeded { .. })
                    ));
                    app.audio.stop();
                }
            }
        }

        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("short.wav");
            wav(&path);
            let mut app = MusicApp::without_profile();
            let mut doc = automerge::AutoCommit::new();
            app.sync(&doc, &automerge::ROOT);
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            let request = app.open_local_file(path.to_str().unwrap()).unwrap();
            wait_for_audio(&mut app);
            let invocation = tilo_actions::InvocationId::new();
            // A warm native pipeline can deliver its first buffer before the host
            // registers the invocation. New-source evidence must still count.
            app.begin_resource_stream_invocation(invocation, request);
            app.tick();
            assert!(matches!(
                app.take_effect_result(invocation),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
            app.audio.stop();
        }

        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("corrupt.wav");
            std::fs::write(&path, b"not a WAV stream").unwrap();
            let mut app = MusicApp::without_profile();
            let mut doc = automerge::AutoCommit::new();
            app.sync(&doc, &automerge::ROOT);
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            let invocation = tilo_actions::InvocationId::new();
            let (services, recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);
            recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(path));
            let payload = crate::actions::effect_payload(&crate::actions::MusicEffect::OpenFile);
            assert!(matches!(
                app.start_effect(invocation, &payload).unwrap(),
                ui::framework::Effected::Pending
            ));
            let until = tilo_test_env::hang_deadline();
            loop {
                app.sync(&doc, &automerge::ROOT);
                app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
                app.sync(&doc, &automerge::ROOT);
                app.tick();
                if let Some(update) = app.take_effect_result(invocation) {
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
        let mut app = MusicApp::without_profile();
        let arguments = [path.to_str().unwrap().to_owned()];
        app.open_launch(&ui::framework::LaunchInput::new(&arguments, None));
        assert!(app.playback_error.is_some());
        assert!(app.pb.current.is_none());
    }

    fn picked(name: &str, bytes: Vec<u8>) -> ui::portal::PickedContents {
        ui::portal::PickedContents {
            name: name.into(),
            media_type: None,
            bytes,
            location: None,
        }
    }

    /// Deliver `outcome` as the answer to an Open audio file chooser that
    /// asked for contents, as a browser page's does.
    fn answer_contents(app: &mut MusicApp, outcome: ui::portal::ContentsOutcome) {
        let (sender, receiver) = std::sync::mpsc::channel();
        sender.send(outcome).unwrap();
        let invocation = tilo_actions::InvocationId::new();
        app.local_file_pick = Some((invocation, crate::app::LocalFilePick::Contents(receiver)));
        app.drain_local_file_pick();
    }

    /// A file a page hands over by its contents is checked the same way as
    /// one chosen by path, and a refusal says why rather than nothing.
    #[test]
    fn a_file_chosen_by_contents_that_music_cannot_play_says_why() {
        let mut app = MusicApp::without_profile();
        answer_contents(
            &mut app,
            ui::portal::ContentsOutcome::Picked(vec![picked("notes.txt", b"hello".to_vec())]),
        );
        assert!(app.local_file_pick.is_none());
        assert!(app.local_file.is_none());
        assert_eq!(
            app.toast.get().map(|(text, _)| text),
            Some("This file type is not supported by Music")
        );
        assert_eq!(
            ui::window_toolbar::notice().map(|notice| notice.text),
            Some("This file type is not supported by Music".to_owned()),
            "the window bar says why"
        );
        let failed = app.resource_stream_results.iter().any(|update| {
            matches!(update, tilo_actions::InvocationUpdate::Failed { error, .. }
                if error.code == "music.open_file_failed")
        });
        assert!(failed, "the Open audio file effect settles as failed");
    }

    /// A file playing from a page-local copy of its bytes is not kept to
    /// reopen: the copy is gone with the page.
    #[test]
    fn a_file_playing_from_a_page_copy_is_not_kept() {
        let mut app = MusicApp::without_profile();
        app.local_file = Some(
            crate::local_file::LocalFile::from_page_url("Song.mp3", "blob:page/1".into()).unwrap(),
        );
        assert_eq!(app.local_file.as_ref().unwrap().track.title, "Song");
        assert!(app.kept_local_file().is_none());
        assert!(
            crate::local_file::LocalFile::from_page_url("notes.txt", "blob:page/2".into()).is_err()
        );
    }

    /// A file a page hands over by its contents, with no place on the
    /// device, plays from those contents.
    #[test]
    #[cfg(feature = "audio")]
    fn a_file_chosen_by_contents_plays() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        wav(&path);
        let bytes = std::fs::read(&path).unwrap();
        let mut app = MusicApp::without_profile();
        answer_contents(
            &mut app,
            ui::portal::ContentsOutcome::Picked(vec![picked("Tone.wav", bytes)]),
        );
        let file = app.local_file.as_ref().expect("the chosen file is open");
        assert_eq!(file.track.title, "Tone");
        assert!(app.pb.playing);
        wait_for_audio(&mut app);
    }

    #[test]
    #[cfg(feature = "audio")]
    fn profile_write_failure_does_not_start_the_requested_file() {
        const CHILD: &str = "TILO_TEST_LOCAL_FILE_PROFILE_FAILURE";
        if let Some(path) = std::env::var_os(CHILD) {
            assert_eq!(unsafe { gstreamer::ffi::gst_is_initialized() }, 0);
            let mut app = MusicApp::without_profile();
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

impl MusicApp {
    pub(super) fn make_source_buttons() -> Vec<ui::widget::Button> {
        use crate::actions::MusicEffect;
        use ui::tilo_resource::Effect as _;
        [
            (MusicEffect::Sources, "music-sources"),
            (MusicEffect::SpotifyQueue, "music-spotify-queue-refresh"),
            (
                MusicEffect::NewSpotifyPlaylist,
                "music-spotify-create-playlist",
            ),
        ]
        .into_iter()
        .filter_map(|(effect, key)| {
            let resource = tilo_actions::Resource::new("music.library", "music.library", "Library")
                .with_action(effect.bound());
            tilo_actions::ActionBinding::unit_for(&resource, effect.id())
                .ok()
                .map(|binding| ui::widget::Button::new(key, key).on_action(binding))
        })
        .collect()
    }
}

#[cfg(test)]
mod spotify_tests {
    use super::*;
    use ui::widget::Widget as _;

    #[test]
    fn spotify_search_projection_uses_provider_ids_and_retains_youtube_ids() {
        let hit = crate::backend::TrackHit {
            video_id: "spotify:track:1234567890123456789012".into(),
            title: "Song".into(),
            artist: "Artist".into(),
            artist_id: Some("spotify:artist:abcdefghijklmnopqrstuv".into()),
            album: Some("Album".into()),
            album_id: Some("spotify:album:ABCDEFGHIJKLMNOPQRSTUV".into()),
            duration_s: 120,
            cover_url: String::new(),
        };
        let mut app = MusicApp::without_profile();
        app.disconnect();
        app.ingest_search_results(
            "Song",
            crate::backend::SearchBundle {
                tracks: vec![hit.clone()],
                ..Default::default()
            },
        );
        assert_eq!(
            app.live_results.as_ref().unwrap().1.as_slice(),
            std::slice::from_ref(&hit.video_id)
        );
        assert!(app.lib.track(&hit.video_id).is_some());
        assert_eq!(
            super::super::live_track_id("existing-video"),
            "yt:existing-video"
        );
        app.ingest_search_results(
            "Song",
            crate::backend::SearchBundle {
                tracks: vec![hit.clone()],
                ..Default::default()
            },
        );
        assert_eq!(app.live_results.as_ref().unwrap().1, [hit.video_id]);
    }

    #[test]
    fn home_has_one_source_picker_with_semantic_actions() {
        use crate::actions::MusicEffect;
        use ui::tilo_resource::Effect as _;
        for effect in [
            MusicEffect::ConnectSpotifyLibrary,
            MusicEffect::ConnectSpotify,
            MusicEffect::BrowseSpotify,
            MusicEffect::BrowseYouTube,
            MusicEffect::DisconnectSpotify,
        ] {
            assert_eq!(effect.bound().kind, tilo_actions::OperationKind::Effect);
        }
        for (width, base_theme) in [360.0, 1000.0].into_iter().flat_map(|width| {
            [
                ui::theme::Theme::dark(),
                ui::theme::Theme::light(),
                ui::theme::Theme::high_contrast(),
            ]
            .map(|theme| (width, theme))
        }) {
            for scale in [1.0, 2.0] {
                let mut app = MusicApp::without_profile();
                app.disconnect();
                assert!(app.device_row("music.source:catalogue").is_some());
                assert!(
                    app.device_rows()
                        .iter()
                        .any(|row| row.id() == "music.source:catalogue")
                );
                app.rect = vello::kurbo::Rect::new(0.0, 0.0, width, 1000.0);
                let theme = base_theme.clone().with_text_scale(scale);
                let mut tree = taffy::TaffyTree::new();
                app.build_layout(&mut tree, &theme, &mut std::collections::HashMap::new());
                let mut text = ui::text_render::TextRenderer::new();
                text.set_text_scale(scale);
                app.render_frame(
                    &mut vello::Scene::new(),
                    vello::kurbo::Affine::IDENTITY,
                    &mut text,
                    &theme,
                );
                let visible: Vec<_> = app
                    .children()
                    .into_iter()
                    .filter(|button| button.test_id() == Some("music-sources"))
                    .collect();
                assert_eq!(visible.len(), 1, "one compact source picker on Home");
                let rect = visible[0].get_rect();
                assert!(rect.width() > 0.0 && rect.height() > 0.0);
                assert!(rect.x0 >= 0.0 && rect.x1 <= width);
                assert!(
                    app.children().into_iter().all(|button| !matches!(
                        button.test_id(),
                        Some(
                            "music-connect-spotify"
                                | "music-spotify-playback"
                                | "music-disconnect-spotify"
                                | "music-spotify-queue-refresh"
                        )
                    )),
                    "Home does not expose backend operations as rows of buttons"
                );
            }
        }
    }
}
