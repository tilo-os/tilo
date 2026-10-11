use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use ui::framework::{
    Dispatcher, Effect as _, EffectAnswer, EffectCx, EffectStatus, Effected, LiveRow,
};

use ui::tilo_resource::Atom;
use ui::widget::{AppModel, ModelPath};
use ui::widget::{CropSelection, Widget};

mod views;
use views::{
    device_view, diagnostics_view, discovery_view, history_view, live_view, loading_view,
    print_setup_view, queue_view, scan_setup_view,
};
type El = Box<dyn Widget>;

use crate::resource::{
    OCR_LANGUAGES, PrintCentreEffect, PrintDefaults, SCAN_COLORS, SCAN_RESOLUTIONS, SCAN_SOURCES,
};

pub struct PrintCentre {
    inner: El,
    /// The page `inner` shows.
    page: Page,
    refresh: Option<mpsc::Receiver<Snapshot>>,
    snapshot: Option<Snapshot>,
    notice: Option<OperationNotice>,
    pending_cancel: Option<String>,
    /// What the last search for network printers found.
    discovered: Option<Result<Vec<os_actions::printing::DiscoveredPrinter>, String>>,
    picker: Option<ui::portal::PortalChannel>,
    pending_document: Option<PathBuf>,
    print_preview: PrintPreview,
    print_preview_rx: Option<mpsc::Receiver<Option<thumbnail::RgbaThumb>>>,
    selected_printer: Option<String>,
    print_copies: ui::LocalState<f64>,
    print_two_sided: ui::LocalState<bool>,
    selected_scanner: Option<String>,
    scan: ScanState,
    /// The scan is being saved where the person chose.
    scan_saving: bool,
    /// The person's print and scan defaults, as last presented.
    defaults: PrintDefaults,
    /// Where changes to the defaults go.
    pub(crate) dispatcher: Option<Dispatcher<PrintDefaults>>,
    pub(crate) controls: Option<ui::framework::Actions<PrintDefaults>>,
    /// An effect replaced the page since the last layout.
    replaced: bool,
}

/// The scan page's controls and the pages scanned so far.
struct ScanState {
    resolution: ui::LocalState<usize>,
    color: ui::LocalState<usize>,
    source: ui::LocalState<usize>,
    language: ui::LocalState<usize>,
    crop: ui::LocalState<CropSelection>,
    /// The page under review, if any.
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
    },
    #[default]
    Unavailable,
}

fn load_print_preview(path: &std::path::Path) -> Option<thumbnail::RgbaThumb> {
    let category = thumbnail::category_for_path(path);
    thumbnail::generate(&thumbnail::Source::Path(path), category, 720)
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
        self.width = rotated.width();
        self.height = rotated.height();
        self.rgba = Arc::from(rotated.clone().into_raw());
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(rotated)
            .write_to(&mut png, image::ImageFormat::Png)
            .map_err(|error| format!("could not update the scanned page: {error}"))?;
        self.png = Arc::from(png.into_inner());
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
        self.width = cropped.width();
        self.height = cropped.height();
        self.rgba = Arc::from(cropped.clone().into_raw());
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(cropped)
            .write_to(&mut png, image::ImageFormat::Png)
            .map_err(|error| format!("could not update the scanned page: {error}"))?;
        self.png = Arc::from(png.into_inner());
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
    scanners: Result<Vec<os_actions::printing::Scanner>, String>,
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
    ChooseDocument,
    SelectPrinter(String),
    PrintDocument,
    CancelPrintSetup,
    ScanSetup,
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
    SetTwoSided(bool),
    SetScanResolution(usize),
    SetScanColor(usize),
    SetScanSource(usize),
    SetOcrLanguage(usize),
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
            UiCommand::ChooseDocument => E::PrintDocument,
            UiCommand::SelectPrinter(printer) => E::ChoosePrinter { printer },
            UiCommand::PrintDocument => E::SendToPrinter,
            UiCommand::CancelPrintSetup => E::ClosePrintSetup,
            UiCommand::ScanSetup => E::ScanDocument,
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
            UiCommand::SetTwoSided(value) => {
                dispatcher.submit(PrintDefaults::paths().two_sided.set(value));
                return Ok(());
            }
            UiCommand::SetScanResolution(index) => {
                let dpi = SCAN_RESOLUTIONS.get(index).ok_or("invalid resolution")?;
                dispatcher.submit(PrintDefaults::paths().scan_resolution.set(*dpi));
                return Ok(());
            }
            UiCommand::SetScanColor(index) => {
                let value = SCAN_COLORS.get(index).ok_or("invalid color")?;
                dispatcher.submit(
                    PrintDefaults::paths()
                        .scan_color
                        .set(Atom((*value).to_owned())),
                );
                return Ok(());
            }
            UiCommand::SetScanSource(index) => {
                let value = SCAN_SOURCES.get(index).ok_or("invalid source")?;
                dispatcher.submit(
                    PrintDefaults::paths()
                        .scan_source
                        .set(Atom((*value).to_owned())),
                );
                return Ok(());
            }
            UiCommand::SetOcrLanguage(index) => {
                let (value, _) = OCR_LANGUAGES.get(index).ok_or("invalid language")?;
                dispatcher.submit(
                    PrintDefaults::paths()
                        .ocr_language
                        .set(Atom((*value).to_owned())),
                );
                return Ok(());
            }
        };
        dispatcher.effect(&effect);
        Ok(())
    }
}

/// The page the surface shows.
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
}

/// How saving scanned pages ended.
enum ScanSaved {
    Saved(String),
    Cancelled,
    NoPicker,
    Failed,
}

#[derive(Clone)]
struct OperationNotice {
    success: bool,
    title: String,
    message: String,
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
fn save_scanned_page(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    private_fs::owner_only(&mut options);
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
    let mut row = LiveRow::new(
        format!("print-centre.printer.{}", resource_key(&queue.name)),
        "print-centre.printer",
        display_name(&queue.name),
    )
    .effect(&PrintCentreEffect::OpenPrinter { printer: printer() });
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
fn scanner_row(scanner: &os_actions::printing::Scanner) -> LiveRow {
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
        JobState, PrintJob, PrinterQueue, PrinterSupply, PrinterTelemetry, QueueState, Scanner,
    };
    Snapshot {
        queues: Ok(vec![PrinterQueue {
            name: "Studio_printer".into(),
            state: QueueState::Printing,
            default: true,
            accepting_jobs: true,
            reason: None,
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
                scanners: os_actions::printing::scanners(),
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
            inner: loading_view(),
            page: Page::Loading,
            refresh: None,
            snapshot: None,
            notice: None,
            pending_cancel: None,
            discovered: None,
            picker: None,
            pending_document: None,
            print_preview: PrintPreview::default(),
            print_preview_rx: None,
            selected_printer: None,
            print_copies: ui::LocalState::new(1.0),
            print_two_sided: ui::LocalState::new(defaults.two_sided),
            selected_scanner: None,
            scan: ScanState::default(),
            scan_saving: false,
            defaults,
            dispatcher: None,
            controls: None,
            replaced: false,
        }
    }

    /// Where the controls send their effects and the defaults' changes.
    fn door(&self) -> Door {
        Door(self.dispatcher.clone(), self.controls.clone())
    }

    /// Ask CUPS and SANE what this machine has.
    pub(crate) fn refresh(&mut self) {
        self.notice = None;
        self.page = Page::Loading;
        self.replaced = true;
        self.inner = loading_view();
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
    ///
    /// # Errors
    /// Returns why the printer, document or scanner it names is not known,
    /// or why the work it started failed.
    pub(crate) fn perform(
        &mut self,
        effect: PrintCentreEffect,
        cx: &mut EffectCx<PrintDefaults, Self>,
    ) -> EffectAnswer {
        match effect {
            PrintCentreEffect::Refresh => self.refresh(),
            PrintCentreEffect::FindPrinters => return Ok(self.discover(cx)),
            effect @ (PrintCentreEffect::AddPrinter { .. }
            | PrintCentreEffect::SetDefaultPrinter { .. }
            | PrintCentreEffect::PausePrinter { .. }
            | PrintCentreEffect::ResumePrinter { .. }
            | PrintCentreEffect::CancelDocument { .. }
            | PrintCentreEffect::RetryDocument { .. }) => return self.device_job(effect, cx),
            PrintCentreEffect::OpenDashboard => self.open_dashboard(),
            PrintCentreEffect::OpenQueue => self.open_queue(),
            PrintCentreEffect::OpenHistory => self.open_history(),
            PrintCentreEffect::RunDiagnostics => self.open_diagnostics(),
            PrintCentreEffect::PrintDocument => self.request_document(),
            PrintCentreEffect::ScanDocument => self.open_scan(),
            PrintCentreEffect::OpenPrinter { printer } => {
                let printer = self.known_printer(printer)?;
                self.open_device(&printer);
            }
            PrintCentreEffect::ConfirmCancel { job } => {
                self.pending_cancel = Some(self.known_job(job)?);
                self.open_queue();
            }
            PrintCentreEffect::ChoosePrinter { printer } => {
                self.selected_printer = Some(self.known_printer(printer)?);
                self.rebuild_print_setup();
            }
            PrintCentreEffect::SendToPrinter => return self.send_to_printer(cx),
            PrintCentreEffect::ClosePrintSetup => self.close_print_setup(),
            PrintCentreEffect::ScanWith { scanner } => {
                let scanner = self.known_scanner(scanner)?;
                if self.page != Page::Scan {
                    self.open_scan();
                }
                self.selected_scanner = Some(scanner);
                self.rebuild_scan_setup();
            }
            PrintCentreEffect::ScanPage => return self.scan_page(cx),
            PrintCentreEffect::ScanAnother => self.scan_another(),
            PrintCentreEffect::PreviousScanPage => self.previous_scan_page(),
            PrintCentreEffect::NextScanPage => self.next_scan_page(),
            PrintCentreEffect::MoveScanPageEarlier => self.move_scan_page_earlier(),
            PrintCentreEffect::MoveScanPageLater => self.move_scan_page_later(),
            PrintCentreEffect::RemoveScanPage => self.remove_scan_page(),
            PrintCentreEffect::RotateScanPage => self.rotate_scan_page(),
            PrintCentreEffect::CropScanPage => self.crop_scan_page(),
            PrintCentreEffect::RecognizeText => return self.recognize_text(cx),
            PrintCentreEffect::CopyRecognizedText => self.copy_recognized_text()?,
            PrintCentreEffect::SaveScannedPage => return self.save_scan(false, cx),
            PrintCentreEffect::SaveScannedDocument => return self.save_scan(true, cx),
        }
        Ok(Effected::done())
    }

    /// Start `effect`'s work on a printer or document in the background.
    fn device_job(
        &mut self,
        effect: PrintCentreEffect,
        cx: &mut EffectCx<PrintDefaults, Self>,
    ) -> EffectAnswer {
        match effect {
            PrintCentreEffect::AddPrinter { printer } => {
                let printer = self.known_discovery(&printer)?;
                let shown = display_name(&printer.name);
                Ok(Self::run(
                    cx,
                    "Printer added",
                    format!("{shown} is ready for your apps."),
                    "Could not add printer",
                    format!("Check that {shown} is awake and connected, then try again."),
                    move || os_actions::printing::add_driverless(&printer.uri, &printer.name),
                ))
            }
            PrintCentreEffect::SetDefaultPrinter { printer } => {
                let name = self.known_printer(printer)?;
                let shown = display_name(&name);
                Ok(Self::run(
                    cx,
                    "Preferred printer changed",
                    format!("New documents will use {shown} unless you choose another printer."),
                    "Could not change the preferred printer",
                    format!("{shown} is still available; try again in a moment."),
                    move || os_actions::printing::set_default(&name),
                ))
            }
            PrintCentreEffect::PausePrinter { printer } => {
                let name = self.known_printer(printer)?;
                Ok(Self::run(
                    cx,
                    "Printer paused",
                    "Documents will wait until you resume it.".into(),
                    "Could not pause printer",
                    "Nothing changed. Check the printer connection and try again.".into(),
                    move || os_actions::printing::set_enabled(&name, false),
                ))
            }
            PrintCentreEffect::ResumePrinter { printer } => {
                let name = self.known_printer(printer)?;
                Ok(Self::run(
                    cx,
                    "Printer resumed",
                    "Waiting documents can print now.".into(),
                    "Could not resume printer",
                    "Nothing changed. Check the printer connection and try again.".into(),
                    move || os_actions::printing::set_enabled(&name, true),
                ))
            }
            PrintCentreEffect::CancelDocument { job } => {
                let id = self.known_job(job)?;
                self.pending_cancel = None;
                Ok(Self::run(
                    cx,
                    "Document canceled",
                    "It was removed from the printer.".into(),
                    "Could not cancel document",
                    "The document may still print. Check the queue and try again.".into(),
                    move || os_actions::printing::cancel_job(&id),
                ))
            }
            PrintCentreEffect::RetryDocument { job } => {
                let id = self.known_job(job)?;
                Ok(Self::run(
                    cx,
                    "Document sent again",
                    "It is back in the print queue.".into(),
                    "Could not try again",
                    "Check the printer, then try again.".into(),
                    move || os_actions::printing::retry_job(&id),
                ))
            }
            _ => Err(format!("`{}` is not printer work", effect.id())),
        }
    }

    /// The scan and print steps that have nothing to act on right now.
    pub(crate) fn effect_statuses(&self) -> Vec<EffectStatus> {
        const NO_DOCUMENT: &str = "Choose a document to print first.";
        const NO_PAGE: &str = "Scan a page first.";
        let mut statuses = Vec::new();
        if self.pending_document.is_none() {
            statuses.push(EffectStatus::unavailable(
                PrintCentreEffect::CHOOSE_PRINTER,
                "no-document",
                NO_DOCUMENT,
            ));
            statuses.push(EffectStatus::unavailable(
                PrintCentreEffect::SEND_TO_PRINTER,
                "no-document",
                NO_DOCUMENT,
            ));
            statuses.push(EffectStatus::unavailable(
                PrintCentreEffect::CLOSE_PRINT_SETUP,
                "no-document",
                NO_DOCUMENT,
            ));
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
            .discovered
            .as_ref()
            .is_some_and(|found| found.as_ref().is_ok_and(|found| !found.is_empty()))
        {
            statuses.push(EffectStatus::unavailable(
                PrintCentreEffect::ADD_PRINTER,
                "nothing-found",
                "Find network printers first.",
            ));
        }
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
        statuses
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
        self.discovered
            .as_ref()
            .and_then(|found| found.as_ref().ok())
            .and_then(|found| found.iter().find(|printer| printer.uri == uri))
            .cloned()
            .ok_or_else(|| "The last search did not find that printer".to_owned())
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
                self.print_preview = demo_print_preview();
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
        if let Some(Ok(found)) = &self.discovered {
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

    fn request_print_preview(&mut self, path: PathBuf) {
        let (tx, rx) = mpsc::channel();
        self.print_preview = PrintPreview::Loading;
        self.print_preview_rx = Some(rx);
        let _ = std::thread::Builder::new()
            .name("tilo-print-preview".into())
            .spawn(move || {
                let _ = tx.send(load_print_preview(&path));
            });
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
        self.print_preview =
            result.map_or(PrintPreview::Unavailable, |preview| PrintPreview::Ready {
                rgba: Arc::from(preview.rgba),
                width: preview.width,
                height: preview.height,
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
                    self.request_print_preview(path);
                }
                self.rebuild_print_setup();
            }
            ui::portal::PortalOutcome::Cancelled => {}
            ui::portal::PortalOutcome::Failed(_) => {
                self.notice = Some(OperationNotice {
                    success: false,
                    title: "Could not open the file picker".into(),
                    message: "Try again, or print from the document's app.".into(),
                });
                self.rebuild_dashboard();
            }
        }
        true
    }

    fn rebuild_print_setup(&mut self) {
        if let (Some(snapshot), Some(path)) = (&self.snapshot, &self.pending_document) {
            self.page = Page::Print;
            self.replaced = true;
            self.inner = print_setup_view(
                snapshot,
                path,
                &self.print_preview,
                self.selected_printer.as_deref(),
                &self.print_copies,
                &self.print_two_sided,
                &Door(self.dispatcher.clone(), self.controls.clone()),
            );
        }
    }

    fn rebuild_scan_setup(&mut self) {
        if let Some(snapshot) = &self.snapshot {
            self.page = Page::Scan;
            self.replaced = true;
            self.inner = scan_setup_view(
                snapshot,
                self.selected_scanner.as_deref(),
                &self.scan,
                self.notice.as_ref(),
                &Door(self.dispatcher.clone(), self.controls.clone()),
            );
        }
    }

    fn rebuild_dashboard(&mut self) {
        self.page = Page::Dashboard;
        self.replaced = true;
        let door = self.door();
        self.inner = self.snapshot.as_ref().map_or_else(
            || empty_view(&door),
            |snapshot| live_view(snapshot, &door, self.notice.as_ref()),
        );
    }

    fn open_dashboard(&mut self) {
        self.scan.result = None;
        self.scan.in_progress = false;
        self.rebuild_dashboard();
    }

    fn open_queue(&mut self) {
        let door = self.door();
        if let Some(snapshot) = &self.snapshot {
            self.page = Page::Queue;
            self.replaced = true;
            self.inner = queue_view(
                snapshot,
                &door,
                self.pending_cancel.as_deref(),
                self.notice.as_ref(),
            );
        }
    }

    fn open_history(&mut self) {
        let door = self.door();
        if let Some(snapshot) = &self.snapshot {
            self.page = Page::History;
            self.replaced = true;
            self.inner = history_view(snapshot, &door);
        }
    }

    fn open_diagnostics(&mut self) {
        let door = self.door();
        if let Some(snapshot) = &self.snapshot {
            self.page = Page::Diagnostics;
            self.replaced = true;
            self.inner = diagnostics_view(snapshot, &door);
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
            self.replaced = true;
            self.inner = device_view(queue, &door);
        }
    }

    fn open_discovery(&mut self) {
        self.page = Page::Discovery;
        self.replaced = true;
        self.inner = discovery_view(self.discovered.as_ref(), &self.door());
    }

    /// Search the network for printers, showing the search while it runs.
    fn discover(&mut self, cx: &mut EffectCx<PrintDefaults, Self>) -> Effected {
        self.discovered = None;
        self.open_discovery();
        cx.thread(
            |_| os_actions::printing::discover_driverless(),
            |app: &mut Self, found: Result<_, String>, _cx| {
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

    /// Run `work` on a printer or document in the background, then tell the
    /// person how it went and show what CUPS reports afterwards.
    fn run(
        cx: &mut EffectCx<PrintDefaults, Self>,
        success_title: &'static str,
        success_message: String,
        failure_title: &'static str,
        failure_message: String,
        work: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) -> Effected {
        cx.thread(
            move |_| work(),
            move |app: &mut Self, result: Result<(), String>, _cx| {
                let success = result.is_ok();
                app.finished(OperationNotice {
                    success,
                    title: if success {
                        success_title
                    } else {
                        failure_title
                    }
                    .into(),
                    message: if success {
                        success_message
                    } else {
                        failure_message.clone()
                    },
                });
                if success {
                    Ok(Effected::done())
                } else {
                    Err(failure_message)
                }
            },
        )
    }

    /// Work in the background finished, as `notice` says.
    fn finished(&mut self, notice: OperationNotice) {
        self.notice = Some(notice);
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
    fn send_to_printer(&mut self, cx: &mut EffectCx<PrintDefaults, Self>) -> EffectAnswer {
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
        self.print_preview = PrintPreview::Unavailable;
        self.print_preview_rx = None;
        let copies = format!("{:.0}", self.print_copies.get().clamp(1.0, 99.0))
            .parse::<u16>()
            .unwrap_or(1);
        let two_sided = self.print_two_sided.get();
        Ok(Self::run(
            cx,
            "Document sent to printer",
            format!("{filename} was sent to {shown_queue}."),
            "Could not print document",
            format!("{filename} was not sent. Check the printer and try again."),
            move || {
                os_actions::printing::print_document(
                    &queue,
                    &path,
                    os_actions::printing::PrintSettings { copies, two_sided },
                )
            },
        ))
    }

    fn close_print_setup(&mut self) {
        self.pending_document = None;
        self.print_preview = PrintPreview::Unavailable;
        self.print_preview_rx = None;
        self.selected_printer = None;
        self.print_copies.set(1.0);
        self.print_two_sided.set(self.defaults.two_sided);
        self.rebuild_dashboard();
    }

    /// Start a new scanned document with the first scanner.
    fn open_scan(&mut self) {
        self.notice = None;
        self.scan.result = None;
        self.scan.pages.clear();
        self.scan.selected = 0;
        self.scan.in_progress = false;
        self.selected_scanner = self.snapshot.as_ref().and_then(|snapshot| {
            snapshot
                .scanners
                .as_ref()
                .ok()
                .and_then(|scanners| scanners.first())
                .map(|scanner| scanner.device.clone())
        });
        self.rebuild_scan_setup();
    }

    /// Scan a page with the chosen scanner and settings, recognizing its
    /// text in the person's language.
    fn scan_page(&mut self, cx: &mut EffectCx<PrintDefaults, Self>) -> EffectAnswer {
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
            1 => os_actions::printing::ScanColor::Grayscale,
            2 => os_actions::printing::ScanColor::LineArt,
            _ => os_actions::printing::ScanColor::Color,
        };
        let source = match self.scan.source.get() {
            1 => os_actions::printing::ScanSource::Feeder,
            2 => os_actions::printing::ScanSource::FeederDuplex,
            _ => os_actions::printing::ScanSource::Flatbed,
        };
        let (ocr_code, ocr_language) = match self.scan.language.get() {
            1 => ("deu", "German"),
            2 => ("fra", "French"),
            3 => ("spa", "Spanish"),
            _ => ("eng", "English"),
        };
        self.scan.in_progress = true;
        self.rebuild_scan_setup();
        Ok(cx.thread(
            move |_| {
                os_actions::printing::scan_page(
                    &device,
                    os_actions::printing::ScanSettings {
                        resolution_dpi,
                        color,
                        source,
                    },
                )
                .and_then(|png| decode_scanned_page(png, ocr_code, ocr_language))
            },
            |app: &mut Self, result: Result<ScannedPage, String>, _cx| {
                let answer = match &result {
                    Ok(_) => Ok(Effected::done()),
                    Err(error) => Err(error.clone()),
                };
                app.scan_finished(result);
                answer
            },
        ))
    }

    fn scan_finished(&mut self, result: Result<ScannedPage, String>) {
        self.scan.in_progress = false;
        match result {
            Ok(page) => {
                self.scan.pages.push(page.clone());
                self.scan.selected = self.scan.pages.len().saturating_sub(1);
                self.scan.result = Some(page);
                self.scan.crop.set(CropSelection::default());
            }
            Err(_) => {
                self.notice = Some(OperationNotice {
                    success: false,
                    title: "Could not scan this page".into(),
                    message: "Check that the scanner is connected and the paper is positioned, then try again.".into(),
                });
            }
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
        self.notice = None;
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
                    self.notice = None;
                }
                Err(message) => {
                    self.notice = Some(OperationNotice {
                        success: false,
                        title: "Could not rotate this page".into(),
                        message,
                    });
                }
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
                    self.notice = None;
                }
                Err(message) => {
                    self.notice = Some(OperationNotice {
                        success: false,
                        title: "Could not crop this page".into(),
                        message,
                    });
                }
            }
        }
        self.rebuild_scan_setup();
    }

    /// Recognize the text of the page under review again.
    fn recognize_text(&mut self, cx: &mut EffectCx<PrintDefaults, Self>) -> EffectAnswer {
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
        Ok(cx.thread(
            move |_| recognize_page(&page),
            move |app: &mut Self, result: Result<(Option<String>, String), String>, _cx| {
                let answer = match &result {
                    Ok(_) => Ok(Effected::done()),
                    Err(error) => Err(error.clone()),
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
                self.notice = Some(OperationNotice {
                    success: false,
                    title: "Text was not updated".into(),
                    message,
                });
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
        self.notice = Some(OperationNotice {
            success: true,
            title: "Text copied".into(),
            message: "Paste the recognized text into any app.".into(),
        });
        self.rebuild_scan_setup();
        Ok(())
    }

    /// Ask where to save the page under review as an image, or every
    /// scanned page as one PDF (`document`), and save it there.
    fn save_scan(
        &mut self,
        document: bool,
        cx: &mut EffectCx<PrintDefaults, Self>,
    ) -> EffectAnswer {
        if self.scan_saving {
            return Err("The scan is already being saved".into());
        }
        let pages = if document {
            self.scan.pages.clone()
        } else {
            self.scan.result.iter().cloned().collect()
        };
        if pages.is_empty() {
            return Err("Scan a page first".into());
        }
        self.scan_saving = true;
        let request = if document {
            ui::portal::PickRequest::save("Save scanned document", "Save", "Scanned document.pdf")
                .thread_name("print-centre-scan-document-save-picker")
        } else {
            ui::portal::PickRequest::save("Save scanned page", "Save", "Scanned page.png")
                .thread_name("print-centre-scan-save-picker")
        };
        Ok(cx.thread(
            move |_| {
                let mut path = match request.send().recv() {
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
                    Arc::<[u8]>::from(scanned_document_pdf(&pages))
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
            },
            move |app: &mut Self, saved: ScanSaved, _cx| {
                app.scan_saving = false;
                let (notice, answer) = match saved {
                    ScanSaved::Saved(name) => (
                        OperationNotice {
                            success: true,
                            title: if document {
                                "Scanned document saved"
                            } else {
                                "Scanned page saved"
                            }
                            .into(),
                            message: format!("Saved {name}."),
                        },
                        Ok(Effected::done()),
                    ),
                    ScanSaved::Cancelled => return Ok(Effected::done()),
                    ScanSaved::NoPicker => (
                        OperationNotice {
                            success: false,
                            title: "Could not open the file picker".into(),
                            message: "Try saving the scanned page again.".into(),
                        },
                        Err("Could not open the file picker".into()),
                    ),
                    ScanSaved::Failed => (
                        OperationNotice {
                            success: false,
                            title: if document {
                                "Could not save scanned document"
                            } else {
                                "Could not save scanned page"
                            }
                            .into(),
                            message: "Choose another location and try again.".into(),
                        },
                        Err("Could not save the scan there".into()),
                    ),
                };
                app.notice = Some(notice);
                app.rebuild_scan_setup();
                answer
            },
        ))
    }
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
impl ui::widget::Decorator for PrintCentre {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.inner.as_ref()
    }
    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.inner.as_mut()
    }
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        if self.poll_picker() {
            self.inner.sync(state, path);
            return true;
        }
        if self.poll_print_preview() {
            self.inner.sync(state, path);
            return true;
        }
        if let Some(snapshot) = self.refresh.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.refresh = None;
            self.snapshot = Some(snapshot);
            self.rebuild_dashboard();
            self.inner.sync(state, path);
            return true;
        }
        self.inner.update(state, path)
    }
    fn name(&self) -> &'static str {
        "PrintCentre"
    }
    fn take_structure_dirty(&mut self) -> bool {
        // Effects replace the whole page; the new one needs a new layout.
        self.inner.take_structure_dirty() | std::mem::take(&mut self.replaced)
    }
    fn needs_idle_sync(&self) -> bool {
        self.refresh.is_some() || self.picker.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::PrintCentreShell;
    use ui::framework::ResourceApp;
    use ui::prelude::TestApp;
    use ui::theme::Theme;
    use vello::kurbo::Rect;

    fn registry() -> ui::tilo_actions::ActionRegistry {
        ui::tilo_actions::ActionRegistry::with(
            <PrintDefaults as ui::tilo_resource::Resource>::register,
        )
    }

    thread_local! {
        /// What CUPS and SANE report to a live window in a test, instead of
        /// asking the machine the test runs on.
        pub(super) static INSTALLED: std::cell::RefCell<Option<Snapshot>> =
            const { std::cell::RefCell::new(None) };
    }

    /// A live Print Centre whose refresh found the studio printer, its
    /// documents and its scanner.
    fn live_with_studio() -> TestApp<PrintCentreShell> {
        INSTALLED.with(|installed| *installed.borrow_mut() = Some(demo_snapshot()));
        let mut app = TestApp::<PrintCentreShell>::new().expect("launch");
        for _ in 0..600 {
            app.settle();
            if app
                .harness()
                .resources()
                .iter()
                .flat_map(|resource| resource.children.iter())
                .any(|child| child.kind == "print-centre.printer")
            {
                return app;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("the refresh never reported the studio printer");
    }

    /// The tree Print Centre publishes once a refresh has found printers,
    /// documents and scanners.
    fn populated_resources() -> Vec<ui::tilo_actions::Resource> {
        live_with_studio().harness().resources()
    }

    // The defaults' actions all take an argument; the sweep validates the
    // whole published surface (the pages, every printer, document and
    // scanner) and Ctrl+K.
    ui::action_contract_tests! {
        seed: ui::automerge::AutoCommit::new(),
        registry: registry(),
        resources: |_| populated_resources(),
        palette: ["Print a document", "Open print queue", "Find network printers", "Open printer"],
    }

    /// Print Centre's own widget in a captured frame, without the window
    /// chrome around it.
    fn print_centre(node: &ui::headless::InspectionNode) -> &ui::headless::InspectionNode {
        fn find(node: &ui::headless::InspectionNode) -> Option<&ui::headless::InspectionNode> {
            if node.widget == "ResourceApp" {
                return Some(node);
            }
            node.children.iter().find_map(find)
        }
        find(node).expect("Print Centre in the frame")
    }

    fn queue(
        state: os_actions::printing::QueueState,
        accepting_jobs: bool,
    ) -> os_actions::printing::PrinterQueue {
        os_actions::printing::PrinterQueue {
            name: "Studio".into(),
            state,
            default: false,
            accepting_jobs,
            reason: None,
            telemetry: os_actions::printing::PrinterTelemetry::default(),
        }
    }

    /// The id of the published row of `kind`.
    fn row_id(surface: &ui::serde_json::Value, kind: &str) -> String {
        surface
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["resource_kind"] == kind))
            .and_then(|row| row["resource"].as_str())
            .unwrap_or_else(|| panic!("no {kind} row in {surface}"))
            .to_owned()
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn scenarios_and_resources_expose_real_navigation_and_backend_states() {
        eprintln!("phase every_scenario_captures_at_its_viewport");
        {
            for scenario in PrintCentreShell::scenarios()
                .iter()
                .map(|name| Some(*name))
                .chain([None])
            {
                let mut app = TestApp::<PrintCentreShell>::capture(scenario)
                    .unwrap_or_else(|error| panic!("{scenario:?}: {error}"));
                let (width, height) = PrintCentreShell::shot_viewport(scenario);
                assert_eq!(app.harness().inspection().viewport, [width, height]);
            }
            let mut queue = TestApp::<PrintCentreShell>::capture(Some("queue")).unwrap();
            let frame = queue.harness().inspection_json().unwrap();
            assert!(frame.contains("Travel itinerary"), "{frame}");
            let mut empty = TestApp::<PrintCentreShell>::capture(Some("empty")).unwrap();
            let frame = empty.harness().inspection_json().unwrap();
            assert!(frame.contains("Set up your first printer"), "{frame}");
        }

        eprintln!("phase published_tree_exposes_real_navigation_and_device_operations");
        {
            let mut app = live_with_studio();
            let audit = app.harness().native_app_audit();
            assert!(audit.is_certified(), "{audit:?}");
            let surface = app.agent_surface();
            let actions = surface
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["resource_kind"] == "print-centre.defaults")
                .filter_map(|row| row["action"].as_str())
                .collect::<Vec<_>>();
            for expected in [
                "refresh",
                "open-queue",
                "open-history",
                "run-diagnostics",
                "find-printers",
                "print-document",
                "scan-document",
                "field-edit",
            ] {
                assert!(
                    actions.contains(&expected),
                    "missing {expected}: {actions:?}"
                );
            }
            let printer = row_id(&surface, "print-centre.printer");
            let job = row_id(&surface, "print-centre.job");
            let scanner = row_id(&surface, "print-centre.scanner");

            // A page opens through the effect a control sends; a row opens
            // its own printer.
            app.effect(&PrintCentreEffect::OpenHistory).unwrap();
            let frame = app.harness().inspection_json().unwrap();
            assert!(frame.contains("Shipping labels"), "{frame}");
            app.call_on(&printer, "open-printer", ui::serde_json::json!({}))
                .unwrap();
            let frame = app.harness().inspection_json().unwrap();
            assert!(frame.contains("print-centre.device"), "{frame}");
            app.call_on(&scanner, "scan-with", ui::serde_json::json!({}))
                .unwrap();
            let frame = app.harness().inspection_json().unwrap();
            assert!(frame.contains("print-centre.scan.resolution"), "{frame}");
            assert!(
                app.agent_surface()
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(
                        |row| row["resource"] == job.as_str() && row["action"] == "cancel-document"
                    ),
                "a printing document can be cancelled"
            );

            // Cancelling from the dashboard asks first, in the queue.
            app.effect(&PrintCentreEffect::ConfirmCancel {
                job: "Studio_printer-42".into(),
            })
            .unwrap();
            let frame = app.harness().inspection_json().unwrap();
            assert!(frame.contains("Confirm cancel"), "{frame}");

            // Steps with nothing to act on are refused: no search found a
            // printer to add, no page was scanned, no document is set up.
            assert!(
                app.effect(&PrintCentreEffect::AddPrinter {
                    printer: "ipps://studio.local/ipp/print".into()
                })
                .is_err()
            );
            assert!(app.effect(&PrintCentreEffect::RotateScanPage).is_err());
            assert!(app.effect(&PrintCentreEffect::SendToPrinter).is_err());

            // A printer the snapshot does not list is refused, not guessed.
            assert!(
                app.effect(&PrintCentreEffect::OpenPrinter {
                    printer: "Elsewhere".into()
                })
                .is_err()
            );
        }

        eprintln!("phase scan_review_steps_are_offered_only_with_a_page_to_act_on");
        {
            let no_page =
                |effect| EffectStatus::unavailable(effect, "no-page", "Scan a page first.");
            let no_scanner = EffectStatus::unavailable(
                PrintCentreEffect::SCAN_PAGE,
                "no-scanner",
                "Open scanning with a scanner first.",
            );
            let blocked = |centre: &PrintCentre, status: &EffectStatus| {
                centre.effect_statuses().contains(status)
            };
            let mut centre = PrintCentre::new();
            centre.show(Some(&demo_snapshot()));
            centre.open_scan();
            assert!(!blocked(&centre, &no_scanner));
            assert!(blocked(
                &centre,
                &no_page(PrintCentreEffect::ROTATE_SCAN_PAGE)
            ));
            centre.scan_finished(Ok(demo_scanned_page()));
            assert!(!blocked(
                &centre,
                &no_page(PrintCentreEffect::ROTATE_SCAN_PAGE)
            ));
            assert!(!blocked(
                &centre,
                &no_page(PrintCentreEffect::COPY_RECOGNIZED_TEXT)
            ));
            assert!(blocked(
                &centre,
                &no_page(PrintCentreEffect::NEXT_SCAN_PAGE)
            ));
            centre.rotate_scan_page();
            assert!(
                centre
                    .scan
                    .result
                    .as_ref()
                    .is_some_and(|page| page.ocr_needs_refresh)
            );
            centre.scan_another();
            assert_eq!(centre.scan.pages.len(), 1);
            assert!(blocked(
                &centre,
                &no_page(PrintCentreEffect::ROTATE_SCAN_PAGE)
            ));
            centre.open_dashboard();
            assert!(blocked(&centre, &no_scanner));
        }

        eprintln!("phase scan_controls_change_the_person_s_defaults");
        {
            let mut app = live_with_studio();
            app.effect(&PrintCentreEffect::ScanDocument).unwrap();
            app.call_on(
                "tilo.control:print-centre.scan.resolution.select",
                "select",
                ui::serde_json::json!({ "index": 2 }),
            )
            .unwrap();
            app.settle();
            assert_eq!(app.state().scan_resolution, 600);
            app.call_on(
                "tilo.control:print-centre.scan.language.select",
                "select",
                ui::serde_json::json!({ "index": 1 }),
            )
            .unwrap();
            app.settle();
            assert_eq!(app.state().ocr_language.0, "deu");
        }

        eprintln!("phase backend_failures_remain_distinct_from_legitimate_empty_results");
        {
            let unavailable = Snapshot {
                queues: Err("cups socket closed".into()),
                jobs: Err("jobs timed out".into()),
                scanners: Err("scanner service missing".into()),
            };
            assert_eq!(
                failed_areas(&unavailable, true, true, true),
                vec!["printers", "documents", "scanners"]
            );

            let empty = Snapshot {
                queues: Ok(Vec::new()),
                jobs: Ok(Vec::new()),
                scanners: Ok(Vec::new()),
            };
            assert!(failed_areas(&empty, true, true, true).is_empty());
            let door = Door::default();
            assert!(
                views::unavailable_notice(&failed_areas(&unavailable, true, true, true), &door)
                    .is_some()
            );
            assert!(
                views::unavailable_notice(&failed_areas(&empty, true, true, true), &door).is_none()
            );
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn phone_and_large_text_layouts_keep_actions_visible_and_measured() {
        eprintln!("phase scan_review_actions_wrap_inside_phone_viewports");
        {
            fn inspect(widget: &ui::headless::InspectionNode, width: f64, buttons: &mut usize) {
                let [x, y, w, h] = widget.rect;
                let rect = Rect::new(x, y, x + w, y + h);
                if widget.widget == "Button" && rect.area() > 0.0 {
                    *buttons += 1;
                    assert!(rect.width() >= 44.0, "button is too narrow: {rect:?}");
                    assert!(rect.height() >= 44.0, "button is too short: {rect:?}");
                    assert!(rect.x0 >= -0.5, "button escaped left edge: {rect:?}");
                    assert!(
                        rect.x1 <= width + 0.5,
                        "button escaped {width}px phone viewport: {rect:?}"
                    );
                }
                for child in &widget.children {
                    inspect(child, width, buttons);
                }
            }

            for width in [360_u32, 390] {
                let mut centre =
                    TestApp::<PrintCentreShell>::capture_at(Some("scan-result"), (width, 760))
                        .unwrap();
                centre
                    .harness()
                    .set_interaction_environment(ui::InteractionEnvironment::TOUCH);
                let mut buttons = 0;
                inspect(
                    print_centre(&centre.harness().inspection().root),
                    f64::from(width),
                    &mut buttons,
                );
                assert!(buttons >= 8, "expected the complete scan-review action set");
            }
        }

        eprintln!("phase print_setup_footer_stays_visible_and_touch_sized_on_phones");
        {
            fn collect_buttons(widget: &ui::headless::InspectionNode, buttons: &mut Vec<Rect>) {
                let [x, y, w, h] = widget.rect;
                let rect = Rect::new(x, y, x + w, y + h);
                if widget.widget == "Button" && rect.area() > 0.0 {
                    buttons.push(rect);
                }
                for child in &widget.children {
                    collect_buttons(child, buttons);
                }
            }

            for width in [360_u32, 520] {
                let height = 820_u32;
                let mut centre =
                    TestApp::<PrintCentreShell>::capture_at(Some("print"), (width, height))
                        .unwrap();
                centre
                    .harness()
                    .set_interaction_environment(ui::InteractionEnvironment::TOUCH);
                let snapshot = centre.harness().inspection();
                let surface = print_centre(&snapshot.root);

                let mut buttons = Vec::new();
                collect_buttons(surface, &mut buttons);
                buttons.sort_by(|left, right| left.y0.total_cmp(&right.y0));
                let footer_buttons = buttons.iter().skip(1).collect::<Vec<_>>();
                assert_eq!(
                    footer_buttons.len(),
                    2,
                    "Cancel and Print must stay visible; buttons were {buttons:?}"
                );
                let root_bottom = surface.rect[1] + surface.rect[3];
                for rect in footer_buttons {
                    assert!(
                        rect.width() >= 44.0,
                        "footer button is too narrow: {rect:?}"
                    );
                    assert!(
                        rect.height() >= 44.0,
                        "footer button is too short: {rect:?}"
                    );
                    assert!(
                        rect.x0 >= 0.0 && rect.x1 <= f64::from(width),
                        "button escaped viewport: {rect:?}"
                    );
                    assert!(
                        rect.y1 <= root_bottom,
                        "button fell below compact layout: {rect:?}"
                    );
                }
            }
        }

        eprintln!("phase dashboard_cards_and_actions_stay_measured_at_large_text_scales");
        {
            fn rect_for(widget: &ui::headless::InspectionNode, id: &str) -> Option<Rect> {
                if widget.test_id.as_deref() == Some(id) {
                    let [x, y, width, height] = widget.rect;
                    return Some(Rect::new(x, y, x + width, y + height));
                }
                widget.children.iter().find_map(|child| rect_for(child, id))
            }

            fn collect_cards(widget: &ui::headless::InspectionNode, cards: &mut Vec<Rect>) {
                let [x, y, width, height] = widget.rect;
                let dashboard_group = widget.widget == "Card"
                    || matches!(
                        widget.test_id.as_deref(),
                        Some(
                            "print-centre.scan.open"
                                | "print-centre.history.open"
                                | "print-centre.diagnostics.open"
                        )
                    );
                if dashboard_group && width > 0.0 && height > 0.0 {
                    cards.push(Rect::new(x, y, x + width, y + height));
                }
                for child in &widget.children {
                    collect_cards(child, cards);
                }
            }

            let ids = [
                "print-centre.add",
                "print-centre.print",
                "print-centre.queue",
                "print-centre.job.cancel",
                "print-centre.refresh",
                "print-centre.device.open.Studio_printer",
                "print-centre.scan.open",
                "print-centre.history.open",
                "print-centre.diagnostics.open",
            ];
            for text_scale in [1.0, 2.0] {
                let mut centre =
                    TestApp::<PrintCentreShell>::capture_at(Some("active"), (1440, 900)).unwrap();
                centre
                    .harness()
                    .set_theme(Theme::dark().with_text_scale(text_scale));
                centre
                    .harness()
                    .set_interaction_environment(ui::InteractionEnvironment::TOUCH);
                let snapshot = centre.harness().inspection();
                let root = &snapshot.root;
                let controls = ids.map(|id| {
                    let rect = rect_for(root, id).unwrap_or_else(|| panic!("missing {id}"));
                    assert!(
                        rect.x0 >= -0.5 && rect.x1 <= 1440.5,
                        "{id} escaped the viewport at {text_scale}x text: {rect:?}"
                    );
                    assert!(
                        rect.width() >= 44.0 && rect.height() >= 44.0,
                        "{id} lost its touch target at {text_scale}x text: {rect:?}"
                    );
                    rect
                });
                assert!(
                    controls[1].y1 <= 900.5,
                    "the primary Print action must remain in the captured viewport: {:?}",
                    controls[1]
                );

                for (index, pair) in controls[3..].windows(2).enumerate() {
                    let (before, after) = (pair[0], pair[1]);
                    assert!(
                        before.y1 + 16.0 <= after.y0,
                        "{} and {} overlap at {text_scale}x text: {before:?} {after:?}",
                        ids[index + 3],
                        ids[index + 4],
                    );
                }

                let mut cards = Vec::new();
                collect_cards(root, &mut cards);
                assert!(
                    cards.len() >= 5,
                    "expected distinct active-job, printer, scan, history, and device-help groups"
                );
                for (index, card) in cards.iter().enumerate() {
                    assert!(
                        card.x0 >= -0.5 && card.x1 <= 1440.5,
                        "card {index} escaped the viewport at {text_scale}x text: {card:?}"
                    );
                    for other in cards.iter().skip(index + 1) {
                        let overlaps = card.x0 < other.x1
                            && other.x0 < card.x1
                            && card.y0 < other.y1
                            && other.y0 < card.y1;
                        assert!(
                            !overlaps,
                            "measured dashboard cards overlap at {text_scale}x text: {card:?} {other:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn backend_telemetry_is_humanized_and_never_invented() {
        eprintln!("phase backend_states_are_translated_to_people_facing_copy");
        {
            use os_actions::printing::QueueState;
            assert_eq!(
                friendly_queue_state(&queue(QueueState::Ready, true)),
                "Ready"
            );
            assert_eq!(
                friendly_queue_state(&queue(QueueState::Ready, false)),
                "Not accepting new documents"
            );
            assert_eq!(
                friendly_queue_state(&queue(QueueState::Offline, true)),
                "Cannot reach this printer"
            );
        }

        eprintln!("phase supply_gauge_uses_the_lowest_known_cartridge");
        {
            let mut printer = queue(os_actions::printing::QueueState::Ready, true);
            printer.telemetry.supplies = Some(vec![
                os_actions::printing::PrinterSupply {
                    name: "Black".into(),
                    color: None,
                    level_percent: Some(82),
                },
                os_actions::printing::PrinterSupply {
                    name: "Cyan".into(),
                    color: None,
                    level_percent: Some(14),
                },
            ]);
            let (fraction, percent) = supply_level(&printer).unwrap();
            assert!((fraction - 0.14).abs() < f32::EPSILON);
            assert_eq!(percent, 14);
        }

        eprintln!("phase missing_supply_telemetry_is_not_invented");
        {
            let printer = queue(os_actions::printing::QueueState::Ready, true);
            assert_eq!(supply_level(&printer), None);
        }

        eprintln!("phase backend_identifiers_are_humanized_before_display");
        {
            assert_eq!(display_name("Studio_printer"), "Studio printer");
            assert_eq!(display_name("office--laser_2"), "office laser 2");
            assert_eq!(display_name("  Nearby_Printer  "), "Nearby Printer");
        }

        eprintln!("phase device_reasons_are_translated_without_leaking_backend_jargon");
        {
            assert_eq!(
                friendly_reason("media-empty-warning"),
                "Add paper to continue."
            );
            assert_eq!(
                friendly_reason("marker-supply-low-warning"),
                "A supply is running low; printing can usually continue for now."
            );
            assert_eq!(
                friendly_reason("vendor-specific-0x92"),
                "The printer reported a condition that may need attention."
            );
        }
    }

    #[test]
    fn scan_output_is_opaque_byte_exact_private_and_previewable() {
        eprintln!("phase scan_result_fixture_is_opaque_and_visually_structured");
        {
            let pixels = demo_scanned_page_pixels(320, 440);
            assert_eq!(pixels.len(), 320 * 440 * 4);
            assert!(
                pixels
                    .chunks_exact(4)
                    .all(|pixel| pixel.last() == Some(&255))
            );
            let distinct = pixels
                .chunks_exact(4)
                .map(<[u8]>::to_vec)
                .collect::<std::collections::BTreeSet<_>>();
            assert!(
                distinct.len() >= 4,
                "fixture must expose blank-preview regressions"
            );
        }

        eprintln!("phase scanned_pages_are_saved_byte_exactly_with_private_permissions");
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("receipt.png");
            let bytes = b"deterministic scanned page bytes";
            save_scanned_page(&path, bytes).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600);
            }
        }

        eprintln!("phase selected_image_produces_real_bounded_preview_pixels");
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("boarding-pass.png");
            let mut source =
                image::RgbaImage::from_pixel(1_200, 600, image::Rgba([245, 245, 240, 255]));
            for y in 80..180 {
                for x in 120..1_080 {
                    source.put_pixel(x, y, image::Rgba([24, 92, 170, 255]));
                }
            }
            source.save(&path).unwrap();

            let preview = load_print_preview(&path).expect("a selected PNG has a real preview");
            assert!(preview.is_valid());
            assert_eq!(preview.max_edge(), 720);
            assert!(
                preview
                    .rgba
                    .chunks_exact(4)
                    .any(|pixel| pixel[..3] == [24, 92, 170]),
                "preview pixels come from the selected document"
            );
        }
    }

    fn scanned_page(width: u32) -> ScannedPage {
        ScannedPage {
            png: Arc::from(Vec::<u8>::new()),
            rgba: Arc::from(vec![
                255;
                usize::try_from(width).unwrap_or(0).saturating_mul(4)
            ]),
            width,
            height: 1,
            recognized_text: None,
            ocr_note: "No text was found on this page.".into(),
            ocr_language: "English".into(),
            ocr_needs_refresh: false,
            ocr_in_progress: false,
        }
    }

    #[test]
    fn scan_page_edits_invalidate_ocr_and_export_in_order() {
        eprintln!("phase multipage_scan_session_preserves_reorders_navigates_and_deletes_pages");
        {
            let mut centre = PrintCentre::new();
            centre.show(Some(&demo_snapshot()));
            centre.open_scan();
            centre.scan_finished(Ok(scanned_page(10)));
            centre.scan_another();
            assert_eq!(
                centre.scan.pages.len(),
                1,
                "scan another preserves page one"
            );
            centre.scan_finished(Ok(scanned_page(20)));
            assert_eq!(centre.scan.selected, 1);

            centre.move_scan_page_earlier();
            assert_eq!(centre.scan.selected, 0);
            assert_eq!(centre.scan.pages[0].width, 20);
            centre.next_scan_page();
            assert_eq!(centre.scan.result.as_ref().map(|page| page.width), Some(10));

            centre.remove_scan_page();
            assert_eq!(centre.scan.pages.len(), 1);
            assert_eq!(centre.scan.selected, 0);
            assert_eq!(centre.scan.result.as_ref().map(|page| page.width), Some(20));
        }

        eprintln!("phase rotation_updates_saved_pixels_and_invalidates_stale_ocr");
        {
            let mut page = scanned_page(3);
            page.recognized_text = Some("stale orientation".into());
            page.rotate_clockwise().unwrap();
            assert_eq!((page.width, page.height), (1, 3));
            assert!(!page.png.is_empty());
            let decoded = image::load_from_memory(&page.png).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (1, 3));
            assert!(page.recognized_text.is_none());
            assert!(page.ocr_needs_refresh);
        }

        eprintln!("phase crop_updates_preview_and_export_bytes_and_invalidates_ocr");
        {
            let mut page = ScannedPage {
                png: Arc::from(Vec::<u8>::new()),
                rgba: Arc::from(vec![255; 8 * 6 * 4]),
                width: 8,
                height: 6,
                recognized_text: Some("stale text".into()),
                ocr_note: "Recognized 2 words.".into(),
                ocr_language: "English".into(),
                ocr_needs_refresh: false,
                ocr_in_progress: false,
            };
            page.crop_to(CropSelection {
                left: 0.25,
                top: 0.0,
                right: 0.75,
                bottom: 0.5,
            })
            .unwrap();
            assert_eq!((page.width, page.height), (4, 3));
            assert_eq!(page.rgba.len(), 4 * 3 * 4);
            assert!(image::load_from_memory(&page.png).is_ok());
            assert!(page.recognized_text.is_none());
            assert!(page.ocr_needs_refresh);
        }

        eprintln!("phase whole_document_export_keeps_page_order_and_ocr_layer");
        {
            let mut first = scanned_page(3);
            first.recognized_text = Some("FIRST PAGE".into());
            let mut second = scanned_page(5);
            second.recognized_text = Some("SECOND PAGE".into());
            let bytes = scanned_document_pdf(&[first, second]);
            let pdf = String::from_utf8_lossy(&bytes);
            assert!(pdf.starts_with("%PDF-1.7"));
            assert!(pdf.contains("/Count 2"));
            assert!(pdf.find("FIRST PAGE") < pdf.find("SECOND PAGE"));
            assert!(pdf.contains("3 Tr"));
        }
    }
}
