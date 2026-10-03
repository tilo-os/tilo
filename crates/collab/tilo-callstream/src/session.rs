//! Send/receive composition over the streaming stack.
//!
//! [`VideoStreamSender`] composes [`crate::throttle::StreamingThrottle`] +
//! [`crate::encoder::VideoEncoder`] and emits [`crate::stream::SurfaceFrame`]
//! bytes stamped with the codec. [`VideoStreamReceiver`] composes
//! [`crate::stream::SurfaceStreamReceiver`] + [`crate::decoder::VideoDecoder`]
//! (rebuilt when the codec on the wire changes) and turns wire bytes back
//! into [`crate::decoder::DecodedFrame`]s for a call tile.
//!
//! This is the exact composition the compositor's
//! `collab::flatpak_share::SharedFlatpakSession` uses for a shared Wayland
//! surface, and what the chat call app drives directly for `ScreenCast`
//! capture — reusing the same encode/transport/decode code instead of a
//! second codec.

use std::time::Instant;

use media::StreamCodec;

use crate::decoder::{DecodedFrame, VideoDecoder};
use crate::encoder::{EncodeError, VideoEncoder};
use crate::stream::{FrameError, ReceiverEvent, SurfaceFrame, SurfaceStreamReceiver};
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

/// Send half of a video stream: throttle + encode → framed [`SurfaceFrame`]
/// bytes. Own one per outgoing source; feed captured RGBA via
/// [`Self::offer_frame`], which returns the on-wire bytes to publish or
/// `None` if throttled or the encoder emitted nothing.
pub struct VideoStreamSender {
    encoder: VideoEncoder,
    throttle: StreamingThrottle,
    sequence: u64,
    width: u32,
    height: u32,
}

impl VideoStreamSender {
    /// Builds a sender encoding `codec` at `width × height`, up to `fps`
    /// frames per second (the throttle enforces the cadence). Pick `codec`
    /// with [`media::realtime::negotiate`] over the receivers' capabilities.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError`] if no encoder for `codec` can run here.
    pub fn new(codec: StreamCodec, width: u32, height: u32, fps: u32) -> Result<Self, EncodeError> {
        let per_second = u64::from(fps.max(1));
        let interval = std::time::Duration::from_micros(
            1_000_000u64.checked_div(per_second).unwrap_or(1).max(1),
        );
        Ok(Self {
            encoder: VideoEncoder::with_options(
                codec,
                width,
                height,
                fps,
                crate::encoder::DEFAULT_BITRATE_BPS,
                0,
                true,
            )?,
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

    /// The codec on the wire.
    pub fn codec(&self) -> StreamCodec {
        self.encoder.codec()
    }

    /// Make the next frame a keyframe, for a viewer that just joined.
    pub fn force_keyframe(&mut self) {
        self.encoder.force_keyframe();
    }

    /// Offers a captured RGBA frame (`width * height * 4` bytes) at wall clock
    /// `now`. Returns the [`SurfaceFrame`]-framed bytes of every access unit
    /// the encoder finished, in order: usually this frame's, plus any late
    /// unit of an earlier frame, since every later unit references it. Empty
    /// if throttled, size-mismatched, or the encoder is still working. A
    /// sequence-space or framing limit failure is returned visibly and
    /// leaves the sender's throttle and sequence unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError::SequenceExhausted`] when this stream must be
    /// restarted instead of wrapping, or [`FrameError::PayloadTooLarge`] if
    /// the encoder output cannot be represented by the realtime wire budget.
    pub fn offer_frame(&mut self, rgba: &[u8], now: Instant) -> Result<Vec<Vec<u8>>, FrameError> {
        if self.sequence == u64::MAX {
            return Err(FrameError::SequenceExhausted);
        }
        if !self.throttle.should_encode(now) {
            return Ok(Vec::new());
        }
        let Ok(units) = self.encoder.encode(rgba) else {
            return Ok(Vec::new());
        };
        let codec = self.encoder.codec();
        let display_size = (codec != StreamCodec::H264).then(|| {
            (
                u16::try_from(self.width).unwrap_or(u16::MAX),
                u16::try_from(self.height).unwrap_or(u16::MAX),
            )
        });
        let mut sequence = self.sequence;
        let mut wire = Vec::with_capacity(units.len());
        for unit in units.into_iter().filter(|unit| !unit.data.is_empty()) {
            let frame = SurfaceFrame {
                sequence,
                keyframe: unit.keyframe,
                codec,
                display_size,
                payload: unit.data,
            };
            wire.push(frame.encode()?);
            sequence = sequence
                .checked_add(1)
                .ok_or(FrameError::SequenceExhausted)?;
        }
        if wire.is_empty() {
            return Ok(wire);
        }
        // Only consume the throttle slot after framing succeeded. A local
        // payload-limit failure must not suppress the next captured frame.
        self.throttle.mark_encoded(now);
        self.sequence = sequence;
        Ok(wire)
    }

    /// The next sequence number this sender will mint. Diagnostic.
    pub fn next_sequence(&self) -> u64 {
        self.sequence
    }
}

/// Receive half of a video stream: [`SurfaceFrame`] reframing + decode →
/// RGBA [`DecodedFrame`]s. Own one per incoming source (keyed by sender +
/// track); feed it bytes from [`VideoStreamSender::offer_frame`] via
/// [`Self::push`]. The decoder follows the codec each frame names. Sequence
/// gaps are counted via [`Self::dropped_count`].
pub struct VideoStreamReceiver {
    framer: SurfaceStreamReceiver,
    decoder: VideoDecoder,
}

impl VideoStreamReceiver {
    pub fn new() -> Self {
        Self {
            framer: SurfaceStreamReceiver::new(),
            decoder: VideoDecoder::default(),
        }
    }

    /// Pushes wire `bytes` (one or more [`SurfaceFrame`]-framed payloads,
    /// possibly split across calls) and returns every decoded RGBA frame that
    /// became available; a not-yet-decodable NALU simply yields no output.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<DecodedFrame> {
        // Malformed or oversize wire input is explicitly rejected by the
        // framer, which clears its partial state before this returns no frame.
        if self.framer.push_bytes(bytes).is_err() {
            return Vec::new();
        }
        let Ok(events) = self.framer.drain() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for ev in events {
            let ReceiverEvent::Frame(frame) = ev else {
                continue;
            };
            if frame.codec != self.decoder.codec() {
                self.decoder = VideoDecoder::new(frame.codec);
            }
            self.decoder.set_display_size(
                frame
                    .display_size
                    .map(|(w, h)| (u32::from(w), u32::from(h))),
            );
            if let Some(decoded) = self
                .decoder
                .decode_with_hint(&frame.payload, Some(frame.keyframe))
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

    /// Captured RGBA streams end to end through the shared stack, the first frame is a keyframe,
    /// and the keyframe probe tells IDR from non-IDR.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn captured_frames_stream_end_to_end_starting_with_a_keyframe() {
        captured_rgba_streams_end_to_end_through_the_shared_stack();
        first_frame_is_flagged_a_keyframe();
        keyframe_probe_detects_idr_and_rejects_non_idr();
    }

    /// A captured RGBA frame runs the full
    /// shared stack (throttle + encoder → framed bytes → reframe + decoder →
    /// RGBA) and lands as a same-sized decoded frame — the same path a chat
    /// screen-share tile runs.
    fn captured_rgba_streams_end_to_end_through_the_shared_stack() {
        const W: u32 = 128;
        const H: u32 = 96;
        let mut sender = VideoStreamSender::new(StreamCodec::Vp9, W, H, 30).expect("sender");
        let now = Instant::now();
        let wire = sender
            .offer_frame(&gradient(W, H), now)
            .expect("sequence available")
            .pop()
            .expect("throttle admits first frame + encoder emits a NALU");
        assert!(wire.len() > crate::stream::FRAME_HEADER_LEN, "framed bytes");

        let mut receiver = VideoStreamReceiver::new();
        let frames = receiver.push(&wire);
        assert_eq!(frames.len(), 1, "one frame decoded off the wire");
        assert_eq!((frames[0].width, frames[0].height), (W, H));
        assert_eq!(frames[0].rgba.len(), (W * H * 4) as usize);
    }

    /// A second frame inside the throttle interval, a wrong-sized buffer and an exhausted sequence
    /// all yield no frame (the last as a visible, non-wrapping error).
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn the_session_refuses_frames_it_must_not_send() {
        throttle_rejects_a_second_frame_within_the_interval();
        wrong_sized_buffer_yields_no_frame();
        video_sender_sequence_exhaustion_is_a_visible_non_wrapping_error();
    }

    #[allow(
        clippy::arithmetic_side_effects,
        reason = "test phase: arithmetic on small fixed test values cannot overflow"
    )]
    fn throttle_rejects_a_second_frame_within_the_interval() {
        const W: u32 = 32;
        const H: u32 = 32;
        let mut sender = VideoStreamSender::new(StreamCodec::Vp9, W, H, 30).expect("sender");
        let now = Instant::now();
        assert!(
            !sender
                .offer_frame(&gradient(W, H), now)
                .expect("sequence available")
                .is_empty()
        );
        // 1 ms later — inside the ~33 ms interval — must be throttled.
        assert!(
            sender
                .offer_frame(&gradient(W, H), now + Duration::from_millis(1))
                .expect("sequence available")
                .is_empty()
        );
        // Past the interval, accepts again: the encoder gets a second frame,
        // whose unit comes back now or, from a busy encoder, at the end.
        let mut units = 1 + sender
            .offer_frame(&gradient(W, H), now + Duration::from_millis(40))
            .expect("sequence available")
            .len();
        units += sender.encoder.finish().expect("finish").len();
        assert_eq!(units, 2, "two frames were encoded");
    }

    fn first_frame_is_flagged_a_keyframe() {
        const W: u32 = 64;
        const H: u32 = 64;
        let mut sender = VideoStreamSender::new(StreamCodec::Vp9, W, H, 30).expect("sender");
        let wire = sender
            .offer_frame(&gradient(W, H), Instant::now())
            .expect("sequence available")
            .pop()
            .expect("frame");
        let (frame, _) = SurfaceFrame::decode(&wire).expect("decode").expect("whole");
        assert!(frame.keyframe, "the first access unit is a keyframe");
        assert_eq!(frame.codec, StreamCodec::Vp9);
        assert_eq!(frame.display_size, Some((64, 64)));
        assert_eq!(frame.sequence, 0);
    }

    fn wrong_sized_buffer_yields_no_frame() {
        let mut sender = VideoStreamSender::new(StreamCodec::Vp9, 32, 32, 30).expect("sender");
        // RGB (3 bytes/px), not RGBA — size mismatch yields no frame.
        assert!(
            sender
                .offer_frame(&vec![0u8; 32 * 32 * 3], Instant::now())
                .expect("sequence available")
                .is_empty()
        );
    }

    fn keyframe_probe_detects_idr_and_rejects_non_idr() {
        // 4-byte start code + nal_unit_type 5 (IDR).
        assert!(is_h264_keyframe(&[0, 0, 0, 1, 0x65]));
        // 3-byte start code + nal_unit_type 1 (non-IDR slice).
        assert!(!is_h264_keyframe(&[0, 0, 1, 0x41]));
        // No start code.
        assert!(!is_h264_keyframe(&[0xAA, 0xBB, 0xCC]));
    }

    fn video_sender_sequence_exhaustion_is_a_visible_non_wrapping_error() {
        let mut sender = VideoStreamSender::new(StreamCodec::Vp9, 32, 32, 30).expect("sender");
        sender.sequence = u64::MAX;
        let now = Instant::now();
        assert_eq!(
            sender.offer_frame(&gradient(32, 32), now),
            Err(FrameError::SequenceExhausted)
        );
        assert_eq!(sender.next_sequence(), u64::MAX);
        // The rejected call did not consume a throttle slot.
        sender.sequence = 0;
        assert!(
            !sender
                .offer_frame(&gradient(32, 32), now)
                .expect("sequence reset for test")
                .is_empty()
        );
    }
}
