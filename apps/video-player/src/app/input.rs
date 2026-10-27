//! What the pointer, a shortcut and the volume controls do, and the preferences they persist.

use super::*;

impl PlayerApp {
    /// Show and keep the preferences just changed.
    pub(super) fn preferences_changed(&mut self) {
        configure_player_settings(&mut self.player, self.preferences);
        self.persist_preferences();
    }

    /// Keep the playback preferences with this device's settings. A
    /// device-local preference must never prevent playback, so a refused write
    /// is logged and the next change retries it.
    pub(super) fn persist_preferences(&self) {
        let Some(setting) = &self.setting else {
            return;
        };
        let preferences = self.preferences;
        if let Err(error) = setting.update(|stored| {
            stored.autoplay = preferences.autoplay;
            stored.loop_enabled = preferences.loop_enabled;
            stored.volume = preferences.volume;
            stored.rate = preferences.rate;
        }) {
            tracing::warn!(%error, "video player: could not keep playback preferences");
        }
    }

    /// A volume change is kept once it settles, so a drag writes one value
    /// instead of one per step.
    pub(super) fn volume_changed(&mut self, volume: f64) {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a fraction in 0.0..=1.0; f32 precision is sufficient"
        )]
        let volume = volume.clamp(0.0, 1.0) as f32;
        self.player.set_volume(volume);
        // `set_volume` clamps and unmutes, so the player answers the level
        // that actually plays.
        if (self.preferences.volume - self.player.volume()).abs() > f32::EPSILON {
            self.preferences.volume = self.player.volume();
            self.volume_settling_since = Some(unix_now_ms());
        }
    }

    /// Keep the volume once the person has stopped changing it. The last
    /// value always lands, because a change that keeps arriving keeps the
    /// timer running until it stops.
    pub(super) fn settle_volume(&mut self) {
        let Some(since) = self.volume_settling_since else {
            return;
        };
        if unix_now_ms().saturating_sub(since) < VOLUME_SETTLE_MS {
            return;
        }
        self.volume_settling_since = None;
        self.persist_preferences();
    }

    /// Drain a pending picker answer, opening the chosen file.
    pub(super) fn drain_portal(&mut self) {
        let pending = self.portal_open.take();
        let mut outcome: Option<PortalOpenOutcome> = None;
        if let Some(request) = pending.as_ref() {
            let received = match &request.channel {
                PickerChannel::Path(channel) => channel.try_recv().map(|answer| match answer {
                    PortalOutcome::Opened(p) => Ok(Picked::Path(p)),
                    PortalOutcome::Cancelled => Err(None),
                    PortalOutcome::Failed(error) => Err(Some(error)),
                }),
                PickerChannel::Contents(channel) => channel.try_recv().map(|answer| match answer {
                    ContentsOutcome::Picked(files) => files
                        .into_iter()
                        .next()
                        .map(Picked::Contents)
                        .ok_or_else(|| Some("the picker returned no file".into())),
                    ContentsOutcome::Cancelled => Err(None),
                    ContentsOutcome::Failed(error) => Err(Some(error)),
                }),
            };
            match received {
                Ok(result) => outcome = Some((request.generation, result)),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    outcome = Some((
                        request.generation,
                        Err(Some("video picker disconnected".into())),
                    ));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if outcome.is_none() {
            self.portal_open = pending;
            return;
        }
        let reply = pending.and_then(|pending| pending.reply);
        // The receiver is dropped before opening the chosen file, so the
        // open cannot cancel the request it completes.
        let Some((generation, outcome)) = outcome else {
            return;
        };
        if generation != self.portal_generation {
            tracing::warn!(
                generation,
                current_generation = self.portal_generation,
                "video player: ignored a stale portal outcome"
            );
            if let Some(reply) = reply {
                reply.finish(Err(
                    "ignored a stale video picker response; open again".into()
                ));
            }
            return;
        }
        let result = match outcome {
            Ok(Picked::Path(path)) => self.open_path(&path).map(|()| true),
            Ok(Picked::Contents(file)) => self.open_contents(&file).map(|()| true),
            Err(None) => Ok(false),
            Err(Some(e)) => {
                self.open_error = Some(e.clone());
                self.dirty = true;
                Err(e)
            }
        };
        let Some(reply) = reply else {
            return;
        };
        match result {
            // Picking is not decoding: answer once a picture arrives.
            Ok(true) => self.await_open_frame(reply),
            Ok(false) => reply.finish(Err("video selection was cancelled".into())),
            Err(message) => reply.finish(Err(message)),
        }
    }

    /// Watch together: follow where playback of the open video stands for
    /// everyone, and say where it stands here when this player moved it
    /// (played, paused, sought, changed speed). Only a live window with the
    /// right to edit says so; a viewer follows.
    pub(super) fn tick_together(&mut self) {
        if !self.mode.is_live() {
            return;
        }
        let (Some(media_ref), Some(dispatcher)) =
            (self.current_media_ref.clone(), self.dispatcher.clone())
        else {
            return;
        };
        let now = unix_now_ms();
        let shared = self.shared_library.transport_for(&media_ref).cloned();
        let Some(engine) = self.player.engine_mut() else {
            return;
        };
        let local = engine.transport_state(now);
        let step = self.together.step(
            &media_ref,
            shared.as_ref(),
            local,
            now,
            engine.duration_ms().unwrap_or(0),
        );
        match step {
            TogetherStep::Follow(state) => {
                engine.apply_transport(&state, now);
                self.dirty = true;
            }
            TogetherStep::Publish(state) if !dispatcher.read_only() => {
                dispatcher.submit(PlayerResource::SET_TRANSPORT.with(verbs::SetTransport {
                    media_ref,
                    playing: state.playing,
                    position_ms: state.position_ms,
                    rate: f64::from(state.rate),
                    epoch_ms: state.epoch_ms,
                }));
            }
            TogetherStep::Publish(_) | TogetherStep::Stay => {}
        }
    }

    /// Do what the player's own settings menu chose.
    pub(super) fn drain_player_settings(&mut self) {
        for event in self.player.take_setting_events() {
            let source_id = self.current_source_id.clone().unwrap_or_default();
            let effect = match event {
                PlayerSettingEvent::Toggled { id, on } if id == "autoplay" => {
                    Some(VideoEffect::SetAutoplay { enabled: on })
                }
                PlayerSettingEvent::Toggled { id, on } if id == "loop" => {
                    Some(VideoEffect::SetLoop { enabled: on })
                }
                PlayerSettingEvent::Chose { id, index } if id == "playback-rate" => PLAYBACK_RATES
                    .get(index)
                    .map(|rate| VideoEffect::SetPlaybackRate {
                        rate: f64::from(*rate),
                    }),
                PlayerSettingEvent::Chose { id, index } if id == "audio-track" => {
                    self.shown_metadata.audio_tracks.get(index).map(|track| {
                        VideoEffect::SelectAudioTrack {
                            track: crate::actions::audio_resource_id(&source_id, &track.stream_id),
                        }
                    })
                }
                PlayerSettingEvent::Chose { id, index } if id == "subtitle-track" => {
                    Some(match index.checked_sub(1) {
                        None => VideoEffect::SubtitlesOff,
                        Some(index) => match self.shown_metadata.subtitle_tracks.get(index) {
                            Some(track) => VideoEffect::SelectSubtitles {
                                track: crate::actions::subtitle_resource_id(
                                    &source_id,
                                    &track.stream_id,
                                ),
                            },
                            None => continue,
                        },
                    })
                }
                PlayerSettingEvent::Chose { id, index } if id == "chapters" => {
                    self.shown_metadata.chapters.get(index).map(|chapter| {
                        VideoEffect::JumpToChapter {
                            chapter: crate::actions::chapter_resource_id(&source_id, &chapter.id),
                        }
                    })
                }
                PlayerSettingEvent::Activated { id } if id == "picture-in-picture" => {
                    Some(VideoEffect::PictureInPicture)
                }
                PlayerSettingEvent::Toggled { .. }
                | PlayerSettingEvent::Chose { .. }
                | PlayerSettingEvent::Activated { .. } => None,
            };
            if let Some(effect) = effect
                && let Err(error) = self.perform(effect)
            {
                tracing::warn!(%error, "video player: a settings choice was refused");
            }
        }
    }

    pub(super) fn handle_app_pointer(&mut self, _state: &mut ui::widget::Edits<'_>) -> bool {
        self.resolve_library();
        let in_list = self.library_list_rect.contains(self.last_cursor);
        if self.library_visible && self.drawer.sidebar_contains(self.last_cursor) {
            if self.library_back_rect.contains(self.last_cursor) {
                self.request_video_show();
            } else if self.library_queue_tab_rect.contains(self.last_cursor) {
                self.request_library_tab(LibraryTab::Queue);
            } else if self.library_recent_tab_rect.contains(self.last_cursor) {
                self.request_library_tab(LibraryTab::Recent);
            } else if self.library_shared_tab_rect.contains(self.last_cursor) {
                self.request_library_tab(LibraryTab::Shared);
            } else if self.library_tab == LibraryTab::Shared
                && self.shared_watch_later_rect.contains(self.last_cursor)
            {
                self.add_current_to_watch_later();
            } else if self.library_tab == LibraryTab::Shared
                && self.shared_bookmark_rect.contains(self.last_cursor)
            {
                self.bookmark_current_time();
            } else if self.library_tab == LibraryTab::Shared
                && in_list
                && let Some(row) = self
                    .shared_row_hits
                    .iter()
                    .find(|row| row.jump.is_some_and(|jump| jump.contains(self.last_cursor)))
                    .cloned()
            {
                self.request_annotation_jump(&row.id);
            } else if self.library_tab == LibraryTab::Shared
                && in_list
                && let Some(row) = self
                    .shared_row_hits
                    .iter()
                    .find(|row| row.remove.contains(self.last_cursor))
                    .cloned()
            {
                if row.annotation {
                    self.delete_annotation(&row.id);
                } else {
                    self.delete_playlist(&row.id);
                }
            } else if self.library_tab == LibraryTab::Recent
                && self.library_clear_rect.contains(self.last_cursor)
            {
                self.request_library_clear();
            } else if in_list
                && let Some(row) = self
                    .library_row_hits
                    .iter()
                    .find(|row| row.row.contains(self.last_cursor))
                    .cloned()
            {
                if row
                    .remove
                    .is_some_and(|rect| rect.contains(self.last_cursor))
                {
                    self.request_library_remove(&row.id);
                } else {
                    self.request_library_play(&row.id);
                }
            }
            self.dirty = true;
            return true;
        }
        if self.status_action_rect.contains(self.last_cursor) {
            self.request_status_action();
            return true;
        }
        false
    }

    /// Handle the keyboard values this app owns. Factoring this from winit's
    /// event carrier keeps shortcut tests fully safe and avoids fabricating
    /// invalid platform event structs just to exercise a key binding.
    pub(super) fn handle_app_shortcut_input(
        &self,
        logical_key: &winit::keyboard::Key,
        state: ElementState,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        if state != ElementState::Pressed {
            return false;
        }
        // The library is an app-owned side panel rather than a modal supplied
        // by the shared player surface. Give it the conventional close key so
        // keyboard and switch-control users never have to hunt for the small
        // header button. Do not consume Escape while the panel is absent: the
        // shell and the embedded player may have a more relevant use for it.
        if matches!(
            logical_key,
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
        ) && self.library_visible
        {
            self.request_library_hide();
            return true;
        }
        let winit::keyboard::Key::Character(key) = logical_key else {
            return false;
        };
        if modifiers.state().control_key() && key.eq_ignore_ascii_case("o") {
            self.request_open_action();
        } else if key.eq_ignore_ascii_case("l") {
            self.request_library_visibility_change();
        } else if key.eq_ignore_ascii_case("p") && self.player.has_media() {
            self.request(&VideoEffect::PictureInPicture);
        } else {
            return false;
        }
        true
    }

    /// Route a logical key through the app-owned shortcuts, then the focused
    /// reusable player surface. This mirrors the live event order while
    /// remaining safe to exercise without fabricating a platform `KeyEvent`.
    pub(super) fn route_key_input(
        &mut self,
        logical_key: &winit::keyboard::Key,
        state: ElementState,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        self.handle_app_shortcut_input(logical_key, state, modifiers)
            || self.player.handle_key_input(logical_key, state)
    }
}
