//! The real-time encoder and decoder, end to end through `GStreamer`.
//!
//! * The software round trips need no GPU and run in CI: VP9 (libvpx) and AV1
//!   (SVT-AV1 or libaom encode, dav1d or libaom decode) are royalty-free and
//!   part of the build environment's plugin set. They fail, rather than skip,
//!   when those elements are missing, because then the royalty-free fallback
//!   the OS relies on is not there.
//! * The hardware round trips run only where the registry lists a hardware
//!   encoder for the codec (which the VA-API and NVCODEC plugins do only when
//!   a device reports the capability). Where none is listed they say so and
//!   return; they never substitute a software element and report success.
//! * The latency measurement prints per-frame encode and decode times for
//!   every path this machine has.
#![cfg(feature = "realtime")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::similar_names,
    clippy::panic_in_result_fn,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, Instant};

use tilo_media::FramePayload;
use tilo_media::realtime::{
    CodecTier, DecodeOutput, DecoderConfig, EncoderConfig, InputFormat, InputFrame,
    RealtimeDecoder, RealtimeEncoder, RealtimePolicy, StreamCodec, local_capabilities,
};

/// A gradient that moves one step per frame, so every frame differs and an
/// encoder cannot skip any.
fn frame(width: u32, height: u32, n: u32) -> Vec<u8> {
    Frames::new(width, height).frame(n)
}

/// [`frame`] for every `n` at one size. Smooth in both axes (a fair test of a
/// 4:2:0 codec) and shifted a little each frame: green depends only on y and
/// blue only on x + y, so only red moves between frames and the rest is built
/// once. The test binary is unoptimised and a 720p frame is almost a million
/// pixels, so this is most of what a long round trip costs.
struct Frames {
    width: u32,
    /// Each frame with red still zero.
    base: Vec<u8>,
}

impl Frames {
    fn new(width: u32, height: u32) -> Self {
        let blue: Vec<u8> = (0..width + height)
            .map(|d| ((d * 100 / (width + height).max(1)) + 60) as u8)
            .collect();
        let mut base = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            let green = (y * 200 / height.max(1) + 20) as u8;
            for &b in &blue[y as usize..(y + width) as usize] {
                base.extend_from_slice(&[0, green, b, 255]);
            }
        }
        Self { width, base }
    }

    fn frame(&self, n: u32) -> Vec<u8> {
        let red: Vec<u8> = (0..self.width)
            .map(|x| ((x * 200 / self.width.max(1)) + n).min(255) as u8)
            .collect();
        let mut out = self.base.clone();
        if self.width > 0 {
            for row in out.chunks_exact_mut(self.width as usize * 4) {
                for (px, &r) in row.chunks_exact_mut(4).zip(&red) {
                    px[0] = r;
                }
            }
        }
        out
    }
}

fn psnr_rgb(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let mut sum = 0f64;
    let mut n = 0f64;
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for c in 0..3 {
            let d = f64::from(pa[c]) - f64::from(pb[c]);
            sum += d * d;
            n += 1.0;
        }
    }
    let mse = sum / n;
    if mse == 0.0 {
        return f64::INFINITY;
    }
    20.0 * 255f64.log10() - 10.0 * mse.log10()
}

fn rgba_of(picture: &tilo_media::DecodedPicture) -> Vec<u8> {
    match &picture.frame.payload {
        FramePayload::Rgba(b) => b.to_vec(),
        other => panic!("asked for RGBA, got {other:?}"),
    }
}

struct RoundTrip {
    encoder_element: &'static str,
    decoder_element: &'static str,
    decoded: usize,
    last_psnr: f64,
    encode_times: Vec<Duration>,
    decode_times: Vec<Duration>,
}

fn round_trip(
    codec: StreamCodec,
    policy: RealtimePolicy,
    width: u32,
    height: u32,
    frames: u32,
) -> Result<RoundTrip, tilo_media::RealtimeError> {
    round_trip_from(
        codec,
        policy,
        InputFormat::Rgba,
        false,
        width,
        height,
        frames,
    )
}

/// `frame` in `format`'s byte order (BGR formats swap red and blue).
fn frame_as(format: InputFormat, width: u32, height: u32, n: u32) -> Vec<u8> {
    in_format(format, frame(width, height, n))
}

/// An RGBA `frame` in `format`'s byte order.
fn in_format(format: InputFormat, mut f: Vec<u8>) -> Vec<u8> {
    if matches!(format, InputFormat::Bgra | InputFormat::Bgrx) {
        for px in f.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
    }
    f
}

fn round_trip_from(
    codec: StreamCodec,
    policy: RealtimePolicy,
    format: InputFormat,
    dmabuf_path: bool,
    width: u32,
    height: u32,
    frames: u32,
) -> Result<RoundTrip, tilo_media::RealtimeError> {
    let mut config = EncoderConfig::new(codec, width, height, 30);
    config.policy = policy;
    config.input = format;
    config.dmabuf_input = dmabuf_path;
    config.bitrate_bps = 4_000_000;
    let mut encoder = RealtimeEncoder::new(&config)?;
    let mut decoder = RealtimeDecoder::new(DecoderConfig {
        codec,
        output: DecodeOutput::Rgba,
        policy,
        display_size: Some((width, height)),
    })?;
    // A receiver is told the stream's colour out of band, as the shell's
    // stream announcement does: not every encoder writes a colour
    // description (VA-API's do not) and not every decoder reads one (libaom's
    // `av1dec` guesses BT.601 below HD while `dav1ddec` reads BT.709).
    decoder.set_stream_color(Some(config.color));
    let mut decoded = 0;
    // Only the last picture's PSNR is reported, so it alone is scored, after
    // the loop; every picture is still checked to be RGBA of the input's size.
    let mut last_picture: Option<(u32, Vec<u8>)> = None;
    let source = Frames::new(width, height);
    let mut encode_times = Vec::new();
    let mut decode_times = Vec::new();
    for n in 0..frames {
        let input = source.frame(n);
        let packed = in_format(format, input.clone());
        let t = Instant::now();
        let units = encoder.encode(
            InputFrame::Packed(&packed),
            Duration::from_millis(u64::from(n) * 33),
        )?;
        encode_times.push(t.elapsed());
        assert!(
            units.len() <= 1,
            "{}: one frame in produced {} units",
            encoder.element().element,
            units.len()
        );
        if n == 0 {
            assert!(
                units.first().is_some_and(|u| u.keyframe),
                "{}: the first frame must be a keyframe",
                encoder.element().element
            );
        }
        for unit in units {
            let t = Instant::now();
            let picture = decoder.decode(&unit.data, Some(unit.keyframe))?;
            decode_times.push(t.elapsed());
            let picture = picture.unwrap_or_else(|| {
                panic!(
                    "{} produced no picture for frame {n} from {}",
                    decoder.element().map_or("?", |e| e.element),
                    encoder.element().element
                )
            });
            assert_eq!((picture.frame.width, picture.frame.height), (width, height));
            let rgba = rgba_of(&picture);
            assert_eq!(rgba.len(), input.len());
            last_picture = Some((n, rgba));
            decoded += 1;
        }
    }
    let last_psnr = last_picture.map_or(0.0, |(n, rgba)| psnr_rgb(&source.frame(n), &rgba));
    Ok(RoundTrip {
        encoder_element: encoder.element().element,
        decoder_element: decoder.element().map_or("?", |e| e.element),
        decoded,
        last_psnr,
        encode_times,
        decode_times,
    })
}

fn software_round_trip(codec: StreamCodec) {
    let rt = round_trip(codec, RealtimePolicy::SoftwareOnly, 320, 240, 30).unwrap_or_else(|e| {
        panic!(
            "the royalty-free software {} path must work without a GPU: {e}",
            codec.wire_label()
        )
    });
    assert_eq!(rt.decoded, 30, "every frame decoded, one for one");
    assert!(
        rt.last_psnr >= 30.0,
        "{} -> {}: PSNR {:.1} dB is below 30 dB",
        rt.encoder_element,
        rt.decoder_element,
        rt.last_psnr
    );
    eprintln!(
        "realtime: {} software round trip {} -> {}: 30/30 frames, PSNR {:.1} dB",
        codec.wire_label(),
        rt.encoder_element,
        rt.decoder_element,
        rt.last_psnr
    );
}

#[test]
fn software_codecs_round_trip_royalty_free_and_ten_bit() {
    // Phase `vp9_round_trips_in_software_without_a_gpu`.
    {
        software_round_trip(StreamCodec::Vp9);
    }
    // Phase `av1_round_trips_in_software_without_a_gpu`.
    {
        software_round_trip(StreamCodec::Av1);
    }
    // Phase `software_elements_are_the_royalty_free_tier`.
    {
        for codec in [StreamCodec::Vp9, StreamCodec::Av1] {
            let mut config = EncoderConfig::new(codec, 64, 64, 30);
            config.policy = RealtimePolicy::SoftwareOnly;
            let enc = RealtimeEncoder::new(&config).expect("software encoder");
            assert_eq!(
                enc.element().tier,
                CodecTier::RoyaltyFree,
                "{:?}",
                enc.element()
            );
            let dec = RealtimeDecoder::new(DecoderConfig {
                codec,
                output: DecodeOutput::Rgba,
                policy: RealtimePolicy::SoftwareOnly,
                display_size: None,
            })
            .expect("software decoder");
            assert_eq!(dec.element().unwrap().tier, CodecTier::RoyaltyFree);
        }
    }
    // Phase `ten_bit_output_is_p010_planes`.
    {
        let mut config = EncoderConfig::new(StreamCodec::Vp9, 64, 64, 30);
        config.policy = RealtimePolicy::SoftwareOnly;
        let mut enc = RealtimeEncoder::new(&config).expect("encoder");
        let unit = enc
            .encode(InputFrame::Packed(&frame(64, 64, 0)), Duration::ZERO)
            .expect("encode")
            .remove(0);
        let mut dec = RealtimeDecoder::new(DecoderConfig {
            codec: StreamCodec::Vp9,
            output: DecodeOutput::P010,
            policy: RealtimePolicy::SoftwareOnly,
            display_size: None,
        })
        .expect("decoder");
        let pic = dec
            .decode(&unit.data, Some(true))
            .expect("decode")
            .expect("picture");
        let planar = pic.frame.planar().expect("planar payload");
        assert_eq!(planar.format, tilo_media::PlanarFormat::P010);
        assert_eq!(planar.planes.len(), 2);
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn keyframes_and_hardware_round_trips_behave_for_realtime_streams() {
    // Phase `hardware_round_trips_where_the_device_has_them`.
    fn hardware_round_trips_where_the_device_has_them() {
        let codecs = hardware_codecs();
        if codecs.is_empty() {
            eprintln!("realtime: no hardware encode+decode listed on this machine; not run");
            return;
        }
        // RGBA (a CPU readback), BGRx (what a compositor renders), and BGRx
        // through the dmabuf-capable pipeline, which converts on the GPU.
        for (codec, format, dmabuf) in codecs.iter().flat_map(|c| {
            [
                (*c, InputFormat::Rgba, false),
                (*c, InputFormat::Bgrx, false),
                (*c, InputFormat::Bgrx, true),
            ]
        }) {
            let rt = round_trip_from(codec, RealtimePolicy::Auto, format, dmabuf, 1280, 720, 30)
                .unwrap_or_else(|e| {
                    panic!(
                        "{} hardware is listed but did not run: {e}",
                        codec.wire_label()
                    )
                });
            let caps_listing: Vec<&str> = tilo_media::hw::realtime_encoders(codec)
                .iter()
                .filter(|e| e.is_hardware())
                .map(|e| e.element)
                .collect();
            assert!(
                caps_listing.contains(&rt.encoder_element),
                "{}: a hardware encoder is listed ({caps_listing:?}) but {} ran",
                codec.wire_label(),
                rt.encoder_element
            );
            assert_eq!(rt.decoded, 30);
            assert!(
                rt.last_psnr >= 35.0,
                "{} from {format:?} (dmabuf pipeline {dmabuf}): PSNR {:.1} dB means a colour \
                 conversion is wrong",
                rt.encoder_element,
                rt.last_psnr
            );
            eprintln!(
                "realtime: {} hardware round trip from {format:?} (dmabuf pipeline {dmabuf}) {} -> {}: PSNR {:.1} dB",
                codec.wire_label(),
                rt.encoder_element,
                rt.decoder_element,
                rt.last_psnr
            );
        }
    }
    // Phase `a_forced_keyframe_lands_on_the_next_frame_only`.
    {
        let mut config = EncoderConfig::new(StreamCodec::Vp9, 160, 120, 30);
        config.policy = RealtimePolicy::SoftwareOnly;
        config.keyframe_interval = 0;
        let mut enc = RealtimeEncoder::new(&config).expect("encoder");
        let mut flags = Vec::new();
        for n in 0..8u32 {
            if n == 5 {
                enc.force_keyframe();
            }
            if n == 3 {
                // A live bitrate change must not disturb the stream.
                enc.set_bitrate(1_000_000);
            }
            let units = enc
                .encode(
                    InputFrame::Packed(&frame(160, 120, n)),
                    Duration::from_millis(u64::from(n) * 33),
                )
                .expect("encode");
            flags.push(units.first().expect("one unit per frame").keyframe);
        }
        assert_eq!(
            flags,
            [true, false, false, false, false, true, false, false],
            "keyframes exactly at the start and where one was forced"
        );
    }
    // Phase `a_late_joiner_decodes_nothing_until_a_keyframe`.
    {
        let mut config = EncoderConfig::new(StreamCodec::Vp9, 160, 120, 30);
        config.policy = RealtimePolicy::SoftwareOnly;
        let mut enc = RealtimeEncoder::new(&config).expect("encoder");
        let mut units = Vec::new();
        for n in 0..4u32 {
            if n == 3 {
                enc.force_keyframe();
            }
            units.extend(
                enc.encode(
                    InputFrame::Packed(&frame(160, 120, n)),
                    Duration::from_millis(u64::from(n) * 33),
                )
                .expect("encode"),
            );
        }
        let mut dec = RealtimeDecoder::new(DecoderConfig {
            codec: StreamCodec::Vp9,
            output: DecodeOutput::Rgba,
            policy: RealtimePolicy::SoftwareOnly,
            display_size: None,
        })
        .expect("decoder");
        // Join after the first keyframe: frames 1 and 2 reference what we never saw.
        for u in &units[1..3] {
            assert!(!u.keyframe);
            assert!(
                dec.decode(&u.data, None).expect("no error").is_none(),
                "a delta frame before any keyframe must not produce garbage"
            );
        }
        assert!(units[3].keyframe);
        assert!(dec.decode(&units[3].data, None).expect("decode").is_some());
    }
    // Phase `hardware_round_trips_where_the_device_has_them` (defined above).
    hardware_round_trips_where_the_device_has_them();
}

/// Hardware paths this machine lists, never substituted.
fn hardware_codecs() -> Vec<StreamCodec> {
    let caps = local_capabilities();
    StreamCodec::ALL
        .into_iter()
        .filter(|c| caps.hardware_encode.contains(c) && caps.hardware_decode.contains(c))
        .collect()
}

fn percentile(times: &[Duration], p: f64) -> Duration {
    let mut v = times.to_vec();
    v.sort();
    let idx = ((v.len() as f64 - 1.0) * p).round() as usize;
    v[idx.min(v.len() - 1)]
}

/// Per-frame latency of every real-time path this machine has, at 720p.
///
/// The first frame (element start-up) is excluded. The bound is loose — a
/// shared CI runner is slow and noisy — and exists to catch an element that
/// buffers frames (which shows up as a missing picture, or as whole frame
/// intervals of delay), not to benchmark.
#[test]
fn realtime_latency_per_frame() {
    let mut paths = vec![
        (StreamCodec::Vp9, RealtimePolicy::SoftwareOnly),
        (StreamCodec::Av1, RealtimePolicy::SoftwareOnly),
    ];
    for c in hardware_codecs() {
        paths.push((c, RealtimePolicy::Auto));
    }
    for (codec, policy) in paths {
        let rt = round_trip(codec, policy, 1280, 720, 60).expect("path listed as available");
        let enc = &rt.encode_times[1..];
        let dec = &rt.decode_times[1..];
        let enc_med = percentile(enc, 0.5);
        let dec_med = percentile(dec, 0.5);
        eprintln!(
            "realtime latency 1280x720 {:>11} {:>10} -> {:<10} encode p50 {:>6.2} ms p95 {:>6.2} ms | decode p50 {:>6.2} ms p95 {:>6.2} ms",
            codec.wire_label(),
            rt.encoder_element,
            rt.decoder_element,
            enc_med.as_secs_f64() * 1e3,
            percentile(enc, 0.95).as_secs_f64() * 1e3,
            dec_med.as_secs_f64() * 1e3,
            percentile(dec, 0.95).as_secs_f64() * 1e3,
        );
        assert!(
            enc_med + dec_med < Duration::from_millis(250),
            "{}: {enc_med:?} + {dec_med:?} per frame is not real time",
            rt.encoder_element
        );
    }
}

/// The compositor's zero-copy path: a `BGRx` frame in a GPU-tiled dmabuf (the
/// layout a compositor renders into) imported by `vapostproc` and encoded on
/// the GPU, decoded back and compared. Runs where VA-API lists a hardware
/// encoder; otherwise says so.
#[cfg(unix)]
#[test]
fn a_gpu_dmabuf_is_encoded_without_a_cpu_copy_on_va_hardware() {
    use std::os::fd::AsFd;
    use tilo_media::realtime::{CodecFamily, DmabufInput, DmabufInputPlane};

    let caps = local_capabilities();
    let Some(codec) = [StreamCodec::Av1, StreamCodec::H264, StreamCodec::Vp9]
        .into_iter()
        .find(|c| {
            caps.hardware_encode.contains(c)
                && tilo_media::hw::realtime_encoders(*c).iter().any(|e| {
                    e.family == CodecFamily::Va && tilo_media::encode::registry_has(e.element)
                })
        })
    else {
        eprintln!("realtime: no VA-API hardware encoder listed; dmabuf import not run");
        return;
    };
    let (w, h) = (640u32, 360u32);
    let bgrx = frame_as(InputFormat::Bgrx, w, h, 0);
    let image = tilo_media::realtime::upload_to_dmabuf(InputFormat::Bgrx, &bgrx, w, h)
        .expect("VA-API is listed, so vapostproc can allocate a dmabuf");

    let mut config = EncoderConfig::new(codec, w, h, 30);
    config.input = InputFormat::Bgrx;
    config.dmabuf_input = true;
    config.policy = RealtimePolicy::Prefer(CodecFamily::Va);
    let mut enc = RealtimeEncoder::new(&config).expect("VA encoder");
    assert!(enc.element().is_hardware(), "{:?}", enc.element());
    assert!(
        enc.accepts_dmabuf(),
        "a VA encoder with vapostproc imports dmabufs"
    );
    assert!(
        enc.accepts_dmabuf_format(image.fourcc, image.modifier),
        "the driver's own layout {:#x} is importable",
        image.modifier
    );

    let planes: Vec<DmabufInputPlane<'_>> = image
        .planes
        .iter()
        .map(|p| DmabufInputPlane {
            fd: p.fd.as_fd(),
            offset: p.offset,
            stride: p.stride,
        })
        .collect();
    let units = enc
        .encode(
            InputFrame::Dmabuf(DmabufInput {
                planes: &planes,
                fourcc: image.fourcc,
                modifier: image.modifier,
                width: w,
                height: h,
            }),
            Duration::ZERO,
        )
        .expect("dmabuf encode");
    let unit = units.first().expect("one unit");
    assert!(unit.keyframe);

    // A layout the post-processor does not list is refused up front, and
    // the encoder keeps working for packed frames.
    if !enc.accepts_dmabuf_format(image.fourcc, tilo_media::dmabuf::DRM_FORMAT_MOD_LINEAR) {
        let refused = enc.encode(
            InputFrame::Dmabuf(DmabufInput {
                planes: &planes,
                fourcc: image.fourcc,
                modifier: tilo_media::dmabuf::DRM_FORMAT_MOD_LINEAR,
                width: w,
                height: h,
            }),
            Duration::from_millis(33),
        );
        assert!(
            matches!(refused, Err(tilo_media::RealtimeError::InvalidInput(_))),
            "{refused:?}"
        );
        let packed = enc
            .encode(InputFrame::Packed(&bgrx), Duration::from_millis(66))
            .expect("packed frames still encode");
        assert_eq!(packed.len(), 1);
    }

    let mut dec = RealtimeDecoder::new(DecoderConfig {
        display_size: Some((w, h)),
        ..DecoderConfig::new(codec)
    })
    .expect("decoder");
    let pic = dec
        .decode(&unit.data, Some(true))
        .expect("decode")
        .expect("picture");
    let psnr = psnr_rgb(&frame(w, h, 0), &rgba_of(&pic));
    eprintln!(
        "realtime: {} dmabuf {}:{:#x} imported by {} -> {}: PSNR {psnr:.1} dB",
        codec.wire_label(),
        tilo_media::dmabuf::fourcc_str(image.fourcc),
        image.modifier,
        enc.element().element,
        dec.element().map_or("?", |e| e.element)
    );
    // One intra frame through two GPU colour conversions lands near 30 dB; a
    // misread layout (wrong stride, tiling or channel order) lands below 15.
    assert!(psnr >= 26.0, "the imported dmabuf's pixels arrived intact");
}
