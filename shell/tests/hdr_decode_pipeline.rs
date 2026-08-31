//! HEVC Main10 decode pipeline test: round-trips a 64x64 gradient 10-bit
//! YUV frame through libx265 -> HEVC Main10 NALUs -> `HevcDecoder` ->
//! P010, asserting the output is genuinely 10-bit-wide (the top 6 bits of
//! luma carry content, not the always-zero pattern an 8-bit-in-16-bit
//! downconvert leaves).
//!
//! Fedora configures `libavcodec-free` with
//! `--disable-decoder='h264,hevc,vc1,vvc'` and no libx265, so on a stock
//! image none of this is observable. The gate is explicit rather than a
//! quiet self-skip — under the feature a missing codec is a failure, the
//! feature being the caller asserting the codec is there:
//!
//! ```text
//! cargo test -p shell --features hevc-codec-tests --test hdr_decode_pipeline
//! ```
#![cfg(all(feature = "hdr-decode", feature = "hevc-codec-tests"))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use ffmpeg_next as ffmpeg;
use ffmpeg_next::ffi;
use shell::network::decoder::{DecodedPixels, HevcDecoder};

/// libx265's 10-bit mode wants Y + half-res U/V, each sample in the
/// LOWER 10 bits of a native-LE u16.
fn gradient_frame_yuv420p10(width: u32, height: u32) -> ffmpeg::frame::Video {
    let mut frame = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::YUV420P10LE, width, height);

    // Diagonal gradient covering the full 10-bit range, so the round-trip
    // has non-uniform content to preserve.
    let y_stride = frame.stride(0);
    let range = (width as u32 + height as u32).saturating_sub(2).max(1);
    {
        let y = frame.data_mut(0);
        for row in 0..height as usize {
            for col in 0..width as usize {
                let sum = (row + col) as u32;
                let value = ((sum * 1023) / range).min(1023) as u16;
                let off = row * y_stride + col * 2;
                let bytes = value.to_le_bytes();
                y[off] = bytes[0];
                y[off + 1] = bytes[1];
            }
        }
    }
    // U + V — neutral 512 in 10-bit (half-resolution).
    let u_stride = frame.stride(1);
    let v_stride = frame.stride(2);
    let uv_width = (width / 2) as usize;
    let uv_height = (height / 2) as usize;
    {
        let u = frame.data_mut(1);
        for row in 0..uv_height {
            for col in 0..uv_width {
                let value: u16 = 512;
                let off = row * u_stride + col * 2;
                let bytes = value.to_le_bytes();
                u[off] = bytes[0];
                u[off + 1] = bytes[1];
            }
        }
    }
    {
        let v = frame.data_mut(2);
        for row in 0..uv_height {
            for col in 0..uv_width {
                let value: u16 = 512;
                let off = row * v_stride + col * 2;
                let bytes = value.to_le_bytes();
                v[off] = bytes[0];
                v[off + 1] = bytes[1];
            }
        }
    }
    frame
}

/// `None` if this host's libavcodec lacks libx265.
fn encode_hevc_main10_frames(width: u32, height: u32, num_frames: u32) -> Option<Vec<u8>> {
    let _ = ffmpeg::init();
    // The encoder-registry name matches the FFmpeg CLI's `-c:v libx265`; a
    // build without it returns null and we self-skip.
    let cname = std::ffi::CString::new("libx265").ok()?;
    let ptr = unsafe { ffi::avcodec_find_encoder_by_name(cname.as_ptr()) };
    if ptr.is_null() {
        eprintln!("SKIP: libavcodec has no libx265 encoder built in");
        return None;
    }
    let codec_ff = unsafe { ffmpeg::Codec::wrap(ptr as *mut _) };
    let ctx = ffmpeg::codec::Context::new_with_codec(codec_ff);

    let mut video = ctx.encoder().video().ok()?;
    video.set_width(width);
    video.set_height(height);
    video.set_format(ffmpeg::format::Pixel::YUV420P10LE);
    video.set_time_base(ffmpeg::Rational::new(1, 30));
    video.set_frame_rate(Some(ffmpeg::Rational::new(30, 1)));
    video.set_gop(1); // one keyframe up front + every P-frame is fine
    video.set_max_b_frames(0);
    video.set_bit_rate(4_000_000);
    // Tag colorimetry so the decoder side sees BT.2020 + PQ.
    unsafe {
        let cctx = video.as_mut_ptr();
        (*cctx).color_primaries = ffi::AVColorPrimaries::AVCOL_PRI_BT2020;
        (*cctx).color_trc = ffi::AVColorTransferCharacteristic::AVCOL_TRC_SMPTE2084;
        (*cctx).colorspace = ffi::AVColorSpace::AVCOL_SPC_BT2020_NCL;
        (*cctx).color_range = ffi::AVColorRange::AVCOL_RANGE_MPEG;
    }

    let mut opts = ffmpeg::Dictionary::new();
    // "ultrafast" + "no-info" keep the bitstream compact at 64x64.
    opts.set("preset", "ultrafast");
    opts.set("tune", "zerolatency");
    opts.set("x265-params", "log-level=none:keyint=1");
    let encoder = video.open_as_with(codec_ff, opts).ok()?;
    let mut opened = encoder;

    let mut out = Vec::new();
    for i in 0..num_frames {
        let mut frame = gradient_frame_yuv420p10(width, height);
        // Same gradient every frame: libx265 still motion-encodes each into a
        // distinct P-frame.
        let _ = i;
        unsafe {
            (*frame.as_mut_ptr()).pts = i as i64;
        }
        opened.send_frame(&frame).ok()?;
        drain_packets(&mut opened, &mut out);
    }
    opened.send_eof().ok()?;
    drain_packets(&mut opened, &mut out);

    Some(out)
}

fn drain_packets(enc: &mut ffmpeg::codec::encoder::Video, out: &mut Vec<u8>) {
    let mut pkt = ffmpeg::packet::Packet::empty();
    while enc.receive_packet(&mut pkt).is_ok() {
        if let Some(data) = pkt.data() {
            out.extend_from_slice(data);
        }
    }
}

#[test]
fn hevc_main10_round_trip_preserves_10_bit_precision() {
    const W: u32 = 64;
    const H: u32 = 64;

    // `hevc-codec-tests` states this build has an HEVC-capable libavcodec,
    // so a missing encoder is a failure, not a quiet pass.
    let nalus = encode_hevc_main10_frames(W, H, 5).expect(
        "libx265 HEVC Main10 encoder missing — `hevc-codec-tests` asserts \
         this libavcodec has one",
    );
    assert!(
        !nalus.is_empty(),
        "libx265 produced empty bitstream — test setup wrong"
    );

    let mut dec = HevcDecoder::new().expect("HEVC decoder builds");
    let mut got_frame = false;
    // libavcodec may buffer the first packet, so retry until a frame lands.
    for retry in 0..8 {
        if let Some(frame) = dec.decode(&nalus) {
            assert_eq!(frame.width, W);
            assert_eq!(frame.height, H);
            got_frame = true;
            // Genuinely 10-bit: an 8-bit downcast would leave the top 6 bits of Y
            // all zero.
            match &frame.pixels {
                DecodedPixels::P010 { y, .. } => {
                    let all_top6_zero = y.iter().all(|s| (*s >> 10) == 0);
                    assert!(
                        !all_top6_zero,
                        "top 6 bits of every luma sample are zero — decoder likely downcast to 8-bit"
                    );
                    // And the gradient's precision survived, not just its top bits.
                    let mut buckets = std::collections::HashSet::new();
                    for s in y.iter().take(4096) {
                        buckets.insert(*s >> 6);
                    }
                    assert!(
                        buckets.len() >= 32,
                        "gradient yielded only {} unique luma codes — decode may have lost precision",
                        buckets.len()
                    );
                }
                other => panic!("HEVC decoder returned non-P010 pixels: {other:?}"),
            }
            // Colorimetry — the encoder tagged BT.2020 + PQ.
            let cm = dec.last_colorimetry().expect("colorimetry captured");
            assert!(
                cm.is_hdr(),
                "decoder should report HDR colorimetry, got {cm:?}"
            );
            break;
        }
        eprintln!("retry {retry}: no frame yet");
    }
    assert!(
        got_frame,
        "HEVC decoder never produced a frame from encoded NALUs"
    );
}

#[test]
fn hevc_decoder_returns_none_for_empty_input() {
    let mut dec = HevcDecoder::new().expect("HEVC decoder builds");
    assert!(dec.decode(&[]).is_none());
}

#[test]
fn video_decoder_factory_picks_hevc_from_wire_label() {
    use shell::network::decoder::VideoDecoder;
    let d = VideoDecoder::for_wire_codec("hevc-main10").expect("hevc-main10 works");
    match d {
        VideoDecoder::Hevc(_) => {}
        _ => panic!("wire label hevc-main10 should build a Hevc decoder"),
    }
}
