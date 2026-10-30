use std::cell::RefCell;
use std::io::Write;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, mpsc};
use ui::framework::{
    Dispatcher, Effect as _, EffectAnswer, EffectCx, EffectStatus, Effected, Live, LiveRow,
    Publishes, RunMode, ViewCx,
};
use ui::widget::Element;

use ui::widget::{AppModel, ModelPath};
use ui::widget::{CropSelection, Widget};

mod notice;
mod views;
mod window_bar;
use notice::{OperationNotice, Retry, Work};
use views::{
    PrintForm, device_view, diagnostics_view, discovery_view, history_view, live_view,
    loading_view, print_setup_view, queue_view, scan_setup_view,
};
type El = Box<dyn Widget>;

use crate::resource::{PrintCentreEffect, PrintDefaults, SCAN_RESOLUTIONS};

/// What the window shows and the work running on it, shared by the
/// [`PrintCentreWindow`] and the view's effect handler.
pub struct PrintCentre {
    /// A page an effect or a finished job put up, for the window to show
    /// next.
    shown: Option<El>,
    /// The page the window shows.
    page: Page,
    refresh: Option<mpsc::Receiver<Snapshot>>,
    snapshot: Option<Snapshot>,
    /// What Needs you reports back, and what each waiting failure's Try
    /// again asks for.
    needs: ui::attention::Presses,
    pending_retries: std::collections::HashMap<ui::attention::ItemId, Retry>,
    pending_cancel: Option<String>,
    /// What the last search for network printers found.
    discovered: Option<Result<Vec<os_actions::printing::DiscoveredPrinter>, String>>,
    picker: Option<ui::portal::PortalChannel>,
    pending_document: Option<PathBuf>,
    print_preview: PrintPreview,
    print_preview_rx: Option<mpsc::Receiver<Option<PrintPagePreview>>>,
    selected_printer: Option<String>,
    print_copies: ui::LocalState<f64>,
    print_two_sided: ui::LocalState<bool>,
    /// The pages to print, as typed; empty or unparsable sends the whole
    /// document only after the range is read back at send time.
    print_range: ui::LocalState<String>,
    /// What the chosen printer's driver reports it can do. A form control is
    /// drawn for an option only when it appears here.
    print_options: os_actions::printing::PrinterOptions,
    print_options_rx: Option<mpsc::Receiver<os_actions::printing::PrinterOptions>>,
    /// The driver values the person chose, by the option CUPS takes them as.
    print_choice: PrintChoice,
    /// The document page the card shows, and how many the document has.
    preview_page: usize,
    preview_pages: usize,
    selected_scanner: Option<String>,
    scan: ScanState,
    /// The scan is being saved where the person chose.
    scan_saving: bool,
    /// The person's print and scan defaults, as last presented.
    defaults: PrintDefaults,
    /// Where changes to the defaults go.
    pub(crate) dispatcher: Option<Dispatcher<PrintDefaults>>,
    pub(crate) controls: Option<ui::framework::Actions<PrintDefaults>>,
    /// The window bar's entries, which run the pages' effects.
    pub(crate) bar: Option<ui::framework::BarActions<PrintDefaults>>,
}

/// The [`PrintCentre`] as the window and the effect handler share it.
pub(crate) type Shared = Rc<RefCell<PrintCentre>>;

/// The window: the page the print centre shows.
pub struct PrintCentreWindow {
    centre: Shared,
    page: El,
    /// The defaults as the document holds them, shown as they change;
    /// `None` for a window outside the framework.
    live: Option<Live<PrintDefaults>>,
    presented: Option<(u64, u64)>,
    /// The page was replaced since the last layout.
    replaced: bool,
}

/// The scan page's controls and the pages scanned so far.
struct ScanState {
    resolution: ui::LocalState<usize>,
    color: ui::LocalState<usize>,
    source: ui::LocalState<usize>,
    language: ui::LocalState<usize>,
    crop: ui::LocalState<CropSelection>,
    result: Option<ScannedPage>,
    pages: Vec<ScannedPage>,
    selected: usize,
    in_progress: bool,
}

impl Default for ScanState {
    fn default() -> Self {
        Self {
            resolution: ui::LocalState::new(1),
            color: ui::LocalState::new(0),
            source: ui::LocalState::new(0),
            language: ui::LocalState::new(0),
            crop: ui::LocalState::new(CropSelection::default()),
            result: None,
            pages: Vec::new(),
            selected: 0,
            in_progress: false,
        }
    }
}

#[derive(Clone, Default)]
enum PrintPreview {
    Loading,
    Ready {
        rgba: Arc<[u8]>,
        width: u32,
        height: u32,
        /// Which page of the document this is (0-based), and how many the
        /// document has, so the card can say "Page 3 of 40".
        page: usize,
        pages: usize,
    },
    #[default]
    Unavailable,
}

/// One page of the document being printed, with the document's page count.
#[derive(Clone)]
struct PrintPagePreview {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    page: usize,
    pages: usize,
}

/// `path`'s page `page` (0-based) drawn to fit a preview box, and how many
/// pages the document has.
///
/// A PDF pages through the PDF engine, which Print Centre links and the
/// thumbnailer every app shares leaves out; an image or a text file is one
/// page, and the thumbnailer already draws it.
fn load_print_preview(path: &std::path::Path, page: usize) -> Option<PrintPagePreview> {
    let bytes = std::fs::read(path).ok()?;
    if let Some(document) = tilo_pdf::PagePreview::open(&bytes) {
        let pages = document.page_count();
        let page = page.min(pages.saturating_sub(1));
        let (rgba, width, height) = document.render(page, 720, 960)?;
        return Some(PrintPagePreview {
            rgba,
            width,
            height,
            page,
            pages,
        });
    }
    // Not a page format: the shared thumbnailer draws the whole file.
    thumbnail::set_pdf_rasteriser(tilo_pdf::first_page_preview);
    let category = thumbnail::category_for_path(path);
    let thumb = thumbnail::generate(&thumbnail::Source::Path(path), category, 720)?;
    Some(PrintPagePreview {
        rgba: thumb.rgba,
        width: thumb.width,
        height: thumb.height,
        page: 0,
        pages: 1,
    })
}

/// The options the driver offers, as the person chose them. Each is a value
/// the driver reported; `None` is the printer's own default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PrintChoice {
    pub(crate) page_size: Option<String>,
    pub(crate) colour: Option<String>,
    pub(crate) duplex: Option<String>,
    pub(crate) orientation: Option<String>,
}

#[derive(Clone)]
struct ScannedPage {
    png: Arc<[u8]>,
    rgba: Arc<[u8]>,
    width: u32,
    height: u32,
    recognized_text: Option<String>,
    ocr_note: String,
    ocr_language: String,
    ocr_needs_refresh: bool,
    ocr_in_progress: bool,
}

impl ScannedPage {
    fn rotate_clockwise(&mut self) -> Result<(), String> {
        let source = image::RgbaImage::from_raw(self.width, self.height, self.rgba.to_vec())
            .ok_or_else(|| "scanned page pixels were incomplete".to_string())?;
        let rotated = image::imageops::rotate90(&source);
        let png = ui::png::encode_rgba8(rotated.width(), rotated.height(), rotated.as_raw())
            .map_err(|error| format!("could not update the scanned page: {error}"))?;
        self.width = rotated.width();
        self.height = rotated.height();
        self.rgba = Arc::from(rotated.into_raw());
        self.png = Arc::from(png);
        self.recognized_text = None;
        self.ocr_note = "Text recognition needs updating after rotation.".into();
        self.ocr_needs_refresh = true;
        self.ocr_in_progress = false;
        Ok(())
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "validated normalized crop coordinates are clamped to 0..=1 and multiplied by u32 image dimensions before conversion"
    )]
    fn crop_to(&mut self, crop: CropSelection) -> Result<(), String> {
        if ![crop.left, crop.top, crop.right, crop.bottom]
            .into_iter()
            .all(f64::is_finite)
            || crop.left < 0.0
            || crop.top < 0.0
            || crop.right > 1.0
            || crop.bottom > 1.0
            || crop.right - crop.left < 0.05
            || crop.bottom - crop.top < 0.05
        {
            return Err("crop selection was invalid".into());
        }
        let source = image::RgbaImage::from_raw(self.width, self.height, self.rgba.to_vec())
            .ok_or_else(|| "scanned page pixels were incomplete".to_string())?;
        let width = f64::from(self.width);
        let height = f64::from(self.height);
        let x = (crop.left.clamp(0.0, 0.95) * width).floor() as u32;
        let y = (crop.top.clamp(0.0, 0.95) * height).floor() as u32;
        let right = (crop.right.clamp(0.05, 1.0) * width).ceil() as u32;
        let bottom = (crop.bottom.clamp(0.05, 1.0) * height).ceil() as u32;
        let crop_width = right
            .saturating_sub(x)
            .max(1)
            .min(self.width.saturating_sub(x));
        let crop_height = bottom
            .saturating_sub(y)
            .max(1)
            .min(self.height.saturating_sub(y));
        let cropped = image::imageops::crop_imm(&source, x, y, crop_width, crop_height).to_image();
        let png = ui::png::encode_rgba8(cropped.width(), cropped.height(), cropped.as_raw())
            .map_err(|error| format!("could not update the scanned page: {error}"))?;
        self.width = cropped.width();
        self.height = cropped.height();
        self.rgba = Arc::from(cropped.into_raw());
        self.png = Arc::from(png);
        self.recognized_text = None;
        self.ocr_note = "Text recognition needs updating after cropping.".into();
        self.ocr_needs_refresh = true;
        self.ocr_in_progress = false;
        Ok(())
    }
}

/// What CUPS and SANE reported: printers, documents and scanners.
#[derive(Clone)]
pub(crate) struct Snapshot {
    queues: Result<Vec<os_actions::printing::PrinterQueue>, String>,
    jobs: Result<Vec<os_actions::printing::PrintJob>, String>,
    scanners: Result<Vec<tilo_capture::scanner::Scanner>, String>,
}

/// Where the page's controls send what they do: the surface's effects and
/// the person's defaults go through the same door as the palette's and an
/// agent's.
#[derive(Clone, Default)]
struct Door(
    Option<Dispatcher<PrintDefaults>>,
    Option<ui::framework::Actions<PrintDefaults>>,
);

#[derive(Clone)]
enum UiCommand {
    Refresh,
    Dashboard,
    Queue,
    History,
    Diagnostics,
    Discover,
    AddPrinter(os_actions::printing::DiscoveredPrinter),
    Device(String),
    SetDefault(String),
    SetEnabled(String, bool),
    RequestCancel(String),
    ConfirmCancel(String),
    RetryJob(String),
    PreviousPrintPage,
    NextPrintPage,
    SetPrintOption { option: String, value: String },
    PrintDocument,
    CancelPrintSetup,
    ScanSetup,
    NewScannedDocument,
    SelectScanner(String),
    ScanPage,
    SaveScannedPage,
    SaveScannedDocument,
    CopyRecognizedText,
    ScanAnother,
    PreviousScanPage,
    NextScanPage,
    MoveScanPageEarlier,
    MoveScanPageLater,
    DeleteScanPage,
    RotateScanPage,
    ApplyScanCrop,
    RecognizeCurrentPage,
}

impl Door {
    fn send(&self, request: UiCommand) -> Result<(), String> {
        use PrintCentreEffect as E;
        let dispatcher = self.0.as_ref().ok_or("Print Centre is not open")?;
        let effect = match request {
            UiCommand::Refresh => E::Refresh,
            UiCommand::Dashboard => E::OpenDashboard,
            UiCommand::Queue => E::OpenQueue,
            UiCommand::History => E::OpenHistory,
            UiCommand::Diagnostics => E::RunDiagnostics,
            UiCommand::Discover => E::FindPrinters,
            UiCommand::AddPrinter(printer) => E::AddPrinter {
                printer: printer.uri,
            },
            UiCommand::Device(printer) => E::OpenPrinter { printer },
            UiCommand::SetDefault(printer) => E::SetDefaultPrinter { printer },
            UiCommand::SetEnabled(printer, true) => E::ResumePrinter { printer },
            UiCommand::SetEnabled(printer, false) => E::PausePrinter { printer },
            UiCommand::RequestCancel(job) => E::ConfirmCancel { job },
            UiCommand::ConfirmCancel(job) => E::CancelDocument { job },
            UiCommand::RetryJob(job) => E::RetryDocument { job },
            UiCommand::PreviousPrintPage => E::PreviousPrintPage,
            UiCommand::NextPrintPage => E::NextPrintPage,
            UiCommand::SetPrintOption { option, value } => E::SetPrintOption { option, value },
            UiCommand::PrintDocument => E::SendToPrinter,
            UiCommand::CancelPrintSetup => E::ClosePrintSetup,
            UiCommand::ScanSetup => E::ScanDocument,
            UiCommand::NewScannedDocument => E::NewScannedDocument,
            UiCommand::SelectScanner(scanner) => E::ScanWith { scanner },
            UiCommand::ScanPage => E::ScanPage,
            UiCommand::SaveScannedPage => E::SaveScannedPage,
            UiCommand::SaveScannedDocument => E::SaveScannedDocument,
            UiCommand::CopyRecognizedText => E::CopyRecognizedText,
            UiCommand::ScanAnother => E::ScanAnother,
            UiCommand::PreviousScanPage => E::PreviousScanPage,
            UiCommand::NextScanPage => E::NextScanPage,
            UiCommand::MoveScanPageEarlier => E::MoveScanPageEarlier,
            UiCommand::MoveScanPageLater => E::MoveScanPageLater,
            UiCommand::DeleteScanPage => E::RemoveScanPage,
            UiCommand::RotateScanPage => E::RotateScanPage,
            UiCommand::ApplyScanCrop => E::CropScanPage,
            UiCommand::RecognizeCurrentPage => E::RecognizeText,
        };
        dispatcher.effect(&effect);
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
enum Page {
    #[default]
    Loading,
    Dashboard,
    Queue,
    History,
    Diagnostics,
    Device(String),
    Discovery,
    Print,
    Scan,
    /// What wants a person, opened from the window's bar.
    Needs,
}

/// How saving scanned pages ended.
enum ScanSaved {
    Saved(String),
    Cancelled,
    NoPicker,
    Failed,
}

fn display_name(raw: &str) -> String {
    raw.replace(['_', '-'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
fn job_title(job: &os_actions::printing::PrintJob) -> String {
    job.title.clone().unwrap_or_else(|| {
        format!(
            "Document {}",
            job.id
                .rsplit_once('-')
                .map_or(job.id.as_str(), |(_, id)| id)
        )
    })
}
fn failed_areas(
    snapshot: &Snapshot,
    queues: bool,
    jobs: bool,
    scanners: bool,
) -> Vec<&'static str> {
    let mut areas = Vec::new();
    if queues && snapshot.queues.is_err() {
        areas.push("printers");
    }
    if jobs && snapshot.jobs.is_err() {
        areas.push("documents");
    }
    if scanners && snapshot.scanners.is_err() {
        areas.push("scanners");
    }
    areas
}
fn scanned_document_pdf(pages: &[ScannedPage]) -> Vec<u8> {
    let pages = pages
        .iter()
        .map(|page| tilo_doc_pdf::ScannedPdfPage {
            width: page.width,
            height: page.height,
            rgb: page
                .rgba
                .chunks_exact(4)
                .flat_map(|pixel| pixel.iter().take(3).copied())
                .collect(),
            recognized_text: page.recognized_text.clone().unwrap_or_default(),
        })
        .collect::<Vec<_>>();
    tilo_doc_pdf::render_scanned_pdf(&pages)
}
/// Off the window's thread: wait for where the person chose to save, and
/// write the scanned page there, or every page as one PDF (`document`).
fn write_scan(
    chooser: &ui::portal::PortalChannel,
    document: bool,
    pages: &[ScannedPage],
) -> ScanSaved {
    let mut path = match chooser.recv() {
        Ok(ui::portal::PortalOutcome::Opened(path)) => path,
        Ok(ui::portal::PortalOutcome::Cancelled) => return ScanSaved::Cancelled,
        Ok(ui::portal::PortalOutcome::Failed(_)) | Err(_) => {
            return ScanSaved::NoPicker;
        }
    };
    if path.extension().is_none() {
        path.set_extension(if document { "pdf" } else { "png" });
    }
    let bytes = if document {
        Arc::<[u8]>::from(scanned_document_pdf(pages))
    } else {
        pages
            .first()
            .map_or_else(|| Arc::from(Vec::new()), |page| page.png.clone())
    };
    if bytes.is_empty() {
        return ScanSaved::Cancelled;
    }
    match save_scanned_page(&path, bytes.as_ref()) {
        Ok(()) => ScanSaved::Saved(
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("the page")
                .to_owned(),
        ),
        Err(_) => ScanSaved::Failed,
    }
}

fn save_scanned_page(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    tilo_files::native::private::owner_only(&mut options);
    let mut file = options
        .open(path)
        .map_err(|error| format!("could not create the scanned page: {error}"))?;
    file.write_all(bytes)
        .map_err(|error| format!("could not write the scanned page: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("could not finish saving the scanned page: {error}"))
}
fn resource_key(value: &str) -> String {
    hex::encode(value)
}
fn printer_row(queue: &os_actions::printing::PrinterQueue) -> LiveRow {
    let printer = || queue.name.clone();
    // Opening the printer is Print Centre's own view (`open-printer`, which
    // names it), not this row's: a view here would be the row's disclosure,
    // a private one that keeps making it the default, pausing and resuming
    // it from agents.
    let mut row = LiveRow::new(
        format!("print-centre.printer.{}", resource_key(&queue.name)),
        "print-centre.printer",
        display_name(&queue.name),
    );
    if !queue.default {
        row = row.effect(&PrintCentreEffect::SetDefaultPrinter { printer: printer() });
    }
    row.effect(&if queue.accepting_jobs {
        PrintCentreEffect::PausePrinter { printer: printer() }
    } else {
        PrintCentreEffect::ResumePrinter { printer: printer() }
    })
}
fn job_row(job: &os_actions::printing::PrintJob) -> LiveRow {
    use os_actions::printing::JobState;
    let mut row = LiveRow::new(
        format!("print-centre.job.{}", resource_key(&job.id)),
        "print-centre.job",
        job_title(job),
    );
    let job_id = || job.id.clone();
    if matches!(
        job.state,
        Some(JobState::Pending | JobState::Held | JobState::Processing | JobState::Stopped)
    ) {
        row = row.effect(&PrintCentreEffect::CancelDocument { job: job_id() });
    }
    if matches!(
        job.state,
        Some(JobState::Stopped | JobState::Canceled | JobState::Aborted)
    ) {
        row = row.effect(&PrintCentreEffect::RetryDocument { job: job_id() });
    }
    row
}
fn scanner_row(scanner: &tilo_capture::scanner::Scanner) -> LiveRow {
    LiveRow::new(
        format!("print-centre.scanner.{}", resource_key(&scanner.device)),
        "print-centre.scanner",
        scanner.description.clone(),
    )
    .effect(&PrintCentreEffect::ScanWith {
        scanner: scanner.device.clone(),
    })
}
fn discovered_row(printer: &os_actions::printing::DiscoveredPrinter) -> LiveRow {
    LiveRow::new(
        format!("print-centre.found.{}", resource_key(&printer.uri)),
        "print-centre.found-printer",
        printer.name.clone(),
    )
    .effect(&PrintCentreEffect::AddPrinter {
        printer: printer.uri.clone(),
    })
}
fn friendly_queue_state(queue: &os_actions::printing::PrinterQueue) -> &'static str {
    use os_actions::printing::QueueState;
    match queue.state {
        QueueState::Ready if queue.accepting_jobs => "Ready",
        QueueState::Ready => "Not accepting new documents",
        QueueState::Printing => "Printing",
        QueueState::Paused => "Paused",
        QueueState::Offline => "Cannot reach this printer",
    }
}
fn supply_level(queue: &os_actions::printing::PrinterQueue) -> Option<(f32, u8)> {
    let percent = queue
        .telemetry
        .supplies
        .as_ref()?
        .iter()
        .filter_map(|supply| supply.level_percent)
        .min()?;
    Some((f32::from(percent) / 100.0, percent))
}
fn friendly_reason(reason: &str) -> &'static str {
    let normalized = reason.to_ascii_lowercase();
    if normalized.contains("media-empty") || normalized.contains("out-of-paper") {
        "Add paper to continue."
    } else if normalized.contains("jam") {
        "Clear the paper jam, then resume the printer."
    } else if normalized.contains("cover-open") || normalized.contains("door-open") {
        "Close the printer cover."
    } else if normalized.contains("toner-low") || normalized.contains("marker-supply-low") {
        "A supply is running low; printing can usually continue for now."
    } else if normalized.contains("toner-empty") || normalized.contains("marker-supply-empty") {
        "Replace the empty supply to continue."
    } else if normalized.contains("offline") || normalized.contains("not-connected") {
        "Check that the printer is awake and connected."
    } else {
        "The printer reported a condition that may need attention."
    }
}
fn demo_snapshot() -> Snapshot {
    use os_actions::printing::{
        JobState, PrintJob, PrinterQueue, PrinterSupply, PrinterTelemetry, QueueState,
    };
    use tilo_capture::scanner::Scanner;
    Snapshot {
        queues: Ok(vec![PrinterQueue {
            name: "Studio_printer".into(),
            state: QueueState::Printing,
            default: true,
            accepting_jobs: true,
            reason: None,
            device_uri: None,
            telemetry: PrinterTelemetry {
                available: true,
                state_reasons: Vec::new(),
                supplies: Some(vec![
                    PrinterSupply {
                        name: "Black".into(),
                        color: Some("#111111".into()),
                        level_percent: Some(72),
                    },
                    PrinterSupply {
                        name: "Colour".into(),
                        color: None,
                        level_percent: Some(64),
                    },
                ]),
            },
        }]),
        jobs: Ok(vec![
            PrintJob {
                id: "Studio_printer-42".into(),
                title: Some("Travel itinerary.pdf".into()),
                owner: "ada".into(),
                size_bytes: 1_240_000,
                submitted: "today".into(),
                destination: "Studio_printer".into(),
                state: Some(JobState::Processing),
                state_reasons: Vec::new(),
                impressions_completed: Some(8),
                impressions_total: Some(12),
            },
            PrintJob {
                id: "Studio_printer-41".into(),
                title: Some("Shipping labels.pdf".into()),
                owner: "ada".into(),
                size_bytes: 440_000,
                submitted: "yesterday".into(),
                destination: "Studio_printer".into(),
                state: Some(JobState::Completed),
                state_reasons: Vec::new(),
                impressions_completed: Some(2),
                impressions_total: Some(2),
            },
        ]),
        scanners: Ok(vec![Scanner {
            device: "scanner0".into(),
            description: "Studio scanner".into(),
        }]),
    }
}

fn demo_print_preview() -> PrintPreview {
    let (width, height) = (320, 420);
    let mut page = image::RgbaImage::from_pixel(width, height, image::Rgba([248, 248, 246, 255]));
    for y in 28..88 {
        for x in 24..296 {
            page.put_pixel(x, y, image::Rgba([44, 91, 162, 255]));
        }
    }
    for (index, line_width) in [238, 260, 214, 248, 176, 226].into_iter().enumerate() {
        let y = 126_u32.saturating_add(u32::try_from(index).unwrap_or(0).saturating_mul(38));
        for row in y..y.saturating_add(7) {
            for x in 28..28_u32.saturating_add(line_width) {
                page.put_pixel(x, row, image::Rgba([54, 59, 68, 255]));
            }
        }
    }
    PrintPreview::Ready {
        rgba: Arc::from(page.into_raw()),
        width,
        height,
        // The scenario says how many pages the document stands for.
        page: 0,
        pages: 40,
    }
}

/// What a colour duplex printer reports it can do, as a capture's stand-in
/// for the machine it runs on.
fn demo_printer_options() -> os_actions::printing::PrinterOptions {
    use os_actions::printing::{OptionChoice, PrinterOptions};
    let choice = |key: &str, label: &str, default: bool| OptionChoice {
        key: key.to_owned(),
        label: label.to_owned(),
        default,
    };
    PrinterOptions {
        page_sizes: vec![
            choice("A4", "A4", true),
            choice("A5", "A5", false),
            choice("Letter", "Letter", false),
        ],
        colour_modes: vec![
            choice("RGB", "Colour", true),
            choice("Gray", "Black and white", false),
        ],
        duplex_modes: vec![
            choice("None", "One side", true),
            choice("DuplexNoTumble", "Two-sided, long edge", false),
            choice("DuplexTumble", "Two-sided, short edge", false),
        ],
        orientations: vec![
            choice("3", "Portrait", true),
            choice("4", "Landscape", false),
        ],
    }
}

/// Two printers a discovery found nearby.
fn demo_discovered_printers() -> Vec<os_actions::printing::DiscoveredPrinter> {
    vec![
        os_actions::printing::DiscoveredPrinter {
            uri: "ipps://studio.local/ipp/print".into(),
            name: "Studio printer".into(),
            location: Some("Upstairs workspace".into()),
        },
        os_actions::printing::DiscoveredPrinter {
            uri: "ipps://office.local/ipp/print".into(),
            name: "Office laser".into(),
            location: Some("Downstairs office".into()),
        },
    ]
}

/// A scanned receipt whose text was recognized.
fn demo_scanned_page() -> ScannedPage {
    ScannedPage {
        png: Arc::from(Vec::<u8>::new()),
        rgba: Arc::from(demo_scanned_page_pixels(320, 440)),
        width: 320,
        height: 440,
        recognized_text: Some(
            "Travel receipt\nCoffee and breakfast  €12.40\nMonday, 7 September".into(),
        ),
        ocr_note: "Recognized 8 words.".into(),
        ocr_language: "English".into(),
        ocr_needs_refresh: false,
        ocr_in_progress: false,
    }
}

/// What CUPS and SANE report in screenshot `scenario`; `None` shows a
/// machine without printers.
pub(crate) fn scenario_snapshot(scenario: Option<&str>) -> Option<Snapshot> {
    use os_actions::printing::{JobState, QueueState};
    let mut snapshot = demo_snapshot();
    match scenario {
        Some("empty") => return None,
        Some("service-error" | "service-error-narrow") => {
            snapshot.queues = Err("service unavailable".into());
            snapshot.jobs = Err("service unavailable".into());
            snapshot.scanners = Err("service unavailable".into());
        }
        // The dashboard of a quiet printer.
        None | Some("narrow") => {
            if let Ok(queues) = &mut snapshot.queues
                && let Some(queue) = queues.first_mut()
            {
                queue.state = QueueState::Ready;
            }
            if let Ok(jobs) = &mut snapshot.jobs
                && let Some(job) = jobs.first_mut()
            {
                job.state = Some(JobState::Completed);
                job.impressions_completed = job.impressions_total;
            }
        }
        Some("attention") => {
            if let Ok(queues) = &mut snapshot.queues
                && let Some(queue) = queues.first_mut()
            {
                queue.state = QueueState::Paused;
                queue.accepting_jobs = false;
                queue.telemetry.state_reasons = vec!["media-empty-warning".into()];
            }
            if let Ok(jobs) = &mut snapshot.jobs
                && let Some(job) = jobs.first_mut()
            {
                job.state = Some(JobState::Stopped);
                job.state_reasons = vec!["media-empty-warning".into()];
            }
        }
        Some(_) => {}
    }
    Some(snapshot)
}

/// A deterministic, fully opaque receipt-like page for visual regression
/// captures. It deliberately contains recognizable structure so a blank or
/// zero-sized image viewport is immediately obvious in screenshots.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "deterministic screenshot fixture indexes a checked allocation and divides/modulos by max(width, 1)"
)]
fn demo_scanned_page_pixels(width: u32, height: u32) -> Vec<u8> {
    let width_usize = usize::try_from(width).unwrap_or(0);
    let height_usize = usize::try_from(height).unwrap_or(0);
    let len = width_usize
        .checked_mul(height_usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .unwrap_or(0);
    let mut rgba = vec![0_u8; len];
    for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
        let x = index % width_usize.max(1);
        let y = index / width_usize.max(1);
        let border = x < 4
            || y < 4
            || x >= width_usize.saturating_sub(4)
            || y >= height_usize.saturating_sub(4);
        let header = (38..54).contains(&y) && (34..238).contains(&x);
        let rule = [92, 119, 146, 198, 225, 252, 306]
            .iter()
            .any(|line| y >= *line && y < line.saturating_add(3))
            && (34..width_usize.saturating_sub(34)).contains(&x);
        let total = (328..366).contains(&y) && (30..width_usize.saturating_sub(30)).contains(&x);
        let ink = (total && !(334..360).contains(&y)) || header || rule;
        let color = if border {
            [196, 198, 202, 255]
        } else if total {
            [224, 235, 255, 255]
        } else if ink {
            [45, 51, 65, 255]
        } else {
            [250, 248, 243, 255]
        };
        pixel.copy_from_slice(&color);
    }
    rgba
}

fn request_snapshot() -> Option<mpsc::Receiver<Snapshot>> {
    let (tx, rx) = mpsc::channel();
    #[cfg(test)]
    let installed = tests::INSTALLED.with(|installed| installed.borrow().clone());
    std::thread::Builder::new()
        .name("tilo-print-centre-refresh".into())
        .spawn(move || {
            #[cfg(test)]
            if let Some(snapshot) = installed {
                let _ = tx.send(snapshot);
                return;
            }
            let _ = tx.send(Snapshot {
                queues: os_actions::printing::queues(),
                jobs: os_actions::printing::jobs(),
                scanners: tilo_capture::scanner::scanners(),
            });
        })
        .ok()
        .map(|_| rx)
}

fn empty_view(tx: &Door) -> El {
    let tx = tx.clone();
    Box::new(
        ui::widget::EmptyState::new("Set up your first printer")
            .with_action("Find printers")
            .with_action_id("print-centre.add.first")
            .on_effect(move || {
                let _ = tx.send(UiCommand::Discover);
            }),
    )
}
impl PrintCentre {
    pub fn new() -> Self {
        let defaults = PrintDefaults::default();
        Self {
            shown: None,
            page: Page::Loading,
            refresh: None,
            snapshot: None,
            needs: ui::attention::Presses::new(),
            pending_retries: std::collections::HashMap::new(),
            pending_cancel: None,
            discovered: None,
            picker: None,
            pending_document: None,
            print_preview: PrintPreview::default(),
            print_preview_rx: None,
            selected_printer: None,
            print_range: ui::LocalState::new(String::new()),
            print_options: os_actions::printing::PrinterOptions::default(),
            print_options_rx: None,
            print_choice: PrintChoice::default(),
            preview_page: 0,
            preview_pages: 0,
            print_copies: ui::LocalState::new(1.0),
            print_two_sided: ui::LocalState::new(defaults.two_sided),
            selected_scanner: None,
            scan: ScanState::default(),
            scan_saving: false,
            defaults,
            dispatcher: None,
            controls: None,
            bar: None,
        }
    }

    /// Where the controls send their effects and the defaults' changes.
    fn door(&self) -> Door {
        Door(self.dispatcher.clone(), self.controls.clone())
    }

    /// Ask CUPS and SANE what this machine has.
    pub(crate) fn refresh(&mut self) {
        self.page = Page::Loading;
        self.shown = Some(loading_view());
        self.refresh = request_snapshot();
    }

    /// Show `snapshot` as if a refresh had just reported it.
    pub(crate) fn show(&mut self, snapshot: Option<&Snapshot>) {
        self.refresh = None;
        self.snapshot = snapshot.cloned();
        self.rebuild_dashboard();
    }

    /// The person's defaults changed, here or on another device: the scan
    /// controls and the next document to print start from them.
    pub(crate) fn present_defaults(&mut self, defaults: &PrintDefaults) {
        if self.pending_document.is_none() {
            self.print_two_sided.set(defaults.two_sided);
        }
        self.scan.resolution.set(defaults.resolution_index());
        self.scan.color.set(defaults.color_index());
        self.scan.source.set(defaults.source_index());
        self.scan.language.set(defaults.language_index());
        self.defaults = defaults.clone();
    }

    /// Answer `effect`, from a control, the palette or an agent.
    pub(crate) fn perform(
        &mut self,
        effect: PrintCentreEffect,
        me: &Shared,
        cx: &mut EffectCx<PrintDefaults>,
    ) -> EffectAnswer {
        match effect {
            PrintCentreEffect::Refresh => self.refresh(),
            PrintCentreEffect::FindPrinters => return Ok(self.discover(me, cx)),
            effect @ (PrintCentreEffect::AddPrinter { .. }
            | PrintCentreEffect::SetDefaultPrinter { .. }
            | PrintCentreEffect::PausePrinter { .. }
            | PrintCentreEffect::ResumePrinter { .. }
            | PrintCentreEffect::CancelDocument { .. }
            | PrintCentreEffect::RetryDocument { .. }) => return self.device_job(effect, me, cx),
            PrintCentreEffect::OpenDashboard => self.open_dashboard(),
            PrintCentreEffect::OpenQueue => self.open_queue(),
            PrintCentreEffect::OpenHistory => self.open_history(),
            PrintCentreEffect::RunDiagnostics => self.open_diagnostics(),
            PrintCentreEffect::OpenNeeds => self.open_needs(),
            PrintCentreEffect::PrintDocument => self.request_document(),
            PrintCentreEffect::ScanDocument => self.open_scan(),
            PrintCentreEffect::NewScannedDocument => self.new_scanned_document()?,
            PrintCentreEffect::OpenPrinter { printer } => {
                let printer = self.known_printer(printer)?;
                self.open_device(&printer);
            }
            PrintCentreEffect::ConfirmCancel { job } => {
                self.pending_cancel = Some(self.known_job(job)?);
                self.open_queue();
            }
            PrintCentreEffect::ChoosePrinter { printer } => {
                let printer = self.known_printer(printer)?;
                // What this printer's driver reports is what the form draws:
                // a field it does not honour is not offered at all.
                self.request_print_options(&printer);
                self.selected_printer = Some(printer);
                self.rebuild_print_setup();
            }
            PrintCentreEffect::PreviousPrintPage => self.turn_preview_page(-1),
            PrintCentreEffect::NextPrintPage => self.turn_preview_page(1),
            PrintCentreEffect::SetPrintOption { option, value } => {
                self.set_print_option(&option, &value)?;
                self.rebuild_print_setup();
            }
            PrintCentreEffect::SendToPrinter => return self.send_to_printer(me, cx),
            PrintCentreEffect::ClosePrintSetup => self.close_print_setup(),
            PrintCentreEffect::ScanWith { scanner } => {
                let scanner = self.known_scanner(scanner)?;
                if self.page != Page::Scan {
                    self.open_scan();
                }
                self.selected_scanner = Some(scanner);
                self.rebuild_scan_setup();
            }
            PrintCentreEffect::ScanPage => return self.scan_page(me, cx),
            PrintCentreEffect::ScanAnother => self.scan_another(),
            PrintCentreEffect::PreviousScanPage => self.previous_scan_page(),
            PrintCentreEffect::NextScanPage => self.next_scan_page(),
            PrintCentreEffect::MoveScanPageEarlier => self.move_scan_page_earlier(),
            PrintCentreEffect::MoveScanPageLater => self.move_scan_page_later(),
            PrintCentreEffect::RemoveScanPage => self.remove_scan_page(),
            PrintCentreEffect::RotateScanPage => self.rotate_scan_page(),
            PrintCentreEffect::CropScanPage => self.crop_scan_page(),
            PrintCentreEffect::RecognizeText => return self.recognize_text(me, cx),
            PrintCentreEffect::CopyRecognizedText => self.copy_recognized_text()?,
            PrintCentreEffect::SaveScannedPage => return self.save_scan(false, me, cx),
            PrintCentreEffect::SaveScannedDocument => return self.save_scan(true, me, cx),
        }
        Ok(Effected::done())
    }

    /// Start `effect`'s work on a printer or document in the background.
    /// Its failure's Try again asks for `effect` again.
    fn device_job(
        &mut self,
        effect: PrintCentreEffect,
        me: &Shared,
        cx: &mut EffectCx<PrintDefaults>,
    ) -> EffectAnswer {
        let retry = Retry::Effect(effect.clone());
        let (success, failure, work) = match effect {
            PrintCentreEffect::AddPrinter { printer } => {
                let printer = self.known_discovery(&printer)?;
                let shown = display_name(&printer.name);
                (
                    format!("Printer added. {shown} is ready for your apps."),
                    format!("Could not add printer. Check that {shown} is awake and connected, then try again."),
                    Work::AddPrinter {
                        uri: printer.uri,
                        name: printer.name,
                    },
                )
            }
            PrintCentreEffect::SetDefaultPrinter { printer } => {
                let name = self.known_printer(printer)?;
                let shown = display_name(&name);
                (
                    format!("Preferred printer changed. New documents will use {shown} unless you choose another printer."),
                    format!("Could not change the preferred printer. {shown} is still available; try again in a moment."),
                    Work::SetDefault(name),
                )
            }
            PrintCentreEffect::PausePrinter { printer } => (
                "Printer paused. Documents will wait until you resume it.".into(),
                "Could not pause printer. Nothing changed. Check the printer connection and try again.".into(),
                Work::SetEnabled(self.known_printer(printer)?, false),
            ),
            PrintCentreEffect::ResumePrinter { printer } => (
                "Printer resumed. Waiting documents can print now.".into(),
                "Could not resume printer. Nothing changed. Check the printer connection and try again.".into(),
                Work::SetEnabled(self.known_printer(printer)?, true),
            ),
            PrintCentreEffect::CancelDocument { job } => {
                let id = self.known_job(job)?;
                self.pending_cancel = None;
                (
                    "Document canceled. It was removed from the printer.".into(),
                    "Could not cancel document. It may still print. Check the queue and try again.".into(),
                    Work::Cancel(id),
                )
            }
            PrintCentreEffect::RetryDocument { job } => (
                "Document sent again. It is back in the print queue.".into(),
                "Could not send the document again. Check the printer, then try again.".into(),
                Work::RetryJob(self.known_job(job)?),
            ),
            _ => return Err(format!("`{}` is not printer work", effect.id())),
        };
        Ok(Self::run(me, cx, success, failure, work, retry))
    }

    /// The scan and print steps that have nothing to act on right now.
    pub(crate) fn effect_statuses(&self) -> Vec<EffectStatus> {
        const NO_DOCUMENT: &str = "Choose a document to print first.";
        let mut statuses = Vec::new();
        if let Some(reason) = self.scan_reset_blocked() {
            statuses.push(EffectStatus::unavailable(
                PrintCentreEffect::NEW_SCANNED_DOCUMENT,
                "scan-busy",
                reason,
            ));
        }
        if self.pending_document.is_none() {
            statuses.push(EffectStatus::unavailable(
                PrintCentreEffect::CHOOSE_PRINTER,
                "no-document",
                NO_DOCUMENT,
            ));
            for effect in [
                PrintCentreEffect::SEND_TO_PRINTER,
                PrintCentreEffect::CLOSE_PRINT_SETUP,
            ] {
                statuses.push(EffectStatus::unavailable(
                    effect,
                    "no-document",
                    NO_DOCUMENT,
                ));
            }
        } else if self.selected_printer.is_none() {
            statuses.push(EffectStatus::unavailable(
                PrintCentreEffect::SEND_TO_PRINTER,
                "no-printer",
                "Choose a printer first.",
            ));
        }
        if self.page != Page::Scan || self.selected_scanner.is_none() {
            statuses.push(EffectStatus::unavailable(
                PrintCentreEffect::SCAN_PAGE,
                "no-scanner",
                "Open scanning with a scanner first.",
            ));
        } else if self.scan.in_progress {
            statuses.push(EffectStatus::unavailable(
                PrintCentreEffect::SCAN_PAGE,
                "scanning",
                "A page is being scanned.",
            ));
        }
        if !self
            .nearby()
            .is_some_and(|found| found.is_ok_and(|found| !found.is_empty()))
        {
            statuses.push(EffectStatus::unavailable(
                PrintCentreEffect::ADD_PRINTER,
                "nothing-found",
                "Find network printers first.",
            ));
        }
        self.scan_review_statuses(&mut statuses);
        statuses
    }

    /// The steps on a scanned page, unavailable while there is none to review
    /// or while the scan is being saved.
    fn scan_review_statuses(&self, statuses: &mut Vec<EffectStatus>) {
        const NO_PAGE: &str = "Scan a page first.";
        let page = self.scan.result.as_ref();
        let reviewing = self.page == Page::Scan && page.is_some();
        let first = self.scan.selected == 0;
        let last = self.scan.selected.saturating_add(1) >= self.scan.pages.len();
        for (effect, available) in [
            (PrintCentreEffect::SCAN_ANOTHER, reviewing),
            (PrintCentreEffect::PREVIOUS_SCAN_PAGE, reviewing && !first),
            (PrintCentreEffect::NEXT_SCAN_PAGE, reviewing && !last),
            (
                PrintCentreEffect::MOVE_SCAN_PAGE_EARLIER,
                reviewing && !first,
            ),
            (PrintCentreEffect::MOVE_SCAN_PAGE_LATER, reviewing && !last),
            (PrintCentreEffect::REMOVE_SCAN_PAGE, reviewing),
            (PrintCentreEffect::ROTATE_SCAN_PAGE, reviewing),
            (PrintCentreEffect::CROP_SCAN_PAGE, reviewing),
            (
                PrintCentreEffect::RECOGNIZE_TEXT,
                reviewing && page.is_some_and(|page| !page.ocr_in_progress),
            ),
            (
                PrintCentreEffect::COPY_RECOGNIZED_TEXT,
                reviewing
                    && page
                        .and_then(|page| page.recognized_text.as_deref())
                        .is_some_and(|text| !text.trim().is_empty()),
            ),
            (PrintCentreEffect::SAVE_SCANNED_PAGE, reviewing),
            (PrintCentreEffect::SAVE_SCANNED_DOCUMENT, reviewing),
        ] {
            if !available {
                statuses.push(EffectStatus::unavailable(effect, "no-page", NO_PAGE));
            } else if self.scan_saving
                && (effect.id == PrintCentreEffect::SAVE_SCANNED_PAGE.id
                    || effect.id == PrintCentreEffect::SAVE_SCANNED_DOCUMENT.id)
            {
                statuses.push(EffectStatus::unavailable(
                    effect,
                    "saving",
                    "The scan is being saved.",
                ));
            }
        }
    }

    fn known_printer(&self, printer: String) -> Result<String, String> {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.queues.as_ref().ok())
            .is_some_and(|queues| queues.iter().any(|queue| queue.name == printer))
            .then_some(printer)
            .ok_or_else(|| "Print Centre does not know that printer".to_owned())
    }

    fn known_job(&self, job: String) -> Result<String, String> {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.jobs.as_ref().ok())
            .is_some_and(|jobs| jobs.iter().any(|known| known.id == job))
            .then_some(job)
            .ok_or_else(|| "Print Centre does not know that document".to_owned())
    }

    fn known_scanner(&self, scanner: String) -> Result<String, String> {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.scanners.as_ref().ok())
            .is_some_and(|scanners| scanners.iter().any(|known| known.device == scanner))
            .then_some(scanner)
            .ok_or_else(|| "Print Centre does not know that scanner".to_owned())
    }

    fn known_discovery(
        &self,
        uri: &str,
    ) -> Result<os_actions::printing::DiscoveredPrinter, String> {
        self.nearby()
            .and_then(Result::ok)
            .and_then(|found| found.into_iter().find(|printer| printer.uri == uri))
            .ok_or_else(|| "That printer is already added or wasn't found nearby".to_owned())
    }

    /// The printers the last search found that no queue prints to yet:
    /// only those can be added, so only those are offered.
    fn nearby(&self) -> Option<Result<Vec<os_actions::printing::DiscoveredPrinter>, String>> {
        let queues = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.queues.as_ref().ok())
            .map_or(&[][..], Vec::as_slice);
        self.discovered.as_ref().map(|found| {
            found
                .as_ref()
                .map(|found| os_actions::printing::not_added(found, queues))
                .map_err(Clone::clone)
        })
    }

    /// Open the page screenshot `scenario` shows, the way its effects do.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        match scenario {
            Some("queue") => self.open_queue(),
            Some("history") => self.open_history(),
            Some("diagnostics") => self.open_diagnostics(),
            Some("device") => self.open_device("Studio_printer"),
            Some("discovery") => {
                self.discovered = Some(Ok(demo_discovered_printers()));
                self.open_discovery();
            }
            Some("print" | "print-narrow") => {
                self.pending_document = Some(PathBuf::from("Travel itinerary.pdf"));
                self.selected_printer = Some("Studio_printer".into());
                // A forty-page document, so the pager and the page range are
                // part of what a person sees rather than a claim in a comment.
                self.preview_pages = 40;
                self.print_preview = demo_print_preview();
                // What this printer's driver reports, in place of asking the
                // machine the capture runs on.
                self.print_options = demo_printer_options();
                self.print_range.set(String::new());
                self.print_two_sided.set(self.defaults.two_sided);
                self.rebuild_print_setup();
            }
            Some("scan") => self.open_scan(),
            Some("scan-result" | "scan-result-narrow") => {
                self.open_scan();
                self.scan_finished(Ok(demo_scanned_page()));
            }
            _ => {}
        }
    }

    /// Printers, documents and scanners from the last CUPS/SANE snapshot,
    /// and the printers the last search found nearby.
    pub(crate) fn device_rows(&self) -> Vec<LiveRow> {
        let mut rows = Vec::new();
        if let Some(snapshot) = &self.snapshot {
            if let Ok(queues) = &snapshot.queues {
                rows.extend(queues.iter().map(printer_row));
            }
            if let Ok(jobs) = &snapshot.jobs {
                rows.extend(jobs.iter().map(job_row));
            }
            if let Ok(scanners) = &snapshot.scanners {
                rows.extend(scanners.iter().map(scanner_row));
            }
        }
        if let Some(Ok(found)) = &self.nearby() {
            rows.extend(found.iter().map(discovered_row));
        }
        rows
    }

    fn request_document(&mut self) {
        if self.picker.is_some() {
            return;
        }
        self.print_copies.set(1.0);
        self.print_two_sided.set(self.defaults.two_sided);
        self.print_preview = PrintPreview::Unavailable;
        self.print_preview_rx = None;
        self.picker = Some(
            ui::portal::PickRequest::open("Choose a document to print", "Choose")
                .thread_name("print-centre-document-picker")
                .filter(
                    ui::portal::FileFilter::new("Documents and images")
                        .mimetype("application/pdf")
                        .mimetype("image/png")
                        .mimetype("image/jpeg")
                        .mimetype("text/plain"),
                )
                .all_files()
                .send(),
        );
    }

    /// Draw `path`'s `page` in the card, asking a worker for it.
    fn request_print_preview(&mut self, path: PathBuf, page: usize) {
        let (tx, rx) = mpsc::channel();
        self.print_preview = PrintPreview::Loading;
        self.print_preview_rx = Some(rx);
        let _ = std::thread::Builder::new()
            .name("tilo-print-preview".into())
            .spawn(move || {
                let _ = tx.send(load_print_preview(&path, page));
            });
    }

    /// Show the next or previous page of the document being set up, when
    /// there is one either side.
    fn turn_preview_page(&mut self, step: i32) {
        let Some(path) = self.pending_document.clone() else {
            return;
        };
        let pages = self.preview_pages;
        if pages <= 1 {
            return;
        }
        let last = pages.saturating_sub(1);
        let current = self.preview_page.min(last);
        let next = if step < 0 {
            current.saturating_sub(1)
        } else {
            current.saturating_add(1).min(last)
        };
        if next == current {
            return;
        }
        self.preview_page = next;
        self.request_print_preview(path, next);
    }

    /// Choose one of the driver's options for the next document, or refuse a
    /// value it never reported.
    fn set_print_option(&mut self, option: &str, value: &str) -> Result<(), String> {
        let offered = match option {
            "page-size" => (
                &self.print_options.page_sizes,
                &mut self.print_choice.page_size,
            ),
            "colour" => (
                &self.print_options.colour_modes,
                &mut self.print_choice.colour,
            ),
            "two-sided" => (
                &self.print_options.duplex_modes,
                &mut self.print_choice.duplex,
            ),
            "orientation" => (
                &self.print_options.orientations,
                &mut self.print_choice.orientation,
            ),
            other => return Err(format!("{other} is not an option this printer reports")),
        };
        if os_actions::printing::PrinterOptions::choice(offered.0, value).is_none() {
            return Err(format!("this printer does not offer {value}"));
        }
        *offered.1 = Some(value.to_owned());
        Ok(())
    }

    /// Ask `queue`'s driver what it can do, off this thread.
    fn request_print_options(&mut self, queue: &str) {
        let (tx, rx) = mpsc::channel();
        self.print_options_rx = Some(rx);
        let queue = queue.to_owned();
        let _ = std::thread::Builder::new()
            .name("tilo-print-options".into())
            .spawn(move || {
                let reported = os_actions::printing::printer_options(&queue).unwrap_or_default();
                let _ = tx.send(reported);
            });
    }

    /// Take up what the driver said, if it has answered: the form then draws
    /// only the options this printer reports.
    fn poll_print_options(&mut self) -> bool {
        let Some(receiver) = self.print_options_rx.as_ref() else {
            return false;
        };
        let reported = match receiver.try_recv() {
            Ok(reported) => reported,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                os_actions::printing::PrinterOptions::default()
            }
        };
        self.print_options_rx = None;
        self.take_print_options(reported);
        self.rebuild_print_setup();
        true
    }

    /// Adopt what the driver reported: the choices it no longer offers fall
    /// back to its own default.
    fn take_print_options(&mut self, reported: os_actions::printing::PrinterOptions) {
        self.print_options = reported;
        // A choice the new printer does not offer falls back to its default.
        let kept = |choice: &Option<String>, offered: &[os_actions::printing::OptionChoice]| {
            choice
                .as_deref()
                .filter(|key| os_actions::printing::PrinterOptions::choice(offered, key).is_some())
                .map(str::to_owned)
        };
        self.print_choice = PrintChoice {
            page_size: kept(&self.print_choice.page_size, &self.print_options.page_sizes),
            colour: kept(&self.print_choice.colour, &self.print_options.colour_modes),
            duplex: kept(&self.print_choice.duplex, &self.print_options.duplex_modes),
            orientation: kept(
                &self.print_choice.orientation,
                &self.print_options.orientations,
            ),
        };
    }

    fn poll_print_preview(&mut self) -> bool {
        let Some(receiver) = self.print_preview_rx.as_ref() else {
            return false;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        };
        self.print_preview_rx = None;
        self.print_preview = result.map_or(PrintPreview::Unavailable, |preview| {
            self.preview_page = preview.page;
            self.preview_pages = preview.pages;
            PrintPreview::Ready {
                rgba: Arc::from(preview.rgba),
                width: preview.width,
                height: preview.height,
                page: preview.page,
                pages: preview.pages,
            }
        });
        self.rebuild_print_setup();
        true
    }

    fn poll_picker(&mut self) -> bool {
        let Some(receiver) = self.picker.as_ref() else {
            return false;
        };
        let outcome = match receiver.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                ui::portal::PortalOutcome::Failed("The file picker stopped responding".into())
            }
        };
        self.picker = None;
        match outcome {
            ui::portal::PortalOutcome::Opened(path) => {
                self.pending_document = Some(path);
                self.selected_printer = self.snapshot.as_ref().and_then(|snapshot| {
                    snapshot.queues.as_ref().ok().and_then(|queues| {
                        queues
                            .iter()
                            .find(|queue| queue.default)
                            .or_else(|| queues.first())
                            .map(|queue| queue.name.clone())
                    })
                });
                if let Some(path) = self.pending_document.clone() {
                    self.request_print_preview(path, 0);
                }
                if let Some(queue) = self.selected_printer.clone() {
                    self.request_print_options(&queue);
                }
                self.rebuild_print_setup();
            }
            ui::portal::PortalOutcome::Cancelled => {}
            ui::portal::PortalOutcome::Failed(_) => {
                self.tell(OperationNotice::failure(
                    "Could not open the file picker. Try again, or print from the document's app.",
                    Retry::Effect(PrintCentreEffect::PrintDocument),
                ));
                self.rebuild_dashboard();
            }
        }
        true
    }

    fn rebuild_print_setup(&mut self) {
        if let (Some(snapshot), Some(path)) = (&self.snapshot, &self.pending_document) {
            self.page = Page::Print;
            self.shown = Some(print_setup_view(
                snapshot,
                path,
                &self.print_preview,
                self.selected_printer.as_deref(),
                &self.print_copies,
                &self.print_two_sided,
                &PrintForm {
                    copies: &self.print_copies,
                    two_sided: &self.print_two_sided,
                    range: &self.print_range,
                    options: &self.print_options,
                    choice: &self.print_choice,
                    pages: self.preview_pages,
                },
                &Door(self.dispatcher.clone(), self.controls.clone()),
            ));
        }
    }

    fn rebuild_scan_setup(&mut self) {
        if let Some(snapshot) = &self.snapshot {
            self.page = Page::Scan;
            self.shown = Some(scan_setup_view(
                snapshot,
                self.selected_scanner.as_deref(),
                &self.scan,
                &Door(self.dispatcher.clone(), self.controls.clone()),
            ));
        }
    }

    fn rebuild_dashboard(&mut self) {
        self.page = Page::Dashboard;
        let door = self.door();
        self.shown = Some(
            self.snapshot
                .as_ref()
                .map_or_else(|| empty_view(&door), |snapshot| live_view(snapshot, &door)),
        );
    }

    fn open_dashboard(&mut self) {
        self.rebuild_dashboard();
    }

    fn open_queue(&mut self) {
        let door = self.door();
        if let Some(snapshot) = &self.snapshot {
            self.page = Page::Queue;
            self.shown = Some(queue_view(snapshot, &door, self.pending_cancel.as_deref()));
        }
    }

    fn open_history(&mut self) {
        let door = self.door();
        if let Some(snapshot) = &self.snapshot {
            self.page = Page::History;
            self.shown = Some(history_view(snapshot, &door));
        }
    }

    /// What wants a person: the system's waiting stack, in this window.
    fn open_needs(&mut self) {
        self.page = Page::Needs;
        let body = ui::widget::Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(ui::attention::Stack::with_presses(
                self.needs.clone(),
            )));
        self.shown = Some(Box::new(
            ui::widget::Scrollable::new_vertical().with_child(Box::new(body)),
        ));
    }

    fn open_diagnostics(&mut self) {
        let door = self.door();
        if let Some(snapshot) = &self.snapshot {
            self.page = Page::Diagnostics;
            self.shown = Some(diagnostics_view(snapshot, &door));
        }
    }

    fn open_device(&mut self, name: &str) {
        let door = self.door();
        if let Some(queue) = self
            .snapshot
            .as_ref()
            .and_then(|s| s.queues.as_ref().ok())
            .and_then(|queues| queues.iter().find(|q| q.name == name))
        {
            self.page = Page::Device(name.to_owned());
            self.shown = Some(device_view(queue, &door));
        }
    }

    fn open_discovery(&mut self) {
        self.page = Page::Discovery;
        let nearby = self.nearby();
        self.shown = Some(discovery_view(nearby.as_ref(), &self.door()));
    }

    /// Search the network for printers, showing the search while it runs.
    fn discover(&mut self, me: &Shared, cx: &mut EffectCx<PrintDefaults>) -> Effected {
        self.discovered = None;
        self.open_discovery();
        let me = Rc::clone(me);
        cx.thread(
            |_| os_actions::printing::discover_driverless(),
            move |(), found: Result<_, String>, _cx| {
                let mut app = me.borrow_mut();
                let answer = match &found {
                    Ok(_) => Ok(Effected::done()),
                    Err(_) => Err("Could not search for printers right now".to_owned()),
                };
                app.discovered = Some(found);
                if app.page == Page::Discovery {
                    app.open_discovery();
                }
                answer
            },
        )
    }

    /// Run `work` on a printer or document in the background, then say how
    /// it went, `success` or `failure` (with Try again asking for `retry`),
    /// and show what CUPS reports afterwards.
    fn run(
        me: &Shared,
        cx: &mut EffectCx<PrintDefaults>,
        success: String,
        failure: String,
        work: Work,
        retry: Retry,
    ) -> Effected {
        let job = notice::job_for(&work);
        #[cfg(test)]
        let stand_in = notice::stand_in::current();
        let me = Rc::clone(me);
        cx.thread(
            move |_| {
                #[cfg(test)]
                if let Some(devices) = stand_in {
                    return notice::stand_in::answer(&devices, format!("{work:?}"), || ());
                }
                work.perform()
            },
            move |(), result: Result<(), String>, _cx| {
                let mut app = me.borrow_mut();
                if result.is_ok() {
                    job.done(success.clone());
                    app.finished(OperationNotice::success(success));
                    Ok(Effected::done())
                } else {
                    job.failed(failure.clone());
                    app.finished(OperationNotice::failure(failure.clone(), retry));
                    Err(failure)
                }
            },
        )
    }

    /// Work in the background finished, as `notice` says.
    fn finished(&mut self, notice: OperationNotice) {
        self.tell(notice);
        if self.scan.result.is_some() {
            // Saving a scan is not a printer-state mutation. Keep the page
            // and its OCR visible so the person can confirm success, save
            // another copy, or scan the next page without losing their
            // place.
            self.rebuild_scan_setup();
        } else {
            self.refresh = request_snapshot();
        }
    }

    /// Print the document being set up on the chosen printer.
    fn send_to_printer(&mut self, me: &Shared, cx: &mut EffectCx<PrintDefaults>) -> EffectAnswer {
        let (Some(path), Some(queue)) =
            (self.pending_document.take(), self.selected_printer.take())
        else {
            return Err("Choose a document and a printer first".into());
        };
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Document")
            .to_string();
        let shown_queue = display_name(&queue);
        // The typed range is read back here, so a range nobody can print is
        // refused before the document leaves the window.
        let typed = self.print_range.get();
        let page_range = if typed.trim().is_empty() {
            None
        } else {
            match os_actions::printing::PageRange::parse(&typed) {
                Ok(range) => Some(range),
                Err(reason) => {
                    self.pending_document = Some(path);
                    self.selected_printer = Some(queue);
                    self.rebuild_print_setup();
                    return Err(reason);
                }
            }
        };
        let chosen_copies = self.print_copies.get();
        let copies = format!("{:.0}", chosen_copies.clamp(1.0, 99.0))
            .parse::<u16>()
            .unwrap_or(1);
        let two_sided = self.print_two_sided.get();
        let settings = os_actions::printing::PrintSettings {
            copies,
            two_sided,
            page_range,
            page_size: self.print_choice.page_size.clone(),
            colour: self.print_choice.colour.clone(),
            duplex: self.print_choice.duplex.clone(),
            orientation: self.print_choice.orientation.clone(),
        };
        self.print_preview = PrintPreview::Unavailable;
        self.print_preview_rx = None;
        let retry = Retry::Print {
            path: path.clone(),
            printer: queue.clone(),
            settings: settings.clone(),
        };
        Ok(Self::run(
            me,
            cx,
            format!("Document sent to printer. {filename} was sent to {shown_queue}."),
            format!(
                "Could not print document. {filename} was not sent. Check the printer and try again."
            ),
            Work::Print {
                printer: queue,
                path,
                settings,
            },
            retry,
        ))
    }

    fn close_print_setup(&mut self) {
        self.pending_document = None;
        self.print_preview = PrintPreview::Unavailable;
        self.print_preview_rx = None;
        self.selected_printer = None;
        self.print_copies.set(1.0);
        self.print_range.set(String::new());
        self.print_options = os_actions::printing::PrinterOptions::default();
        self.print_options_rx = None;
        self.print_choice = PrintChoice::default();
        self.preview_page = 0;
        self.preview_pages = 0;
        self.print_two_sided.set(self.defaults.two_sided);
        self.rebuild_dashboard();
    }

    /// Return to the scanned document without discarding unsaved pages or
    /// forgetting a scan that is still running.
    fn open_scan(&mut self) {
        if self.selected_scanner.is_none() {
            self.selected_scanner = self.snapshot.as_ref().and_then(|snapshot| {
                snapshot
                    .scanners
                    .as_ref()
                    .ok()
                    .and_then(|scanners| scanners.first())
                    .map(|scanner| scanner.device.clone())
            });
        }
        self.rebuild_scan_setup();
    }

    fn scan_reset_blocked(&self) -> Option<&'static str> {
        if self.scan.in_progress {
            Some("Wait for the page to finish scanning before starting a new document.")
        } else if self.scan_saving {
            Some("Wait for the scan to finish saving before starting a new document.")
        } else {
            None
        }
    }

    /// The host confirms this destructive action before delivering it.
    /// Navigation and Scan another page never take this path.
    fn new_scanned_document(&mut self) -> Result<(), String> {
        if let Some(reason) = self.scan_reset_blocked() {
            return Err(reason.into());
        }
        self.scan.pages.clear();
        self.scan.result = None;
        self.scan.selected = 0;
        self.scan.crop.set(CropSelection::default());
        self.open_scan();
        Ok(())
    }

    /// Scan a page with the chosen scanner and settings, recognizing its
    /// text in the person's language.
    fn scan_page(&mut self, me: &Shared, cx: &mut EffectCx<PrintDefaults>) -> EffectAnswer {
        let Some(device) = self.selected_scanner.clone() else {
            return Err("Choose a scanner first".into());
        };
        if self.scan.in_progress {
            return Err("A page is being scanned".into());
        }
        let resolution_dpi = SCAN_RESOLUTIONS
            .get(self.scan.resolution.get())
            .copied()
            .unwrap_or(300);
        let color = match self.scan.color.get() {
            1 => tilo_capture::scanner::ScanColor::Grayscale,
            2 => tilo_capture::scanner::ScanColor::LineArt,
            _ => tilo_capture::scanner::ScanColor::Color,
        };
        let source = match self.scan.source.get() {
            1 => tilo_capture::scanner::ScanSource::Feeder,
            2 => tilo_capture::scanner::ScanSource::FeederDuplex,
            _ => tilo_capture::scanner::ScanSource::Flatbed,
        };
        let (ocr_code, ocr_language) = match self.scan.language.get() {
            1 => ("deu", "German"),
            2 => ("fra", "French"),
            3 => ("spa", "Spanish"),
            _ => ("eng", "English"),
        };
        self.scan.in_progress = true;
        self.rebuild_scan_setup();
        let job = ui::jobs::start(
            ui::jobs::Spec::new("Scanning a page")
                .subject(ui::jobs::Subject::new(
                    notice::owner(),
                    display_name(&device),
                ))
                .unit(ui::jobs::Unit::Steps)
                .phase("Scanning"),
        );
        let scanner = device.clone();
        #[cfg(test)]
        let stand_in = notice::stand_in::current();
        let me = Rc::clone(me);
        Ok(cx.thread(
            move |_| {
                #[cfg(test)]
                if let Some(devices) = stand_in {
                    return notice::stand_in::answer(
                        &devices,
                        format!("scan {device}"),
                        demo_scanned_page,
                    );
                }
                tilo_capture::scanner::scan_page(
                    &device,
                    tilo_capture::scanner::ScanSettings {
                        resolution_dpi,
                        color,
                        source,
                    },
                )
                .and_then(|png| decode_scanned_page(png, ocr_code, ocr_language))
            },
            move |(), result: Result<ScannedPage, String>, _cx| {
                let mut app = me.borrow_mut();
                let answer = match &result {
                    Ok(_) => {
                        job.done("Page scanned");
                        Ok(Effected::done())
                    }
                    Err(error) => {
                        job.failed(error.clone());
                        Err(error.clone())
                    }
                };
                app.scan_finished(result.map_err(|_| Retry::Scan { scanner }));
                answer
            },
        ))
    }

    /// A page scanned, or a scan that failed, which `retry` asks for again.
    fn scan_finished(&mut self, result: Result<ScannedPage, Retry>) {
        self.scan.in_progress = false;
        match result {
            Ok(page) => {
                self.scan.pages.push(page.clone());
                self.scan.selected = self.scan.pages.len().saturating_sub(1);
                self.scan.result = Some(page);
                self.scan.crop.set(CropSelection::default());
            }
            Err(retry) => self.tell(OperationNotice::failure(
                "Could not scan this page. Check that the scanner is connected and the paper is positioned, then try again.",
                retry,
            )),
        }
        self.rebuild_scan_setup();
    }

    /// Show the page now selected, with a fresh crop frame.
    fn show_selected_scan_page(&mut self) {
        self.scan.result = self.scan.pages.get(self.scan.selected).cloned();
        self.scan.crop.set(CropSelection::default());
        self.rebuild_scan_setup();
    }

    fn scan_another(&mut self) {
        self.scan.result = None;
        self.rebuild_scan_setup();
    }

    fn previous_scan_page(&mut self) {
        self.scan.selected = self.scan.selected.saturating_sub(1);
        self.show_selected_scan_page();
    }

    fn next_scan_page(&mut self) {
        if self.scan.selected.saturating_add(1) < self.scan.pages.len() {
            self.scan.selected = self.scan.selected.saturating_add(1);
        }
        self.show_selected_scan_page();
    }

    fn move_scan_page_earlier(&mut self) {
        if self.scan.selected > 0 {
            let previous = self.scan.selected.saturating_sub(1);
            self.scan.pages.swap(self.scan.selected, previous);
            self.scan.selected = previous;
        }
        self.show_selected_scan_page();
    }

    fn move_scan_page_later(&mut self) {
        let next = self.scan.selected.saturating_add(1);
        if next < self.scan.pages.len() {
            self.scan.pages.swap(self.scan.selected, next);
            self.scan.selected = next;
        }
        self.show_selected_scan_page();
    }

    fn remove_scan_page(&mut self) {
        if self.scan.selected < self.scan.pages.len() {
            self.scan.pages.remove(self.scan.selected);
        }
        self.scan.selected = self
            .scan
            .selected
            .min(self.scan.pages.len().saturating_sub(1));
        self.show_selected_scan_page();
    }

    fn rotate_scan_page(&mut self) {
        if let Some(page) = self.scan.pages.get_mut(self.scan.selected) {
            match page.rotate_clockwise() {
                Ok(()) => {
                    self.scan.result = Some(page.clone());
                }
                Err(message) => self.tell(OperationNotice::failure_only(format!(
                    "Could not rotate this page: {message}"
                ))),
            }
        }
        self.rebuild_scan_setup();
    }

    fn crop_scan_page(&mut self) {
        let crop = self.scan.crop.get();
        if crop == CropSelection::default() {
            return;
        }
        if let Some(page) = self.scan.pages.get_mut(self.scan.selected) {
            match page.crop_to(crop) {
                Ok(()) => {
                    self.scan.result = Some(page.clone());
                    self.scan.crop.set(CropSelection::default());
                }
                Err(message) => self.tell(OperationNotice::failure_only(format!(
                    "Could not crop this page: {message}"
                ))),
            }
        }
        self.rebuild_scan_setup();
    }

    /// Recognize the text of the page under review again.
    fn recognize_text(&mut self, me: &Shared, cx: &mut EffectCx<PrintDefaults>) -> EffectAnswer {
        let Some(current) = self.scan.pages.get_mut(self.scan.selected) else {
            return Err("Scan a page first".into());
        };
        if current.ocr_in_progress {
            return Err("The text is already being recognized".into());
        }
        current.ocr_in_progress = true;
        let page = current.clone();
        self.scan.result = Some(page.clone());
        self.rebuild_scan_setup();
        let identity = page.png.clone();
        let job = ui::jobs::start(
            ui::jobs::Spec::new("Reading the text on this page")
                .subject(ui::jobs::Subject::new(notice::owner(), "Scanned page"))
                .unit(ui::jobs::Unit::Steps)
                .phase("Reading"),
        );
        #[cfg(test)]
        let stand_in = notice::stand_in::current();
        let me = Rc::clone(me);
        Ok(cx.thread(
            move |_| {
                #[cfg(test)]
                if let Some(devices) = stand_in {
                    return notice::stand_in::answer(&devices, "recognize".into(), || {
                        (
                            Some("Stand-in text".to_owned()),
                            "Recognized 2 words.".to_owned(),
                        )
                    });
                }
                recognize_page(&page)
            },
            move |(), result: Result<(Option<String>, String), String>, _cx| {
                let mut app = me.borrow_mut();
                let answer = match &result {
                    Ok((_, note)) => {
                        job.done(note.clone());
                        Ok(Effected::done())
                    }
                    Err(error) => {
                        job.failed(error.clone());
                        Err(error.clone())
                    }
                };
                app.recognition_finished(&identity, result);
                answer
            },
        ))
    }

    fn recognition_finished(
        &mut self,
        identity: &Arc<[u8]>,
        result: Result<(Option<String>, String), String>,
    ) {
        let Some(index) = self
            .scan
            .pages
            .iter()
            .position(|page| Arc::ptr_eq(&page.png, identity))
        else {
            return;
        };
        match result {
            Ok((text, note)) => {
                let Some(page) = self.scan.pages.get_mut(index) else {
                    return;
                };
                page.recognized_text = text;
                page.ocr_note = note;
                page.ocr_needs_refresh = false;
                page.ocr_in_progress = false;
                if index == self.scan.selected {
                    self.scan.result = Some(page.clone());
                }
            }
            Err(message) => {
                if let Some(page) = self.scan.pages.get_mut(index) {
                    page.ocr_in_progress = false;
                }
                if index == self.scan.selected {
                    self.scan.result = self.scan.pages.get(index).cloned();
                }
                self.tell(OperationNotice::failure(
                    format!("Text was not updated. {message}"),
                    Retry::Recognize {
                        page: identity.clone(),
                    },
                ));
            }
        }
        self.rebuild_scan_setup();
    }

    fn copy_recognized_text(&mut self) -> Result<(), String> {
        let text = self
            .scan
            .result
            .as_ref()
            .and_then(|page| page.recognized_text.as_deref())
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| "This page has no recognized text".to_owned())?;
        ui::text_selection::clipboard_copy(text);
        self.tell(OperationNotice::success(
            "Text copied. Paste the recognized text into any app.",
        ));
        self.rebuild_scan_setup();
        Ok(())
    }

    /// Ask where to save the page under review as an image, or every
    /// scanned page as one PDF (`document`), and save it there.
    fn save_scan(
        &mut self,
        document: bool,
        me: &Shared,
        cx: &mut EffectCx<PrintDefaults>,
    ) -> EffectAnswer {
        if self.scan_saving {
            return Err("The scan is already being saved".into());
        }
        let pages = if document {
            self.scan.pages.clone()
        } else {
            self.scan.result.iter().cloned().collect()
        };
        let Some(page) = self
            .reviewed_page()
            .or_else(|| pages.first().map(|page| page.png.clone()))
        else {
            return Err("Scan a page first".into());
        };
        let retry = Retry::Save { document, page };
        self.scan_saving = true;
        let job = ui::jobs::start(
            ui::jobs::Spec::new(if document {
                "Saving the scanned document"
            } else {
                "Saving the scanned page"
            })
            .subject(ui::jobs::Subject::new(notice::owner(), "Scanned pages"))
            .unit(ui::jobs::Unit::Steps)
            .phase("Writing the file"),
        );
        let request = if document {
            ui::portal::PickRequest::save("Save scanned document", "Save", "Scanned document.pdf")
                .thread_name("print-centre-scan-document-save-picker")
        } else {
            ui::portal::PickRequest::save("Save scanned page", "Save", "Scanned page.png")
                .thread_name("print-centre-scan-save-picker")
        };
        // Asked here, on the window's thread, from the desktop's chooser.
        let chooser = request.send();
        let me = Rc::clone(me);
        Ok(cx.thread(
            move |_| write_scan(&chooser, document, &pages),
            move |(), saved: ScanSaved, _cx| {
                let mut app = me.borrow_mut();
                app.scan_saving = false;
                match &saved {
                    ScanSaved::Saved(name) => job.done(format!("The scan is in {name}")),
                    ScanSaved::Cancelled => job.stopped(),
                    ScanSaved::NoPicker | ScanSaved::Failed => {
                        job.failed("The scan was not saved");
                    }
                }
                let (notice, answer) = match saved {
                    ScanSaved::Saved(name) => (
                        OperationNotice::success(if document {
                            format!("Scanned document saved as {name}.")
                        } else {
                            format!("Scanned page saved as {name}.")
                        }),
                        Ok(Effected::done()),
                    ),
                    ScanSaved::Cancelled => return Ok(Effected::done()),
                    ScanSaved::NoPicker => (
                        OperationNotice::failure(
                            "Could not open the file picker. Try saving the scan again.",
                            retry,
                        ),
                        Err("Could not open the file picker".into()),
                    ),
                    ScanSaved::Failed => (
                        OperationNotice::failure(
                            if document {
                                "Could not save scanned document. Choose another location and try again."
                            } else {
                                "Could not save scanned page. Choose another location and try again."
                            },
                            retry,
                        ),
                        Err("Could not save the scan there".into()),
                    ),
                };
                app.tell(notice);
                app.rebuild_scan_setup();
                answer
            },
        ))
    }
}

/// One print-centre test at a time: the waiting stack and the ledger are the
/// process's, so two running at once would see each other's rows.
#[cfg(test)]
pub(crate) fn serial() -> std::sync::MutexGuard<'static, ()> {
    notice::serial()
}

/// A page the scanner returned as `png`, with its text recognized in
/// `ocr_code` (named `ocr_language`).
fn decode_scanned_page(
    png: Vec<u8>,
    ocr_code: &str,
    ocr_language: &str,
) -> Result<ScannedPage, String> {
    let decoded = image::load_from_memory(&png)
        .map_err(|error| format!("scanner returned an invalid image: {error}"))?;
    let width = decoded.width();
    let height = decoded.height();
    let rgba: Arc<[u8]> = Arc::from(decoded.to_rgba8().into_raw());
    let (recognized_text, ocr_note) = match tilo_ocr::recognize(&decoded, ocr_code) {
        Ok(document) if document.words.is_empty() => {
            (None, "No text was found on this page.".into())
        }
        Ok(document) => {
            let count = document.words.len();
            (
                Some(document.plain_text()),
                format!("Recognized {count} words."),
            )
        }
        Err(tilo_ocr::Error::Unavailable) => (
            None,
            "Text recognition is not installed; the scanned page is still ready.".into(),
        ),
        Err(_) => (
            None,
            "Text could not be recognized; the scanned page is still ready.".into(),
        ),
    };
    Ok(ScannedPage {
        png: Arc::from(png),
        rgba,
        width,
        height,
        recognized_text,
        ocr_note,
        ocr_language: ocr_language.into(),
        ocr_needs_refresh: false,
        ocr_in_progress: false,
    })
}

/// `page`'s text, recognized again in the language it was scanned in, with
/// a note on what was found.
fn recognize_page(page: &ScannedPage) -> Result<(Option<String>, String), String> {
    let rgba = image::RgbaImage::from_raw(page.width, page.height, page.rgba.to_vec())
        .ok_or_else(|| "The scanned page pixels were incomplete.".to_string())?;
    let code = match page.ocr_language.as_str() {
        "German" => "deu",
        "French" => "fra",
        "Spanish" => "spa",
        _ => "eng",
    };
    tilo_ocr::recognize(&image::DynamicImage::ImageRgba8(rgba), code)
        .map(|document| {
            let count = document.words.len();
            let text = document.plain_text();
            let recognized = (!text.trim().is_empty()).then_some(text);
            let note = if count == 0 {
                "No text was found on this page.".into()
            } else {
                format!("Recognized {count} words.")
            };
            (recognized, note)
        })
        .map_err(|_| "Text could not be recognized on this page.".into())
}
impl Default for PrintCentre {
    fn default() -> Self {
        Self::new()
    }
}
impl PrintCentre {
    /// Take part in the resource: the controls and the window bar run their
    /// effects through its dispatcher. A live window asks CUPS and SANE; a
    /// screenshot shows its scenario's made-up devices, or none.
    fn attach(&mut self, cx: &ViewCx<PrintDefaults>) {
        self.controls = Some(cx.actions());
        self.dispatcher = Some(cx.dispatcher());
        self.bar = Some(ui::framework::BarActions::new(cx.dispatcher()));
        match cx.mode() {
            RunMode::Live => self.refresh(),
            RunMode::Capture => self.show(cx.fixture::<Snapshot>().as_deref()),
            RunMode::Past => self.show(None),
        }
    }

    /// What the window bar and Needs you asked, and what came back from the
    /// document picker, the preview, the driver and CUPS since the last
    /// update. Returns whether the page needs its update (`Some(true)`) or
    /// a sync (`Some(false)`) for it.
    fn take_news(&mut self) -> Option<bool> {
        // The window bar's slots are pressed between events: send what they
        // asked through the page's own door.
        let asked = self.take_window_toolbar_events() | self.take_attention();
        self.declare_window_toolbar();
        if asked {
            return Some(true);
        }
        if self.poll_picker() || self.poll_print_preview() || self.poll_print_options() {
            return Some(false);
        }
        if let Some(snapshot) = self.refresh.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.refresh = None;
            self.snapshot = Some(snapshot);
            self.rebuild_dashboard();
            return Some(false);
        }
        None
    }
}

impl PrintCentreWindow {
    /// A window over `centre`.
    fn new(centre: PrintCentre, live: Option<Live<PrintDefaults>>) -> Self {
        let mut window = Self {
            centre: Rc::new(RefCell::new(centre)),
            page: loading_view(),
            live,
            presented: None,
            replaced: false,
        };
        window.follow_document();
        window.show_page();
        window
    }

    /// Start the defaults the document holds from when they changed, here
    /// or on another device.
    fn follow_document(&mut self) {
        let Some(live) = &self.live else {
            return;
        };
        let Some(shown) = live.changed_since(&mut self.presented) else {
            return;
        };
        let defaults = (*shown).clone();
        self.centre.borrow_mut().present_defaults(&defaults);
    }

    /// Show the page an effect or a finished job put up.
    fn show_page(&mut self) {
        if let Some(page) = self.centre.borrow_mut().shown.take() {
            self.page = page;
            self.replaced = true;
        }
    }
}

/// The Print Centre window over this machine's printers and scanners,
/// answering its effects and publishing its devices as live rows.
pub(crate) fn window(cx: &ViewCx<PrintDefaults>) -> Element {
    let mut centre = PrintCentre::new();
    centre.attach(cx);
    let window = PrintCentreWindow::new(centre, Some(cx.live().clone()));
    let centre = Rc::clone(&window.centre);
    {
        let (answer, status) = (Rc::clone(&centre), Rc::clone(&centre));
        cx.effects(move |effect: PrintCentreEffect, cx| {
            answer.borrow_mut().perform(effect, &answer, cx)
        })
        // The scan and print steps that have nothing to act on right now.
        .status(move || status.borrow().effect_statuses());
    }
    {
        let centre = Rc::clone(&centre);
        cx.on_stage(move |scenario| centre.borrow_mut().stage_scenario(scenario));
    }
    cx.publishing(
        // The printers, documents and scanners CUPS and SANE reported.
        Publishes::new().live_rows(move || centre.borrow().device_rows()),
        Box::new(window),
    )
}

impl ui::widget::Decorator for PrintCentreWindow {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.page.as_ref()
    }
    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.page.as_mut()
    }
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.follow_document();
        {
            let mut centre = self.centre.borrow_mut();
            centre.take_window_toolbar_events();
            centre.take_attention();
            centre.declare_window_toolbar();
        }
        self.show_page();
        self.page.sync(state, path);
    }
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.follow_document();
        let news = self.centre.borrow_mut().take_news();
        self.show_page();
        match news {
            Some(true) => {
                self.page.update(state, path);
                true
            }
            Some(false) => {
                self.page.sync(state, path);
                true
            }
            None => self.page.update(state, path) || self.replaced,
        }
    }
    fn name(&self) -> &'static str {
        "PrintCentre"
    }
    fn take_structure_dirty(&mut self) -> bool {
        // Effects replace the whole page; the new one needs a new layout.
        self.show_page();
        self.page.take_structure_dirty() | std::mem::take(&mut self.replaced)
    }
    fn needs_idle_sync(&self) -> bool {
        let centre = self.centre.borrow();
        centre.refresh.is_some() || centre.picker.is_some()
    }
    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.page.next_wake_deadline()
    }
}

#[cfg(test)]
mod tests;
