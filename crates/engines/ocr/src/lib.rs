//! Shared OCR pipeline for Scanner, Photos, Files, and image editors.
//!
//! The service owns preprocessing and the external-engine boundary. Callers get
//! structured words with confidence and geometry, not an irreversible text blob.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "deskew math operates on validated, capped image coordinates and finite ±5° angles"
)]

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tilo_image_codec::DecodedImage;

use image::{DynamicImage, GrayImage, Luma, imageops};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_EDGE: u32 = 10_000;
const MAX_PIXELS: u64 = 60_000_000;
const MAX_TSV_BYTES: usize = 16 * 1024 * 1024;
const MAX_WORDS: usize = 250_000;
const MAX_WORD_BYTES: usize = 4096;
const MAX_ENGINE_STDERR_BYTES: usize = 4096;
const ENGINE_TIMEOUT: Duration = Duration::from_secs(120);
const DESKEW_STEP_DEGREES: f64 = 0.5;
const DESKEW_STEPS: i32 = 10;
const DESKEW_PREVIEW_EDGE: u32 = 1200;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bounds {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub confidence: f32,
    pub bounds: Bounds,
    pub page: u32,
    pub block: u32,
    #[serde(default)]
    pub paragraph: u32,
    pub line: u32,
    #[serde(default)]
    pub position: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Document {
    pub words: Vec<Word>,
    pub language: String,
}

/// Structured recognition plus the preprocessing decision that led to it, so an
/// app can explain a corrected scan and keep previews and exports aligned.
#[derive(Clone, Debug, PartialEq)]
pub struct Recognition {
    pub document: Document,
    /// Corrective clockwise rotation in degrees, when evidence was strong
    /// enough to alter the image.
    pub deskew_degrees: Option<f64>,
}

impl Document {
    #[must_use]
    pub fn plain_text(&self) -> String {
        let mut text = String::new();
        let mut previous = None;
        for word in &self.words {
            let line = (word.page, word.block, word.paragraph, word.line);
            if previous.is_some_and(|value| value != line) {
                text.push('\n');
            } else if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&word.text);
            previous = Some(line);
        }
        text
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("image is too large for safe OCR")]
    TooLarge,
    #[error("invalid OCR language identifier")]
    InvalidLanguage,
    #[error("OCR engine is not installed")]
    Unavailable,
    #[error("OCR engine failed: {0}")]
    Engine(String),
    #[error(transparent)]
    Image(#[from] image::ImageError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    /// A stable explanation suitable for app notices.
    ///
    /// Engine stderr and filesystem details carry implementation jargon, paths
    /// and document context, so a caller must not show the diagnostic
    /// `Display`.
    #[must_use]
    pub const fn user_message(&self) -> &'static str {
        match self {
            Self::TooLarge => "This image is too large for text recognition. Try a smaller scan.",
            Self::InvalidLanguage => {
                "That recognition language is not available. Choose another language and try again."
            }
            Self::Unavailable => {
                "Text recognition is not installed. You can still keep and export the image."
            }
            Self::Engine(_) => {
                "Text could not be recognized in this image. Check the language or try a clearer scan."
            }
            Self::Image(_) => {
                "This image could not be prepared for text recognition. Try scanning it again."
            }
            Self::Io(_) => {
                "Text recognition could not access its working files. Try again in a moment."
            }
        }
    }
}

/// Contrast-normalise and deskew an image before recognition. The returned
/// image stays available to the UI for a before/after preview.
pub fn preprocess(image: &DynamicImage) -> Result<DynamicImage, Error> {
    prepare(image).map(|(image, _)| image)
}

fn prepare(image: &DynamicImage) -> Result<(DynamicImage, Option<f64>), Error> {
    let (width, height) = (image.width(), image.height());
    validate_dimensions(width, height)?;
    let gray = image.to_luma8();
    let contrasted = imageops::contrast(&gray, 22.0);
    let deskew_degrees = estimate_skew_degrees(&contrasted).filter(|angle| angle.abs() >= 0.4);
    let corrected = deskew_degrees.map_or_else(
        || contrasted.clone(),
        |angle| rotate_luma(&contrasted, angle),
    );
    Ok((DynamicImage::ImageLuma8(corrected), deskew_degrees))
}

/// Estimate the small corrective rotation that makes text baselines
/// horizontal. A capped preview keeps the work bounded; a blank page, a
/// photograph or weak evidence returns `None` rather than rotate.
#[must_use]
pub fn estimate_skew_degrees(image: &GrayImage) -> Option<f64> {
    if image.width() < 32 || image.height() < 32 {
        return None;
    }
    let preview = if image.width().max(image.height()) > DESKEW_PREVIEW_EDGE {
        imageops::resize(
            image,
            image.width().saturating_mul(DESKEW_PREVIEW_EDGE) / image.width().max(image.height()),
            image.height().saturating_mul(DESKEW_PREVIEW_EDGE) / image.width().max(image.height()),
            imageops::FilterType::Triangle,
        )
    } else {
        image.clone()
    };
    let threshold = otsu_threshold(&preview);
    let dark = preview
        .pixels()
        .filter(|pixel| pixel[0] <= threshold)
        .count();
    let total = usize::try_from(preview.width())
        .unwrap_or(usize::MAX)
        .saturating_mul(usize::try_from(preview.height()).unwrap_or(usize::MAX));
    if dark < total / 1_000 || dark > total.saturating_mul(2) / 5 {
        return None;
    }

    let baseline = projection_score(&preview, threshold, 0.0);
    let mut best = (0.0, baseline);
    for step in -DESKEW_STEPS..=DESKEW_STEPS {
        let angle = f64::from(step) * DESKEW_STEP_DEGREES;
        let score = projection_score(&preview, threshold, angle);
        if score > best.1 {
            best = (angle, score);
        }
    }
    (best.0.abs() >= 0.4 && best.1 > baseline.saturating_mul(108) / 100).then_some(best.0)
}

fn otsu_threshold(image: &GrayImage) -> u8 {
    let mut histogram = [0_u64; 256];
    for pixel in image.pixels() {
        if let Some(count) = histogram.get_mut(usize::from(pixel[0])) {
            *count = count.saturating_add(1);
        }
    }
    let total = u64::from(image.width()) * u64::from(image.height());
    let sum = histogram
        .iter()
        .enumerate()
        .fold(0_u64, |acc, (value, count)| {
            acc.saturating_add(u64::try_from(value).unwrap_or(255).saturating_mul(*count))
        });
    let mut background_count = 0_u64;
    let mut background_sum = 0_u64;
    let mut best_variance = 0.0;
    let mut best = 127_u8;
    for (value, count) in histogram.iter().enumerate() {
        background_count = background_count.saturating_add(*count);
        if background_count == 0 || background_count == total {
            continue;
        }
        background_sum = background_sum
            .saturating_add(u64::try_from(value).unwrap_or(255).saturating_mul(*count));
        let foreground_count = total - background_count;
        let background_mean = background_sum as f64 / background_count as f64;
        let foreground_mean = (sum - background_sum) as f64 / foreground_count as f64;
        let delta = background_mean - foreground_mean;
        let variance = background_count as f64 * foreground_count as f64 * delta * delta;
        if variance > best_variance {
            best_variance = variance;
            best = u8::try_from(value).unwrap_or(255);
        }
    }
    best
}

fn projection_score(image: &GrayImage, threshold: u8, correction_degrees: f64) -> u64 {
    let angle = correction_degrees.to_radians();
    let (sin, cos) = angle.sin_cos();
    let cx = (f64::from(image.width()) - 1.0) / 2.0;
    let cy = (f64::from(image.height()) - 1.0) / 2.0;
    let padding = ((f64::from(image.width()) * sin.abs()).ceil() as usize).saturating_add(2);
    let mut rows = vec![0_u32; usize::try_from(image.height()).unwrap_or(0) + padding * 2];
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel[0] > threshold {
            continue;
        }
        let projected = sin * (f64::from(x) - cx) + cos * (f64::from(y) - cy) + cy;
        let row = (projected.round() as isize).saturating_add(padding as isize);
        if let Ok(row) = usize::try_from(row)
            && let Some(value) = rows.get_mut(row)
        {
            *value = value.saturating_add(1);
        }
    }
    rows.into_iter().fold(0_u64, |score, count| {
        score.saturating_add(u64::from(count).saturating_mul(u64::from(count)))
    })
}

fn rotate_luma(image: &GrayImage, degrees: f64) -> GrayImage {
    let angle = degrees.to_radians();
    let (sin, cos) = angle.sin_cos();
    let cx = (f64::from(image.width()) - 1.0) / 2.0;
    let cy = (f64::from(image.height()) - 1.0) / 2.0;
    GrayImage::from_fn(image.width(), image.height(), |x, y| {
        let dx = f64::from(x) - cx;
        let dy = f64::from(y) - cy;
        let source_x = cos * dx + sin * dy + cx;
        let source_y = -sin * dx + cos * dy + cy;
        if source_x < 0.0
            || source_y < 0.0
            || source_x >= f64::from(image.width())
            || source_y >= f64::from(image.height())
        {
            return Luma([255]);
        }
        bilinear_luma(image, source_x, source_y)
    })
}

fn bilinear_luma(image: &GrayImage, x: f64, y: f64) -> Luma<u8> {
    let x0 = (x.floor() as u32).min(image.width().saturating_sub(1));
    let y0 = (y.floor() as u32).min(image.height().saturating_sub(1));
    let x1 = x0.saturating_add(1).min(image.width().saturating_sub(1));
    let y1 = y0.saturating_add(1).min(image.height().saturating_sub(1));
    let fx = x - f64::from(x0);
    let fy = y - f64::from(y0);
    let top = f64::from(image.get_pixel(x0, y0)[0]) * (1.0 - fx)
        + f64::from(image.get_pixel(x1, y0)[0]) * fx;
    let bottom = f64::from(image.get_pixel(x0, y1)[0]) * (1.0 - fx)
        + f64::from(image.get_pixel(x1, y1)[0]) * fx;
    Luma([((top * (1.0 - fy) + bottom * fy).round() as u8)])
}

fn validate_dimensions(width: u32, height: u32) -> Result<(), Error> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(Error::TooLarge)?;
    if width > MAX_EDGE || height > MAX_EDGE || pixels > MAX_PIXELS {
        return Err(Error::TooLarge);
    }
    Ok(())
}

/// Recognise decoded pixels with Tesseract's TSV output. `language` is
/// restricted to data identifiers, so it can become neither an argv option nor
/// a path.
pub fn recognize(image: &DecodedImage, language: &str) -> Result<Document, Error> {
    recognize_detailed(image, language).map(|result| result.document)
}

/// Recognise decoded pixels and report any automatic page straightening
/// applied before the engine saw it.
pub fn recognize_detailed(image: &DecodedImage, language: &str) -> Result<Recognition, Error> {
    validate_language(language)?;
    let image = image::RgbaImage::from_raw(image.width, image.height, image.rgba.clone())
        .map(DynamicImage::ImageRgba8)
        .ok_or(Error::TooLarge)?;
    let (prepared, deskew_degrees) = prepare(&image)?;
    // The engine is a process reading a file; the page has neither, and
    // `tempfile` would panic there looking for a temp dir.
    let scratch = tilo_paths::temp_dir().ok_or(Error::Unavailable)?;
    let mut input = tempfile::Builder::new()
        .suffix(".png")
        .tempfile_in(scratch)?;
    // The encoder is named, not looked up by format: a lookup keeps every
    // encoder the image crate has in the binary.
    prepared.write_with_encoder(image::codecs::png::PngEncoder::new(&mut input))?;
    recognize_path(input.path(), language).map(|document| Recognition {
        document,
        deskew_degrees,
    })
}

pub fn recognize_path(path: &Path, language: &str) -> Result<Document, Error> {
    recognize_path_with_program(path, language, Path::new("tesseract"))
}

fn recognize_path_with_program(
    path: &Path,
    language: &str,
    program: &Path,
) -> Result<Document, Error> {
    recognize_path_with_program_timeout(path, language, program, ENGINE_TIMEOUT)
}

fn recognize_path_with_program_timeout(
    path: &Path,
    language: &str,
    program: &Path,
    timeout: Duration,
) -> Result<Document, Error> {
    recognize_path_with_program_args_timeout(path, language, program, &[], timeout)
}

fn recognize_path_with_program_args_timeout(
    path: &Path,
    language: &str,
    program: &Path,
    program_args: &[&OsStr],
    timeout: Duration,
) -> Result<Document, Error> {
    validate_language(language)?;
    let mut command = Command::new(program);
    command
        .args(program_args)
        .arg(path)
        .arg("stdout")
        .arg("-l")
        .arg(language)
        .arg("tsv");
    let output = bounded_command::run_prepared_with_limits(
        command,
        None,
        MAX_TSV_BYTES,
        MAX_ENGINE_STDERR_BYTES,
        timeout,
    )
    .map_err(|error| match error {
        bounded_command::Error::Process(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Error::Unavailable
        }
        bounded_command::Error::Process(error) => Error::Io(error),
        bounded_command::Error::OutputTooLarge {
            stream: "stdout", ..
        } => Error::Engine("OCR output exceeded the safety limit".into()),
        bounded_command::Error::OutputTooLarge {
            stream: "stderr", ..
        } => Error::Engine("OCR diagnostics exceeded the safety limit".into()),
        bounded_command::Error::Timeout(_) => Error::Engine("OCR engine timed out".into()),
        _ => Error::Engine("OCR engine process failed".into()),
    })?;
    if !output.status.success() {
        // Only 500 characters of engine diagnostic reach an app.
        let diagnostic = output.stderr_diagnostic().chars().take(500).collect();
        return Err(Error::Engine(diagnostic));
    }
    parse_tsv(&String::from_utf8_lossy(&output.stdout), language)
}

fn validate_language(language: &str) -> Result<(), Error> {
    if language.is_empty()
        || language.len() > 64
        || !language
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        || !language
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        || !language
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'+' | b'-'))
    {
        return Err(Error::InvalidLanguage);
    }
    Ok(())
}

pub fn parse_tsv(tsv: &str, language: &str) -> Result<Document, Error> {
    parse_tsv_bounded(tsv, language, MAX_TSV_BYTES, MAX_WORDS, MAX_WORD_BYTES)
}

fn parse_tsv_bounded(
    tsv: &str,
    language: &str,
    max_tsv_bytes: usize,
    max_words: usize,
    max_word_bytes: usize,
) -> Result<Document, Error> {
    validate_language(language)?;
    if tsv.len() > max_tsv_bytes {
        return Err(Error::Engine("OCR output exceeded the safety limit".into()));
    }
    let mut words = Vec::new();
    for row in tsv.lines().skip(1) {
        let Ok(columns) = row.splitn(12, '\t').collect::<Vec<_>>().try_into() else {
            continue;
        };
        let [
            level,
            page_col,
            block_col,
            paragraph_col,
            line_col,
            word_col,
            x_col,
            y_col,
            width_col,
            height_col,
            confidence_col,
            text_col,
        ]: [&str; 12] = columns;
        if level != "5" || text_col.trim().is_empty() || text_col.len() > max_word_bytes {
            continue;
        }
        let number = |column: &str| column.parse::<u32>().ok();
        let Some((page, block, paragraph, line, position, x, y, width, height)) = number(page_col)
            .zip(number(block_col))
            .zip(number(paragraph_col))
            .zip(number(line_col))
            .zip(number(word_col))
            .zip(number(x_col))
            .zip(number(y_col))
            .zip(number(width_col))
            .zip(number(height_col))
            .map(|((((((((a, b), c), d), e), f), g), h), i)| (a, b, c, d, e, f, g, h, i))
        else {
            continue;
        };
        if width == 0
            || height == 0
            || x.checked_add(width).is_none()
            || y.checked_add(height).is_none()
        {
            continue;
        }
        let confidence = confidence_col.parse::<f32>().unwrap_or(-1.0);
        if confidence < 0.0 || !confidence.is_finite() {
            continue;
        }
        if words.len() >= max_words {
            return Err(Error::Engine("OCR returned too many words".into()));
        }
        words.push(Word {
            text: text_col.trim().to_string(),
            // Engines conventionally report 0..=100, but this is an external
            // process boundary: keep the public 0..=1 contract even when one
            // emits 140 or infinity.
            confidence: (confidence / 100.0).clamp(0.0, 1.0),
            bounds: Bounds {
                x,
                y,
                width,
                height,
            },
            page,
            block,
            paragraph,
            line,
            position,
        });
    }
    words.sort_by_key(|word| {
        (
            word.page,
            word.block,
            word.paragraph,
            word.line,
            word.position,
        )
    });
    Ok(Document {
        words,
        language: language.to_string(),
    })
}
