//! Output-encode (SDR → HDR PQ) shader spot-check.
//!
//! The compositor's HDR output path renders SDR surfaces into an sRGB
//! intermediate, then lifts them into the output's PQ BT.2020 signal so
//! SDR UI composits correctly alongside HDR video. Two tracks: the CPU
//! reference (`map_sdr_pixel_to_pq_bt2020` places sRGB white at the
//! BT.2408 reference-white level, black at PQ zero) and the GPU shader
//! (the WGSL render pass parses + validates through naga so an edit can't
//! silently break the production encode path) — both use the same maths
//! (203-nit white, BT.709->2020, PQ inverse EOTF).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::shell::color_pipeline::{
    OUTPUT_ENCODE_SHADER_SOURCE, SDR_REFERENCE_WHITE_NITS, map_sdr_pixel_to_pq_bt2020, pq_eotf,
};

#[test]
fn pq_output_encode_maps_black_gray_and_white_and_its_wgsl_validates() {
    // Phase `cpu_encode_places_sdr_white_at_reference_white`.
    {
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

    // Phase `cpu_encode_keeps_black_at_pq_zero`.
    {
        assert_eq!(map_sdr_pixel_to_pq_bt2020([0, 0, 0]), [0, 0, 0]);
    }

    // Phase `cpu_encode_mid_gray_is_below_reference_white`.
    {
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

    // Phase `wgsl_output_encode_parses_and_validates`.
    {
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

    // Phase `wgsl_output_encode_has_render_pass_entry_points`.
    {
        // A render pass, not compute: the scanout target is an imported
        // dma-buf with colour-attachment usage only.
        let module = naga::front::wgsl::parse_str(OUTPUT_ENCODE_SHADER_SOURCE)
            .expect("output-encode WGSL parses");
        let has = |name: &str, stage: naga::ShaderStage| {
            module
                .entry_points
                .iter()
                .any(|ep| ep.name == name && ep.stage == stage)
        };
        assert!(
            has("vs_main", naga::ShaderStage::Vertex),
            "WGSL missing `vs_main`"
        );
        assert!(
            has("fs_main", naga::ShaderStage::Fragment),
            "WGSL missing `fs_main`"
        );
    }
}
