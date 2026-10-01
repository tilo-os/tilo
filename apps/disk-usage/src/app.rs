use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use ui::time::{Duration, Instant};
use ui::widget::{AppModel, ModelPath};

use ui::a11y::{AccessNode, AccessState, Role};
use ui::automerge::{AutoCommit, ObjId};
use ui::chart_palette::ChartPalette;
use ui::presence::PresenceCtx;
use ui::taffy::{
    Dimension, LengthPercentageAuto, NodeId, Position, Rect as TRect, Size, Style, TaffyTree,
};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::tilo_actions::{
    ActionContract, ActionOutput, ActionSpec, DiscoveryScope, OperationKind, Resource,
};
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Circle, Point, Rect};
use ui::vello::peniko::Fill;
use ui::widget::{Button, ButtonFace, PieChart, PieSlice, PieStyle, Widget};
use ui::winit::event::{ElementState, MouseButton, WindowEvent};

use crate::doc::DiskUsageDoc;
use crate::scan::{Entry, ScanCancellation, ScanResult};
use ui::format::bytes_si;

const TOP_H: f64 = 78.0;
pub(crate) const OPERATION_TYPE: &str = "disk-usage.operation";

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DiskUsageOperation {
    Analyze { path: PathBuf },
    Choose,
    Back,
    Home,
}

pub(crate) fn operation_action(
    id: &str,
    label: &str,
    operation: &DiskUsageOperation,
) -> Option<ActionSpec> {
    let mut action =
        ActionSpec::operation(id, label, OperationKind::Effect, OPERATION_TYPE, operation).ok()?;
    action.contract = ActionContract {
        discovery: DiscoveryScope::Launchable,
        ..ActionContract::default()
    };
    Some(action)
}

#[derive(Clone)]
struct Place {
    label: String,
    caption: String,
    path: PathBuf,
    choose: bool,
}

enum View {
    Home,
    Loading(PathBuf),
    Analysis(ScanResult),
    Error(PathBuf, String),
}

pub struct DiskUsageApp {
    view: View,
    history: Vec<PathBuf>,
    recent: Vec<PathBuf>,
    places: Vec<Place>,
    chart: PieChart,
    rect: Rect,
    pointer: Point,
    scan_rx: Option<Receiver<ScanResult>>,
    scan_cancellation: Option<ScanCancellation>,
    picker_rx: Option<ui::portal::PortalChannel>,
    shot_mode: bool,
    hosted: bool,
}

impl DiskUsageApp {
    pub fn new() -> Self {
        let home = tilo_paths::home().unwrap_or_else(|| PathBuf::from("/"));
        let workspace = std::env::var_os("TILO_WORKSPACE_ROOTS")
            .and_then(|s| s.to_string_lossy().split(':').next().map(PathBuf::from))
            .or_else(tilo_paths::workspace_root)
            .unwrap_or_else(|| PathBuf::from(tilo_paths::WORKSPACE_MOUNT));
        let places = vec![
            Place {
                label: "Home".into(),
                caption: "Documents, downloads and personal files".into(),
                path: home,
                choose: false,
            },
            Place {
                label: "Workspace".into(),
                caption: "Shared projects and hydrated files".into(),
                path: workspace,
                choose: false,
            },
            Place {
                label: "File system".into(),
                caption: "Everything on this system".into(),
                path: PathBuf::from("/"),
                choose: false,
            },
        ];
        Self {
            view: View::Home,
            history: Vec::new(),
            recent: Vec::new(),
            places,
            chart: PieChart::new(Vec::new()).with_style(PieStyle::default()),
            rect: Rect::ZERO,
            pointer: Point::ZERO,
            scan_rx: None,
            scan_cancellation: None,
            picker_rx: None,
            shot_mode: false,
            hosted: false,
        }
    }

    pub fn sample() -> Self {
        let mut app = Self::new();
        app.shot_mode = true;
        let path = PathBuf::from("/home/you");
        let values = [
            ("Projects", 84_200_000_000),
            ("Videos", 48_600_000_000),
            ("Photos", 31_800_000_000),
            ("Downloads", 19_400_000_000),
            ("Documents", 8_700_000_000),
            ("Other", 5_100_000_000),
        ];
        let entries = values
            .into_iter()
            .map(|(name, bytes)| Entry {
                path: path.join(name.to_lowercase()),
                name: name.into(),
                bytes,
                files: bytes / 1_000_000,
                unreadable: 0,
            })
            .collect::<Vec<_>>();
        let total = entries.iter().map(|e| e.bytes).sum();
        app.set_result(ScanResult {
            path,
            entries,
            total,
            files: 12_842,
            unreadable: 0,
            error: None,
        });
        app
    }

    pub fn sample_with_inspection() -> Self {
        let mut app = Self::sample();
        let _ = app.chart.inspect("/home/you/projects");
        app
    }

    pub fn sample_home() -> Self {
        let mut app = Self::new();
        app.shot_mode = true;
        app.recent = vec![
            PathBuf::from("/home/you/Projects/tilo"),
            PathBuf::from(tilo_paths::WORKSPACE_MOUNT).join("design-system"),
        ];
        app
    }

    fn palette(theme: &Theme, index: usize) -> ui::vello::peniko::Color {
        ChartPalette::from_theme(theme).categorical(index)
    }

    fn set_result(&mut self, result: ScanResult) {
        let total = result.total;
        let slices = result
            .entries
            .iter()
            .map(|entry| {
                let slice = PieSlice::new(
                    entry.path.to_string_lossy(),
                    &entry.name,
                    entry.bytes as f64,
                    ui::vello::peniko::Color::TRANSPARENT,
                )
                .with_detail(format!(
                    "{} · {} files",
                    bytes_si(entry.bytes),
                    entry.files
                ));
                if entry.path.file_name().is_some_and(|name| name == ".") {
                    slice.non_interactive()
                } else {
                    slice
                }
            })
            .collect();
        if self.shot_mode {
            self.chart.set_slices_immediate(slices);
        } else {
            self.chart.set_slices(slices);
        }
        self.chart
            .set_center(bytes_si(total), "used in this folder");
        if self.recent.first() != Some(&result.path) {
            self.recent.retain(|p| p != &result.path);
            self.recent.insert(0, result.path.clone());
            self.recent.truncate(5);
        }
        self.view = View::Analysis(result);
    }

    fn start_scan(&mut self, path: PathBuf, push_history: bool) {
        if let Some(cancellation) = self.scan_cancellation.take() {
            cancellation.cancel();
        }
        if push_history && let View::Analysis(current) = &self.view {
            self.history.push(current.path.clone());
        }
        let (tx, rx) = mpsc::channel();
        let worker_path = path.clone();
        let cancellation = ScanCancellation::default();
        let worker_cancellation = cancellation.clone();
        thread::spawn(move || {
            let result = crate::scan::scan_with_cancellation(worker_path, &worker_cancellation);
            if !worker_cancellation.is_cancelled() {
                let _ = tx.send(result);
            }
        });
        self.scan_rx = Some(rx);
        self.scan_cancellation = Some(cancellation);
        self.view = View::Loading(path);
    }

    fn choose_folder(&mut self) {
        if self.picker_rx.is_none() {
            self.picker_rx = Some(
                ui::portal::PickRequest::directory("Choose a folder to analyze", "Analyze")
                    .thread_name("disk-usage-folder-picker")
                    .send(),
            );
        }
    }

    fn cancel_scan(&mut self) {
        if let Some(cancellation) = self.scan_cancellation.take() {
            cancellation.cancel();
        }
        self.scan_rx = None;
    }

    fn go_home(&mut self) {
        self.cancel_scan();
        self.history.clear();
        self.view = View::Home;
    }

    fn go_back(&mut self) {
        self.cancel_scan();
        if let Some(path) = self.history.pop() {
            self.start_scan(path, false);
        } else {
            self.view = View::Home;
        }
    }

    fn back_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + 18.0,
            self.rect.y0 + 18.0,
            self.rect.x0 + 116.0,
            self.rect.y0 + 58.0,
        )
    }

    fn cards(&self) -> Vec<Place> {
        let recent_limit = if self.rect.width() < 700.0 { 2 } else { 5 };
        let mut out = self
            .recent
            .iter()
            .take(recent_limit)
            .map(|path| Place {
                label: path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("File system")
                    .into(),
                caption: path.to_string_lossy().into_owned(),
                path: path.clone(),
                choose: false,
            })
            .collect::<Vec<_>>();
        out.extend(self.places.clone());
        out.push(Place {
            label: "Choose folder".into(),
            caption: "Analyze another location".into(),
            path: PathBuf::new(),
            choose: true,
        });
        out
    }

    fn card_rect(&self, index: usize) -> Rect {
        let content_w = (self.rect.width() - 64.0).min(900.0);
        let cols = if content_w >= 700.0 { 2 } else { 1 };
        let w = (content_w - 16.0 * (cols - 1) as f64) / cols as f64;
        let narrow = self.rect.width() < 700.0;
        let height = if narrow { 78.0 } else { 88.0 };
        let stride = if narrow { 90.0 } else { 104.0 };
        let x0 = self.rect.center().x - content_w / 2.0 + (index % cols) as f64 * (w + 16.0);
        let y0 = self.rect.y0 + if narrow { 160.0 } else { 190.0 } + (index / cols) as f64 * stride;
        Rect::new(x0, y0, x0 + w, y0 + height)
    }
}

impl Default for DiskUsageApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DiskUsageApp {
    fn drop(&mut self) {
        if let Some(cancellation) = &self.scan_cancellation {
            cancellation.cancel();
        }
    }
}

impl DiskUsageApp {
    fn answer_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<Option<ActionOutput>, String> {
        if kind == OperationKind::Document || type_id != OPERATION_TYPE {
            return Ok(None);
        }
        let operation: DiskUsageOperation =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        match operation {
            DiskUsageOperation::Analyze { path } => {
                self.start_scan(path, matches!(self.view, View::Analysis(_)));
            }
            DiskUsageOperation::Choose => self.choose_folder(),
            DiskUsageOperation::Back => self.go_back(),
            DiskUsageOperation::Home => self.go_home(),
        }
        Ok(Some(ActionOutput::default()))
    }
}

impl Widget for DiskUsageApp {
    ui::provides!(dyn ui::resources::PublishesResources);

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, _) = ui::doc::document(state, path);
        if !self.shot_mode {
            self.recent = DiskUsageDoc::recent(state);
        }
    }
    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        let (state, _) = ui::doc::document(state, path);
        if let Some(outcome) = self
            .picker_rx
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
        {
            self.picker_rx = None;
            match outcome {
                ui::portal::PortalOutcome::Opened(path) => self.start_scan(path, false),
                ui::portal::PortalOutcome::Cancelled => {}
                ui::portal::PortalOutcome::Failed(message) => {
                    self.view = View::Error(PathBuf::new(), message);
                }
            }
            return true;
        }
        if let Some(rx) = &self.scan_rx
            && let Ok(result) = rx.try_recv()
        {
            self.scan_rx = None;
            self.scan_cancellation = None;
            if let Some(message) = result.error.clone() {
                self.view = View::Error(result.path, message);
            } else {
                DiskUsageDoc::remember(state, &result.path);
                self.set_result(result);
            }
            return true;
        }
        if let Some(id) = self.chart.take_activated() {
            let path = PathBuf::from(id);
            if path.file_name().is_some_and(|n| n != ".") {
                self.start_scan(path, true);
                return true;
            }
        }
        false
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.chart.build_layout(taffy, theme, node_map);
        let _ = taffy.set_style(
            child,
            Style {
                position: Position::Absolute,
                inset: TRect {
                    left: LengthPercentageAuto::length(24.0),
                    right: LengthPercentageAuto::length(24.0),
                    top: LengthPercentageAuto::length(TOP_H as f32),
                    bottom: LengthPercentageAuto::length(18.0),
                },
                size: Size {
                    width: Dimension::auto(),
                    height: Dimension::auto(),
                },
                ..Default::default()
            },
        );
        let node = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x + l.size.width),
            origin.y + f64::from(l.location.y + l.size.height),
        );
        if let Ok(children) = taffy.children(node)
            && let Some(child) = children.first()
        {
            self.chart.apply_layout(taffy, *child, origin);
        }
    }
    #[allow(
        clippy::too_many_lines,
        reason = "the four small visual states share one retained canvas and theme context"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        match &self.view {
            View::Home => {
                tr.draw_text_weighted(
                    scene,
                    "Analyze storage",
                    theme.tokens.typography.scale.x3l,
                    theme.tokens.colors.text_primary,
                    transform
                        * Affine::translate((self.rect.center().x - 220.0, self.rect.y0 + 76.0)),
                    None,
                    TextWeight::Bold,
                );
                tr.draw_text(
                    scene,
                    "Choose a location to see what is taking up space",
                    theme.tokens.typography.scale.base,
                    theme.tokens.colors.text_dim,
                    transform
                        * Affine::translate((self.rect.center().x - 220.0, self.rect.y0 + 128.0)),
                    None,
                );
                for (i, place) in self.cards().iter().enumerate() {
                    let r = self.card_rect(i);
                    let hover = r.contains(self.pointer);
                    Button::paint(
                        scene,
                        transform,
                        tr,
                        theme,
                        &ButtonFace::new(r, &place.label).hovered(hover),
                    );
                    tr.draw_text(
                        scene,
                        &place.caption,
                        theme.tokens.typography.scale.xs,
                        theme.tokens.colors.text_dim,
                        transform * Affine::translate((r.x0 + 18.0, r.y1 - 25.0)),
                        Some((r.width() - 36.0) as f32),
                    );
                }
            }
            View::Loading(path) => {
                let back = self.back_rect();
                Button::paint(
                    scene,
                    transform,
                    tr,
                    theme,
                    &ButtonFace::new(back, "← Back").hovered(back.contains(self.pointer)),
                );
                let title = path.to_string_lossy();
                tr.draw_text_weighted(
                    scene,
                    "Calculating folder sizes…",
                    theme.tokens.typography.scale.x2l,
                    theme.tokens.colors.text_primary,
                    transform
                        * Affine::translate((
                            self.rect.center().x - 170.0,
                            self.rect.center().y - 24.0,
                        )),
                    None,
                    TextWeight::Semibold,
                );
                tr.draw_text(
                    scene,
                    &title,
                    theme.tokens.typography.scale.sm,
                    theme.tokens.colors.text_dim,
                    transform
                        * Affine::translate((
                            self.rect.center().x - 170.0,
                            self.rect.center().y + 18.0,
                        )),
                    None,
                );
            }
            View::Analysis(result) => {
                let back = self.back_rect();
                Button::paint(
                    scene,
                    transform,
                    tr,
                    theme,
                    &ButtonFace::new(back, "← Back").hovered(back.contains(self.pointer)),
                );
                let title = result.path.to_string_lossy();
                tr.draw_text_weighted(
                    scene,
                    &title,
                    theme.tokens.typography.scale.lg,
                    theme.tokens.colors.text_primary,
                    transform * Affine::translate((self.rect.x0 + 132.0, self.rect.y0 + 25.0)),
                    Some((self.rect.width() - 360.0).max(160.0) as f32),
                    TextWeight::Semibold,
                );
                let stats = format!(
                    "{} items  ·  {} unreadable",
                    result.files, result.unreadable
                );
                tr.draw_text(
                    scene,
                    &stats,
                    theme.tokens.typography.scale.xs,
                    theme.tokens.colors.text_dim,
                    transform * Affine::translate((self.rect.x1 - 220.0, self.rect.y0 + 30.0)),
                    None,
                );
                // Assign theme-derived colours immediately before the shared chart paints.
                let slices = result
                    .entries
                    .iter()
                    .enumerate()
                    .map(|(i, e)| {
                        let slice = PieSlice::new(
                            e.path.to_string_lossy(),
                            &e.name,
                            e.bytes as f64,
                            Self::palette(theme, i),
                        )
                        .with_detail(format!(
                            "{} · {} files",
                            bytes_si(e.bytes),
                            e.files
                        ));
                        if e.path.file_name().is_some_and(|name| name == ".") {
                            slice.non_interactive()
                        } else {
                            slice
                        }
                    })
                    .collect();
                self.chart.style_mut().tooltip_min_width = 220.0;
                self.chart.style_mut().tooltip_right_inset = if self.rect.width() >= 900.0 {
                    245.0
                } else {
                    0.0
                };
                if self.shot_mode {
                    self.chart.set_slices_immediate(slices);
                } else {
                    self.chart.set_slices(slices);
                }
                self.chart.draw(scene, transform, tr, theme, presence);
                if self.rect.width() >= 900.0 {
                    let x = self.rect.x1 - 245.0;
                    let mut y = self.rect.y0 + 122.0;
                    tr.draw_text_weighted(
                        scene,
                        "Largest folders",
                        theme.tokens.typography.scale.sm,
                        theme.tokens.colors.text_primary,
                        transform * Affine::translate((x, y - 34.0)),
                        None,
                        TextWeight::Semibold,
                    );
                    for (i, entry) in result.entries.iter().take(7).enumerate() {
                        scene.fill(
                            Fill::NonZero,
                            transform,
                            Self::palette(theme, i),
                            None,
                            &Circle::new((x + 5.0, y + 7.0), 5.0),
                        );
                        tr.draw_text(
                            scene,
                            &entry.name,
                            theme.tokens.typography.scale.sm,
                            theme.tokens.colors.text_primary,
                            transform * Affine::translate((x + 20.0, y)),
                            Some(125.0),
                        );
                        let percent = if result.total == 0 {
                            0.0
                        } else {
                            entry.bytes as f64 / result.total as f64 * 100.0
                        };
                        tr.draw_text(
                            scene,
                            &format!("{} · {percent:.0}%", bytes_si(entry.bytes)),
                            theme.tokens.typography.scale.xs,
                            theme.tokens.colors.text_dim,
                            transform * Affine::translate((x + 20.0, y + 22.0)),
                            None,
                        );
                        y += 58.0;
                    }
                } else {
                    // Preserve the category-to-colour mapping on compact
                    // screens instead of dropping the legend altogether.
                    let columns = 2;
                    let column_width = (self.rect.width() - 40.0) / f64::from(columns);
                    let y0 = self.rect.y1 - 68.0;
                    for (i, entry) in result.entries.iter().take(6).enumerate() {
                        let column = i % columns as usize;
                        let row = i / columns as usize;
                        let x = self.rect.x0 + 20.0 + column as f64 * column_width;
                        let y = y0 + row as f64 * 22.0;
                        scene.fill(
                            Fill::NonZero,
                            transform,
                            Self::palette(theme, i),
                            None,
                            &Circle::new((x + 4.0, y + 6.0), 4.0),
                        );
                        let percent = if result.total == 0 {
                            0.0
                        } else {
                            entry.bytes as f64 / result.total as f64 * 100.0
                        };
                        tr.draw_text(
                            scene,
                            &format!("{}  {percent:.0}%", entry.name),
                            theme.tokens.typography.scale.xs,
                            theme.tokens.colors.text_dim,
                            transform * Affine::translate((x + 14.0, y)),
                            Some((column_width - 18.0).max(40.0) as f32),
                        );
                    }
                }
            }
            View::Error(path, message) => {
                let back = self.back_rect();
                Button::paint(
                    scene,
                    transform,
                    tr,
                    theme,
                    &ButtonFace::new(back, "← Back").hovered(back.contains(self.pointer)),
                );
                tr.draw_text_weighted(
                    scene,
                    "Couldn’t analyze this folder",
                    theme.tokens.typography.scale.x2l,
                    theme.tokens.colors.text_primary,
                    transform * Affine::translate((48.0, 120.0)),
                    None,
                    TextWeight::Bold,
                );
                let detail = if path.as_os_str().is_empty() {
                    message.clone()
                } else {
                    format!("{} — {message}", path.display())
                };
                tr.draw_text(
                    scene,
                    &detail,
                    theme.tokens.typography.scale.sm,
                    theme.tokens.colors.text_dim,
                    transform * Affine::translate((48.0, 170.0)),
                    None,
                );
            }
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            }
        ) {
            match &self.view {
                View::Home => {
                    if let Some(place) = self
                        .cards()
                        .iter()
                        .enumerate()
                        .find(|(i, _)| self.card_rect(*i).contains(self.pointer))
                        .map(|(_, p)| p.clone())
                    {
                        if place.choose {
                            self.choose_folder();
                        } else {
                            self.start_scan(place.path, false);
                        }
                        *ctx.handled = true;
                    }
                }
                _ => {
                    if self.back_rect().contains(self.pointer) {
                        self.go_back();
                        *ctx.handled = true;
                    }
                }
            }
        }
    }
    fn accessibility(&self) -> Option<AccessNode> {
        let mut root = AccessNode::new(Role::Window).with_name("Disk Usage Analyzer");
        if matches!(self.view, View::Home) {
            root.children = self
                .cards()
                .iter()
                .map(|p| {
                    AccessNode::new(Role::Button)
                        .with_action_target(if p.choose {
                            "disk-usage.choose".into()
                        } else {
                            p.path.to_string_lossy().into_owned()
                        })
                        .with_name(&p.label)
                        .with_description(&p.caption)
                        .with_state(AccessState {
                            focusable: true,
                            ..Default::default()
                        })
                })
                .collect();
        } else {
            root.children = vec![
                AccessNode::new(Role::Button)
                    .with_action_target("disk-usage.back")
                    .with_name("Back")
                    .with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    }),
            ];
        }
        Some(root)
    }
    fn perform_action_at(
        &mut self,
        action: &ui::a11y::Action,
        key_path: &[u64],
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        let Some(key) = key_path.last() else {
            return false;
        };
        if *key == ui::a11y::hash_key(&"disk-usage.back") {
            self.go_back();
            return true;
        }
        if *key == ui::a11y::hash_key(&"disk-usage.choose") {
            self.choose_folder();
            return true;
        }
        if let Some(place) = self
            .cards()
            .into_iter()
            .filter(|place| !place.choose)
            .find(|place| ui::a11y::hash_key(&place.path.to_string_lossy()) == *key)
        {
            self.start_scan(place.path, false);
            return true;
        }
        false
    }
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[OPERATION_TYPE]
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
            .map(ui::widget::ResourceOperationStart::Completed))
    }
    fn children(&self) -> Vec<&dyn Widget> {
        if matches!(self.view, View::Analysis(_)) {
            vec![&self.chart]
        } else {
            vec![]
        }
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if matches!(self.view, View::Analysis(_)) {
            vec![&mut self.chart]
        } else {
            vec![]
        }
    }
    fn name(&self) -> &'static str {
        "DiskUsageApp"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
    fn next_wake_deadline(&self) -> Option<Instant> {
        (self.scan_rx.is_some() || self.picker_rx.is_some())
            .then_some(())
            .map(|()| ui::time::now() + Duration::from_millis(80))
    }
}

impl DiskUsageApp {
    pub(crate) fn mark_framework_hosted(&mut self) {
        self.hosted = true;
    }

    pub(crate) fn is_framework_hosted(&self) -> bool {
        self.hosted
    }

    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }

    pub(crate) fn in_analysis(&self) -> bool {
        matches!(self.view, View::Analysis(_))
    }

    /// Places and the open scan. Not a second `disk-usage` root; choose / home
    /// / back already ride `local_operations`.
    fn live_analyze_extras(&self) -> Vec<Resource> {
        let mut extras = Vec::new();
        for place in &self.places {
            if let Some(resource) = analyze_resource(&place.label, &place.path) {
                extras.push(resource);
            }
        }
        if let View::Analysis(result) = &self.view {
            for entry in &result.entries {
                if entry.path.file_name().is_some_and(|name| name != ".")
                    && let Some(resource) = analyze_resource(&entry.name, &entry.path)
                {
                    extras.push(resource);
                }
            }
        }
        extras
    }
}

fn analyze_resource(label: &str, path: &std::path::Path) -> Option<Resource> {
    let mut resource = Resource::new(
        format!("disk-usage.folder:{}", path.display()),
        "disk-usage.folder",
        label,
    );
    resource.actions.push(operation_action(
        &format!("analyze:{}", path.display()),
        &format!("Analyze {label}"),
        &DiskUsageOperation::Analyze {
            path: path.to_path_buf(),
        },
    )?);
    Some(resource)
}

impl ui::resources::PublishesResources for DiskUsageApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        if self.hosted {
            return self.live_analyze_extras();
        }
        let state = <crate::resource::DiskUsage as ui::tilo_resource::Resource>::read(doc)
            .unwrap_or_default();
        let mut root = ui::tilo_resource::publish::<crate::resource::DiskUsage>(
            "disk-usage:root",
            "Disk Usage Analyzer",
            &state,
        );
        root.children.extend(self.live_analyze_extras());
        vec![root]
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        if self.hosted && id == "disk-usage:root" {
            return None;
        }
        fn find(resources: &[Resource], id: &str) -> Option<Resource> {
            for resource in resources {
                if resource.id.as_str() == id {
                    return Some(resource.clone());
                }
                if let Some(found) = find(&resource.children, id) {
                    return Some(found);
                }
            }
            None
        }
        let state = <crate::resource::DiskUsage as ui::tilo_resource::Resource>::read(doc)
            .unwrap_or_default();
        let mut root = ui::tilo_resource::publish::<crate::resource::DiskUsage>(
            "disk-usage:root",
            "Disk Usage Analyzer",
            &state,
        );
        root.children.extend(self.live_analyze_extras());
        find(&[root], id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Disk Usage publishes only effect operations (scans and navigation), so
    // the registry decodes nothing; the sweep checks the published surface.
    ui::action_contract_tests! {
        seed: AutoCommit::new(),
        registry: ui::tilo_actions::ActionRegistry::new(),
        resources: |doc| ui::resources::resources(&DiskUsageApp::sample(), doc),
        palette: ["Analyze Projects", "Choose folder", "Go back"],
    }

    /// Common locations are exposed as resources, and the analyze action, clicks and a portal-picked folder all enter the same scan pipeline.
    #[test]
    fn folder_scans_enter_through_resources_clicks_and_portal() {
        // --- phase: resource_surface_exposes_common_locations ---
        {
            let app = DiskUsageApp::new();
            let resources = ui::resources::resources(&app, &AutoCommit::new());
            assert_eq!(resources.len(), 1);
            assert!(resources[0].actions.len() >= 3);
            assert_eq!(app.local_resource_operation_handlers(), &[OPERATION_TYPE]);
        }

        // --- phase: analyze_operation_uses_the_same_navigation_path_as_clicks ---
        {
            let mut app = DiskUsageApp::new();
            let path = PathBuf::from("/definitely-not-a-real-disk-usage-test-path");
            let payload = serde_json::to_vec(&DiskUsageOperation::Analyze { path: path.clone() })
                .unwrap_or_default();
            let handled = app
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        OperationKind::Effect,
                        OPERATION_TYPE,
                        &payload,
                    ),
                    &mut AutoCommit::new(),
                    &ObjId::Root,
                )
                .map(|started| started.and_then(ui::widget::ResourceOperationStart::completed))
                .unwrap_or_default();
            assert!(handled.is_some());
            assert!(matches!(app.view, View::Loading(ref loading) if loading == &path));
            assert!(app.next_wake_deadline().is_some());
            app.go_home();
            assert!(matches!(app.view, View::Home));
            assert!(app.next_wake_deadline().is_none());
        }

        // --- phase: portal_selected_folder_enters_the_scan_pipeline ---
        {
            let mut app = DiskUsageApp::new();
            let selected = PathBuf::from("/portal-selected-folder");
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(selected.clone()));
            let payload = serde_json::to_vec(&DiskUsageOperation::Choose).unwrap_or_default();
            let handled = ui::host::scoped(&services, || {
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        OperationKind::Effect,
                        OPERATION_TYPE,
                        &payload,
                    ),
                    &mut AutoCommit::new(),
                    &ObjId::Root,
                )
                .map(|started| started.and_then(ui::widget::ResourceOperationStart::completed))
                .unwrap_or_default()
            });
            assert!(handled.is_some());
            let picks = recorder.pick_requests();
            assert_eq!(picks.len(), 1);
            assert!(picks[0].is_directory());
            let mut document = AutoCommit::new();
            assert!(app.update(&mut document, &ObjId::Root));
            assert!(matches!(app.view, View::Loading(ref path) if path == &selected));
            app.cancel_scan();
        }
    }

    /// The idle analyzer schedules no frames and the narrow home view keeps common places visible.
    #[test]
    fn home_view_idles_and_keeps_common_places_visible() {
        // --- phase: idle_analyzer_does_not_schedule_frames ---
        {
            assert!(DiskUsageApp::new().next_wake_deadline().is_none());
        }

        // --- phase: narrow_home_keeps_common_places_visible ---
        {
            let mut app = DiskUsageApp::sample_home();
            app.recent.extend([
                PathBuf::from("/one"),
                PathBuf::from("/two"),
                PathBuf::from("/three"),
            ]);
            app.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
            assert_eq!(app.cards().len(), 6);
            assert!(app.cards().iter().any(|place| place.label == "File system"));
            assert_eq!(
                app.cards().last().map(|place| place.label.as_str()),
                Some("Choose folder")
            );
        }
    }

    #[test]
    fn hosted_recent_folder_is_the_published_row() {
        let mut doc = AutoCommit::new();
        let path = PathBuf::from("/tmp/notes");
        DiskUsageDoc::remember(&mut doc, &path);
        doc.commit();
        let mut app = DiskUsageApp::new();
        app.mark_hosted_for_test();
        assert!(ui::resources::resource(&app, &doc, "disk-usage:root").is_none());
        let row =
            ui::resources::resource(&app, &doc, &format!("disk-usage.recent:{}", path.display()))
                .expect("hosted recent folder");
        assert_eq!(row.kind, "disk-usage.recent");
        assert!(
            row.actions
                .iter()
                .any(|action| action.id.starts_with("analyze:")),
            "a hosted recent folder stays the published row"
        );
    }
}
