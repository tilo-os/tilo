//! Typed CUPS/SANE integration shared by Settings and a future Print Centre.
//!
//! This is deliberately a narrow vocabulary.  Callers cannot pass command
//! fragments: queue names and URIs are validated and are always individual
//! argv entries.  CUPS remains the policy/driver authority; qOS does not grow
//! a second printer database.

use crate::process::{run, run_capture};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueState {
    Ready,
    Printing,
    Paused,
    Offline,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrinterQueue {
    pub name: String,
    pub state: QueueState,
    pub default: bool,
    pub accepting_jobs: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredPrinter {
    pub uri: String,
    pub name: String,
    pub location: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintJob {
    pub id: String,
    pub owner: String,
    pub size_bytes: u64,
    pub submitted: String,
    pub destination: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scanner {
    pub device: String,
    pub description: String,
}

pub fn queues() -> Result<Vec<PrinterQueue>, String> {
    let printers = run_capture("lpstat", &["-p", "-d"])?;
    let accepting = run_capture("lpstat", &["-a"]).unwrap_or_default();
    Ok(parse_queues(&printers, &accepting))
}

pub fn jobs() -> Result<Vec<PrintJob>, String> {
    Ok(parse_jobs(&run_capture("lpstat", &["-o"])?))
}

/// Discover only driverless IPP devices.  `driverless` delegates DNS-SD and
/// IPP capability probing to cups-filters and therefore avoids a qOS-specific
/// discovery stack or legacy PPD downloads.
pub fn discover_driverless() -> Result<Vec<DiscoveredPrinter>, String> {
    Ok(parse_driverless(&run_capture("driverless", &[])?))
}

pub fn add_driverless(uri: &str, requested_name: &str) -> Result<(), String> {
    validate_uri(uri)?;
    let name = queue_name(requested_name)?;
    run(
        "lpadmin",
        &["-p", &name, "-E", "-v", uri, "-m", "everywhere"],
    )
}

pub fn remove_queue(name: &str) -> Result<(), String> {
    let name = queue_name(name)?;
    run("lpadmin", &["-x", &name])
}

pub fn set_default(name: &str) -> Result<(), String> {
    let name = queue_name(name)?;
    run("lpoptions", &["-d", &name])
}

pub fn set_enabled(name: &str, enabled: bool) -> Result<(), String> {
    let name = queue_name(name)?;
    if enabled {
        run("cupsenable", &[&name])?;
        run("cupsaccept", &[&name])
    } else {
        // Stop accepting before disabling so a paused queue does not keep
        // accumulating work users believe will print immediately.
        run("cupsreject", &[&name])?;
        run("cupsdisable", &[&name])
    }
}

pub fn cancel_job(id: &str) -> Result<(), String> {
    validate_job_id(id)?;
    run("cancel", &[id])
}

pub fn retry_job(id: &str) -> Result<(), String> {
    validate_job_id(id)?;
    run("lp", &["-i", id, "-H", "restart"])
}

pub fn scanners() -> Result<Vec<Scanner>, String> {
    Ok(parse_scanners(&run_capture("scanimage", &["-L"])?))
}

fn queue_name(raw: &str) -> Result<String, String> {
    let value = raw.trim().replace(' ', "_");
    if value.is_empty()
        || value.len() > 127
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        return Err("printer name may contain letters, numbers, '.', '-' and '_'".into());
    }
    Ok(value)
}

fn validate_uri(uri: &str) -> Result<(), String> {
    if uri.len() > 2048
        || uri.bytes().any(|b| b == 0 || b == b'\r' || b == b'\n')
        || !matches!(uri.split_once(":"), Some(("ipp" | "ipps", rest)) if rest.starts_with("//"))
    {
        return Err("only a valid ipp:// or ipps:// printer URI is accepted".into());
    }
    Ok(())
}

fn validate_job_id(id: &str) -> Result<(), String> {
    if id.len() > 255
        || !id.rsplit_once('-').is_some_and(|(q, n)| {
            queue_name(q).is_ok() && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
        })
    {
        return Err("invalid CUPS job id".into());
    }
    Ok(())
}

pub fn parse_queues(printer_text: &str, accepting_text: &str) -> Vec<PrinterQueue> {
    let default = printer_text
        .lines()
        .find_map(|line| line.trim().strip_prefix("system default destination:"))
        .map(str::trim);
    let accepting: std::collections::HashMap<&str, bool> = accepting_text
        .lines()
        .filter_map(|line| {
            let (name, tail) = line.trim().split_once(' ')?;
            Some((name, tail.starts_with("accepting requests")))
        })
        .collect();
    printer_text
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("printer ")?;
            let (name, tail) = rest.split_once(" is ")?;
            let lower = tail.to_ascii_lowercase();
            let state = if lower.contains("printing") {
                QueueState::Printing
            } else if lower.contains("disabled") || lower.contains("stopped") {
                QueueState::Paused
            } else if lower.contains("idle") {
                QueueState::Ready
            } else {
                QueueState::Offline
            };
            let reason = tail
                .split_once(" - ")
                .map(|(_, r)| r.trim().trim_end_matches('.').to_string())
                .filter(|r| !r.is_empty());
            Some(PrinterQueue {
                name: name.trim().to_string(),
                state,
                default: default == Some(name.trim()),
                accepting_jobs: accepting.get(name.trim()).copied().unwrap_or(true),
                reason,
            })
        })
        .collect()
}

pub fn parse_jobs(text: &str) -> Vec<PrintJob> {
    text.lines()
        .filter_map(|line| {
            let mut p = line.split_whitespace();
            let id = p.next()?;
            let owner = p.next()?;
            let size = p.next()?.parse().ok()?;
            validate_job_id(id).ok()?;
            Some(PrintJob {
                destination: id.rsplit_once('-')?.0.to_string(),
                id: id.to_string(),
                owner: owner.to_string(),
                size_bytes: size,
                submitted: p.collect::<Vec<_>>().join(" "),
            })
        })
        .collect()
}

pub fn parse_driverless(text: &str) -> Vec<DiscoveredPrinter> {
    text.lines()
        .filter_map(|line| {
            // cups-filters prints: URI "make model" "location"
            let line = line.trim();
            let split = line.find(char::is_whitespace)?;
            let uri = line[..split].trim();
            validate_uri(uri).ok()?;
            let quoted: Vec<&str> = line[split..].split('"').collect();
            let name = quoted.get(1).map(|s| s.trim()).filter(|s| !s.is_empty())?;
            let location = quoted
                .get(3)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            Some(DiscoveredPrinter {
                uri: uri.to_string(),
                name: name.to_string(),
                location,
            })
        })
        .collect()
}

pub fn parse_scanners(text: &str) -> Vec<Scanner> {
    text.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("device `")?;
            let (device, description) = rest.split_once("' is a ")?;
            Some(Scanner {
                device: device.to_string(),
                description: description.trim().to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_queue_health_and_accepting_state() {
        let q = parse_queues(
            "printer Office is printing Office-8. enabled since now\nprinter Lab is disabled since now - media-empty\nsystem default destination: Office\n",
            "Office accepting requests since now\nLab not accepting requests since now\n",
        );
        assert_eq!(q.len(), 2);
        assert!(q[0].default && q[0].accepting_jobs);
        assert_eq!(q[1].state, QueueState::Paused);
        assert_eq!(q[1].reason.as_deref(), Some("media-empty"));
        assert!(!q[1].accepting_jobs);
    }

    #[test]
    fn parses_driverless_and_scanners() {
        let d = parse_driverless("ipps://office.local/ipp/print \"Acme Laser 4\" \"Copy room\"\n");
        assert_eq!(d[0].name, "Acme Laser 4");
        assert_eq!(d[0].location.as_deref(), Some("Copy room"));
        let s = parse_scanners("device `airscan:e0:Office' is a Acme network scanner\n");
        assert_eq!(s[0].device, "airscan:e0:Office");
    }

    #[test]
    fn validates_all_external_identifiers() {
        assert!(validate_uri("ipps://printer.local/ipp/print").is_ok());
        assert!(validate_uri("file:///etc/shadow").is_err());
        assert!(validate_job_id("Office-42").is_ok());
        assert!(validate_job_id("Office;shutdown-42").is_err());
        assert_eq!(queue_name("Office Laser").unwrap(), "Office_Laser");
    }
}
