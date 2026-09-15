//! Shared OCR pipeline for Scanner, Photos, Files, and image editors.
//!
//! The service owns preprocessing and the external-engine boundary. Callers get
//! structured words with confidence and geometry, not an irreversible text blob.

use std::ffi::OsStr;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use image::{DynamicImage, ImageFormat, imageops};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_EDGE: u32 = 10_000;
const MAX_PIXELS: u64 = 60_000_000;
const MAX_TSV_BYTES: usize = 16 * 1024 * 1024;
const MAX_WORDS: usize = 250_000;
const MAX_WORD_BYTES: usize = 4096;
const MAX_ENGINE_STDERR_BYTES: usize = 4096;
const ENGINE_TIMEOUT: Duration = Duration::from_secs(120);

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

/// Contrast-normalise and orient an image before recognition. The returned
/// image remains available to the UI for an honest before/after preview.
///
/// # Errors
/// Returns [`Error::TooLarge`] when dimensions exceed the processing bounds.
pub fn preprocess(image: &DynamicImage) -> Result<DynamicImage, Error> {
    let (width, height) = (image.width(), image.height());
    validate_dimensions(width, height)?;
    let gray = image.to_luma8();
    let contrasted = imageops::contrast(&gray, 22.0);
    Ok(DynamicImage::ImageLuma8(contrasted))
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
    validate_language(language)?;
    let prepared = preprocess(image)?;
    let mut input = tempfile::Builder::new().suffix(".png").tempfile()?;
    prepared.write_to(&mut input, ImageFormat::Png)?;
    recognize_path(input.path(), language)
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
    let mut child = Command::new(program)
        .args(program_args)
        .arg(path)
        .arg("stdout")
        .arg("-l")
        .arg(language)
        .arg("tsv")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::Unavailable
            } else {
                Error::Io(error)
            }
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Engine("OCR output stream was unavailable".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::Engine("OCR error stream was unavailable".into()))?;
    let stdout_rx = read_stream_bounded(stdout, MAX_TSV_BYTES);
    let stderr_rx = read_stream_bounded(stderr, MAX_ENGINE_STDERR_BYTES);
    let started = Instant::now();
    let (status, stdout) = 'process: loop {
        if let Ok(result) = stdout_rx.try_recv() {
            let bytes = result?;
            if bytes.len() > MAX_TSV_BYTES {
                terminate(&mut child);
                return Err(Error::Engine("OCR output exceeded the safety limit".into()));
            }
            if let Some(status) = child.try_wait()? {
                break (status, bytes);
            }
            // The reader reached EOF just before the process became waitable.
            // Keep polling without allocating any more output.
            loop {
                if let Some(status) = child.try_wait()? {
                    break 'process (status, bytes);
                }
                if started.elapsed() >= timeout {
                    terminate(&mut child);
                    return Err(Error::Engine("OCR engine timed out".into()));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        if let Some(status) = child.try_wait()? {
            let bytes = stdout_rx
                .recv()
                .map_err(|_| Error::Engine("OCR output reader stopped unexpectedly".into()))??;
            break (status, bytes);
        }
        if started.elapsed() >= timeout {
            terminate(&mut child);
            return Err(Error::Engine("OCR engine timed out".into()));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    // The process may become waitable before the reader notification wins the
    // race above (notably after its writer receives SIGPIPE at our bound).
    // Enforce the same limit on the joined path so an oversized stream can
    // never be misreported as a generic engine exit.
    if stdout.len() > MAX_TSV_BYTES {
        terminate(&mut child);
        return Err(Error::Engine("OCR output exceeded the safety limit".into()));
    }
    let stderr = stderr_rx
        .recv()
        .map_err(|_| Error::Engine("OCR error reader stopped unexpectedly".into()))??;
    if !status.success() {
        return Err(Error::Engine(
            String::from_utf8_lossy(&stderr)
                .trim()
                .chars()
                .take(500)
                .collect(),
        ));
    }
    parse_tsv(&String::from_utf8_lossy(&stdout), language)
}

fn read_stream_bounded(
    reader: impl Read + Send + 'static,
    limit: usize,
) -> mpsc::Receiver<Result<Vec<u8>, std::io::Error>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let take = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
        let result = reader.take(take).read_to_end(&mut bytes).map(|_| bytes);
        let _ = tx.send(result);
    });
    rx
}

fn terminate(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
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

    #[cfg(unix)]
    fn fake_engine(body: &str, timeout: Duration) -> Result<Document, Error> {
        recognize_path_with_program_args_timeout(
            Path::new("input with spaces.png"),
            "eng",
            Path::new("/bin/sh"),
            &[OsStr::new("-c"), OsStr::new(body), OsStr::new("fake-ocr")],
            timeout,
        )
    }

    #[test]
    fn parses_geometry_confidence_and_lines() {
        let input = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n5\t1\t1\t1\t1\t1\t10\t20\t30\t12\t96.5\tHello\n5\t1\t1\t1\t2\t1\t10\t40\t40\t12\t80\tworld";
        let document = parse_tsv(input, "eng").unwrap();
        assert_eq!(document.plain_text(), "Hello\nworld");
        assert_eq!(document.words[0].bounds.x, 10);
        assert!((document.words[0].confidence - 0.965).abs() < 0.001);
    }

    #[test]
    fn language_cannot_inject_options_or_paths() {
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

    #[test]
    fn oversized_images_are_rejected_before_engine_work() {
        let image = DynamicImage::new_luma8(MAX_EDGE + 1, 1);
        assert!(matches!(preprocess(&image), Err(Error::TooLarge)));
    }

    #[test]
    fn preprocessing_is_deterministic_grayscale_and_contrast_enhancing() {
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

    #[test]
    fn exact_pixel_limit_is_accepted_but_one_pixel_over_is_rejected() {
        assert!(validate_dimensions(10_000, 6_000).is_ok());
        assert!(matches!(
            validate_dimensions(10_000, 6_001),
            Err(Error::TooLarge)
        ));
    }

    #[test]
    fn unicode_words_and_page_block_line_boundaries_are_preserved() {
        let header = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n";
        let rows = "5\t1\t1\t1\t1\t1\t0\t0\t20\t10\t99\tGrüße\n5\t1\t2\t1\t1\t1\t0\t20\t20\t10\t91\t世界\n5\t2\t1\t1\t1\t1\t0\t0\t20\t10\t87\tمرحبا";
        let document = parse_tsv(&format!("{header}{rows}"), "deu+jpn+ara").unwrap();
        assert_eq!(document.plain_text(), "Grüße\n世界\nمرحبا");
        assert_eq!(document.words.len(), 3);
    }

    #[test]
    fn engine_rows_are_ordered_and_paragraphs_never_merge() {
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

    #[test]
    fn malformed_non_word_and_negative_confidence_rows_are_ignored() {
        let input = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n1\t1\t0\t0\t0\t0\t0\t0\t100\t100\t-1\tpage\n5\tbogus\t1\t1\t1\t1\t0\t0\t10\t10\t99\tbad-number\n5\t1\t1\t1\t1\t1\t0\t0\t10\t10\t-1\trejected\n5\t1\t1\t1\t1\t2\t2\t3\t10\t11\t0\taccepted";
        let document = parse_tsv(input, "eng").unwrap();
        assert_eq!(document.plain_text(), "accepted");
        assert!(document.words[0].confidence.abs() < f32::EPSILON);
    }

    #[test]
    fn zero_sized_and_overflowing_geometry_is_rejected() {
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

    #[test]
    fn large_deterministic_malformed_corpus_never_panics_or_leaks_invalid_words() {
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

    #[test]
    fn confidence_from_an_untrusted_engine_is_bounded() {
        let input = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n5\t1\t1\t1\t1\t1\t0\t0\t10\t10\t140\toverconfident";
        let document = parse_tsv(input, "eng").unwrap();
        assert!((document.words[0].confidence - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn adversarial_output_word_and_text_limits_fail_closed() {
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

    #[test]
    fn absent_engine_is_a_typed_error_when_not_installed() {
        if Command::new("tesseract").arg("--version").output().is_ok() {
            return;
        }
        let missing = recognize_path(Path::new("/definitely/not/an/image.png"), "eng");
        assert!(matches!(missing, Err(Error::Unavailable)));
    }

    #[cfg(unix)]
    #[test]
    fn engine_process_boundary_parses_successful_tsv() {
        let result = fake_engine(
            "printf 'level\\tpage_num\\tblock_num\\tpar_num\\tline_num\\tword_num\\tleft\\ttop\\twidth\\theight\\tconf\\ttext\\n5\\t1\\t1\\t1\\t1\\t1\\t7\\t8\\t20\\t9\\t94\\tReceipt\\n'",
            Duration::from_secs(2),
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

    #[cfg(unix)]
    #[test]
    fn engine_failure_is_bounded_and_typed() {
        let error =
            fake_engine("printf '%0600d' 0 >&2; exit 7", Duration::from_secs(2)).unwrap_err();
        match error {
            Error::Engine(message) => assert_eq!(message.chars().count(), 500),
            other => panic!("expected engine error, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn runaway_engine_is_killed_at_the_output_limit() {
        let error = fake_engine("yes x", Duration::from_secs(2)).unwrap_err();
        assert!(matches!(error, Error::Engine(message) if message.contains("safety limit")));
    }

    #[cfg(unix)]
    #[test]
    fn wedged_engine_is_killed_at_the_deadline() {
        let started = Instant::now();
        let error = fake_engine("while :; do :; done", Duration::from_millis(50)).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(matches!(error, Error::Engine(message) if message.contains("timed out")));
    }
}
