//! An announced stream colour drives the decoder's conversion.
//!
//! `GStreamer`'s VA-API encoders write no colour description, so a decoder
//! guesses from the picture size: BT.601 below HD. For an SD stream encoded
//! as BT.709 that guess shifts every colour. `set_stream_color` replaces the
//! guess with what the sender announced. Runs where a VA-API hardware encoder
//! is listed; otherwise it says so and returns.
#![cfg(feature = "realtime")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::Duration;

use tilo_media::FramePayload;
use tilo_media::realtime::{
    CodecFamily, DecodeOutput, DecoderConfig, EncoderConfig, InputFrame, RealtimeDecoder,
    RealtimeEncoder, RealtimePolicy, StreamCodec, StreamColor, local_capabilities,
};

const W: u32 = 640;
const H: u32 = 360;

fn frame() -> Vec<u8> {
    let mut out = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            out.extend_from_slice(&[
                (x * 200 / W + 20) as u8,
                (y * 200 / H + 20) as u8,
                ((x + y) * 100 / (W + H) + 60) as u8,
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
    20.0 * 255f64.log10() - 10.0 * (sum / n).log10()
}

#[test]
fn the_announced_colour_replaces_the_decoders_size_based_guess() {
    let caps = local_capabilities();
    let Some(codec) = [StreamCodec::H264, StreamCodec::Av1].into_iter().find(|c| {
        caps.hardware_encode.contains(c)
            && tilo_media::hw::realtime_encoders(*c)
                .iter()
                .any(|e| e.family == CodecFamily::Va && tilo_media::encode::registry_has(e.element))
    }) else {
        eprintln!("no VA-API hardware encoder listed here; not run");
        return;
    };
    let mut config = EncoderConfig::new(codec, W, H, 30);
    config.policy = RealtimePolicy::Prefer(CodecFamily::Va);
    let mut enc = RealtimeEncoder::new(&config).expect("VA encoder");
    let input = frame();
    let unit = enc
        .encode(InputFrame::Packed(&input), Duration::ZERO)
        .expect("encode")
        .remove(0);

    let decode = |announced: Option<StreamColor>| {
        let mut dec = RealtimeDecoder::new(DecoderConfig {
            codec,
            output: DecodeOutput::Rgba,
            policy: RealtimePolicy::Auto,
            display_size: Some((W, H)),
        })
        .expect("decoder");
        dec.set_stream_color(announced);
        let pic = dec.decode(&unit.data, Some(true)).unwrap().unwrap();
        let FramePayload::Rgba(rgba) = &pic.frame.payload else {
            panic!("RGBA output");
        };
        (psnr(&input, rgba), pic.color)
    };
    let (guessed, guessed_color) = decode(None);
    let (announced, announced_color) = decode(Some(StreamColor::BT709));
    eprintln!(
        "{} via {}: decoder's guess {guessed_color:?} -> {guessed:.1} dB, announced BT.709 -> {announced:.1} dB",
        codec.wire_label(),
        enc.element().element
    );
    assert_eq!(announced_color, StreamColor::BT709);
    assert!(announced >= 35.0, "announced colour: {announced:.1} dB");
    assert!(
        announced >= guessed,
        "the announcement must not make the picture worse"
    );
}
