//! Baseline image diffing for `shots check`.
//!
//! The goal is a *simple* per-pixel comparison that fails on regressions
//! but tolerates the trivial GPU noise that a re-render can produce.
//! No fuzzy matching, no perceptual heuristics — if a channel changes
//! by more than `threshold` units it counts as a diff.
//!
//! On every mismatched PNG we also write a `<name>.diff.png` next to
//! the current capture: the same image, but every changed pixel painted
//! solid red so the maintainer can eyeball where the regression lives.

use std::path::{Path, PathBuf};

/// One entry in a [`DiffReport`]. The path is relative to the baseline
/// root so the printer can emit short, copy-pasteable names.
#[derive(Debug)]
pub struct DiffEntry {
    pub path: PathBuf,
    pub kind: DiffKind,
}

#[derive(Debug)]
pub enum DiffKind {
    /// All pixels matched (within the configured threshold).
    Ok,
    /// `n` pixels exceeded the per-channel threshold.
    Diff(u64),
    /// File exists in baseline but not in current capture.
    Missing,
    /// File exists in both, but dimensions differ.
    SizeMismatch {
        baseline: (u32, u32),
        current: (u32, u32),
    },
    /// Decode error (truncated PNG, missing file mid-walk, etc.).
    ReadError(String),
}

#[derive(Debug, Default)]
pub struct DiffReport {
    pub entries: Vec<DiffEntry>,
}

/// Walk `baseline` recursively, looking for `.png` files, and diff each
/// against the same-named file under `current`. Returns a [`DiffReport`]
/// listing every comparison; the caller is responsible for converting
/// it to an exit code / human-readable summary.
///
/// `threshold` is the per-channel (R/G/B) delta tolerated before a pixel
/// counts as different. Alpha is intentionally ignored — wgpu's
/// premultiplied output can wobble in the bottom bits without changing
/// what the user sees.
///
/// # Errors
///
/// Returns an error if the baseline directory does not exist or if any filesystem operations fail.
pub fn compare_tree(baseline: &Path, current: &Path, threshold: u8) -> Result<DiffReport, String> {
    if !baseline.is_dir() {
        return Err(format!(
            "baseline directory '{}' not found",
            baseline.display()
        ));
    }
    let mut report = DiffReport::default();
    walk(baseline, baseline, current, threshold, &mut report)?;
    Ok(report)
}

fn walk(
    root: &Path,
    base_dir: &Path,
    current_root: &Path,
    threshold: u8,
    report: &mut DiffReport,
) -> Result<(), String> {
    let entries = std::fs::read_dir(base_dir)
        .map_err(|e| format!("read_dir '{}': {e}", base_dir.display()))?;
    let mut entries: Vec<_> = entries
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .collect();
    // Stable order so test output is deterministic across runs.
    entries.sort();
    for entry in entries {
        if entry.is_dir() {
            walk(root, &entry, current_root, threshold, report)?;
            continue;
        }
        if entry.extension().and_then(|s| s.to_str()) != Some("png") {
            continue;
        }
        // Skip files we generated on a previous check run (`*.diff.png`)
        // so a re-check doesn't compare diffs-of-diffs.
        if entry
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.ends_with(".diff.png"))
        {
            continue;
        }
        let rel = entry
            .strip_prefix(root)
            .map_err(|e| format!("strip_prefix: {e}"))?
            .to_path_buf();
        let cur = current_root.join(&rel);
        let kind = if cur.exists() {
            match compare_png(&entry, &cur, threshold) {
                Ok(0) => DiffKind::Ok,
                Ok(n) => {
                    // Write the visual diff next to the current capture so
                    // the maintainer can `xdg-open` it directly.
                    if let Err(e) = write_diff_overlay(&entry, &cur, threshold) {
                        eprintln!(
                            "shots check: diff overlay for {} failed: {e}",
                            cur.display()
                        );
                    }
                    DiffKind::Diff(n)
                }
                Err(DiffError::SizeMismatch { baseline, current }) => {
                    DiffKind::SizeMismatch { baseline, current }
                }
                Err(DiffError::Io(msg)) => DiffKind::ReadError(msg),
            }
        } else {
            DiffKind::Missing
        };
        report.entries.push(DiffEntry { path: rel, kind });
    }
    Ok(())
}

#[derive(Debug)]
enum DiffError {
    SizeMismatch {
        baseline: (u32, u32),
        current: (u32, u32),
    },
    Io(String),
}

/// Decode both PNGs and count pixels where any R/G/B channel exceeds
/// `threshold`. Alpha is ignored — see [`compare_tree`] for the
/// rationale.
fn compare_png(a: &Path, b: &Path, threshold: u8) -> Result<u64, DiffError> {
    let (a_pixels, a_dims) = load_rgba(a).map_err(DiffError::Io)?;
    let (b_pixels, b_dims) = load_rgba(b).map_err(DiffError::Io)?;
    if a_dims != b_dims {
        return Err(DiffError::SizeMismatch {
            baseline: a_dims,
            current: b_dims,
        });
    }
    Ok(count_diff_pixels(&a_pixels, &b_pixels, threshold))
}

/// Pure pixel-diff counter — lifted out of `compare_png` so unit tests
/// can drive it without round-tripping through the PNG codec.
pub fn count_diff_pixels(a: &[u8], b: &[u8], threshold: u8) -> u64 {
    assert_eq!(a.len(), b.len(), "buffers must be the same length");
    let mut n: u64 = 0;
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        let &[ra, ga, ba, _] = pa else {
            continue; // chunks_exact(4) guarantees len == 4
        };
        let &[rb, gb, bb, _] = pb else {
            continue; // chunks_exact(4) guarantees len == 4
        };
        if ra.abs_diff(rb) > threshold || ga.abs_diff(gb) > threshold || ba.abs_diff(bb) > threshold
        {
            // A diff count is bounded by the pixel count of a real image; saturating
            // is defensive rather than load-bearing here.
            n = n.saturating_add(1);
        }
    }
    n
}

/// Write `<current>.diff.png` — a copy of the current capture with every
/// pixel that exceeds `threshold` repainted solid red. Helps the
/// maintainer eyeball where a regression lives without diffing the
/// images by hand in GIMP.
fn write_diff_overlay(baseline: &Path, current: &Path, threshold: u8) -> Result<(), String> {
    let (b_pix, dims) = load_rgba(baseline)?;
    let (mut c_pix, c_dims) = load_rgba(current)?;
    if dims != c_dims {
        return Err(format!("size mismatch: {dims:?} vs {c_dims:?}"));
    }
    for (a, b) in c_pix.chunks_exact_mut(4).zip(b_pix.chunks_exact(4)) {
        if let ([ra, ga, ba, aa], [rb, gb, bb, _ab]) = (a, b)
            && (ra.abs_diff(*rb) > threshold
                || ga.abs_diff(*gb) > threshold
                || ba.abs_diff(*bb) > threshold)
        {
            *ra = 255;
            *ga = 0;
            *ba = 0;
            *aa = 255;
        }
    }
    let out_path = current.with_extension("diff.png");
    save_rgba(&out_path, &c_pix, dims.0, dims.1)
}

/// # Errors
///
/// Returns an error if the file cannot be opened or decoded. `shots`'s
/// offscreen path always writes RGBA, but external baselines may use any
/// PNG colour type; all of them expand to RGBA8.
fn load_rgba(path: &Path) -> Result<(Vec<u8>, (u32, u32)), String> {
    let decoded =
        ui::png::read_rgba8(path).map_err(|e| format!("decode '{}': {e}", path.display()))?;
    Ok((decoded.rgba, (decoded.width, decoded.height)))
}

fn save_rgba(path: &Path, pixels: &[u8], width: u32, height: u32) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    ui::png::write_rgba8(path, width, height, pixels)
        .map_err(|e| format!("write '{}': {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_diff_counts_classifies_and_skips_overlays() {
        // Phase `count_diff_pixels_respects_threshold`.
        // Pin the threshold semantics: deltas at-or-below the threshold do
        // not count as a diff; above counts.
        {
            // Two 1×2 buffers, second pixel differs by 5 in the green channel.
            let a = [10, 20, 30, 255, 10, 20, 30, 255];
            let b = [10, 25, 30, 255, 10, 26, 30, 255];
            // threshold=5 → first pixel's |Δg|=5 is NOT > 5; second's =6 is.
            assert_eq!(count_diff_pixels(&a, &b, 5), 1);
            // threshold=4 → both pixels exceed.
            assert_eq!(count_diff_pixels(&a, &b, 4), 2);
            // threshold=255 → nothing exceeds.
            assert_eq!(count_diff_pixels(&a, &b, 255), 0);
        }
        // Phase `baseline_diff_reports_pixel_count`.
        // Verify the `count_diff_pixels` reports the exact number we wrote
        // into the second buffer, validating the per-pixel walk.
        {
            // 4×4 buffer, mutate 7 specific pixels by a +30 delta in R.
            let width = 4u32;
            let height = 4u32;
            let pixel_count = (width * height) as usize;
            let mut baseline = vec![0u8; pixel_count * 4];
            for i in 0..pixel_count {
                baseline[i * 4] = 50;
                baseline[i * 4 + 1] = 80;
                baseline[i * 4 + 2] = 120;
                baseline[i * 4 + 3] = 255;
            }
            let mut current = baseline.clone();
            for &idx in &[0usize, 3, 5, 6, 9, 12, 15] {
                current[idx * 4] = current[idx * 4].saturating_add(30);
            }
            assert_eq!(count_diff_pixels(&baseline, &current, 4), 7);
            // Identical buffers → zero diff.
            assert_eq!(count_diff_pixels(&baseline, &baseline, 4), 0);
        }
        // Phase `baseline_diff_tree_walker_classifies_each_file`.
        // End-to-end roundtrip: write two PNGs to a tempdir, run the diff
        // tree walker, and assert the report kinds line up.
        {
            let tmp = std::env::temp_dir().join(format!("shots-diff-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            let baseline = tmp.join("baseline");
            let current = tmp.join("current");
            std::fs::create_dir_all(&baseline).unwrap();
            std::fs::create_dir_all(&current).unwrap();

            // Match: identical 2x2 frames.
            let pixels = vec![
                10u8, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255,
            ];
            save_rgba(&baseline.join("ok.png"), &pixels, 2, 2).unwrap();
            save_rgba(&current.join("ok.png"), &pixels, 2, 2).unwrap();

            // Diff: same dims, different pixels.
            let mut altered = pixels.clone();
            altered[0] = altered[0].saturating_add(50);
            altered[4] = altered[4].saturating_add(50);
            save_rgba(&baseline.join("diff.png"), &pixels, 2, 2).unwrap();
            save_rgba(&current.join("diff.png"), &altered, 2, 2).unwrap();

            // Missing: baseline exists, current does not.
            save_rgba(&baseline.join("missing.png"), &pixels, 2, 2).unwrap();

            // SizeMismatch: same name, different dimensions.
            let small = vec![1u8; 4];
            save_rgba(&baseline.join("size.png"), &pixels, 2, 2).unwrap();
            save_rgba(&current.join("size.png"), &small, 1, 1).unwrap();

            let report = compare_tree(&baseline, &current, 4).unwrap();
            let by_name: std::collections::HashMap<_, _> = report
                .entries
                .iter()
                .map(|e| {
                    (
                        e.path.file_name().unwrap().to_str().unwrap().to_string(),
                        &e.kind,
                    )
                })
                .collect();
            assert!(matches!(by_name["ok.png"], DiffKind::Ok));
            match by_name["diff.png"] {
                DiffKind::Diff(n) => assert_eq!(*n, 2, "two pixels mutated"),
                other => panic!("expected Diff, got {other:?}"),
            }
            assert!(matches!(by_name["missing.png"], DiffKind::Missing));
            assert!(matches!(by_name["size.png"], DiffKind::SizeMismatch { .. }));

            // The walker should have written a diff overlay PNG for diff.png.
            let overlay = current.join("diff.diff.png");
            assert!(
                overlay.exists(),
                "diff overlay must be written to {}",
                overlay.display()
            );

            let _ = std::fs::remove_dir_all(&tmp);
        }
        // Phase `diff_overlay_files_are_skipped`.
        // `*.diff.png` artefacts from a previous check must not be
        // re-processed on the next run (they're diff overlays of a
        // previous diff, not real baselines).
        {
            let tmp =
                std::env::temp_dir().join(format!("shots-diff-skip-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            let baseline = tmp.join("baseline");
            let current = tmp.join("current");
            std::fs::create_dir_all(&baseline).unwrap();
            std::fs::create_dir_all(&current).unwrap();
            let pixels = vec![0u8, 0, 0, 255, 0, 0, 0, 255];
            save_rgba(&baseline.join("a.png"), &pixels, 2, 1).unwrap();
            save_rgba(&baseline.join("a.diff.png"), &pixels, 2, 1).unwrap();
            save_rgba(&current.join("a.png"), &pixels, 2, 1).unwrap();
            let report = compare_tree(&baseline, &current, 4).unwrap();
            let names: Vec<_> = report
                .entries
                .iter()
                .map(|e| e.path.file_name().unwrap().to_str().unwrap().to_string())
                .collect();
            assert_eq!(names, vec!["a.png".to_string()]);
            let _ = std::fs::remove_dir_all(&tmp);
        }
    }
}
