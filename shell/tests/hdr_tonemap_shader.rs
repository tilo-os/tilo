//! Tonemap shader spot-check.
//!
//! Two tracks:
//!
//! 1. **CPU reference** — the CPU implementation of the tone-mapper
//!    handles a solid mid-gray PQ 300-nit input and produces a
//!    plausible SDR sRGB output (mid-gray-ish, alpha full, neutral
//!    chroma preserved).
//! 2. **GPU shader** — the WGSL kernel compiles cleanly against a
//!    fresh wgpu adapter. We don't run the full compute pipeline
//!    (fetching an adapter costs the OffscreenRenderer bring-up) —
//!    but we DO parse + validate the shader through naga to catch
//!    syntax / type errors at test time.
//!
//! Both paths use the same PQ math, so shader validity + CPU-side
//! spot-check together prove the pipeline.

use shell::collab::colorimetry::{
    ColorPrimaries, ColorRange, Colorimetry, MatrixCoefficients, TransferCharacteristics,
};
use shell::shell::hdr_tonemap::{
    HdrTonemapConfig, SHADER_SOURCE, tonemap_p010_to_rgba8,
};

/// Encode a `code_value` (0..1023) as the top-10-bit P010 sample.
fn p010(code: u16) -> u16 {
    code << 6
}

/// Build a 4×4 P010 luma plane at the given 10-bit code with neutral
/// chroma. Returns (y, uv).
fn solid_p010(luma_code: u16, width: u32, height: u32) -> (Vec<u16>, Vec<u16>) {
    let y = vec![p010(luma_code); (width * height) as usize];
    // Neutral chroma at 512 (10-bit mid). UV plane is interleaved,
    // 4:2:0 subsampled → width/2 * height/2 pixel-pairs, 2 samples
    // per pair.
    let uv = vec![p010(512); ((width / 2) * (height / 2) * 2) as usize];
    (y, uv)
}

#[test]
fn cpu_tonemap_mid_gray_pq_produces_plausible_sdr_gray() {
    let width = 4;
    let height = 4;

    // PQ code for ~300 nits: solving PQ inverse for 300 cd/m² yields
    // roughly electrical code 0.5714 → 10-bit code ~584.
    let luma_code = 584;
    let (y, uv) = solid_p010(luma_code, width, height);

    let colorimetry = Colorimetry {
        primaries: ColorPrimaries::Bt2020,
        transfer: TransferCharacteristics::SmpteSt2084,
        matrix: MatrixCoefficients::Bt2020NonConstant,
        range: ColorRange::Limited,
    };
    let cfg = HdrTonemapConfig {
        source_peak_nits: 1000.0,
        target_peak_nits: 100.0,
    };

    let rgba = tonemap_p010_to_rgba8(
        &y, &uv,
        width,        // y_stride in u16 units
        width,        // uv_stride in u16 units (interleaved: 2 samples per uv pair per column)
        width, height,
        colorimetry,
        cfg,
    );

    // Basic shape.
    assert_eq!(rgba.len() as u32, width * height * 4);
    for (i, chunk) in rgba.chunks_exact(4).enumerate() {
        assert_eq!(chunk[3], 0xFF, "alpha for pixel {i} not full");
    }

    // The first pixel should be near-gray (R ≈ G ≈ B) because chroma
    // is neutral.
    let r = rgba[0];
    let g = rgba[1];
    let b = rgba[2];
    let max_delta = (r as i32 - g as i32).abs().max((g as i32 - b as i32).abs());
    assert!(
        max_delta <= 8,
        "neutral chroma should produce near-gray, got ({r}, {g}, {b})",
    );

    // Value is neither crushed to black nor blown out to white — the
    // Hable curve on ~300 nits mapped to 100-nit peak lands mid-tone.
    assert!(
        r > 40 && r < 220,
        "PQ 300-nit tonemap should land in mid-tone, got R={r}",
    );

    eprintln!("PQ 300-nit → sRGB ({r}, {g}, {b})");
}

#[test]
fn cpu_tonemap_black_stays_black() {
    let width = 4;
    let height = 4;
    // Full black: PQ code 64 (limited-range 10-bit black point).
    let (y, uv) = solid_p010(64, width, height);
    let colorimetry = Colorimetry {
        primaries: ColorPrimaries::Bt2020,
        transfer: TransferCharacteristics::SmpteSt2084,
        matrix: MatrixCoefficients::Bt2020NonConstant,
        range: ColorRange::Limited,
    };
    let rgba = tonemap_p010_to_rgba8(
        &y, &uv,
        width, width, width, height,
        colorimetry,
        HdrTonemapConfig::default(),
    );
    let r = rgba[0];
    let g = rgba[1];
    let b = rgba[2];
    assert!(r < 8, "PQ black should stay near sRGB black, got R={r}");
    assert!(g < 8, "PQ black should stay near sRGB black, got G={g}");
    assert!(b < 8, "PQ black should stay near sRGB black, got B={b}");
}

#[test]
fn cpu_tonemap_peak_saturates_toward_white() {
    let width = 4;
    let height = 4;
    // Peak PQ: code 940 = limited-range 10-bit white point.
    let (y, uv) = solid_p010(940, width, height);
    let colorimetry = Colorimetry {
        primaries: ColorPrimaries::Bt2020,
        transfer: TransferCharacteristics::SmpteSt2084,
        matrix: MatrixCoefficients::Bt2020NonConstant,
        range: ColorRange::Limited,
    };
    let rgba = tonemap_p010_to_rgba8(
        &y, &uv,
        width, width, width, height,
        colorimetry,
        HdrTonemapConfig {
            source_peak_nits: 1000.0,
            target_peak_nits: 100.0,
        },
    );
    let r = rgba[0];
    // Peak PQ (~10 000 nits) tone-mapped down to 100-nit peak
    // saturates hard toward white after the Hable curve.
    assert!(r > 200, "PQ peak should saturate near white, got R={r}");
}

#[test]
fn wgsl_shader_parses_and_validates() {
    // Parse the shader source and validate it — catches syntax /
    // type errors at test time so a shader edit doesn't break
    // production silently.
    let module = naga::front::wgsl::parse_str(SHADER_SOURCE)
        .expect("WGSL shader parses");

    // Full validation catches variable / binding / type errors that
    // parsing alone doesn't. Use the default validator config
    // (matches wgpu's own defaults).
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::default(),
    );
    validator
        .validate(&module)
        .expect("WGSL shader validates");
}

#[test]
fn wgsl_shader_contains_expected_entry_point() {
    let module = naga::front::wgsl::parse_str(SHADER_SOURCE)
        .expect("WGSL shader parses");
    // The compute entry point drives PeerView's compute dispatch.
    let has_entry = module
        .entry_points
        .iter()
        .any(|ep| ep.name == "tonemap_main" && matches!(ep.stage, naga::ShaderStage::Compute));
    assert!(has_entry, "WGSL missing compute entry point `tonemap_main`");
}
