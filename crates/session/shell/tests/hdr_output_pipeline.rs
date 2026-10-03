//! Output colour-pipeline integration: SDR passthrough vs HDR encode.
//!
//! Where `hdr_output_encode_shader.rs` proves the encode *maths*, this
//! exercises the compositor's *decision* path end to end, exactly as it
//! runs it:
//! 1. [`OutputColorState::from_display`] resolves a probed
//!    [`DisplayCapabilities`] into the output's scanout colour space (and
//!    implied swapchain format).
//! 2. [`plan_surface_op`] picks passthrough / tone-map / SDR->HDR encode /
//!    transcode for a surface against that output.
//! 3. For SDR->HDR, [`map_sdr_pixel_to_pq_bt2020`] (the CPU mirror of
//!    `hdr_output_encode.wgsl`) performs the encode.
//!
//! It also covers the receive-side call site in [`PeerView::new_for_stream`].
//! Headless, deterministic maths — no GPU or HDR panel. A DRM
//! connector scanning out `Rgba16Float` is the documented hardware-only gap.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::diagnostics::PeerSurfaceMetrics;
use shell::shell::color_pipeline::{
    OutputColorSpace, OutputColorState, SDR_REFERENCE_WHITE_NITS, SurfaceColorOp,
    map_sdr_pixel_to_pq_bt2020, plan_surface_op, pq_eotf,
};
use shell::shell::hdr_display::DisplayCapabilities;
use shell::shell::peer_view::PeerView;

use shell::collab::colorimetry::Colorimetry;

#[test]
#[allow(
    clippy::similar_names,
    reason = "on_sdr/on_hdr name the two output-capability cases under test; renaming either \
away from the sdr/hdr pair would hurt readability, not help it"
)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn output_pipeline_encodes_only_where_the_output_and_surface_differ() {
    // Phase `probe_resolves_output_colour_state`.
    // An SDR panel is driven SDR (no HDR swapchain); an HDR10 panel is driven
    // PQ with an HDR-carrying swapchain format.
    {
        let sdr = OutputColorState::from_display(&DisplayCapabilities::sdr_default());
        assert_eq!(sdr.space, OutputColorSpace::SdrSrgb);
        assert!(
            sdr.preferred_swapchain_format().is_none(),
            "an SDR output must stay on the ordinary 8-bit sRGB swapchain"
        );

        let hdr = OutputColorState::from_display(&DisplayCapabilities::hdr10_baseline());
        assert_eq!(hdr.space, OutputColorSpace::HdrPq);
        assert!(
            hdr.preferred_swapchain_format().is_some(),
            "an HDR output must select an HDR-carrying swapchain format"
        );
    }

    // Phase `sdr_surface_on_sdr_output_is_passthrough_no_encode`.
    // SDR content on an SDR output is uploaded untouched — the passthrough
    // branch that must *never* run the SDR→HDR encode.
    {
        let out = OutputColorState::from_display(&DisplayCapabilities::sdr_default());
        assert_eq!(
            plan_surface_op(Colorimetry::sdr_bt709(), &out, None),
            SurfaceColorOp::Passthrough,
            "SDR-on-SDR must be a straight passthrough"
        );
    }

    // Phase `sdr_surface_on_hdr_output_selects_and_performs_encode`.
    // SDR content on an HDR output is *encoded* (`MapSdrToHdr`), and the
    // encode is real: sRGB white lands at the BT.2408 reference-white level
    // (~203 cd/m²), black stays PQ zero, mid-gray sits well below white.
    {
        let out = OutputColorState::from_display(&DisplayCapabilities::hdr10_baseline());
        assert_eq!(
            plan_surface_op(Colorimetry::sdr_bt709(), &out, None),
            SurfaceColorOp::MapSdrToHdr,
            "SDR-on-HDR must select the SDR→HDR encode op"
        );

        // Run the encode the op selected (the CPU mirror of the WGSL kernel).
        let white = map_sdr_pixel_to_pq_bt2020([255, 255, 255]);
        for (i, c) in white.iter().enumerate() {
            let nits = pq_eotf(f32::from(*c) / 1023.0);
            assert!(
                (nits - SDR_REFERENCE_WHITE_NITS).abs() < 6.0,
                "encoded SDR white channel {i} = {nits} nits, expected ~{SDR_REFERENCE_WHITE_NITS}"
            );
        }
        assert_eq!(
            map_sdr_pixel_to_pq_bt2020([0, 0, 0]),
            [0, 0, 0],
            "encoded SDR black must be PQ zero"
        );
        let gray = map_sdr_pixel_to_pq_bt2020([128, 128, 128]);
        for c in gray {
            let nits = pq_eotf(f32::from(c) / 1023.0);
            assert!(
                nits > 5.0 && nits < SDR_REFERENCE_WHITE_NITS,
                "encoded mid-gray = {nits} nits should sit below reference white, never at peak"
            );
        }

        // Not a no-op: white's PQ code must differ from a naive 8->10-bit rescale
        // of 255, which would put white at panel peak (the clipping bug).
        assert!(
            white.iter().all(|&c| c < 1000),
            "SDR white must NOT be pushed to PQ peak: {white:?}"
        );
    }

    // Phase `hdr_surface_quadrants`.
    // HDR on an SDR output tone-maps down; HDR on a matching PQ output passes through.
    {
        let sdr_out = OutputColorState::from_display(&DisplayCapabilities::sdr_default());
        assert!(
            matches!(
                plan_surface_op(Colorimetry::hdr10(), &sdr_out, Some(4000.0)),
                SurfaceColorOp::TonemapHdrToSdr { .. }
            ),
            "HDR-on-SDR must tone-map down"
        );

        let pq_out = OutputColorState::from_display(&DisplayCapabilities::hdr10_baseline());
        assert_eq!(
            plan_surface_op(Colorimetry::hdr10(), &pq_out, None),
            SurfaceColorOp::Passthrough,
            "PQ-on-PQ must pass through"
        );
        assert_eq!(
            plan_surface_op(Colorimetry::hdr_hlg(), &pq_out, None),
            SurfaceColorOp::TranscodeHdrTransfer,
            "HLG-on-PQ must transcode transfers, not tone-map"
        );
    }

    // Phase `peer_view_sdr_stream_never_cpu_tonemaps`.
    // The receive-side call site: a plain SDR (H.264) stream never runs
    // through the CPU HDR->SDR tone-mapper on either output (both leave
    // `hdr_tonemap` disabled).
    {
        let sdr_display = DisplayCapabilities::sdr_default();
        let hdr_display = DisplayCapabilities::hdr10_baseline();

        let on_sdr = PeerView::new_for_stream(
            1,
            7,
            "h264",
            PeerSurfaceMetrics::default(),
            &sdr_display,
            None,
        );
        assert!(
            !on_sdr.is_hdr(),
            "an SDR stream on an SDR output must not engage the HDR tone-map path"
        );

        let on_hdr = PeerView::new_for_stream(
            2,
            7,
            "h264",
            PeerSurfaceMetrics::default(),
            &hdr_display,
            None,
        );
        assert!(
            !on_hdr.is_hdr(),
            "an SDR stream on an HDR output routes to the SDR→HDR encode, \
             not the CPU HDR→SDR tone-mapper"
        );
    }
}
