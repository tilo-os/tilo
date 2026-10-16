//! The typography baselines' dimensions, checked from layout alone.
//!
//! Pixel comparison against the baselines needs llvmpipe and runs where that
//! is pinned. Layout needs neither a GPU nor a particular adapter, so this
//! check runs everywhere and catches a card that grew or shrank, which would
//! otherwise surface only as a dimension mismatch in the golden run.

use crate::typography_visual_regression::{CASES, baseline_path, frame_size};

/// Width and height from a PNG's IHDR chunk, which the format requires to
/// come first, right after the 8-byte signature.
fn png_dimensions(bytes: &[u8]) -> (u32, u32) {
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "PNG signature");
    assert_eq!(&bytes[12..16], b"IHDR", "IHDR is the first chunk");
    let be = |field: &[u8]| u32::from_be_bytes(field.try_into().unwrap());
    (be(&bytes[16..20]), be(&bytes[20..24]))
}

#[test]
fn every_case_lays_out_at_its_baseline_size() {
    tilo_ui::i18n::I18nManager::init("en-US");
    let mismatches: Vec<String> = CASES
        .iter()
        .filter_map(|&case| {
            let path = baseline_path(case);
            let bytes = std::fs::read(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            let baseline = png_dimensions(&bytes);
            let laid_out = frame_size(case);
            (laid_out != baseline).then(|| {
                format!(
                    "{}: lays out at {}x{}, baseline is {}x{}",
                    case.name, laid_out.0, laid_out.1, baseline.0, baseline.1
                )
            })
        })
        .collect();
    assert!(
        mismatches.is_empty(),
        "typography card layout no longer matches its baselines; regenerate them \
         as described in typography_visual_regression.rs:\n{}",
        mismatches.join("\n")
    );
}
