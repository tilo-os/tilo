//! Local, bounded image intelligence.
//!
//! Recognition happens from pixels available on this device.  The helper does
//! not write a document, start a network request, or retain an image; callers
//! choose whether a result stays local or becomes an explicit shared action.

use crate::model::{AnalysisProvenance, ImageAnalysis, OpenImage};
use crate::{doc, pixels};

/// Maximum text retained from an OCR engine.  This is deliberately below the
/// CRDT's practical change budget and prevents a pathological scan becoming a
/// collaborative denial-of-service payload.
pub const MAX_OCR_CHARS: usize = 32 * 1024;

/// Run the shared Tilo OCR pipeline on a locally available photograph.
///
/// # Errors
/// Returns a stable, non-sensitive error when the bytes are unavailable, not
/// decodable, or OCR cannot run. Engine diagnostics and source paths never
/// cross this boundary.
pub fn recognize_text(open: &OpenImage, language: &str) -> Result<ImageAnalysis, String> {
    if !valid_language(language) {
        return Err("OCR language must be a short ASCII language code".to_owned());
    }
    let bytes = doc::bytes_of(open).ok_or_else(|| {
        "This photograph is not available on this device yet; retry after it syncs".to_owned()
    })?;
    let rgba = pixels::decode(&bytes).ok_or_else(|| {
        "This photograph cannot be safely prepared for text recognition".to_owned()
    })?;
    let image = image::RgbaImage::from_raw(rgba.w, rgba.h, rgba.bytes)
        .ok_or_else(|| "This photograph has invalid decoded pixels".to_owned())?;
    let document = tilo_ocr::recognize(&image::DynamicImage::ImageRgba8(image), language)
        .map_err(|error| error.user_message().to_owned())?;
    let mut text = document.plain_text();
    truncate_chars(&mut text, MAX_OCR_CHARS);
    Ok(ImageAnalysis {
        ocr_text: text,
        provenance: AnalysisProvenance {
            source: "local-ocr".to_owned(),
            engine: "tilo-ocr".to_owned(),
            language: language.to_owned(),
            content: open.content.clone().unwrap_or_default(),
            // This return value is intentionally local. `SetImageAnalysis`
            // changes it to shared only at the collaborative consent boundary.
            scope: "local".to_owned(),
        },
        ..ImageAnalysis::default()
    })
}

fn valid_language(language: &str) -> bool {
    !language.is_empty()
        && language.len() <= 16
        && language
            .bytes()
            .all(|byte| byte.is_ascii_alphabetic() || byte == b'_')
}

fn truncate_chars(value: &mut String, max: usize) {
    if value.chars().count() > max {
        let end = value
            .char_indices()
            .nth(max)
            .map_or(value.len(), |(index, _)| index);
        value.truncate(end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_language_is_refused_before_reading_pixels() {
        let open = OpenImage::from_path("/not/read.png", 1, 1);
        assert!(recognize_text(&open, "eng; rm -rf").is_err());
    }

    #[test]
    fn truncation_keeps_unicode_boundaries() {
        let mut value = "ééé".to_owned();
        truncate_chars(&mut value, 2);
        assert_eq!(value, "éé");
    }
}
