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

use super::{Encoder, EncoderError, EncoderInit, EncoderKind, FrameInput, FrameStats, ProbeOutcome};
use ffmpeg_next as ffmpeg;
use ffmpeg_next::ffi;
use std::ffi::CString;
use std::sync::Once;
use std::time::Instant;

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
pub fn probe(codec: super::Codec) -> ProbeOutcome {
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

/// NVENC H.264 backend via libavcodec.
pub struct NvEncBackend {
    width: u32,
    height: u32,
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
                    "nvenc H.264 requires even dimensions, got {}x{}",
                    init.width, init.height
                ),
            });
        }
        // Reuse the probe as a construction gate.
        let outcome = probe();
        if !outcome.available {
            return Err(EncoderError::Unavailable {
                kind: EncoderKind::NvEnc,
                reason: outcome.reason,
            });
        }
        ensure_ffmpeg_init();
        let codec_ptr = find_encoder("h264_nvenc").ok_or_else(|| EncoderError::Unavailable {
            kind: EncoderKind::NvEnc,
            reason: "h264_nvenc vanished between probe and open".to_string(),
        })?;
        // SAFETY: non-null pointer into libavcodec's static registry.
        let codec_ff = unsafe { ffmpeg::Codec::wrap(codec_ptr) };

        let ctx = ffmpeg::codec::Context::new_with_codec(codec_ff);
        let time_base = ffmpeg::Rational::new(1, init.fps.max(1) as i32);
        let mut video_enc =
            ctx.encoder()
                .video()
                .map_err(|e| EncoderError::ConfigFailed {
                    kind: EncoderKind::NvEnc,
                    detail: format!("encoder().video(): {e}"),
                })?;
        // NVENC accepts NV12 CPU-side input; it uploads to its own
        // GPU surface internally.
        video_enc.set_width(init.width);
        video_enc.set_height(init.height);
        video_enc.set_format(ffmpeg::format::Pixel::NV12);
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
        // Low-latency preset — matches the flatpak-share cadence.
        opts.set("preset", "p3");
        opts.set("tune", "ll");
        opts.set("profile", "baseline");
        opts.set("rc", "cbr");

        let encoder = video_enc.open_as_with(codec_ff, opts).map_err(|e| {
            EncoderError::ConfigFailed {
                kind: EncoderKind::NvEnc,
                detail: format!("avcodec_open2(h264_nvenc): {e}"),
            }
        })?;

        let sw_frame = ffmpeg::frame::Video::new(
            ffmpeg::format::Pixel::NV12,
            init.width,
            init.height,
        );
        let sws = ffmpeg::software::scaling::Context::get(
            ffmpeg::format::Pixel::RGBA,
            init.width,
            init.height,
            ffmpeg::format::Pixel::NV12,
            init.width,
            init.height,
            ffmpeg::software::scaling::Flags::BILINEAR,
        )
        .map_err(|e| EncoderError::ConfigFailed {
            kind: EncoderKind::NvEnc,
            detail: format!("sws_getContext(RGBA→NV12): {e}"),
        })?;

        tracing::info!(
            width = init.width,
            height = init.height,
            fps = init.fps,
            bitrate_kbps = init.bitrate_kbps,
            "nvenc h264 encoder opened"
        );

        Ok(Self {
            width: init.width,
            height: init.height,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvenc_probe_shape() {
        // On a build with the `nvenc` feature but no NVIDIA hardware,
        // the probe reports unavailable with a driver-absence reason.
        // Test just checks the enum discriminant + non-empty reason
        // string — the actual availability depends on the host.
        let outcome = probe();
        assert_eq!(outcome.kind, EncoderKind::NvEnc);
        assert!(!outcome.reason.is_empty());
    }
}
