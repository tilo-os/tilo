//! The on-disk photo/video library.
//!
//! Captures are ordinary files in a directory — the file system is the
//! source of truth for what is in the gallery, not a bundled sample list.
//! On a qOS install this directory lives under the user's Pictures (or the
//! P2P workspace mount), so the library reflects real content the moment
//! the app opens and stays in sync with anything else writing there.
//!
//! Responsibilities kept here (pure IO, no rendering / no CRDT):
//!   * [`resolve_dir`]   — where captures live (overridable for tests).
//!   * [`scan`]          — list the directory into [`Capture`] metadata.
//!   * [`write_photo`]   — encode an RGBA frame to a timestamped PNG.
//!   * [`load_thumb`]    — decode + downscale a file for the gallery.
//!   * [`delete`]        — remove a backing file.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::frame::FrameBuf;
use crate::model::{Capture, CaptureKind, Facing};

/// File extensions we surface as photos / videos.
const PHOTO_EXT: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp"];
const VIDEO_EXT: &[&str] = &["mp4", "webm", "mov", "mkv", "m4v"];

/// Resolve the capture directory. Honors `CAMERA_DIR` (used by tests and to
/// point the app at the P2P workspace), otherwise `$XDG_PICTURES_DIR/Camera`,
/// falling back to `~/Pictures/Camera`.
pub fn resolve_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CAMERA_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let pics = dirs::picture_dir().unwrap_or_else(|| {
        dirs::home_dir().map(|h| h.join("Pictures")).unwrap_or_else(|| PathBuf::from("."))
    });
    pics.join("Camera")
}

fn ext_lower(path: &Path) -> Option<String> {
    path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase())
}

fn kind_for(ext: &str) -> Option<CaptureKind> {
    if PHOTO_EXT.contains(&ext) {
        Some(CaptureKind::Photo)
    } else if VIDEO_EXT.contains(&ext) {
        Some(CaptureKind::Video)
    } else {
        None
    }
}

fn mtime_secs(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Scan `dir` for image/video files, newest first. Missing directory ⇒ empty
/// (a fresh install simply has an empty library until the first capture).
/// `favorites` marks which file names are starred.
pub fn scan(dir: &Path, favorites: &HashSet<String>) -> Vec<Capture> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(ext) = ext_lower(&path) else { continue };
        let Some(kind) = kind_for(&ext) else { continue };
        let id = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let (w, h) = if matches!(kind, CaptureKind::Photo) {
            image::image_dimensions(&path).unwrap_or((0, 0))
        } else {
            (0, 0)
        };
        let label = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(prettify_stem)
            .unwrap_or_else(|| id.clone());
        out.push(Capture {
            id: id.clone(),
            kind,
            ts: mtime_secs(&path),
            w,
            h,
            dur_ms: 0,
            label,
            favorite: favorites.contains(&id),
            facing: Facing::Back,
            scene: 0,
            path: Some(path),
        });
    }
    // Newest first; ties broken by name for stable ordering.
    out.sort_by(|a, b| b.ts.cmp(&a.ts).then_with(|| a.id.cmp(&b.id)));
    out
}

/// Turn a file stem like `IMG_20260701_193000` or `coast-drive` into a
/// friendlier label for the gallery card.
fn prettify_stem(stem: &str) -> String {
    let cleaned = stem.replace(['_', '-'], " ");
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return stem.to_string();
    }
    // Capitalize the first character; leave the rest as-is.
    let mut chars = cleaned.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => cleaned.to_string(),
    }
}

/// Encode `frame` to a PNG in `dir`, creating the directory on first use.
/// Returns the written file name (the capture id) on success.
pub fn write_photo(dir: &Path, frame: &FrameBuf) -> std::io::Result<String> {
    std::fs::create_dir_all(dir)?;
    let name = format!("IMG_{}.png", timestamp_name());
    let path = dir.join(&name);
    let file = std::fs::File::create(&path)?;
    let w = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, frame.w, frame.h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(png_io)?;
    writer.write_image_data(&frame.rgba).map_err(png_io)?;
    Ok(name)
}

fn png_io(e: png::EncodingError) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
}

/// Decode `path` and downscale to at most `w`×`h`, returning tightly packed
/// RGBA8 plus its dimensions. Aspect ratio is preserved (the result covers
/// the box on its shorter axis via the caller's fit mode).
pub fn load_thumb(path: &Path, w: u32, h: u32) -> Option<(Vec<u8>, u32, u32)> {
    let img = image::open(path).ok()?;
    let scaled = img.thumbnail(w.max(1), h.max(1)).to_rgba8();
    let (sw, sh) = (scaled.width(), scaled.height());
    Some((scaled.into_raw(), sw, sh))
}

/// Decode `path` to a greyscale (luma) buffer, downscaled so its longest
/// side is at most `max` pixels. Returns the buffer plus its dimensions.
/// This is what the scanner feeds to the QR detector when scanning a code
/// out of a photo that already lives in the library (no camera required).
pub fn load_luma(path: &Path, max: u32) -> Option<(Vec<u8>, usize, usize)> {
    let img = image::open(path).ok()?;
    // `thumbnail` bounds the longest side to `max` while preserving aspect,
    // keeping enough resolution for the detector to resolve finder patterns.
    let luma = img.thumbnail(max.max(1), max.max(1)).to_luma8();
    let (w, h) = (luma.width() as usize, luma.height() as usize);
    Some((luma.into_raw(), w, h))
}

/// Remove the backing file for a capture id in `dir`.
pub fn delete(dir: &Path, id: &str) -> std::io::Result<()> {
    let path = dir.join(id);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// A filesystem-safe, sortable timestamp `YYYYMMDD_HHMMSS_mmm`.
fn timestamp_name() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();
    // Civil-date conversion (proleptic Gregorian) — avoids pulling in chrono.
    let days = (secs / 86_400) as i64;
    let rem = (secs % 86_400) as u32;
    let (y, mo, d) = civil_from_days(days);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}{mo:02}{d:02}_{hh:02}{mm:02}{ss:02}_{millis:03}")
}

/// Days since Unix epoch → (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir()
            .join(format!("camera-lib-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn write_then_scan_round_trips() {
        let dir = tmp();
        let frame = crate::frame::viewfinder(1, 48, 32);
        let name = write_photo(&dir, &frame).expect("write");
        assert!(dir.join(&name).exists());
        let caps = scan(&dir, &HashSet::new());
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].id, name);
        assert_eq!(caps[0].w, 48);
        assert_eq!(caps[0].h, 32);
        assert!(caps[0].path.is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn thumb_downscales_below_box() {
        let dir = tmp();
        let frame = crate::frame::viewfinder(2, 200, 120);
        let name = write_photo(&dir, &frame).unwrap();
        let (rgba, tw, th) = load_thumb(&dir.join(&name), 50, 50).expect("thumb");
        assert!(tw <= 50 && th <= 50, "downscaled to {tw}x{th}");
        assert_eq!(rgba.len(), (tw * th * 4) as usize);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn delete_removes_file_and_is_idempotent() {
        let dir = tmp();
        let name = write_photo(&dir, &crate::frame::viewfinder(0, 16, 16)).unwrap();
        assert!(dir.join(&name).exists());
        delete(&dir, &name).unwrap();
        assert!(!dir.join(&name).exists());
        delete(&dir, &name).unwrap(); // second delete is a no-op
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_dir_scans_empty() {
        let dir = std::env::temp_dir().join("camera-lib-does-not-exist-xyz");
        assert!(scan(&dir, &HashSet::new()).is_empty());
    }

    #[test]
    fn civil_date_matches_known_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(1), (1970, 1, 2));
        assert_eq!(civil_from_days(31), (1970, 2, 1));
        // 18_809 days after the epoch is 2021-07-01.
        assert_eq!(civil_from_days(18_809), (2021, 7, 1));
    }
}
