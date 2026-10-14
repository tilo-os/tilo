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

/// SANE through `scanimage`. The device is one argv value and every option
/// comes from the typed set, so no caller reaches a free-form argument.
#[cfg(sane_scanner)]
mod imp {
    use std::process::Command;
    use std::time::Duration;

    use super::{ScanColor, ScanSettings, ScanSource, Scanner};

    const MAX_LIST_BYTES: usize = 1024 * 1024;
    const LIST_TIMEOUT: Duration = Duration::from_millis(2_500);
    const MAX_SCAN_BYTES: usize = 128 * 1024 * 1024;
    const MAX_SCAN_DIAGNOSTIC_BYTES: usize = 1024 * 1024;
    const SCAN_TIMEOUT: Duration = Duration::from_secs(10 * 60);

    pub(super) fn scanners() -> Result<Vec<Scanner>, String> {
        let output = bounded_command::run("scanimage", &["-L"], None, MAX_LIST_BYTES, LIST_TIMEOUT)
            .map_err(|error| format!("scanimage failed: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "scanimage exited with status {:?}: {}",
                output.status.code(),
                output.stderr_diagnostic()
            ));
        }
        Ok(parse_scanners(&String::from_utf8_lossy(&output.stdout)))
    }

    pub(super) fn scan_page(device: &str, settings: ScanSettings) -> Result<Vec<u8>, String> {
        let output = bounded_command::run_prepared_with_limits(
            scan_command(device, settings)?,
            None,
            MAX_SCAN_BYTES,
            MAX_SCAN_DIAGNOSTIC_BYTES,
            SCAN_TIMEOUT,
        )
        .map_err(|error| match error {
            bounded_command::Error::OutputTooLarge {
                stream: "stdout", ..
            } => "scanner output exceeded the safety limit".to_string(),
            bounded_command::Error::OutputTooLarge {
                stream: "stderr", ..
            } => "scanner diagnostics exceeded the safety limit".to_string(),
            bounded_command::Error::Timeout(_) => "scanner timed out".to_string(),
            _ => format!("scanner process failed: {error}"),
        })?;
        if !output.status.success() {
            let diagnostic = sanitize(&String::from_utf8_lossy(&output.stderr), 512);
            return Err(if diagnostic.is_empty() {
                "scanner failed".into()
            } else {
                format!("scanner failed: {diagnostic}")
            });
        }
        if output.stdout.is_empty() {
            return Err("scanner returned an empty page".into());
        }
        Ok(output.stdout)
    }

    fn scan_command(device: &str, settings: ScanSettings) -> Result<Command, String> {
        if device.is_empty()
            || device.len() > 1024
            || device.bytes().any(|byte| matches!(byte, 0 | b'\n' | b'\r'))
        {
            return Err("invalid scanner device identity".into());
        }
        if !matches!(
            settings.resolution_dpi,
            75 | 100 | 150 | 200 | 300 | 400 | 600
        ) {
            return Err("unsupported scan resolution".into());
        }
        let mode = match settings.color {
            ScanColor::Color => "Color",
            ScanColor::Grayscale => "Gray",
            ScanColor::LineArt => "Lineart",
        };
        let source = match settings.source {
            ScanSource::Flatbed => "Flatbed",
            ScanSource::Feeder => "ADF",
            ScanSource::FeederDuplex => "ADF Duplex",
        };
        let mut command = Command::new("scanimage");
        command
            .arg("--device-name")
            .arg(device)
            .arg("--format=png")
            .arg("--resolution")
            .arg(settings.resolution_dpi.to_string())
            .arg("--mode")
            .arg(mode)
            .arg("--source")
            .arg(source);
        Ok(command)
    }

    /// The scanners in `scanimage -L` output.
    fn parse_scanners(text: &str) -> Vec<Scanner> {
        text.lines()
            .filter_map(|line| {
                let rest = line.trim().strip_prefix("device `")?;
                let (device, description) = rest.split_once("' is a ")?;
                Some(Scanner {
                    device: sanitize(device, 160),
                    description: sanitize(description, 120),
                })
            })
            .collect()
    }

    /// `raw` without control characters, at most `limit` characters, its
    /// whitespace collapsed.
    fn sanitize(raw: &str, limit: usize) -> String {
        raw.chars()
            .filter(|character| !character.is_control())
            .take(limit)
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn scanners_parse_and_scan_options_stay_typed() {
            let found = parse_scanners("device `airscan:e0:Office' is a Acme network scanner\n");
            assert_eq!(found[0].device, "airscan:e0:Office");
            let invalid = ScanSettings {
                resolution_dpi: 301,
                color: ScanColor::Color,
                source: ScanSource::Flatbed,
            };
            assert!(scan_command("scanner", invalid).is_err());
            let valid = ScanSettings {
                resolution_dpi: 300,
                ..invalid
            };
            assert!(scan_command("scanner\n--batch", valid).is_err());
            assert!(scan_command("scanner", valid).is_ok());
        }
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
