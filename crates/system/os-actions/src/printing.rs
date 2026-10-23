//! Typed CUPS integration shared by Settings and Print Centre.
//!
//! This is deliberately a narrow vocabulary.  Callers cannot pass command
//! fragments: queue names and URIs are validated and are always individual
//! argv entries.  CUPS remains the policy/driver authority; Tilo does not grow
//! a second printer database.

use crate::process::{run, run_capture};
use std::collections::{BTreeMap, BTreeSet};

const PRINTER_ATTRIBUTES_TEST: &str = "/usr/share/cups/ipptool/get-printer-attributes.test";
const JOBS_TEST: &str = "/usr/share/cups/ipptool/get-jobs.test";

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
    /// The printer this queue prints to, as CUPS reports it (`lpstat -v`).
    /// `None` when CUPS did not say.
    pub device_uri: Option<String>,
    /// Live standard IPP attributes. `available == false` means CUPS/the
    /// device could not be queried; `supplies == None` means it answered but
    /// did not implement marker attributes.
    pub telemetry: PrinterTelemetry,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrinterTelemetry {
    pub available: bool,
    pub state_reasons: Vec<String>,
    pub supplies: Option<Vec<PrinterSupply>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrinterSupply {
    pub name: String,
    pub color: Option<String>,
    pub level_percent: Option<u8>,
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
    /// Human-facing document name reported by IPP, sanitized and bounded.
    pub title: Option<String>,
    pub owner: String,
    pub size_bytes: u64,
    pub submitted: String,
    pub destination: String,
    pub state: Option<JobState>,
    pub state_reasons: Vec<String>,
    pub impressions_completed: Option<u32>,
    pub impressions_total: Option<u32>,
}

/// Finite document options accepted by the Print Centre. Callers cannot pass
/// arbitrary CUPS option strings through this type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrintSettings {
    pub copies: u16,
    pub two_sided: bool,
}

impl Default for PrintSettings {
    fn default() -> Self {
        Self {
            copies: 1,
            two_sided: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    Pending,
    Held,
    Processing,
    Stopped,
    Canceled,
    Aborted,
    Completed,
    Unknown,
}

impl PrintJob {
    pub fn progress_percent(&self) -> Option<u8> {
        let total = self.impressions_total.filter(|total| *total > 0)?;
        let completed = self.impressions_completed.unwrap_or(0).min(total);
        // `completed <= total`, so the percentage is always in `0..=100`.
        let percent = completed.saturating_mul(100).checked_div(total)?;
        Some(u8::try_from(percent).unwrap_or(100))
    }
}

/// List CUPS print queues, enriched with live IPP telemetry where a printer answers it.
pub fn queues() -> Result<Vec<PrinterQueue>, String> {
    let printers = run_capture("lpstat", &["-p", "-d"])?;
    let accepting = run_capture("lpstat", &["-a"]).unwrap_or_default();
    let devices = run_capture("lpstat", &["-v"]).unwrap_or_default();
    let mut queues = parse_queues(&printers, &accepting);
    let devices = parse_device_uris(&devices);
    for queue in &mut queues {
        queue.device_uri = devices.get(&queue.name).cloned();
    }
    for queue in &mut queues {
        if let Ok(output) = query_ipp(&queue.name, PRINTER_ATTRIBUTES_TEST) {
            queue.telemetry = parse_ipp_printer_attributes(&output);
        }
    }
    Ok(queues)
}

/// List CUPS print jobs, enriched with live IPP telemetry where a destination answers it.
pub fn jobs() -> Result<Vec<PrintJob>, String> {
    // Include completed/cancelled jobs retained by CUPS: Print Centre is a
    // recovery and audit surface, not merely a duplicate of the transient
    // portal dialog.
    let mut jobs = parse_jobs(&run_capture("lpstat", &["-W", "all", "-o"])?);
    let destinations: BTreeSet<String> = jobs.iter().map(|job| job.destination.clone()).collect();
    for destination in destinations {
        let Ok(output) = query_ipp(&destination, JOBS_TEST) else {
            continue;
        };
        let telemetry = parse_ipp_jobs(&output);
        for job in jobs.iter_mut().filter(|job| job.destination == destination) {
            let Some(number) = job
                .id
                .rsplit_once('-')
                .and_then(|(_, id)| id.parse::<u32>().ok())
            else {
                continue;
            };
            if let Some(details) = telemetry.get(&number) {
                job.title.clone_from(&details.title);
                job.state = details.state;
                job.state_reasons.clone_from(&details.state_reasons);
                job.impressions_completed = details.impressions_completed;
                job.impressions_total = details.impressions_total;
            }
        }
    }
    Ok(jobs)
}

fn query_ipp(queue: &str, test: &str) -> Result<String, String> {
    let queue = queue_name(queue)?;
    if !std::path::Path::new(test).is_file() {
        return Err("standard CUPS IPP query is unavailable".into());
    }
    let uri = format!("ipp://localhost/printers/{queue}");
    run_capture("ipptool", &["-tv", &uri, test])
}

/// Discover only driverless IPP devices.  `driverless` delegates DNS-SD and
/// IPP capability probing to cups-filters and therefore avoids a Tilo-specific
/// discovery stack or legacy PPD downloads.
pub fn discover_driverless() -> Result<Vec<DiscoveredPrinter>, String> {
    Ok(parse_driverless(&run_capture("driverless", &[])?))
}

/// Whether `printer` already backs one of `queues`: the same device,
/// whichever of its addresses (`ipp`/`ipps`, port, trailing slash) the queue
/// was added with.
pub fn is_added(printer: &DiscoveredPrinter, queues: &[PrinterQueue]) -> bool {
    let wanted = device_identity(&printer.uri);
    queues
        .iter()
        .filter_map(|queue| queue.device_uri.as_deref())
        .any(|uri| device_identity(uri) == wanted)
}

/// The printers in `found` that no queue in `queues` prints to yet: the ones
/// that can still be added.
pub fn not_added(found: &[DiscoveredPrinter], queues: &[PrinterQueue]) -> Vec<DiscoveredPrinter> {
    found
        .iter()
        .filter(|printer| !is_added(printer, queues))
        .cloned()
        .collect()
}

/// What tells two printer URIs apart as devices. A printer advertised over
/// DNS-SD is the same device as `ipp://NAME._ipp._tcp.local/` and
/// `ipps://NAME._ipps._tcp.local/`; a printer at an address is its host and
/// resource path, whatever the scheme and port.
fn device_identity(uri: &str) -> String {
    let rest = uri.split_once("://").map_or(uri, |(_, rest)| rest);
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match host.strip_prefix('[') {
        // An IPv6 literal: `[addr]` or `[addr]:port`.
        Some(v6) => v6.split_once(']').map_or(v6, |(addr, _)| addr),
        None => host.split_once(':').map_or(host, |(name, _)| name),
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let host = ["._ipps._tcp.local", "._ipp._tcp.local"]
        .iter()
        .find_map(|suffix| host.strip_suffix(suffix))
        .unwrap_or(&host)
        .to_owned();
    let path = path.split(['?', '#']).next().unwrap_or("");
    format!("{host}/{}", path.trim_end_matches('/'))
}

/// Add a driverless IPP printer at `uri`, naming the queue `requested_name`.
pub fn add_driverless(uri: &str, requested_name: &str) -> Result<(), String> {
    validate_uri(uri)?;
    let name = queue_name(requested_name)?;
    // One queue per printer: CUPS's own list decides, so a second window,
    // a stale search or another app can't add the same printer twice.
    let devices = parse_device_uris(&run_capture("lpstat", &["-v"]).unwrap_or_default());
    let wanted = device_identity(uri);
    if let Some((queue, _)) = devices
        .iter()
        .find(|(_, device)| device_identity(device) == wanted)
    {
        return Err(format!("this printer is already added as {queue}"));
    }
    run(
        "lpadmin",
        &["-p", &name, "-E", "-v", uri, "-m", "everywhere"],
    )
}

/// Remove the print queue `name`.
pub fn remove_queue(name: &str) -> Result<(), String> {
    let name = queue_name(name)?;
    run("lpadmin", &["-x", &name])
}

/// Make `name` the default print queue.
pub fn set_default(name: &str) -> Result<(), String> {
    let name = queue_name(name)?;
    run("lpoptions", &["-d", &name])
}

/// Enable/accept or disable/reject jobs on the print queue `name`.
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

/// Cancel the CUPS job `id`.
pub fn cancel_job(id: &str) -> Result<(), String> {
    validate_job_id(id)?;
    run("cancel", &[id])
}

/// Restart the CUPS job `id`.
pub fn retry_job(id: &str) -> Result<(), String> {
    validate_job_id(id)?;
    run("lp", &["-i", id, "-H", "restart"])
}

/// Submit one portal-authorized local document to a named queue.
///
/// # Errors
///
/// Returns an error if the queue, file, or bounded settings are invalid, or
/// if CUPS cannot accept the document.
pub fn print_document(
    queue: &str,
    path: &std::path::Path,
    settings: PrintSettings,
) -> Result<(), String> {
    let queue = queue_name(queue)?;
    if !(1..=99).contains(&settings.copies) {
        return Err("copies must be between 1 and 99".into());
    }
    let metadata = std::fs::metadata(path).map_err(|_| "document is no longer available")?;
    if !metadata.is_file() {
        return Err("selected document is not a regular file".into());
    }
    if metadata.len() > 2 * 1024 * 1024 * 1024 {
        return Err("selected document is too large to print".into());
    }
    let path = path
        .to_str()
        .ok_or_else(|| "selected document name is not supported".to_string())?;
    let copies = settings.copies.to_string();
    let mut args = vec!["-d", queue.as_str(), "-n", copies.as_str()];
    if settings.two_sided {
        args.extend(["-o", "sides=two-sided-long-edge"]);
    }
    args.extend(["--", path]);
    run("lp", &args)
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
        || !matches!(uri.split_once(':'), Some(("ipp" | "ipps", rest)) if rest.starts_with("//"))
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
            let name = queue_name(name).ok()?;
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
                .map(|(_, r)| sanitize_text(r.trim().trim_end_matches('.'), 120))
                .filter(|r| !r.is_empty());
            Some(PrinterQueue {
                name: name.clone(),
                state,
                default: default == Some(name.as_str()),
                accepting_jobs: accepting.get(name.as_str()).copied().unwrap_or(true),
                reason,
                device_uri: None,
                telemetry: PrinterTelemetry::default(),
            })
        })
        .collect()
}

/// Each queue's device URI from `lpstat -v` (`device for NAME: URI`).
pub fn parse_device_uris(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("device for ")?;
            let (name, uri) = rest.split_once(':')?;
            let name = queue_name(name).ok()?;
            let uri = uri.trim();
            (!uri.is_empty() && uri.len() <= 2048).then(|| (name, uri.to_owned()))
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
                title: None,
                owner: sanitize_text(owner, 80),
                size_bytes: size,
                submitted: sanitize_text(&p.collect::<Vec<_>>().join(" "), 120),
                state: None,
                state_reasons: Vec::new(),
                impressions_completed: None,
                impressions_total: None,
            })
        })
        .collect()
}

fn sanitize_text(raw: &str, limit: usize) -> String {
    raw.chars()
        .filter(|character| !character.is_control())
        .take(limit)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn attribute<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let line = line.trim();
    let prefix = line.split_once(' ').map_or(line, |(head, _)| head);
    if prefix != name {
        return None;
    }
    line.split_once(" = ").map(|(_, value)| value.trim())
}

fn values(raw: &str, limit: usize, each: usize) -> Vec<String> {
    raw.split(',')
        .map(|value| sanitize_text(value.trim().trim_matches('"'), each))
        .filter(|value| !value.is_empty())
        .take(limit)
        .collect()
}

pub fn parse_ipp_printer_attributes(text: &str) -> PrinterTelemetry {
    let mut telemetry = PrinterTelemetry {
        available: true,
        ..Default::default()
    };
    let mut names = None;
    let mut colors = Vec::new();
    let mut levels = Vec::new();
    for line in text.lines() {
        if let Some(value) = attribute(line, "printer-state-reasons") {
            telemetry.state_reasons = values(value, 12, 96)
                .into_iter()
                .filter(|reason| reason != "none")
                .collect();
        } else if let Some(value) = attribute(line, "marker-names") {
            names = Some(values(value, 16, 64));
        } else if let Some(value) = attribute(line, "marker-colors") {
            colors = values(value, 16, 32);
        } else if let Some(value) = attribute(line, "marker-levels") {
            levels = value
                .split(',')
                .take(16)
                .map(|level| {
                    level
                        .trim()
                        .parse::<i16>()
                        .ok()
                        .and_then(|level| u8::try_from(level).ok())
                        .filter(|level| *level <= 100)
                })
                .collect();
        }
    }
    telemetry.supplies = names.map(|names| {
        names
            .into_iter()
            .enumerate()
            .map(|(index, name)| PrinterSupply {
                name,
                color: colors.get(index).cloned(),
                level_percent: levels.get(index).copied().flatten(),
            })
            .collect()
    });
    telemetry
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IppJobTelemetry {
    pub title: Option<String>,
    pub state: Option<JobState>,
    pub state_reasons: Vec<String>,
    pub impressions_completed: Option<u32>,
    pub impressions_total: Option<u32>,
}

pub fn parse_ipp_jobs(text: &str) -> BTreeMap<u32, IppJobTelemetry> {
    let mut jobs = BTreeMap::new();
    let mut current = None;
    for line in text.lines() {
        if let Some(value) = attribute(line, "job-id").and_then(|value| value.parse::<u32>().ok()) {
            current = Some(value);
            jobs.entry(value).or_insert_with(IppJobTelemetry::default);
            continue;
        }
        let Some(job) = current.and_then(|id| jobs.get_mut(&id)) else {
            continue;
        };
        if let Some(value) = attribute(line, "job-name") {
            let title = sanitize_text(value.trim_matches('"'), 160);
            job.title = (!title.is_empty()).then_some(title);
        } else if let Some(value) = attribute(line, "job-state") {
            let value = value
                .split_whitespace()
                .next()
                .unwrap_or(value)
                .trim_matches('"')
                .to_ascii_lowercase();
            job.state = Some(match value.as_str() {
                "pending" | "3" => JobState::Pending,
                "pending-held" | "held" | "4" => JobState::Held,
                "processing" | "5" => JobState::Processing,
                "processing-stopped" | "stopped" | "6" => JobState::Stopped,
                "canceled" | "7" => JobState::Canceled,
                "aborted" | "8" => JobState::Aborted,
                "completed" | "9" => JobState::Completed,
                _ => JobState::Unknown,
            });
        } else if let Some(value) = attribute(line, "job-state-reasons") {
            job.state_reasons = values(value, 12, 96)
                .into_iter()
                .filter(|reason| reason != "none")
                .collect();
        } else if let Some(value) = attribute(line, "job-impressions-completed") {
            job.impressions_completed = value.parse().ok();
        } else if let Some(value) = attribute(line, "job-impressions") {
            job.impressions_total = value.parse().ok();
        }
    }
    jobs
}

pub fn parse_driverless(text: &str) -> Vec<DiscoveredPrinter> {
    text.lines()
        .filter_map(|line| {
            // cups-filters prints: URI "make model" "location"
            let line = line.trim();
            let split = line.find(char::is_whitespace)?;
            let uri = line.get(..split)?.trim();
            validate_uri(uri).ok()?;
            let quoted: Vec<&str> = line.get(split..)?.split('"').collect();
            let name = quoted
                .get(1)
                .map(|s| sanitize_text(s, 100))
                .filter(|s| !s.is_empty())?;
            let location = quoted
                .get(3)
                .map(|s| sanitize_text(s, 100))
                .filter(|s| !s.is_empty());
            Some(DiscoveredPrinter {
                uri: uri.to_string(),
                name,
                location,
            })
        })
        .fold(Vec::new(), |mut found: Vec<DiscoveredPrinter>, printer| {
            // A printer advertising both `ipp` and `ipps` is one printer,
            // reached over `ipps` when it offers it.
            let identity = device_identity(&printer.uri);
            match found
                .iter_mut()
                .find(|seen| device_identity(&seen.uri) == identity)
            {
                Some(seen) if printer.uri.starts_with("ipps:") => *seen = printer,
                Some(_) => {}
                None => found.push(printer),
            }
            found
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cups_queues_devices_supplies_and_identifiers_parse_bounded() {
        {
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
        {
            let d =
                parse_driverless("ipps://office.local/ipp/print \"Acme Laser 4\" \"Copy room\"\n");
            assert_eq!(d[0].name, "Acme Laser 4");
            assert_eq!(d[0].location.as_deref(), Some("Copy room"));
        }
        {
            let d = parse_driverless(
                "ipp://Acme%20Laser._ipp._tcp.local/ \"Acme Laser 4\" \"\"\nipps://Acme%20Laser._ipps._tcp.local/ \"Acme Laser 4\" \"\"\n",
            );
            assert_eq!(d.len(), 1);
            assert!(d[0].uri.starts_with("ipps://"));
        }
        {
            assert!(validate_uri("ipps://printer.local/ipp/print").is_ok());
            assert!(validate_uri("file:///etc/shadow").is_err());
            assert!(validate_job_id("Office-42").is_ok());
            assert!(validate_job_id("Office;shutdown-42").is_err());
            assert_eq!(queue_name("Office Laser").unwrap(), "Office_Laser");
            assert!(
                print_document(
                    "Office",
                    std::path::Path::new("/does/not/matter"),
                    PrintSettings {
                        copies: 0,
                        two_sided: false,
                    },
                )
                .is_err()
            );
        }
        {
            let telemetry = parse_ipp_printer_attributes(
                " marker-names (1setOf nameWithoutLanguage) = \"Black Toner\",\"Cyan\u{1b} Toner\"\n marker-colors (1setOf nameWithoutLanguage) = black,cyan\n marker-levels (1setOf integer) = 83,-1\n printer-state-reasons (1setOf keyword) = none,media-low\n",
            );
            assert!(telemetry.available);
            let supplies = telemetry.supplies.unwrap();
            assert_eq!(supplies[0].level_percent, Some(83));
            assert_eq!(supplies[1].level_percent, None);
            assert!(!supplies[1].name.contains('\u{1b}'));
            assert_eq!(telemetry.state_reasons, ["media-low"]);

            let jobs = parse_ipp_jobs(
                " job-id (integer) = 42\n job-name (nameWithoutLanguage) = \"Travel itinerary.pdf\"\n job-state (enum) = processing\n job-state-reasons (1setOf keyword) = job-printing\n job-impressions-completed (integer) = 3\n job-impressions (integer) = 12\n",
            );
            assert_eq!(jobs[&42].title.as_deref(), Some("Travel itinerary.pdf"));
            assert_eq!(jobs[&42].state, Some(JobState::Processing));
            assert_eq!(jobs[&42].impressions_completed, Some(3));
            assert_eq!(jobs[&42].impressions_total, Some(12));
        }
    }

    #[test]
    fn a_printer_already_added_is_not_offered_again_whichever_address_it_was_added_by() {
        let devices = parse_device_uris(
            "device for Office: ipps://Acme%20Laser._ipps._tcp.local/\ndevice for Lab: ipp://192.168.1.20:631/ipp/print\n",
        );
        let queue = |name: &str| PrinterQueue {
            name: name.into(),
            state: QueueState::Ready,
            default: false,
            accepting_jobs: true,
            reason: None,
            device_uri: devices.get(name).cloned(),
            telemetry: PrinterTelemetry::default(),
        };
        let queues = [queue("Office"), queue("Lab")];
        let found = |uri: &str| DiscoveredPrinter {
            uri: uri.into(),
            name: "Printer".into(),
            location: None,
        };
        assert!(is_added(
            &found("ipp://Acme%20Laser._ipp._tcp.local/"),
            &queues
        ));
        assert!(is_added(&found("ipps://192.168.1.20/ipp/print/"), &queues));
        let other = found("ipps://Other%20Inkjet._ipps._tcp.local/");
        assert!(!is_added(&other, &queues));
        let offered = not_added(
            &[
                found("ipps://Acme%20Laser._ipps._tcp.local/"),
                other.clone(),
            ],
            &queues,
        );
        assert_eq!(offered, vec![other]);
    }
}
