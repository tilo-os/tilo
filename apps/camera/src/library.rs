//! Bounded file encoding and image decoding for Camera effects.
//!
//! On a Tilo install captured bytes may be projected into the user's Pictures
//! directory, while ownership and collection management remain with Photos or
//! Video Player.
//!
//! Responsibilities kept here (pure IO, no rendering / no CRDT):
//!   * [`resolve_dir`]   — where captures live (overridable for tests).
//!   * [`write_photo`]   — encode an RGBA frame to a timestamped JPEG/PNG.
//!   * [`load_luma`]     — decode + downscale an image for the code scanner.

use std::path::{Path, PathBuf};

use crate::frame::{self, FrameBuf};
use crate::model::PhotoFormat;

/// Resolve the capture directory. Honors `CAMERA_DIR` (used by tests and to
/// point the app at the P2P workspace), otherwise `$XDG_PICTURES_DIR/Camera`,
/// falling back to `~/Pictures/Camera`.
pub fn resolve_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CAMERA_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    let pics = tilo_paths::picture_dir().unwrap_or_else(|| {
        tilo_paths::home().map_or_else(|| PathBuf::from("."), |h| h.join("Pictures"))
    });
    pics.join("Camera")
}

/// Encode `frame` to `dir` in the requested `format` (a real JPEG or a real
/// PNG), creating the directory on first use. Returns the written file name
/// (the capture id) on success.
///
/// # Errors
///
/// Returns an error if `dir` cannot be created or the file cannot be written
/// or encoded.
pub fn write_photo(dir: &Path, frame: &FrameBuf, format: PhotoFormat) -> std::io::Result<String> {
    let (name, bytes) = encode_photo(frame, format)?;
    crate::files::write(&dir.join(&name), &bytes)?;
    Ok(name)
}

/// Encode `frame` in `format` under a timestamped capture name, for a host
/// with no directory to write it to.
///
/// # Errors
///
/// Returns an error if the frame is invalid or cannot be encoded.
pub fn encode_photo(frame: &FrameBuf, format: PhotoFormat) -> std::io::Result<(String, Vec<u8>)> {
    if !valid_frame(frame) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid camera frame",
        ));
    }
    let name = format!("IMG_{}.{}", timestamp_name(), format.extension());
    let bytes = match format {
        PhotoFormat::Png => encode_png(frame)?,
        PhotoFormat::Jpeg => encode_jpeg(frame)?,
    };
    Ok((name, bytes))
}

fn encode_png(frame: &FrameBuf) -> std::io::Result<Vec<u8>> {
    use image::{ExtendedColorType, ImageEncoder};
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(&frame.rgba, frame.w, frame.h, ExtendedColorType::Rgba8)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(out)
}

fn encode_jpeg(frame: &FrameBuf) -> std::io::Result<Vec<u8>> {
    use image::{ExtendedColorType, ImageEncoder};
    let rgb = frame::rgba_to_rgb(&frame.rgba);
    let mut out = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90);
    enc.write_image(&rgb, frame.w, frame.h, ExtendedColorType::Rgb8)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(out)
}

/// Decode image bytes to a greyscale (luma) buffer, downscaled so its longest
/// side is at most `max` pixels, plus its dimensions — what the scanner feeds
/// to the QR detector when scanning a code out of a chosen image. The shared
/// decoder reads the header first, so attacker-controlled dimensions are
/// refused before any pixel is allocated even after the content store has
/// verified the byte hash.
pub fn load_luma_bytes(bytes: &[u8], max: u32) -> Option<(Vec<u8>, usize, usize)> {
    let img = thumbnail::decode_image(bytes)?;
    // `thumbnail` bounds the longest side to `max` while preserving aspect,
    // keeping enough resolution for the detector to resolve finder patterns.
    let luma = img.thumbnail(max.max(1), max.max(1)).to_luma8();
    let (w, h) = (luma.width() as usize, luma.height() as usize);
    Some((luma.into_raw(), w, h))
}

fn valid_frame(frame: &FrameBuf) -> bool {
    let Some(pixels) = u64::from(frame.w).checked_mul(u64::from(frame.h)) else {
        return false;
    };
    let Some(bytes) = pixels.checked_mul(4).and_then(|n| usize::try_from(n).ok()) else {
        return false;
    };
    frame.w > 0
        && frame.h > 0
        && pixels <= thumbnail::MAX_DECODE_PIXELS
        && frame.rgba.len() == bytes
}

/// A filesystem-safe, sortable timestamp `YYYYMMDD_HHMMSS_mmm`. Shared by the
/// photo and video writers so capture file names are consistent.
pub fn timestamp_name() -> String {
    let now_ms = ui::time::unix_now_ms();
    let secs = now_ms / 1_000;
    let millis = now_ms % 1_000;
    // Civil-date conversion (proleptic Gregorian) — avoids pulling in chrono.
    let days = (secs / 86_400) as i64;
    let rem = (secs % 86_400) as u32;
    let (y, mo, d) = ui::date::civil_from_days(days);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}{mo:02}{d:02}_{hh:02}{mm:02}{ss:02}_{millis:03}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("camera-lib-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn write_photo_stores_real_decodable_images_and_refuses_malformed_frames() {
        // Phase `write_round_trips_real_dimensions`.
        {
            let dir = tmp();
            let frame = crate::frame::viewfinder(1, 48, 32);
            let name = write_photo(&dir, &frame, PhotoFormat::Png).expect("write");
            assert!(dir.join(&name).exists());
            assert!(
                Path::new(&name)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("png"))
            );
            assert_eq!(image::image_dimensions(dir.join(&name)).unwrap(), (48, 32));
            std::fs::remove_dir_all(&dir).ok();
        }
        // Phase `jpeg_format_writes_a_real_jpeg`.
        {
            let dir = tmp();
            let frame = crate::frame::viewfinder(3, 64, 48);
            let name = write_photo(&dir, &frame, PhotoFormat::Jpeg).expect("write");
            assert!(
                Path::new(&name)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("jpg"))
            );
            // The bytes are a genuine JPEG (SOI marker) and decode at full size.
            let bytes = std::fs::read(dir.join(&name)).unwrap();
            assert_eq!(&bytes[0..2], &[0xFF, 0xD8], "JPEG SOI marker");
            let (w, h) = image::image_dimensions(dir.join(&name)).unwrap();
            assert_eq!((w, h), (64, 48));
            std::fs::remove_dir_all(&dir).ok();
        }
        // Phase `scanner_luma_downscales_below_box`.
        {
            let dir = tmp();
            let frame = crate::frame::viewfinder(2, 200, 120);
            let name = write_photo(&dir, &frame, PhotoFormat::Png).unwrap();
            let (luma, tw, th) =
                load_luma_bytes(&std::fs::read(dir.join(&name)).unwrap(), 50).expect("luma");
            assert!(tw <= 50 && th <= 50, "downscaled to {tw}x{th}");
            assert_eq!(luma.len(), tw * th);
            std::fs::remove_dir_all(&dir).ok();
        }
        // Phase `malformed_frames_are_refused_before_io`.
        {
            let dir = tmp();
            assert!(
                write_photo(
                    &dir,
                    &FrameBuf {
                        rgba: vec![0; 1],
                        w: 8,
                        h: 8,
                    },
                    PhotoFormat::Png,
                )
                .is_err()
            );
            std::fs::remove_dir_all(&dir).ok();
        }
    }
}
