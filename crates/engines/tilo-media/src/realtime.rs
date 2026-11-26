//! Real-time encode and decode, next to playback: a frame goes in the moment it
//! was captured, compressed bytes come out as soon as the encoder has them, and
//! the receiver decodes each access unit as it arrives.
//!
//! [`RealtimeEncoder`] takes one frame per call and supports forcing a keyframe
//! and changing the bitrate while running; [`RealtimeDecoder`] takes one access
//! unit per call and hands back the picture in the memory the caller asked for.
//! Both pick from [`crate::hw::realtime`]'s tables — hardware first, then
//! royalty-free software, then Cisco's `openh264` — and construction proves the
//! choice by running a frame through it.
//!
//! [`capture`] feeds a call. [`CodecCapabilities`] and [`negotiate`] decide which
//! codec peers share. The types and the negotiation always compile; the
//! `GStreamer` implementation needs `--features realtime`.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

pub use crate::hw::realtime::{CodecElement, CodecFamily, CodecTier, RealtimePolicy, StreamCodec};

pub mod capture;
#[cfg(gst_realtime)]
mod gst_impl;
#[cfg(web_page)]
mod web_decoder;
#[cfg(web_page)]
pub use web_decoder::RealtimeDecoder;

#[cfg(all(gst_realtime, unix))]
pub use gst_impl::upload_to_dmabuf;
#[cfg(gst_realtime)]
pub use gst_impl::{RealtimeDecoder, RealtimeEncoder, local_capabilities, rescan_registry};

/// Why a real-time encoder or decoder could not be built or run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealtimeError {
    NotCompiled,
    /// No element for `codec` is installed, or every installed one failed its
    /// start-up probe. `tried` lists each attempt and why it failed.
    Unavailable {
        codec: StreamCodec,
        tried: Vec<String>,
    },
    InvalidConfig(String),
    /// The input does not match the configured format or size.
    InvalidInput(String),
    Pipeline(String),
}

impl std::fmt::Display for RealtimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RealtimeError::NotCompiled => {
                write!(f, "real-time media is not compiled into this build")
            }
            RealtimeError::Unavailable { codec, tried } => {
                if tried.is_empty() {
                    write!(f, "no {} element is installed", codec.wire_label())
                } else {
                    write!(
                        f,
                        "no working {} element: {}",
                        codec.wire_label(),
                        tried.join("; ")
                    )
                }
            }
            RealtimeError::InvalidConfig(m) => write!(f, "invalid configuration: {m}"),
            RealtimeError::InvalidInput(m) => write!(f, "invalid input: {m}"),
            RealtimeError::Pipeline(m) => write!(f, "pipeline error: {m}"),
        }
    }
}

impl std::error::Error for RealtimeError {}

pub const MAX_DIMENSION: u32 = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputFormat {
    Rgba,
    Bgra,
    Bgrx,
    Nv12,
    P010,
}

impl InputFormat {
    pub fn gst_format(self) -> &'static str {
        match self {
            InputFormat::Rgba => "RGBA",
            InputFormat::Bgra => "BGRA",
            InputFormat::Bgrx => "BGRx",
            InputFormat::Nv12 => "NV12",
            InputFormat::P010 => "P010_10LE",
        }
    }

    pub fn frame_len(self, width: u32, height: u32) -> Option<usize> {
        let w = usize::try_from(width).ok()?;
        let h = usize::try_from(height).ok()?;
        let pixels = w.checked_mul(h)?;
        match self {
            InputFormat::Rgba | InputFormat::Bgra | InputFormat::Bgrx => pixels.checked_mul(4),
            InputFormat::Nv12 => pixels.checked_add(pixels.checked_div(2)?),
            InputFormat::P010 => pixels.checked_add(pixels.checked_div(2)?)?.checked_mul(2),
        }
    }
}

/// The stream's colour signalling, as CICP code points (ITU-T H.273). Carried
/// into the encoder's caps so the bitstream says what the samples are, and read
/// back from the decoder's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamColor {
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    pub full_range: bool,
}

impl StreamColor {
    pub const BT709: StreamColor = StreamColor {
        primaries: 1,
        transfer: 1,
        matrix: 1,
        full_range: false,
    };

    pub const BT2100_PQ: StreamColor = StreamColor {
        primaries: 9,
        transfer: 16,
        matrix: 9,
        full_range: false,
    };

    pub fn is_hdr(self) -> bool {
        matches!(self.transfer, 16 | 18)
    }
}

impl Default for StreamColor {
    fn default() -> Self {
        Self::BT709
    }
}

/// HDR10 static metadata (SMPTE ST 2086 + CTA-861.3), carried into the encoder's
/// caps as `mastering-display-info` and `content-light-level`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HdrStaticMetadata {
    pub display_primaries: [(u16, u16); 3],
    pub white_point: (u16, u16),
    pub max_luminance: u32,
    pub min_luminance: u32,
    pub max_cll: u16,
    pub max_fall: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EncoderConfig {
    pub codec: StreamCodec,
    pub width: u32,
    pub height: u32,
    /// Nominal frame rate for rate control; each frame carries its own timestamp.
    pub fps: u32,
    pub bitrate_bps: u64,
    /// Frames between periodic keyframes. `0` means only the first frame and those
    /// asked for with [`RealtimeEncoder::force_keyframe`].
    pub keyframe_interval: u32,
    pub input: InputFormat,
    /// Offer a dmabuf import path, honoured only when the chosen encoder can import
    /// a dmabuf without a CPU copy; check [`RealtimeEncoder::accepts_dmabuf`].
    pub dmabuf_input: bool,
    pub color: StreamColor,
    /// HDR10 static metadata, written with keyframes. `None` for SDR.
    pub hdr: Option<HdrStaticMetadata>,
    pub screen_content: bool,
    pub policy: RealtimePolicy,
    /// Encode at this size rather than the input's, scaling each frame first (on the
    /// GPU where the VA-API path converts, else in `videoscale`). Both dimensions must
    /// be even.
    pub output_size: Option<(u32, u32)>,
}

impl EncoderConfig {
    pub fn new(codec: StreamCodec, width: u32, height: u32, fps: u32) -> Self {
        Self {
            codec,
            width,
            height,
            fps,
            bitrate_bps: 4_000_000,
            keyframe_interval: 0,
            input: InputFormat::Rgba,
            dmabuf_input: false,
            color: StreamColor::BT709,
            hdr: None,
            screen_content: false,
            policy: RealtimePolicy::encoder_from_env(),
            output_size: None,
        }
    }

    /// Every encoder in the tables works on 4:2:0 input, so both dimensions must be
    /// even.
    ///
    /// # Errors
    ///
    /// [`RealtimeError::InvalidConfig`] naming the problem.
    pub fn validate(&self) -> Result<(), RealtimeError> {
        if self.width == 0 || self.height == 0 {
            return Err(RealtimeError::InvalidConfig(format!(
                "{}x{} has a zero dimension",
                self.width, self.height
            )));
        }
        if self.width > MAX_DIMENSION || self.height > MAX_DIMENSION {
            return Err(RealtimeError::InvalidConfig(format!(
                "{}x{} exceeds {MAX_DIMENSION}",
                self.width, self.height
            )));
        }
        if !self.width.is_multiple_of(2) || !self.height.is_multiple_of(2) {
            return Err(RealtimeError::InvalidConfig(format!(
                "{}x{} is odd; 4:2:0 encoders need even dimensions",
                self.width, self.height
            )));
        }
        if self.input == InputFormat::P010 && self.codec == StreamCodec::H264 {
            return Err(RealtimeError::InvalidConfig(
                "10-bit input cannot be carried by the H.264 path".into(),
            ));
        }
        Ok(())
    }
}

pub enum InputFrame<'a> {
    Packed(&'a [u8]),
    #[cfg(unix)]
    Dmabuf(DmabufInput<'a>),
}

#[cfg(unix)]
pub struct DmabufInput<'a> {
    pub planes: &'a [DmabufInputPlane<'a>],
    pub fourcc: u32,
    pub modifier: u64,
    pub width: u32,
    pub height: u32,
}

#[cfg(unix)]
pub struct DmabufInputPlane<'a> {
    pub fd: std::os::fd::BorrowedFd<'a>,
    pub offset: u32,
    pub stride: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedUnit {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub pts: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeOutput {
    Rgba,
    P010,
    Native,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecoderConfig {
    pub codec: StreamCodec,
    pub output: DecodeOutput,
    pub policy: RealtimePolicy,
    pub display_size: Option<(u32, u32)>,
}

impl DecoderConfig {
    pub fn new(codec: StreamCodec) -> Self {
        Self {
            codec,
            output: DecodeOutput::Rgba,
            policy: RealtimePolicy::decoder_from_env(),
            display_size: None,
        }
    }
}

/// Crop `frame` to at most `width` x `height`, keeping the top-left corner.
/// Planar and dmabuf pictures keep their buffers (a smaller width and height over
/// the same strides is a crop); packed RGBA is repacked because its rows are
/// tight. A picture already within the size is returned unchanged.
pub fn crop_frame(frame: crate::Frame, width: u32, height: u32) -> crate::Frame {
    use crate::frame::FramePayload;
    use std::sync::Arc;

    let w = frame.width.min(width);
    let h = frame.height.min(height);
    if w == frame.width && h == frame.height {
        return frame;
    }
    let payload = match frame.payload {
        FramePayload::Rgba(bytes) => {
            let (Ok(src_row), Ok(dst_row), Ok(rows)) = (
                usize::try_from(frame.width).map(|v| v.saturating_mul(4)),
                usize::try_from(w).map(|v| v.saturating_mul(4)),
                usize::try_from(h),
            ) else {
                return crate::Frame {
                    payload: FramePayload::Rgba(bytes),
                    ..frame
                };
            };
            let mut out = Vec::with_capacity(dst_row.saturating_mul(rows));
            for row in bytes.chunks_exact(src_row.max(1)).take(rows) {
                out.extend_from_slice(row.get(..dst_row).unwrap_or(row));
            }
            FramePayload::Rgba(Arc::new(out))
        }
        FramePayload::Planar(image) => {
            let mut image = Arc::unwrap_or_clone(image);
            for (i, plane) in image.planes.iter_mut().enumerate() {
                let (pw, ph) = if i == 0 {
                    (w, h)
                } else {
                    (w.div_ceil(2), h.div_ceil(2))
                };
                plane.width = plane.width.min(pw);
                plane.height = plane.height.min(ph);
            }
            FramePayload::Planar(Arc::new(image))
        }
        #[cfg(unix)]
        FramePayload::Dmabuf(image) => match Arc::try_unwrap(image) {
            Ok(mut image) => {
                image.width = w;
                image.height = h;
                FramePayload::Dmabuf(Arc::new(image))
            }
            // Shared: leave the buffer's own description alone; the frame's size below
            // says what to show.
            Err(shared) => FramePayload::Dmabuf(shared),
        },
    };
    crate::Frame {
        width: w,
        height: h,
        pts_ms: frame.pts_ms,
        payload,
    }
}

/// Run on a media thread each time a [`RealtimeDecoder`] has a picture out, an
/// element failed, or a candidate finished starting, so a loop that does not wait
/// for pictures is woken to collect it with
/// [`RealtimeDecoder::take_late_picture`]. It must not block: a ping, not the work.
pub type PictureWake = std::sync::Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Debug)]
pub struct DecodedPicture {
    pub frame: crate::Frame,
    pub color: StreamColor,
}

/// What a machine can encode and decode, split by whether hardware does it.
/// Exchanged between peers as wire labels, so a sender can pick a codec every
/// receiver can take.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodecCapabilities {
    pub hardware_encode: BTreeSet<StreamCodec>,
    pub hardware_decode: BTreeSet<StreamCodec>,
    pub software_encode: BTreeSet<StreamCodec>,
    pub software_decode: BTreeSet<StreamCodec>,
    /// The largest picture each hardware decoder takes, where it has a limit (the
    /// Hantro G1 on an RK3566 stops at 1920x1088 for H.264). A sender encodes no
    /// larger, so a receiver never falls back to software decoding. A codec missing
    /// here has no known limit.
    pub hardware_decode_max: BTreeMap<StreamCodec, (u32, u32)>,
}

impl CodecCapabilities {
    /// What a peer that never advertised capabilities is assumed to handle: H.264,
    /// which every release of the protocol has carried.
    pub fn legacy_peer() -> Self {
        Self {
            software_decode: BTreeSet::from([StreamCodec::H264]),
            ..Self::default()
        }
    }

    pub fn can_encode(&self, codec: StreamCodec) -> bool {
        self.hardware_encode.contains(&codec) || self.software_encode.contains(&codec)
    }

    pub fn can_decode(&self, codec: StreamCodec) -> bool {
        self.hardware_decode.contains(&codec) || self.software_decode.contains(&codec)
    }

    pub fn labels(set: &BTreeSet<StreamCodec>) -> Vec<String> {
        set.iter().map(|c| c.wire_label().to_string()).collect()
    }

    /// Parse wire labels, skipping ones this build does not know.
    pub fn parse_labels<S: AsRef<str>>(labels: &[S]) -> BTreeSet<StreamCodec> {
        labels
            .iter()
            .filter_map(|l| StreamCodec::from_wire_label(l.as_ref()))
            .collect()
    }
}

/// What the sender should use, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Negotiated {
    pub codec: StreamCodec,
    /// Whether the codec was chosen because both ends run it in hardware.
    pub hardware_both_ends: bool,
}

/// Pick the SDR codec for one sender and a set of receivers: AV1, then VP9, when
/// the sender encodes it in hardware and every receiver decodes it in hardware;
/// otherwise H.264 when the sender can encode it at all (hardware, or Cisco's
/// openh264); with no H.264 encoder, a royalty-free codec every receiver can
/// decode. `None` when the sender can produce nothing a receiver understands.
///
/// Receivers that never advertised capabilities are passed as
/// [`CodecCapabilities::legacy_peer`].
pub fn negotiate(local: &CodecCapabilities, receivers: &[CodecCapabilities]) -> Option<Negotiated> {
    for codec in [StreamCodec::Av1, StreamCodec::Vp9] {
        if local.hardware_encode.contains(&codec)
            && receivers.iter().all(|r| r.hardware_decode.contains(&codec))
        {
            return Some(Negotiated {
                codec,
                hardware_both_ends: true,
            });
        }
    }
    if local.can_encode(StreamCodec::H264)
        && receivers.iter().all(|r| r.can_decode(StreamCodec::H264))
    {
        return Some(Negotiated {
            codec: StreamCodec::H264,
            hardware_both_ends: local.hardware_encode.contains(&StreamCodec::H264)
                && receivers
                    .iter()
                    .all(|r| r.hardware_decode.contains(&StreamCodec::H264)),
        });
    }
    for codec in [StreamCodec::Vp9, StreamCodec::Av1] {
        if local.can_encode(codec) && receivers.iter().all(|r| r.can_decode(codec)) {
            return Some(Negotiated {
                codec,
                hardware_both_ends: false,
            });
        }
    }
    None
}

/// Pick a 10-bit codec for an HDR stream: HEVC Main10 or AV1, which the sender
/// must encode in hardware (there is no software HEVC encoder, and software AV1 at
/// 10 bits is too slow for real time) and every receiver must decode. `None` means
/// share in SDR.
pub fn negotiate_hdr(
    local: &CodecCapabilities,
    receivers: &[CodecCapabilities],
) -> Option<Negotiated> {
    for codec in [StreamCodec::Av1, StreamCodec::HevcMain10] {
        if local.hardware_encode.contains(&codec) && receivers.iter().all(|r| r.can_decode(codec)) {
            return Some(Negotiated {
                codec,
                hardware_both_ends: receivers.iter().all(|r| r.hardware_decode.contains(&codec)),
            });
        }
    }
    None
}

/// Whether `data` begins a random-access point for `codec`: a receiver joining
/// mid-stream cannot decode what came before. Unknown or malformed input reads as
/// "not a keyframe".
pub fn is_keyframe(codec: StreamCodec, data: &[u8]) -> bool {
    match codec {
        StreamCodec::H264 => annexb_nal_types(data).any(|h| (h & 0x1f) == 5),
        StreamCodec::HevcMain10 => {
            annexb_nal_types(data).any(|h| (16..=21).contains(&((h >> 1) & 0x3f)))
        }
        StreamCodec::Vp9 => vp9_is_keyframe(data),
        StreamCodec::Av1 => av1_has_sequence_header(data),
    }
}

fn annexb_nal_types(data: &[u8]) -> impl Iterator<Item = u8> + '_ {
    let mut i: usize = 0;
    std::iter::from_fn(move || {
        while let Some(window) = data.get(i..).and_then(|s| s.first_chunk::<3>()) {
            if *window == [0, 0, 1] {
                let header = data.get(i.checked_add(3)?).copied();
                i = i.saturating_add(3);
                if header.is_some() {
                    return header;
                }
                return None;
            }
            i = i.saturating_add(1);
        }
        None
    })
}

fn vp9_is_keyframe(data: &[u8]) -> bool {
    let Some(&b) = data.first() else {
        return false;
    };
    if b >> 6 != 0b10 {
        return false;
    }
    let profile_low = (b >> 5) & 1;
    let profile_high = (b >> 4) & 1;
    let profile = (profile_high << 1) | profile_low;
    let shift_base: u8 = if profile == 3 { 2 } else { 3 };
    let show_existing = (b >> shift_base) & 1;
    let Some(frame_type_shift) = shift_base.checked_sub(1) else {
        return false;
    };
    let frame_type = (b >> frame_type_shift) & 1;
    show_existing == 0 && frame_type == 0
}

fn av1_has_sequence_header(data: &[u8]) -> bool {
    const OBU_SEQUENCE_HEADER: u8 = 1;
    let mut i: usize = 0;
    while let Some(&header) = data.get(i) {
        let obu_type = (header >> 3) & 0x0f;
        let has_extension = (header >> 2) & 1 == 1;
        let has_size = (header >> 1) & 1 == 1;
        if obu_type == OBU_SEQUENCE_HEADER {
            return true;
        }
        if !has_size {
            return false;
        }
        let mut pos = i.saturating_add(1);
        if has_extension {
            pos = pos.saturating_add(1);
        }
        let Some((size, used)) = leb128(data.get(pos..).unwrap_or_default()) else {
            return false;
        };
        let Some(next) = pos
            .checked_add(used)
            .and_then(|p| p.checked_add(usize::try_from(size).ok()?))
        else {
            return false;
        };
        i = next;
    }
    false
}

fn leb128(data: &[u8]) -> Option<(u64, usize)> {
    let mut value: u64 = 0;
    for (i, &byte) in data.iter().take(8).enumerate() {
        let shift = u32::try_from(i).ok()?.checked_mul(7)?;
        value |= u64::from(byte & 0x7f).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some((value, i.checked_add(1)?));
        }
    }
    None
}

// Real-time media without `GStreamer`: every constructor says so.
#[cfg(not(gst_realtime))]
mod unavailable {
    use super::{CodecCapabilities, EncodedUnit, EncoderConfig, InputFrame, RealtimeError};
    #[cfg(not(web_page))]
    use super::{DecodedPicture, DecoderConfig};
    use crate::hw::realtime::CodecElement;
    use std::time::Duration;

    // No value of this type exists: the placeholders below can never be constructed,
    // which the compiler then knows.
    #[derive(Debug)]
    enum Never {}

    #[derive(Debug)]
    pub struct RealtimeEncoder {
        never: Never,
    }

    impl RealtimeEncoder {
        pub fn new(_config: &EncoderConfig) -> Result<Self, RealtimeError> {
            Err(RealtimeError::NotCompiled)
        }

        pub fn element(&self) -> &'static CodecElement {
            match self.never {}
        }

        pub fn accepts_dmabuf(&self) -> bool {
            false
        }

        pub fn encode(
            &mut self,
            _frame: InputFrame<'_>,
            _pts: Duration,
        ) -> Result<Vec<EncodedUnit>, RealtimeError> {
            Err(RealtimeError::NotCompiled)
        }

        /// # Errors
        ///
        /// Never returns: no encoder exists to finish.
        pub fn finish(self) -> Result<Vec<EncodedUnit>, RealtimeError> {
            match self.never {}
        }

        pub fn force_keyframe(&mut self) {}

        pub fn set_bitrate(&mut self, _bits_per_second: u64) {}
    }

    #[cfg(not(web_page))]
    #[derive(Debug)]
    pub struct RealtimeDecoder {
        never: Never,
    }

    #[cfg(not(web_page))]
    impl RealtimeDecoder {
        pub fn new(_config: DecoderConfig) -> Result<Self, RealtimeError> {
            Err(RealtimeError::NotCompiled)
        }

        pub fn new_without_waiting(_config: DecoderConfig) -> Result<Self, RealtimeError> {
            Err(RealtimeError::NotCompiled)
        }

        pub fn startup_deadline(&self) -> Option<std::time::Instant> {
            match self.never {}
        }

        pub fn element(&self) -> Option<&'static CodecElement> {
            match self.never {}
        }

        pub fn set_display_size(&mut self, _size: Option<(u32, u32)>) {}

        pub fn set_stream_color(&mut self, _color: Option<super::StreamColor>) {}

        pub fn set_picture_wait(&mut self, _wait: Duration) {}

        pub fn set_picture_wake(&mut self, _wake: Option<super::PictureWake>) {}

        pub fn decode(
            &mut self,
            _unit: &[u8],
            _keyframe_hint: Option<bool>,
        ) -> Result<Option<DecodedPicture>, RealtimeError> {
            Err(RealtimeError::NotCompiled)
        }

        pub fn picture_pending(&self) -> bool {
            match self.never {}
        }

        /// # Errors
        ///
        /// Never returns: no decoder exists to drain.
        pub fn take_late_picture(&mut self) -> Result<Option<DecodedPicture>, RealtimeError> {
            match self.never {}
        }
    }

    pub fn local_capabilities() -> CodecCapabilities {
        CodecCapabilities::default()
    }

    pub fn rescan_registry() {}
}

#[cfg(all(not(gst_realtime), not(web_page)))]
pub use unavailable::RealtimeDecoder;
#[cfg(not(gst_realtime))]
pub use unavailable::{RealtimeEncoder, local_capabilities, rescan_registry};

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(
        hw_enc: &[StreamCodec],
        hw_dec: &[StreamCodec],
        sw: &[StreamCodec],
    ) -> CodecCapabilities {
        CodecCapabilities {
            hardware_encode: hw_enc.iter().copied().collect(),
            hardware_decode: hw_dec.iter().copied().collect(),
            software_encode: sw.iter().copied().collect(),
            software_decode: sw.iter().copied().collect(),
            hardware_decode_max: BTreeMap::new(),
        }
    }

    use StreamCodec::{Av1, H264, HevcMain10, Vp9};

    #[test]
    fn realtime_codec_negotiation_respects_every_end() {
        {
            let sender = caps(&[Av1, Vp9, H264], &[Av1, Vp9, H264], &[H264]);
            let rx_hw = caps(&[], &[Av1, Vp9, H264], &[H264]);
            let got = negotiate(&sender, &[rx_hw.clone(), rx_hw.clone()]).unwrap();
            assert_eq!(got.codec, Av1);
            assert!(got.hardware_both_ends);

            // One receiver decodes AV1 only in software: AV1 is off the table.
            let rx_sw_av1 = caps(&[], &[Vp9, H264], &[Av1, H264]);
            let got = negotiate(&sender, &[rx_hw, rx_sw_av1]).unwrap();
            assert_eq!(got.codec, Vp9, "VP9 is hardware at every end");
        }
        {
            let sender = caps(&[Av1, Vp9], &[Av1, Vp9], &[H264]);
            let got = negotiate(&sender, &[CodecCapabilities::legacy_peer()]).unwrap();
            assert_eq!(got.codec, H264);
            assert!(!got.hardware_both_ends);
        }
        {
            let sender = CodecCapabilities {
                hardware_encode: [Av1].into(),
                hardware_decode: [Av1, Vp9].into(),
                software_encode: [Vp9, Av1].into(),
                software_decode: [Vp9, Av1].into(),
                hardware_decode_max: BTreeMap::new(),
            };
            let receiver = CodecCapabilities {
                software_decode: [Vp9, Av1].into(),
                ..CodecCapabilities::default()
            };
            assert_eq!(negotiate(&sender, &[receiver]).unwrap().codec, Vp9);
            assert_eq!(
                negotiate(&sender, &[CodecCapabilities::legacy_peer()]),
                None,
                "nothing a legacy peer can decode"
            );
        }
        {
            let sender = caps(&[Vp9], &[], &[H264]);
            assert_eq!(negotiate(&sender, &[]).unwrap().codec, Vp9);
        }
        {
            let sender = caps(&[HevcMain10], &[HevcMain10], &[]);
            let rx = caps(&[], &[], &[HevcMain10]);
            assert_eq!(
                negotiate_hdr(&sender, std::slice::from_ref(&rx))
                    .unwrap()
                    .codec,
                HevcMain10
            );
            assert_eq!(negotiate_hdr(&caps(&[], &[], &[Av1]), &[rx]), None);
        }
        {
            let parsed = CodecCapabilities::parse_labels(&["av1", "h266", "h264"]);
            assert_eq!(parsed, BTreeSet::from([Av1, H264]));
            assert_eq!(CodecCapabilities::labels(&parsed), ["h264", "av1"]);
        }
    }

    #[test]
    fn realtime_bitstreams_and_frame_layouts_are_read_exactly() {
        {
            assert!(is_keyframe(H264, &[0, 0, 0, 1, 0x67, 0, 0, 1, 0x65]));
            assert!(!is_keyframe(H264, &[0, 0, 1, 0x41]));
            assert!(!is_keyframe(H264, &[0xAA]));
            assert!(is_keyframe(HevcMain10, &[0, 0, 0, 1, 0x26, 0x01]));
            assert!(!is_keyframe(HevcMain10, &[0, 0, 0, 1, 0x02, 0x01]));
            assert!(is_keyframe(Vp9, &[0x82, 0x49, 0x83, 0x42]));
            assert!(!is_keyframe(Vp9, &[0x86, 0x00]));
            assert!(is_keyframe(Av1, &[0x12, 0x00, 0x0a, 0x01, 0x00]));
            assert!(!is_keyframe(Av1, &[0x12, 0x00, 0x32, 0x01, 0x00]));
            assert!(!is_keyframe(Av1, &[0x12, 0x85]), "truncated size");
        }
        {
            let mut c = EncoderConfig::new(H264, 1280, 720, 30);
            assert!(c.validate().is_ok());
            c.width = 1281;
            assert!(matches!(c.validate(), Err(RealtimeError::InvalidConfig(_))));
            c.width = 0;
            assert!(c.validate().is_err());
            let mut c = EncoderConfig::new(H264, 64, 64, 30);
            c.input = InputFormat::P010;
            assert!(c.validate().is_err(), "H.264 is 8-bit only here");
        }
        {
            use crate::frame::{FramePayload, PlanarFormat, PlanarImage, PlaneData};
            use std::sync::Arc;
            let mut rgba = Vec::new();
            for y in 0..3u8 {
                for x in 0..4u8 {
                    rgba.extend_from_slice(&[x, y, 0, 255]);
                }
            }
            let frame = crate::Frame {
                width: 4,
                height: 3,
                pts_ms: 7,
                payload: FramePayload::Rgba(Arc::new(rgba)),
            };
            let cropped = crop_frame(frame, 3, 2);
            assert_eq!((cropped.width, cropped.height, cropped.pts_ms), (3, 2, 7));
            let FramePayload::Rgba(bytes) = &cropped.payload else {
                panic!("still RGBA")
            };
            assert_eq!(
                bytes.as_slice(),
                &[
                    0, 0, 0, 255, 1, 0, 0, 255, 2, 0, 0, 255, 0, 1, 0, 255, 1, 1, 0, 255, 2, 1, 0,
                    255
                ]
            );

            let plane = |w, h| PlaneData {
                bytes: Arc::new(vec![0; 64]),
                stride: 8,
                width: w,
                height: h,
            };
            let planar = crate::Frame {
                width: 8,
                height: 6,
                pts_ms: 0,
                payload: FramePayload::Planar(Arc::new(PlanarImage {
                    format: PlanarFormat::Nv12,
                    planes: vec![plane(8, 6), plane(4, 3)],
                    colorimetry: crate::dmabuf::Colorimetry::BT709_LIMITED,
                })),
            };
            let cropped = crop_frame(planar, 6, 4);
            let p = cropped.planar().expect("planar");
            assert_eq!((p.planes[0].width, p.planes[0].height), (6, 4));
            assert_eq!((p.planes[1].width, p.planes[1].height), (3, 2));
            assert_eq!(p.planes[0].stride, 8, "a crop keeps the stride");

            let small = crate::Frame {
                width: 2,
                height: 2,
                pts_ms: 0,
                payload: FramePayload::Rgba(Arc::new(vec![1; 16])),
            };
            assert_eq!(crop_frame(small, 8, 8).width, 2);
        }
        {
            assert_eq!(InputFormat::Rgba.frame_len(4, 2), Some(32));
            assert_eq!(InputFormat::Nv12.frame_len(4, 2), Some(12));
            assert_eq!(InputFormat::P010.frame_len(4, 2), Some(24));
            assert_eq!(InputFormat::Rgba.frame_len(u32::MAX, u32::MAX), None);
        }
    }
}
