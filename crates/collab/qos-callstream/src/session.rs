//! The clean send/receive seam over the streaming stack.
//!
//! [`VideoStreamSender`] composes [`crate::throttle::StreamingThrottle`] +
//! [`crate::encoder::H264Encoder`] and emits [`crate::stream::SurfaceFrame`]
//! bytes. [`VideoStreamReceiver`] composes
//! [`crate::stream::SurfaceStreamReceiver`] + [`crate::decoder::H264Decoder`]
//! and turns wire bytes back into [`crate::decoder::DecodedFrame`]s for a
//! call tile.
//!
//! This is the exact composition the compositor's
//! `collab::flatpak_share::SharedFlatpakSession` uses for a shared Wayland
//! surface, and what the chat call app drives directly for `ScreenCast`
//! capture — reusing the same encode/transport/decode code instead of a
//! second codec.

use std::time::Instant;

use crate::decoder::{DecodedFrame, H264Decoder};
use crate::encoder::{EncodeError, H264Encoder};
use crate::stream::{ReceiverEvent, SurfaceFrame, SurfaceStreamReceiver};
use crate::throttle::StreamingThrottle;

/// Scans `nalus` for an H.264 IDR NAL unit (`nal_unit_type == 5`) by
/// walking Annex-B start-codes. Returns `false` for a stream without a
/// start-code — treating "unknown" as non-keyframe is safer, since a
/// late-joining viewer then waits for the next detectable IDR. Same probe
/// the compositor's share session uses to set the `SurfaceFrame` flag.
pub fn is_h264_keyframe(nalus: &[u8]) -> bool {
    let mut i: usize = 0;
    // Loop while at least 4 bytes remain from `i` — enough for either
    // start-code shape and its follow-on NAL header byte to be checked below
    // without any raw indexing.
    while let Some(&[b0, b1, b2, b3]) = nalus.get(i..).and_then(|s| s.first_chunk::<4>()) {
        // 4-byte start code `00 00 00 01`, NAL header at `i+4`.
        if b0 == 0 && b1 == 0 && b2 == 0 && b3 == 1 {
            if nalus
                .get(i.saturating_add(4))
                .is_some_and(|h| (h & 0x1f) == 5)
            {
                return true;
            }
            i = i.saturating_add(4);
            continue;
        }
        // 3-byte start code `00 00 01`, NAL header at `i+3`.
        if b0 == 0 && b1 == 0 && b2 == 1 {
            if (b3 & 0x1f) == 5 {
                return true;
            }
            i = i.saturating_add(3);
            continue;
        }
        i = i.saturating_add(1);
    }
    false
}

/// Send half of a video stream: throttle + H.264 encode → framed
/// [`SurfaceFrame`] bytes. Own one per outgoing source; feed captured RGBA
/// via [`Self::offer_frame`], which returns the on-wire bytes to publish
/// or `None` if throttled or the encoder emitted nothing.
pub struct VideoStreamSender {
    encoder: H264Encoder,
    throttle: StreamingThrottle,
    sequence: u64,
    width: u32,
    height: u32,
}

impl VideoStreamSender {
    /// Builds a sender encoding `width × height` at up to `fps` frames per second (the throttle enforces the real cadence).
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError`] if the underlying H.264 encoder fails to
    /// initialise for `width`/`height`/`fps`.
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, EncodeError> {
        let per_second = u64::from(fps.max(1));
        let interval = std::time::Duration::from_micros(
            1_000_000u64.checked_div(per_second).unwrap_or(1).max(1),
        );
        Ok(Self {
            encoder: H264Encoder::new(width, height, fps)?,
            throttle: StreamingThrottle::with_interval(interval),
            sequence: 0,
            width,
            height,
        })
    }

    /// Configured width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Configured height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Offers a captured RGBA frame (`width * height * 4` bytes) at wall clock
    /// `now`. Returns `Some(bytes)` — the [`SurfaceFrame`]-framed encoded
    /// access unit — when the throttle admits it and the encoder produced a
    /// non-empty NALU; `None` if throttled, size-mismatched, or empty output.
    pub fn offer_frame(&mut self, rgba: &[u8], now: Instant) -> Option<Vec<u8>> {
        if !self.throttle.should_encode(now) {
            return None;
        }
        let nalus = self.encoder.encode(rgba).ok()?;
        if nalus.is_empty() {
            return None;
        }
        self.throttle.mark_encoded(now);
        let keyframe = is_h264_keyframe(&nalus);
        let frame = SurfaceFrame {
            sequence: self.sequence,
            keyframe,
            payload: nalus,
        };
        self.sequence = self.sequence.wrapping_add(1);
        Some(frame.encode())
    }

    /// The next sequence number this sender will mint. Diagnostic.
    pub fn next_sequence(&self) -> u64 {
        self.sequence
    }
}

/// Receive half of a video stream: [`SurfaceFrame`] reframing + H.264
/// decode → RGBA [`DecodedFrame`]s. Own one per incoming source (keyed by
/// sender + track); feed it bytes from [`VideoStreamSender::offer_frame`]
/// via [`Self::push`]. Sequence gaps are counted via [`Self::dropped_count`].
pub struct VideoStreamReceiver {
    framer: SurfaceStreamReceiver,
    decoder: H264Decoder,
}

impl VideoStreamReceiver {
    pub fn new() -> Self {
        Self {
            framer: SurfaceStreamReceiver::new(),
            decoder: H264Decoder::new(),
        }
    }

    /// Pushes wire `bytes` (one or more [`SurfaceFrame`]-framed payloads,
    /// possibly split across calls) and returns every decoded RGBA frame that
    /// became available; a not-yet-decodable NALU simply yields no output.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<DecodedFrame> {
        self.framer.push_bytes(bytes);
        // A corrupt length header can't be recovered mid-stream; yield nothing and let the next bytes reframe.
        let Ok(events) = self.framer.drain() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for ev in events {
            if let ReceiverEvent::Frame(frame) = ev
                && let Some(decoded) = self.decoder.decode(&frame.payload)
            {
                out.push(decoded);
            }
        }
        out
    }

    /// Cumulative count of frames the receiver detected as missing from the sequence.
    pub fn dropped_count(&self) -> u64 {
        self.framer.dropped_count()
    }
}

impl Default for VideoStreamReceiver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn gradient(width: u32, height: u32) -> Vec<u8> {
        let capacity = width.saturating_mul(height).saturating_mul(4) as usize;
        let mut out = Vec::with_capacity(capacity);
        let channel = |numerator: u32, denominator: u32| -> u8 {
            let scaled = numerator
                .saturating_mul(255)
                .checked_div(denominator.max(1))
                .unwrap_or(0);
            u8::try_from(scaled).unwrap_or(u8::MAX)
        };
        for y in 0..height {
            for x in 0..width {
                out.push(channel(x, width));
                out.push(channel(y, height));
                out.push(128);
                out.push(255);
            }
        }
        out
    }

    /// The headline reuse contract: a captured RGBA frame runs the full
    /// shared stack (throttle + encoder → framed bytes → reframe + decoder →
    /// RGBA) and lands as a same-sized decoded frame — the same path a chat
    /// screen-share tile runs.
    #[test]
    fn captured_rgba_streams_end_to_end_through_the_shared_stack() {
        const W: u32 = 128;
        const H: u32 = 96;
        let mut sender = VideoStreamSender::new(W, H, 30).expect("sender");
        let now = Instant::now();
        let wire = sender
            .offer_frame(&gradient(W, H), now)
            .expect("throttle admits first frame + encoder emits a NALU");
        assert!(wire.len() > crate::stream::FRAME_HEADER_LEN, "framed bytes");

        let mut receiver = VideoStreamReceiver::new();
        let frames = receiver.push(&wire);
        assert_eq!(frames.len(), 1, "one frame decoded off the wire");
        assert_eq!((frames[0].width, frames[0].height), (W, H));
        assert_eq!(frames[0].rgba.len(), (W * H * 4) as usize);
    }

    #[test]
    fn throttle_rejects_a_second_frame_within_the_interval() {
        const W: u32 = 32;
        const H: u32 = 32;
        let mut sender = VideoStreamSender::new(W, H, 30).expect("sender");
        let now = Instant::now();
        assert!(sender.offer_frame(&gradient(W, H), now).is_some());
        // 1 ms later — inside the ~33 ms interval — must be throttled.
        assert!(
            sender
                .offer_frame(&gradient(W, H), now + Duration::from_millis(1))
                .is_none()
        );
        // Past the interval, accepts again.
        assert!(
            sender
                .offer_frame(&gradient(W, H), now + Duration::from_millis(40))
                .is_some()
        );
    }

    #[test]
    fn first_frame_is_flagged_a_keyframe() {
        const W: u32 = 64;
        const H: u32 = 64;
        let mut sender = VideoStreamSender::new(W, H, 30).expect("sender");
        let wire = sender
            .offer_frame(&gradient(W, H), Instant::now())
            .expect("frame");
        let (frame, _) = SurfaceFrame::decode(&wire).expect("decode").expect("whole");
        assert!(
            frame.keyframe,
            "the first H.264 access unit is an IDR keyframe"
        );
        assert_eq!(frame.sequence, 0);
    }

    #[test]
    fn wrong_sized_buffer_yields_no_frame() {
        let mut sender = VideoStreamSender::new(32, 32, 30).expect("sender");
        // RGB (3 bytes/px), not RGBA — size mismatch folds to None.
        assert!(
            sender
                .offer_frame(&vec![0u8; 32 * 32 * 3], Instant::now())
                .is_none()
        );
    }

    #[test]
    fn keyframe_probe_detects_idr_and_rejects_non_idr() {
        // 4-byte start code + nal_unit_type 5 (IDR).
        assert!(is_h264_keyframe(&[0, 0, 0, 1, 0x65]));
        // 3-byte start code + nal_unit_type 1 (non-IDR slice).
        assert!(!is_h264_keyframe(&[0, 0, 1, 0x41]));
        // No start code.
        assert!(!is_h264_keyframe(&[0xAA, 0xBB, 0xCC]));
    }
}
