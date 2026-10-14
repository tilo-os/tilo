//! Real-time encode and decode, next to playback.
//!
//! Playback ([`crate::MediaEngine`]) owns a clock and paces frames to it. Screen
//! sharing, calls and recording need the opposite: a frame goes in the moment
//! it was captured, compressed bytes come out as soon as the encoder has them,
//! and the receiver decodes each access unit the moment it arrives. This
//! module is that path:
//!
//! * [`RealtimeEncoder`] takes one frame per call (RGBA/BGRA/NV12/P010 in
//!   system memory, or a compositor dmabuf that a VA-API pipeline imports
//!   without a copy), and supports forcing a keyframe and changing the bitrate
//!   while running.
//! * [`RealtimeDecoder`] takes one access unit per call and hands back the
//!   picture in the memory the caller asked for.
//!
//! Both pick their element from [`crate::hw::realtime`]'s tables:
//! hardware first, then royalty-free software, then Cisco's `openh264` binary
//! loaded from the system. Construction proves the choice by running a frame
//! through it, and falls through to the next candidate when a listed element
//! is present but cannot actually run, so a selection that reads as hardware
//! has been observed to work.
//!
//! [`CodecCapabilities`] and [`negotiate`] decide which codec two or more
//! peers should use: AV1, then VP9, when the sender can encode it in hardware
//! and every receiver can decode it in hardware; otherwise H.264.
//!
//! The types and the negotiation are always compiled. The `GStreamer`
//! implementation needs `--features realtime`; without it construction
//! returns [`RealtimeError::NotCompiled`].

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

pub use crate::hw::realtime::{CodecElement, CodecFamily, CodecTier, RealtimePolicy, StreamCodec};

#[cfg(gst_realtime)]
mod gst_impl;

#[cfg(all(gst_realtime, unix))]
pub use gst_impl::upload_to_dmabuf;
#[cfg(gst_realtime)]
pub use gst_impl::{RealtimeDecoder, RealtimeEncoder, local_capabilities, rescan_registry};

/// Why a real-time encoder or decoder could not be built or run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealtimeError {
    /// This build carries no `GStreamer` (`--features realtime` is off).
    NotCompiled,
    /// No element for `codec` is installed, or every installed one failed its
    /// start-up probe. `tried` lists each attempt and why it failed.
    Unavailable {
        codec: StreamCodec,
        tried: Vec<String>,
    },
    /// The caller's configuration cannot be encoded (zero or odd size,
    /// dimensions past the ceiling).
    InvalidConfig(String),
    /// The input does not match the configured format or size.
    InvalidInput(String),
    /// The pipeline reported an error while running.
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

/// Largest width or height the real-time path accepts.
pub const MAX_DIMENSION: u32 = 8192;

/// The pixel layout of frames handed to [`RealtimeEncoder`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputFormat {
    /// 8-bit RGBA, tightly packed.
    Rgba,
    /// 8-bit BGRA, tightly packed (DRM `AR24`, what most compositors render).
    Bgra,
    /// 8-bit `BGRx`, tightly packed (DRM `XR24`).
    Bgrx,
    /// 8-bit 4:2:0, Y plane then interleaved `CbCr`.
    Nv12,
    /// 10-bit 4:2:0 in 16-bit little-endian containers, samples in the high
    /// bits. The HDR input.
    P010,
}

impl InputFormat {
    /// The `GStreamer` format string.
    pub fn gst_format(self) -> &'static str {
        match self {
            InputFormat::Rgba => "RGBA",
            InputFormat::Bgra => "BGRA",
            InputFormat::Bgrx => "BGRx",
            InputFormat::Nv12 => "NV12",
            InputFormat::P010 => "P010_10LE",
        }
    }

    /// The exact byte length of one tightly packed frame, or `None` when the
    /// size overflows.
    pub fn frame_len(self, width: u32, height: u32) -> Option<usize> {
        let w = usize::try_from(width).ok()?;
        let h = usize::try_from(height).ok()?;
        let pixels = w.checked_mul(h)?;
        match self {
            InputFormat::Rgba | InputFormat::Bgra | InputFormat::Bgrx => pixels.checked_mul(4),
            // Luma plus half as much interleaved chroma.
            InputFormat::Nv12 => pixels.checked_add(pixels.checked_div(2)?),
            InputFormat::P010 => pixels.checked_add(pixels.checked_div(2)?)?.checked_mul(2),
        }
    }
}

/// The colour signalling for a stream, as CICP code points (ITU-T H.273).
///
/// Carried into the encoder's caps so the bitstream's VUI / colour config says
/// what the samples are, and read back from the decoder's caps on the other
/// side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamColor {
    /// `ColourPrimaries`: 1 = BT.709, 9 = BT.2020, 12 = Display P3.
    pub primaries: u8,
    /// `TransferCharacteristics`: 1 = BT.709, 13 = sRGB, 16 = PQ, 18 = HLG.
    pub transfer: u8,
    /// `MatrixCoefficients`: 0 = identity, 1 = BT.709, 9 = BT.2020 NCL.
    pub matrix: u8,
    /// Full-range samples rather than limited ("TV") range.
    pub full_range: bool,
}

impl StreamColor {
    /// SDR BT.709, limited range. What every stream is assumed to be when it
    /// says nothing.
    pub const BT709: StreamColor = StreamColor {
        primaries: 1,
        transfer: 1,
        matrix: 1,
        full_range: false,
    };

    /// HDR10: BT.2020 primaries and matrix, PQ transfer, limited range.
    pub const BT2100_PQ: StreamColor = StreamColor {
        primaries: 9,
        transfer: 16,
        matrix: 9,
        full_range: false,
    };

    /// Whether the transfer is an HDR one (PQ or HLG).
    pub fn is_hdr(self) -> bool {
        matches!(self.transfer, 16 | 18)
    }
}

impl Default for StreamColor {
    fn default() -> Self {
        Self::BT709
    }
}

/// HDR10 static metadata (SMPTE ST 2086 + CTA-861.3), in the units the
/// standards define. Carried into the encoder's caps as `mastering-display-info`
/// and `content-light-level`, which `GStreamer` encoders write as SEI / metadata
/// OBUs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HdrStaticMetadata {
    /// Display primaries `[(x, y); 3]` in 0.00002 units, order G, B, R.
    pub display_primaries: [(u16, u16); 3],
    /// White point `(x, y)` in 0.00002 units.
    pub white_point: (u16, u16),
    /// Maximum mastering luminance in 0.0001 cd/m².
    pub max_luminance: u32,
    /// Minimum mastering luminance in 0.0001 cd/m².
    pub min_luminance: u32,
    /// Maximum content light level, cd/m².
    pub max_cll: u16,
    /// Maximum frame-average light level, cd/m².
    pub max_fall: u16,
}

/// How a [`RealtimeEncoder`] is set up.
#[derive(Clone, Debug, PartialEq)]
pub struct EncoderConfig {
    pub codec: StreamCodec,
    pub width: u32,
    pub height: u32,
    /// Nominal frame rate for rate control. Frames may still arrive at any
    /// cadence; each carries its own timestamp.
    pub fps: u32,
    /// Target bitrate in bits per second. `0` lets the encoder choose.
    pub bitrate_bps: u64,
    /// Frames between periodic keyframes. `0` means only the first frame and
    /// those asked for with [`RealtimeEncoder::force_keyframe`].
    pub keyframe_interval: u32,
    /// Layout of frames handed to [`RealtimeEncoder::encode`].
    pub input: InputFormat,
    /// Offer a dmabuf import path. Honoured only when the chosen encoder can
    /// import a dmabuf without a CPU copy; check
    /// [`RealtimeEncoder::accepts_dmabuf`].
    pub dmabuf_input: bool,
    /// Colour signalling written into the bitstream.
    pub color: StreamColor,
    /// HDR10 static metadata, written with keyframes. `None` for SDR.
    pub hdr: Option<HdrStaticMetadata>,
    /// Screen content rather than camera footage: tunes software encoders for
    /// sharp text and large static areas.
    pub screen_content: bool,
    pub policy: RealtimePolicy,
    /// Encode at this size rather than the input's: the pipeline scales each
    /// frame first (on the GPU where the VA-API path converts, else in
    /// `videoscale`). Both dimensions must be even.
    pub output_size: Option<(u32, u32)>,
}

impl EncoderConfig {
    /// A low-latency SDR configuration with the environment's policy.
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

    /// Validate dimensions. Every encoder in the tables works on 4:2:0 input,
    /// so both dimensions must be even.
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

/// A frame for [`RealtimeEncoder::encode`].
pub enum InputFrame<'a> {
    /// A tightly packed frame in the configured [`InputFormat`].
    Packed(&'a [u8]),
    /// A dmabuf from the compositor. The fds are borrowed for the call; the
    /// encoder duplicates what it keeps.
    #[cfg(unix)]
    Dmabuf(DmabufInput<'a>),
}

/// A borrowed dmabuf description.
#[cfg(unix)]
pub struct DmabufInput<'a> {
    /// One entry per plane. The same fd may back several planes.
    pub planes: &'a [DmabufInputPlane<'a>],
    /// DRM fourcc of the pixel layout.
    pub fourcc: u32,
    /// DRM format modifier.
    pub modifier: u64,
    pub width: u32,
    pub height: u32,
}

/// One plane of a [`DmabufInput`].
#[cfg(unix)]
pub struct DmabufInputPlane<'a> {
    pub fd: std::os::fd::BorrowedFd<'a>,
    pub offset: u32,
    pub stride: u32,
}

/// One compressed access unit (H.264/HEVC), frame (VP9) or temporal unit (AV1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedUnit {
    pub data: Vec<u8>,
    /// A random-access point: a receiver can start decoding here.
    pub keyframe: bool,
    /// The timestamp the frame was submitted with.
    pub pts: Duration,
}

/// What a [`RealtimeDecoder`] hands back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeOutput {
    /// Tightly packed RGBA8 in system memory, converted if needed.
    Rgba,
    /// 10-bit P010 planes in system memory, converted if needed. The HDR
    /// receive path; 8-bit streams are widened.
    P010,
    /// Whatever the decoder produces with the fewest copies: a dmabuf from a
    /// hardware decoder, else NV12/P010 planes, else RGBA.
    Native,
}

/// How a [`RealtimeDecoder`] is set up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecoderConfig {
    pub codec: StreamCodec,
    pub output: DecodeOutput,
    pub policy: RealtimePolicy,
    /// The picture size the sender announced, when the transport carries it.
    ///
    /// A decoded picture larger than this is cropped to it (top-left). Some
    /// hardware encoders code a frame padded to their own alignment without
    /// signalling the smaller display size: measured, radeonsi's AV1 encoder
    /// through `vaav1enc` turns 1920x1080 into 1920x1082 and 1366x768 into
    /// 1408x768. `None` shows whatever the bitstream says.
    pub display_size: Option<(u32, u32)>,
}

impl DecoderConfig {
    /// RGBA output under the environment's policy.
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
///
/// Planar and dmabuf pictures keep their buffers (a smaller width and height
/// over the same strides is a crop); packed RGBA is repacked because its rows
/// are tight. A picture already within the size is returned unchanged.
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
                // Plane 0 is luma; the rest are 4:2:0 chroma.
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
            // Shared: leave the buffer's own description alone; the frame's
            // size below still says what to show.
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

/// Run on a media thread each time a [`RealtimeDecoder`] has a picture
/// out (or an element failed, or a candidate finished starting), so a loop
/// that does not wait for pictures (see [`RealtimeDecoder::set_picture_wait`])
/// is woken to collect it with [`RealtimeDecoder::take_late_picture`]
/// instead of polling. It must not block: a wake-up ping, not the work
/// itself.
pub type PictureWake = std::sync::Arc<dyn Fn() + Send + Sync>;

/// One decoded picture from a [`RealtimeDecoder`].
#[derive(Clone, Debug)]
pub struct DecodedPicture {
    pub frame: crate::Frame,
    /// Colour signalling read from the decoder's output caps.
    pub color: StreamColor,
}

/// What a machine can encode and decode, split by whether hardware does it.
///
/// Exchanged between peers (as wire labels) so a sender can pick a codec every
/// receiver can take.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodecCapabilities {
    pub hardware_encode: BTreeSet<StreamCodec>,
    pub hardware_decode: BTreeSet<StreamCodec>,
    pub software_encode: BTreeSet<StreamCodec>,
    pub software_decode: BTreeSet<StreamCodec>,
    /// The largest picture each hardware decoder takes, where it has a limit
    /// (the Hantro G1 on an RK3566 stops at 1920x1088 for H.264). A sender
    /// encodes no larger, so a receiver never falls back to software
    /// decoding. A codec missing here has no known limit.
    pub hardware_decode_max: BTreeMap<StreamCodec, (u32, u32)>,
}

impl CodecCapabilities {
    /// What a peer that never advertised capabilities is assumed to handle:
    /// H.264 decode, which every release of the protocol has carried.
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

    /// Wire labels for one of the sets.
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

/// Pick the SDR codec for one sender and a set of receivers.
///
/// AV1, then VP9, when the sender encodes it in hardware and every receiver
/// decodes it in hardware. Otherwise H.264 when the sender can encode it at
/// all (hardware, or Cisco's openh264). With no H.264 encoder on the sender, a
/// royalty-free codec every receiver can decode in any way. `None` when the
/// sender can produce nothing a receiver understands.
///
/// Receivers that never advertised capabilities should be passed as
/// [`CodecCapabilities::legacy_peer`], which pins the choice to H.264.
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

/// Pick a 10-bit codec for an HDR stream: HEVC Main10 or AV1, which the
/// sender must encode in hardware (there is no software HEVC encoder, and
/// software AV1 at 10 bits is too slow for real time) and every receiver must
/// decode. `None` means share in SDR.
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

/// Whether `data` begins a random-access point for `codec`.
///
/// For a receiver that joins mid-stream: everything before the first
/// keyframe is undecodable. Unknown or malformed input reads as "not a
/// keyframe", so a late joiner waits for the next detectable one.
pub fn is_keyframe(codec: StreamCodec, data: &[u8]) -> bool {
    match codec {
        StreamCodec::H264 => annexb_nal_types(data).any(|h| (h & 0x1f) == 5),
        // IRAP pictures: BLA/IDR/CRA, NAL types 16..=21.
        StreamCodec::HevcMain10 => {
            annexb_nal_types(data).any(|h| (16..=21).contains(&((h >> 1) & 0x3f)))
        }
        StreamCodec::Vp9 => vp9_is_keyframe(data),
        StreamCodec::Av1 => av1_has_sequence_header(data),
    }
}

/// The first byte after every Annex-B start code in `data`.
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

/// VP9 uncompressed header: `frame_marker` (2 bits, `0b10`), profile bits,
/// then `show_existing_frame` and `frame_type` (0 = key frame).
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
    // Profile 3 has a reserved zero bit before `show_existing_frame`.
    let shift_base: u8 = if profile == 3 { 2 } else { 3 };
    let show_existing = (b >> shift_base) & 1;
    let Some(frame_type_shift) = shift_base.checked_sub(1) else {
        return false;
    };
    let frame_type = (b >> frame_type_shift) & 1;
    show_existing == 0 && frame_type == 0
}

/// Whether an AV1 temporal unit (OBU stream with size fields) carries a
/// sequence header. Encoders emit one with every key frame, and a decoder
/// cannot start without one.
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
            // Without a size the rest of the unit is this one OBU.
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

/// Decode an unsigned LEB128 value; returns the value and the bytes used.
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

/// Real-time media without `GStreamer`: every constructor says so.
#[cfg(not(gst_realtime))]
mod unavailable {
    use super::{
        CodecCapabilities, DecodedPicture, DecoderConfig, EncodedUnit, EncoderConfig, InputFrame,
        RealtimeError,
    };
    use crate::hw::realtime::CodecElement;
    use std::time::Duration;

    /// No value of this type exists: the placeholders below can never be
    /// constructed, which the compiler then knows.
    #[derive(Debug)]
    enum Never {}

    /// Placeholder for builds without `--features realtime`.
    #[derive(Debug)]
    pub struct RealtimeEncoder {
        never: Never,
    }

    impl RealtimeEncoder {
        /// # Errors
        ///
        /// Always [`RealtimeError::NotCompiled`].
        pub fn new(_config: &EncoderConfig) -> Result<Self, RealtimeError> {
            Err(RealtimeError::NotCompiled)
        }

        pub fn element(&self) -> &'static CodecElement {
            match self.never {}
        }

        pub fn accepts_dmabuf(&self) -> bool {
            false
        }

        /// # Errors
        ///
        /// Always [`RealtimeError::NotCompiled`].
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

    /// Placeholder for builds without `--features realtime`.
    #[derive(Debug)]
    pub struct RealtimeDecoder {
        never: Never,
    }

    impl RealtimeDecoder {
        /// # Errors
        ///
        /// Always [`RealtimeError::NotCompiled`].
        pub fn new(_config: DecoderConfig) -> Result<Self, RealtimeError> {
            Err(RealtimeError::NotCompiled)
        }

        /// # Errors
        ///
        /// Always [`RealtimeError::NotCompiled`].
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

        /// # Errors
        ///
        /// Always [`RealtimeError::NotCompiled`].
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

    /// Nothing can be encoded or decoded without `GStreamer`.
    pub fn local_capabilities() -> CodecCapabilities {
        CodecCapabilities::default()
    }

    /// Nothing to rescan without `GStreamer`.
    pub fn rescan_registry() {}
}

#[cfg(not(gst_realtime))]
pub use unavailable::{RealtimeDecoder, RealtimeEncoder, local_capabilities, rescan_registry};

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
        // Phase `av1_wins_only_when_every_end_has_it_in_hardware`.
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
        // Phase `a_legacy_receiver_pins_h264`.
        {
            let sender = caps(&[Av1, Vp9], &[Av1, Vp9], &[H264]);
            let got = negotiate(&sender, &[CodecCapabilities::legacy_peer()]).unwrap();
            assert_eq!(got.codec, H264);
            assert!(!got.hardware_both_ends);
        }
        // Phase `without_any_h264_encoder_a_royalty_free_codec_is_used`.
        {
            // Stock image, no Cisco openh264, AMD with AV1 encode.
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
        // Phase `no_receivers_means_the_senders_own_best`.
        {
            let sender = caps(&[Vp9], &[], &[H264]);
            assert_eq!(negotiate(&sender, &[]).unwrap().codec, Vp9);
        }
        // Phase `hdr_needs_a_hardware_10_bit_encoder`.
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
        // Phase `labels_skip_codecs_this_build_does_not_know`.
        {
            let parsed = CodecCapabilities::parse_labels(&["av1", "h266", "h264"]);
            assert_eq!(parsed, BTreeSet::from([Av1, H264]));
            assert_eq!(CodecCapabilities::labels(&parsed), ["h264", "av1"]);
        }
    }

    #[test]
    fn realtime_bitstreams_and_frame_layouts_are_read_exactly() {
        // Phase `keyframe_probes_read_each_codecs_own_header`.
        {
            assert!(is_keyframe(H264, &[0, 0, 0, 1, 0x67, 0, 0, 1, 0x65]));
            assert!(!is_keyframe(H264, &[0, 0, 1, 0x41]));
            assert!(!is_keyframe(H264, &[0xAA]));
            // HEVC IDR_W_RADL (19) → header byte 19 << 1 = 0x26.
            assert!(is_keyframe(HevcMain10, &[0, 0, 0, 1, 0x26, 0x01]));
            // TRAIL_R (1) → 0x02.
            assert!(!is_keyframe(HevcMain10, &[0, 0, 0, 1, 0x02, 0x01]));
            // VP9 profile 0 key frame: 10 0 0 0 0 .. = 0x80 (show_existing 0,
            // frame_type 0); inter frame sets frame_type: 0x84.
            assert!(is_keyframe(Vp9, &[0x82, 0x49, 0x83, 0x42]));
            assert!(!is_keyframe(Vp9, &[0x86, 0x00]));
            // AV1: temporal delimiter (type 2, size 0) then sequence header
            // (type 1, size 1).
            assert!(is_keyframe(Av1, &[0x12, 0x00, 0x0a, 0x01, 0x00]));
            // Temporal delimiter then a frame OBU (type 6).
            assert!(!is_keyframe(Av1, &[0x12, 0x00, 0x32, 0x01, 0x00]));
            assert!(!is_keyframe(Av1, &[0x12, 0x85]), "truncated size");
        }
        // Phase `config_rejects_what_no_420_encoder_can_take`.
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
        // Phase `cropping_removes_encoder_padding_from_packed_and_planar_pictures`.
        {
            use crate::frame::{FramePayload, PlanarFormat, PlanarImage, PlaneData};
            use std::sync::Arc;
            // 4x3 RGBA whose last column and row are padding.
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

            // Within bounds: untouched.
            let small = crate::Frame {
                width: 2,
                height: 2,
                pts_ms: 0,
                payload: FramePayload::Rgba(Arc::new(vec![1; 16])),
            };
            assert_eq!(crop_frame(small, 8, 8).width, 2);
        }
        // Phase `frame_lengths_match_each_layout`.
        {
            assert_eq!(InputFormat::Rgba.frame_len(4, 2), Some(32));
            assert_eq!(InputFormat::Nv12.frame_len(4, 2), Some(12));
            assert_eq!(InputFormat::P010.frame_len(4, 2), Some(24));
            assert_eq!(InputFormat::Rgba.frame_len(u32::MAX, u32::MAX), None);
        }
    }
}
