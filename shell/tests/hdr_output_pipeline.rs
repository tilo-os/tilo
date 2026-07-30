//! Output colour-pipeline integration: SDR passthrough vs HDR encode.
//!
//! Where `hdr_output_encode_shader.rs` proves the encode *maths* (CPU
//! reference + WGSL kernel validity), this exercises the compositor's
//! *decision* path end to end: probe → resolved output colour state →
//! per-surface composition op → the actual encode, plus the real
//! receive-side call site in [`PeerView::new_for_stream`].
//!
//! The chain under test is exactly the one the compositor runs:
//!
//! 1. [`OutputColorState::from_display`] resolves a probed
//!    [`DisplayCapabilities`] into the colour space an output is scanned
//!    out in (and the swapchain format that implies).
//! 2. [`plan_surface_op`] picks passthrough / tone-map / SDR→HDR encode
//!    / transcode for a surface against that output.
//! 3. For the SDR→HDR quadrant, [`map_sdr_pixel_to_pq_bt2020`] — the CPU
//!    mirror of `hdr_output_encode.wgsl` — performs the encode.
//!
//! No GPU or HDR panel is required: every step here is the headless,
//! deterministic maths. The last mile (a DRM connector in an HDR
//! signalling mode scanning out `Rgba16Float`) is the documented
//! hardware-only gap.

use shell::diagnostics::PeerSurfaceMetrics;
use shell::shell::color_pipeline::{
    OutputColorSpace, OutputColorState, SurfaceColorOp, map_sdr_pixel_to_pq_bt2020, plan_surface_op,
    pq_eotf, SDR_REFERENCE_WHITE_NITS,
};
use shell::shell::hdr_display::DisplayCapabilities;
use shell::shell::peer_view::PeerView;

use shell::collab::colorimetry::Colorimetry;

/// The production probe→state resolution: an SDR panel is driven SDR
/// (no HDR swapchain); an HDR10 panel is driven PQ with an HDR-carrying
/// swapchain format.
#[test]
fn probe_resolves_output_colour_state() {
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

/// SDR content on an SDR output is uploaded untouched — the passthrough
/// fast path, no encode. This is the branch that must *never* run the
/// SDR→HDR encode.
#[test]
fn sdr_surface_on_sdr_output_is_passthrough_no_encode() {
    let out = OutputColorState::from_display(&DisplayCapabilities::sdr_default());
    assert_eq!(
        plan_surface_op(Colorimetry::sdr_bt709(), &out, None),
        SurfaceColorOp::Passthrough,
        "SDR-on-SDR must be a straight passthrough"
    );
}

/// The heart of the output pipeline: SDR content on an HDR output is
/// *encoded* (`MapSdrToHdr`), and the encode is real — sRGB white lands
/// at the BT.2408 reference-white level (~203 cd/m²), black stays at PQ
/// zero, and mid-gray sits well below reference white (never clipped up
/// to panel peak).
#[test]
fn sdr_surface_on_hdr_output_selects_and_performs_encode() {
    let out = OutputColorState::from_display(&DisplayCapabilities::hdr10_baseline());
    assert_eq!(
        plan_surface_op(Colorimetry::sdr_bt709(), &out, None),
        SurfaceColorOp::MapSdrToHdr,
        "SDR-on-HDR must select the SDR→HDR encode op"
    );

    // Run the encode the op selected (the CPU mirror of the WGSL kernel).
    let white = map_sdr_pixel_to_pq_bt2020([255, 255, 255]);
    for (i, c) in white.iter().enumerate() {
        let nits = pq_eotf(*c as f32 / 1023.0);
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
        let nits = pq_eotf(c as f32 / 1023.0);
        assert!(
            nits > 5.0 && nits < SDR_REFERENCE_WHITE_NITS,
            "encoded mid-gray = {nits} nits should sit below reference white, never at peak"
        );
    }

    // The encode is not a no-op: white's PQ code must differ from a
    // naive 8-bit→10-bit rescale of 255 (which would put white at panel
    // peak, i.e. the clipping bug this pipeline exists to avoid).
    assert!(
        white.iter().all(|&c| c < 1000),
        "SDR white must NOT be pushed to PQ peak: {white:?}"
    );
}

/// HDR content on an SDR output tone-maps down (the receive/share path's
/// decode direction); HDR content on a matching PQ output passes through.
#[test]
fn hdr_surface_quadrants() {
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

/// The real receive-side call site: a plain SDR (H.264) stream is never
/// run through the CPU HDR→SDR tone-mapper, whether the output is SDR or
/// HDR — an SDR source routes to passthrough (SDR output) or the SDR→HDR
/// encode (HDR output), both of which leave `hdr_tonemap` disabled. This
/// exercises `PeerView::new_for_stream`'s production decision without the
/// `hdr-decode` feature (an H.264 decoder is always available).
#[test]
fn peer_view_sdr_stream_never_cpu_tonemaps() {
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
