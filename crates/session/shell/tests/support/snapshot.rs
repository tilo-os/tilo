//! Golden-image snapshot comparison for PNG renders: asserts a freshly
//! rendered PNG matches a repo-checked-in golden within a per-channel
//! absolute diff on R/G/B/A (`tolerance = 4` accepts `|r1-r2| <= 4`); any
//! pixel whose max channel delta exceeds it counts as a mismatch.
//!
//! # Behaviour matrix
//!
//! | Golden exists | `UPDATE_SNAPSHOTS=1` | Effect                                |
//! | ------------- | -------------------- | ------------------------------------- |
//! | yes           | unset                | Compare; panic on mismatch            |
//! | yes           | set                  | Overwrite golden with rendered        |
//! | no            | unset, local run     | Seed the golden, print a notice, pass |
//! | no            | unset, CI            | Fail: a checked-in golden is required |
//! | no            | set                  | Seed / refresh the golden             |
//!
//! Seeding remains frictionless on a developer machine. CI never creates an
//! unreviewed baseline: it asks for `UPDATE_SNAPSHOTS=1`, followed by a normal
//! run that compares the checked-in file.

#![allow(dead_code)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::fs;
use std::path::{Path, PathBuf};

/// `rendered` is produced by the test; `golden` is checked in under
/// `tests/snapshots/`.
pub struct SnapshotAssertion {
    pub rendered_png: PathBuf,
    pub golden_png: PathBuf,
    pub tolerance: u8,
}

impl SnapshotAssertion {
    pub fn new(rendered: impl Into<PathBuf>, golden: impl Into<PathBuf>) -> Self {
        Self {
            rendered_png: rendered.into(),
            golden_png: golden.into(),
            tolerance: 4,
        }
    }

    pub fn with_tolerance(mut self, t: u8) -> Self {
        self.tolerance = t;
        self
    }

    /// See the module doc for the full behaviour matrix.
    pub fn assert(self) {
        let update = std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|v| v == "1");
        let require_existing_golden = std::env::var_os("CI").is_some();
        self.assert_with_policy(update, require_existing_golden);
    }

    /// [`Self::assert`] with the refresh decision handed in rather than read
    /// from the environment — this module's own tests exercise both sides of
    /// it, and the environment is process-global, so reading it here would
    /// race other tests over `UPDATE_SNAPSHOTS`.
    pub fn assert_with(self, update: bool) {
        self.assert_with_policy(update, false);
    }

    /// Variant used by tests and CI policy checks. `require_existing_golden`
    /// is deliberately explicit so module tests do not mutate process-global
    /// environment variables while running in parallel.
    pub fn assert_with_policy(self, update: bool, require_existing_golden: bool) {
        assert!(
            self.rendered_png.exists(),
            "snapshot: rendered PNG missing at {}",
            self.rendered_png.display()
        );

        if update {
            copy_over(&self.rendered_png, &self.golden_png);
            eprintln!("refreshed golden: {}", self.golden_png.display());
            return;
        }

        if !self.golden_png.exists() {
            assert!(
                !require_existing_golden,
                "snapshot golden missing at {}\n  run locally with UPDATE_SNAPSHOTS=1, review the image, and commit it",
                self.golden_png.display(),
            );
            copy_over(&self.rendered_png, &self.golden_png);
            eprintln!("wrote new golden: {}", self.golden_png.display());
            return;
        }

        let rendered = decode_png(&self.rendered_png);
        let golden = decode_png(&self.golden_png);

        assert!(
            !(rendered.width != golden.width || rendered.height != golden.height),
            "snapshot dimension mismatch: rendered={} ({}x{}) golden={} ({}x{})",
            self.rendered_png.display(),
            rendered.width,
            rendered.height,
            self.golden_png.display(),
            golden.width,
            golden.height,
        );

        let diff = diff_rgba(&rendered.rgba, &golden.rgba, self.tolerance);
        assert!(
            diff.mismatching_pixels == 0,
            "snapshot diff: rendered={} golden={}\n  \
             {} pixels differ (max delta {} per channel; tolerance {})\n  \
             rerun with UPDATE_SNAPSHOTS=1 to refresh the golden",
            self.rendered_png.display(),
            self.golden_png.display(),
            diff.mismatching_pixels,
            diff.max_delta,
            self.tolerance,
        );
    }
}

fn copy_over(src: &Path, dst: &Path) {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|e| panic!("mkdir {}: {e}", parent.display()));
    }
    fs::copy(src, dst)
        .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", src.display(), dst.display()));
}

struct DecodedPng {
    width: u32,
    height: u32,
    /// Always RGBA8, tight-packed at `4 * width` bytes per row.
    rgba: Vec<u8>,
}

fn decode_png(path: &Path) -> DecodedPng {
    let decoded = ui::png::read_rgba8(path)
        .unwrap_or_else(|e| panic!("snapshot: png decode {}: {e}", path.display()));
    DecodedPng {
        width: decoded.width,
        height: decoded.height,
        rgba: decoded.rgba,
    }
}

struct DiffResult {
    mismatching_pixels: usize,
    max_delta: u8,
}

fn diff_rgba(a: &[u8], b: &[u8], tolerance: u8) -> DiffResult {
    debug_assert_eq!(a.len(), b.len());
    let mut mismatching = 0usize;
    let mut max_delta = 0u8;
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        let dr = pa[0].abs_diff(pb[0]);
        let dg = pa[1].abs_diff(pb[1]);
        let db = pa[2].abs_diff(pb[2]);
        let da = pa[3].abs_diff(pb[3]);
        let d = dr.max(dg).max(db).max(da);
        if d > tolerance {
            mismatching = mismatching.saturating_add(1);
        }
        if d > max_delta {
            max_delta = d;
        }
    }
    DiffResult {
        mismatching_pixels: mismatching,
        max_delta,
    }
}

// Unit tests live here rather than a sibling `tests/` binary:
// `tests/support/` is included from each integration test crate as `mod
// support;`, so these `#[cfg(test)]` blocks only compile when a test
// binary with `mod support;` is being built.

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A tempdir-equivalent without adding `tempfile`. Each test gets its own
    /// subdirectory under `target/snapshot-tests-<n>/` so parallel
    /// cargo-test workers don't stomp.
    fn scratch_dir() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let base = std::env::var_os("CARGO_TARGET_DIR")
            .map_or_else(
                || {
                    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .ancestors()
                        .nth(3)
                        .unwrap_or_else(|| Path::new("."))
                        .join("target")
                },
                PathBuf::from,
            )
            .join(format!("snapshot-tests/{pid}-{n}"));
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn write_png(path: &Path, width: u32, height: u32, rgba_fill: [u8; 4]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut data = Vec::with_capacity(width.saturating_mul(height).saturating_mul(4) as usize);
        for _ in 0..(width.saturating_mul(height)) {
            data.extend_from_slice(&rgba_fill);
        }
        write_png_bytes(path, width, height, &data);
    }

    fn write_png_bytes(path: &Path, width: u32, height: u32, rgba: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        ui::png::write_rgba8(path, width, height, rgba).unwrap();
    }

    /// The panic message of `f`, which must panic; `#[should_panic(expected)]`
    /// matched this message by substring, and so do the callers here.
    fn panic_message(f: impl FnOnce()) -> String {
        let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
            .expect_err("the snapshot assertion must panic");
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
            .unwrap_or_default()
    }

    #[test]
    fn matching_or_updated_snapshots_pass_and_missing_goldens_are_seeded() {
        // Phase `identical_pngs_pass`.
        {
            let dir = scratch_dir();
            let rendered = dir.join("rendered.png");
            let golden = dir.join("golden.png");
            write_png(&rendered, 4, 4, [10, 20, 30, 255]);
            write_png(&golden, 4, 4, [10, 20, 30, 255]);

            SnapshotAssertion::new(rendered, golden).assert_with(false);
        }

        // Phase `diff_within_tolerance_passes`.
        {
            let dir = scratch_dir();
            let rendered = dir.join("rendered.png");
            let golden = dir.join("golden.png");
            // Delta = 3, below the default tolerance of 4.
            let mut rendered_px = vec![
                10, 20, 30, 255, 10, 20, 30, 255, 10, 20, 30, 255, 10, 20, 30, 255,
            ];
            let golden_px = rendered_px.clone();
            rendered_px[0] = 13;
            write_png_bytes(&rendered, 2, 2, &rendered_px);
            write_png_bytes(&golden, 2, 2, &golden_px);

            SnapshotAssertion::new(rendered, golden).assert_with(false);
        }

        // Phase `update_snapshots_overwrites_golden`.
        {
            let dir = scratch_dir();
            let rendered = dir.join("rendered.png");
            let golden = dir.join("golden.png");
            // Render is red, golden is blue — would fail without the flag.
            write_png(&rendered, 4, 4, [255, 0, 0, 255]);
            write_png(&golden, 4, 4, [0, 0, 255, 255]);

            SnapshotAssertion::new(&rendered, &golden).assert_with(true);

            let g = decode_png(&golden);
            assert_eq!(&g.rgba[..4], &[255, 0, 0, 255]);
        }

        // Phase `missing_golden_writes_it_and_passes`.
        {
            let dir = scratch_dir();
            let rendered = dir.join("rendered.png");
            // Deliberately NOT creating the golden ahead of time.
            let golden = dir.join("subdir/golden.png");
            write_png(&rendered, 4, 4, [100, 120, 140, 255]);

            SnapshotAssertion::new(&rendered, &golden).assert_with(false);

            assert!(
                golden.exists(),
                "golden should have been seeded on first run"
            );
        }
    }

    #[test]
    fn mismatched_or_missing_snapshots_fail_naming_the_reason() {
        // Case `dimension_mismatch_panics` (was `#[should_panic(expected = "dimension mismatch")]`).
        let message = panic_message(|| {
            let dir = scratch_dir();
            let rendered = dir.join("rendered.png");
            let golden = dir.join("golden.png");
            write_png(&rendered, 4, 4, [0, 0, 0, 255]);
            write_png(&golden, 8, 8, [0, 0, 0, 255]);

            SnapshotAssertion::new(rendered, golden).assert_with(false);
        });
        assert!(
            message.contains("dimension mismatch"),
            "case dimension_mismatch_panics: {message}"
        );

        // Case `diff_above_tolerance_panics` (was `#[should_panic(expected = "pixels differ")]`).
        let message = panic_message(|| {
            let dir = scratch_dir();
            let rendered = dir.join("rendered.png");
            let golden = dir.join("golden.png");
            // 2×2 image, one pixel differs by 20 > tolerance (4 default).
            let mut rendered_px = vec![
                10, 20, 30, 255, 10, 20, 30, 255, 10, 20, 30, 255, 10, 20, 30, 255,
            ];
            let mut golden_px = rendered_px.clone();
            rendered_px[0] = 30; // red channel of pixel 0: 30 vs 10 → delta 20
            let _ = &mut golden_px;
            write_png_bytes(&rendered, 2, 2, &rendered_px);
            write_png_bytes(&golden, 2, 2, &golden_px);

            SnapshotAssertion::new(rendered, golden).assert_with(false);
        });
        assert!(
            message.contains("pixels differ"),
            "case diff_above_tolerance_panics: {message}"
        );

        // Case `ci_policy_rejects_a_missing_golden` (was
        // `#[should_panic(expected = "snapshot golden missing")]`).
        let message = panic_message(|| {
            let dir = scratch_dir();
            let rendered = dir.join("rendered.png");
            let golden = dir.join("subdir/golden.png");
            write_png(&rendered, 4, 4, [100, 120, 140, 255]);

            SnapshotAssertion::new(&rendered, &golden).assert_with_policy(false, true);
        });
        assert!(
            message.contains("snapshot golden missing"),
            "case ci_policy_rejects_a_missing_golden: {message}"
        );
    }
}
