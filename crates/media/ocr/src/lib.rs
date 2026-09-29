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

use image::{DynamicImage, GrayImage, ImageFormat, Luma, imageops};
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

/// Structured recognition plus the visible preprocessing decision that led to
/// it. Apps can explain a corrected scan and keep previews/export aligned
/// instead of treating cleanup as an invisible engine side effect.
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
    /// A stable, people-facing explanation suitable for app notices.
    ///
    /// Engine stderr and filesystem details can contain implementation jargon,
    /// paths, or sensitive document context, so callers must not display the
    /// error's diagnostic `Display` representation directly.
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
/// image remains available to the UI for an honest before/after preview.
///
/// # Errors
/// Returns [`Error::TooLarge`] when dimensions exceed the processing bounds.
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

/// Estimate the small corrective rotation needed to make text baselines
/// horizontal. A capped preview keeps work bounded for full-resolution scans;
/// blank pages, photographs, and weak/ambiguous evidence return `None` rather
/// than rotating content speculatively.
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

/// Recognise an in-memory image with Tesseract's TSV output. `language` is
/// restricted to data identifiers, preventing it from becoming an argv option
/// or filesystem path.
///
/// # Errors
/// Returns a typed validation, image, I/O, availability, timeout, or bounded
/// engine-output error. No partial OCR document is returned on failure.
pub fn recognize(image: &DynamicImage, language: &str) -> Result<Document, Error> {
    recognize_detailed(image, language).map(|result| result.document)
}

/// Recognise an image and report any automatic page straightening applied
/// before the engine saw it.
///
/// # Errors
/// Returns the same typed validation, image, I/O, availability, timeout, or
/// bounded engine-output errors as [`recognize`].
pub fn recognize_detailed(image: &DynamicImage, language: &str) -> Result<Recognition, Error> {
    validate_language(language)?;
    let (prepared, deskew_degrees) = prepare(image)?;
    // The engine is a process reading a file; the page has neither, and
    // `tempfile` would panic there looking for a temp dir.
    let scratch = tilo_paths::temp_dir().ok_or(Error::Unavailable)?;
    let mut input = tempfile::Builder::new()
        .suffix(".png")
        .tempfile_in(scratch)?;
    prepared.write_to(&mut input, ImageFormat::Png)?;
    recognize_path(input.path(), language).map(|document| Recognition {
        document,
        deskew_degrees,
    })
}

/// Recognise an image path through the bounded OCR process boundary.
///
/// # Errors
/// Returns the same typed validation, process, timeout, and output errors as
/// [`recognize`].
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
        return Err(Error::Engine(
            String::from_utf8_lossy(&output.stderr)
                .trim()
                .chars()
                .filter(|character| {
                    !character.is_control() || matches!(character, ' ' | '\n' | '\t')
                })
                .take(500)
                .collect(),
        ));
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

/// Parse bounded Tesseract TSV into structured words.
///
/// # Errors
/// Returns an error for invalid language identifiers or output exceeding the
/// global byte/word limits.
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
            // Engines conventionally report 0..=100, but this is still an
            // external process boundary. Keep the public confidence contract
            // in 0..=1 even when a buggy engine emits 140 or infinity.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    /// Run `body` as the engine; its deadline only catches a hang, so every
    /// bound the result reports comes from the output limits.
    #[cfg(unix)]
    fn fake_engine(body: &str) -> Result<Document, Error> {
        fake_engine_with_deadline(body, tilo_test_env::hang_bound())
    }

    #[cfg(unix)]
    fn fake_engine_with_deadline(body: &str, timeout: Duration) -> Result<Document, Error> {
        recognize_path_with_program_args_timeout(
            Path::new("input with spaces.png"),
            "eng",
            Path::new("/bin/sh"),
            &[OsStr::new("-c"), OsStr::new(body), OsStr::new("fake-ocr")],
            timeout,
        )
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn engine_output_parses_into_bounded_ordered_words() {
        // Phase `parses_geometry_confidence_and_lines`.
        {
            let input = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n5\t1\t1\t1\t1\t1\t10\t20\t30\t12\t96.5\tHello\n5\t1\t1\t1\t2\t1\t10\t40\t40\t12\t80\tworld";
            let document = parse_tsv(input, "eng").unwrap();
            assert_eq!(document.plain_text(), "Hello\nworld");
            assert_eq!(document.words[0].bounds.x, 10);
            assert!((document.words[0].confidence - 0.965).abs() < 0.001);
        }
        // Phase `unicode_words_and_page_block_line_boundaries_are_preserved`.
        {
            let header = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n";
            let rows = "5\t1\t1\t1\t1\t1\t0\t0\t20\t10\t99\tGrüße\n5\t1\t2\t1\t1\t1\t0\t20\t20\t10\t91\t世界\n5\t2\t1\t1\t1\t1\t0\t0\t20\t10\t87\tمرحبا";
            let document = parse_tsv(&format!("{header}{rows}"), "deu+jpn+ara").unwrap();
            assert_eq!(document.plain_text(), "Grüße\n世界\nمرحبا");
            assert_eq!(document.words.len(), 3);
        }
        // Phase `engine_rows_are_ordered_and_paragraphs_never_merge`.
        {
            let header = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n";
            let rows = "5\t1\t1\t2\t1\t2\t40\t40\t10\t10\t90\tparagraph\n\
                        5\t1\t1\t1\t1\t2\t20\t10\t10\t10\t90\tworld\n\
                        5\t1\t1\t2\t1\t1\t10\t40\t10\t10\t90\tNew\n\
                        5\t1\t1\t1\t1\t1\t0\t10\t10\t10\t90\tHello";
            let document = parse_tsv(&format!("{header}{rows}"), "eng").unwrap();
            assert_eq!(document.plain_text(), "Hello world\nNew paragraph");
            assert_eq!(document.words[2].paragraph, 2);
            assert_eq!(document.words[3].position, 2);
        }
        // Phase `malformed_non_word_and_negative_confidence_rows_are_ignored`.
        {
            let input = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n1\t1\t0\t0\t0\t0\t0\t0\t100\t100\t-1\tpage\n5\tbogus\t1\t1\t1\t1\t0\t0\t10\t10\t99\tbad-number\n5\t1\t1\t1\t1\t1\t0\t0\t10\t10\t-1\trejected\n5\t1\t1\t1\t1\t2\t2\t3\t10\t11\t0\taccepted";
            let document = parse_tsv(input, "eng").unwrap();
            assert_eq!(document.plain_text(), "accepted");
            assert!(document.words[0].confidence.abs() < f32::EPSILON);
        }
        // Phase `zero_sized_and_overflowing_geometry_is_rejected`.
        {
            let header = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n";
            let rows = "5\t1\t1\t1\t1\t1\t0\t0\t0\t10\t99\tzero-width\n\
                        5\t1\t1\t1\t1\t2\t0\t0\t10\t0\t99\tzero-height\n\
                        5\t1\t1\t1\t1\t3\t4294967295\t0\t1\t1\t99\tx-overflow\n\
                        5\t1\t1\t1\t1\t4\t0\t4294967295\t1\t1\t99\ty-overflow\n\
                        5\t1\t1\t1\t1\t5\t5\t6\t7\t8\t99\tvalid";
            let document = parse_tsv(&format!("{header}{rows}"), "eng").unwrap();
            assert_eq!(document.plain_text(), "valid");
            assert_eq!(
                document.words[0].bounds,
                Bounds {
                    x: 5,
                    y: 6,
                    width: 7,
                    height: 8
                }
            );
        }
        // Phase `large_deterministic_malformed_corpus_never_panics_or_leaks_invalid_words`.
        {
            let mut input = String::from(
                "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n",
            );
            let mut seed = 0x9e37_79b9_u32;
            for index in 0..10_000_u32 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                match seed % 6 {
                    0 => input.push_str("\t\t\t\n"),
                    1 => input.push_str("5\t-1\tnope\t1\t1\t1\t0\t0\t1\t1\tNaN\tbad\n"),
                    2 => input.push_str("5\t1\t1\t1\t1\t1\t4294967295\t0\t2\t1\t99\tbad\n"),
                    3 => input.push_str("999999999999999999999999999999999999\n"),
                    4 => input.push_str("5\t1\t1\t1\t1\t1\t0\t0\t1\t1\tinf\t世界\n"),
                    _ => {
                        let _ = writeln!(
                            input,
                            "5\t1\t1\t1\t{}\t{}\t0\t0\t1\t1\t80\tok-{index}",
                            index % 17,
                            index
                        );
                    }
                }
            }
            let document = parse_tsv(&input, "eng").unwrap();
            assert!(!document.words.is_empty());
            assert!(document.words.iter().all(|word| {
                !word.text.is_empty()
                    && word.bounds.width > 0
                    && word.bounds.height > 0
                    && word.bounds.x.checked_add(word.bounds.width).is_some()
                    && word.bounds.y.checked_add(word.bounds.height).is_some()
                    && word.confidence.is_finite()
                    && (0.0..=1.0).contains(&word.confidence)
            }));
        }
        // Phase `confidence_from_an_untrusted_engine_is_bounded`.
        {
            let input = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n5\t1\t1\t1\t1\t1\t0\t0\t10\t10\t140\toverconfident";
            let document = parse_tsv(input, "eng").unwrap();
            assert!((document.words[0].confidence - 1.0).abs() < f32::EPSILON);
        }
        // Phase `adversarial_output_word_and_text_limits_fail_closed`.
        {
            let header = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n";
            let row = "5\t1\t1\t1\t1\t1\t0\t0\t10\t10\t99\tok\n";
            assert!(parse_tsv_bounded(&format!("{header}{row}{row}"), "eng", 4096, 1, 16).is_err());
            assert!(parse_tsv_bounded(&format!("{header}{row}"), "eng", 8, 10, 16).is_err());
            let long = format!("5\t1\t1\t1\t1\t1\t0\t0\t10\t10\t99\t{}", "x".repeat(17));
            assert!(
                parse_tsv_bounded(&format!("{header}{long}"), "eng", 4096, 10, 16)
                    .unwrap()
                    .words
                    .is_empty()
            );
        }
    }

    #[test]
    fn inputs_are_validated_and_errors_are_typed_and_actionable() {
        // Phase `absent_engine_is_a_typed_error_when_not_installed`.
        fn absent_engine_is_a_typed_error_when_not_installed() {
            if Command::new("tesseract").arg("--version").output().is_ok() {
                return;
            }
            let missing = recognize_path(Path::new("/definitely/not/an/image.png"), "eng");
            assert!(matches!(missing, Err(Error::Unavailable)));
        }
        // Phase `language_cannot_inject_options_or_paths`.
        {
            assert!(matches!(
                parse_tsv("", "../eng"),
                Err(Error::InvalidLanguage)
            ));
            assert!(matches!(
                parse_tsv("", "eng --psm 1"),
                Err(Error::InvalidLanguage)
            ));
            assert!(matches!(parse_tsv("", "-eng"), Err(Error::InvalidLanguage)));
            assert!(matches!(parse_tsv("", "eng-"), Err(Error::InvalidLanguage)));
        }
        // Phase `oversized_images_are_rejected_before_engine_work`.
        {
            let image = DynamicImage::new_luma8(MAX_EDGE + 1, 1);
            assert!(matches!(preprocess(&image), Err(Error::TooLarge)));
        }
        // Phase `exact_pixel_limit_is_accepted_but_one_pixel_over_is_rejected`.
        {
            assert!(validate_dimensions(10_000, 6_000).is_ok());
            assert!(matches!(
                validate_dimensions(10_000, 6_001),
                Err(Error::TooLarge)
            ));
        }
        // Phase `absent_engine_is_a_typed_error_when_not_installed` (defined above).
        absent_engine_is_a_typed_error_when_not_installed();
        // Phase `people_facing_errors_are_actionable_and_never_expose_engine_diagnostics`.
        {
            let cases = [
                (Error::TooLarge, "smaller scan"),
                (Error::InvalidLanguage, "Choose another language"),
                (Error::Unavailable, "still keep and export"),
                (
                    Error::Engine("/private/document/path: secret engine failure".into()),
                    "clearer scan",
                ),
                (
                    Error::Io(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "/private/working/path",
                    )),
                    "Try again",
                ),
            ];
            for (error, action) in cases {
                let message = error.user_message();
                assert!(message.contains(action), "{message:?} lacked {action:?}");
                assert!(!message.contains("/private"));
                assert!(!message.to_ascii_lowercase().contains("engine failure"));
            }
        }
    }

    #[test]
    fn preprocessing_and_deskew_are_deterministic_and_conservative() {
        // Phase `preprocessing_is_deterministic_grayscale_and_contrast_enhancing`.
        {
            let mut source = image::GrayImage::new(3, 1);
            source.put_pixel(0, 0, image::Luma([64]));
            source.put_pixel(1, 0, image::Luma([128]));
            source.put_pixel(2, 0, image::Luma([192]));
            let source = DynamicImage::ImageLuma8(source);
            let first = preprocess(&source).unwrap().to_luma8();
            let second = preprocess(&source).unwrap().to_luma8();
            assert_eq!(first, second);
            assert!(first.get_pixel(0, 0)[0] < 64);
            assert!(first.get_pixel(2, 0)[0] > 192);
        }
        // Phase `deskew_recovers_small_camera_rotation_without_changing_canvas`.
        {
            let clean = ruled_document();
            for skew in [-4.5, -2.0, 1.5, 3.0, 4.5] {
                let skewed = rotate_luma(&clean, skew);
                let correction = estimate_skew_degrees(&skewed).expect("clear text baselines");
                assert!(
                    (correction + skew).abs() <= DESKEW_STEP_DEGREES,
                    "skew {skew}° produced correction {correction}°"
                );

                let before_threshold = otsu_threshold(&skewed);
                let before_score = projection_score(&skewed, before_threshold, 0.0);
                let corrected = preprocess(&DynamicImage::ImageLuma8(skewed))
                    .unwrap()
                    .to_luma8();
                assert_eq!(corrected.dimensions(), clean.dimensions());
                let threshold = otsu_threshold(&corrected);
                assert!(projection_score(&corrected, threshold, 0.0) > before_score);
            }
        }
        // Phase `deskew_declines_blank_tiny_and_photo_like_inputs`.
        {
            assert!(estimate_skew_degrees(&GrayImage::from_pixel(400, 300, Luma([255]))).is_none());
            assert!(estimate_skew_degrees(&GrayImage::from_pixel(16, 16, Luma([0]))).is_none());
            let checker = GrayImage::from_fn(320, 240, |x, y| {
                if (x + y) % 2 == 0 {
                    Luma([0])
                } else {
                    Luma([255])
                }
            });
            assert!(estimate_skew_degrees(&checker).is_none());
        }
        // Phase `deskew_rotation_preserves_antialiased_glyph_edges`.
        {
            let source = GrayImage::from_fn(96, 96, |x, y| {
                if (24..72).contains(&x) && (42..48).contains(&y) {
                    Luma([0])
                } else {
                    Luma([255])
                }
            });
            let rotated = rotate_luma(&source, 2.5);
            assert!(
                rotated.pixels().any(|pixel| pixel[0] > 0 && pixel[0] < 255),
                "subpixel rotation should blend an edge instead of stair-stepping it"
            );
            assert_eq!(rotated.dimensions(), source.dimensions());
        }
    }

    fn ruled_document() -> GrayImage {
        GrayImage::from_fn(420, 280, |x, y| {
            let on_text_line = [48_u32, 92, 136, 180, 224]
                .iter()
                .any(|line| y >= *line && y < line + 4 && (28..390).contains(&x));
            if on_text_line && (x / 34) % 5 != 4 {
                Luma([24])
            } else {
                Luma([244])
            }
        })
    }

    #[cfg(unix)]
    #[test]
    fn the_engine_process_boundary_is_bounded_and_fails_closed() {
        // Phase `engine_process_boundary_parses_successful_tsv`.
        {
            let result = fake_engine(
                "printf 'level\\tpage_num\\tblock_num\\tpar_num\\tline_num\\tword_num\\tleft\\ttop\\twidth\\theight\\tconf\\ttext\\n5\\t1\\t1\\t1\\t1\\t1\\t7\\t8\\t20\\t9\\t94\\tReceipt\\n'",
            )
            .unwrap();
            assert_eq!(result.plain_text(), "Receipt");
            assert_eq!(
                result.words[0].bounds,
                Bounds {
                    x: 7,
                    y: 8,
                    width: 20,
                    height: 9
                }
            );
        }
        // Phase `engine_failure_is_bounded_and_typed`.
        {
            let error = fake_engine("printf '%0600d' 0 >&2; exit 7").unwrap_err();
            match error {
                Error::Engine(message) => assert_eq!(message.chars().count(), 500),
                other => panic!("expected engine error, got {other:?}"),
            }
        }
        // Phase `noisy_successful_engine_fails_closed_without_deadlocking`.
        {
            let error = fake_engine(
                "(yes diagnostic | head -c 100000 >&2) || :; printf 'level\\tpage_num\\tblock_num\\tpar_num\\tline_num\\tword_num\\tleft\\ttop\\twidth\\theight\\tconf\\ttext\\n5\\t1\\t1\\t1\\t1\\t1\\t0\\t0\\t12\\t8\\t99\\tDone\\n'",
            )
            .unwrap_err();
            assert!(matches!(error, Error::Engine(message) if message.contains("diagnostics")));
        }
        // Phase `runaway_engine_is_killed_at_the_output_limit`.
        {
            let error = fake_engine("yes x").unwrap_err();
            assert!(matches!(error, Error::Engine(message) if message.contains("safety limit")));
        }
        // Phase `wedged_engine_is_killed_at_the_deadline`.
        {
            let error = fake_engine_with_deadline("while :; do :; done", Duration::from_millis(50))
                .unwrap_err();
            assert!(matches!(error, Error::Engine(message) if message.contains("timed out")));
        }
    }

    #[cfg(unix)]
    #[test]
    #[allow(
        clippy::needless_collect,
        reason = "collecting starts every OCR worker before joining, which is the concurrency property under test"
    )]
    fn concurrent_engine_requests_keep_results_isolated() {
        let workers: Vec<_> = (0..8)
            .map(|index| {
                std::thread::spawn(move || {
                    let body = format!(
                        "printf 'level\\tpage_num\\tblock_num\\tpar_num\\tline_num\\tword_num\\tleft\\ttop\\twidth\\theight\\tconf\\ttext\\n5\\t1\\t1\\t1\\t1\\t1\\t{index}\\t0\\t10\\t10\\t90\\tword-{index}\\n'"
                    );
                    fake_engine(&body).unwrap()
                })
            })
            .collect();
        for (index, worker) in workers.into_iter().enumerate() {
            let document = worker.join().expect("OCR worker must not panic");
            assert_eq!(document.plain_text(), format!("word-{index}"));
            assert_eq!(document.words[0].bounds.x, index as u32);
        }
    }
}
