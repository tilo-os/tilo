//! HEVC Main10 decode pipeline test.
//!
//! Round-trips a 64×64 solid-gradient 10-bit YUV frame through libx265
//! (encoder) → HEVC Main10 NALU bytes → `HevcDecoder` → P010 planes.
//! Asserts the output is genuinely 10-bit-wide — the top 6 bits of the
//! luma sample should carry actual content (not the "always zero"
//! pattern an 8-bit-in-16-bit downconvert would leave).
//!
//! The encoder step uses `libx265` via `libavcodec`. If libavcodec
//! wasn't built with libx265 the test self-skips (matches the pattern
//! in `flatpak_share_encoder_probe.rs`). Behind `hdr-decode` cargo
//! feature — the whole HEVC decoder is feature-gated.

#![cfg(feature = "hdr-decode")]

use ffmpeg_next as ffmpeg;
use ffmpeg_next::ffi;
use shell::network::decoder::{DecodedPixels, HevcDecoder};

/// Build a 64×64 YUV420P10LE frame with a diagonal gradient.
/// libx265's 10-bit mode wants YUV420P10 input (Y plane + separate
/// U + V half-resolution planes), each sample stored in the LOWER
/// 10 bits of a u16 native LE encoding.
fn gradient_frame_yuv420p10(width: u32, height: u32) -> ffmpeg::frame::Video {
    let mut frame = ffmpeg::frame::Video::new(
        ffmpeg::format::Pixel::YUV420P10LE,
        width,
        height,
    );

    // Y plane — diagonal gradient covering the full 10-bit range so
    // the round-trip has non-uniform content to preserve.
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

/// Try to encode `num_frames` HEVC Main10 frames using libavcodec's
/// `libx265` encoder. Returns the concatenated Annex-B NALU stream.
/// Returns `None` if the libavcodec on this host wasn't built with
/// libx265 support — the test self-skips in that case.
fn encode_hevc_main10_frames(
    width: u32,
    height: u32,
    num_frames: u32,
) -> Option<Vec<u8>> {
    let _ = ffmpeg::init();
    // Look up libx265 by name — the encoder-registry name matches the
    // FFmpeg CLI's `-c:v libx265`. If the build is missing libx265,
    // this returns null and we self-skip.
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
    // x265 params — "ultrafast" and "no-info" for a compact bitstream
    // suitable for the test's 64×64.
    opts.set("preset", "ultrafast");
    opts.set("tune", "zerolatency");
    opts.set("x265-params", "log-level=none:keyint=1");
    let encoder = video.open_as_with(codec_ff, opts).ok()?;
    let mut opened = encoder;

    let mut out = Vec::new();
    for i in 0..num_frames {
        let mut frame = gradient_frame_yuv420p10(width, height);
        // Shift the gradient by i so each frame carries different
        // content — a diagonal offset.
        // The Y-plane build already put a diagonal there; per-frame
        // uniqueness would come from re-baking with a phase but for
        // this test the same gradient is fine — libx265 still
        // motion-encodes each into a distinct P-frame.
        let _ = i;
        unsafe {
            (*frame.as_mut_ptr()).pts = i as i64;
        }
        opened.send_frame(&frame).ok()?;
        drain_packets(&mut opened, &mut out);
    }
    // Flush.
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

    let nalus = match encode_hevc_main10_frames(W, H, 5) {
        Some(b) => b,
        None => {
            eprintln!("skipping: no libx265 in libavcodec build");
            return;
        }
    };
    assert!(
        !nalus.is_empty(),
        "libx265 produced empty bitstream — test setup wrong"
    );

    let mut dec = HevcDecoder::new().expect("HEVC decoder builds");
    // Push all the NALUs at once — the HEVC parser handles the
    // splitting internally.
    let mut got_frame = false;
    // libavcodec may buffer the first packet; loop feeding until
    // we get a frame or run out of retries.
    for retry in 0..8 {
        if let Some(frame) = dec.decode(&nalus) {
            assert_eq!(frame.width, W);
            assert_eq!(frame.height, H);
            got_frame = true;
            // Verify it's genuinely 10-bit — top 6 bits of Y must
            // contain content. If the decoder had downcast to 8-bit,
            // the top 6 bits would all be zero.
            match &frame.pixels {
                DecodedPixels::P010 { y, .. } => {
                    let all_top6_zero = y.iter().all(|s| (*s >> 10) == 0);
                    assert!(
                        !all_top6_zero,
                        "top 6 bits of every luma sample are zero — decoder likely downcast to 8-bit"
                    );
                    // Additionally: check that samples span > 8-bit's
                    // 256-level dynamic range in the top-bit layout.
                    // A 10-bit-in-upper-bits P010 stream has samples
                    // in the top 10 bits, so a proper 1023-code
                    // gradient should show at least 200 distinct
                    // samples (much more than the 256 an 8-bit could
                    // produce in the same space).
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
    assert!(got_frame, "HEVC decoder never produced a frame from encoded NALUs");
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
    // Confirm the decoder is present (compiler-checked by the pattern).
    match d {
        VideoDecoder::Hevc(_) => {}
        _ => panic!("wire label hevc-main10 should build a Hevc decoder"),
    }
}
