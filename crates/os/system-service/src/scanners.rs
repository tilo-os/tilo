//! The `scanners` resource: what [`tilo_system_resources::scanners`] says it is, published with
//! the action that looks for the machine's scanners again and marked unavailable where this
//! computer has no scanner service.
//!
//! The machine side is SANE's own listing (`scanimage -L`), run with a bound on time and size:
//! it names the devices the drivers find. Scanning a page is the capture studio's, not this
//! service's.

use std::time::Duration;

use tilo_actions::ResourceSpec;
use tilo_service::{offer, unavailable};
use tilo_system_resources::scanners::{SCANNERS, ScanAction, ScannerShown, Scanners};

use crate::i18n::name;

/// How long SANE may take to list the scanners.
const LIST_TIMEOUT: Duration = Duration::from_secs(20);
/// The most a listing may be: a few scanners' names.
const MOST: usize = 256 * 1024;

/// The `scanners` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.scanners", label = "Scanners")]
pub struct ScannersReading;

/// The `scanners` resource reading `scanners`.
#[must_use]
pub fn resource(scanners: &Scanners) -> ResourceSpec {
    let mut resource = offer::<ScanAction>(tilo_resource::typed_resource::<ScannersReading>(
        SCANNERS,
        name("so.tilo.system.scanners", "Scanners"),
    ));
    if !scanners.available {
        unavailable(
            &mut resource,
            "find-scanners",
            "no_scanner",
            "This computer has no scanner service",
        );
    }
    resource
}

/// The scanners this machine reaches now.
#[must_use]
pub fn reading() -> Scanners {
    match list() {
        Ok(found) => Scanners {
            available: true,
            found,
            ..Scanners::default()
        },
        Err(error) => Scanners {
            available: false,
            problem: Some(error),
            ..Scanners::default()
        },
    }
}

/// What SANE lists, as the scanners it names.
fn list() -> Result<Vec<ScannerShown>, String> {
    let output = bounded_command::run("scanimage", &["-L"], None, MOST, LIST_TIMEOUT)
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        let why = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if why.is_empty() {
            format!("the scanner listing ended with {}", output.status)
        } else {
            why
        });
    }
    Ok(parse(&String::from_utf8_lossy(&output.stdout)))
}

/// The scanners `text` lists, one line each, as SANE writes them.
fn parse(text: &str) -> Vec<ScannerShown> {
    text.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<ScannerShown> {
    let rest = line.trim().strip_prefix("device `")?;
    let (device, rest) = rest.split_once('\'')?;
    let description = rest
        .trim()
        .strip_prefix("is a")
        .map_or_else(|| rest.trim(), str::trim);
    Some(ScannerShown {
        device: device.to_owned(),
        description: description.to_owned(),
    })
}

/// The `scanners` resource's surface, as a shell reads it over `so.tilo.device`.
#[must_use]
pub fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Scanners, ScanAction>(SCANNERS, "Scanners")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sane_listing_names_each_scanner() {
        let text = "device `epson2:libusb:001:004' is a Epson Perfection V39 flatbed scanner\n\
                    device `airscan:e0:Office' is a eSCL Office scanner\n";
        let found = parse(text);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].device, "epson2:libusb:001:004");
        assert_eq!(found[0].description, "Epson Perfection V39 flatbed scanner");
        assert_eq!(found[1].device, "airscan:e0:Office");
        assert_eq!(found[1].description, "eSCL Office scanner");
    }

    #[test]
    fn a_listing_with_no_scanner_names_none() {
        assert!(parse("No scanners were identified.\n").is_empty());
        assert!(parse("").is_empty());
    }
}
