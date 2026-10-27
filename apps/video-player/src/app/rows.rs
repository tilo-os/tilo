//! The rows the window's chrome reads: what is playing, what the library holds and what each row says, the tab it shows, and the rect of its own control.

use super::*;

impl PlayerApp {
    /// What the player shows that the document does not hold, for its live
    /// rows.
    pub(crate) fn player_rows(&self) -> crate::actions::PlayerRows<'_> {
        crate::actions::PlayerRows {
            title: self.player.has_media().then_some(self.title.as_str()),
            source_id: self.current_source_id.as_deref(),
            audio_tracks: &self.shown_metadata.audio_tracks,
            subtitle_tracks: &self.shown_metadata.subtitle_tracks,
            chapters: &self.shown_metadata.chapters,
            queue: self.library_items(self.queue.iter()),
            recent: self.library_items(self.recent.iter()),
            playing: self.player.is_playing(),
        }
    }

    /// What the current queue or recent row says about the open video: its
    /// real state, playing or paused.
    pub(crate) fn current_row_state(&self) -> &'static str {
        if self.player.is_playing() {
            "Playing"
        } else {
            "Paused"
        }
    }

    pub(super) fn library_items<'a>(
        &self,
        entries: impl Iterator<Item = &'a LocalMediaEntry>,
    ) -> Vec<crate::actions::LocalMediaItemState> {
        entries
            .map(|entry| crate::actions::LocalMediaItemState {
                id: entry.id.clone(),
                title: entry.title.clone(),
                current: self.current_queue_id.as_deref() == Some(entry.id.as_str()),
            })
            .collect()
    }

    /// Show the shared library the document holds.
    pub(crate) fn present_library(&mut self, state: &crate::shared::Library) {
        if state != &self.shared_library {
            self.shared_library = state.clone();
            self.dirty = true;
        }
    }

    /// The open video's shareable identity, which the shared library's
    /// notes and transport name it by; never a path.
    pub fn media_ref(&self) -> Option<&str> {
        self.current_media_ref.as_deref()
    }

    /// Whether the library panel shows.
    pub fn library_visible(&self) -> bool {
        self.library_visible
    }

    /// The library tab shown or last shown: `queue`, `recent` or `shared`.
    pub fn library_tab_name(&self) -> &'static str {
        self.library_tab.name()
    }

    /// Shared read access to the inner video surface, for tests that assert
    /// a decoded frame landed.
    pub fn player(&self) -> &VideoPlayer {
        &self.player
    }

    /// What this device keeps between launches, as the next launch reads it:
    /// `None` for a capture or a past version, which keep nothing.
    #[cfg(test)]
    pub(crate) fn stored_settings(&self) -> Option<PlayerSettings> {
        self.setting.as_ref().map(Setting::get)
    }

    /// Whether a decoded picture is currently on the surface. The `--shots`
    /// path pumps `sync` until this is true so a real (asynchronously prerolled)
    /// decode is captured, not a blank frame.
    pub fn has_media_frame(&self) -> bool {
        self.player.latest_frame().is_some()
    }

    /// The picture area says the decoder could not play the video. The
    /// decoder's own wording is for logs and agents; the person is told what
    /// it means for them, the same way wherever a decode fails.
    pub(super) fn fail_playback(&mut self) {
        self.open_error = Some(DECODER_FAILURE.into());
        self.dirty = true;
    }

    /// Where the library's tab, action or row at `index` in
    /// [`Widget::accessibility`]'s order (past "Back to video") stands.
    pub(super) fn library_control_rect(&self, index: usize) -> Option<Rect> {
        let shared = self.library_tab == LibraryTab::Shared;
        match index {
            0 => Some(self.library_queue_tab_rect),
            1 => Some(self.library_recent_tab_rect),
            2 => Some(self.library_shared_tab_rect),
            3 if shared => Some(self.shared_watch_later_rect),
            4 if shared => Some(self.shared_bookmark_rect),
            index if shared => shared_accessibility_targets(self)
                .get(index.saturating_sub(5))
                .map(|(_, _, rect)| *rect),
            index => self.local_row_rect(index.saturating_sub(3)),
        }
    }
}
