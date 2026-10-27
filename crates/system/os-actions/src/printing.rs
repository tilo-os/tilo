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

/// One value of an option the driver reports, as `lpoptions` prints it: the
/// key CUPS takes, the label the form shows, and whether it is the printer's
/// default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptionChoice {
    pub key: String,
    pub label: String,
    pub default: bool,
}

/// What a printer's driver says it can do, from `lpoptions -l -p <queue>`.
///
/// The print form draws a control only for an option that appears here: a
/// field the driver did not report is a field the printer may silently
/// ignore, which is how a person comes to print forty pages on A4 when they
/// asked for A5.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrinterOptions {
    /// Paper sizes the printer accepts (`PageSize`, `media`).
    pub page_sizes: Vec<OptionChoice>,
    /// Colour modes (`ColorModel`, `print-color-mode`).
    pub colour_modes: Vec<OptionChoice>,
    /// Two-sided modes (`Duplex`, `sides`).
    pub duplex_modes: Vec<OptionChoice>,
    /// Page orientations (`Orientation`, `orientation-requested`).
    pub orientations: Vec<OptionChoice>,
}

impl PrinterOptions {
    /// The choice `key` names, if the driver reported it.
    pub fn choice<'a>(choices: &'a [OptionChoice], key: &str) -> Option<&'a OptionChoice> {
        choices.iter().find(|choice| choice.key == key)
    }

    /// The default of `choices`, if the driver marked one.
    pub fn default_key(choices: &[OptionChoice]) -> Option<&str> {
        choices
            .iter()
            .find(|choice| choice.default)
            .map(|choice| choice.key.as_str())
    }
}

/// Every option `queue`'s driver reports, with the values its own list holds.
///
/// # Errors
///
/// Returns an error when the queue name is invalid or CUPS cannot answer for
/// it. An empty [`PrinterOptions`] is a printer that reported none of them,
/// which the form shows by drawing none of those fields.
pub fn printer_options(queue: &str) -> Result<PrinterOptions, String> {
    let queue = queue_name(queue)?;
    Ok(parse_lpoptions(&run_capture(
        "lpoptions",
        &["-p", queue.as_str(), "-l"],
    )?))
}

/// `lpoptions` text into the options the form can offer. Each line is
/// `Key/Label: *default other other`; the `*` marks the default. Only the
/// four options the form asks about are kept, and only their own values.
fn parse_lpoptions(text: &str) -> PrinterOptions {
    let mut options = PrinterOptions::default();
    for line in text.lines() {
        let Some((name, values)) = line.split_once(':') else {
            continue;
        };
        let (key, label) = name.split_once('/').unwrap_or((name, name));
        let choices: Vec<OptionChoice> = values
            .split_whitespace()
            .filter(|value| !value.is_empty())
            .map(|value| {
                let (default, value) = match value.strip_prefix('*') {
                    Some(rest) => (true, rest),
                    None => (false, value),
                };
                OptionChoice {
                    key: value.to_owned(),
                    label: option_value_label(value),
                    default,
                }
            })
            .collect();
        if choices.is_empty() {
            continue;
        }
        match key.to_ascii_lowercase().as_str() {
            "pagesize" | "media" => options.page_sizes = choices,
            "colormodel" | "print-color-mode" => options.colour_modes = choices,
            "duplex" | "sides" => options.duplex_modes = choices,
            "orientation" | "orientation-requested" => options.orientations = choices,
            _ => {}
        }
        let _ = label;
    }
    options
}

/// A value as the form says it: CUPS's own keys are run together
/// (`DuplexNoTumble`, `3`), and a person reads "Two-sided, long edge" or
/// "Landscape".
fn option_value_label(value: &str) -> String {
    match value {
        "DuplexNoTumble" | "two-sided-long-edge" => "Two-sided, long edge".to_owned(),
        "DuplexTumble" | "two-sided-short-edge" => "Two-sided, short edge".to_owned(),
        "None" | "one-sided" => "One side".to_owned(),
        "Color" | "color" => "Colour".to_owned(),
        "Gray" | "monochrome" => "Black and white".to_owned(),
        "3" => "Portrait".to_owned(),
        "4" => "Landscape".to_owned(),
        "5" => "Portrait, upside down".to_owned(),
        "6" => "Landscape, upside down".to_owned(),
        other => other.to_owned(),
    }
}

/// A page range as `lp` takes it: `1-3`, `1,4,9`, `2-`. Pages are positive,
/// a range never runs backwards, and nothing else is allowed through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageRange(String);

impl PageRange {
    /// `text` as a page range that `lp` will read.
    ///
    /// # Errors
    ///
    /// Returns an error naming the rule that was broken: an empty range, a
    /// zero page, a backwards span, or a character that is not a digit, a
    /// dash or a comma.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("enter the pages to print, such as 1-3".into());
        }
        if text.len() > 64 {
            return Err("that page range is too long to be one job".into());
        }
        for part in text.split(',') {
            let part = part.trim();
            if part.is_empty() {
                return Err("a page range cannot have an empty part".into());
            }
            let (first, last) = match part.split_once('-') {
                Some((first, last)) => (first, Some(last)),
                None => (part, None),
            };
            let first = page_number(first)?;
            if let Some(last) = last
                && !last.is_empty()
            {
                let last = page_number(last)?;
                if last < first {
                    return Err("a page range cannot end before it starts".into());
                }
            }
        }
        Ok(Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn page_number(text: &str) -> Result<u32, String> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err("page numbers are written with digits, as in 1-3".into());
    }
    match text.parse::<u32>() {
        Ok(0) | Err(_) => Err("pages are numbered from one".into()),
        Ok(number) => Ok(number),
    }
}

/// Finite document options accepted by the Print Centre. Callers cannot pass
/// arbitrary CUPS option strings through this type: every option here is one
/// the printer's driver reported, and [`print_document`] checks it against
/// that report before CUPS sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintSettings {
    pub copies: u16,
    pub two_sided: bool,
    /// The pages to print: `None` is the whole document.
    pub page_range: Option<PageRange>,
    /// `PageSize`/`media` value, as the driver reported it.
    pub page_size: Option<String>,
    /// `ColorModel`/`print-color-mode` value.
    pub colour: Option<String>,
    /// `Duplex`/`sides` value; the plain boolean above stays for a call that
    /// only knows two-sided or not.
    pub duplex: Option<String>,
    /// `Orientation`/`orientation-requested` value.
    pub orientation: Option<String>,
}

impl Default for PrintSettings {
    fn default() -> Self {
        Self {
            copies: 1,
            two_sided: false,
            page_range: None,
            page_size: None,
            colour: None,
            duplex: None,
            orientation: None,
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
    settings: &PrintSettings,
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
    let options = print_options(&queue, settings)?;
    let copies = settings.copies.to_string();
    let mut args: Vec<String> = vec!["-d".into(), queue, "-n".into(), copies];
    for (key, value) in &options {
        args.push("-o".into());
        args.push(format!("{key}={value}"));
    }
    args.push("--".into());
    args.push(path.to_owned());
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    run("lp", &borrowed)
}

/// The `-o key=value` pairs `settings` asks of `queue`, each one a value the
/// driver reported: an option the printer never listed is refused here rather
/// than silently dropped by CUPS after the person chose it.
fn print_options(queue: &str, settings: &PrintSettings) -> Result<Vec<(String, String)>, String> {
    let mut options: Vec<(String, String)> = Vec::new();
    if let Some(range) = &settings.page_range {
        options.push(("page-ranges".into(), range.as_str().to_owned()));
    }
    if settings.two_sided && settings.duplex.is_none() {
        options.push(("sides".into(), "two-sided-long-edge".into()));
    }
    let asked = [
        ("PageSize", settings.page_size.as_deref()),
        ("ColorModel", settings.colour.as_deref()),
        ("Duplex", settings.duplex.as_deref()),
        ("Orientation", settings.orientation.as_deref()),
    ];
    let unsupported = asked.iter().filter(|(_, value)| value.is_some()).count();
    if unsupported == 0 {
        return Ok(options);
    }
    // Whatever the driver reports decides which of them may travel; a queue
    // that cannot answer keeps only what every printer reads (pages, sides).
    let reported = printer_options(queue).unwrap_or_default();
    for (name, value) in asked {
        let Some(value) = value else { continue };
        let choices = match name {
            "PageSize" => &reported.page_sizes,
            "ColorModel" => &reported.colour_modes,
            "Duplex" => &reported.duplex_modes,
            _ => &reported.orientations,
        };
        if PrinterOptions::choice(choices, value).is_none() {
            return Err(format!(
                "this printer does not offer {value} as its {name}; choose one it reports"
            ));
        }
        options.push((name.to_owned(), value.to_owned()));
    }
    Ok(options)
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
                    &PrintSettings {
                        copies: 0,
                        ..PrintSettings::default()
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

#[cfg(test)]
mod print_option_tests {
    use super::*;

    /// What a colour duplex printer reports, as CUPS prints it.
    const REPORT: &str = "\
PageSize/Media Size: *A4 A5 Letter Legal
ColorModel/Color Mode: *RGB Gray
Duplex/2-Sided Printing: *None DuplexNoTumble DuplexTumble
Resolution/Output Resolution: *600dpi 300dpi
Orientation/Orientation: *3 4
printer-make-and-model/Custom.10 = Acme Laser 4
";

    #[test]
    fn the_drivers_own_options_come_out_with_their_defaults() {
        let options = parse_lpoptions(REPORT);
        assert_eq!(
            options
                .page_sizes
                .iter()
                .map(|choice| choice.key.as_str())
                .collect::<Vec<_>>(),
            ["A4", "A5", "Letter", "Legal"]
        );
        assert_eq!(PrinterOptions::default_key(&options.page_sizes), Some("A4"));
        assert_eq!(
            PrinterOptions::default_key(&options.colour_modes),
            Some("RGB")
        );
        assert_eq!(
            options
                .duplex_modes
                .iter()
                .find(|choice| choice.key == "DuplexTumble")
                .map(|choice| choice.label.as_str()),
            Some("Two-sided, short edge")
        );
        assert_eq!(
            PrinterOptions::default_key(&options.orientations),
            Some("3"),
            "the orientation values keep the numbers CUPS takes"
        );
        assert!(
            options
                .orientations
                .iter()
                .any(|choice| choice.label == "Landscape"),
            "a person reads a word, not a 4"
        );
    }

    #[test]
    fn a_printer_that_reports_nothing_offers_nothing() {
        let options = parse_lpoptions("");
        assert_eq!(options, PrinterOptions::default());
        assert!(
            parse_lpoptions("Resolution/Output Resolution: *600dpi\n")
                .page_sizes
                .is_empty()
        );
    }

    #[test]
    fn a_page_range_must_be_one_cups_reads() {
        for good in ["1-3", "1,4,9", "2-", "7", "1-3, 8-10"] {
            assert!(PageRange::parse(good).is_ok(), "{good}");
        }
        for bad in ["", "  ", "0", "1-0", "5-2", "1,,3", "1-a", "-3", "one"] {
            assert!(PageRange::parse(bad).is_err(), "{bad}");
        }
    }
}
