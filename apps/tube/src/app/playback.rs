//! The watch page's player: the shared `VideoPlayer` and the media engine beneath it.

use ui::widget::VideoPlayer;
use ui::widget::Widget as _;

use crate::api::Request;
use crate::history::WatchHistory;
use crate::i18n::{arg, t, t_args};
use crate::model::Route;
use crate::screens;

use super::{PLAYBACK_SPEEDS, QUALITIES, Tube};

/// How far the playhead moves between two writes of the watch history: a two-hour video is a few
/// hundred small edits, not one per frame, and still resumes at the minute the person stopped at.
const PROGRESS_STEP_MS: u64 = 5_000;

pub(super) fn pump_player(player: &mut VideoPlayer) {
    player.sync(&(), &());
}

impl Tube {
    pub(super) fn ensure_playback(&mut self) {
        if !self.online {
            return;
        }
        match self.model.route.clone() {
            Route::Watch(id) => {
                if self.playing_id.as_deref() == Some(id.as_str()) {
                    return;
                }
                self.playing_id = Some(id.clone());
                self.player_video = None;
                self.player.borrow_mut().pause();
                if self
                    .model
                    .video(&id)
                    .is_some_and(crate::model::Video::plays_only_in_app)
                {
                    return;
                }
                media::MediaEngine::prepare_in_background();
                let quality = self.model.settings.preferred_quality.clone();
                self.request_live(Request::PlayVideo { id, quality });
            }
            _ => {
                if let Some(left) = self.playing_id.take() {
                    self.remember_progress_of(&left, true);
                    self.player_video = None;
                    self.resume_at_ms = None;
                    self.player.borrow_mut().pause();
                }
            }
        }
    }

    pub(super) fn tick_playback(&mut self) -> bool {
        let Route::Watch(id) = self.model.route.clone() else {
            return false;
        };
        if self.player_video.as_deref() != Some(id.as_str()) {
            return false;
        }
        let events = self.player.borrow_mut().take_media_events();
        if self.player_stopped(&events) {
            return true;
        }
        if !self.player.borrow().has_media() {
            return false;
        }
        let (pos_ms, dur_ms, playing) = {
            let p = self.player.borrow();
            (p.position_ms(), p.duration_ms(), p.is_playing())
        };
        self.model.playhead = pos_ms as f64 / 1000.0;
        self.model.playing = playing;
        self.remember_progress(false);

        let mut changed = self.apply_auto_skip();
        let show_end = self.current_show_skip_end();
        if show_end != self.show_skip_end {
            self.show_skip_end = show_end;
            changed = true;
        }
        let known_dur = dur_ms > 0;
        let at_end = known_dur && pos_ms + 250 >= dur_ms;
        if at_end {
            self.remember_progress_of(&id, true);
        }
        if at_end && self.maybe_autoplay_next() {
            changed = true;
        }
        changed
    }

    pub(super) fn player_stopped(&mut self, events: &[media::MediaEvent]) -> bool {
        if !events
            .iter()
            .any(|event| matches!(event, media::MediaEvent::LimitReached))
        {
            return false;
        }
        self.model.playing = false;
        self.model.push_notice(t("tube-notice-playback-limit"));
        true
    }

    pub(super) fn apply_player_setting_event(
        &mut self,
        ev: ui::widget::video_player::PlayerSettingEvent,
    ) {
        use ui::widget::video_player::PlayerSettingEvent as E;
        match ev {
            E::Chose { id, index } if id == "resolution" => {
                let Some(quality) = QUALITIES.get(index) else {
                    return;
                };
                if self.model.settings.preferred_quality == *quality {
                    return;
                }
                if self.change_preferences(|settings| {
                    settings.preferred_quality = (*quality).to_string();
                }) {
                    self.reload_stream_at_current_position();
                }
            }
            E::Chose { id, index } if id == screens::SPEED_SETTING => {
                let Some(speed) = PLAYBACK_SPEEDS.get(index).copied() else {
                    return;
                };
                self.change_preferences(|settings| settings.playback_speed = speed);
                if let Some(engine) = self.player.borrow_mut().engine_mut() {
                    engine.set_rate(speed);
                }
            }
            E::Chose { id, index } if id == screens::CAPTIONS_SETTING => {
                let language = match index {
                    0 => None,
                    index => self
                        .model
                        .caption_tracks
                        .get(index.saturating_sub(1))
                        .map(|track| track.language.clone()),
                };
                if self.change_preferences(|settings| {
                    settings.caption_language.clone_from(&language);
                }) {
                    self.model.captions.clear();
                    self.model.captions_notice = None;
                    self.load_chosen_captions();
                }
            }
            E::Activated { id } if id == screens::SKIP_SETTING => {
                if let Some(end) = self.current_show_skip_end() {
                    self.skip_to(end);
                    self.structure_dirty = true;
                }
            }
            _ => {}
        }
    }

    fn reload_stream_at_current_position(&mut self) {
        if !self.online {
            return;
        }
        let position = {
            let player = self.player.borrow();
            if player.has_media() {
                player.position_ms()
            } else {
                (self.model.playhead * 1000.0).max(0.0) as u64
            }
        };
        self.resume_at_ms = Some(position);
        self.playing_id = None;
        self.player_video = None;
        self.player.borrow_mut().pause();
        self.ensure_playback();
        self.structure_dirty = true;
    }

    pub(super) fn load_resolved_stream(
        &mut self,
        video: String,
        url: String,
        has_audio: bool,
        quality: &str,
    ) -> bool {
        if !matches!(&self.model.route, Route::Watch(id) if *id == video) {
            return false;
        }
        let now_playing = self.model.video(&video).map(|v| {
            let dearrow = self.model.settings.dearrow_titles_on();
            ui::widget::video_player::NowPlaying {
                track_id: v.id.clone(),
                title: v.display_title(dearrow).to_string(),
                artist: v.channel_name.clone(),
                art_url: v.thumbnail_url.clone(),
            }
        });
        let duration_ms =
            u64::from(self.model.video(&video).map_or(0, |video| video.duration)) * 1000;
        let resume = self.resume_at_ms.take().map(|resume| {
            if duration_ms > 0 {
                resume.min(duration_ms)
            } else {
                resume
            }
        });
        let speed = self.model.settings.playback_speed;
        {
            let mut p = self.playing_player();
            p.open(media::Source::Remote { url });
            if let Some(resume) = resume {
                p.seek_to_ms(resume);
            }
            if let Some(engine) = p.engine_mut() {
                engine.set_rate(speed);
            }
            p.play();
            if let Some(np) = now_playing {
                p.set_now_playing(np);
            }
        }
        if let Some(resume) = resume {
            self.model.playhead = resume as f64 / 1000.0;
        }
        self.model.stream_quality = quality.to_string();
        if self.online && !self.mpris_started {
            self.player
                .borrow_mut()
                .enable_mpris("tilo-tube", "Tilo Tube");
            self.mpris_started = true;
        }
        self.player_video = Some(video);
        self.model.playing = true;
        tracing::debug!(%quality, has_audio, "stream loaded into player");
        true
    }

    pub(super) fn playing_player(&self) -> std::cell::RefMut<'_, VideoPlayer> {
        let mut player = self.player.borrow_mut();
        player.ensure_engine(media::MediaEngine::new);
        player
    }

    pub(super) fn current_duration(&self) -> Option<f64> {
        match &self.model.route {
            Route::Watch(id) => self.model.video(id).map(|v| f64::from(v.duration)),
            _ => None,
        }
    }

    pub(super) fn current_show_skip_end(&self) -> Option<f64> {
        self.model.show_skip_end()
    }

    pub(super) fn seek_to(&mut self, at: f64) {
        self.model.playhead = at;
        self.player
            .borrow_mut()
            .seek_to_ms((self.model.playhead * 1000.0) as u64);
        self.apply_auto_skip();
        self.maybe_autoplay_next();
    }

    pub(super) fn skip_to(&mut self, to: f64) {
        if let Some(d) = self.current_duration() {
            let to = to.clamp(0.0, d);
            self.model.playhead = to;
            self.player.borrow_mut().seek_to_ms((to * 1000.0) as u64);
            self.model.push_notice(t_args(
                "tube-notice-skipped-to",
                &[arg("time", crate::util::clock(to))],
            ));
        }
    }

    pub(super) fn apply_auto_skip(&mut self) -> bool {
        if !self.model.settings.sponsorblock_enabled {
            return false;
        }
        let Route::Watch(id) = self.model.route.clone() else {
            return false;
        };
        let (seg_end, label) = {
            let Some(v) = self.model.video(&id) else {
                return false;
            };
            let mut found = None;
            for s in &v.sponsor {
                if self.model.settings.should_auto_skip(s.category)
                    && s.contains(self.model.playhead)
                {
                    found = Some((
                        s.end,
                        t_args(
                            "tube-notice-skipped-segment",
                            &[
                                arg("segment", crate::i18n::t(s.category.message())),
                                arg("start", crate::util::clock(s.start)),
                                arg("end", crate::util::clock(s.end)),
                            ],
                        ),
                    ));
                    break;
                }
            }
            match found {
                Some(f) => f,
                None => return false,
            }
        };
        self.model.playhead = seg_end;
        self.player
            .borrow_mut()
            .seek_to_ms((seg_end * 1000.0) as u64);
        self.model.push_notice(label);
        true
    }

    pub(super) fn open_video(&mut self, id: String) {
        let watched_at_ms = ui::time::unix_now_ms();
        self.edit_history(&WatchHistory::RECORD.with((id.clone(), watched_at_ms)));
        self.navigate_to(Route::Watch(id.clone()));
        let resume_at_ms = self.model.resume_at_ms(&id);
        self.model.playhead = resume_at_ms.map_or(0.0, |position| position as f64 / 1000.0);
        self.resume_at_ms = resume_at_ms;
        self.progress_written = None;
        self.model.stream_quality.clear();
        self.model.captions.clear();
        self.model.caption_tracks.clear();
        self.model.captions_of.clear();
        self.model.captions_notice = None;
        self.model.playing = true;
        self.request_captions_for(&id);
        self.request_live(Request::Branding(id.clone()));
        if let Some(cid) = self.model.video(&id).map(|v| v.channel_id.clone()) {
            self.request_channel_if_stub(&cid);
        }
        self.request_live(Request::VideoMeta(id));
    }

    pub(super) fn request_captions_for(&mut self, id: &str) {
        if !self.online {
            #[cfg(feature = "samples")]
            let tracks = crate::sample::caption_tracks(id);
            #[cfg(not(feature = "samples"))]
            let tracks = Vec::new();
            self.model.captions_of = id.to_string();
            self.model.caption_tracks = tracks;
            self.load_chosen_captions();
            return;
        }
        let language = self.model.settings.caption_language.clone();
        self.request_live(Request::Captions {
            id: id.to_string(),
            language,
        });
    }

    pub(super) fn load_chosen_captions(&mut self) {
        let id = self.model.captions_of.clone();
        if id.is_empty() {
            return;
        }
        let Some(language) = self.model.settings.caption_language.clone() else {
            self.model.captions.clear();
            self.model.captions_notice = None;
            return;
        };
        #[cfg(feature = "samples")]
        if let Some(cues) = crate::sample::caption_cues(&id, &language) {
            self.model.captions = cues;
            self.model.captions_notice = None;
            return;
        }
        if self.model.caption_tracks.is_empty() {
            return;
        }
        if self.model.chosen_caption_track().is_none() {
            self.model.captions.clear();
            self.model.captions_notice = Some(t("tube-captions-not-offered"));
            return;
        }
        if self.online && self.model.captions.is_empty() {
            self.model.captions_notice = Some(t("tube-captions-loading"));
            self.request_live(Request::Captions {
                id,
                language: Some(language),
            });
        }
    }

    fn remember_progress(&mut self, force: bool) {
        let Route::Watch(id) = self.model.route.clone() else {
            return;
        };
        self.remember_progress_of(&id, force);
    }

    fn remember_progress_of(&mut self, id: &str, force: bool) {
        let id = id.to_string();
        if self.player_video.as_deref() != Some(id.as_str()) {
            return;
        }
        let position = {
            let player = self.player.borrow();
            if player.has_media() {
                player.position_ms()
            } else {
                (self.model.playhead * 1000.0).max(0.0) as u64
            }
        };
        let bucket = position / PROGRESS_STEP_MS;
        let written = self.progress_written.clone();
        let recorded = self.model.history_positions.get(&id).copied();
        if !force
            && written
                .as_ref()
                .is_some_and(|(video, bucket_written)| *video == id && *bucket_written == bucket)
        {
            return;
        }
        if !force && recorded.is_some_and(|recorded| recorded / PROGRESS_STEP_MS == bucket) {
            return;
        }
        let duration_ms = {
            let player = self.player.borrow();
            if player.duration_ms() > 0 {
                player.duration_ms()
            } else {
                self.model
                    .video(&id)
                    .map_or(0, |video| u64::from(video.duration) * 1000)
            }
        };
        self.progress_written = Some((id.clone(), bucket));
        self.edit_history(&WatchHistory::PROGRESS.with((id, position, duration_ms)));
    }

    fn maybe_autoplay_next(&mut self) -> bool {
        if !self.model.settings.autoplay {
            return false;
        }
        let Route::Watch(id) = self.model.route.clone() else {
            return false;
        };
        let Some(dur) = self.model.video(&id).map(|v| f64::from(v.duration)) else {
            return false;
        };
        if self.model.playhead < dur - 0.001 {
            return false;
        }
        let next = self
            .model
            .related_display(&id)
            .first()
            .map(|v| v.id.clone());
        if let Some(next) = next
            && next != id
        {
            self.open_video(next);
            return true;
        }
        false
    }
}
