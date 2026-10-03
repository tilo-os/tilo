//! Document scanners: one page at a time, as PNG.
//!
//! Linux reaches scanners through SANE (feature `scanner`); macOS and the
//! web have no scanner access here yet and report none. The controls are a
//! small typed set so no caller can pass scanner options through a string.

/// A scanner the system knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scanner {
    /// What [`scan_page`] takes.
    pub device: String,
    pub description: String,
}

/// How to scan one page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanSettings {
    pub resolution_dpi: u16,
    pub color: ScanColor,
    pub source: ScanSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanColor {
    Color,
    Grayscale,
    LineArt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanSource {
    Flatbed,
    Feeder,
    FeederDuplex,
}

/// The scanners attached now.
///
/// # Errors
///
/// The scanner service failed; unsupported targets answer an empty list.
pub fn scanners() -> Result<Vec<Scanner>, String> {
    imp::scanners()
}

/// Scan one page as PNG.
///
/// # Errors
///
/// An invalid device, a scanner that fails or times out, or a target without
/// scanner access.
pub fn scan_page(device: &str, settings: ScanSettings) -> Result<Vec<u8>, String> {
    imp::scan_page(device, settings)
}

#[cfg(sane_scanner)]
mod imp {
    use super::{ScanColor, ScanSettings, ScanSource, Scanner};
    use os_actions::printing as sane;

    pub(super) fn scanners() -> Result<Vec<Scanner>, String> {
        Ok(sane::scanners()?
            .into_iter()
            .map(|s| Scanner {
                device: s.device,
                description: s.description,
            })
            .collect())
    }

    pub(super) fn scan_page(device: &str, settings: ScanSettings) -> Result<Vec<u8>, String> {
        sane::scan_page(
            device,
            sane::ScanSettings {
                resolution_dpi: settings.resolution_dpi,
                color: match settings.color {
                    ScanColor::Color => sane::ScanColor::Color,
                    ScanColor::Grayscale => sane::ScanColor::Grayscale,
                    ScanColor::LineArt => sane::ScanColor::LineArt,
                },
                source: match settings.source {
                    ScanSource::Flatbed => sane::ScanSource::Flatbed,
                    ScanSource::Feeder => sane::ScanSource::Feeder,
                    ScanSource::FeederDuplex => sane::ScanSource::FeederDuplex,
                },
            },
        )
    }
}

#[cfg(not(sane_scanner))]
mod imp {
    use super::{ScanSettings, Scanner};

    #[allow(
        clippy::unnecessary_wraps,
        reason = "the same signature as the SANE backend's"
    )]
    pub(super) fn scanners() -> Result<Vec<Scanner>, String> {
        Ok(Vec::new())
    }

    pub(super) fn scan_page(_device: &str, _settings: ScanSettings) -> Result<Vec<u8>, String> {
        Err(crate::NO_SCANNER.into())
    }
}
