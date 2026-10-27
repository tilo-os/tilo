//! What the player remembers: the recent list and the position a clip was left at, the queue entry a finished clip advances past, and the source that is open now.

use super::*;

impl PlayerApp {
    pub(super) fn record_current_history(&mut self) {
        let Some(queue_id) = self.current_queue_id.as_deref() else {
            return;
        };
        let Some(entry) = self
            .queue
            .iter()
            .find(|entry| entry.id == queue_id)
            .cloned()
        else {
            return;
        };
        self.recent.retain(|item| item.id != entry.id);
        self.recent.push_front(entry);
        self.recent
            .truncate(crate::preferences::RECENT_HISTORY_LIMIT);
        if let Err(error) = self.persist_recent_history() {
            tracing::warn!(%error, "video player: could not persist recent history");
        }
        self.dirty = true;
    }

    pub(super) fn persist_recent_history(&self) -> std::io::Result<()> {
        let entries: Vec<RecentMedia> = self
            .recent
            .iter()
            .filter_map(|entry| self.saved_recent_row(entry))
            .collect();
        match &self.setting {
            Some(setting) => setting.update(|stored| stored.recent_history = entries),
            None => Ok(()),
        }
    }

    /// The row `entry` is kept as, or `None` when it cannot be kept: a page's
    /// `blob:` source dies with the page, so an entry is kept by its kept
    /// copy, while that copy is retained, or not at all. The row carries where
    /// the video stood, which is the only place a resume point outlives the
    /// session.
    pub(super) fn saved_recent_row(&self, entry: &LocalMediaEntry) -> Option<RecentMedia> {
        let source = match self.kept.get(&entry.id) {
            Some(token) => crate::kept::source(token),
            None => entry.source.clone(),
        };
        let retained = crate::kept::token_of(&source)
            .is_none_or(|token| self.kept_retained.iter().any(|kept| kept == token));
        (retained && !crate::kept::page_local(&source)).then(|| RecentMedia {
            id: entry.id.clone(),
            title: entry.title.clone(),
            position_ms: self.position_of(&source),
            source,
        })
    }

    /// Where `source` left off here, `0` when it has nothing to resume.
    pub(super) fn position_of(&self, source: &media::Source) -> u64 {
        self.resume
            .point(&resume_key(source))
            .map_or(0, |(position_ms, _)| position_ms)
    }

    /// Write where the open video stands into its Recent row, leaving the
    /// rest of this device's settings as they are.
    pub(super) fn persist_resume_position(&self) -> std::io::Result<()> {
        let Some(setting) = &self.setting else {
            return Ok(());
        };
        let resume = &self.resume;
        setting.update(|stored| {
            for row in &mut stored.recent_history {
                row.position_ms = resume
                    .point(&resume_key(&row.source))
                    .map_or(0, |(position_ms, _)| position_ms);
            }
        })
    }

    /// Remember where the open video stands, so opening it again resumes
    /// there. A playing video writes every few seconds; `settled` writes at
    /// once, for the moments playback pauses, the item closes and another
    /// item replaces it. A video within [`RESUME_END_MARGIN_MS`] of its end
    /// has finished, so its point is cleared and replaying starts at the
    /// beginning.
    ///
    /// A source whose length the decoder never reports (a live stream) has no
    /// place to seek back to, so it keeps no point: the pending seek of the
    /// point it was opened with is never spent, and nothing is written.
    pub(super) fn remember_position(&mut self, settled: bool) {
        // While a stored point is still waiting for the decoder's length, the
        // playhead means nothing yet: writing it would forget that point.
        if self.pending_resume.is_some() {
            return;
        }
        let Some(media_ref) = self.current_media_ref.clone() else {
            return;
        };
        if !self.player.has_media() {
            return;
        }
        let position = self.player.position_ms();
        let playing = self.player.is_playing();
        let now = unix_now_ms();
        let written = self.resume_write.as_ref();
        if written.is_some_and(|written| {
            written.media_ref == media_ref
                && written.position_ms == position
                && written.playing == playing
        }) {
            return;
        }
        // A drag across the timeline, or a video playing on, is one write
        // every few seconds, not one per frame. The newest value still lands:
        // playback comes to rest with a `settled` write.
        if !settled
            && playing
            && written.is_some_and(|written| {
                written.media_ref == media_ref
                    && now.saturating_sub(written.at_ms) < RESUME_WRITE_INTERVAL_MS
            })
        {
            return;
        }
        self.resume_write = Some(ResumeWrite {
            media_ref: media_ref.clone(),
            position_ms: position,
            playing,
            at_ms: now,
        });
        let duration = self.player.duration_ms();
        let position = if duration > 0 {
            position.min(duration)
        } else {
            position
        };
        if duration > 0 && position.saturating_add(RESUME_END_MARGIN_MS) >= duration {
            // Finished: replay starts at the beginning.
            self.resume.forget(&media_ref);
        } else {
            self.resume.record(&media_ref, position, playing);
        }
        if let Err(error) = self.persist_resume_position() {
            tracing::warn!(%error, "video player: could not keep where the video stood");
        }
    }

    /// Move a just-opened video to where it left off, once the decoder knows
    /// how long it is: a seek is clamped to the length the player knows, and
    /// until the decoder answers, that is only the placeholder length it was
    /// built with.
    pub(super) fn apply_pending_resume(&mut self) {
        let Some(pending) = self.pending_resume.clone() else {
            return;
        };
        if self.current_media_ref.as_deref() != Some(pending.media_ref.as_str()) {
            // The video it was taken for is gone.
            self.pending_resume = None;
            return;
        }
        let duration = self
            .player
            .engine()
            .and_then(media::MediaEngine::duration_ms);
        let Some(duration) = duration.filter(|duration| *duration > 0) else {
            return;
        };
        if self.player.duration_ms() != duration {
            // The player has not picked the length up yet: a seek now would be
            // clamped to the placeholder it still holds.
            return;
        }
        self.pending_resume = None;
        self.player.seek_to_ms(pending.position_ms.min(duration));
        self.dirty = true;
    }

    pub(super) fn clear_recent_history(&mut self) -> Result<(), String> {
        let previous = self.recent.clone();
        self.recent.clear();
        if let Err(error) = self.persist_recent_history() {
            self.recent = previous;
            return Err(format!("could not clear local recent history: {error}"));
        }
        // A cleared history is a cleared memory of where those videos stood.
        self.resume.clear();
        self.resume_write = None;
        if !self.kept_retained.is_empty() {
            self.kept.clear();
            self.kept_retained.clear();
            crate::kept::forget_all();
        }
        self.dirty = true;
        Ok(())
    }

    pub(super) fn remove_queue_entry(&mut self, item_id: &str) -> Result<(), String> {
        let Some(index) = self.queue.iter().position(|entry| entry.id == item_id) else {
            return Err("that local queue item is no longer available".into());
        };
        self.queue.remove(index);
        if self.current_queue_id.as_deref() == Some(item_id) {
            self.current_queue_id = None;
        }
        self.dirty = true;
        Ok(())
    }

    pub(super) fn advance_after_engine_end(&mut self) {
        if self.preferences.loop_enabled {
            return;
        }
        let Some(current) = self.current_queue_id.as_deref() else {
            return;
        };
        let Some(index) = self.queue.iter().position(|entry| entry.id == current) else {
            return;
        };
        let Some(next) = self.queue.get(index.saturating_add(1)) else {
            return;
        };
        let next_id = next.id.clone();
        if let Err(error) = self.open_local_entry(&next_id) {
            self.open_error = Some(error);
            self.dirty = true;
        }
    }

    /// Assign the next never-reused source identity. Once the u64 namespace is
    /// exhausted the app refuses a new source rather than making an action
    /// address from the start of the process current again.
    pub(super) fn set_current_source(&mut self, source: &media::Source) -> Result<(), String> {
        let generation = self.source_generation.checked_add(1).ok_or_else(|| {
            "video source identity generation exhausted; restart Video Player".to_string()
        })?;
        self.source_generation = generation;
        self.current_source_id = Some(local_source_token(source, generation));
        self.current_media_ref = Some(local_source_token(source, 0));
        Ok(())
    }

    pub(super) fn close_media(&mut self) {
        self.missing_codec = None;
        self.fail_open("video was closed before decoding a first frame".into());
        // Closing is one of the moments the video rests: keep where it stood.
        self.remember_position(true);
        self.pending_resume = None;
        self.resume_write = None;
        let player = configured_player(self.mode, self.preferences);
        self.player = player;
        self.title = "No media".into();
        self.current_source_id = None;
        self.current_media_ref = None;
        self.current_queue_id = None;
        self.dirty = true;
    }

    pub(super) fn request_picture_in_picture(&mut self) {
        if !self.player.has_media() || self.player.get_rect().width() < 1.0 {
            return;
        }
        let region = self.player.get_rect();
        let aspect = (region.width() / region.height().max(1.0)).clamp(0.4, 2.8);
        self.pending_pip = Some(ui::floating_surface::FloatingSurfaceRequest {
            id: "video-player.current-video".into(),
            title: self.title.clone(),
            role: ui::floating_surface::FloatingRole::PictureInPicture,
            source: ui::floating_surface::FloatingSource::AppElement {
                app_id: "so.tilo.video-player".into(),
                element_id: "video_surface".into(),
                region,
            },
            preferred_size: vello::kurbo::Size::new(360.0, 360.0 / aspect),
            aspect_ratio: Some(aspect),
            initial_rect: None,
            presentation_rect: None,
            tracking_source: None,
        });
        self.dirty = true;
    }
}
