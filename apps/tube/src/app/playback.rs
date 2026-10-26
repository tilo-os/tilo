//! The watch page's player: the shared `VideoPlayer` and the media engine
//! beneath it. What a stream is opened from, where playback starts again, what
//! each frame does with the playhead (mirror it, skip a segment set to skip,
//! move on at the end), what seeking and skipping command of it, and what
//! playback writes down afterwards: the position it keeps in the person's watch
//! history, and the captions it shows while it plays.

use ui::widget::VideoPlayer;
use ui::widget::Widget as _;

use crate::api::Request;
use crate::history::WatchHistory;
use crate::i18n::{arg, t, t_args};
use crate::model::Route;
use crate::screens;

use super::{PLAYBACK_SPEEDS, QUALITIES, TubeApp};

/// How far the playhead moves between two writes of the watch history: a
/// two-hour video is a few hundred small edits, not one per frame, and still
/// resumes at the minute the person stopped at.
const PROGRESS_STEP_MS: u64 = 5_000;

/// Pull the frames the player's engine decoded into its surface, as the
/// player's own `sync` does every frame; it reads no document.
pub(super) fn pump_player(player: &mut VideoPlayer) {
    player.sync(&(), &());
}

impl TubeApp {
    /// If the current route is a watch page we haven't started playing yet, ask
    /// the worker to resolve + decode a real stream for it — exactly once per
    /// watch-open. Online-only, so the offline `--shots`/test paths never spawn
    /// the worker or reach the network. Opening a *different* video re-arms this
    /// and drops the previous video's frame so the poster shows until the new
    /// stream decodes.
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
                // A fresh resolution is pending; drop the previously-loaded
                // source so a new video starts clean once its URL lands.
                self.player_video = None;
                self.player.borrow_mut().pause();
                // Nothing here plays it; the page says where it does.
                if self
                    .model
                    .video(&id)
                    .is_some_and(crate::model::Video::plays_only_in_app)
                {
                    return;
                }
                // The decoder starts up while the stream URL resolves.
                media::MediaEngine::prepare_in_background();
                // The quality the person chose is what the stream resolves to:
                // the choice is not a preference that never reaches the wire.
                let quality = self.model.settings.preferred_quality.clone();
                self.request_live(Request::PlayVideo { id, quality });
            }
            // Left the watch page: pause the engine so audio stops and it isn't
            // still streaming a video the user is no longer watching.
            _ => {
                if let Some(left) = self.playing_id.take() {
                    // The minute they stopped at is the one to come back to.
                    self.remember_progress_of(&left, true);
                    self.player_video = None;
                    self.resume_at_ms = None;
                    self.player.borrow_mut().pause();
                }
            }
        }
    }

    /// Per-frame playback bookkeeping: mirror the engine's playhead onto the
    /// model (so `SponsorBlock` auto-skip + autoplay-next key off the real
    /// position), auto-skip enabled segments by seeking the engine, and advance
    /// to the next recommendation when the stream ends. Returns `true` only when
    /// something *visible* changed (a skip notice or an autoplay navigation) so
    /// it never dirties the tree merely because the playhead advanced. A no-op
    /// off the watch route or before a stream has loaded.
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
        // Until a source is open the engine has no playhead to follow: an
        // auto-skip from the zero position would seek nowhere and fire again
        // on every frame.
        if !self.player.borrow().has_media() {
            return false;
        }
        // Short borrow: read the engine's position/duration/playing, then drop
        // the borrow before any seek (auto-skip re-borrows).
        let (pos_ms, dur_ms, playing) = {
            let p = self.player.borrow();
            (p.position_ms(), p.duration_ms(), p.is_playing())
        };
        self.model.playhead = pos_ms as f64 / 1000.0;
        self.model.playing = playing;
        // Where this video was left, kept for "continue watching" on this
        // device and every other one; throttled, and forced when playback
        // stops or the page closes.
        self.remember_progress(false);

        // Auto-skip runs against the real playhead and seeks the engine.
        let mut changed = self.apply_auto_skip();
        // On-video "Skip" overlay visibility: recompute which (if any) show-mode
        // segment the playhead sits in. A transition (enter/leave) must rebuild
        // the tree so the overlay appears/dismisses — the advancing playhead by
        // itself never dirties it.
        let show_end = self.current_show_skip_end();
        if show_end != self.show_skip_end {
            self.show_skip_end = show_end;
            changed = true;
        }
        // Reaching the end advances to the next "up next" when autoplay is on.
        // `duration_ms` is the media duration once known; treat within 250ms of
        // the end (or an explicitly non-playing engine at the end) as finished.
        let known_dur = dur_ms > 0;
        let at_end = known_dur && pos_ms + 250 >= dur_ms;
        if at_end {
            // Watched to the end: there is no minute to come back to, and
            // "continue watching" must not offer the credits.
            self.remember_progress_of(&id, true);
        }
        if at_end && self.maybe_autoplay_next() {
            changed = true;
        }
        changed
    }

    /// Whether the player's terminal `events` stopped playback in a way the
    /// page says: a host that streamed all it allows for now (a page's
    /// source through the host's fetch proxy) ends with a notice, never a
    /// picture that silently stops.
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

    /// Apply one player-setting interaction drained from the shared player. The
    /// resolution choice round-trips into the model's preferred quality; other
    /// settings are ignored for now. This is the production drain path (called
    /// from `sync`), factored out so it is directly testable.
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
                // The first row is "Off"; the rest are the tracks the video
                // offers, in the order the page listed them.
                let language = match index {
                    0 => None,
                    index => self
                        .model
                        .caption_tracks
                        .get(index.saturating_sub(1))
                        .map(|track| track.language.clone()),
                };
                if self.change_preferences(|settings| {
                    settings.caption_language = language.clone();
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

    /// Resolve the open video again, for a choice that changes what is
    /// fetched (a smaller quality), starting from where the playhead is now so
    /// the change is not a jump back to the beginning.
    fn reload_stream_at_current_position(&mut self) {
        if !self.online {
            // Nothing was fetched: the page plays what it was given.
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

    /// Load a resolved, directly-playable stream URL into the shared player
    /// engine and start it — the real path both the worker's
    /// [`Update::StreamResolved`] and the transport tests take. A resolution for
    /// a video that is no longer the open watch page is dropped (returns `false`)
    /// so audio never bleeds across pages.
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
        // Now-playing metadata for the desktop media widget (MPRIS): title,
        // channel and the real thumbnail, built before we take the player borrow.
        let now_playing = self.model.video(&video).map(|v| {
            let dearrow = self.model.settings.dearrow_titles_on();
            ui::widget::video_player::NowPlaying {
                track_id: v.id.clone(),
                title: v.display_title(dearrow).to_string(),
                artist: v.channel_name.clone(),
                art_url: v.thumbnail_url.clone(),
            }
        });
        // Continue where the person left off: the seek can only happen once
        // the source exists, so it is taken here rather than when the page
        // opened. Everything the player needs is read before it is borrowed.
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
        // Bring up MPRIS once, in the live app, so media keys + the system media
        // widget can drive this player and show its now-playing.
        if self.online && !self.mpris_started {
            self.player
                .borrow_mut()
                .enable_mpris("tilo-tube", "Tilo Tube");
            self.mpris_started = true;
        }
        self.player_video = Some(video);
        self.model.playing = true;
        // No debug text under the player — the picture playing IS the feedback.
        // Quality/audio provenance is a developer diagnostic, so it goes to the
        // log, not an on-screen toast.
        tracing::debug!(%quality, has_audio, "stream loaded into player");
        true
    }

    /// The shared player, with its media engine built if this is the first
    /// thing it plays. Tube builds no engine with its window: the first one
    /// starts `GStreamer`, which took 0.8 s of the window's first frame
    /// whether or not anything was ever watched.
    pub(super) fn playing_player(&self) -> std::cell::RefMut<'_, VideoPlayer> {
        let mut player = self.player.borrow_mut();
        player.ensure_engine(media::MediaEngine::new);
        player
    }

    /// Resolve the current watch video's duration, if any.
    pub(super) fn current_duration(&self) -> Option<f64> {
        match &self.model.route {
            Route::Watch(id) => self.model.video(id).map(|v| f64::from(v.duration)),
            _ => None,
        }
    }

    /// The end (seconds) of the show-mode `SponsorBlock` segment the playhead is
    /// currently inside, if any — what the on-video "Skip" overlay offers.
    /// `None` off the watch route, when `SponsorBlock` is off, or when no show-mode
    /// segment contains the playhead.
    pub(super) fn current_show_skip_end(&self) -> Option<f64> {
        self.model.show_skip_end()
    }

    /// Move the open video to `at` seconds, within it: the model and the
    /// engine, then what playback does there (skip a segment set to skip,
    /// move on at the end).
    pub(super) fn seek_to(&mut self, at: f64) {
        self.model.playhead = at;
        // Seek the real stream (no-op when no source is loaded yet).
        self.player
            .borrow_mut()
            .seek_to_ms((self.model.playhead * 1000.0) as u64);
        self.apply_auto_skip();
        self.maybe_autoplay_next();
    }

    /// Skip a segment by hand: jump the model and engine playhead to `to` and
    /// confirm with a toast.
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

    /// If the playhead sits inside an enabled `SponsorBlock` segment, jump the
    /// engine (and the model) to its end and log the skip. Returns `true` when a
    /// skip actually happened. Runs both as the user scrubs and each frame from
    /// [`Self::tick_playback`] against the real engine playhead.
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
                // Only Auto-skip-mode categories skip on their own; Show-mode
                // segments wait for the manual skip button.
                if self.model.settings.should_auto_skip(s.category)
                    && s.contains(self.model.playhead)
                {
                    found = Some((
                        s.end,
                        t_args(
                            "tube-notice-skipped-segment",
                            &[
                                arg("segment", s.category.label()),
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
        // Seek the real stream past the segment (no-op when no source is loaded).
        self.player
            .borrow_mut()
            .seek_to_ms((seg_end * 1000.0) as u64);
        self.model.push_notice(label);
        true
    }

    /// Open a video's watch page: record history, route to it, reset the
    /// playhead and start playing, and kick off metadata/branding fetches.
    /// Shared by the card and search-result open paths and by autoplay-next so they stay
    /// in lockstep.
    pub(super) fn open_video(&mut self, id: String) {
        let watched_at_ms = ui::time::unix_now_ms();
        self.edit_history(&WatchHistory::RECORD.with((id.clone(), watched_at_ms)));
        self.navigate_to(Route::Watch(id.clone()));
        // Continue where the person left this video (here or on another
        // device), rather than at 0:00 as though they had never seen it. The
        // seek itself waits for the stream: `load_resolved_stream` takes it.
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
        // SponsorBlock, DeArrow and dislikes come from the network, so an
        // offline window (a capture, a staged test) asks for none of them.
        self.request_live(Request::Branding(id.clone()));
        // Fetch the uploader's real avatar + subscriber count when we only hold a
        // name stub (feed/search items don't carry the channel's avatar or subs),
        // so the watch-page channel row shows a real photo + count rather than an
        // initials chip and "0 subscribers".
        if let Some(cid) = self.model.video(&id).map(|v| v.channel_id.clone()) {
            self.request_channel_if_stub(&cid);
        }
        self.request_live(Request::VideoMeta(id));
    }

    /// Ask for the caption tracks the open video offers, and for the lines of
    /// the track the person chose. Offline (a capture, a staged test) the
    /// bundled sample video's captions are used instead, so the surface is
    /// reviewable without the network.
    pub(super) fn request_captions_for(&mut self, id: &str) {
        if !self.online {
            let tracks = crate::sample::caption_tracks(id);
            self.model.captions_of = id.to_string();
            self.model.caption_tracks = tracks;
            self.load_chosen_captions();
            return;
        }
        let language = self.model.settings.caption_language.clone();
        // Looking the tracks up is part of opening the page: the caption
        // control lists what this video offers, on or off.
        self.request_live(Request::Captions {
            id: id.to_string(),
            language,
        });
    }

    /// Take up the caption track the person chose, from the tracks the open
    /// video offers, and say so when it is not there. Shared by a fresh page,
    /// a track list that arrives from the worker, and a change of choice.
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
        if let Some(cues) = crate::sample::caption_cues(&id, &language) {
            self.model.captions = cues;
            self.model.captions_notice = None;
            return;
        }
        if self.model.caption_tracks.is_empty() {
            // The list has not arrived yet (or the video offers none): the
            // page says nothing about captions until it knows.
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

    /// Write where the open video was left into the person's watch history,
    /// at most a few seconds apart: pausing, closing the page and finishing
    /// write at once, and a running playhead writes when it crosses another
    /// [`PROGRESS_STEP_MS`].
    fn remember_progress(&mut self, force: bool) {
        let Route::Watch(id) = self.model.route.clone() else {
            return;
        };
        self.remember_progress_of(&id, force);
    }

    /// [`Self::remember_progress`] for `id`: the page may have moved on (the
    /// video just ended, the reveal left it), and the place it was left still
    /// belongs to it.
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

    /// If autoplay is on and the playhead has reached the end of the current
    /// video, advance to the first "up next" recommendation — the real code
    /// path that consumes the [`Settings::autoplay`] flag. Reaching the end
    /// happens when the engine's stream runs out (mirrored each frame by
    /// [`Self::tick_playback`]) or when a test seeks the playhead to the finish.
    /// Returns `true` when it navigated to a new video.
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
