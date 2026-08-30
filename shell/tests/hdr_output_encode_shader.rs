//! Output-encode (SDR → HDR PQ) shader spot-check.
//!
//! The compositor's HDR output path renders SDR surfaces into an sRGB
//! intermediate, then lifts them into the output's PQ BT.2020 signal so
//! SDR UI composits correctly alongside HDR video. Two tracks: the CPU
//! reference (`map_sdr_pixel_to_pq_bt2020` places sRGB white at the
//! BT.2408 reference-white level, black at PQ zero) and the GPU shader
//! (the WGSL kernel parses + validates through naga so an edit can't
//! silently break the production encode path) — both use the same maths
//! (203-nit white, BT.709->2020, PQ inverse EOTF).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use shell::shell::color_pipeline::{
    OUTPUT_ENCODE_SHADER_SOURCE, SDR_REFERENCE_WHITE_NITS, map_sdr_pixel_to_pq_bt2020, pq_eotf,
};

#[test]
fn cpu_encode_places_sdr_white_at_reference_white() {
    let code = map_sdr_pixel_to_pq_bt2020([255, 255, 255]);
    // Decode the PQ code back to nits: every channel ≈ 203 cd/m².
    for (i, c) in code.iter().enumerate() {
        let nits = pq_eotf(f32::from(*c) / 1023.0);
        assert!(
            (nits - SDR_REFERENCE_WHITE_NITS).abs() < 6.0,
            "white channel {i} decoded to {nits} nits, expected ~{SDR_REFERENCE_WHITE_NITS}",
        );
    }
}

#[test]
fn cpu_encode_keeps_black_at_pq_zero() {
    assert_eq!(map_sdr_pixel_to_pq_bt2020([0, 0, 0]), [0, 0, 0]);
}

#[test]
fn cpu_encode_mid_gray_is_below_reference_white() {
    // sRGB mid-gray (128) is ~21.6% linear, so it lands well under the
    // 203-nit diffuse-white ceiling — never clipped up to peak.
    let code = map_sdr_pixel_to_pq_bt2020([128, 128, 128]);
    for c in code {
        let nits = pq_eotf(f32::from(c) / 1023.0);
        assert!(
            nits > 5.0 && nits < SDR_REFERENCE_WHITE_NITS,
            "mid-gray = {nits} nits"
        );
    }
}

#[test]
fn wgsl_output_encode_parses_and_validates() {
    let module = naga::front::wgsl::parse_str(OUTPUT_ENCODE_SHADER_SOURCE)
        .expect("output-encode WGSL parses");
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::default(),
    );
    validator
        .validate(&module)
        .expect("output-encode WGSL validates");
}

#[test]
fn wgsl_output_encode_has_compute_entry_point() {
    let module = naga::front::wgsl::parse_str(OUTPUT_ENCODE_SHADER_SOURCE)
        .expect("output-encode WGSL parses");
    let has_entry = module
        .entry_points
        .iter()
        .any(|ep| ep.name == "encode_main" && matches!(ep.stage, naga::ShaderStage::Compute));
    assert!(has_entry, "WGSL missing compute entry point `encode_main`");
}
