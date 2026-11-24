//! Print Centre's engine work, as a package reaches it: the printer's own options, printing a
//! document, scanning a page, reading a page's text, and writing the scanned pages as one PDF.
//!
//! The app runs sandboxed with no printer and no scanner, so it asks by name. A document it
//! holds reaches the engine by its content address; what the engine writes back is kept in the
//! content store as a file the app shows. Printing and scanning are jobs, since they can take
//! minutes; the text and the PDF answer now.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Deserialize;

use tilolang_tilo::instance::Platform;

/// The one app this device answers engine work for.
pub const APP: &str = "so.tilo.print-centre";

/// One `engine.call` for this device's app, or a refusal.
///
/// # Errors
///
/// The op is unknown, an input does not read, or the work cannot be done.
pub fn call(platform: &dyn Platform, app: &str, op: &str, input: &str) -> Result<String, String> {
    if app != APP {
        return Err(format!("`{op}` is no op this device runs for {app}"));
    }
    match op {
        "options" => options(input),
        "ocr" => ocr(platform, input),
        "pages_pdf" => pages_pdf(platform, input),
        other => Err(format!("`{other}` is no op this device runs for {app}")),
    }
}

/// One `engine.start`: a print or a scan, answered with the job's id.
///
/// # Errors
///
/// Not this device's app, or the input does not read.
pub fn start(platform: &dyn Platform, app: &str, op: &str, input: &str) -> Result<String, String> {
    if app != APP {
        return Err(format!("`{op}` is no op this device runs for {app}"));
    }
    match op {
        "print" => print(platform, input),
        "scan" => scan(platform, input),
        other => Err(format!("`{other}` is no op this device runs for {app}")),
    }
}

/// How far a job has got, as JSON: its state, and what it made or why it failed.
///
/// # Errors
///
/// No such job on this device.
pub fn status(platform: &dyn Platform, job: &str) -> Result<String, String> {
    let state = jobs()
        .lock()
        .map_err(|_| "the job table is unreadable".to_owned())?
        .get(job)
        .cloned();
    let Some(state) = state else {
        return Err(format!("no job `{job}` runs on this device"));
    };
    state.take_scan(platform);
    if state.stopped.load(Ordering::Relaxed) {
        return Ok(serde_json::json!({ "state": "cancelled", "url": "", "error": "" }).to_string());
    }
    if let Some(error) = state.error.lock().ok().and_then(|error| error.clone()) {
        return Ok(serde_json::json!({ "state": "failed", "url": "", "error": error }).to_string());
    }
    match state.url.lock().ok().and_then(|url| url.clone()) {
        Some(url) => {
            Ok(serde_json::json!({ "state": "done", "url": url, "error": "" }).to_string())
        }
        None => Ok(serde_json::json!({ "state": "running", "url": "", "error": "" }).to_string()),
    }
}

/// Stop a job: false when it is not this device's or it had finished.
pub fn cancel(job: &str) -> bool {
    let Ok(known) = jobs().lock() else {
        return false;
    };
    let Some(state) = known.get(job) else {
        return false;
    };
    if state.finished.load(Ordering::Relaxed) {
        return false;
    }
    state.stopped.store(true, Ordering::Relaxed);
    true
}

/// A scan the scanner answered: its name, and the bytes its receiver keeps as the job's file on
/// the next ask.
type Scan = (String, std::sync::mpsc::Receiver<Result<Vec<u8>, String>>);

/// One job in flight, and what it made.
struct Job {
    finished: AtomicBool,
    stopped: AtomicBool,
    error: Mutex<Option<String>>,
    url: Mutex<Option<String>>,
    /// A scan the scanner answered, kept as the job's file on the next ask.
    scan: Mutex<Option<Scan>>,
}

impl Job {
    /// Keep what the scanner answered, once: the file the app shows.
    fn take_scan(&self, platform: &dyn Platform) {
        let Ok(mut slot) = self.scan.lock() else {
            return;
        };
        let Some((name, rx)) = slot.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(png)) => {
                match platform.keep_content(name, "image/png", &png) {
                    Ok(kept) => {
                        if let Ok(mut url) = self.url.lock() {
                            *url = Some(format!("tilo-blob://{}", kept.hash));
                        }
                    }
                    Err(error) => {
                        if let Ok(mut slot) = self.error.lock() {
                            *slot = Some(error);
                        }
                    }
                }
                *slot = None;
            }
            Ok(Err(error)) => {
                if let Ok(mut slot) = self.error.lock() {
                    *slot = Some(error);
                }
                *slot = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                if let Ok(mut slot) = self.error.lock() {
                    *slot = Some("the scanner did not answer".to_owned());
                }
                *slot = None;
            }
        }
    }
}

fn jobs() -> &'static Mutex<std::collections::BTreeMap<String, Arc<Job>>> {
    static JOBS: OnceLock<Mutex<std::collections::BTreeMap<String, Arc<Job>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()))
}

fn next_job_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("print-centre-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

fn reads<T: for<'de> Deserialize<'de>>(input: &str) -> Result<T, String> {
    serde_json::from_str(input).map_err(|error| error.to_string())
}

/// The bytes a `tilo-blob://` address holds, read from the device's content store.
fn bytes_of(platform: &dyn Platform, url: &str) -> Result<Vec<u8>, String> {
    let hash = url
        .strip_prefix("tilo-blob://")
        .ok_or_else(|| format!("`{url}` is no content address"))?;
    platform
        .read_content(hash)
        .ok_or_else(|| format!("the content store does not hold `{hash}`"))
}

/// Where a job writes the bytes it prints before the printer takes them.
fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("tilo-print-{}-{name}", std::process::id()))
}

fn options(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        queue: String,
    }
    let ask: Ask = reads(input)?;
    let reported = tilo_printing::printing::printer_options(&ask.queue).unwrap_or_default();
    let choices = |list: &[tilo_printing::printing::OptionChoice]| {
        list.iter()
            .map(|choice| {
                serde_json::json!({
                    "key": choice.key,
                    "label": choice.label,
                    "default": choice.default,
                })
            })
            .collect::<Vec<_>>()
    };
    Ok(serde_json::json!({
        "page_sizes": choices(&reported.page_sizes),
        "colour_modes": choices(&reported.colour_modes),
        "duplex_modes": choices(&reported.duplex_modes),
        "orientations": choices(&reported.orientations),
    })
    .to_string())
}

fn print(platform: &dyn Platform, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        queue: String,
        url: String,
        name: String,
        copies: Option<u16>,
        pages: Option<String>,
        two_sided: Option<bool>,
        page_size: Option<String>,
        colour: Option<String>,
        duplex: Option<String>,
        orientation: Option<String>,
    }
    let ask: Ask = reads(input)?;
    let bytes = bytes_of(platform, &ask.url)?;
    let path = scratch(&ask.name.replace(['/', '\\'], "_"));
    std::fs::write(&path, &bytes).map_err(|error| error.to_string())?;
    let settings = tilo_printing::printing::PrintSettings {
        copies: ask.copies.unwrap_or(1).clamp(1, 99),
        two_sided: ask.two_sided.unwrap_or(false),
        page_range: match ask.pages.as_deref().map(str::trim) {
            Some(pages) if !pages.is_empty() => {
                Some(tilo_printing::printing::PageRange::parse(pages)?)
            }
            _ => None,
        },
        page_size: ask.page_size,
        colour: ask.colour,
        duplex: ask.duplex,
        orientation: ask.orientation,
    };
    let id = next_job_id();
    let state = Arc::new(Job {
        finished: AtomicBool::new(false),
        stopped: AtomicBool::new(false),
        error: Mutex::new(None),
        url: Mutex::new(None),
        scan: Mutex::new(None),
    });
    jobs()
        .lock()
        .map_err(|_| "the job table is unreadable".to_owned())?
        .insert(id.clone(), Arc::clone(&state));
    let worker = Arc::clone(&state);
    std::thread::Builder::new()
        .name("print-centre-print".to_owned())
        .spawn(move || {
            let result = tilo_printing::printing::print_document(&ask.queue, &path, &settings);
            let _ = std::fs::remove_file(&path);
            match result {
                Ok(()) => {
                    if let Ok(mut url) = worker.url.lock() {
                        *url = Some(String::new());
                    }
                }
                Err(error) => {
                    if let Ok(mut slot) = worker.error.lock() {
                        *slot = Some(error);
                    }
                }
            }
            worker.finished.store(true, Ordering::Relaxed);
        })
        .map_err(|error| format!("could not start the print: {error}"))?;
    Ok(serde_json::json!({ "job": id }).to_string())
}

fn scan(_platform: &dyn Platform, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        scanner: String,
        resolution: Option<u16>,
        colour: Option<String>,
        source: Option<String>,
        name: Option<String>,
    }
    let ask: Ask = reads(input)?;
    let settings = tilo_capture::scanner::ScanSettings {
        resolution_dpi: ask.resolution.unwrap_or(300).clamp(50, 2400),
        color: match ask.colour.as_deref() {
            Some("grayscale") => tilo_capture::scanner::ScanColor::Grayscale,
            Some("lineart") => tilo_capture::scanner::ScanColor::LineArt,
            _ => tilo_capture::scanner::ScanColor::Color,
        },
        source: match ask.source.as_deref() {
            Some("feeder") => tilo_capture::scanner::ScanSource::Feeder,
            Some("feeder-duplex") => tilo_capture::scanner::ScanSource::FeederDuplex,
            _ => tilo_capture::scanner::ScanSource::Flatbed,
        },
    };
    let id = next_job_id();
    let state = Arc::new(Job {
        finished: AtomicBool::new(false),
        stopped: AtomicBool::new(false),
        error: Mutex::new(None),
        url: Mutex::new(None),
        scan: Mutex::new(None),
    });
    jobs()
        .lock()
        .map_err(|_| "the job table is unreadable".to_owned())?
        .insert(id.clone(), Arc::clone(&state));
    let worker = Arc::clone(&state);
    let name = ask.name.unwrap_or_else(|| format!("scan-{id}.png"));
    let scanner = ask.scanner;
    // The platform is not `Send` everywhere; the scan runs on its own thread with the bytes it
    // writes kept through a channel back to this one, which keeps them when the job is asked.
    let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<u8>, String>>();
    std::thread::Builder::new()
        .name("print-centre-scan".to_owned())
        .spawn(move || {
            let result = tilo_capture::scanner::scan_page(&scanner, settings);
            let _ = tx.send(result);
            worker.finished.store(true, Ordering::Relaxed);
        })
        .map_err(|error| format!("could not start the scan: {error}"))?;
    if let Ok(mut slot) = state.scan.lock() {
        *slot = Some((name, rx));
    }
    Ok(serde_json::json!({ "job": id }).to_string())
}

fn ocr(platform: &dyn Platform, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        url: String,
        language: Option<String>,
    }
    let ask: Ask = reads(input)?;
    let bytes = bytes_of(platform, &ask.url)?;
    let path = scratch("ocr.png");
    std::fs::write(&path, &bytes).map_err(|error| error.to_string())?;
    let outcome = tilo_ocr::recognize_path(&path, ask.language.as_deref().unwrap_or("eng"));
    let _ = std::fs::remove_file(&path);
    let document = outcome.map_err(|error| error.to_string())?;
    Ok(serde_json::json!({ "text": document.plain_text() }).to_string())
}

fn pages_pdf(platform: &dyn Platform, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Page {
        url: String,
        #[serde(default)]
        text: String,
    }
    #[derive(Deserialize)]
    struct Ask {
        pages: Vec<Page>,
        name: Option<String>,
    }
    let ask: Ask = reads(input)?;
    let mut pages = Vec::with_capacity(ask.pages.len());
    for page in &ask.pages {
        let bytes = bytes_of(platform, &page.url)?;
        let decoded = image::load_from_memory(&bytes).map_err(|error| error.to_string())?;
        let rgb = decoded.to_rgb8();
        pages.push(tilo_doc_pdf::ScannedPdfPage {
            width: rgb.width(),
            height: rgb.height(),
            rgb: rgb.into_raw(),
            recognized_text: page.text.clone(),
        });
    }
    let pdf = tilo_doc_pdf::render_scanned_pdf(&pages);
    let name = ask.name.unwrap_or_else(|| "scan.pdf".to_owned());
    let kept = platform.keep_content(&name, "application/pdf", &pdf)?;
    Ok(serde_json::json!({ "url": format!("tilo-blob://{}", kept.hash) }).to_string())
}
