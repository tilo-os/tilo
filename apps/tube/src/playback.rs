//! Playback bridge around [`ui::widget::VideoPlayer`].
//!
//! The data plane resolves a real googlevideo H.264 stream
//! ([`crate::backend::YtBackend::resolve_video_stream`]), downloads a bounded
//! prefix and demuxes it to Annex-B ([`crate::backend::stream::fetch_h264`] →
//! [`crate::backend::mp4`]). Those bytes are pushed here via [`Playback::feed`];
//! the wrapped [`VideoPlayer`] runs the openh264 decode and exposes the decoded
//! picture, which the watch page paints in place of the poster.
//!
//! Feeding is intended to happen off the UI thread (the fetch worker owns the
//! network + demux); the widget only holds the most recently decoded frame.

use ui::widget::VideoPlayer;

pub struct Playback {
    player: VideoPlayer,
    frames_decoded: u64,
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
            frames_decoded: 0,
        }
    }

    /// Feed one access unit of H.264 (Annex-B NAL units). Returns true when a
    /// new frame became available.
    pub fn feed(&mut self, nalus: &[u8]) -> bool {
        let decoded = self.player.feed_frame(nalus);
        if decoded {
            self.frames_decoded += 1;
        }
        decoded
    }

    /// Whether at least one real frame has decoded (poster → video swap point).
    pub fn has_frame(&self) -> bool {
        self.frames_decoded > 0
    }

    /// Total frames decoded so far this session.
    pub fn frames_decoded(&self) -> u64 {
        self.frames_decoded
    }

    /// Width/height of the most recently decoded picture, if any. Non-zero
    /// dimensions are proof a real frame came out of the decoder.
    pub fn frame_dimensions(&self) -> Option<(u32, u32)> {
        self.player.latest_frame().map(|f| (f.width, f.height))
    }

    /// The most recently decoded picture as owned `(width, height, RGBA8)`.
    ///
    /// Copies the decoded pixels out of the widget so a worker that decodes off
    /// the UI thread can hand a finished frame to the UI to paint — the "UI only
    /// drains decoded frames" contract.
    pub fn latest_rgba(&self) -> Option<(u32, u32, Vec<u8>)> {
        self.player
            .latest_frame()
            .map(|f| (f.width, f.height, f.data.as_ref().to_vec()))
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
