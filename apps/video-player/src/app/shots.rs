//! The source and view a review capture stages before it draws.

use super::*;

impl PlayerApp {
    /// Stage a capture of `scenario` (`None` for a plain `--shots`): the
    /// video the launch named, else a synthesized demo clip, the scenario's
    /// view state, and what the capture waits for before it is taken.
    pub(crate) fn stage_capture(&mut self, launched: Option<&str>, scenario: Option<&str>) {
        self.stage_shot_source(launched, scenario);
        self.stage_shot_scenario(scenario);
        if !matches!(scenario, Some("empty" | "error" | "missing-codec")) {
            self.player.reveal_controls();
        }
    }

    /// A player staged as a capture of `scenario` over the demo clip.
    #[cfg(test)]
    pub(crate) fn for_shot_open(launch: Option<&str>, scenario: Option<&str>) -> Self {
        let mut app = Self::new(RunMode::Capture);
        app.stage_capture(launch, scenario);
        app
    }

    #[cfg(test)]
    pub(crate) fn for_shot(scenario: Option<&str>) -> Self {
        Self::for_shot_open(None, scenario)
    }

    pub(super) fn stage_shot_source(&mut self, launched: Option<&str>, scenario: Option<&str>) {
        if matches!(scenario, Some("empty" | "error" | "missing-codec")) {
            return;
        }
        // A capture shows a real video decoded, the launch's or the demo
        // clip, never a synthesized stand-in: without the system decoder it
        // waits for a picture that cannot come and refuses to capture.
        let engine = media::MediaEngine::new();
        if !engine.is_real_decode() {
            self.open_error =
                Some("A real video capture requires the GStreamer decoder and runtime".into());
            self.capture_awaits = CaptureAwaits::Frame;
            return;
        }
        self.player.set_engine(engine);
        if let Some(argument) = launched {
            let source = source_for_argument(argument);
            let title = source_title(&source);
            let queue_id = self.ensure_queued(source.clone(), title.clone());
            let _ = self.open_local_source(source, title, Some(queue_id));
            // Held still for a stable picture, as the demo clip is; the
            // `playing` scenario starts it.
            self.player.pause();
            self.capture_awaits = match scenario {
                Some("native-error") => CaptureAwaits::Outcome,
                Some("loading") => CaptureAwaits::Nothing,
                _ => CaptureAwaits::Frame,
            };
            return;
        }
        let source = demo_source();
        let queue_id = self.ensure_queued(source.clone(), DEMO_TITLE.into());
        let _ = self.set_current_source(&source);
        self.player.open(source);
        self.title = DEMO_TITLE.to_string();
        self.current_queue_id = Some(queue_id);
        // Park at a fixed position for a stable reference frame. The
        // explicit `playing` fixture starts the engine below; default and
        // `paused` deliberately stay deterministic.
        self.player.seek_to_ms(DEMO_POSITION_MS);
        self.player.pause();
        if scenario != Some("loading") {
            self.capture_awaits = CaptureAwaits::Frame;
        }
    }

    /// Whether a capture still waits for what it shows.
    pub(super) fn capture_pending(&self) -> bool {
        let pictured = self.player.latest_frame().is_some();
        match self.capture_awaits {
            CaptureAwaits::Nothing => false,
            CaptureAwaits::Frame => !pictured,
            CaptureAwaits::Outcome => !pictured && self.open_error.is_none(),
        }
    }

    pub(super) fn stage_shot_scenario(&mut self, scenario: Option<&str>) {
        match scenario {
            // Nothing opened: the open itself failed.
            Some("error") => self.fail_playback(),
            Some("missing-codec") => {
                self.fail_playback();
                self.missing_codec = Some("HEVC".into());
            }
            // A video opened and the decoder then gave up on it: the title
            // stays in the window's bar, the picture is the failure.
            Some("native-error") => {
                self.shot_loading = true;
                self.fail_playback();
            }
            Some("loading") => self.shot_loading = true,
            Some("playing") => self.player.play(),
            Some("paused") => self.player.pause(),
            Some("settings") => self.player.open_settings_menu(None),
            Some("metadata") => {
                self.shot_metadata = Some(DecoderMetadata {
                    audio_tracks: vec![
                        crate::actions::TrackState {
                            stream_id: "fixture-audio-en".into(),
                            index: 0,
                            label: "English stereo".into(),
                            selected: true,
                        },
                        crate::actions::TrackState {
                            stream_id: "fixture-audio-de".into(),
                            index: 1,
                            label: "Deutsch 5.1".into(),
                            selected: false,
                        },
                    ],
                    subtitle_tracks: vec![crate::actions::TrackState {
                        stream_id: "fixture-subtitles-en".into(),
                        index: 0,
                        label: "English captions".into(),
                        selected: true,
                    }],
                    chapters: vec![
                        crate::actions::ChapterState {
                            id: "intro".into(),
                            label: "Introduction".into(),
                            seconds: 0.0,
                        },
                        crate::actions::ChapterState {
                            id: "scene".into(),
                            label: "Second act".into(),
                            seconds: 45.0,
                        },
                    ],
                });
                self.sync_decoder_settings();
                self.player.open_settings_menu(None);
            }
            Some("library" | "recent" | "library-focus") => {
                for (url, title) in DEMO_QUEUE {
                    self.ensure_queued(
                        media::Source::Remote { url: (*url).into() },
                        (*title).into(),
                    );
                }
                // The clip that is playing leads, then what came before it.
                self.recent = self
                    .queue
                    .iter()
                    .take(1)
                    .chain(self.queue.iter().skip(1).rev())
                    .cloned()
                    .chain(std::iter::once(LocalMediaEntry {
                        id: "recent-mountain-pass".into(),
                        title: "Mountain pass".into(),
                        source: media::Source::Remote {
                            url: "https://demo.local/mountain.mp4".into(),
                        },
                    }))
                    .collect();
                self.library_visible = true;
                self.library_tab = match scenario {
                    Some("recent") => LibraryTab::Recent,
                    Some("shared") => LibraryTab::Shared,
                    _ => LibraryTab::Queue,
                };
                if scenario == Some("library-focus") {
                    self.library_focus_id = self.queue.get(1).map(|entry| entry.id.clone());
                }
            }
            Some("shared") => {
                self.library_visible = true;
                self.library_tab = LibraryTab::Shared;
            }
            Some("fullscreen") => self.player.toggle_fullscreen(),
            _ => {}
        }
    }
}
