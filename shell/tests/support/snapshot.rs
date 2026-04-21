//! Golden-image snapshot comparison for PNG renders.
//!
//! Lets E2E tests assert that a freshly rendered PNG matches a
//! repo-checked-in golden image within a per-channel absolute diff
//! tolerance. Uses the `png` crate (already a dev-dep) to decode; no
//! new crates are pulled in.
//!
//! # Behaviour matrix
//!
//! | Golden exists | `UPDATE_SNAPSHOTS=1` | Effect                                |
//! | ------------- | -------------------- | ------------------------------------- |
//! | yes           | unset                | Compare; panic on mismatch            |
//! | yes           | set                  | Overwrite golden with rendered        |
//! | no            | unset                | **Update-on-first-run**: copy rendered
//! |               |                      | over → golden, print a notice, pass   |
//! | no            | set                  | Same as above                         |
//!
//! The "update-on-first-run" policy is deliberate — it means the first
//! pass in a new CI environment produces goldens the maintainer then
//! reviews and commits. Turning a missing golden into a hard failure
//! would block that initial seeding step.
//!
//! # Tolerance
//!
//! Per-channel absolute diff. `tolerance = 4` accepts `|r1-r2| <= 4`
//! etc. on R, G, B. Alpha is also checked with the same tolerance so
//! transparency regressions don't slip through. Any pixel whose max
//! channel delta exceeds the tolerance counts as a mismatch.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

/// Drives one golden-image assertion: `rendered` (produced by the
/// test) vs `golden` (checked in under `tests/snapshots/`).
pub struct SnapshotAssertion {
    pub rendered_png: PathBuf,
    pub golden_png: PathBuf,
    pub tolerance: u8,
}

impl SnapshotAssertion {
    /// `rendered` is the PNG the test just wrote (e.g. under
    /// `target/nirios-screenshots/...`). `golden` is the repo-checked-in
    /// expected image (e.g. `quick-shell/tests/snapshots/e2e/foo.png`).
    pub fn new(rendered: impl Into<PathBuf>, golden: impl Into<PathBuf>) -> Self {
        Self {
            rendered_png: rendered.into(),
            golden_png: golden.into(),
            tolerance: 4,
        }
    }

    /// Override the default `tolerance = 4`.
    pub fn with_tolerance(mut self, t: u8) -> Self {
        self.tolerance = t;
        self
    }

    /// Compare rendered vs golden. See the module doc for the full
    /// behaviour matrix.
    pub fn assert(self) {
        let update = std::env::var_os("UPDATE_SNAPSHOTS")
            .map(|v| v == "1")
            .unwrap_or(false);

        if !self.rendered_png.exists() {
            panic!(
                "snapshot: rendered PNG missing at {}",
                self.rendered_png.display()
            );
        }

        if update {
            copy_over(&self.rendered_png, &self.golden_png);
            eprintln!("refreshed golden: {}", self.golden_png.display());
            return;
        }

        if !self.golden_png.exists() {
            // Update-on-first-run.
            copy_over(&self.rendered_png, &self.golden_png);
            eprintln!("wrote new golden: {}", self.golden_png.display());
            return;
        }

        let rendered = decode_png(&self.rendered_png);
        let golden = decode_png(&self.golden_png);

        if rendered.width != golden.width || rendered.height != golden.height {
            panic!(
                "snapshot dimension mismatch: rendered={} ({}x{}) golden={} ({}x{})",
                self.rendered_png.display(),
                rendered.width,
                rendered.height,
                self.golden_png.display(),
                golden.width,
                golden.height,
            );
        }

        let diff = diff_rgba(&rendered.rgba, &golden.rgba, self.tolerance);
        if diff.mismatching_pixels > 0 {
            panic!(
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
}

/// Copy `src` over `dst`, creating parent directories as needed.
fn copy_over(src: &Path, dst: &Path) {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .unwrap_or_else(|e| panic!("mkdir {}: {e}", parent.display()));
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
    let file = fs::File::open(path)
        .unwrap_or_else(|e| panic!("snapshot: open {}: {e}", path.display()));
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder
        .read_info()
        .unwrap_or_else(|e| panic!("snapshot: png header {}: {e}", path.display()));
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .unwrap_or_else(|e| panic!("snapshot: png decode {}: {e}", path.display()));
    buf.truncate(info.buffer_size());

    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            // Expand RGB → RGBA so the diff routine below only has to
            // handle one layout. Opaque alpha.
            let mut out = Vec::with_capacity((info.width * info.height * 4) as usize);
            for px in buf.chunks_exact(3) {
                out.extend_from_slice(px);
                out.push(0xFF);
            }
            out
        }
        other => panic!(
            "snapshot: unsupported color type {:?} in {}",
            other,
            path.display()
        ),
    };

    DecodedPng {
        width: info.width,
        height: info.height,
        rgba,
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
            mismatching += 1;
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

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------
//
// Live alongside the helper rather than in a sibling `tests/` binary —
// `tests/support/` is included from each integration test crate as
// `mod support;`, so `#[cfg(test)]` blocks here only compile when a
// test binary that has `mod support;` is being built, which is exactly
// what we want.

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Pull in a tempdir-equivalent without adding `tempfile`. Each test
    /// gets its own subdirectory under `target/snapshot-tests-<n>/` so
    /// parallel cargo-test workers don't stomp.
    fn scratch_dir() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let base = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join("target")
            })
            .join(format!("snapshot-tests/{pid}-{n}"));
        fs::create_dir_all(&base).unwrap();
        base
    }

    /// Write a tiny solid-colour RGBA8 PNG at `path`.
    fn write_png(path: &Path, width: u32, height: u32, rgba_fill: [u8; 4]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            data.extend_from_slice(&rgba_fill);
        }
        write_png_bytes(path, width, height, &data);
    }

    fn write_png_bytes(path: &Path, width: u32, height: u32, rgba: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let file = fs::File::create(path).unwrap();
        let mut w = std::io::BufWriter::new(file);
        let mut encoder = png::Encoder::new(&mut w, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(rgba).unwrap();
    }

    /// `UPDATE_SNAPSHOTS` is process-global env state, so test cases
    /// that touch it must not run concurrently with ones that assume
    /// it's unset. Serialize via a mutex.
    fn update_snapshots_lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn identical_pngs_pass() {
        let _g = update_snapshots_lock();
        // SAFETY: `assert()` below may read UPDATE_SNAPSHOTS. Guarded
        // by the mutex above so no other test toggles it mid-read.
        unsafe {
            std::env::remove_var("UPDATE_SNAPSHOTS");
        }

        let dir = scratch_dir();
        let rendered = dir.join("rendered.png");
        let golden = dir.join("golden.png");
        write_png(&rendered, 4, 4, [10, 20, 30, 255]);
        write_png(&golden, 4, 4, [10, 20, 30, 255]);

        SnapshotAssertion::new(rendered, golden).assert();
    }

    #[test]
    #[should_panic(expected = "dimension mismatch")]
    fn dimension_mismatch_panics() {
        let _g = update_snapshots_lock();
        unsafe {
            std::env::remove_var("UPDATE_SNAPSHOTS");
        }

        let dir = scratch_dir();
        let rendered = dir.join("rendered.png");
        let golden = dir.join("golden.png");
        write_png(&rendered, 4, 4, [0, 0, 0, 255]);
        write_png(&golden, 8, 8, [0, 0, 0, 255]);

        SnapshotAssertion::new(rendered, golden).assert();
    }

    #[test]
    #[should_panic(expected = "pixels differ")]
    fn diff_above_tolerance_panics() {
        let _g = update_snapshots_lock();
        unsafe {
            std::env::remove_var("UPDATE_SNAPSHOTS");
        }

        let dir = scratch_dir();
        let rendered = dir.join("rendered.png");
        let golden = dir.join("golden.png");
        // 2×2 image, one pixel differs by 20 > tolerance (4 default).
        let mut rendered_px = vec![
            10, 20, 30, 255,
            10, 20, 30, 255,
            10, 20, 30, 255,
            10, 20, 30, 255,
        ];
        let mut golden_px = rendered_px.clone();
        rendered_px[0] = 30; // red channel of pixel 0: 30 vs 10 → delta 20
        let _ = &mut golden_px;
        write_png_bytes(&rendered, 2, 2, &rendered_px);
        write_png_bytes(&golden, 2, 2, &golden_px);

        SnapshotAssertion::new(rendered, golden).assert();
    }

    #[test]
    fn diff_within_tolerance_passes() {
        let _g = update_snapshots_lock();
        unsafe {
            std::env::remove_var("UPDATE_SNAPSHOTS");
        }

        let dir = scratch_dir();
        let rendered = dir.join("rendered.png");
        let golden = dir.join("golden.png");
        // Delta = 3, below the default tolerance of 4.
        let mut rendered_px = vec![
            10, 20, 30, 255,
            10, 20, 30, 255,
            10, 20, 30, 255,
            10, 20, 30, 255,
        ];
        let golden_px = rendered_px.clone();
        rendered_px[0] = 13;
        write_png_bytes(&rendered, 2, 2, &rendered_px);
        write_png_bytes(&golden, 2, 2, &golden_px);

        SnapshotAssertion::new(rendered, golden).assert();
    }

    #[test]
    fn update_snapshots_overwrites_golden() {
        let _g = update_snapshots_lock();

        let dir = scratch_dir();
        let rendered = dir.join("rendered.png");
        let golden = dir.join("golden.png");
        // Render is red, golden is blue — would fail without the flag.
        write_png(&rendered, 4, 4, [255, 0, 0, 255]);
        write_png(&golden, 4, 4, [0, 0, 255, 255]);

        unsafe {
            std::env::set_var("UPDATE_SNAPSHOTS", "1");
        }
        let result = std::panic::catch_unwind(|| {
            SnapshotAssertion::new(&rendered, &golden).assert();
        });
        unsafe {
            std::env::remove_var("UPDATE_SNAPSHOTS");
        }
        result.expect("UPDATE_SNAPSHOTS=1 should not panic");

        // Verify the golden now matches the rendered (both red).
        let g = decode_png(&golden);
        assert_eq!(&g.rgba[..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn missing_golden_writes_it_and_passes() {
        let _g = update_snapshots_lock();
        unsafe {
            std::env::remove_var("UPDATE_SNAPSHOTS");
        }

        let dir = scratch_dir();
        let rendered = dir.join("rendered.png");
        // Deliberately NOT creating the golden ahead of time.
        let golden = dir.join("subdir/golden.png");
        write_png(&rendered, 4, 4, [100, 120, 140, 255]);

        SnapshotAssertion::new(&rendered, &golden).assert();

        assert!(golden.exists(), "golden should have been seeded on first run");
    }
}
