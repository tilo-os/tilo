//! The effects the host asks Video Player for, and what each answers with.

use super::*;

impl PlayerApp {
    /// What differs from the effects' declarations right now: playback
    /// controls need an open video, and clearing needs a recent one.
    pub(crate) fn effect_statuses(&self) -> Vec<EffectStatus> {
        let mut statuses = Vec::new();
        if !self.formats_visible() {
            statuses.push(EffectStatus::unavailable(
                VideoEffect::OPEN_FORMATS,
                "no_missing_decoder",
                crate::i18n::t("video-formats-unavailable"),
            ));
        }
        if !self.player.has_media() {
            for effect in [
                VideoEffect::CLOSE.id,
                VideoEffect::TOGGLE_PLAYBACK.id,
                VideoEffect::TOGGLE_MUTE.id,
                VideoEffect::REVEAL.id,
                VideoEffect::SEEK.id,
                VideoEffect::SEEK_BY.id,
                VideoEffect::RESTART.id,
                VideoEffect::JUMP_TO_END.id,
                VideoEffect::SET_VOLUME.id,
                VideoEffect::SET_PLAYBACK_RATE.id,
                VideoEffect::SUBTITLES_OFF.id,
                VideoEffect::PICTURE_IN_PICTURE.id,
            ] {
                statuses.push(EffectStatus::unavailable(
                    ui::tilo_resource::ActionRef::<VideoEffect, ()>::new(effect, &[]),
                    "no_media",
                    "Open a video first",
                ));
            }
        }
        if self.recent.is_empty() {
            statuses.push(EffectStatus::unavailable(
                VideoEffect::CLEAR_RECENT,
                "no_recent",
                "No video has been played on this device",
            ));
        }
        statuses
    }

    /// Do `effect` now; refused when its target changed since it was
    /// offered.
    #[allow(
        clippy::too_many_lines,
        reason = "all effects stay together so their local semantics cannot drift"
    )]
    pub(super) fn perform(&mut self, effect: VideoEffect) -> Result<(), String> {
        match effect {
            VideoEffect::OpenFormats => {
                if !self.formats_visible() {
                    return Err(crate::i18n::t("video-formats-unavailable"));
                }
                ui::portal::open_uri_detached("settings://updates");
            }
            VideoEffect::OpenVideo { path: Some(path) } => self.open_named(&path)?,
            VideoEffect::OpenVideo { path: None } => self.trigger_open(None)?,
            VideoEffect::PlayQueued { item } => self.open_local_entry(&item)?,
            VideoEffect::RemoveQueued { item } => self.remove_queue_entry(&item)?,
            VideoEffect::ClearRecent => self.clear_recent_history()?,
            VideoEffect::ShowLibrary { tab } => {
                self.library_tab = LibraryTab::named(&tab)
                    .ok_or_else(|| format!("no library tab `{tab}`: queue, recent or shared"))?;
                self.library_visible = true;
                self.player.reveal_controls();
            }
            VideoEffect::HideLibrary => {
                self.library_visible = false;
                self.library_focus_id = None;
            }
            VideoEffect::Close => self.close_media(),
            VideoEffect::TogglePlayback => self.player.toggle_play(),
            VideoEffect::ToggleMute => self.player.toggle_mute(),
            VideoEffect::Reveal => self.player.reveal_controls(),
            VideoEffect::Seek { seconds } => {
                self.player.seek_to_ms((seconds.max(0.0) * 1000.0) as u64);
            }
            VideoEffect::SeekBy { seconds } => {
                let current = self.player.position_ms() as f64;
                let duration = self.player.duration_ms() as f64;
                let target =
                    (current + seconds.clamp(-3600.0, 3600.0) * 1000.0).clamp(0.0, duration) as u64;
                self.player.seek_to_ms(target);
            }
            VideoEffect::Restart => self.player.seek_to_ms(0),
            VideoEffect::JumpToEnd => self.player.seek_to_ms(self.player.duration_ms()),
            VideoEffect::SetVolume { volume } => self.volume_changed(volume),
            VideoEffect::SetAutoplay { enabled } => {
                if self.preferences.autoplay != enabled {
                    self.preferences.autoplay = enabled;
                    self.preferences_changed();
                }
            }
            VideoEffect::SetLoop { enabled } => {
                if self.preferences.loop_enabled != enabled {
                    self.preferences.loop_enabled = enabled;
                    self.preferences_changed();
                }
            }
            VideoEffect::SetPlaybackRate { rate } => {
                let rate = rate.clamp(0.25, 4.0);
                if let Some(engine) = self.player.engine_mut() {
                    engine.set_rate(rate as f32);
                }
                let selected = PLAYBACK_RATES
                    .iter()
                    .position(|candidate| (f64::from(*candidate) - rate).abs() < f64::EPSILON);
                if let Some(selected) = selected {
                    self.player.set_choice_selection("playback-rate", selected);
                }
                // The speed this device plays at is remembered like its
                // volume: 1.5× opened the same lecture quickly next time.
                self.preferences.rate = self
                    .player
                    .engine()
                    .map_or(rate as f32, media::MediaEngine::rate);
                self.persist_preferences();
            }
            VideoEffect::SelectAudioTrack { track } => {
                let index = self
                    .track_named(&track, true)
                    .ok_or("the audio track changed; refresh and try again")?
                    .index;
                if !self
                    .player
                    .engine_mut()
                    .is_some_and(|engine| engine.select_audio_track(index))
                {
                    return Err("the decoder refused that audio track".into());
                }
            }
            VideoEffect::SelectSubtitles { track } => {
                let index = self
                    .track_named(&track, false)
                    .ok_or("the subtitle track changed; refresh and try again")?
                    .index;
                if !self
                    .player
                    .engine_mut()
                    .is_some_and(|engine| engine.select_subtitle_track(Some(index)))
                {
                    return Err("the decoder refused that subtitle track".into());
                }
            }
            VideoEffect::SubtitlesOff => {
                if !self
                    .player
                    .engine_mut()
                    .is_some_and(|engine| engine.select_subtitle_track(None))
                {
                    return Err("the decoder refused to turn subtitles off".into());
                }
            }
            VideoEffect::JumpToChapter { chapter } => {
                let source_id = self.current_source_id.clone().unwrap_or_default();
                let seconds = self
                    .shown_metadata
                    .chapters
                    .iter()
                    .find(|candidate| {
                        crate::actions::chapter_resource_id(&source_id, &candidate.id) == chapter
                    })
                    .ok_or("the chapter changed; refresh and try again")?
                    .seconds;
                self.player.seek_to_ms((seconds.max(0.0) * 1000.0) as u64);
            }
            VideoEffect::JumpToAnnotation { annotation } => {
                let time_ms = self
                    .shared_library
                    .annotations
                    .get(&Id::from_raw(annotation))
                    .ok_or("the review note is gone")?;
                if self.current_media_ref.as_deref() != Some(time_ms.media_ref.as_str()) {
                    return Err("this review note belongs to a different video".into());
                }
                let time_ms = time_ms.time_ms;
                self.player.seek_to_ms(time_ms);
                self.player.reveal_controls();
            }
            VideoEffect::PictureInPicture => self.request_picture_in_picture(),
            VideoEffect::ToggleFullscreen => self.player.toggle_fullscreen(),
        }
        self.dirty = true;
        Ok(())
    }
}
