//! Print Centre's engine work, as a package reaches it: the printer's own options, printing a
//! document, scanning a page, reading a page's text, and writing the scanned pages as one PDF.
//!
//! The app runs sandboxed with no printer and no scanner, so it asks by name. A document it
//! holds reaches the engine by its content address; what the engine writes back is kept in the
//! content store as a file the app shows. Printing and scanning are jobs, since they can take
//! minutes; the text and the PDF answer now.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use serde::Deserialize;
use serde_json::json;

use tilo_content::ContentStore;
use tilolang_tilo::engine::{self, JobOp, JobSink, Needs, Provider, Running, State};

/// The one app this device answers engine work for.
pub const APP: &str = "so.tilo.print-centre";

/// Printing and scanning are jobs, since they can take minutes. Both run in the app's own
/// process: a scan keeps its page in the app's content store as it ends.
const JOBS: &[JobOp] = &[
    JobOp::new("print", Needs::APP_STATE),
    JobOp::new("scan", Needs::APP_STATE),
];

/// Print Centre's engine, keeping what it makes in `store`.
struct Engine {
    store: ContentStore,
}

/// Register Print Centre's engine over the app's content store `store`.
pub fn register(store: &ContentStore) {
    engine::register(
        APP,
        Arc::new(Engine {
            store: store.clone(),
        }),
    );
}

impl Provider for Engine {
    fn jobs(&self) -> &'static [JobOp] {
        JOBS
    }

    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        match op {
            "options" => options(input),
            "ocr" => ocr(&self.store, input),
            "pages_pdf" => pages_pdf(&self.store, input),
            other => Err(format!("`{other}` is no op this device runs for {APP}")),
        }
    }

    fn start(&self, job: &str, op: &str, input: &str, moved: JobSink) -> Result<(), String> {
        match op {
            "print" => print(&self.store, job, input, moved),
            "scan" => scan(&self.store, job, input, moved),
            other => Err(format!("`{other}` is no op this device runs for {APP}")),
        }
    }

    fn cancel(&self, job: &str) -> bool {
        if running().get(job).is_none() {
            return false;
        }
        running().end(job, State::Cancelled, json!({ "url": "", "error": "" }));
        true
    }
}

/// The prints and scans running now.
fn running() -> &'static Running<()> {
    static RUNNING: OnceLock<Running<()>> = OnceLock::new();
    RUNNING.get_or_init(Running::default)
}

/// The job `job` ended: done with the file it made at `url` (empty for a print), or failed.
fn end(job: &str, made: Result<String, String>) {
    match made {
        Ok(url) => running().end(job, State::Done, json!({ "url": url, "error": "" })),
        Err(error) => running().end(job, State::Failed, json!({ "url": "", "error": error })),
    }
}

/// Run `work` as the job `job` on a thread of its own, named `name`: it is kept until it ends,
/// and stopping it tells its end at once.
fn run(
    job: &str,
    moved: JobSink,
    name: &str,
    work: impl FnOnce() -> Result<String, String> + Send + 'static,
) -> Result<(), String> {
    moved.moved(State::Running, json!({ "url": "", "error": "" }));
    running().keep(job, Arc::new(()), moved);
    let named = job.to_owned();
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || end(&named, work()))
        .map(|_| ())
        .map_err(|error| {
            running().forget(job);
            format!("could not start the work: {error}")
        })
}

fn reads<T: for<'de> Deserialize<'de>>(input: &str) -> Result<T, String> {
    serde_json::from_str(input).map_err(|error| error.to_string())
}

/// The bytes a `tilo-blob://` address holds, read from the device's content store.
fn bytes_of(store: &ContentStore, url: &str) -> Result<Vec<u8>, String> {
    let hash = url
        .strip_prefix("tilo-blob://")
        .ok_or_else(|| format!("`{url}` is no content address"))?;
    crate::files::read(store, hash)
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
    let reported = os_actions::printing::printer_options(&ask.queue).unwrap_or_default();
    let choices = |list: &[os_actions::printing::OptionChoice]| {
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

fn print(store: &ContentStore, job: &str, input: &str, moved: JobSink) -> Result<(), String> {
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
    let bytes = bytes_of(store, &ask.url)?;
    let path = scratch(&ask.name.replace(['/', '\\'], "_"));
    std::fs::write(&path, &bytes).map_err(|error| error.to_string())?;
    let settings = os_actions::printing::PrintSettings {
        copies: ask.copies.unwrap_or(1).clamp(1, 99),
        two_sided: ask.two_sided.unwrap_or(false),
        page_range: match ask.pages.as_deref().map(str::trim) {
            Some(pages) if !pages.is_empty() => {
                Some(os_actions::printing::PageRange::parse(pages)?)
            }
            _ => None,
        },
        page_size: ask.page_size,
        colour: ask.colour,
        duplex: ask.duplex,
        orientation: ask.orientation,
    };
    run(job, moved, "print-centre-print", move || {
        let result = os_actions::printing::print_document(&ask.queue, &path, &settings);
        let _ = std::fs::remove_file(&path);
        result.map(|()| String::new())
    })
}

fn scan(store: &ContentStore, job: &str, input: &str, moved: JobSink) -> Result<(), String> {
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
    let name = ask
        .name
        .unwrap_or_else(|| format!("scan-{}.png", uuid::Uuid::new_v4().simple()));
    let scanner = ask.scanner;
    let store = store.clone();
    run(job, moved, "print-centre-scan", move || {
        let png = tilo_capture::scanner::scan_page(&scanner, settings)?;
        let kept = crate::files::keep(&store, &name, "image/png", &png)?;
        Ok(format!("tilo-blob://{}", kept.hash))
    })
}

fn ocr(store: &ContentStore, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        url: String,
        language: Option<String>,
    }
    let ask: Ask = reads(input)?;
    let bytes = bytes_of(store, &ask.url)?;
    let path = scratch("ocr.png");
    std::fs::write(&path, &bytes).map_err(|error| error.to_string())?;
    let outcome = tilo_ocr::recognize_path(&path, ask.language.as_deref().unwrap_or("eng"));
    let _ = std::fs::remove_file(&path);
    let document = outcome.map_err(|error| error.to_string())?;
    Ok(serde_json::json!({ "text": document.plain_text() }).to_string())
}

fn pages_pdf(store: &ContentStore, input: &str) -> Result<String, String> {
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
        let bytes = bytes_of(store, &page.url)?;
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
    let kept = crate::files::keep(store, &name, "application/pdf", &pdf)?;
    Ok(serde_json::json!({ "url": format!("tilo-blob://{}", kept.hash) }).to_string())
}
