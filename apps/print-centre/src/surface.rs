use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use taffy::{NodeId, TaffyTree};
use tilo_actions::{ActionSpec, OperationKind, Resource};
use ui::icons::Icon;
use ui::widget::toggle_group::Segment;
use ui::widget::{AppModel, ModelPath};
use ui::widget::{
    Banner, Button, Card, CardGrid, Column, Container, CropSelection, Divider, Dropdown,
    IconWidget, Image, ImageCropEditor, NumberInput, PagePreview, ProgressBar, Row, ScrollAxis,
    Scrollable, Spacer, Text, Toggle, ToggleGroup, Widget,
};
use ui::{
    presence::PresenceCtx,
    text_render::{TextRenderer, TextWeight},
    theme::Theme,
};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
};
use winit::event::WindowEvent;

type El = Box<dyn Widget>;

/// The one typed door for local printer and scanner operations. The commands
/// remain local because they talk to CUPS/SANE and portals on this machine;
/// publishing them still lets the shell, search, and a collaborating agent
/// discover the real controls and receive an invocation receipt.
pub(crate) const PRINT_CENTRE_OPERATION: &str = "print-centre.operation";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DemoScenario {
    Dashboard,
    Active,
    Attention,
    Empty,
    Queue,
    Device,
    Discovery,
    PrintSetup,
    ScanSetup,
    ScanResult,
    History,
    Diagnostics,
    ServiceError,
}
pub struct PrintCentre {
    inner: El,
    refresh: Option<mpsc::Receiver<Snapshot>>,
    commands_tx: mpsc::Sender<UiCommand>,
    commands_rx: mpsc::Receiver<UiCommand>,
    snapshot: Option<Snapshot>,
    notice: Option<OperationNotice>,
    pending_cancel: Option<String>,
    picker: Option<ui::portal::PortalChannel>,
    pending_document: Option<PathBuf>,
    print_preview: PrintPreview,
    print_preview_rx: Option<mpsc::Receiver<Option<thumbnail::RgbaThumb>>>,
    selected_printer: Option<String>,
    print_copies: ui::LocalState<f64>,
    print_two_sided: ui::LocalState<bool>,
    selected_scanner: Option<String>,
    scan: ScanState,
    scan_save_picker: Option<ui::portal::PortalChannel>,
    scan_save_document: bool,
    pub(crate) actions: crate::resource::ActionSink,
    hosted: bool,
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

#[derive(Clone)]
struct Snapshot {
    queues: Result<Vec<os_actions::printing::PrinterQueue>, String>,
    jobs: Result<Vec<os_actions::printing::PrintJob>, String>,
    scanners: Result<Vec<os_actions::printing::Scanner>, String>,
}

#[derive(Clone)]
enum UiCommand {
    Refresh,
    Dashboard,
    Queue,
    History,
    Diagnostics,
    Discover,
    DiscoveryFinished(Result<Vec<os_actions::printing::DiscoveredPrinter>, String>),
    AddPrinter(os_actions::printing::DiscoveredPrinter),
    Device(String),
    SetDefault(String),
    SetEnabled(String, bool),
    RequestCancel(String),
    ConfirmCancel(String),
    RetryJob(String),
    EffectFinished(OperationNotice),
    ChooseDocument,
    SelectPrinter(String),
    PrintDocument,
    CancelPrintSetup,
    ScanSetup,
    SelectScanner(String),
    ScanPage,
    ScanFinished(Result<ScannedPage, String>),
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
    RecognitionFinished(Arc<[u8]>, Result<(Option<String>, String), String>),
}

#[derive(Clone)]
struct OperationNotice {
    success: bool,
    title: String,
    message: String,
}

fn title(s: &str, size: f32) -> El {
    Box::new(
        Text::new(s)
            .with_size(size)
            .with_weight(TextWeight::Semibold),
    )
}
fn copy(s: &str) -> El {
    Box::new(Text::new(s).with_size(14.0))
}
fn muted(s: &str) -> El {
    Box::new(Text::new(s).with_size(13.0).dimmed())
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

fn notice_banner(notice: &OperationNotice) -> El {
    let banner = if notice.success {
        Banner::success(&notice.title)
    } else {
        Banner::error(&notice.title)
    };
    Box::new(banner.with_message(&notice.message).dismissible())
}

fn unavailable_card(areas: &[&str], tx: &mpsc::Sender<UiCommand>) -> Option<El> {
    if areas.is_empty() {
        return None;
    }
    let subject = match areas {
        ["printers"] => "Printers couldn't be checked",
        ["documents"] => "Print jobs couldn't be checked",
        ["scanners"] => "Scanners couldn't be checked",
        _ => "Some device information couldn't be checked",
    };
    let detail = format!(
        "Print Centre couldn't get {} right now. Your saved devices and documents have not been removed.",
        areas.join(" or ")
    );
    Some(Box::new(Card::titled_with_action(
        subject,
        icon_command_action(
            "print-centre.service.retry",
            "Try again",
            Icon::Refresh,
            tx,
            UiCommand::Refresh,
        ),
        copy(&detail),
    )))
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
fn gap(px: f32) -> El {
    Box::new(Container::new().with_no_padding().with_height(px))
}

fn centered_page(content: El, max_width: f32) -> El {
    Box::new(
        Row::new()
            .push(Box::new(Spacer::new()))
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_max_width(max_width)
                    .filling()
                    .with_child(content),
            ))
            .push(Box::new(Spacer::new())),
    )
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
fn action(id: &str, label: &str) -> El {
    Box::new(
        Button::new(id, label, None)
            .fit_content(44.0)
            .on_view(|| {}),
    )
}

pub(crate) fn resource_action(
    id: &str,
    label: &str,
    kind: OperationKind,
    command: &str,
) -> Option<ActionSpec> {
    ActionSpec::operation(id, label, kind, PRINT_CENTRE_OPERATION, &command).ok()
}

/// Resource identifiers are shell-facing rather than display text. Keep the
/// externally visible address valid and stable even when CUPS reports spaces,
/// punctuation, or non-ASCII queue names.
fn resource_key(value: &str) -> String {
    hex::encode(value)
}

fn printer_resource(queue: &os_actions::printing::PrinterQueue) -> Resource {
    let key = resource_key(&queue.name);
    let mut resource = Resource::new(
        format!("print-centre.printer.{key}"),
        "print-centre.printer",
        display_name(&queue.name),
    );
    if let Some(action) = resource_action(
        "open",
        "Open printer",
        OperationKind::View,
        &format!("device\0{}", queue.name),
    ) {
        resource.actions.push(action);
    }
    if !queue.default
        && let Some(action) = resource_action(
            "set-default",
            "Set as default printer",
            OperationKind::Effect,
            &format!("default\0{}", queue.name),
        )
    {
        resource.actions.push(action);
    }
    let (label, command) = if queue.accepting_jobs {
        ("Pause printer", "pause")
    } else {
        ("Resume printer", "resume")
    };
    if let Some(action) = resource_action(
        "toggle-enabled",
        label,
        OperationKind::Effect,
        &format!("{command}\0{}", queue.name),
    ) {
        resource.actions.push(action);
    }
    resource
}

fn job_resource(job: &os_actions::printing::PrintJob) -> Resource {
    let key = resource_key(&job.id);
    let mut resource = Resource::new(
        format!("print-centre.job.{key}"),
        "print-centre.job",
        job_title(job),
    );
    if let Some(action) = resource_action("open-queue", "Open queue", OperationKind::View, "queue")
    {
        resource.actions.push(action);
    }
    if matches!(
        job.state,
        Some(
            os_actions::printing::JobState::Pending
                | os_actions::printing::JobState::Held
                | os_actions::printing::JobState::Processing
                | os_actions::printing::JobState::Stopped
        )
    ) && let Some(action) = resource_action(
        "cancel",
        "Cancel document",
        OperationKind::Effect,
        &format!("cancel\0{}", job.id),
    ) {
        resource.actions.push(action);
    }
    if matches!(
        job.state,
        Some(
            os_actions::printing::JobState::Stopped
                | os_actions::printing::JobState::Canceled
                | os_actions::printing::JobState::Aborted
        )
    ) && let Some(action) = resource_action(
        "retry",
        "Print again",
        OperationKind::Effect,
        &format!("retry\0{}", job.id),
    ) {
        resource.actions.push(action);
    }
    resource
}

fn scanner_resource(scanner: &os_actions::printing::Scanner) -> Resource {
    let key = resource_key(&scanner.device);
    let mut resource = Resource::new(
        format!("print-centre.scanner.{key}"),
        "print-centre.scanner",
        scanner.description.clone(),
    );
    if let Some(action) = resource_action(
        "scan",
        "Scan a document",
        OperationKind::View,
        &format!("scan-device\0{}", scanner.device),
    ) {
        resource.actions.push(action);
    }
    resource
}

fn command_action(id: &str, label: &str, tx: &mpsc::Sender<UiCommand>, command: UiCommand) -> El {
    let tx = tx.clone();
    Box::new(
        Button::new(id, label, None)
            .fit_content(44.0)
            .on_effect(move || {
                let _ = tx.send(command.clone());
            }),
    )
}

fn icon_command_action(
    id: &str,
    label: &str,
    icon: Icon,
    tx: &mpsc::Sender<UiCommand>,
    command: UiCommand,
) -> El {
    let tx = tx.clone();
    Box::new(
        Button::new(id, label, None)
            .with_icon(icon)
            .fit_content(44.0)
            .on_effect(move || {
                let _ = tx.send(command.clone());
            }),
    )
}

fn primary_command_action(
    id: &str,
    label: &str,
    tx: &mpsc::Sender<UiCommand>,
    command: UiCommand,
) -> El {
    let tx = tx.clone();
    Box::new(
        Button::new(id, label, None)
            .fit_content(44.0)
            .primary()
            .on_effect(move || {
                let _ = tx.send(command.clone());
            }),
    )
}

fn primary_icon_command_action(
    id: &str,
    label: &str,
    icon: Icon,
    tx: &mpsc::Sender<UiCommand>,
    command: UiCommand,
) -> El {
    let tx = tx.clone();
    Box::new(
        Button::new(id, label, None)
            .with_icon(icon)
            .fit_content(44.0)
            .primary()
            .on_effect(move || {
                let _ = tx.send(command.clone());
            }),
    )
}

fn header(subtitle: &str) -> El {
    Box::new(
        Row::new()
            .push(Box::new(
                Container::new()
                    .with_min_width(220.0)
                    .with_flex_grow()
                    .with_no_padding()
                    .with_child(Box::new(
                        Column::new()
                            .with_no_padding()
                            .with_intrinsic_height()
                            .push(title("Print Centre", 28.0))
                            .push(muted(subtitle)),
                    )),
            ))
            .push(Box::new(
                Container::new().with_width(300.0).with_child(Box::new(
                    Row::new()
                        .no_wrap()
                        .push(action("print-centre.add", "Add printer"))
                        .push(action("print-centre.print", "Print a document")),
                )),
            )),
    )
}

fn live_header(subtitle: &str, tx: &mpsc::Sender<UiCommand>) -> El {
    Box::new(
        // The title and document actions need independently measured rows.
        // A fixed-width, non-wrapping row clipped the primary action at large
        // text sizes; this row can use the page width and wrap on a phone.
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(title("Print Centre", 28.0))
                    .push(muted(subtitle)),
            ))
            .push(gap(14.0))
            .push(Box::new(
                Row::new()
                    .push(icon_command_action(
                        "print-centre.add",
                        "Add printer",
                        Icon::Plus,
                        tx,
                        UiCommand::Discover,
                    ))
                    .push(primary_icon_command_action(
                        "print-centre.print",
                        "Print a document",
                        Icon::Printer,
                        tx,
                        UiCommand::ChooseDocument,
                    )),
            )),
    )
}

/// App-local semantic boundary for a measured dashboard surface. It lets the
/// headless contract assert real surface bounds without changing shared UI.
struct DashboardCard {
    inner: El,
}

impl DashboardCard {
    fn new(inner: El) -> Self {
        Self { inner }
    }
}

impl Widget for DashboardCard {
    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        self.inner.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.inner.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.inner.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.inner.draw(scene, transform, text, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.inner.handle_event(event, ctx);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.inner.as_ref()]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.inner.as_mut()]
    }

    fn get_rect(&self) -> Rect {
        self.inner.get_rect()
    }

    fn name(&self) -> &'static str {
        "DashboardCard"
    }
}

/// Dashboard cards deliberately use one intrinsic column. This makes a
/// growing preview, supply gauge, and footer contribute to the same surface
/// measurement instead of allowing a fixed/card-grid row to paint below its
/// reported bounds.
fn dashboard_card(title_text: impl Into<String>, body: El, footer: Option<El>) -> El {
    let mut content = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(title(&title_text.into(), 18.0))
        .push(gap(14.0))
        .push(body);
    if let Some(footer) = footer {
        content = content.push(gap(16.0)).push(footer);
    }
    Box::new(DashboardCard::new(Box::new(
        Container::new()
            .with_background()
            .with_child(Box::new(content)),
    )))
}

fn subpage_header(title_text: &str, subtitle: &str, tx: &mpsc::Sender<UiCommand>) -> El {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(icon_command_action(
                "print-centre.back",
                "Print Centre",
                Icon::ArrowLeft,
                tx,
                UiCommand::Dashboard,
            ))
            .push(gap(14.0))
            .push(title(title_text, 28.0))
            .push(muted(subtitle)),
    )
}

fn loading_view() -> El {
    Box::new(
        Column::new()
            .with_intrinsic_height()
            .push(header("Checking printers and documents"))
            .push(gap(32.0))
            .push(Box::new(Card::titled(
                "Getting things ready",
                Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .push(copy(
                            "Looking for printers, scanners, and unfinished documents.",
                        ))
                        .push(gap(16.0))
                        .push(Box::new(
                            ProgressBar::indeterminate().with_label("Loading Print Centre"),
                        )),
                ),
            ))),
    )
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

/// Keep supply text and its visual meter on separate rows so enlarged text
/// cannot be painted through by the meter's fill.
fn supply_meter(name: &str, level: f32, detail: &str) -> El {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(copy(&format!("{name}: {detail}")))
            .push(gap(6.0))
            .push(Box::new(
                ProgressBar::new(level).with_label(format!("{name} level")),
            )),
    )
}

fn live_job_card(job: &os_actions::printing::PrintJob, tx: &mpsc::Sender<UiCommand>) -> El {
    let progress = f32::from(job.progress_percent().unwrap_or(0)) / 100.0;
    let page_count = job.impressions_total.unwrap_or(1).max(1);
    let page = job
        .impressions_completed
        .unwrap_or(0)
        .saturating_add(1)
        .min(page_count);
    let status = match job.state {
        Some(os_actions::printing::JobState::Held) => "Waiting to print",
        Some(os_actions::printing::JobState::Stopped) => "Needs attention",
        _ => "Printing now",
    };
    // The direct measured surface lets wide windows use the space beside the
    // preview, while the flexible detail column wraps below it on a phone.
    dashboard_card(
        status,
        Box::new(
            Row::new()
                .align_start()
                .push(Box::new(PagePreview::new(
                    "Current document",
                    page,
                    page_count,
                )))
                .push(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_flex_grow()
                        .with_min_width(260.0)
                        .with_child(Box::new(
                            Column::new()
                                .with_no_padding()
                                .with_intrinsic_height()
                                .push(title(&job_title(job), 17.0))
                                .push(muted(&format!(
                                    "Sent to {}",
                                    display_name(&job.destination)
                                )))
                                .push(gap(12.0))
                                .push(Box::new(
                                    ProgressBar::new(progress)
                                        .with_label("Current document progress"),
                                )),
                        )),
                )),
        ),
        Some(Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(command_action(
                    "print-centre.queue",
                    "Open queue",
                    tx,
                    UiCommand::Queue,
                ))
                .push(gap(10.0))
                .push(command_action(
                    "print-centre.job.cancel",
                    "Cancel",
                    tx,
                    UiCommand::RequestCancel(job.id.clone()),
                )),
        )),
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "dashboard composition keeps the snapshot-derived hierarchy and empty/active states auditable in one place"
)]
fn live_view(
    snapshot: &Snapshot,
    tx: &mpsc::Sender<UiCommand>,
    notice: Option<&OperationNotice>,
) -> El {
    let queues = snapshot.queues.as_deref().unwrap_or(&[]);
    let jobs = snapshot.jobs.as_deref().unwrap_or(&[]);
    let scanners = snapshot.scanners.as_deref().unwrap_or(&[]);
    let active: Vec<_> = jobs
        .iter()
        .filter(|job| {
            matches!(
                job.state,
                Some(
                    os_actions::printing::JobState::Pending
                        | os_actions::printing::JobState::Held
                        | os_actions::printing::JobState::Processing
                        | os_actions::printing::JobState::Stopped
                )
            )
        })
        .collect();
    let subtitle =
        if snapshot.queues.is_err() || snapshot.jobs.is_err() || snapshot.scanners.is_err() {
            "Some information needs refreshing".to_string()
        } else {
            match active.len() {
                0 => "Everything is ready".to_string(),
                1 => "One document is active".to_string(),
                n => format!("{n} documents are active"),
            }
        };
    let mut body = Column::new()
        .with_intrinsic_height()
        .push(live_header(&subtitle, tx))
        .push(gap(20.0));
    if let Some(notice) = notice {
        body = body.push(notice_banner(notice)).push(gap(14.0));
    }
    if let Some(issue) = unavailable_card(&failed_areas(snapshot, true, true, true), tx) {
        body = body.push(issue).push(gap(14.0));
    }
    if let Some(job) = active.first() {
        body = body.push(live_job_card(job, tx)).push(gap(28.0));
    }
    let cards: Vec<El> = queues
        .iter()
        .map(|queue| {
            let detail = if queue.default {
                "Preferred printer"
            } else {
                "Available to your apps"
            };
            let mut details = Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(copy(friendly_queue_state(queue)))
                .push(gap(10.0));
            details = if let Some((level, percent)) = supply_level(queue) {
                details.push(supply_meter(
                    "Lowest supply",
                    level,
                    &format!("{percent}% remaining"),
                ))
            } else {
                details.push(muted("Supply levels unavailable"))
            };
            details = details.push(gap(8.0)).push(muted(detail));
            dashboard_card(
                display_name(&queue.name),
                Box::new(
                    Row::new()
                        .align_start()
                        .push(Box::new(IconWidget::new(Icon::Printer, 34.0).dim()))
                        .push(Box::new(details)),
                ),
                Some(icon_command_action(
                    &format!("print-centre.device.open.{}", queue.name),
                    "Open",
                    Icon::ArrowRight,
                    tx,
                    UiCommand::Device(queue.name.clone()),
                )),
            )
        })
        .collect();
    if snapshot.queues.is_ok() {
        body = body
            .push(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(title("Your printers", 19.0))
                    .push(gap(8.0))
                    .push(icon_command_action(
                        "print-centre.refresh",
                        "Refresh",
                        Icon::Refresh,
                        tx,
                        UiCommand::Refresh,
                    )),
            ))
            .push(gap(10.0));
        if queues.is_empty() {
            body = body.push(Box::new(Card::titled_with_action(
                "No printers added",
                icon_command_action(
                    "print-centre.discover.empty",
                    "Find a printer",
                    Icon::Search,
                    tx,
                    UiCommand::Discover,
                ),
                copy("You can still scan documents below. Add a printer whenever you need one."),
            )));
        } else {
            // Printer cards remain a measured vertical sequence. It avoids a
            // grid row whose height is resolved before variable telemetry has
            // finished measuring at a larger text scale.
            let mut printer_cards = Column::new().with_no_padding().with_intrinsic_height();
            for card in cards {
                printer_cards = printer_cards.push(card).push(gap(14.0));
            }
            body = body.push(Box::new(printer_cards));
        }
    }
    if let Some(scanner) = scanners.first() {
        body = body.push(gap(18.0)).push(dashboard_card(
            "Scan a document",
            Box::new(
                Row::new()
                    .align_start()
                    .push(Box::new(IconWidget::new(Icon::Camera, 30.0).dim()))
                    .push(copy(&format!("{} is ready", scanner.description))),
            ),
            Some(primary_icon_command_action(
                "print-centre.scan.open",
                "Start scanning",
                Icon::Camera,
                tx,
                UiCommand::ScanSetup,
            )),
        ));
    }
    let mut utility_cards = Column::new().with_no_padding().with_intrinsic_height();
    if snapshot.jobs.is_ok() {
        utility_cards =
            utility_cards.push(Box::new(Container::new().with_no_padding().with_child(
                dashboard_card(
                    "Recent documents",
                    copy(
                        if jobs.iter().any(|job| {
                            matches!(job.state, Some(os_actions::printing::JobState::Completed))
                        }) {
                            "Review what printed and quickly print a retained job again."
                        } else {
                            "Completed documents will appear here."
                        },
                    ),
                    Some(icon_command_action(
                        "print-centre.history.open",
                        "See all",
                        Icon::History,
                        tx,
                        UiCommand::History,
                    )),
                ),
            )));
        utility_cards = utility_cards.push(gap(14.0));
    }
    utility_cards = utility_cards.push(Box::new(Container::new().with_no_padding().with_child(
        dashboard_card(
            "Device help",
            copy("Plain-language checks for connections, paper, supplies, and scanners."),
            Some(icon_command_action(
                "print-centre.diagnostics.open",
                "Check devices",
                Icon::Info,
                tx,
                UiCommand::Diagnostics,
            )),
        ),
    )));
    body = body.push(gap(18.0)).push(Box::new(utility_cards));
    centered_page(
        Box::new(Scrollable::new(ScrollAxis::Vertical).with_child(Box::new(body))),
        1120.0,
    )
}

fn history_view(snapshot: &Snapshot, tx: &mpsc::Sender<UiCommand>) -> El {
    let jobs = snapshot.jobs.as_deref().unwrap_or(&[]);
    let terminal: Vec<_> = jobs
        .iter()
        .filter(|job| {
            matches!(
                job.state,
                Some(
                    os_actions::printing::JobState::Completed
                        | os_actions::printing::JobState::Canceled
                        | os_actions::printing::JobState::Aborted
                )
            )
        })
        .collect();
    let mut body = Column::new()
        .with_intrinsic_height()
        .push(subpage_header(
            "Print history",
            "Documents retained by your print service",
            tx,
        ))
        .push(gap(20.0));
    if let Some(issue) = unavailable_card(&failed_areas(snapshot, false, true, false), tx) {
        body = body.push(issue).push(gap(14.0));
    }
    if terminal.is_empty() {
        body = body.push(Box::new(Card::titled(
            "No completed documents yet",
            copy("After a document finishes or is canceled, its status will appear here."),
        )));
    } else {
        for job in terminal.into_iter().take(50) {
            let status = match job.state {
                Some(os_actions::printing::JobState::Completed) => "Printed",
                Some(os_actions::printing::JobState::Canceled) => "Canceled",
                _ => "Did not print",
            };
            let pages = job.impressions_total.map_or_else(
                || "Page count unavailable".to_string(),
                |count| format!("{count} page{}", if count == 1 { "" } else { "s" }),
            );
            let details = Box::new(
                Row::new()
                    .push(Box::new(PagePreview::new(
                        "Printed document",
                        job.impressions_total.unwrap_or(1).max(1),
                        job.impressions_total.unwrap_or(1).max(1),
                    )))
                    .push(Box::new(
                        Column::new()
                            .with_no_padding()
                            .with_intrinsic_height()
                            .push(copy(status))
                            .push(muted(&format!(
                                "{pages}, sent to {}, {}",
                                display_name(&job.destination),
                                job.submitted
                            )))
                            .push(gap(12.0))
                            .push(primary_icon_command_action(
                                &format!("print-centre.history.repeat.{}", job.id),
                                "Print again",
                                Icon::Refresh,
                                tx,
                                UiCommand::RetryJob(job.id.clone()),
                            )),
                    )),
            );
            body = body
                .push(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_max_width(840.0)
                        .with_child(Box::new(Card::titled(job_title(job), details))),
                ))
                .push(gap(12.0));
        }
    }
    centered_page(
        Box::new(Scrollable::new_vertical().with_child(Box::new(body))),
        920.0,
    )
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

#[allow(
    clippy::too_many_lines,
    reason = "diagnostic cards are a linear translation of queue, supply, and scanner state into people-facing checks"
)]
fn diagnostics_view(snapshot: &Snapshot, tx: &mpsc::Sender<UiCommand>) -> El {
    let queues = snapshot.queues.as_deref().unwrap_or(&[]);
    let scanners = snapshot.scanners.as_deref().unwrap_or(&[]);
    let responding = queues
        .iter()
        .filter(|queue| queue.telemetry.available)
        .count();
    let supply_reports = queues
        .iter()
        .filter(|queue| {
            queue
                .telemetry
                .supplies
                .as_ref()
                .is_some_and(|items| !items.is_empty())
        })
        .count();
    let printer_word = |count: usize| if count == 1 { "printer" } else { "printers" };
    let scanner_word = if scanners.len() == 1 {
        "scanner"
    } else {
        "scanners"
    };
    let mut summary = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(copy(&format!(
            "{responding} of {} {} responded",
            queues.len(),
            printer_word(queues.len())
        )))
        .push(gap(8.0))
        .push(copy(&format!(
            "{supply_reports} {} report ink or toner levels",
            printer_word(supply_reports)
        )))
        .push(gap(8.0))
        .push(copy(&format!(
            "{} {scanner_word} available",
            scanners.len()
        )));
    if let Some(issue) = unavailable_card(&failed_areas(snapshot, true, false, true), tx) {
        summary = summary.push(gap(14.0)).push(issue);
    }
    let mut body = Column::new()
        .with_intrinsic_height()
        .push(subpage_header(
            "Device help",
            "Useful checks without printer jargon",
            tx,
        ))
        .push(gap(20.0))
        .push(Box::new(Card::titled_with_action(
            "Connection check",
            command_action(
                "print-centre.diagnostics.refresh",
                "Check again",
                tx,
                UiCommand::Refresh,
            ),
            Box::new(summary),
        )))
        .push(gap(16.0));
    for queue in queues {
        let mut checks = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(copy(friendly_queue_state(queue)));
        if queue.telemetry.available {
            checks = checks.push(muted("The printer answered the latest status check."));
        } else {
            checks = checks.push(muted(
                "Live details are unavailable. The printer may be asleep or disconnected.",
            ));
        }
        for reason in queue.telemetry.state_reasons.iter().take(3) {
            checks = checks.push(gap(8.0)).push(copy(friendly_reason(reason)));
        }
        if let Some(supplies) = &queue.telemetry.supplies {
            for supply in supplies.iter().take(6) {
                if let Some(percent) = supply.level_percent {
                    checks = checks.push(gap(10.0)).push(supply_meter(
                        &supply.name,
                        f32::from(percent) / 100.0,
                        &format!("{percent}% remaining"),
                    ));
                }
            }
        }
        checks = checks.push(gap(12.0)).push(command_action(
            "print-centre.diagnostics.device",
            "Open printer controls",
            tx,
            UiCommand::Device(queue.name.clone()),
        ));
        body = body
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_max_width(840.0)
                    .with_child(Box::new(Card::titled(
                        display_name(&queue.name),
                        Box::new(checks),
                    ))),
            ))
            .push(gap(12.0));
    }
    let scanner_copy = if scanners.is_empty() {
        "No scanner was found. Check its cable or network connection, then refresh."
    } else {
        "Your scanner is available for a new document."
    };
    body = body
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_max_width(840.0)
                .with_child(Box::new(Card::titled_with_action(
                    "Scanner",
                    if scanners.is_empty() {
                        command_action(
                            "print-centre.diagnostics.refresh",
                            "Check again",
                            tx,
                            UiCommand::Refresh,
                        )
                    } else {
                        command_action(
                            "print-centre.diagnostics.scan",
                            "Start scanning",
                            tx,
                            UiCommand::ScanSetup,
                        )
                    },
                    copy(scanner_copy),
                ))),
        ))
        .push(gap(12.0));
    centered_page(
        Box::new(Scrollable::new_vertical().with_child(Box::new(body))),
        920.0,
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "the queue's state-to-shared-widget composition is clearer kept in one linear view builder"
)]
fn queue_view(
    snapshot: &Snapshot,
    tx: &mpsc::Sender<UiCommand>,
    pending_cancel: Option<&str>,
    notice: Option<&OperationNotice>,
) -> El {
    let jobs = snapshot.jobs.as_deref().unwrap_or(&[]);
    let mut body = Column::new()
        .with_intrinsic_height()
        .push(subpage_header(
            "Print queue",
            "Every document, in one place",
            tx,
        ))
        .push(gap(20.0));
    if let Some(notice) = notice {
        body = body.push(notice_banner(notice)).push(gap(14.0));
    }
    if let Some(issue) = unavailable_card(&failed_areas(snapshot, false, true, false), tx) {
        body = body.push(issue).push(gap(14.0));
    }
    if pending_cancel.is_some() {
        body = body
            .push(Box::new(
                Banner::warning("Cancel this document?")
                    .with_message("It will be removed from the printer. This cannot be undone."),
            ))
            .push(gap(14.0));
    }
    if jobs.is_empty() {
        body = body.push(Box::new(Card::titled(
            "Nothing waiting",
            copy("New documents will appear here as soon as an app sends them."),
        )));
    } else {
        let mut cards: Vec<El> = Vec::new();
        for job in jobs.iter().take(24) {
            let state = match job.state {
                Some(os_actions::printing::JobState::Completed) => "Printed",
                Some(os_actions::printing::JobState::Canceled) => "Canceled",
                Some(os_actions::printing::JobState::Aborted) => "Could not print",
                Some(os_actions::printing::JobState::Held) => "Waiting",
                Some(os_actions::printing::JobState::Stopped) => "Needs attention",
                _ => "In progress",
            };
            let mut actions = Row::new();
            if matches!(
                job.state,
                Some(
                    os_actions::printing::JobState::Stopped
                        | os_actions::printing::JobState::Canceled
                        | os_actions::printing::JobState::Aborted
                )
            ) {
                actions = actions.push(command_action(
                    "print-centre.job.retry",
                    "Try again",
                    tx,
                    UiCommand::RetryJob(job.id.clone()),
                ));
            }
            if !matches!(
                job.state,
                Some(
                    os_actions::printing::JobState::Completed
                        | os_actions::printing::JobState::Canceled
                )
            ) {
                let confirming = pending_cancel == Some(job.id.as_str());
                actions = actions.push(command_action(
                    "print-centre.job.cancel",
                    if confirming {
                        "Confirm cancel"
                    } else {
                        "Cancel"
                    },
                    tx,
                    if confirming {
                        UiCommand::ConfirmCancel(job.id.clone())
                    } else {
                        UiCommand::RequestCancel(job.id.clone())
                    },
                ));
            }
            cards.push(Box::new(Card::titled(
                job_title(job),
                Box::new(
                    Row::new()
                        .push(Box::new(PagePreview::new(
                            "Queued document",
                            job.impressions_completed
                                .unwrap_or(0)
                                .saturating_add(1)
                                .min(job.impressions_total.unwrap_or(1).max(1)),
                            job.impressions_total.unwrap_or(1).max(1),
                        )))
                        .push(Box::new(
                            Column::new()
                                .with_no_padding()
                                .with_intrinsic_height()
                                .push(copy(state))
                                .push(muted(&format!(
                                    "Sent to {}",
                                    display_name(&job.destination)
                                )))
                                .push(gap(10.0))
                                .push(Box::new(
                                    ProgressBar::new(
                                        f32::from(job.progress_percent().unwrap_or(0)) / 100.0,
                                    )
                                    .with_label("Document progress"),
                                ))
                                .push(gap(10.0))
                                .push(Box::new(actions)),
                        )),
                ),
            )));
        }
        body = body.push(Box::new(
            CardGrid::new(cards)
                .with_min_card_width(420.0)
                .with_card_height(330.0),
        ));
    }
    centered_page(
        Box::new(Scrollable::new(ScrollAxis::Vertical).with_child(Box::new(body))),
        920.0,
    )
}

fn device_view(queue: &os_actions::printing::PrinterQueue, tx: &mpsc::Sender<UiCommand>) -> El {
    let enabled =
        queue.accepting_jobs && !matches!(queue.state, os_actions::printing::QueueState::Paused);
    let mut actions = Row::new().push(command_action(
        "print-centre.device.enabled",
        if enabled {
            "Pause printer"
        } else {
            "Resume printer"
        },
        tx,
        UiCommand::SetEnabled(queue.name.clone(), !enabled),
    ));
    if !queue.default {
        actions = actions.push(command_action(
            "print-centre.device.default",
            "Make preferred",
            tx,
            UiCommand::SetDefault(queue.name.clone()),
        ));
    }
    actions = actions
        .push(command_action(
            "print-centre.device.queue",
            "View print queue",
            tx,
            UiCommand::Queue,
        ))
        .push(command_action(
            "print-centre.device.diagnostics",
            "Check printer",
            tx,
            UiCommand::Diagnostics,
        ));
    let supplies: El = match &queue.telemetry.supplies {
        Some(items) if !items.is_empty() => {
            let mut column = Column::new().with_no_padding().with_intrinsic_height();
            for supply in items {
                let percent = supply.level_percent.unwrap_or(0);
                column = column
                    .push(supply_meter(
                        &supply.name,
                        f32::from(percent) / 100.0,
                        &format!("{percent}% remaining"),
                    ))
                    .push(gap(10.0));
            }
            Box::new(column)
        }
        _ => copy("This printer does not report supply levels."),
    };
    let hero = Box::new(
        Row::new()
            .push(Box::new(IconWidget::new(Icon::Printer, 52.0)))
            .push(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(title(friendly_queue_state(queue), 21.0))
                    .push(muted(if queue.default {
                        "Preferred printer"
                    } else {
                        "Available to your apps"
                    })),
            )),
    );
    let body = Column::new()
        .with_intrinsic_height()
        .push(subpage_header(
            &display_name(&queue.name),
            "Status, supplies, and controls",
            tx,
        ))
        .push(gap(20.0))
        .push(Box::new(Card::new(hero)))
        .push(gap(16.0))
        .push(Box::new(Card::titled("Ink and toner", supplies)))
        .push(gap(16.0))
        .push(Box::new(Card::titled(
            "Printer controls",
            Box::new(actions),
        )));
    centered_page(
        Box::new(Scrollable::new(ScrollAxis::Vertical).with_child(Box::new(body))),
        920.0,
    )
}

fn discovery_view(
    result: Option<&Result<Vec<os_actions::printing::DiscoveredPrinter>, String>>,
    tx: &mpsc::Sender<UiCommand>,
) -> El {
    let mut body = Column::new()
        .with_intrinsic_height()
        .push(subpage_header(
            "Add a printer",
            "Nearby printers appear automatically",
            tx,
        ))
        .push(gap(20.0));
    match result {
        None => body = body.push(Box::new(Card::titled("Looking nearby", Box::new(ProgressBar::indeterminate().with_label("Finding nearby printers"))))),
        Some(Ok(printers)) if printers.is_empty() => body = body.push(Box::new(Card::titled("No printers found", copy("Make sure the printer is awake and connected to the same network, then search again.")))).push(gap(12.0)).push(command_action("print-centre.discover.retry", "Search again", tx, UiCommand::Discover)),
        Some(Ok(printers)) => {
            for printer in printers {
                let add_id = format!("print-centre.discover.add.{}", printer.name);
                body = body.push(Box::new(Card::titled_with_action(
                    &printer.name,
                    primary_icon_command_action(&add_id, "Add", Icon::Plus, tx, UiCommand::AddPrinter(printer.clone())),
                    Box::new(
                        Row::new()
                            .push(Box::new(IconWidget::new(Icon::Printer, 34.0).dim()))
                            .push(Box::new(
                                Column::new()
                                    .with_no_padding()
                                    .with_intrinsic_height()
                                    .push(copy("Ready to add"))
                                    .push(muted(
                                        printer.location.as_deref().unwrap_or("Nearby"),
                                    )),
                            )),
                    ),
                ))).push(gap(12.0));
            }
        }
        Some(Err(_)) => body = body.push(Box::new(Card::titled("Could not search right now", copy("Check your network connection and try again.")))).push(gap(12.0)).push(command_action("print-centre.discover.retry", "Try again", tx, UiCommand::Discover)),
    }
    centered_page(
        Box::new(Scrollable::new(ScrollAxis::Vertical).with_child(Box::new(body))),
        1120.0,
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "print setup is one cohesive review form whose shared controls are composed from a single snapshot"
)]
fn print_setup_view(
    snapshot: &Snapshot,
    path: &std::path::Path,
    preview: &PrintPreview,
    selected: Option<&str>,
    copies: &ui::LocalState<f64>,
    two_sided: &ui::LocalState<bool>,
    tx: &mpsc::Sender<UiCommand>,
) -> El {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Selected document");
    let printer_options = snapshot
        .queues
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|queue| {
            let status = friendly_queue_state(queue);
            let label = format!("{} ({status})", display_name(&queue.name));
            (queue.name.clone(), label)
        })
        .collect::<Vec<_>>();
    let mut printer_picker = Dropdown::new(Vec::new(), "")
        .with_id("print-centre.print.printer")
        .with_accessible_name("Printer")
        .with_placeholder("Choose a printer");
    printer_picker.set_value_options(printer_options);
    if let Some(selected) = selected {
        printer_picker.set_selected(selected);
    }
    let select_tx = tx.clone();
    printer_picker = printer_picker.on_change(move |name| {
        let _ = select_tx.send(UiCommand::SelectPrinter(name.to_owned()));
    });
    let can_print = selected.is_some();
    let mut actions = Row::new()
        .push(Box::new(Spacer::new()))
        .push(command_action(
            "print-centre.print.cancel",
            "Cancel",
            tx,
            UiCommand::CancelPrintSetup,
        ));
    if can_print {
        actions = actions.push(primary_command_action(
            "print-centre.print.submit",
            "Print",
            tx,
            UiCommand::PrintDocument,
        ));
    }
    let preview_widget: El = match preview {
        PrintPreview::Ready {
            rgba,
            width,
            height,
        } => Box::new(
            Container::new()
                .with_size(180.0, 220.0)
                .with_no_padding()
                .with_child(Box::new(
                    Image::from_rgba(rgba.clone(), *width, *height)
                        .filling()
                        .with_alt(format!("Preview of {filename}")),
                )),
        ),
        PrintPreview::Loading => Box::new(Container::new().with_size(180.0, 138.0).with_child(
            Box::new(ProgressBar::indeterminate().with_label("Preparing document preview")),
        )),
        PrintPreview::Unavailable => Box::new(
            Container::new()
                .with_size(180.0, 138.0)
                .with_child(Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .push(Box::new(IconWidget::new(Icon::FileText, 42.0).dim()))
                        .push(gap(10.0))
                        .push(copy("Preview unavailable"))
                        .push(muted("The original document is unchanged.")),
                )),
        ),
    };
    let document_card: El = Box::new(Card::titled(
        filename,
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(preview_widget)
                .push(gap(10.0))
                .push(copy("Ready to print"))
                .push(muted("The printer will render this document")),
        ),
    ));
    let options_card: El = Box::new(Card::titled(
        "Printing options",
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(Box::new(
                    Row::new()
                        .push(copy("Copies"))
                        .push(Box::new(Spacer::new()))
                        .push(Box::new(
                            NumberInput::from_state(copies)
                                .with_range(1.0, 99.0)
                                .with_step(1.0)
                                .with_id("print-centre.print.copies")
                                .with_accessible_name("Number of copies")
                                .with_size(150.0, 40.0),
                        )),
                ))
                .push(gap(18.0))
                .push(Box::new(
                    Row::new()
                        .push(copy("Print on both sides"))
                        .push(Box::new(Spacer::new()))
                        .push(Box::new(Toggle::from_state(two_sided))),
                )),
        ),
    ));
    let printer_card: El = Box::new(Card::titled(
        "Printer",
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(Box::new(printer_picker))
                .push(gap(10.0))
                .push(muted("Choose where to send this document.")),
        ),
    ));
    let cards = Row::new()
        .align_start()
        .push(Box::new(
            Container::new()
                .with_min_width(260.0)
                .with_flex_grow()
                .with_no_padding()
                .with_child(document_card),
        ))
        .push(Box::new(
            Container::new()
                .with_min_width(260.0)
                .with_flex_grow()
                .with_no_padding()
                .with_child(options_card),
        ))
        .push(Box::new(
            Container::new()
                .with_min_width(260.0)
                .with_flex_grow()
                .with_no_padding()
                .with_child(printer_card),
        ));
    let mut review = Column::new().with_no_padding().with_intrinsic_height();
    if let Some(issue) = unavailable_card(&failed_areas(snapshot, true, false, false), tx) {
        review = review.push(issue).push(gap(12.0));
    }
    let review = review.push(Box::new(cards)).push(gap(24.0));
    // This is deliberately a real surface rather than two loose buttons at
    // the bottom of the viewport. It visually closes the scrolling review,
    // keeps the primary action in a predictable trailing position, and gives
    // compact/touch layouts a forgiving inset around both actions.
    let action_footer: El = Box::new(
        Container::new().with_background().with_child(Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(Box::new(Divider::horizontal()))
                .push(gap(12.0))
                .push(Box::new(actions)),
        )),
    );
    let body = Column::new()
        .with_fill_remaining_height()
        .with_vertical_padding(20.0)
        .with_horizontal_padding(24.0, 24.0)
        .push(subpage_header(
            "Print a document",
            "Review the document and choose a printer",
            tx,
        ))
        .push(gap(16.0))
        .push(Box::new(
            Scrollable::new(ScrollAxis::Vertical)
                .fill_parent_height()
                .with_child(Box::new(review)),
        ))
        .push(gap(8.0))
        .push(action_footer);
    centered_page(Box::new(body), 1120.0)
}

#[allow(
    clippy::too_many_lines,
    reason = "one page composing the scan controls, the review pane and the page strip"
)]
fn scan_setup_view(
    snapshot: &Snapshot,
    selected: Option<&str>,
    scan: &ScanState,
    notice: Option<&OperationNotice>,
    tx: &mpsc::Sender<UiCommand>,
) -> El {
    let ScanState {
        resolution,
        color,
        source,
        language,
        crop,
        ..
    } = scan;
    let result = scan.result.as_ref();
    let selected_page = scan.selected;
    let page_count = scan.pages.len();
    let scanning = scan.in_progress;
    if let Some(page) = result {
        let preview =
            ImageCropEditor::new(page.rgba.clone(), page.width, page.height, crop.clone());
        let mut details = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(title(
                &format!("Page {} of {page_count}", selected_page.saturating_add(1)),
                20.0,
            ))
            .push(gap(6.0))
            .push(muted(if page.height >= page.width {
                "Portrait page"
            } else {
                "Landscape page"
            }))
            .push(muted(&format!("Reading text as {}", page.ocr_language)))
            .push(gap(14.0))
            .push(copy(&page.ocr_note));
        if let Some(text) = &page.recognized_text {
            let snippet: String = text.chars().take(320).collect();
            if !snippet.trim().is_empty() {
                details = details
                    .push(gap(12.0))
                    .push(Box::new(Card::titled("Recognized text", copy(&snippet))));
            }
        }
        let primary_actions = Row::new()
            .push(primary_command_action(
                "print-centre.scan.save-document",
                "Save document",
                tx,
                UiCommand::SaveScannedDocument,
            ))
            .push(command_action(
                "print-centre.scan.again",
                "Scan another page",
                tx,
                UiCommand::ScanAnother,
            ));

        let mut edit_actions = Row::new().push(command_action(
            "print-centre.scan.rotate",
            "Rotate right",
            tx,
            UiCommand::RotateScanPage,
        ));
        edit_actions = edit_actions.push(command_action(
            "print-centre.scan.apply-crop",
            "Apply crop",
            tx,
            UiCommand::ApplyScanCrop,
        ));
        if page.ocr_in_progress {
            edit_actions = edit_actions.push(muted("Updating recognized text…"));
        } else if page.ocr_needs_refresh {
            edit_actions = edit_actions.push(command_action(
                "print-centre.scan.recognize",
                "Update recognized text",
                tx,
                UiCommand::RecognizeCurrentPage,
            ));
        }
        if page
            .recognized_text
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty())
        {
            edit_actions = edit_actions.push(command_action(
                "print-centre.scan.copy-text",
                "Copy recognized text",
                tx,
                UiCommand::CopyRecognizedText,
            ));
        }

        details = details
            .push(gap(16.0))
            .push(title("Page tools", 16.0))
            .push(gap(8.0))
            .push(Box::new(edit_actions));
        let cards = Row::new()
            .align_start()
            .push(Box::new(
                Container::new()
                    .with_min_width(280.0)
                    .with_flex_grow()
                    .with_no_padding()
                    .with_height(360.0)
                    .with_child(Box::new(preview)),
            ))
            .push(Box::new(
                Container::new()
                    .with_min_width(280.0)
                    .with_flex_grow()
                    .with_no_padding()
                    .with_child(Box::new(Card::new(Box::new(details)))),
            ));

        let mut page_actions = Row::new();
        if selected_page > 0 {
            page_actions = page_actions.push(command_action(
                "print-centre.scan.previous",
                "Previous page",
                tx,
                UiCommand::PreviousScanPage,
            ));
        }
        if selected_page.saturating_add(1) < page_count {
            page_actions = page_actions.push(command_action(
                "print-centre.scan.next",
                "Next page",
                tx,
                UiCommand::NextScanPage,
            ));
        }
        if selected_page > 0 {
            page_actions = page_actions.push(command_action(
                "print-centre.scan.move-earlier",
                "Move before previous page",
                tx,
                UiCommand::MoveScanPageEarlier,
            ));
        }
        if selected_page.saturating_add(1) < page_count {
            page_actions = page_actions.push(command_action(
                "print-centre.scan.move-later",
                "Move after next page",
                tx,
                UiCommand::MoveScanPageLater,
            ));
        }
        page_actions = page_actions.push(command_action(
            "print-centre.scan.delete-page",
            "Remove this page",
            tx,
            UiCommand::DeleteScanPage,
        ));

        let finish_actions = Row::new()
            .push(command_action(
                "print-centre.scan.save-page",
                "Export this page",
                tx,
                UiCommand::SaveScannedPage,
            ))
            .push(command_action(
                "print-centre.scan.done",
                "Back to Print Centre",
                tx,
                UiCommand::Dashboard,
            ));
        let mut body = Column::new()
            .with_intrinsic_height()
            .push(subpage_header(
                "Scanned document",
                "Review the page before saving it",
                tx,
            ))
            .push(gap(20.0));
        if let Some(notice) = notice {
            body = body.push(notice_banner(notice)).push(gap(14.0));
        }
        body = body
            .push(Box::new(primary_actions))
            .push(gap(16.0))
            .push(Box::new(cards))
            .push(gap(12.0))
            .push(Box::new(Card::titled(
                "Pages in this document",
                Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .push(copy(&format!(
                            "Page {} selected · {page_count} {} total",
                            selected_page.saturating_add(1),
                            if page_count == 1 { "page" } else { "pages" }
                        )))
                        .push(gap(10.0))
                        .push(Box::new(page_actions)),
                ),
            )))
            .push(gap(12.0))
            .push(Box::new(finish_actions));
        return Box::new(Scrollable::new_vertical().with_child(Box::new(body)));
    }

    let mut devices = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(8.0);
    if let Ok(scanners) = &snapshot.scanners {
        for scanner in scanners {
            let active = selected == Some(scanner.device.as_str());
            let label = if active {
                format!("Selected: {}", scanner.description)
            } else {
                scanner.description.clone()
            };
            devices = devices.push(command_action(
                "print-centre.scan.device",
                &label,
                tx,
                UiCommand::SelectScanner(scanner.device.clone()),
            ));
        }
        if scanners.is_empty() {
            devices = devices.push(copy(
                "No scanner is available. Check its cable or network connection, then refresh Print Centre.",
            ));
        }
    } else if let Some(issue) = unavailable_card(&["scanners"], tx) {
        devices = devices.push(issue);
    }
    let resolution_state = resolution.clone();
    let resolution_control = ToggleGroup::new(
        vec![
            Segment::new("150 dpi"),
            Segment::new("300 dpi"),
            Segment::new("600 dpi"),
        ],
        resolution.get(),
    )
    .with_flexible_width()
    .with_id("print-centre.scan.resolution")
    .on_view("print-centre.scan.resolution.select", move |index| {
        resolution_state.set(index);
    });
    let color_state = color.clone();
    let color_control = ToggleGroup::new(
        vec![
            Segment::new("Color"),
            Segment::new("Grayscale"),
            Segment::new("Text"),
        ],
        color.get(),
    )
    .with_flexible_width()
    .with_id("print-centre.scan.color")
    .on_view("print-centre.scan.color.select", move |index| {
        color_state.set(index);
    });
    let source_state = source.clone();
    let source_control = ToggleGroup::new(
        vec![
            Segment::new("Flatbed"),
            Segment::new("Feeder"),
            Segment::new("Two-sided"),
        ],
        source.get(),
    )
    .with_flexible_width()
    .with_id("print-centre.scan.source")
    .on_view("print-centre.scan.source.select", move |index| {
        source_state.set(index);
    });
    let language_state = language.clone();
    let language_control = ToggleGroup::new(
        vec![
            Segment::new("English"),
            Segment::new("Deutsch"),
            Segment::new("Français"),
            Segment::new("Español"),
        ],
        language.get(),
    )
    .with_flexible_width()
    .with_id("print-centre.scan.language")
    .on_view("print-centre.scan.language.select", move |index| {
        language_state.set(index);
    });
    let settings = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(copy("Quality"))
        .push(gap(6.0))
        .push(Box::new(resolution_control))
        .push(gap(14.0))
        .push(copy("Appearance"))
        .push(gap(6.0))
        .push(Box::new(color_control))
        .push(gap(14.0))
        .push(copy("Paper source"))
        .push(gap(6.0))
        .push(Box::new(source_control))
        .push(gap(14.0))
        .push(copy("Text language"))
        .push(gap(6.0))
        .push(Box::new(language_control));
    let can_scan = selected.is_some() && !scanning;
    let mut actions = Row::new().push(command_action(
        "print-centre.scan.cancel",
        "Cancel",
        tx,
        UiCommand::Dashboard,
    ));
    if can_scan {
        actions = actions.push(primary_command_action(
            "print-centre.scan.start",
            "Scan page",
            tx,
            UiCommand::ScanPage,
        ));
    } else if scanning {
        actions = actions.push(copy("Scanning page…"));
    }
    let mut body = Column::new()
        .with_intrinsic_height()
        .push(subpage_header(
            "Scan a document",
            "Choose the scanner and how this page should look",
            tx,
        ))
        .push(gap(20.0));
    if let Some(notice) = notice {
        body = body.push(notice_banner(notice)).push(gap(14.0));
    }
    body = body
        .push(Box::new(Card::titled(
            "Current document",
            copy(&if page_count == 0 {
                "No pages captured yet".into()
            } else {
                format!("{page_count} pages captured — new scans will be added")
            }),
        )))
        .push(gap(16.0))
        .push(Box::new(Card::titled("Scanner", Box::new(devices))))
        .push(gap(16.0))
        .push(Box::new(Card::titled("Page settings", Box::new(settings))))
        .push(gap(16.0))
        .push(Box::new(actions));
    Box::new(Scrollable::new_vertical().with_child(Box::new(body)))
}

fn empty_view() -> El {
    let setup = Box::new(Column::new().with_no_padding().with_intrinsic_height()
        .push(title("Nearby printers appear automatically", 18.0))
        .push(copy("Choose Find printers and we’ll guide you. Drivers and network details stay out of the way unless you need them."))
        .push(gap(18.0)).push(Box::new(Row::new().push(action("print-centre.add.first", "Find printers")).push(action("print-centre.help", "Connection help")))));
    Box::new(Column::new().with_intrinsic_height()
        .push(header("Print and scan without digging through settings")).push(gap(36.0))
        .push(Box::new(Card::titled("Set up your first printer", setup)))
        .push(gap(18.0)).push(Box::new(Card::titled("Already have a document?",
            copy("Open it and choose Print, or drop it here to preview pages and finishing options before anything is sent.")))))
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

#[allow(
    clippy::too_many_lines,
    reason = "the deterministic demo catalogue keeps each workflow fixture visible in one dispatcher"
)]
fn workflow_demo(scenario: DemoScenario) -> Option<El> {
    let (tx, _rx) = mpsc::channel();
    if scenario == DemoScenario::Queue {
        return Some(queue_view(&demo_snapshot(), &tx, None, None));
    }
    if scenario == DemoScenario::Device {
        let snapshot = demo_snapshot();
        if let Some(queue) = snapshot
            .queues
            .as_ref()
            .ok()
            .and_then(|queues| queues.first())
        {
            return Some(device_view(queue, &tx));
        }
    }
    if scenario == DemoScenario::Discovery {
        let printers = Ok(vec![
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
        ]);
        return Some(discovery_view(Some(&printers), &tx));
    }
    if scenario == DemoScenario::PrintSetup {
        let snapshot = demo_snapshot();
        let copies = ui::LocalState::new(1.0);
        let two_sided = ui::LocalState::new(true);
        return Some(print_setup_view(
            &snapshot,
            std::path::Path::new("Travel itinerary.pdf"),
            &demo_print_preview(),
            Some("Studio_printer"),
            &copies,
            &two_sided,
            &tx,
        ));
    }
    if scenario == DemoScenario::ScanSetup {
        let snapshot = demo_snapshot();
        return Some(scan_setup_view(
            &snapshot,
            Some("scanner0"),
            &ScanState::default(),
            None,
            &tx,
        ));
    }
    if scenario == DemoScenario::ScanResult {
        let snapshot = demo_snapshot();
        let pixels: Arc<[u8]> = Arc::from(demo_scanned_page_pixels(320, 440));
        let page = ScannedPage {
            png: Arc::from(Vec::<u8>::new()),
            rgba: pixels,
            width: 320,
            height: 440,
            recognized_text: Some(
                "Travel receipt\nCoffee and breakfast  €12.40\nMonday, 7 September".into(),
            ),
            ocr_note: "Recognized 8 words.".into(),
            ocr_language: "English".into(),
            ocr_needs_refresh: false,
            ocr_in_progress: false,
        };
        let scan = ScanState {
            result: Some(page.clone()),
            pages: vec![page],
            ..ScanState::default()
        };
        return Some(scan_setup_view(
            &snapshot,
            Some("scanner0"),
            &scan,
            None,
            &tx,
        ));
    }
    if scenario == DemoScenario::History {
        return Some(history_view(&demo_snapshot(), &tx));
    }
    if scenario == DemoScenario::Diagnostics {
        return Some(diagnostics_view(&demo_snapshot(), &tx));
    }
    None
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

fn demo_view(scenario: DemoScenario) -> El {
    if let Some(view) = workflow_demo(scenario) {
        return view;
    }
    if scenario == DemoScenario::Empty {
        return empty_view();
    }
    let mut snapshot = demo_snapshot();
    if scenario == DemoScenario::ServiceError {
        snapshot.queues = Err("service unavailable".into());
        snapshot.jobs = Err("service unavailable".into());
        snapshot.scanners = Err("service unavailable".into());
    } else if scenario == DemoScenario::Dashboard {
        if let Ok(queues) = &mut snapshot.queues
            && let Some(queue) = queues.first_mut()
        {
            queue.state = os_actions::printing::QueueState::Ready;
        }
        if let Ok(jobs) = &mut snapshot.jobs
            && let Some(job) = jobs.first_mut()
        {
            job.state = Some(os_actions::printing::JobState::Completed);
            job.impressions_completed = job.impressions_total;
        }
    } else if scenario == DemoScenario::Attention {
        if let Ok(queues) = &mut snapshot.queues
            && let Some(queue) = queues.first_mut()
        {
            queue.state = os_actions::printing::QueueState::Paused;
            queue.accepting_jobs = false;
            queue.telemetry.state_reasons = vec!["media-empty-warning".into()];
        }
        if let Ok(jobs) = &mut snapshot.jobs
            && let Some(job) = jobs.first_mut()
        {
            job.state = Some(os_actions::printing::JobState::Stopped);
            job.state_reasons = vec!["media-empty-warning".into()];
        }
    }
    let (tx, _rx) = mpsc::channel();
    live_view(&snapshot, &tx, None)
}

fn request_snapshot() -> Option<mpsc::Receiver<Snapshot>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("tilo-print-centre-refresh".into())
        .spawn(move || {
            let _ = tx.send(Snapshot {
                queues: os_actions::printing::queues(),
                jobs: os_actions::printing::jobs(),
                scanners: os_actions::printing::scanners(),
            });
        })
        .ok()
        .map(|_| rx)
}

impl PrintCentre {
    pub fn new() -> Self {
        let (commands_tx, commands_rx) = mpsc::channel();
        Self {
            inner: loading_view(),
            refresh: request_snapshot(),
            commands_tx,
            commands_rx,
            snapshot: None,
            notice: None,
            pending_cancel: None,
            picker: None,
            pending_document: None,
            print_preview: PrintPreview::default(),
            print_preview_rx: None,
            selected_printer: None,
            print_copies: ui::LocalState::new(1.0),
            print_two_sided: ui::LocalState::new(false),
            selected_scanner: None,
            scan: ScanState::default(),
            scan_save_picker: None,
            scan_save_document: false,
            actions: crate::resource::ActionSink::default(),
            hosted: false,
        }
    }
    pub fn demo(scenario: DemoScenario) -> Self {
        let (commands_tx, commands_rx) = mpsc::channel();
        Self {
            inner: demo_view(scenario),
            refresh: None,
            commands_tx,
            commands_rx,
            snapshot: None,
            notice: None,
            pending_cancel: None,
            picker: None,
            pending_document: None,
            print_preview: PrintPreview::default(),
            print_preview_rx: None,
            selected_printer: None,
            print_copies: ui::LocalState::new(1.0),
            print_two_sided: ui::LocalState::new(false),
            selected_scanner: None,
            scan: ScanState::default(),
            scan_save_picker: None,
            scan_save_document: false,
            actions: crate::resource::ActionSink::default(),
            hosted: false,
        }
    }

    pub(crate) fn mark_framework_hosted(&mut self) {
        self.hosted = true;
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn is_framework_hosted(&self) -> bool {
        self.hosted
    }

    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
        self.actions.mark_hosted_for_test();
    }

    #[cfg(test)]
    pub(crate) fn sink_is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    /// Printers, jobs, and scanners from the live CUPS/SANE snapshot. Not a
    /// second `print-centre.dashboard` root; dashboard Views already ride publish.
    fn live_device_resources(&self) -> Vec<Resource> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        let mut extras = Vec::new();
        if let Ok(queues) = &snapshot.queues {
            extras.extend(queues.iter().map(printer_resource));
        }
        if let Ok(jobs) = &snapshot.jobs {
            extras.extend(jobs.iter().map(job_resource).map(|mut job| {
                job.actions
                    .retain(|action| action.id.as_str() != "open-queue");
                job
            }));
        }
        if let Ok(scanners) = &snapshot.scanners {
            extras.extend(scanners.iter().map(scanner_resource));
        }
        extras
    }

    fn request_document(&mut self) {
        if self.picker.is_some() {
            return;
        }
        self.print_copies.set(1.0);
        self.print_two_sided.set(false);
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

    fn rebuild_print_setup(&mut self) {
        if let (Some(snapshot), Some(path)) = (&self.snapshot, &self.pending_document) {
            self.inner = print_setup_view(
                snapshot,
                path,
                &self.print_preview,
                self.selected_printer.as_deref(),
                &self.print_copies,
                &self.print_two_sided,
                &self.commands_tx,
            );
        }
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

    fn rebuild_scan_setup(&mut self) {
        if let Some(snapshot) = &self.snapshot {
            self.inner = scan_setup_view(
                snapshot,
                self.selected_scanner.as_deref(),
                &self.scan,
                self.notice.as_ref(),
                &self.commands_tx,
            );
        }
    }

    fn poll_scan_save_picker(&mut self) -> bool {
        let Some(receiver) = self.scan_save_picker.as_ref() else {
            return false;
        };
        let outcome = match receiver.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                ui::portal::PortalOutcome::Failed("The file picker stopped responding".into())
            }
        };
        self.scan_save_picker = None;
        let save_document = std::mem::take(&mut self.scan_save_document);
        match outcome {
            ui::portal::PortalOutcome::Opened(mut path) => {
                if path.extension().is_none() {
                    path.set_extension(if save_document { "pdf" } else { "png" });
                }
                let bytes = if save_document {
                    Arc::<[u8]>::from(scanned_document_pdf(&self.scan.pages))
                } else if let Some(page) = &self.scan.result {
                    page.png.clone()
                } else {
                    return true;
                };
                if !bytes.is_empty() {
                    self.spawn_effect(
                        "tilo-save-scanned-page",
                        if save_document {
                            "Scanned document saved"
                        } else {
                            "Scanned page saved"
                        },
                        format!(
                            "Saved {}.",
                            path.file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("the page")
                        ),
                        if save_document {
                            "Could not save scanned document"
                        } else {
                            "Could not save scanned page"
                        },
                        "Choose another location and try again.".into(),
                        move || save_scanned_page(&path, bytes.as_ref()),
                    );
                }
            }
            ui::portal::PortalOutcome::Cancelled => {}
            ui::portal::PortalOutcome::Failed(_) => {
                self.notice = Some(OperationNotice {
                    success: false,
                    title: "Could not open the file picker".into(),
                    message: "Try saving the scanned page again.".into(),
                });
                self.rebuild_scan_setup();
            }
        }
        true
    }

    fn rebuild_dashboard(&mut self) {
        self.inner = self.snapshot.as_ref().map_or_else(empty_view, |snapshot| {
            live_view(snapshot, &self.commands_tx, self.notice.as_ref())
        });
    }

    fn spawn_effect(
        &self,
        name: &'static str,
        success_title: &'static str,
        success_message: String,
        failure_title: &'static str,
        failure_message: String,
        effect: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) {
        let tx = self.commands_tx.clone();
        let _ = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let success = effect().is_ok();
                let _ = tx.send(UiCommand::EffectFinished(OperationNotice {
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
                        failure_message
                    },
                }));
            });
    }

    #[allow(
        clippy::too_many_lines,
        reason = "all UI command transitions remain centralized so operation feedback and refresh behavior cannot diverge"
    )]
    fn handle_command(&mut self, command: UiCommand) {
        match command {
            UiCommand::Refresh => {
                self.notice = None;
                self.inner = loading_view();
                self.refresh = request_snapshot();
            }
            UiCommand::Dashboard => {
                self.scan.result = None;
                self.scan.in_progress = false;
                self.rebuild_dashboard();
            }
            UiCommand::Queue => {
                if let Some(snapshot) = &self.snapshot {
                    self.inner = queue_view(
                        snapshot,
                        &self.commands_tx,
                        self.pending_cancel.as_deref(),
                        self.notice.as_ref(),
                    );
                }
            }
            UiCommand::History => {
                if let Some(snapshot) = &self.snapshot {
                    self.inner = history_view(snapshot, &self.commands_tx);
                }
            }
            UiCommand::Diagnostics => {
                if let Some(snapshot) = &self.snapshot {
                    self.inner = diagnostics_view(snapshot, &self.commands_tx);
                }
            }
            UiCommand::Device(name) => {
                if let Some(queue) = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.queues.as_ref().ok())
                    .and_then(|queues| queues.iter().find(|q| q.name == name))
                {
                    self.inner = device_view(queue, &self.commands_tx);
                }
            }
            UiCommand::Discover => {
                self.inner = discovery_view(None, &self.commands_tx);
                let tx = self.commands_tx.clone();
                let _ = std::thread::Builder::new()
                    .name("tilo-printer-discovery".into())
                    .spawn(move || {
                        let _ = tx.send(UiCommand::DiscoveryFinished(
                            os_actions::printing::discover_driverless(),
                        ));
                    });
            }
            UiCommand::DiscoveryFinished(result) => {
                self.inner = discovery_view(Some(&result), &self.commands_tx);
            }
            UiCommand::AddPrinter(printer) => {
                let shown = display_name(&printer.name);
                self.spawn_effect(
                    "tilo-add-printer",
                    "Printer added",
                    format!("{shown} is ready for your apps."),
                    "Could not add printer",
                    format!("Check that {shown} is awake and connected, then try again."),
                    move || os_actions::printing::add_driverless(&printer.uri, &printer.name),
                );
            }
            UiCommand::SetDefault(name) => {
                let shown = display_name(&name);
                self.spawn_effect(
                    "tilo-default-printer",
                    "Preferred printer changed",
                    format!("New documents will use {shown} unless you choose another printer."),
                    "Could not change the preferred printer",
                    format!("{shown} is still available; try again in a moment."),
                    move || os_actions::printing::set_default(&name),
                );
            }
            UiCommand::SetEnabled(name, enabled) => self.spawn_effect(
                "tilo-toggle-printer",
                if enabled {
                    "Printer resumed"
                } else {
                    "Printer paused"
                },
                if enabled {
                    "Waiting documents can print now.".into()
                } else {
                    "Documents will wait until you resume it.".into()
                },
                if enabled {
                    "Could not resume printer"
                } else {
                    "Could not pause printer"
                },
                "Nothing changed. Check the printer connection and try again.".into(),
                move || os_actions::printing::set_enabled(&name, enabled),
            ),
            UiCommand::RequestCancel(id) => {
                self.pending_cancel = Some(id);
                if let Some(snapshot) = &self.snapshot {
                    self.inner = queue_view(
                        snapshot,
                        &self.commands_tx,
                        self.pending_cancel.as_deref(),
                        self.notice.as_ref(),
                    );
                }
            }
            UiCommand::ConfirmCancel(id) => {
                self.pending_cancel = None;
                self.spawn_effect(
                    "tilo-cancel-print-job",
                    "Document canceled",
                    "It was removed from the printer.".into(),
                    "Could not cancel document",
                    "The document may still print. Check the queue and try again.".into(),
                    move || os_actions::printing::cancel_job(&id),
                );
            }
            UiCommand::RetryJob(id) => self.spawn_effect(
                "tilo-retry-print-job",
                "Document sent again",
                "It is back in the print queue.".into(),
                "Could not try again",
                "Check the printer, then try again.".into(),
                move || os_actions::printing::retry_job(&id),
            ),
            UiCommand::EffectFinished(notice) => {
                self.notice = Some(notice);
                if self.scan.result.is_some() {
                    // Saving a scan is not a printer-state mutation. Keep the
                    // page and its OCR visible so the user can confirm success,
                    // save another copy, or scan the next page without losing
                    // their place.
                    self.rebuild_scan_setup();
                } else {
                    self.refresh = request_snapshot();
                }
            }
            UiCommand::ChooseDocument => self.request_document(),
            UiCommand::SelectPrinter(name) => {
                self.selected_printer = Some(name);
                self.rebuild_print_setup();
            }
            UiCommand::PrintDocument => {
                let (Some(path), Some(queue)) =
                    (self.pending_document.take(), self.selected_printer.take())
                else {
                    return;
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
                self.spawn_effect(
                    "tilo-print-document",
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
                );
            }
            UiCommand::CancelPrintSetup => {
                self.pending_document = None;
                self.print_preview = PrintPreview::Unavailable;
                self.print_preview_rx = None;
                self.selected_printer = None;
                self.print_copies.set(1.0);
                self.print_two_sided.set(false);
                self.rebuild_dashboard();
            }
            UiCommand::ScanSetup => {
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
            UiCommand::SelectScanner(device) => {
                self.selected_scanner = Some(device);
                self.rebuild_scan_setup();
            }
            UiCommand::ScanPage => {
                let Some(device) = self.selected_scanner.clone() else {
                    return;
                };
                let resolution_dpi = [150_u16, 300, 600]
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
                let tx = self.commands_tx.clone();
                let _ = std::thread::Builder::new()
                    .name("tilo-scan-document".into())
                    .spawn(move || {
                        let result = os_actions::printing::scan_page(
                            &device,
                            os_actions::printing::ScanSettings {
                                resolution_dpi,
                                color,
                                source,
                            },
                        )
                        .and_then(|png| {
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
                                    (Some(document.plain_text()), format!("Recognized {count} words."))
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
                        });
                        let _ = tx.send(UiCommand::ScanFinished(result));
                    });
            }
            UiCommand::ScanFinished(result) => {
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
            UiCommand::SaveScannedPage => {
                if self.scan.result.is_some() && self.scan_save_picker.is_none() {
                    self.scan_save_document = false;
                    self.scan_save_picker = Some(
                        ui::portal::PickRequest::save(
                            "Save scanned page",
                            "Save",
                            "Scanned page.png",
                        )
                        .thread_name("print-centre-scan-save-picker")
                        .send(),
                    );
                }
            }
            UiCommand::SaveScannedDocument => {
                if !self.scan.pages.is_empty() && self.scan_save_picker.is_none() {
                    self.scan_save_document = true;
                    self.scan_save_picker = Some(
                        ui::portal::PickRequest::save(
                            "Save scanned document",
                            "Save",
                            "Scanned document.pdf",
                        )
                        .thread_name("print-centre-scan-document-save-picker")
                        .send(),
                    );
                }
            }
            UiCommand::CopyRecognizedText => {
                let Some(text) = self
                    .scan
                    .result
                    .as_ref()
                    .and_then(|page| page.recognized_text.as_deref())
                    .filter(|text| !text.trim().is_empty())
                else {
                    return;
                };
                ui::text_selection::clipboard_copy(text);
                self.notice = Some(OperationNotice {
                    success: true,
                    title: "Text copied".into(),
                    message: "Paste the recognized text into any app.".into(),
                });
                self.rebuild_scan_setup();
            }
            UiCommand::ScanAnother => {
                self.notice = None;
                self.scan.result = None;
                self.rebuild_scan_setup();
            }
            UiCommand::PreviousScanPage => {
                self.scan.selected = self.scan.selected.saturating_sub(1);
                self.scan.result = self.scan.pages.get(self.scan.selected).cloned();
                self.scan.crop.set(CropSelection::default());
                self.rebuild_scan_setup();
            }
            UiCommand::NextScanPage => {
                if self.scan.selected.saturating_add(1) < self.scan.pages.len() {
                    self.scan.selected = self.scan.selected.saturating_add(1);
                }
                self.scan.result = self.scan.pages.get(self.scan.selected).cloned();
                self.scan.crop.set(CropSelection::default());
                self.rebuild_scan_setup();
            }
            UiCommand::MoveScanPageEarlier => {
                if self.scan.selected > 0 {
                    let previous = self.scan.selected.saturating_sub(1);
                    self.scan.pages.swap(self.scan.selected, previous);
                    self.scan.selected = previous;
                }
                self.scan.result = self.scan.pages.get(self.scan.selected).cloned();
                self.scan.crop.set(CropSelection::default());
                self.rebuild_scan_setup();
            }
            UiCommand::MoveScanPageLater => {
                let next = self.scan.selected.saturating_add(1);
                if next < self.scan.pages.len() {
                    self.scan.pages.swap(self.scan.selected, next);
                    self.scan.selected = next;
                }
                self.scan.result = self.scan.pages.get(self.scan.selected).cloned();
                self.scan.crop.set(CropSelection::default());
                self.rebuild_scan_setup();
            }
            UiCommand::DeleteScanPage => {
                if self.scan.selected < self.scan.pages.len() {
                    self.scan.pages.remove(self.scan.selected);
                }
                self.scan.selected = self
                    .scan
                    .selected
                    .min(self.scan.pages.len().saturating_sub(1));
                self.scan.result = self.scan.pages.get(self.scan.selected).cloned();
                self.scan.crop.set(CropSelection::default());
                self.rebuild_scan_setup();
            }
            UiCommand::RotateScanPage => {
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
            UiCommand::ApplyScanCrop => {
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
            UiCommand::RecognizeCurrentPage => {
                let Some(current) = self.scan.pages.get_mut(self.scan.selected) else {
                    return;
                };
                if current.ocr_in_progress {
                    return;
                }
                current.ocr_in_progress = true;
                let page = current.clone();
                self.scan.result = Some(page.clone());
                self.rebuild_scan_setup();
                let identity = page.png.clone();
                let tx = self.commands_tx.clone();
                let _ =
                    std::thread::Builder::new()
                        .name("tilo-recognize-scanned-page".into())
                        .spawn(move || {
                            let result = image::RgbaImage::from_raw(
                                page.width,
                                page.height,
                                page.rgba.to_vec(),
                            )
                            .ok_or_else(|| "The scanned page pixels were incomplete.".to_string())
                            .and_then(|rgba| {
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
                                    .map_err(|_| {
                                        "Text could not be recognized on this page.".into()
                                    })
                            });
                            let _ = tx.send(UiCommand::RecognitionFinished(identity, result));
                        });
            }
            UiCommand::RecognitionFinished(identity, result) => {
                let Some(index) = self
                    .scan
                    .pages
                    .iter()
                    .position(|page| Arc::ptr_eq(&page.png, &identity))
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
        }
    }
}
impl Default for PrintCentre {
    fn default() -> Self {
        Self::new()
    }
}
impl PrintCentre {
    fn answer_resource_operation(
        &mut self,
        k: OperationKind,
        id: &str,
        payload: &[u8],
        _: &AutoCommit,
        _: &ObjId,
    ) -> Result<bool, String> {
        if id != PRINT_CENTRE_OPERATION || !matches!(k, OperationKind::View | OperationKind::Effect)
        {
            return Ok(false);
        }
        let command: String = serde_json::from_slice(payload)
            .map_err(|error| format!("invalid Print Centre operation: {error}"))?;
        let (verb, value) = command.split_once('\0').unwrap_or((&command, ""));
        let expected_kind = match verb {
            "refresh" | "discover" | "default" | "pause" | "resume" | "cancel" | "retry" => {
                OperationKind::Effect
            }
            "dashboard" | "queue" | "history" | "diagnostics" | "print" | "scan" | "device"
            | "scan-device" => OperationKind::View,
            _ => return Err("unknown Print Centre operation".into()),
        };
        if k != expected_kind {
            return Err("Print Centre operation has the wrong execution kind".into());
        }
        let next = match verb {
            "refresh" => UiCommand::Refresh,
            "discover" => UiCommand::Discover,
            "dashboard" => UiCommand::Dashboard,
            "queue" => UiCommand::Queue,
            "history" => UiCommand::History,
            "diagnostics" => UiCommand::Diagnostics,
            "print" => UiCommand::ChooseDocument,
            "scan" => UiCommand::ScanSetup,
            "device" if !value.is_empty() => UiCommand::Device(value.into()),
            "scan-device" if !value.is_empty() => UiCommand::SelectScanner(value.into()),
            "default" if !value.is_empty() => UiCommand::SetDefault(value.into()),
            "pause" if !value.is_empty() => UiCommand::SetEnabled(value.into(), false),
            "resume" if !value.is_empty() => UiCommand::SetEnabled(value.into(), true),
            "cancel" if !value.is_empty() => UiCommand::ConfirmCancel(value.into()),
            "retry" if !value.is_empty() => UiCommand::RetryJob(value.into()),
            _ => return Err("Print Centre resource is no longer available".into()),
        };
        self.handle_command(next);
        Ok(true)
    }
}

impl ui::undo::CommittedActions for PrintCentre {
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        // Hosted edits belong to the framework dispatcher. Standalone stays
        // on the local host, which has no document action yet.
        if self.actions.is_hosted() {
            return None;
        }
        self.actions.take_committed()
    }
}

impl Widget for PrintCentre {
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::resources::PublishesResources
    );

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        if self.poll_picker() {
            self.inner.sync(state, path);
            return true;
        }
        if self.poll_print_preview() {
            self.inner.sync(state, path);
            return true;
        }
        if self.poll_scan_save_picker() {
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
        if let Ok(command) = self.commands_rx.try_recv() {
            self.handle_command(command);
            self.inner.sync(state, path);
            return true;
        }
        self.inner.update(state, path)
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.inner.build_layout(taffy, theme, node_map)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.inner.apply_layout(taffy, node, origin);
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.inner.draw(scene, transform, text, theme, presence);
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.inner.handle_event(event, ctx);
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.inner.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.inner.as_mut()]
    }
    fn name(&self) -> &'static str {
        "PrintCentre"
    }
    fn get_rect(&self) -> Rect {
        self.inner.get_rect()
    }
    fn take_structure_dirty(&mut self) -> bool {
        self.inner.take_structure_dirty()
    }
    fn needs_idle_sync(&self) -> bool {
        self.refresh.is_some() || self.picker.is_some() || self.scan_save_picker.is_some()
    }
    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[PRINT_CENTRE_OPERATION]
    }
}

fn find_resource_tree(resources: &[Resource], id: &str) -> Option<Resource> {
    for resource in resources {
        if resource.id.as_str() == id {
            return Some(resource.clone());
        }
        if let Some(found) = find_resource_tree(&resource.children, id) {
            return Some(found);
        }
    }
    None
}

impl ui::resources::PublishesResources for PrintCentre {
    fn resources(&self, _: &AutoCommit) -> Vec<Resource> {
        if self.hosted {
            return self.live_device_resources();
        }
        let mut root = ui::tilo_resource::publish::<crate::resource::PrintCentreResource>(
            "print-centre",
            "Print Centre",
            &crate::resource::PrintCentreState,
        );
        if let Some(snapshot) = &self.snapshot {
            if let Ok(queues) = &snapshot.queues {
                root.children.extend(queues.iter().map(printer_resource));
            }
            if let Ok(jobs) = &snapshot.jobs {
                root.children.extend(jobs.iter().map(job_resource));
            }
            if let Ok(scanners) = &snapshot.scanners {
                root.children.extend(scanners.iter().map(scanner_resource));
            }
        }
        vec![root]
    }

    fn resource(&self, _: &AutoCommit, id: &str) -> Option<Resource> {
        let mut root = ui::tilo_resource::publish::<crate::resource::PrintCentreResource>(
            "print-centre",
            "Print Centre",
            &crate::resource::PrintCentreState,
        );
        if let Some(snapshot) = &self.snapshot {
            if let Ok(queues) = &snapshot.queues {
                root.children.extend(queues.iter().map(printer_resource));
            }
            if let Ok(jobs) = &snapshot.jobs {
                root.children.extend(jobs.iter().map(job_resource));
            }
            if let Ok(scanners) = &snapshot.scanners {
                root.children.extend(scanners.iter().map(scanner_resource));
            }
        }
        if self.hosted && id == "print-centre" {
            return None;
        }
        find_resource_tree(&[root], id)
            .or_else(|| find_resource_tree(&self.live_device_resources(), id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ROOT;

    #[test]
    fn hosted_resources_keep_devices_without_dashboard_root() {
        let mut centre = PrintCentre::demo(DemoScenario::Dashboard);
        centre.snapshot = Some(demo_snapshot());
        let doc = AutoCommit::new();
        assert!(!centre.is_framework_hosted());
        let standalone = ui::resources::resources(&centre, &doc);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "print-centre.dashboard");
        assert!(
            standalone[0]
                .children
                .iter()
                .any(|child| child.kind == "print-centre.printer")
        );

        centre.mark_hosted_for_test();
        let printer_id = standalone[0]
            .children
            .iter()
            .find(|child| child.kind == "print-centre.printer")
            .map(|child| child.id.as_str().to_owned())
            .expect("printer");
        assert!(ui::resources::resource(&centre, &doc, "print-centre").is_none());
        assert!(
            ui::resources::resource(&centre, &doc, &printer_id)
                .is_some_and(|printer| printer.actions.iter().any(|action| action.id == "open")),
            "a hosted printer stays the published row"
        );
        let extras = ui::resources::resources(&centre, &doc);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "print-centre.dashboard"
                    && resource.id.as_str() != "print-centre"),
            "hosted must not republish the dashboard: {:?}",
            extras
                .iter()
                .map(|resource| resource.id.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            extras
                .iter()
                .any(|resource| resource.kind == "print-centre.printer")
        );
        assert!(
            extras
                .iter()
                .any(|resource| resource.kind == "print-centre.job")
        );
        assert!(extras.iter().all(|resource| {
            resource
                .actions
                .iter()
                .all(|action| action.id.as_str() != "open-queue")
        }));
    }

    /// The surface Print Centre publishes once a refresh has found printers,
    /// documents and scanners.
    fn populated_resources(doc: &AutoCommit) -> Vec<Resource> {
        let mut centre = PrintCentre::demo(DemoScenario::Dashboard);
        centre.snapshot = Some(demo_snapshot());
        ui::resources::resources(&centre, doc)
    }

    // Print Centre keeps no document: it publishes only effect and view
    // operations, so the contract is the surface sweep (protocol validation
    // over the dashboard and every printer, job and scanner) and Ctrl+K.
    ui::action_contract_tests! {
        seed: AutoCommit::new(),
        registry: tilo_actions::ActionRegistry::new(),
        resources: |doc| populated_resources(doc),
        palette: ["Print a document", "Open print queue", "Find network printers", "Open printer"],
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

    #[test]
    fn scenarios_and_resources_expose_real_navigation_and_backend_states() {
        eprintln!("phase scenarios_build");
        {
            for s in [
                DemoScenario::Dashboard,
                DemoScenario::Active,
                DemoScenario::Attention,
                DemoScenario::Empty,
                DemoScenario::PrintSetup,
                DemoScenario::ScanSetup,
                DemoScenario::ScanResult,
                DemoScenario::History,
                DemoScenario::Diagnostics,
                DemoScenario::ServiceError,
            ] {
                assert_eq!(PrintCentre::demo(s).name(), "PrintCentre");
            }
        }

        eprintln!("phase resource_surface_exposes_real_navigation_and_device_operations");
        {
            let mut centre = PrintCentre::demo(DemoScenario::Dashboard);
            centre.snapshot = Some(demo_snapshot());
            let mut doc = AutoCommit::new();
            let resources = ui::resources::resources(&centre, &doc);
            let root = resources.first().expect("Print Centre root resource");
            let action_ids = root
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>();
            for expected in [
                "refresh",
                "open-queue",
                "open-history",
                "run-diagnostics",
                "find-printers",
                "print-document",
                "scan-document",
            ] {
                assert!(action_ids.contains(&expected), "missing {expected} action");
            }
            assert!(
                root.children
                    .iter()
                    .any(|resource| resource.kind == "print-centre.printer"),
                "configured queues are first-class resources"
            );
            assert!(
                root.children
                    .iter()
                    .any(|resource| resource.kind == "print-centre.job"),
                "recent documents are first-class resources"
            );
            assert!(
                root.children
                    .iter()
                    .any(|resource| resource.kind == "print-centre.scanner"),
                "available scanners are first-class resources"
            );

            let payload = serde_json::to_vec("queue").expect("operation payload");
            assert!(
                centre
                    .start_operation(
                        &ui::widget::ResourceOperation::local(
                            ui::tilo_actions::InvocationId::new(),
                            OperationKind::View,
                            PRINT_CENTRE_OPERATION,
                            &payload
                        ),
                        &mut doc,
                        &ROOT
                    )
                    .map(|started| started.is_some())
                    .expect("queue view operation")
            );
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
            let (tx, _rx) = mpsc::channel();
            assert!(unavailable_card(&failed_areas(&unavailable, true, true, true), &tx).is_some());
            assert!(unavailable_card(&failed_areas(&empty, true, true, true), &tx).is_none());
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
                let centre = ui::headless::Driver::new(
                    ui::State::new(),
                    Box::new(PrintCentre::demo(DemoScenario::ScanResult)),
                    (width, 760),
                );
                let mut buttons = 0;
                inspect(&centre.inspection().root, f64::from(width), &mut buttons);
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
                let centre = ui::headless::Driver::new(
                    ui::State::new(),
                    Box::new(PrintCentre::demo(DemoScenario::PrintSetup)),
                    (width, height),
                );
                let snapshot = centre.inspection();

                let mut buttons = Vec::new();
                collect_buttons(&snapshot.root, &mut buttons);
                buttons.sort_by(|left, right| left.y0.total_cmp(&right.y0));
                let footer_buttons = buttons.iter().skip(1).collect::<Vec<_>>();
                assert_eq!(
                    footer_buttons.len(),
                    2,
                    "Cancel and Print must stay visible; buttons were {buttons:?}"
                );
                let root_bottom = snapshot.root.rect[1] + snapshot.root.rect[3];
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
                if widget.widget == "DashboardCard" && width > 0.0 && height > 0.0 {
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
                let centre = ui::headless::Driver::new_with_theme(
                    ui::State::new(),
                    Box::new(PrintCentre::demo(DemoScenario::Active)),
                    (1440, 900),
                    Theme::dark().with_text_scale(text_scale),
                );
                let snapshot = centre.inspection();
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
                    "expected active, printer, scan, and utility cards"
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
            let mut centre = PrintCentre::demo(DemoScenario::ScanSetup);
            centre.handle_command(UiCommand::ScanFinished(Ok(scanned_page(10))));
            centre.handle_command(UiCommand::ScanAnother);
            assert_eq!(
                centre.scan.pages.len(),
                1,
                "scan another preserves page one"
            );
            centre.handle_command(UiCommand::ScanFinished(Ok(scanned_page(20))));
            assert_eq!(centre.scan.selected, 1);

            centre.handle_command(UiCommand::MoveScanPageEarlier);
            assert_eq!(centre.scan.selected, 0);
            assert_eq!(centre.scan.pages[0].width, 20);
            centre.handle_command(UiCommand::NextScanPage);
            assert_eq!(centre.scan.result.as_ref().map(|page| page.width), Some(10));

            centre.handle_command(UiCommand::DeleteScanPage);
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
