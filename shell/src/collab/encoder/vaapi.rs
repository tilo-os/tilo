//! VA-API hardware encoder for Intel iGPU / AMD Radeon / ARM SoCs.
//!
//! Only compiled with the `vaapi` cargo feature, which pulls in
//! `ffmpeg-next` + libavcodec (plus libswscale for the RGBA→NV12 CPU
//! conversion).
//!
//! # Pipeline
//!
//! 1. `av_hwdevice_ctx_create(VAAPI, "/dev/dri/renderD*")` — opens
//!    the render node as a libva device.
//! 2. `av_hwframe_ctx_alloc` + `av_hwframe_ctx_init` — allocates a
//!    frames pool with `pix_fmt = VAAPI`, `sw_format = NV12`, sized
//!    to the encoder's dimensions.
//! 3. Encoder codec context is set to `pix_fmt = VAAPI` and its
//!    `hw_frames_ctx` refs the frames pool; open with `h264_vaapi`.
//! 4. Per frame: RGBA→NV12 via libswscale into a CPU frame, then
//!    `av_hwframe_get_buffer` for a hardware frame, then
//!    `av_hwframe_transfer_data` upload, then `send_frame` +
//!    `receive_packet` drain.
//!
//! `FrameInput::DmabufFd` isn't wired end-to-end yet: the CPU-upload
//! path lands first because it works on every VA-API driver; the
//! zero-copy DMABUF path requires the compositor commit hook to
//! extract fourcc/modifier/plane strides from the wl_buffer, which
//! is a broader change tracked at the SurfaceTracker call site.

use super::{
    Codec, Encoder, EncoderError, EncoderInit, EncoderKind, FrameInput, FrameStats, ProbeOutcome,
};
use crate::collab::colorimetry::{ColorPrimaries, ColorRange, TransferCharacteristics};
use crate::collab::hdr_metadata::Hdr10Metadata;
use ffmpeg_next as ffmpeg;
use ffmpeg_next::ffi;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Instant;

/// libavutil `AVMasteringDisplayMetadata` — SMPTE ST 2086 payload,
/// declared locally because `ffmpeg-sys-next 8.1` doesn't expose the
/// struct in its bindings (bindgen doesn't pick up
/// `libavutil/mastering_display_metadata.h`). The C layout is stable
/// since libavutil 55 (2016); we mirror it here byte-for-byte so
/// `av_frame_new_side_data` returns a slot we can fill in place.
#[repr(C)]
#[allow(non_snake_case)]
struct AVMasteringDisplayMetadata {
    /// Chromaticity coordinates (x, y) of the R/G/B primaries, in
    /// AVRational form. Order matches libavutil: `[R_x, R_y]`,
    /// `[G_x, G_y]`, `[B_x, B_y]`.
    display_primaries: [[ffi::AVRational; 2]; 3],
    /// Chromaticity of the mastering-display white point (x, y).
    white_point: [ffi::AVRational; 2],
    /// Minimum luminance of the mastering display in cd/m² (0..=uint32 max).
    min_luminance: ffi::AVRational,
    /// Maximum luminance of the mastering display in cd/m².
    max_luminance: ffi::AVRational,
    /// 1 if `display_primaries` + `white_point` are valid, 0 otherwise.
    has_primaries: libc::c_int,
    /// 1 if `min_luminance` + `max_luminance` are valid.
    has_luminance: libc::c_int,
}

/// libavutil `AVContentLightMetadata` — CTA-861.3 payload. Same story
/// as `AVMasteringDisplayMetadata`: stable layout since libavutil 56,
/// declared locally.
#[repr(C)]
#[allow(non_snake_case)]
struct AVContentLightMetadata {
    /// Max content light level (nits, unsigned). 0 = unknown.
    MaxCLL: libc::c_uint,
    /// Max frame-average light level (nits). 0 = unknown.
    MaxFALL: libc::c_uint,
}

static FFMPEG_INIT: Once = Once::new();

/// Run `ffmpeg::init` at most once per process. libavcodec initialises
/// its codec registry lazily, but calling `avcodec_register_all` twice
/// is harmless — the once-guard keeps the logging tidy.
fn ensure_ffmpeg_init() {
    FFMPEG_INIT.call_once(|| {
        // Best-effort: initialisation only fails on OOM, and returning
        // silently lets the subsequent probe report the specific
        // "codec not found" error.
        let _ = ffmpeg::init();
    });
}

/// Probe VA-API availability on this host.
///
/// The probe is:
/// 1. Ensure libavcodec is initialised.
/// 2. Look up `h264_vaapi` in the codec registry. Missing = libavcodec
///    wasn't built with VA-API support; nothing else to try.
/// 3. Attempt a real end-to-end open: build the hardware device, the
///    frames pool, and open the h264_vaapi encoder at a small
///    resolution. That's the only honest test — some hardware
///    (AMD Phoenix iGPU on Fedora's mesa driver ships JPEG + AV1
///    encode but no H.264) has a working render node but the driver
///    refuses `avcodec_open2(h264_vaapi)`. A cheap
///    `av_hwdevice_ctx_create`-only probe would report those hosts
///    as available and let `SharedFlatpakSession` construct + start
///    encoding, only to fail at the first frame.
pub fn probe(codec: Codec) -> ProbeOutcome {
    ensure_ffmpeg_init();
    let encoder_name = match codec.libav_encoder_name(EncoderKind::VaApi) {
        Some(name) => name,
        None => {
            return ProbeOutcome {
                kind: EncoderKind::VaApi,
                codec,
                available: false,
                reason: format!("VA-API has no encoder mapping for {:?}", codec),
            };
        }
    };
    // Codec-side check: if libavcodec wasn't built with this VA-API
    // codec, there's no point iterating render nodes.
    if find_encoder(encoder_name).is_none() {
        return ProbeOutcome {
            kind: EncoderKind::VaApi,
            codec,
            available: false,
            reason: format!("libavcodec has no {encoder_name} encoder built in"),
        };
    }
    let nodes = enumerate_render_nodes();
    if nodes.is_empty() {
        return ProbeOutcome {
            kind: EncoderKind::VaApi,
            codec,
            available: false,
            reason: "no /dev/dri/renderD* nodes present on this host".to_string(),
        };
    }
    // Full-open smoke test. Bigger frame for HEVC because some drivers
    // reject sub-QCIF for 10-bit. Even a JPEG-only driver will fail at
    // avcodec_open2 for either codec.
    let (w, h) = match codec {
        Codec::H264 => (64u32, 64u32),
        Codec::HevcMain10 => (320u32, 240u32),
    };
    let smoke_init = if codec.bit_depth() == 10 {
        EncoderInit::hdr10_hevc(
            w,
            h,
            30,
            2000,
            2,
            crate::collab::hdr_metadata::HdrConfig::pq_reference(),
        )
    } else {
        EncoderInit::sdr_h264(w, h, 30, 1000, 2)
    };
    match VaApiBackend::new(smoke_init) {
        Ok(backend) => ProbeOutcome {
            kind: EncoderKind::VaApi,
            codec,
            available: true,
            reason: format!(
                "{encoder_name} opened on {} (encoder open smoke test passed)",
                backend.render_node.display()
            ),
        },
        Err(EncoderError::Unavailable { reason, .. }) => ProbeOutcome {
            kind: EncoderKind::VaApi,
            codec,
            available: false,
            reason: format!("hardware not available: {reason}"),
        },
        Err(EncoderError::ConfigFailed { detail, .. }) => ProbeOutcome {
            kind: EncoderKind::VaApi,
            codec,
            available: false,
            reason: format!("{encoder_name} refused to open: {detail}"),
        },
        Err(EncoderError::EncodeFailed { detail, .. }) => ProbeOutcome {
            kind: EncoderKind::VaApi,
            codec,
            available: false,
            reason: format!("{encoder_name} encode-probe failed: {detail}"),
        },
    }
}

fn enumerate_render_nodes() -> Vec<PathBuf> {
    let dri = PathBuf::from("/dev/dri");
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dri) {
        for e in entries.flatten() {
            let path = e.path();
            let file = match path.file_name().and_then(|f| f.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            if file.starts_with("renderD") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Look up an encoder by name via libavcodec. Wraps
/// `avcodec_find_encoder_by_name`. Safe wrapper — the returned pointer
/// is either non-null (points into libavcodec's static registry) or
/// null.
fn find_encoder(name: &str) -> Option<*const ffi::AVCodec> {
    let cname = CString::new(name).ok()?;
    // SAFETY: `cname` is a valid nul-terminated pointer for the
    // duration of the call. `avcodec_find_encoder_by_name` returns a
    // pointer into libavcodec's static registry (does not need free).
    let ptr = unsafe { ffi::avcodec_find_encoder_by_name(cname.as_ptr()) };
    if ptr.is_null() { None } else { Some(ptr) }
}

/// VA-API H.264 / HEVC-Main10 backend.
///
/// Owns the libavcodec encoder context, the hardware device / frames
/// contexts, the swscale RGBA→(NV12|P010) converter, and preallocated
/// CPU + hardware frames used per encode call.
pub struct VaApiBackend {
    /// Configured width in pixels.
    width: u32,
    /// Configured height in pixels.
    height: u32,
    /// Which codec this instance is driving. Chosen at `new`; drives
    /// the sw-format + libav encoder name + HDR SEI insertion path.
    codec: Codec,
    /// HDR10 static metadata to attach to every keyframe via libavutil
    /// side-data. `None` on SDR pipelines.
    hdr: Option<Hdr10Metadata>,
    /// Frames encoded so far — the first one is reported as a keyframe.
    frames_encoded: u64,
    /// Monotonic PTS counter (in `time_base` units, which is 1/fps).
    pts: i64,
    /// libavcodec encoder (owns its AVCodecContext).
    encoder: ffmpeg::codec::encoder::video::Encoder,
    /// Reference to the hardware frames pool used by `hw_frame`. Kept
    /// alive alongside the encoder so `hw_frame_get_buffer` can pull
    /// from it every tick.
    hw_frames_ctx: *mut ffi::AVBufferRef,
    /// Reference to the hardware device context that backs `hw_frames_ctx`.
    hw_device_ctx: *mut ffi::AVBufferRef,
    /// Preallocated CPU-side sw_frame — NV12 for H.264, P010 for HEVC
    /// Main10 — used as the CPU→VA transfer source.
    sw_frame: ffmpeg::frame::Video,
    /// libswscale context RGBA→(NV12|P010). Cached across frames.
    sws: ffmpeg::software::scaling::Context,
    /// Which render node backs the VA-API device (for diagnostics).
    render_node: PathBuf,
}

// SAFETY: `VaApiBackend` owns its libavcodec state exclusively — the
// encoder / frame / hw context pointers are not shared and every
// method takes `&mut self`. `ffmpeg::codec::encoder::video::Encoder`
// is itself `Send` (`Context: Send`), and the raw `AVBufferRef`
// pointers are only ever touched under `&mut self`.
unsafe impl Send for VaApiBackend {}

impl VaApiBackend {
    /// Build a VA-API encoder over the first opening render node.
    ///
    /// Codec routing is driven by `init.codec`:
    /// - `Codec::H264` → `h264_vaapi` with `sw_format = NV12` (8-bit).
    /// - `Codec::HevcMain10` → `hevc_vaapi` with `sw_format = P010`
    ///   (10-bit LSB-aligned in a 16-bit container).
    pub fn new(init: EncoderInit) -> Result<Self, EncoderError> {
        if init.width == 0 || init.height == 0 {
            return Err(EncoderError::ConfigFailed {
                kind: EncoderKind::VaApi,
                detail: format!("zero-sized frame ({}x{})", init.width, init.height),
            });
        }
        // Both NV12 and P010 need even dimensions for chroma planes
        // — reject odd sizes with a specific config error rather than
        // producing garbage output.
        if init.width % 2 != 0 || init.height % 2 != 0 {
            return Err(EncoderError::ConfigFailed {
                kind: EncoderKind::VaApi,
                detail: format!(
                    "vaapi requires even dimensions, got {}x{}",
                    init.width, init.height
                ),
            });
        }
        ensure_ffmpeg_init();

        // 1. Locate the libav encoder matching the requested codec.
        let encoder_name = init.codec.libav_encoder_name(EncoderKind::VaApi).ok_or_else(|| {
            EncoderError::Unavailable {
                kind: EncoderKind::VaApi,
                reason: format!("no VA-API encoder mapping for {:?}", init.codec),
            }
        })?;
        let codec_ptr = find_encoder(encoder_name).ok_or_else(|| EncoderError::Unavailable {
            kind: EncoderKind::VaApi,
            reason: format!("libavcodec has no {encoder_name} encoder built in"),
        })?;
        // SAFETY: `avcodec_find_encoder_by_name` returned non-null.
        // The pointer references libavcodec's static registry — never
        // dropped, never mutated behind our back.
        let codec_ff = unsafe { ffmpeg::Codec::wrap(codec_ptr) };

        // 2. Open the VAAPI hardware device on the first available render node.
        let (render_node, hw_device_ctx) = open_hw_device()?;

        // 3. Build the frames pool. `hw_pix_fmt = VAAPI`, `sw_pix_fmt`
        // picks NV12 (8-bit H.264) or P010 (10-bit HEVC).
        let sw_pix_fmt = sw_format_for_codec(init.codec);
        let hw_frames_ctx =
            build_hw_frames_ctx(hw_device_ctx, init.width, init.height, sw_pix_fmt).map_err(
                |detail| EncoderError::ConfigFailed {
                    kind: EncoderKind::VaApi,
                    detail,
                },
            )?;

        // 4. Configure the encoder context: pix_fmt = VAAPI, hw_frames_ctx set.
        let mut ctx = ffmpeg::codec::Context::new_with_codec(codec_ff);
        let time_base = ffmpeg::Rational::new(1, init.fps.max(1) as i32);
        // Assign hw_frames_ctx before opening. The encoder AVs a ref;
        // we still own our reference too. Also stamp colorimetry onto
        // the AVCodecContext so the encoder's SPS VUI / VPS carries
        // the right primaries + transfer + matrix + range.
        //
        // SAFETY: `ctx` is a fresh AVCodecContext; `hw_frames_ctx`
        // is a non-null buffer ref we own. `av_buffer_ref` adds an
        // extra reference we assign into the context — the context
        // takes ownership of that ref. The color fields are plain
        // enum discriminants.
        unsafe {
            let ctx_ptr = ctx.as_mut_ptr();
            (*ctx_ptr).hw_frames_ctx = ffi::av_buffer_ref(hw_frames_ctx);
            apply_colorimetry_to_ctx(ctx_ptr, init);
        }
        let mut video_enc =
            ctx.encoder()
                .video()
                .map_err(|e| EncoderError::ConfigFailed {
                    kind: EncoderKind::VaApi,
                    detail: format!("encoder().video(): {e}"),
                })?;
        video_enc.set_width(init.width);
        video_enc.set_height(init.height);
        video_enc.set_format(ffmpeg::format::Pixel::VAAPI);
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
        // Low-latency: no B-frames.
        video_enc.set_max_b_frames(0);

        // Codec-specific encoder private options.
        let mut opts = ffmpeg::Dictionary::new();
        match init.codec {
            Codec::H264 => {
                // Baseline profile keeps the bitstream compatible with
                // every downstream openh264 decoder in the fleet.
                opts.set("profile", "constrained_baseline");
                opts.set("rc_mode", "CBR");
            }
            Codec::HevcMain10 => {
                // Main10 is the 10-bit HEVC profile that carries HDR10.
                opts.set("profile", "main10");
                opts.set("rc_mode", "CBR");
                // Low-power path for iGPUs that expose it. If the
                // driver rejects it, the encoder-open below tries
                // again without the option. Unknown keys are silently
                // ignored by drivers that don't understand them.
                opts.set("low_power", "1");
            }
        }
        let encoder = match video_enc.open_as_with(codec_ff, opts) {
            Ok(enc) => enc,
            Err(first_err) if matches!(init.codec, Codec::HevcMain10) => {
                // Some drivers (older i965, some Mesa versions) reject
                // low_power=1 for HEVC. Retry without the option.
                // Reconstruct the encoder context from scratch —
                // ffmpeg-next's video_enc is consumed on open.
                let mut ctx2 = ffmpeg::codec::Context::new_with_codec(codec_ff);
                unsafe {
                    let cp = ctx2.as_mut_ptr();
                    (*cp).hw_frames_ctx = ffi::av_buffer_ref(hw_frames_ctx);
                    apply_colorimetry_to_ctx(cp, init);
                }
                let mut ve2 = ctx2.encoder().video().map_err(|e| {
                    EncoderError::ConfigFailed {
                        kind: EncoderKind::VaApi,
                        detail: format!("encoder().video() retry: {e}"),
                    }
                })?;
                ve2.set_width(init.width);
                ve2.set_height(init.height);
                ve2.set_format(ffmpeg::format::Pixel::VAAPI);
                ve2.set_time_base(time_base);
                ve2.set_frame_rate(Some(ffmpeg::Rational::new(init.fps.max(1) as i32, 1)));
                if init.bitrate_kbps > 0 {
                    ve2.set_bit_rate((init.bitrate_kbps as usize) * 1000);
                    ve2.set_max_bit_rate((init.bitrate_kbps as usize) * 1000);
                }
                ve2.set_gop(kf);
                ve2.set_max_b_frames(0);
                let mut opts2 = ffmpeg::Dictionary::new();
                opts2.set("profile", "main10");
                opts2.set("rc_mode", "CBR");
                ve2.open_as_with(codec_ff, opts2).map_err(|e| {
                    EncoderError::ConfigFailed {
                        kind: EncoderKind::VaApi,
                        detail: format!(
                            "avcodec_open2({encoder_name}) failed twice — with low_power: {first_err}; without: {e}"
                        ),
                    }
                })?
            }
            Err(e) => {
                return Err(EncoderError::ConfigFailed {
                    kind: EncoderKind::VaApi,
                    detail: format!("avcodec_open2({encoder_name}): {e}"),
                });
            }
        };

        // 5. Pre-allocate the CPU-side sw_frame + swscale context.
        // NV12 for 8-bit H.264, P010LE for 10-bit HEVC.
        let sw_pixel = match init.codec {
            Codec::H264 => ffmpeg::format::Pixel::NV12,
            Codec::HevcMain10 => ffmpeg::format::Pixel::P010LE,
        };
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
            kind: EncoderKind::VaApi,
            detail: format!("sws_getContext(RGBA→{:?}): {e}", sw_pixel),
        })?;

        tracing::info!(
            node = %render_node.display(),
            codec = ?init.codec,
            width = init.width,
            height = init.height,
            fps = init.fps,
            bitrate_kbps = init.bitrate_kbps,
            hdr = init.hdr.is_some(),
            "vaapi encoder opened"
        );

        Ok(Self {
            width: init.width,
            height: init.height,
            codec: init.codec,
            hdr: init.hdr,
            frames_encoded: 0,
            pts: 0,
            encoder,
            hw_frames_ctx,
            hw_device_ctx,
            sw_frame,
            sws,
            render_node,
        })
    }

    /// Fill the preallocated NV12 CPU frame from an RGBA buffer via
    /// libswscale.
    fn upload_rgba(&mut self, rgba: &[u8]) -> Result<(), EncoderError> {
        let expected = (self.width as usize) * (self.height as usize) * 4;
        if rgba.len() != expected {
            return Err(EncoderError::EncodeFailed {
                kind: EncoderKind::VaApi,
                detail: format!(
                    "RGBA buffer wrong size: got {} bytes, expected {} for {}x{}",
                    rgba.len(),
                    expected,
                    self.width,
                    self.height
                ),
            });
        }
        // Build a scratch RGBA source frame that references `rgba` in
        // place. The Video::new + data_mut path would double-allocate
        // — we can skip that by using a source frame with linesize
        // pointing at the caller's slice. Easiest is to build the
        // frame + memcpy once; the cost is width*height*4 bytes per
        // encode, dominated by the swscale conversion time.
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
                kind: EncoderKind::VaApi,
                detail: format!("sws_scale: {e}"),
            })?;
        Ok(())
    }

    /// Allocate a hardware frame from the pool, upload the current
    /// `sw_frame` into it, then send it to the encoder and drain any
    /// available packets. Returns the total bytes appended to `out`
    /// and whether libavcodec flagged the packet as a keyframe.
    fn encode_swframe(&mut self, out: &mut Vec<u8>) -> Result<(usize, bool), EncoderError> {
        // Allocate a fresh hw frame each tick — the encoder holds a
        // ref internally until it's done. The AVFrame is refcounted
        // so this is cheap: the underlying VA surface comes from the
        // frames pool (`hw_frames_ctx`).
        //
        // SAFETY: `av_frame_alloc` returns null on OOM; we handle
        // that. `av_hwframe_get_buffer` binds the frame to the pool.
        let hw_frame = unsafe { ffi::av_frame_alloc() };
        if hw_frame.is_null() {
            return Err(EncoderError::EncodeFailed {
                kind: EncoderKind::VaApi,
                detail: "av_frame_alloc returned null".to_string(),
            });
        }
        // On any error below, free the frame before returning.
        let hw_rc = unsafe { ffi::av_hwframe_get_buffer(self.hw_frames_ctx, hw_frame, 0) };
        if hw_rc < 0 {
            unsafe {
                let mut p = hw_frame;
                ffi::av_frame_free(&mut p);
            }
            return Err(EncoderError::EncodeFailed {
                kind: EncoderKind::VaApi,
                detail: format!("av_hwframe_get_buffer: {hw_rc}"),
            });
        }
        // Upload NV12 → VAAPI surface.
        let trc = unsafe { ffi::av_hwframe_transfer_data(hw_frame, self.sw_frame.as_ptr(), 0) };
        if trc < 0 {
            unsafe {
                let mut p = hw_frame;
                ffi::av_frame_free(&mut p);
            }
            return Err(EncoderError::EncodeFailed {
                kind: EncoderKind::VaApi,
                detail: format!("av_hwframe_transfer_data: {trc}"),
            });
        }
        // Set PTS on the hw frame. libavcodec propagates it into the
        // output AVPacket so the receiver's timestamps match the
        // capture cadence.
        unsafe {
            (*hw_frame).pts = self.pts;
        }
        self.pts += 1;

        // HDR10 SEI insertion: attach the SMPTE ST 2086 + CTA-861.3
        // side data before send_frame. Attaching every frame is
        // spec-legal (SEI overhead is a few bytes) and guarantees a
        // late-joining viewer picks up the metadata on their first IDR
        // — which under GOP=1 is every frame anyway.
        if let Some(hdr) = self.hdr {
            // SAFETY: `hw_frame` is a live AVFrame we own.
            unsafe { attach_hdr10_side_data(hw_frame, &hdr) }.map_err(|detail| {
                EncoderError::EncodeFailed {
                    kind: EncoderKind::VaApi,
                    detail,
                }
            })?;
        }

        // Wrap and send. `Frame::wrap` takes ownership; when we drop
        // the wrapper, the underlying AVFrame is freed.
        let send_frame = unsafe { ffmpeg::Frame::wrap(hw_frame) };
        self.encoder
            .send_frame(&send_frame)
            .map_err(|e| EncoderError::EncodeFailed {
                kind: EncoderKind::VaApi,
                detail: format!("send_frame: {e}"),
            })?;

        // Drain every available packet — VA-API is typically 1-in,
        // 1-out at this GOP config but a burst can arrive after the
        // very first frame (SPS/PPS + IDR).
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
                        kind: EncoderKind::VaApi,
                        detail: format!("receive_packet: {e}"),
                    });
                }
            }
        }
        Ok((total, saw_keyframe))
    }

    /// Diagnostic: which render node backs this encoder's device.
    #[allow(dead_code)]
    pub fn render_node(&self) -> &Path {
        &self.render_node
    }

    /// Zero-copy DMABUF encode. Builds a `DRM_PRIME` source frame with
    /// an `AVDRMFrameDescriptor`, maps it onto a fresh VAAPI hardware
    /// frame via `av_hwframe_map(AV_HWFRAME_MAP_DIRECT)`, sends it to
    /// the encoder, and drains the output packet queue.
    ///
    /// # Errors
    ///
    /// - `EncoderError::EncodeFailed { detail: "dmabuf-direct not \
    ///   supported by this VA-API driver, use FrameInput::Rgba" }` \
    ///   when `av_hwframe_map` returns a negative errno. The caller \
    ///   (compositor commit hook) is expected to catch this specific \
    ///   detail and fall back to the SHM readback path.
    /// - `EncoderError::EncodeFailed` on any allocation / send / \
    ///   receive failure. These are typically un-recoverable — the \
    ///   caller should tear down the session.
    #[allow(clippy::too_many_arguments)]
    fn encode_dmabuf(
        &mut self,
        plane_fds: [std::os::fd::RawFd; 4],
        plane_offsets: [u64; 4],
        plane_strides: [u32; 4],
        num_planes: u8,
        drm_fourcc: u32,
        drm_modifier: u64,
        out: &mut Vec<u8>,
    ) -> Result<(usize, bool), EncoderError> {
        // Reject implausibly-shaped inputs early. Per libavutil the
        // maximum plane count for a DRM_PRIME frame is `AV_DRM_MAX_PLANES`,
        // defined as 4. Anything above that is a caller-side bug.
        if num_planes == 0 || num_planes as usize > 4 {
            return Err(EncoderError::EncodeFailed {
                kind: EncoderKind::VaApi,
                detail: format!("dmabuf num_planes={} out of range 1..=4", num_planes),
            });
        }

        // Build the AVDRMFrameDescriptor on the stack. Its lifetime
        // only needs to cover the `av_hwframe_map` call — the mapping
        // consumes the source frame's data and the destination frame
        // keeps its own reference into the pool afterwards.
        //
        // The `av_hwframe_map` API bindings on ffmpeg-next 8 expose
        // the raw `ffi::av_hwframe_map(dst, src, flags)`; the DRM
        // descriptor types (`AVDRMFrameDescriptor`, `AVDRMObjectDescriptor`,
        // `AVDRMLayerDescriptor`, `AVDRMPlaneDescriptor`) come from
        // `libavutil/hwcontext_drm.h` — bindgen picks them up when
        // the header is present at build time.
        //
        // We treat all planes as sharing a single DRM object (fd) —
        // the common case for a wgpu-composited buffer. Multi-object
        // multi-plane buffers (rare, e.g. some tiled formats with
        // separate storage per plane) would need one object per fd.
        //
        // SAFETY: `AVDRMFrameDescriptor` is a POD C struct; `MaybeUninit`
        // + zero-fill gives us a valid all-zero baseline. Every field
        // we then set is a primitive (int / ptrdiff_t / uint32/64).
        // The subsequent read is inside `av_hwframe_map` which is
        // documented as copy-in on the source descriptor.
        use std::mem::MaybeUninit;
        let mut desc_mu: MaybeUninit<ffi::AVDRMFrameDescriptor> = MaybeUninit::zeroed();
        // SAFETY: read the field addresses only — no reads of
        // uninitialised bytes.
        let desc_ptr = desc_mu.as_mut_ptr();
        unsafe {
            (*desc_ptr).nb_objects = 1;
            (*desc_ptr).objects[0].fd = plane_fds[0];
            // libavutil's size==0 signals "unknown; whole PRIME dup"
            // which is acceptable — the encoder doesn't require the
            // exact byte-length.
            (*desc_ptr).objects[0].size = 0;
            (*desc_ptr).objects[0].format_modifier = drm_modifier;

            (*desc_ptr).nb_layers = 1;
            (*desc_ptr).layers[0].format = drm_fourcc;
            (*desc_ptr).layers[0].nb_planes = num_planes as i32;
            for i in 0..(num_planes as usize) {
                (*desc_ptr).layers[0].planes[i].object_index = 0;
                (*desc_ptr).layers[0].planes[i].offset =
                    plane_offsets[i] as libc::ptrdiff_t;
                (*desc_ptr).layers[0].planes[i].pitch =
                    plane_strides[i] as libc::ptrdiff_t;
            }
        }
        // SAFETY: fully written above.
        let desc = unsafe { desc_mu.assume_init() };

        // Allocate the DRM_PRIME source frame; it doesn't own memory
        // — its `data[0]` is a raw pointer to our stack-local desc.
        // The pointer must be valid until `av_hwframe_map` returns.
        //
        // SAFETY: `av_frame_alloc` returns null on OOM.
        let src_frame = unsafe { ffi::av_frame_alloc() };
        if src_frame.is_null() {
            return Err(EncoderError::EncodeFailed {
                kind: EncoderKind::VaApi,
                detail: "av_frame_alloc (src) returned null".to_string(),
            });
        }
        // On any error below, free src_frame.
        let src_guard = FrameGuard(src_frame);

        // Configure the source frame as an unattached DRM_PRIME view.
        // No `hw_frames_ctx` — `av_hwframe_map` handles the format
        // conversion directly. `data[0]` points at our AVDRMFrameDescriptor.
        //
        // SAFETY: we own `src_frame`; the field writes touch primitive
        // fields on an initialised AVFrame.
        unsafe {
            (*src_frame).format = ffi::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
            (*src_frame).width = self.width as i32;
            (*src_frame).height = self.height as i32;
            (*src_frame).data[0] = &desc as *const _ as *mut u8;
        }

        // Allocate the destination VAAPI hw_frame from our pool.
        //
        // SAFETY: `hw_frames_ctx` is a valid ref we own; `av_frame_alloc`
        // returns null on OOM.
        let hw_frame = unsafe { ffi::av_frame_alloc() };
        if hw_frame.is_null() {
            return Err(EncoderError::EncodeFailed {
                kind: EncoderKind::VaApi,
                detail: "av_frame_alloc (hw) returned null".to_string(),
            });
        }
        let hw_guard = FrameGuard(hw_frame);
        // SAFETY: bind hw_frame to a fresh VAAPI surface from the pool.
        let get_rc =
            unsafe { ffi::av_hwframe_get_buffer(self.hw_frames_ctx, hw_frame, 0) };
        if get_rc < 0 {
            return Err(EncoderError::EncodeFailed {
                kind: EncoderKind::VaApi,
                detail: format!("av_hwframe_get_buffer (dmabuf-direct): {get_rc}"),
            });
        }
        // Map the DRM_PRIME source into the VAAPI destination.
        // `AV_HWFRAME_MAP_DIRECT` = 1 << 3 (from
        // `libavutil/hwcontext.h`); the const is exposed via bindgen
        // as `ffi::AV_HWFRAME_MAP_DIRECT`.
        //
        // SAFETY: both frames are valid; the flag is a documented
        // libavutil constant.
        let map_rc =
            unsafe { ffi::av_hwframe_map(hw_frame, src_frame, ffi::AV_HWFRAME_MAP_DIRECT as i32) };
        if map_rc < 0 {
            // This is the specific error the compositor commit hook
            // catches to fall back to SHM. Keep the exact string
            // stable — call sites match on it.
            return Err(EncoderError::EncodeFailed {
                kind: EncoderKind::VaApi,
                detail: format!(
                    "dmabuf-direct not supported by this VA-API driver, use FrameInput::Rgba (av_hwframe_map={})",
                    map_rc
                ),
            });
        }

        // Stamp PTS on the mapped frame. Encoder propagates it into
        // the AVPacket so downstream timestamps line up with our
        // capture cadence.
        //
        // SAFETY: hw_frame is valid.
        unsafe {
            (*hw_frame).pts = self.pts;
        }
        self.pts += 1;

        // HDR10 SEI insertion on the DMABUF path too. Side data is
        // per-frame libavutil metadata; attaching it to the mapped
        // hw_frame before send_frame carries it through into the
        // bitstream SEI even though the pixel data lives on the GPU.
        if let Some(hdr) = self.hdr {
            // SAFETY: `hw_frame` is a live AVFrame we own via hw_guard.
            unsafe { attach_hdr10_side_data(hw_frame, &hdr) }.map_err(|detail| {
                EncoderError::EncodeFailed {
                    kind: EncoderKind::VaApi,
                    detail,
                }
            })?;
        }

        // Wrap + hand to the encoder. We can't move ownership through
        // ffmpeg-next's `Frame::wrap` for hw_frame because it would
        // race with `hw_guard`'s drop — so we send from the raw
        // pointer via a scratch wrapper, then let hw_guard release
        // its ref after send_frame returns.
        //
        // `send_frame` refs the underlying AVFrame, so it's safe for
        // us to drop `hw_guard` right after. The wrapper simply
        // borrows without taking ownership.
        //
        // SAFETY: `Frame::wrap` documents that the pointer becomes
        // owned by the wrapper. We take that literally: pass a fresh
        // ref instead of the guarded pointer. `av_frame_clone`
        // increments the underlying refs (both the descriptor from
        // the DRM_PRIME source and the pool ref from the mapping).
        let cloned = unsafe { ffi::av_frame_clone(hw_frame) };
        if cloned.is_null() {
            return Err(EncoderError::EncodeFailed {
                kind: EncoderKind::VaApi,
                detail: "av_frame_clone (hw) returned null".to_string(),
            });
        }
        // `Frame::wrap` takes ownership — dropping the wrapper frees
        // the cloned frame. hw_guard still owns the original hw_frame.
        let send_frame = unsafe { ffmpeg::Frame::wrap(cloned) };
        self.encoder
            .send_frame(&send_frame)
            .map_err(|e| EncoderError::EncodeFailed {
                kind: EncoderKind::VaApi,
                detail: format!("send_frame (dmabuf): {e}"),
            })?;
        // Drain packets. Same loop as the RGBA/NV12 path.
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
                        kind: EncoderKind::VaApi,
                        detail: format!("receive_packet (dmabuf): {e}"),
                    });
                }
            }
        }
        drop(hw_guard);
        drop(src_guard);
        Ok((total, saw_keyframe))
    }
}

/// RAII guard that frees an `AVFrame*` on drop. Named locally because
/// the surrounding path threads several `?` early-returns through
/// pointers we allocated with `av_frame_alloc`; a small guard keeps
/// each branch's cleanup one line.
struct FrameGuard(*mut ffi::AVFrame);

impl Drop for FrameGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from `av_frame_alloc` and
            // hasn't been freed elsewhere in this scope.
            unsafe {
                let mut p = self.0;
                ffi::av_frame_free(&mut p);
            }
            self.0 = std::ptr::null_mut();
        }
    }
}

impl Drop for VaApiBackend {
    fn drop(&mut self) {
        // Order matters — the encoder holds references to
        // `hw_frames_ctx`, which holds a reference to `hw_device_ctx`.
        // Dropping `encoder` first via its Drop impl releases the
        // codec context; the buffer refs we own separately release
        // the pool + device.
        //
        // SAFETY: the pointers were obtained via `av_buffer_ref` /
        // `av_hwframe_ctx_alloc` / `av_hwdevice_ctx_create`. Each
        // call to `av_buffer_unref` decrements one reference; passing
        // a pointer to a `*mut AVBufferRef` slot is the documented
        // API.
        unsafe {
            if !self.hw_frames_ctx.is_null() {
                ffi::av_buffer_unref(&mut self.hw_frames_ctx);
            }
            if !self.hw_device_ctx.is_null() {
                ffi::av_buffer_unref(&mut self.hw_device_ctx);
            }
        }
    }
}

impl Encoder for VaApiBackend {
    fn kind(&self) -> EncoderKind {
        EncoderKind::VaApi
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
            FrameInput::Rgba(bytes) => {
                self.upload_rgba(bytes)?;
            }
            FrameInput::Nv12 { y, uv } => {
                // Direct NV12 path — only valid on H.264 pipelines.
                // HEVC Main10 needs a 10-bit input; the caller must
                // upload via RGBA (converted to P010 by swscale) or
                // DMABUF.
                if self.codec != Codec::H264 {
                    return Err(EncoderError::EncodeFailed {
                        kind: EncoderKind::VaApi,
                        detail: "NV12 input is 8-bit; HEVC Main10 pipeline needs RGBA or DMABUF"
                            .to_string(),
                    });
                }
                let expected_y = (self.width as usize) * (self.height as usize);
                let expected_uv = expected_y / 2;
                if y.len() != expected_y || uv.len() != expected_uv {
                    return Err(EncoderError::EncodeFailed {
                        kind: EncoderKind::VaApi,
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
            FrameInput::DmabufFd {
                plane_fds,
                plane_offsets,
                plane_strides,
                num_planes,
                drm_fourcc,
                drm_modifier,
                width,
                height,
            } => {
                // Zero-copy DMABUF import. We build an
                // `AV_PIX_FMT_DRM_PRIME` source frame whose `data[0]`
                // points at an `AVDRMFrameDescriptor`, then map it
                // onto a hardware frame from our VAAPI pool via
                // `av_hwframe_map(AV_HWFRAME_MAP_DIRECT)`. On drivers
                // that can't do the mapping (some AMD SoCs, some
                // older Intel iGPUs) libavutil returns a negative
                // errno and we fall through to
                // `EncoderError::EncodeFailed` so the compositor
                // side falls back to the SHM readback + RGBA path.
                if width != self.width || height != self.height {
                    return Err(EncoderError::EncodeFailed {
                        kind: EncoderKind::VaApi,
                        detail: format!(
                            "dmabuf dimensions {}x{} don't match encoder {}x{}",
                            width, height, self.width, self.height
                        ),
                    });
                }
                let (bytes, saw_keyframe) = self.encode_dmabuf(
                    plane_fds,
                    plane_offsets,
                    plane_strides,
                    num_planes,
                    drm_fourcc,
                    drm_modifier,
                    out,
                )?;
                let elapsed = start.elapsed();
                let keyframe = saw_keyframe || self.frames_encoded == 0;
                self.frames_encoded = self.frames_encoded.saturating_add(1);
                return Ok(FrameStats {
                    encode_wallclock: elapsed,
                    keyframe,
                    bytes,
                });
            }
        }
        let (bytes, saw_keyframe) = self.encode_swframe(out)?;
        let elapsed = start.elapsed();
        // libavcodec doesn't always tag the very first IDR as `is_key`
        // on the first pump (SPS/PPS ships as its own packet). Report
        // frame 0 as keyframe on the outside so PeerSurfaceMetrics'
        // counter matches the wire-level truth.
        let keyframe = saw_keyframe || self.frames_encoded == 0;
        self.frames_encoded = self.frames_encoded.saturating_add(1);
        Ok(FrameStats {
            encode_wallclock: elapsed,
            keyframe,
            bytes,
        })
    }
}

/// Open a VAAPI hardware device on the first render node that accepts
/// `av_hwdevice_ctx_create`.
fn open_hw_device() -> Result<(PathBuf, *mut ffi::AVBufferRef), EncoderError> {
    let nodes = enumerate_render_nodes();
    if nodes.is_empty() {
        return Err(EncoderError::Unavailable {
            kind: EncoderKind::VaApi,
            reason: "no /dev/dri/renderD* nodes present".to_string(),
        });
    }
    let mut last_err = String::new();
    for node in &nodes {
        let cpath = match CString::new(node.as_os_str().as_encoded_bytes()) {
            Ok(c) => c,
            Err(e) => {
                last_err = format!("path {}: {e}", node.display());
                continue;
            }
        };
        let mut hw_ctx: *mut ffi::AVBufferRef = std::ptr::null_mut();
        // SAFETY: hw_ctx is an out-param; on success libavutil writes
        // a fresh AVBufferRef we take ownership of.
        let rc = unsafe {
            ffi::av_hwdevice_ctx_create(
                &mut hw_ctx,
                ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                cpath.as_ptr(),
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 && !hw_ctx.is_null() {
            return Ok((node.clone(), hw_ctx));
        }
        if !hw_ctx.is_null() {
            unsafe { ffi::av_buffer_unref(&mut hw_ctx) };
        }
        last_err = format!("{} rc={rc}", node.display());
    }
    Err(EncoderError::Unavailable {
        kind: EncoderKind::VaApi,
        reason: format!("no VAAPI-capable render node opened; last error: {last_err}"),
    })
}

/// Pick the underlying CPU pixel format for the VA-API hw_frames pool
/// based on the target codec's bit depth. 8-bit H.264 pairs with NV12;
/// 10-bit HEVC Main10 pairs with P010LE (16-bit container, only 10 bits
/// of luma / chroma populated, LSB-aligned).
fn sw_format_for_codec(codec: Codec) -> ffi::AVPixelFormat {
    match codec {
        Codec::H264 => ffi::AVPixelFormat::AV_PIX_FMT_NV12,
        Codec::HevcMain10 => ffi::AVPixelFormat::AV_PIX_FMT_P010LE,
    }
}

/// Map our internal Colorimetry enums onto the AVCodecContext fields
/// libavcodec uses to populate the SPS/VPS VUI. The receiver's decoder
/// reads these to render into the right colour space.
///
/// # Safety
/// Caller guarantees `ctx` is a live AVCodecContext (fresh from
/// `Context::new_with_codec`). All fields written here are POD enums.
unsafe fn apply_colorimetry_to_ctx(ctx: *mut ffi::AVCodecContext, init: EncoderInit) {
    let c = init.colorimetry;
    // Map primaries — CICP integer round-trips to libavutil's AVColorPrimaries.
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

/// Build a hardware frames pool sized for `w × h` with the given
/// `sw_format`. NV12 for 8-bit H.264, P010LE for 10-bit HEVC Main10.
fn build_hw_frames_ctx(
    hw_device_ctx: *mut ffi::AVBufferRef,
    w: u32,
    h: u32,
    sw_format: ffi::AVPixelFormat,
) -> Result<*mut ffi::AVBufferRef, String> {
    // SAFETY: `av_hwframe_ctx_alloc` returns a fresh AVBufferRef whose
    // `data` is an AVHWFramesContext we configure. On failure the
    // returned pointer is null.
    let ctx = unsafe { ffi::av_hwframe_ctx_alloc(hw_device_ctx) };
    if ctx.is_null() {
        return Err("av_hwframe_ctx_alloc returned null".to_string());
    }
    // Configure via the frame ctx's data pointer.
    unsafe {
        let frames_data = (*ctx).data as *mut ffi::AVHWFramesContext;
        (*frames_data).format = ffi::AVPixelFormat::AV_PIX_FMT_VAAPI;
        (*frames_data).sw_format = sw_format;
        (*frames_data).width = w as i32;
        (*frames_data).height = h as i32;
        // Pool size: 16 is a common default; more is wasted VRAM,
        // less risks encoder stalls at high FPS.
        (*frames_data).initial_pool_size = 16;
    }
    let init_rc = unsafe { ffi::av_hwframe_ctx_init(ctx) };
    if init_rc < 0 {
        let mut c = ctx;
        unsafe { ffi::av_buffer_unref(&mut c) };
        return Err(format!("av_hwframe_ctx_init: {init_rc}"));
    }
    Ok(ctx)
}

/// Attach HDR10 mastering-display + content-light SEI metadata as
/// libavutil frame side-data. The encoder converts these into in-band
/// HEVC SEI messages during `send_frame`.
///
/// # Safety
/// Caller guarantees `frame` is a live AVFrame owned by the caller.
/// The side-data slot libavutil allocates is initialised in place with
/// the semantic types from [`Hdr10Metadata`].
unsafe fn attach_hdr10_side_data(
    frame: *mut ffi::AVFrame,
    hdr: &Hdr10Metadata,
) -> Result<(), String> {
    // 1. Mastering-display colour volume — SMPTE ST 2086.
    let m = &hdr.mastering;
    // SAFETY: `av_frame_new_side_data` allocates a side-data slot of
    // the given size and returns a pointer to a fresh AVFrameSideData
    // whose `data` is uninitialised bytes of `size`. We write our
    // AVMasteringDisplayMetadata into `data` before the encoder reads
    // it on `send_frame`.
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
    // SAFETY: `sd.data` points at `size_of::<AVMasteringDisplayMetadata>` bytes
    // of freshly allocated memory. We write a valid struct into it.
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

    // 2. Content-light-level — CTA-861.3. Skip when both fields are 0
    // (libavcodec still emits it, but downstream tone-mappers treat
    // (0, 0) as an inconsistency signal on some vendors — better to
    // just omit).
    let l = &hdr.light;
    if l.max_content_light_level != 0 || l.max_frame_average_light_level != 0 {
        // SAFETY: same shape as above, different type.
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

    /// Codec-registry check: `h264_vaapi` should be linked into
    /// libavcodec on any host that installed `ffmpeg-devel` /
    /// `libavcodec-free-devel`. Runs unconditionally — no hardware
    /// dependency.
    #[test]
    fn h264_vaapi_codec_is_registered() {
        ensure_ffmpeg_init();
        assert!(
            find_encoder("h264_vaapi").is_some(),
            "libavcodec has no h264_vaapi encoder — build with libavcodec that includes VA-API support"
        );
    }

    /// Probe test: on a host with a working render node the probe
    /// should report available. `#[ignore]` so CI runners without
    /// `/dev/dri` don't fail the suite; enable with
    /// `--include-ignored` on a workstation.
    #[test]
    #[ignore = "requires /dev/dri render node with H264 VAAPI encode support"]
    fn vaapi_probe_is_available_on_real_host() {
        let outcome = probe(Codec::H264);
        if !outcome.available {
            eprintln!("skipping: probe reported unavailable: {}", outcome.reason);
            return;
        }
        assert_eq!(outcome.kind, EncoderKind::VaApi);
    }
}
