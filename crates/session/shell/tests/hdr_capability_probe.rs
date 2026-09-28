//! HDR output, headless: what an output can show comes from the sink's EDID
//! and the compositor's signal, never from a presentation surface; HDR picks
//! the 10-bit scanout format before half float; and the readback path turns
//! a rendered scene into the `XBGR2101010` PQ words KMS scans out, falling
//! back to the SDR swizzle when no encoder is in play.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use settings_store::hardware::OutputTransform;
use shell::compositor::backend::drm::paint::{
    SubrectSource, VelloDrmBridge, encode_subrect_rgba_to_xbgr2101010_transformed,
    swizzle_subrect_rgba_to_xrgb8888_transformed,
};
use shell::compositor::backend::drm::scanout::ScanoutFormat;
use shell::shell::color_pipeline::{SdrToPqEncoder, map_sdr_pixel_to_pq_bt2020_at};
use shell::shell::hdr_display::DisplayCapabilities;
use shell::wayland::edid::parse_hdr_display_info;

/// A 256-byte EDID: BT.2020 chromaticities in the base block and a CTA-861
/// extension whose HDR static metadata block advertises SDR + PQ, a ~1000
/// cd/m² peak and a ~400 cd/m² frame average.
fn hdr_monitor_edid(pq: bool) -> Vec<u8> {
    let mut edid = vec![0_u8; 256];
    edid[0..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
    edid[18] = 1;
    edid[19] = 4;
    // Chromaticity high bytes (the 10-bit value over four): red (0.708, 0.292),
    // green (0.170, 0.797), blue (0.131, 0.046), white D65.
    edid[27] = 181;
    edid[28] = 74;
    edid[29] = 43;
    edid[30] = 204;
    edid[31] = 33;
    edid[32] = 11;
    edid[33] = 80;
    edid[34] = 84;
    edid[126] = 1;
    edid[128] = 0x02;
    edid[129] = 3;
    edid[130] = 11;
    // HDR static metadata data block: tag 7, length 6, extended tag 6.
    edid[132] = 0xE6;
    edid[133] = 0x06;
    edid[134] = if pq { 0b0000_0101 } else { 0b0000_0001 };
    edid[135] = 0x01;
    edid[136] = 138;
    edid[137] = 96;
    edid[138] = 18;
    edid
}

#[test]
fn capabilities_come_from_the_edid_and_the_active_signal() {
    let hdr = parse_hdr_display_info(&hdr_monitor_edid(true));
    // Phase `an_hdr_sink_not_yet_driven_in_hdr_is_sdr`.
    assert_eq!(
        DisplayCapabilities::from_edid(&hdr, false),
        DisplayCapabilities::sdr_default()
    );
    // Phase `the_driven_hdr_output_reports_the_edid_peak`.
    let caps = DisplayCapabilities::from_edid(&hdr, true);
    assert!(caps.supports_pq_output);
    assert!(caps.bt2020_gamut);
    assert!(
        (caps.max_luminance_nits - 1000.0).abs() < 30.0,
        "EDID peak, got {}",
        caps.max_luminance_nits
    );
    // Phase `an_sdr_sink_never_claims_pq`.
    let sdr = parse_hdr_display_info(&hdr_monitor_edid(false));
    assert_eq!(
        DisplayCapabilities::from_edid(&sdr, true),
        DisplayCapabilities::sdr_default()
    );
    // Phase `garbage_edid_is_sdr`.
    assert_eq!(
        DisplayCapabilities::from_edid(&parse_hdr_display_info(&[0xAB; 40]), true),
        DisplayCapabilities::sdr_default()
    );
}

#[test]
fn hdr_prefers_ten_bit_scanout_and_falls_back_to_half_float() {
    assert_eq!(
        ScanoutFormat::HDR_PREFERENCE,
        [ScanoutFormat::Xbgr2101010, ScanoutFormat::Abgr16161616f]
    );
    assert!(ScanoutFormat::HDR_PREFERENCE.iter().all(|f| f.is_hdr()));
    assert!(!ScanoutFormat::Xrgb8888.is_hdr());
    assert_eq!(
        ScanoutFormat::Xbgr2101010.bytes_per_pixel(),
        4,
        "no bigger than SDR"
    );
    assert_eq!(ScanoutFormat::Abgr16161616f.bytes_per_pixel(), 8);
    assert_eq!(
        ScanoutFormat::Xbgr2101010.fourcc(),
        smithay::reexports::drm::buffer::DrmFourcc::Xbgr2101010
    );
    assert_eq!(
        ScanoutFormat::Abgr16161616f.fourcc(),
        smithay::reexports::drm::buffer::DrmFourcc::Abgr16161616f
    );
}

#[test]
fn readback_encodes_a_rendered_scene_into_pq_words() {
    use vello::kurbo::{Affine, Rect};
    use vello::peniko::{Color, Fill};

    let (width, height) = (8_u32, 4_u32);
    let mut bridge = VelloDrmBridge::new(width, height).expect("a real wgpu adapter");
    let mut scene = vello::Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(255, 255, 255),
        None,
        &Rect::new(0.0, 0.0, 4.0, 4.0),
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(200, 30, 10),
        None,
        &Rect::new(4.0, 0.0, 8.0, 4.0),
    );
    let rgba = bridge.paint(&scene).expect("scene renders").to_vec();
    let source = SubrectSource {
        src_rgba: &rgba,
        src_w: width as usize,
        src_h: height as usize,
        src_x: 0,
        src_y: 0,
    };
    let pitch = width as usize * 4;
    let white = 203.0;
    let encoder = SdrToPqEncoder::new(white);

    // Phase `hdr_words_match_the_reference_maths`.
    let mut hdr = vec![0_u8; pitch * height as usize];
    encode_subrect_rgba_to_xbgr2101010_transformed(
        source,
        &mut hdr,
        pitch,
        (width as usize, height as usize),
        OutputTransform::Normal,
        &encoder,
        None,
    );
    let word = |buffer: &[u8], x: usize, y: usize| {
        let at = y * pitch + x * 4;
        u32::from_le_bytes([buffer[at], buffer[at + 1], buffer[at + 2], buffer[at + 3]])
    };
    for (x, y) in [(1, 1), (6, 2)] {
        let at = (y * width as usize + x) * 4;
        let pixel = [rgba[at], rgba[at + 1], rgba[at + 2]];
        let reference = map_sdr_pixel_to_pq_bt2020_at(pixel, white);
        let got = word(&hdr, x, y);
        let channels = [got & 0x3FF, (got >> 10) & 0x3FF, (got >> 20) & 0x3FF];
        for (g, r) in channels.iter().zip(reference) {
            assert!(
                (i64::from(*g) - i64::from(r)).abs() <= 1,
                "{pixel:?}: {g} vs {r}"
            );
        }
        assert_eq!(got >> 30, 0b11, "padding bits set");
    }
    // SDR white sits at the configured brightness, not at the panel peak.
    let white_code = word(&hdr, 1, 1) & 0x3FF;
    let nits = shell::shell::color_pipeline::pq_eotf(
        f32::from(u16::try_from(white_code).unwrap()) / 1023.0,
    );
    assert!((nits - white).abs() < 6.0, "white at {nits} cd/m²");

    // Phase `rotation_is_applied_to_the_hdr_encode_too`.
    let mut rotated = vec![0_u8; pitch * height as usize];
    encode_subrect_rgba_to_xbgr2101010_transformed(
        source,
        &mut rotated,
        pitch,
        (width as usize, height as usize),
        OutputTransform::Rotate180,
        &encoder,
        None,
    );
    assert_eq!(word(&rotated, 7, 3), word(&hdr, 0, 0));
    assert_eq!(word(&rotated, 0, 0), word(&hdr, 7, 3));

    // Phase `without_hdr_the_same_scene_takes_the_sdr_swizzle`.
    let mut sdr = vec![0_u8; pitch * height as usize];
    swizzle_subrect_rgba_to_xrgb8888_transformed(
        source,
        &mut sdr,
        pitch,
        width as usize,
        height as usize,
        OutputTransform::Normal,
    );
    let at = 6 * 4 + 2 * pitch;
    assert_eq!(
        [sdr[at + 2], sdr[at + 1], sdr[at]],
        [
            rgba[(2 * width as usize + 6) * 4],
            rgba[(2 * width as usize + 6) * 4 + 1],
            rgba[(2 * width as usize + 6) * 4 + 2]
        ]
    );
}
