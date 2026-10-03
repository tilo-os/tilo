use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use ui::time::{Duration, Instant};
use ui::widget::{AppModel, ModelPath};

use ui::automerge::{AutoCommit, ObjId};
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::{TextKind, TextRenderer};
use ui::theme::Theme;
use ui::tilo_actions::{
    ActionContract, ActionOutput, ActionSpec, DiscoveryScope, OperationKind, Resource,
};
use ui::tokens::TextRole;
use ui::vello::Scene;
use ui::vello::kurbo::Affine;
use ui::widget::{
    Button, Column, Container, Element, EmptyState, ListRow, PieChart, PieSlice, Row, Scrollable,
    Section, SectionHeader, Text, Widget,
};

use crate::doc::DiskUsageDoc;
use crate::scan::{Entry, ScanCancellation, ScanResult};
use ui::format::bytes_si;

pub(crate) const OPERATION_TYPE: &str = "disk-usage.operation";

/// How many folders the chart names before it groups the rest.
const NAMED_SLICES: usize = 8;

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
    path: PathBuf,
}

enum View {
    Home,
    Loading(PathBuf),
    Analysis(ScanResult),
    Error(PathBuf, String),
}

/// What a control in the current view asks for. Controls run inside the
/// widget tree; the app applies their requests on its next update.
enum Request {
    Analyze(PathBuf),
    Choose,
    Back,
}

type Inbox = Rc<RefCell<Vec<Request>>>;

/// `12842` as `12,842`.
fn count_text(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn files_text(n: u64) -> String {
    if n == 1 {
        "1 file".into()
    } else {
        format!("{} files", count_text(n))
    }
}

struct SliceRow {
    id: String,
    label: String,
    bytes: u64,
    detail: String,
    interactive: bool,
}

/// The chart's slices: the largest folders by name, the remainder grouped.
fn slice_rows(entries: &[Entry]) -> Vec<SliceRow> {
    let row = |entry: &Entry| SliceRow {
        id: entry.path.to_string_lossy().into_owned(),
        label: entry.name.clone(),
        bytes: entry.bytes,
        detail: format!("{}, {}", bytes_si(entry.bytes), files_text(entry.files)),
        interactive: entry.path.file_name().is_some_and(|name| name != "."),
    };
    let mut rows: Vec<SliceRow> = entries.iter().take(NAMED_SLICES).map(row).collect();
    let rest = entries.get(NAMED_SLICES..).unwrap_or_default();
    if !rest.is_empty() {
        let bytes = rest
            .iter()
            .fold(0_u64, |sum, entry| sum.saturating_add(entry.bytes));
        let files = rest
            .iter()
            .fold(0_u64, |sum, entry| sum.saturating_add(entry.files));
        rows.push(SliceRow {
            id: "disk-usage.rest".into(),
            label: "Other folders".into(),
            bytes,
            detail: format!("{}, {}", bytes_si(bytes), files_text(files)),
            interactive: false,
        });
    }
    rows
}

/// The donut of one scan. Slice colours come from the theme the chart is
/// drawn with, and choosing a folder asks the app to analyze it.
struct SliceChart {
    chart: PieChart,
    rows: Vec<SliceRow>,
    inbox: Inbox,
}

impl SliceChart {
    fn new(result: &ScanResult, inspected: Option<&str>, inbox: Inbox) -> Self {
        let rows = slice_rows(&result.entries);
        let mut chart = PieChart::new(Vec::new()).with_legend();
        chart.set_center(bytes_si(result.total), "used in this folder");
        chart.style_mut().tooltip_min_width = 220.0;
        let mut this = Self { chart, rows, inbox };
        this.recolour(&Theme::default());
        if let Some(id) = inspected {
            this.chart.inspect(id);
        }
        this
    }

    fn recolour(&mut self, theme: &Theme) {
        let series = &theme.tokens.colors.presence;
        let slices = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let slice = PieSlice::new(&row.id, &row.label, row.bytes as f64, series.series(i))
                    .with_detail(&row.detail);
                if row.interactive {
                    slice
                } else {
                    slice.non_interactive()
                }
            })
            .collect();
        self.chart.set_slices_immediate(slices);
    }
}

impl ui::widget::Decorator for SliceChart {
    type Inner = PieChart;

    fn inner(&self) -> &PieChart {
        &self.chart
    }

    fn inner_mut(&mut self) -> &mut PieChart {
        &mut self.chart
    }

    fn update(&mut self, _state: &mut AppModel, _path: &ModelPath) -> bool {
        match self.chart.take_activated() {
            Some(id) => {
                self.inbox
                    .borrow_mut()
                    .push(Request::Analyze(PathBuf::from(id)));
                true
            }
            None => false,
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.recolour(theme);
        self.chart.draw(scene, transform, text, theme, presence);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &ui::permissions::PermissionContext,
    ) {
        self.recolour(theme);
        self.chart
            .draw_with_presence(scene, transform, text, theme, presence, permissions);
    }

    fn name(&self) -> &'static str {
        "SliceChart"
    }
}

pub struct DiskUsageApp {
    view: View,
    content: Element,
    structure_dirty: bool,
    inspected: Option<String>,
    history: Vec<PathBuf>,
    recent: Vec<PathBuf>,
    places: Vec<Place>,
    inbox: Inbox,
    scan_rx: Option<Receiver<ScanResult>>,
    scan_cancellation: Option<ScanCancellation>,
    picker_rx: Option<ui::portal::PortalChannel>,
    shot_mode: bool,
    pub(crate) actions: crate::resource::ActionSink,
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
                path: home,
            },
            Place {
                label: "Workspace".into(),
                path: workspace,
            },
            Place {
                label: "File system".into(),
                path: PathBuf::from("/"),
            },
        ];
        let mut app = Self {
            view: View::Home,
            content: Box::new(Column::new()),
            structure_dirty: false,
            inspected: None,
            history: Vec::new(),
            recent: Vec::new(),
            places,
            inbox: Rc::default(),
            scan_rx: None,
            scan_cancellation: None,
            picker_rx: None,
            shot_mode: false,
            actions: crate::resource::ActionSink::default(),
            hosted: false,
        };
        app.rebuild();
        app
    }

    /// A finished scan of a typical home folder.
    pub fn sample() -> Self {
        let mut app = Self::new();
        app.shot_mode = true;
        let path = PathBuf::from("/home/you");
        let values = [
            ("Projects", 84_200_000_000, 48_310),
            ("Videos", 48_600_000_000, 1_204),
            ("Photos", 31_800_000_000, 9_870),
            ("Downloads", 19_400_000_000, 2_615),
            ("Documents", 8_700_000_000, 4_132),
            ("Music", 5_100_000_000, 1_086),
        ];
        let entries = values
            .into_iter()
            .map(|(name, bytes, files)| Entry {
                path: path.join(name.to_lowercase()),
                name: name.into(),
                bytes,
                files,
                unreadable: 0,
            })
            .collect::<Vec<_>>();
        let total = entries.iter().map(|e| e.bytes).sum();
        let files = entries.iter().map(|e| e.files).sum();
        app.set_result(ScanResult {
            path,
            entries,
            total,
            files,
            unreadable: 0,
            error: None,
        });
        app
    }

    /// A scan that has not finished.
    pub fn sample_loading() -> Self {
        let mut app = Self::sample_home();
        app.show(View::Loading(PathBuf::from("/home/you")));
        app
    }

    /// A folder the app could not read.
    pub fn sample_error() -> Self {
        let mut app = Self::sample_home();
        app.show(View::Error(
            PathBuf::from("/root"),
            "The folder is unavailable or access was not granted".into(),
        ));
        app
    }

    /// The same scan with one folder's tooltip showing.
    pub fn sample_with_inspection() -> Self {
        let mut app = Self::sample();
        app.inspected = Some("/home/you/projects".into());
        app.rebuild();
        app
    }

    /// The home view as it looks after a few analyses.
    pub fn sample_home() -> Self {
        let mut app = Self::new();
        app.shot_mode = true;
        app.places = vec![
            Place {
                label: "Home".into(),
                path: PathBuf::from("/home/you"),
            },
            Place {
                label: "Workspace".into(),
                path: PathBuf::from(tilo_paths::WORKSPACE_MOUNT),
            },
            Place {
                label: "File system".into(),
                path: PathBuf::from("/"),
            },
        ];
        app.recent = vec![
            PathBuf::from("/home/you/Projects/tilo"),
            PathBuf::from(tilo_paths::WORKSPACE_MOUNT).join("design-system"),
        ];
        app.rebuild();
        app
    }

    fn set_result(&mut self, result: ScanResult) {
        if self.recent.first() != Some(&result.path) {
            self.recent.retain(|p| p != &result.path);
            self.recent.insert(0, result.path.clone());
            self.recent.truncate(5);
        }
        self.show(View::Analysis(result));
    }

    fn show(&mut self, view: View) {
        self.view = view;
        self.rebuild();
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
        self.show(View::Loading(path));
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
        self.show(View::Home);
    }

    fn go_back(&mut self) {
        self.cancel_scan();
        if let Some(path) = self.history.pop() {
            self.start_scan(path, false);
        } else {
            self.show(View::Home);
        }
    }

    fn apply(&mut self, request: Request) {
        match request {
            Request::Analyze(path) => {
                let push = matches!(self.view, View::Analysis(_));
                self.start_scan(path, push);
            }
            Request::Choose => self.choose_folder(),
            Request::Back => self.go_back(),
        }
    }

    /// The places and the recent folders the home view offers. A recent
    /// folder that is also a place is listed once, as the place.
    fn locations(&self) -> (Vec<Place>, Vec<Place>) {
        let recent = self
            .recent
            .iter()
            .filter(|path| self.places.iter().all(|place| &place.path != *path))
            .take(5)
            .map(|path| Place {
                label: path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("File system")
                    .into(),
                path: path.clone(),
            })
            .collect();
        (self.places.clone(), recent)
    }

    fn rebuild(&mut self) {
        self.content = match &self.view {
            View::Home => self.home_view(),
            View::Loading(path) => self.loading_view(path),
            View::Analysis(result) => self.analysis_view(result),
            View::Error(path, message) => self.error_view(path, message),
        };
        self.structure_dirty = true;
    }

    fn back_button(&self) -> Button {
        let inbox = Rc::clone(&self.inbox);
        Button::icon_only("disk-usage.back", Icon::ChevronLeft, "Back")
            .on_click(move || inbox.borrow_mut().push(Request::Back))
    }

    fn place_row(&self, place: &Place) -> Element {
        let inbox = Rc::clone(&self.inbox);
        let path = place.path.clone();
        Box::new(
            ListRow::new(
                format!("disk-usage.place:{}", place.path.display()),
                &place.label,
            )
            .icon(Icon::Folder)
            .secondary(place.path.to_string_lossy())
            .disclosure()
            .on_activate(move || inbox.borrow_mut().push(Request::Analyze(path.clone()))),
        )
    }

    fn home_view(&self) -> Element {
        let (places, recent) = self.locations();
        let inbox = Rc::clone(&self.inbox);
        let choose = Button::new("disk-usage.choose", "Choose folder", None)
            .with_icon(Icon::FolderOpen)
            .on_click(move || inbox.borrow_mut().push(Request::Choose));
        let mut page = Column::new()
            .with_intrinsic_height()
            .push(Box::new(
                Text::new("Analyze storage")
                    .with_role(TextRole::LargeTitle)
                    .with_heading_level(1),
            ))
            .push(Box::new(
                Section::new(places.iter().map(|p| self.place_row(p)).collect())
                    .with_header(SectionHeader::heading("Places").action(choose)),
            ));
        if !recent.is_empty() {
            page = page.push(Box::new(Section::titled(
                "Recent",
                recent.iter().map(|p| self.place_row(p)).collect(),
            )));
        }
        Box::new(Scrollable::new_vertical().with_child(Box::new(page)))
    }

    /// The Back button on its own, for the views that have no title yet.
    fn back_row(&self) -> Row {
        Row::new().no_wrap().push(Box::new(self.back_button()))
    }

    fn loading_view(&self, path: &std::path::Path) -> Element {
        Box::new(Column::new().push(Box::new(self.back_row())).push(Box::new(
            EmptyState::new("Calculating folder sizes").with_message(path.to_string_lossy()),
        )))
    }

    fn error_view(&self, path: &std::path::Path, message: &str) -> Element {
        let detail = if path.as_os_str().is_empty() {
            message.to_owned()
        } else {
            format!("{}: {message}", path.display())
        };
        Box::new(Column::new().push(Box::new(self.back_row())).push(Box::new(
            EmptyState::error("Couldn’t analyze this folder", detail).with_icon(Icon::FolderOpen),
        )))
    }

    fn analysis_view(&self, result: &ScanResult) -> Element {
        let stats = if result.unreadable > 0 {
            format!(
                "{}, {} unreadable",
                files_text(result.files),
                count_text(result.unreadable)
            )
        } else {
            files_text(result.files)
        };
        let heading = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(
                Text::new(&result.path.to_string_lossy())
                    .with_role(TextRole::Title)
                    .with_kind(TextKind::Identifier),
            ))
            .push(Box::new(
                Text::new(&stats).with_role(TextRole::Caption).numeric(),
            ));
        let header = Row::new()
            .no_wrap()
            .push(Box::new(self.back_button()))
            .push(Box::new(heading));
        let chart = SliceChart::new(result, self.inspected.as_deref(), Rc::clone(&self.inbox));
        Box::new(
            Column::new().push(Box::new(header)).push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_centered_max_width(760.0)
                    .with_child(Box::new(chart)),
            )),
        )
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

impl ui::widget::Decorator for DiskUsageApp {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::resources::PublishesResources;
        inner = content
    );

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let recent = {
            let (doc, _) = ui::doc::document(state, path);
            DiskUsageDoc::recent(doc)
        };
        if !self.shot_mode && recent != self.recent {
            self.recent = recent;
            if matches!(self.view, View::Home) {
                self.rebuild();
            }
        }
        self.content.sync(state, path);
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        let mut changed = self.content.update(state, path);
        let requests = std::mem::take(&mut *self.inbox.borrow_mut());
        for request in requests {
            self.apply(request);
            changed = true;
        }
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
                    self.show(View::Error(PathBuf::new(), message));
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
                self.show(View::Error(result.path, message));
            } else {
                let (doc, _) = ui::doc::document(state, path);
                self.remember_scanned(doc, &result.path);
                self.set_result(result);
            }
            return true;
        }
        changed
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty) | self.content.take_structure_dirty()
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
        if let Some(started) = self.content.start_operation(op, doc, path)? {
            return Ok(Some(started));
        }
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .map(ui::widget::ResourceOperationStart::Completed))
    }

    fn name(&self) -> &'static str {
        "DiskUsageApp"
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        (self.scan_rx.is_some() || self.picker_rx.is_some())
            .then_some(())
            .map(|()| ui::time::now() + Duration::from_millis(80))
    }
}

impl ui::undo::CommittedActions for DiskUsageApp {
    fn take_committed_action(&mut self) -> Option<Vec<ui::automerge::ChangeHash>> {
        // Hosted remembers are applied by the framework dispatcher, which
        // records its own undo bracket. Standalone stays on the local host.
        if self.actions.is_hosted() {
            return None;
        }
        self.actions.take_committed()
    }
}

impl DiskUsageApp {
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

    /// Submit the shared recent-directory projection. Hosted edits use the
    /// framework dispatcher; standalone, `--shots`, and unit tests use the
    /// local action host.
    pub(crate) fn remember_scanned(&mut self, doc: &mut AutoCommit, path: &std::path::Path) {
        let action = crate::resource::DiskUsageAction::Remember {
            path: path.to_string_lossy().into_owned(),
        };
        let context = ui::tilo_actions::ActionContext::for_action(
            <crate::resource::DiskUsageShell as ui::framework::ResourceApp>::ID,
            <crate::resource::DiskUsage as ui::tilo_resource::Resource>::LABEL,
            &action,
        );
        self.actions.dispatch_with_context(
            doc,
            <crate::resource::DiskUsage as ui::tilo_resource::Resource>::ACTION_TYPE,
            &action,
            context,
        );
    }

    #[cfg(test)]
    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&ui::tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    #[allow(dead_code, reason = "framework migration test helper")]
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
        let state = <crate::resource::DiskUsage as ui::tilo_resource::Resource>::read(doc)
            .unwrap_or_default();
        let mut root = ui::tilo_resource::publish::<crate::resource::DiskUsage>(
            "disk-usage:root",
            "Disk Usage Analyzer",
            &state,
        );
        root.children.extend(self.live_analyze_extras());
        if self.hosted && id == "disk-usage:root" {
            return None;
        }
        find_resource_tree(&[root], id)
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

        // --- phase: home_lists_every_place_and_the_five_latest_recent_folders ---
        {
            let mut app = DiskUsageApp::sample_home();
            app.recent.extend([
                PathBuf::from("/one"),
                PathBuf::from("/two"),
                PathBuf::from("/three"),
                PathBuf::from("/four"),
            ]);
            let (places, recent) = app.locations();
            assert!(places.iter().any(|place| place.label == "File system"));
            assert_eq!(recent.len(), 5);
            assert_eq!(recent[0].label, "tilo");
        }

        // --- phase: a_recent_place_is_listed_once ---
        {
            let mut app = DiskUsageApp::sample_home();
            app.recent.insert(0, PathBuf::from("/"));
            let (_, recent) = app.locations();
            assert!(recent.iter().all(|place| place.path.as_os_str() != "/"));
        }
    }

    /// Counts read with thousands separators and the chart names at most a
    /// handful of folders.
    #[test]
    fn counts_are_grouped_and_the_chart_groups_small_folders() {
        assert_eq!(count_text(0), "0");
        assert_eq!(count_text(999), "999");
        assert_eq!(count_text(12_842), "12,842");
        assert_eq!(count_text(1_000_000), "1,000,000");
        assert_eq!(files_text(1), "1 file");
        assert_eq!(files_text(84_200), "84,200 files");

        let entries: Vec<Entry> = (0..12)
            .map(|i| Entry {
                path: PathBuf::from(format!("/data/folder-{i}")),
                name: format!("folder-{i}"),
                bytes: 1_000 - i,
                files: 10,
                unreadable: 0,
            })
            .collect();
        let rows = slice_rows(&entries);
        assert_eq!(rows.len(), NAMED_SLICES + 1);
        let rest = rows.last().expect("grouped remainder");
        assert_eq!(rest.label, "Other folders");
        assert!(!rest.interactive);
        assert_eq!(
            rest.bytes,
            (NAMED_SLICES as u64..12).map(|i| 1_000 - i).sum::<u64>()
        );
    }

    /// The controls a person reaches in each view drive the same navigation
    /// as the published operations: a place opens a scan, Back returns.
    #[test]
    fn places_and_back_drive_navigation() {
        use ui::headless::FrameHarness;

        let activate = |harness: &mut FrameHarness, name: &str| {
            let node = harness
                .a11y_node_named(name)
                .unwrap_or_else(|| panic!("missing control {name:?}"));
            harness
                .perform_a11y_action(node, &ui::a11y::Action::Activate)
                .expect("activation");
            let _ = harness.poll_frame();
        };

        // --- phase: a_place_starts_a_scan ---
        let mut harness = FrameHarness::for_root(
            ui::state::State::new(),
            DiskUsageApp::sample_home(),
            (1280, 800),
            ui::Theme::dark(),
        );
        activate(&mut harness, "Home");
        let app = harness.root::<DiskUsageApp>();
        // The fixture's home folder does not exist here, so the scan may
        // already have reported it unreadable.
        assert!(
            matches!(
                app.view,
                View::Loading(ref path) | View::Error(ref path, _) if path == &PathBuf::from("/home/you")
            ),
            "choosing a place scans it"
        );
        harness.root_mut::<DiskUsageApp>().cancel_scan();

        // --- phase: back_leaves_an_analysis ---
        let mut harness = FrameHarness::for_root(
            ui::state::State::new(),
            DiskUsageApp::sample(),
            (1280, 800),
            ui::Theme::dark(),
        );
        assert!(harness.root::<DiskUsageApp>().in_analysis());
        activate(&mut harness, "Back");
        assert!(matches!(harness.root::<DiskUsageApp>().view, View::Home));
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
