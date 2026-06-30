//! Playback bridge around [`ui::widget::VideoPlayer`].
//!
//! The watch page renders a poster ([`crate::widgets::Thumbnail`]) until
//! real frames arrive. When live playback is wired, the worker pulls H.264
//! NAL units from the Invidious DASH stream (via gstreamer) and feeds them
//! here; `VideoPlayer::feed_frame` decodes and the watch page swaps the
//! poster for [`Playback::player`]. This module keeps that integration
//! point real and typed even though the `--shots` path never drives it.
//!
//! TODO(live): drive `feed(...)` from a gstreamer appsink in the fetch
//! worker once an Invidious stream URL has been resolved.

use ui::widget::VideoPlayer;

pub struct Playback {
    player: VideoPlayer,
    has_frame: bool,
}

impl Default for Playback {
    fn default() -> Self {
        Self::new()
    }
}

impl Playback {
    pub fn new() -> Self {
        Self {
            player: VideoPlayer::new(),
            has_frame: false,
        }
    }

    /// Feed one access unit of H.264 (Annex-B NAL units). Returns true
    /// when a new frame became available.
    pub fn feed(&mut self, nalus: &[u8]) -> bool {
        let decoded = self.player.feed_frame(nalus);
        self.has_frame |= decoded;
        decoded
    }

    pub fn has_frame(&self) -> bool {
        self.has_frame
    }

    pub fn player_mut(&mut self) -> &mut VideoPlayer {
        &mut self.player
    }

    pub fn play(&mut self) {
        self.player.play();
    }
    pub fn pause(&mut self) {
        self.player.pause();
    }
}
