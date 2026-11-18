//! The `print-centre.defaults` resource: how a person prints and scans unless
//! they choose otherwise for one document, as the app that draws it and an agent
//! that sets it both see it.

use tilo_resource::{self, Atom, Model, resource};

pub const SCAN_RESOLUTIONS: [u16; 3] = [150, 300, 600];
pub const SCAN_COLORS: [&str; 3] = ["color", "grayscale", "line-art"];
pub const SCAN_SOURCES: [&str; 3] = ["flatbed", "feeder", "feeder-duplex"];
/// Text recognition languages offered (ISO 639-2), with their names.
pub const OCR_LANGUAGES: [(&str, &str); 4] = [
    ("eng", "English"),
    ("deu", "German"),
    ("fra", "French"),
    ("spa", "Spanish"),
];

/// How a person prints and scans unless they choose otherwise for one document. Each
/// default is edited at its path; a value that is not offered is refused however it is set.
#[derive(Model, Clone, Debug, PartialEq)]
#[model(check = PrintDefaults::offered)]
pub struct PrintDefaults {
    /// Print on both sides of the paper.
    pub two_sided: bool,
    /// Scan resolution, one of [`SCAN_RESOLUTIONS`].
    pub scan_resolution: u16,
    /// Scan appearance, one of [`SCAN_COLORS`].
    pub scan_color: Atom,
    /// Paper source, one of [`SCAN_SOURCES`].
    pub scan_source: Atom,
    /// Language text is recognized in, one of [`OCR_LANGUAGES`].
    pub ocr_language: Atom,
}

impl Default for PrintDefaults {
    fn default() -> Self {
        Self {
            two_sided: false,
            scan_resolution: 300,
            scan_color: Atom(SCAN_COLORS[0].to_owned()),
            scan_source: Atom(SCAN_SOURCES[0].to_owned()),
            ocr_language: Atom(OCR_LANGUAGES[0].0.to_owned()),
        }
    }
}

impl PrintDefaults {
    /// What the resource holds: how the person prints and scans unless they choose otherwise
    /// for one document.
    ///
    /// # Errors
    /// When a field cannot be read from the document.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }

    /// Every default is one the scan and print controls offer.
    fn offered(&self) -> Result<(), &'static str> {
        if !SCAN_RESOLUTIONS.contains(&self.scan_resolution) {
            return Err("scan at 150, 300 or 600 dpi");
        }
        if !SCAN_COLORS.contains(&self.scan_color.0.as_str()) {
            return Err("scan in color, grayscale or line-art");
        }
        if !SCAN_SOURCES.contains(&self.scan_source.0.as_str()) {
            return Err("scan from the flatbed, feeder or feeder-duplex");
        }
        if !OCR_LANGUAGES
            .iter()
            .any(|(code, _)| *code == self.ocr_language.0)
        {
            return Err("recognize eng, deu, fra or spa");
        }
        Ok(())
    }

    /// The offered resolution's index, as the scan controls show it.
    #[must_use]
    pub fn resolution_index(&self) -> usize {
        SCAN_RESOLUTIONS
            .iter()
            .position(|dpi| *dpi == self.scan_resolution)
            .unwrap_or(1)
    }

    #[must_use]
    pub fn color_index(&self) -> usize {
        SCAN_COLORS
            .iter()
            .position(|color| *color == self.scan_color.0)
            .unwrap_or_default()
    }

    #[must_use]
    pub fn source_index(&self) -> usize {
        SCAN_SOURCES
            .iter()
            .position(|source| *source == self.scan_source.0)
            .unwrap_or_default()
    }

    #[must_use]
    pub fn language_index(&self) -> usize {
        OCR_LANGUAGES
            .iter()
            .position(|(code, _)| *code == self.ocr_language.0)
            .unwrap_or_default()
    }
}

#[resource(
    kind = "print-centre.defaults",
    read_content = PrintDefaults::read_document
)]
impl PrintDefaults {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_offered_and_indexed_as_the_controls_show_them() {
        let defaults = PrintDefaults::default();
        assert!(defaults.offered().is_ok());
        assert_eq!(
            (
                defaults.resolution_index(),
                defaults.color_index(),
                defaults.source_index(),
                defaults.language_index(),
            ),
            (1, 0, 0, 0)
        );
        let chosen = PrintDefaults {
            two_sided: true,
            scan_resolution: 600,
            scan_color: Atom("grayscale".into()),
            scan_source: Atom("feeder".into()),
            ocr_language: Atom("fra".into()),
        };
        assert!(chosen.offered().is_ok());
        assert_eq!(
            (
                chosen.resolution_index(),
                chosen.color_index(),
                chosen.source_index(),
                chosen.language_index(),
            ),
            (2, 1, 1, 2)
        );
    }

    #[test]
    fn a_default_no_control_offers_is_refused() {
        let too_fine = PrintDefaults {
            scan_resolution: 1200,
            ..PrintDefaults::default()
        };
        assert!(too_fine.offered().is_err());
        let unknown_language = PrintDefaults {
            ocr_language: Atom("klingon".into()),
            ..PrintDefaults::default()
        };
        assert!(unknown_language.offered().is_err());
    }
}
