//! HDR decode through `GStreamer`: a native 10-bit PQ stream (P010 in,
//! BT.2100 PQ caps, HDR10 metadata) encoded on video hardware and decoded to
//! P010 by every decoder this machine has for it — hardware and, for AV1,
//! dav1d — must keep its 10-bit samples.
//!
//! The colour the decoder reports is printed, not asserted: measured with
//! `GStreamer` 1.28 on radeonsi, `vaav1enc` and `vah265enc` write neither a
//! colour description nor HDR10 metadata into the bitstream, so decoders
//! report their SD/HD defaults. The share wire carries the colorimetry and
//! HDR10 metadata out of band (`SurfaceStreamStart`), and the receiver
//! applies them (`VideoDecoder::set_stream_colorimetry`).
//!
//! Runs only where a 10-bit hardware encoder (AV1 or HEVC Main10) is
//! listed; otherwise it says so and returns. It never substitutes an 8-bit
//! path and reports success.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::cast_possible_truncation,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::Duration;

use media::realtime::{
    DecodeOutput, DecoderConfig, EncoderConfig, HdrStaticMetadata, InputFormat, InputFrame,
    RealtimeDecoder, RealtimeEncoder, RealtimePolicy, StreamColor, local_capabilities,
};
use media::{FramePayload, PlanarFormat, StreamCodec};

const W: u32 = 640;
const H: u32 = 360;

/// A P010 frame: a horizontal luma ramp across the full 10-bit video range
/// (64..=940), so neighbouring columns differ by less than one 8-bit step,
/// over neutral chroma. Samples sit in the high bits.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "the ramp spans the fixed 10-bit video range over a constant width, far from u32 overflow"
)]
fn p010_ramp() -> Vec<u8> {
    let mut out = Vec::with_capacity((W * H * 3) as usize);
    for _ in 0..H {
        for x in 0..W {
            let y = 64 + x * (940 - 64) / (W - 1);
            out.extend_from_slice(&((y as u16) << 6).to_le_bytes());
        }
    }
    for _ in 0..H / 2 {
        for _ in 0..W / 2 {
            out.extend_from_slice(&(512u16 << 6).to_le_bytes());
            out.extend_from_slice(&(512u16 << 6).to_le_bytes());
        }
    }
    out
}

fn hdr10() -> HdrStaticMetadata {
    HdrStaticMetadata {
        // BT.2020 G, B, R and D65 in 0.00002 units.
        display_primaries: [(8500, 39850), (6550, 2300), (35400, 14600)],
        white_point: (15635, 16450),
        max_luminance: 10_000_000,
        min_luminance: 50,
        max_cll: 1000,
        max_fall: 400,
    }
}

#[test]
fn ten_bit_pq_survives_every_decoder() {
    let caps = local_capabilities();
    let codecs: Vec<_> = [StreamCodec::Av1, StreamCodec::HevcMain10]
        .into_iter()
        .filter(|c| caps.hardware_encode.contains(c))
        .collect();
    if codecs.is_empty() {
        eprintln!("no 10-bit hardware encoder listed here; not run");
        return;
    }
    let source = p010_ramp();
    for codec in codecs {
        let mut config = EncoderConfig::new(codec, W, H, 30);
        config.input = InputFormat::P010;
        config.color = StreamColor::BT2100_PQ;
        config.hdr = Some(hdr10());
        config.bitrate_bps = 20_000_000;
        config.policy = RealtimePolicy::Auto;
        let mut enc = RealtimeEncoder::new(&config)
            .unwrap_or_else(|e| panic!("{codec:?} 10-bit hardware is listed but failed: {e}"));
        assert!(enc.element().is_hardware(), "{:?}", enc.element());
        let unit = enc
            .encode(InputFrame::Packed(&source), Duration::ZERO)
            .expect("encode")
            .remove(0);

        for policy in [RealtimePolicy::Auto, RealtimePolicy::SoftwareOnly] {
            let Ok(mut dec) = RealtimeDecoder::new(DecoderConfig {
                codec,
                output: DecodeOutput::P010,
                policy,
                display_size: Some((W, H)),
            }) else {
                eprintln!("{codec:?}: no decoder under {policy:?}; not run");
                continue;
            };
            let element = dec.element().map_or("?", |e| e.element);
            let Some(picture) = dec.decode(&unit.data, Some(true)).expect("decode") else {
                panic!("{element} produced no picture for {codec:?}");
            };
            let FramePayload::Planar(image) = &picture.frame.payload else {
                panic!("{element}: P010 output is planar");
            };
            assert_eq!(image.format, PlanarFormat::P010);
            let luma = &image.planes[0];
            let row: Vec<u16> = luma
                .bytes
                .chunks_exact(2)
                .take(W as usize)
                .map(|b| u16::from_le_bytes([b[0], b[1]]) >> 6)
                .collect();
            // The ramp climbs about 1.4 ten-bit steps per column: an 8-bit
            // decode could only produce multiples of four.
            let ten_bit = row.iter().any(|v| v % 4 != 0);
            let max_err = row
                .iter()
                .enumerate()
                .map(|(x, v)| {
                    let want = 64 + (x as u32) * (940 - 64) / (W - 1);
                    (i64::from(*v) - i64::from(want)).unsigned_abs()
                })
                .max()
                .unwrap();
            eprintln!(
                "{} -> {element}: 10-bit samples {ten_bit}, max luma error {max_err}/1023, colour {:?}",
                enc.element().element,
                picture.color
            );
            assert!(ten_bit, "{element}: the picture was squeezed to 8 bits");
            assert!(max_err <= 24, "{element}: luma is off by {max_err}");
        }
    }
}
