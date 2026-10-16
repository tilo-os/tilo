//! The shared-surface encoder on real video hardware.
//!
//! Every test here runs only where the `GStreamer` registry lists a hardware
//! encoder for the codec (the VA-API and NVCODEC plugins list one only when
//! a device reports the capability) and otherwise says so and returns. None
//! substitutes a software element and reports success: each asserts that
//! the element that ran is hardware.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "in a test these are the assertion, not a hazard"
)]

use media::StreamCodec;
use media::realtime::{CodecFamily, local_capabilities};
use shell::collab::encoder::{EncoderInit, FrameInput, build};
use shell::config::EncoderChoice;
use tilo_callstream::surface_decoder::{DecodedPixels, VideoDecoder};

const W: u32 = 640;
const H: u32 = 360;

#[allow(
    clippy::arithmetic_side_effects,
    reason = "the frame size is a small test constant, so every channel value stays well inside u32"
)]
fn gradient(w: u32, h: u32, n: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            out.extend_from_slice(&[
                ((x * 200 / w) + n).min(255) as u8,
                (y * 200 / h + 20) as u8,
                ((x + y) * 100 / (w + h) + 60) as u8,
                255,
            ]);
        }
    }
    out
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let (mut sum, mut n) = (0f64, 0f64);
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for c in 0..3 {
            let d = f64::from(pa[c]) - f64::from(pb[c]);
            sum += d * d;
            n += 1.0;
        }
    }
    if sum == 0.0 {
        return f64::INFINITY;
    }
    20.0 * 255f64.log10() - 10.0 * (sum / n).log10()
}

fn hardware_codecs() -> Vec<StreamCodec> {
    let caps = local_capabilities();
    StreamCodec::ALL
        .into_iter()
        .filter(|c| caps.hardware_encode.contains(c))
        .collect()
}

/// SDR: every hardware encoder listed here encodes through the shell's
/// encoder, and the shell's decoder recovers the picture at the announced
/// size.
#[test]
fn every_listed_hardware_encoder_round_trips_through_the_shell() {
    let codecs = hardware_codecs();
    if codecs.is_empty() {
        eprintln!("no hardware encoder listed here; not run");
        return;
    }
    for codec in codecs {
        if codec == StreamCodec::HevcMain10 {
            continue; // HDR only; covered below.
        }
        let mut enc = build(
            EncoderInit::sdr(codec, W, H, 30, 4000, 0),
            EncoderChoice::Auto,
        )
        .unwrap_or_else(|e| panic!("{codec:?} hardware is listed but did not build: {e}"));
        assert!(
            enc.kind().is_hardware(),
            "{codec:?} ran on {:?}",
            enc.kind()
        );
        let mut dec = VideoDecoder::new(codec, false);
        dec.set_display_size(Some((W, H)));
        // What the receiver learns from `SurfaceStreamStart`.
        dec.set_stream_colorimetry(Some(shell::collab::colorimetry::Colorimetry::sdr_bt709()));
        let mut last = None;
        for n in 0..10 {
            let input = gradient(W, H, n);
            let mut unit = Vec::new();
            let stats = enc
                .encode(FrameInput::Rgba(&input), &mut unit)
                .expect("encode");
            if n == 0 {
                assert!(stats.keyframe, "the first frame is a keyframe");
            }
            let frame = dec.decode(&unit).expect("decode");
            assert_eq!((frame.width, frame.height), (W, H));
            let DecodedPixels::Rgba8(rgba) = frame.pixels else {
                panic!("SDR decodes to RGBA");
            };
            last = Some(psnr(&input, &rgba));
        }
        let score = last.unwrap();
        eprintln!(
            "{} via {}: PSNR {score:.1} dB",
            codec.wire_label(),
            enc.kind().as_str()
        );
        assert!(score >= 35.0, "{codec:?}: PSNR {score:.1} dB");
    }
}

/// Byte offsets of HDR static metadata in an encoded unit: HEVC prefix SEI
/// payload 137 (mastering display) or an AV1 metadata OBU of type
/// `HDR_MDCV` (2).
fn carries_mastering_metadata(codec: StreamCodec, unit: &[u8]) -> bool {
    match codec {
        StreamCodec::HevcMain10 => unit.windows(6).any(|w| {
            // Start code, PREFIX_SEI_NUT (39) header, first payload type.
            w[..3] == [0, 0, 1] && (w[3] >> 1) & 0x3f == 39 && w[5] == 137
        }),
        StreamCodec::Av1 => unit.windows(3).any(|w| {
            // OBU_METADATA (5) with a size field, then metadata_type 2 as
            // a one-byte leb128 after a one-byte size.
            (w[0] >> 3) & 0x0f == 5 && w[0] & 0x02 != 0 && w[2] == 2
        }),
        _ => false,
    }
}

/// HDR: 10-bit hardware encode (AV1 or HEVC Main10) with HDR10 metadata,
/// decoded to P010 without being squeezed to 8 bits, reporting PQ.
#[test]
fn hdr_ten_bit_hardware_encode_survives_decode() {
    let caps = local_capabilities();
    let codecs: Vec<_> = [StreamCodec::Av1, StreamCodec::HevcMain10]
        .into_iter()
        .filter(|c| caps.hardware_encode.contains(c))
        .collect();
    if codecs.is_empty() {
        eprintln!("no 10-bit hardware encoder listed here; not run");
        return;
    }
    let hdr = shell::collab::hdr_metadata::HdrConfig::pq_reference();
    for codec in codecs {
        let mut enc = build(
            EncoderInit::hdr10(codec, W, H, 30, 8000, 0, hdr),
            EncoderChoice::Auto,
        )
        .unwrap_or_else(|e| panic!("{codec:?} 10-bit hardware did not build: {e}"));
        assert!(enc.kind().is_hardware());
        let mut unit = Vec::new();
        enc.encode(FrameInput::Rgba(&gradient(W, H, 0)), &mut unit)
            .expect("encode");
        let metadata = carries_mastering_metadata(codec, &unit);
        eprintln!(
            "{}: HDR10 mastering metadata in the bitstream: {metadata}",
            codec.wire_label()
        );

        let mut dec = VideoDecoder::new(codec, true);
        dec.set_display_size(Some((W, H)));
        // What the receiver learns from `SurfaceStreamStart`.
        dec.set_stream_colorimetry(Some(hdr.colorimetry()));
        let frame = dec.decode(&unit).expect("decode");
        let DecodedPixels::P010 { y, y_stride, .. } = &frame.pixels else {
            panic!("a 10-bit stream decodes to P010");
        };
        assert!(*y_stride >= W, "stride in samples covers the width");
        // Captures are 8-bit RGB; whether the conversion into PQ fills the
        // two extra bits is reported, not asserted. `hdr_decode_pipeline`
        // proves 10-bit survival with a native 10-bit source.
        let uses_ten_bits = y
            .iter()
            .take((y_stride * H) as usize)
            .any(|s| (s >> 6) & 0b11 != 0);
        eprintln!(
            "{}: decoded {}x{} P010, colorimetry {:?}, low bits in use: {uses_ten_bits}",
            codec.wire_label(),
            frame.width,
            frame.height,
            frame.colorimetry
        );
        assert!(
            frame.colorimetry.is_hdr(),
            "{codec:?}: the receiver ended up with {:?}, not PQ",
            frame.colorimetry
        );
    }
}

/// The compositor's zero-copy path: a GPU-tiled `BGRx` dmabuf imported by
/// the shell encoder through VA-API.
#[test]
fn a_gpu_dmabuf_reaches_the_va_encoder_without_a_copy() {
    use std::os::fd::AsRawFd;
    let caps = local_capabilities();
    let Some(codec) = [StreamCodec::Av1, StreamCodec::H264, StreamCodec::Vp9]
        .into_iter()
        .find(|c| {
            caps.hardware_encode.contains(c)
                && media::hw::realtime_encoders(*c)
                    .iter()
                    .any(|e| e.family == CodecFamily::Va && media::encode::registry_has(e.element))
        })
    else {
        eprintln!("no VA-API hardware encoder listed here; not run");
        return;
    };
    let mut bgrx = gradient(W, H, 0);
    for px in bgrx.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let image = media::realtime::upload_to_dmabuf(media::InputFormat::Bgrx, &bgrx, W, H)
        .expect("VA-API is listed, so vapostproc allocates a dmabuf");
    let mut init = EncoderInit::sdr(codec, W, H, 30, 4000, 0);
    init.dmabuf_input = true;
    let mut enc = build(init, EncoderChoice::Vaapi).expect("VA encoder");
    assert!(enc.kind().is_hardware());
    assert!(enc.accepts_dmabuf(image.fourcc, image.modifier));
    let mut plane_fds = [-1; 4];
    let mut plane_offsets = [0u64; 4];
    let mut plane_strides = [0u32; 4];
    for (i, p) in image.planes.iter().enumerate().take(4) {
        plane_fds[i] = p.fd.as_raw_fd();
        plane_offsets[i] = u64::from(p.offset);
        plane_strides[i] = p.stride;
    }
    let mut unit = Vec::new();
    let stats = enc
        .encode(
            FrameInput::DmabufFd {
                plane_fds,
                plane_offsets,
                plane_strides,
                num_planes: image.planes.len() as u8,
                drm_fourcc: image.fourcc,
                drm_modifier: image.modifier,
                width: W,
                height: H,
            },
            &mut unit,
        )
        .expect("dmabuf encode");
    assert!(stats.keyframe);
    // A readback frame still encodes on the same (BGRx) pipeline.
    let mut readback = Vec::new();
    enc.encode(FrameInput::Rgba(&gradient(W, H, 1)), &mut readback)
        .expect("RGBA fallback on the dmabuf pipeline");
    let mut dec = VideoDecoder::new(codec, false);
    dec.set_display_size(Some((W, H)));
    dec.set_stream_colorimetry(Some(shell::collab::colorimetry::Colorimetry::sdr_bt709()));
    let frame = dec.decode(&unit).expect("decode");
    let DecodedPixels::Rgba8(rgba) = frame.pixels else {
        panic!("SDR decodes to RGBA");
    };
    let score = psnr(&gradient(W, H, 0), &rgba);
    eprintln!(
        "dmabuf {} via VA-API: PSNR {score:.1} dB",
        codec.wire_label()
    );
    assert!(score >= 35.0, "the imported pixels arrived intact");
}
