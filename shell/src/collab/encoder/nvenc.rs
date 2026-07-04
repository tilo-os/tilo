//! NVENC hardware encoder for NVIDIA GPUs.
//!
//! Only compiled with the `nvenc` cargo feature (default off — this is a
//! NVIDIA-only build variant, and pulling in the SDK on non-NVIDIA hosts
//! wastes disk + build time).
//!
//! # Pipeline
//!
//! The backend calls `h264_nvenc` (libavcodec) with an RGBA→NV12
//! CPU-side conversion via libswscale, then hands the CPU frame straight
//! to libavcodec — NVENC pulls its own upload internally.
//!
//! # Runtime probe
//!
//! 1. Check `/dev/nvidia0` / `/dev/nvidiactl` presence — the SDK panics
//!    on missing driver. Skips the codec lookup on non-NVIDIA hosts so
//!    the probe stays cheap.
//! 2. Ask libavcodec if `h264_nvenc` is linked in — libavcodec is only
//!    built against NVENC when the upstream configure step opts in;
//!    Fedora's `ffmpeg-free` skips it, so the probe reports the reason.

use super::{
    Codec, Encoder, EncoderError, EncoderInit, EncoderKind, FrameInput, FrameStats, ProbeOutcome,
};
use crate::collab::colorimetry::{ColorPrimaries, ColorRange, TransferCharacteristics};
use crate::collab::hdr_metadata::Hdr10Metadata;
use ffmpeg_next as ffmpeg;
use ffmpeg_next::ffi;
use std::ffi::CString;
use std::sync::Once;
use std::time::Instant;

/// libavutil `AVMasteringDisplayMetadata` — SMPTE ST 2086. Declared
/// locally because `ffmpeg-sys-next 8.1` doesn't expose the struct in
/// its bindings. Layout matches libavutil since 55 (2016).
#[repr(C)]
#[allow(non_snake_case)]
struct AVMasteringDisplayMetadata {
    display_primaries: [[ffi::AVRational; 2]; 3],
    white_point: [ffi::AVRational; 2],
    min_luminance: ffi::AVRational,
    max_luminance: ffi::AVRational,
    has_primaries: libc::c_int,
    has_luminance: libc::c_int,
}

/// libavutil `AVContentLightMetadata` — CTA-861.3.
#[repr(C)]
#[allow(non_snake_case)]
struct AVContentLightMetadata {
    MaxCLL: libc::c_uint,
    MaxFALL: libc::c_uint,
}

static FFMPEG_INIT: Once = Once::new();

fn ensure_ffmpeg_init() {
    FFMPEG_INIT.call_once(|| {
        let _ = ffmpeg::init();
    });
}

fn find_encoder(name: &str) -> Option<*const ffi::AVCodec> {
    let cname = CString::new(name).ok()?;
    // SAFETY: `cname` is a valid nul-terminated pointer for the
    // duration of the call.
    let ptr = unsafe { ffi::avcodec_find_encoder_by_name(cname.as_ptr()) };
    if ptr.is_null() { None } else { Some(ptr) }
}

/// Probe NVENC availability.
///
/// # Method
/// 1. Check `/dev/nvidia0` (or `/dev/nvidiactl`) presence.
/// 2. Ask libavcodec if `h264_nvenc` is registered. Missing = the
///    ffmpeg build wasn't linked against the NVENC SDK.
pub fn probe(codec: Codec) -> ProbeOutcome {
    let encoder_name = match codec.libav_encoder_name(EncoderKind::NvEnc) {
        Some(name) => name,
        None => {
            return ProbeOutcome {
                kind: EncoderKind::NvEnc,
                codec,
                available: false,
                reason: format!("NVENC has no encoder mapping for {:?}", codec),
            };
        }
    };
    if !std::path::Path::new("/dev/nvidiactl").exists()
        && !std::path::Path::new("/dev/nvidia0").exists()
    {
        return ProbeOutcome {
            kind: EncoderKind::NvEnc,
            codec,
            available: false,
            reason: "no /dev/nvidia* nodes present — non-NVIDIA host".to_string(),
        };
    }
    ensure_ffmpeg_init();
    if find_encoder(encoder_name).is_none() {
        return ProbeOutcome {
            kind: EncoderKind::NvEnc,
            codec,
            available: false,
            reason: format!(
                "libavcodec has no {encoder_name} encoder built in \
                 (rebuild ffmpeg with --enable-nvenc)"
            ),
        };
    }
    ProbeOutcome {
        kind: EncoderKind::NvEnc,
        codec,
        available: true,
        reason: format!("{encoder_name} via libavcodec (NVIDIA driver present)"),
    }
}

/// NVENC H.264 / HEVC-Main10 backend via libavcodec.
pub struct NvEncBackend {
    width: u32,
    height: u32,
    /// Which codec this instance is driving. Drives sw pixel format
    /// (NV12 vs P010LE) and the HDR SEI-insertion path.
    codec: Codec,
    /// HDR10 static metadata to attach to every frame via libavutil
    /// side-data. `None` on SDR pipelines.
    hdr: Option<Hdr10Metadata>,
    frames_encoded: u64,
    pts: i64,
    encoder: ffmpeg::codec::encoder::video::Encoder,
    sw_frame: ffmpeg::frame::Video,
    sws: ffmpeg::software::scaling::Context,
}

// SAFETY: same reasoning as VaApiBackend — every method takes
// `&mut self`, libavcodec's encoder handle is Send.
unsafe impl Send for NvEncBackend {}

impl NvEncBackend {
    pub fn new(init: EncoderInit) -> Result<Self, EncoderError> {
        if init.width == 0 || init.height == 0 {
            return Err(EncoderError::ConfigFailed {
                kind: EncoderKind::NvEnc,
                detail: format!("zero-sized frame ({}x{})", init.width, init.height),
            });
        }
        if init.width % 2 != 0 || init.height % 2 != 0 {
            return Err(EncoderError::ConfigFailed {
                kind: EncoderKind::NvEnc,
                detail: format!(
                    "nvenc requires even dimensions, got {}x{}",
                    init.width, init.height
                ),
            });
        }
        // Reuse the probe as a construction gate.
        let outcome = probe(init.codec);
        if !outcome.available {
            return Err(EncoderError::Unavailable {
                kind: EncoderKind::NvEnc,
                reason: outcome.reason,
            });
        }
        ensure_ffmpeg_init();
        let encoder_name = init.codec.libav_encoder_name(EncoderKind::NvEnc).ok_or_else(|| {
            EncoderError::Unavailable {
                kind: EncoderKind::NvEnc,
                reason: format!("no NVENC encoder mapping for {:?}", init.codec),
            }
        })?;
        let codec_ptr = find_encoder(encoder_name).ok_or_else(|| EncoderError::Unavailable {
            kind: EncoderKind::NvEnc,
            reason: format!("{encoder_name} vanished between probe and open"),
        })?;
        // SAFETY: non-null pointer into libavcodec's static registry.
        let codec_ff = unsafe { ffmpeg::Codec::wrap(codec_ptr) };

        let mut ctx = ffmpeg::codec::Context::new_with_codec(codec_ff);
        let time_base = ffmpeg::Rational::new(1, init.fps.max(1) as i32);
        // Stamp colorimetry onto AVCodecContext before opening so the
        // encoder's SPS VUI / VPS carries the right primaries + transfer
        // + matrix + range.
        //
        // SAFETY: `ctx` is a fresh AVCodecContext. The color fields are
        // POD enum discriminants.
        unsafe {
            let ctx_ptr = ctx.as_mut_ptr();
            apply_colorimetry_to_ctx(ctx_ptr, init);
        }
        let mut video_enc =
            ctx.encoder()
                .video()
                .map_err(|e| EncoderError::ConfigFailed {
                    kind: EncoderKind::NvEnc,
                    detail: format!("encoder().video(): {e}"),
                })?;
        // NV12 for 8-bit H.264, P010LE for 10-bit HEVC Main10. NVENC
        // pulls the CPU frame in and uploads to its own GPU surface.
        let sw_pixel = match init.codec {
            Codec::H264 => ffmpeg::format::Pixel::NV12,
            Codec::HevcMain10 => ffmpeg::format::Pixel::P010LE,
        };
        video_enc.set_width(init.width);
        video_enc.set_height(init.height);
        video_enc.set_format(sw_pixel);
        video_enc.set_time_base(time_base);
        video_enc.set_frame_rate(Some(ffmpeg::Rational::new(init.fps.max(1) as i32, 1)));
        if init.bitrate_kbps > 0 {
            video_enc.set_bit_rate((init.bitrate_kbps as usize) * 1000);
            video_enc.set_max_bit_rate((init.bitrate_kbps as usize) * 1000);
        }
        let kf = init
            .fps
            .max(1)
            .saturating_mul(init.keyframe_interval_secs.max(1));
        video_enc.set_gop(kf);
        video_enc.set_max_b_frames(0);

        let mut opts = ffmpeg::Dictionary::new();
        match init.codec {
            Codec::H264 => {
                // Low-latency preset — matches the flatpak-share cadence.
                opts.set("preset", "p3");
                opts.set("tune", "ll");
                opts.set("profile", "baseline");
                opts.set("rc", "cbr");
            }
            Codec::HevcMain10 => {
                // HEVC NVENC needs p3+ for reasonable HDR bitrate
                // quality; keep tune=ll for low latency. Main10 profile
                // is required for 10-bit HDR; main tier keeps the
                // bitstream compatible with consumer decoders.
                opts.set("preset", "p3");
                opts.set("tune", "ll");
                opts.set("profile", "main10");
                opts.set("tier", "main");
                opts.set("rc", "cbr");
            }
        }

        let encoder = video_enc.open_as_with(codec_ff, opts).map_err(|e| {
            EncoderError::ConfigFailed {
                kind: EncoderKind::NvEnc,
                detail: format!("avcodec_open2({encoder_name}): {e}"),
            }
        })?;

        let sw_frame = ffmpeg::frame::Video::new(sw_pixel, init.width, init.height);
        let sws = ffmpeg::software::scaling::Context::get(
            ffmpeg::format::Pixel::RGBA,
            init.width,
            init.height,
            sw_pixel,
            init.width,
            init.height,
            ffmpeg::software::scaling::Flags::BILINEAR,
        )
        .map_err(|e| EncoderError::ConfigFailed {
            kind: EncoderKind::NvEnc,
            detail: format!("sws_getContext(RGBA→{:?}): {e}", sw_pixel),
        })?;

        tracing::info!(
            codec = ?init.codec,
            width = init.width,
            height = init.height,
            fps = init.fps,
            bitrate_kbps = init.bitrate_kbps,
            hdr = init.hdr.is_some(),
            "nvenc encoder opened"
        );

        Ok(Self {
            width: init.width,
            height: init.height,
            codec: init.codec,
            hdr: init.hdr,
            frames_encoded: 0,
            pts: 0,
            encoder,
            sw_frame,
            sws,
        })
    }

    fn upload_rgba(&mut self, rgba: &[u8]) -> Result<(), EncoderError> {
        let expected = (self.width as usize) * (self.height as usize) * 4;
        if rgba.len() != expected {
            return Err(EncoderError::EncodeFailed {
                kind: EncoderKind::NvEnc,
                detail: format!(
                    "RGBA buffer wrong size: got {} bytes, expected {} for {}x{}",
                    rgba.len(),
                    expected,
                    self.width,
                    self.height
                ),
            });
        }
        let mut src = ffmpeg::frame::Video::new(
            ffmpeg::format::Pixel::RGBA,
            self.width,
            self.height,
        );
        let src_stride = src.stride(0);
        let row_bytes = (self.width as usize) * 4;
        {
            let dst = src.data_mut(0);
            for y in 0..self.height as usize {
                let src_off = y * row_bytes;
                let dst_off = y * src_stride;
                dst[dst_off..dst_off + row_bytes]
                    .copy_from_slice(&rgba[src_off..src_off + row_bytes]);
            }
        }
        self.sws
            .run(&src, &mut self.sw_frame)
            .map_err(|e| EncoderError::EncodeFailed {
                kind: EncoderKind::NvEnc,
                detail: format!("sws_scale: {e}"),
            })?;
        Ok(())
    }

    fn encode_swframe(&mut self, out: &mut Vec<u8>) -> Result<(usize, bool), EncoderError> {
        self.sw_frame.set_pts(Some(self.pts));
        self.pts += 1;
        // HDR10 SEI insertion: attach the mastering-display + CTA-861.3
        // side data on every submitted frame. hevc_nvenc converts these
        // into in-band SEI messages (types 137 + 144). Attaching every
        // frame is spec-legal — a few bytes of overhead — and guarantees
        // late-joining viewers see the metadata on their first IDR.
        if let Some(hdr) = self.hdr {
            // SAFETY: `self.sw_frame` is a live AVFrame owned by the
            // backend. `as_mut_ptr` is unsafe because the caller can
            // mutate the AVFrame from Rust — we only pass the pointer
            // through to libavutil which owns the side-data layout.
            let frame_ptr = unsafe { self.sw_frame.as_mut_ptr() };
            unsafe { attach_hdr10_side_data(frame_ptr, &hdr) }.map_err(|detail| {
                EncoderError::EncodeFailed {
                    kind: EncoderKind::NvEnc,
                    detail,
                }
            })?;
        }
        self.encoder
            .send_frame(&self.sw_frame)
            .map_err(|e| EncoderError::EncodeFailed {
                kind: EncoderKind::NvEnc,
                detail: format!("send_frame: {e}"),
            })?;
        let mut total = 0usize;
        let mut saw_keyframe = false;
        loop {
            let mut pkt = ffmpeg::codec::packet::Packet::empty();
            match self.encoder.receive_packet(&mut pkt) {
                Ok(()) => {
                    if let Some(data) = pkt.data() {
                        out.extend_from_slice(data);
                        total += data.len();
                    }
                    if pkt.is_key() {
                        saw_keyframe = true;
                    }
                }
                Err(ffmpeg::Error::Other {
                    errno: ffmpeg::error::EAGAIN,
                }) => break,
                Err(ffmpeg::Error::Eof) => break,
                Err(e) => {
                    return Err(EncoderError::EncodeFailed {
                        kind: EncoderKind::NvEnc,
                        detail: format!("receive_packet: {e}"),
                    });
                }
            }
        }
        Ok((total, saw_keyframe))
    }
}

impl Encoder for NvEncBackend {
    fn kind(&self) -> EncoderKind {
        EncoderKind::NvEnc
    }
    fn codec(&self) -> Codec {
        self.codec
    }
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }
    fn encode(
        &mut self,
        input: FrameInput<'_>,
        out: &mut Vec<u8>,
    ) -> Result<FrameStats, EncoderError> {
        let start = Instant::now();
        match input {
            FrameInput::Rgba(bytes) => self.upload_rgba(bytes)?,
            FrameInput::Nv12 { y, uv } => {
                // NV12 is 8-bit — reject on HEVC Main10 pipelines where
                // the sw_frame is P010 (16-bit container). Caller must
                // upload via RGBA so swscale can widen the samples.
                if self.codec != Codec::H264 {
                    return Err(EncoderError::EncodeFailed {
                        kind: EncoderKind::NvEnc,
                        detail: "NV12 input is 8-bit; HEVC Main10 pipeline needs RGBA"
                            .to_string(),
                    });
                }
                let expected_y = (self.width as usize) * (self.height as usize);
                let expected_uv = expected_y / 2;
                if y.len() != expected_y || uv.len() != expected_uv {
                    return Err(EncoderError::EncodeFailed {
                        kind: EncoderKind::NvEnc,
                        detail: format!(
                            "NV12 plane sizes wrong: got Y={} UV={} expected Y={} UV={}",
                            y.len(),
                            uv.len(),
                            expected_y,
                            expected_uv
                        ),
                    });
                }
                {
                    let stride_y = self.sw_frame.stride(0);
                    let dst_y = self.sw_frame.data_mut(0);
                    for row in 0..self.height as usize {
                        let src_off = row * (self.width as usize);
                        let dst_off = row * stride_y;
                        dst_y[dst_off..dst_off + self.width as usize]
                            .copy_from_slice(&y[src_off..src_off + self.width as usize]);
                    }
                }
                {
                    let stride_uv = self.sw_frame.stride(1);
                    let uv_rows = self.height as usize / 2;
                    let uv_width = self.width as usize;
                    let dst_uv = self.sw_frame.data_mut(1);
                    for row in 0..uv_rows {
                        let src_off = row * uv_width;
                        let dst_off = row * stride_uv;
                        dst_uv[dst_off..dst_off + uv_width]
                            .copy_from_slice(&uv[src_off..src_off + uv_width]);
                    }
                }
            }
            FrameInput::DmabufFd { .. } => {
                return Err(EncoderError::EncodeFailed {
                    kind: EncoderKind::NvEnc,
                    detail: "DMABUF-direct import isn't the NVENC path; upload via RGBA/NV12"
                        .to_string(),
                });
            }
        }
        let (bytes, saw_keyframe) = self.encode_swframe(out)?;
        let elapsed = start.elapsed();
        let keyframe = saw_keyframe || self.frames_encoded == 0;
        self.frames_encoded = self.frames_encoded.saturating_add(1);
        Ok(FrameStats {
            encode_wallclock: elapsed,
            keyframe,
            bytes,
        })
    }
}

/// Map our internal Colorimetry enums onto the AVCodecContext fields
/// libavcodec uses to populate the SPS/VPS VUI. Same helper as VA-API;
/// duplicated so each backend's translation table sits next to its
/// backend logic.
///
/// # Safety
/// Caller guarantees `ctx` is a live AVCodecContext.
unsafe fn apply_colorimetry_to_ctx(ctx: *mut ffi::AVCodecContext, init: EncoderInit) {
    let c = init.colorimetry;
    let pri = match c.primaries {
        ColorPrimaries::Bt709 => ffi::AVColorPrimaries::AVCOL_PRI_BT709,
        ColorPrimaries::Bt2020 => ffi::AVColorPrimaries::AVCOL_PRI_BT2020,
        ColorPrimaries::DisplayP3 => ffi::AVColorPrimaries::AVCOL_PRI_SMPTE432,
        ColorPrimaries::Unspecified => ffi::AVColorPrimaries::AVCOL_PRI_UNSPECIFIED,
    };
    let trc = match c.transfer {
        TransferCharacteristics::Bt709 => ffi::AVColorTransferCharacteristic::AVCOL_TRC_BT709,
        TransferCharacteristics::SmpteSt2084 => {
            ffi::AVColorTransferCharacteristic::AVCOL_TRC_SMPTE2084
        }
        TransferCharacteristics::AribStdB67 => {
            ffi::AVColorTransferCharacteristic::AVCOL_TRC_ARIB_STD_B67
        }
        TransferCharacteristics::Srgb => ffi::AVColorTransferCharacteristic::AVCOL_TRC_IEC61966_2_1,
        TransferCharacteristics::Unspecified => {
            ffi::AVColorTransferCharacteristic::AVCOL_TRC_UNSPECIFIED
        }
    };
    let mat = match c.matrix {
        crate::collab::colorimetry::MatrixCoefficients::Bt709 => {
            ffi::AVColorSpace::AVCOL_SPC_BT709
        }
        crate::collab::colorimetry::MatrixCoefficients::Bt2020NonConstant => {
            ffi::AVColorSpace::AVCOL_SPC_BT2020_NCL
        }
        crate::collab::colorimetry::MatrixCoefficients::Identity => {
            ffi::AVColorSpace::AVCOL_SPC_RGB
        }
        crate::collab::colorimetry::MatrixCoefficients::Unspecified => {
            ffi::AVColorSpace::AVCOL_SPC_UNSPECIFIED
        }
    };
    let range = match c.range {
        ColorRange::Limited => ffi::AVColorRange::AVCOL_RANGE_MPEG,
        ColorRange::Full => ffi::AVColorRange::AVCOL_RANGE_JPEG,
    };
    unsafe {
        (*ctx).color_primaries = pri;
        (*ctx).color_trc = trc;
        (*ctx).colorspace = mat;
        (*ctx).color_range = range;
    }
}

/// Attach HDR10 SEI side-data to `frame`. See `vaapi::attach_hdr10_side_data`
/// for docs; the two backends share the same libavutil struct layouts.
///
/// # Safety
/// Caller guarantees `frame` is a live AVFrame.
unsafe fn attach_hdr10_side_data(
    frame: *mut ffi::AVFrame,
    hdr: &Hdr10Metadata,
) -> Result<(), String> {
    let m = &hdr.mastering;
    let sd = unsafe {
        ffi::av_frame_new_side_data(
            frame,
            ffi::AVFrameSideDataType::AV_FRAME_DATA_MASTERING_DISPLAY_METADATA,
            std::mem::size_of::<AVMasteringDisplayMetadata>(),
        )
    };
    if sd.is_null() {
        return Err("av_frame_new_side_data (mastering-display) returned null".to_string());
    }
    unsafe {
        let out = (*sd).data as *mut AVMasteringDisplayMetadata;
        (*out).display_primaries = [
            [
                ffi::AVRational { num: m.display_primaries[0].0 as i32, den: 50000 },
                ffi::AVRational { num: m.display_primaries[0].1 as i32, den: 50000 },
            ],
            [
                ffi::AVRational { num: m.display_primaries[1].0 as i32, den: 50000 },
                ffi::AVRational { num: m.display_primaries[1].1 as i32, den: 50000 },
            ],
            [
                ffi::AVRational { num: m.display_primaries[2].0 as i32, den: 50000 },
                ffi::AVRational { num: m.display_primaries[2].1 as i32, den: 50000 },
            ],
        ];
        (*out).white_point = [
            ffi::AVRational { num: m.white_point.0 as i32, den: 50000 },
            ffi::AVRational { num: m.white_point.1 as i32, den: 50000 },
        ];
        (*out).max_luminance = ffi::AVRational {
            num: m.max_display_mastering_luminance as i32,
            den: 10000,
        };
        (*out).min_luminance = ffi::AVRational {
            num: m.min_display_mastering_luminance as i32,
            den: 10000,
        };
        (*out).has_primaries = 1;
        (*out).has_luminance = 1;
    }
    let l = &hdr.light;
    if l.max_content_light_level != 0 || l.max_frame_average_light_level != 0 {
        let sd_l = unsafe {
            ffi::av_frame_new_side_data(
                frame,
                ffi::AVFrameSideDataType::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL,
                std::mem::size_of::<AVContentLightMetadata>(),
            )
        };
        if sd_l.is_null() {
            return Err("av_frame_new_side_data (content-light) returned null".to_string());
        }
        unsafe {
            let out = (*sd_l).data as *mut AVContentLightMetadata;
            (*out).MaxCLL = l.max_content_light_level as libc::c_uint;
            (*out).MaxFALL = l.max_frame_average_light_level as libc::c_uint;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvenc_probe_shape() {
        // On a build with the `nvenc` feature but no NVIDIA hardware,
        // the probe reports unavailable with a driver-absence reason.
        // Test just checks the enum discriminant + non-empty reason
        // string — the actual availability depends on the host.
        let outcome = probe(Codec::H264);
        assert_eq!(outcome.kind, EncoderKind::NvEnc);
        assert!(!outcome.reason.is_empty());
    }
}
