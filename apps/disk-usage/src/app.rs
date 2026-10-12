use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use ui::widget::{AppModel, ModelPath};

use crate::resource::{DiskUsage, DiskUsageEffect, RecentFolders, analyze, folder_row, recent_row};
use ui::framework::{Actions, Dispatcher, EffectCx, Effected, LiveRow, Setting};
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

use ui::tokens::TextRole;
use ui::vello::Scene;
use ui::vello::kurbo::Affine;
use ui::widget::{
    Button, Column, Container, Element, EmptyState, ListRow, NavShell, PieChart, PieSlice,
    Scrollable, Section, SectionHeader, Text, Widget,
};

use crate::scan::{Entry, ScanCancellation, ScanResult};
use ui::format::{bytes_si, count};

/// How many folders the chart names before it groups the rest.
const NAMED_SLICES: usize = 8;

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

/// The navigation operations the resource publishes, which the view's
/// buttons invoke.
const CHOOSE: DiskUsageEffect = DiskUsageEffect::Choose;

/// What a control in the current view asks for. Controls run inside the
/// widget tree; the app applies their requests on its next update.
enum Request {
    Analyze(PathBuf),
    /// The shell's Back: the folder this one was opened from, or home.
    Back,
}

type Inbox = Rc<RefCell<Vec<Request>>>;

/// A folder as the header row names it; nothing before one is chosen.
fn folder_title(folder: &std::path::Path) -> String {
    folder.to_string_lossy().into_owned()
}

fn files_text(n: u64) -> String {
    if n == 1 {
        "1 file".into()
    } else {
        format!("{} files", count(n))
    }
}

fn unreadable_text(n: u64) -> String {
    if n == 1 {
        "1 item couldn’t be read".into()
    } else {
        format!("{} items couldn’t be read", count(n))
    }
}

struct SliceRow {
    id: String,
    label: String,
    bytes: u64,
    files: u64,
    interactive: bool,
}

impl SliceRow {
    /// What the chart says about this slice to assistive technology.
    fn detail(&self) -> String {
        format!("{}, {}", bytes_si(self.bytes), files_text(self.files))
    }
}

/// The chart's slices: the largest folders by name, the remainder grouped.
fn slice_rows(entries: &[Entry]) -> Vec<SliceRow> {
    let row = |entry: &Entry| SliceRow {
        id: entry.path.to_string_lossy().into_owned(),
        label: entry.name.clone(),
        bytes: entry.bytes,
        files: entry.files,
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
            files,
            interactive: false,
        });
    }
    rows
}

/// The donut of one scan. Slice colours come from the theme the chart is
/// drawn with, and choosing a folder asks the app to analyze it. The donut's
/// middle names the folder under the pointer (its size and name) and the
/// whole scan otherwise; the legend and the folder list already carry every
/// size, so a wedge gets no tooltip laid over the donut.
struct SliceChart {
    chart: PieChart,
    rows: Vec<SliceRow>,
    total: String,
    files: String,
    inbox: Inbox,
}

impl SliceChart {
    fn new(
        result: &ScanResult,
        rows: Vec<SliceRow>,
        inspected: Option<&str>,
        inbox: Inbox,
    ) -> Self {
        let mut chart = PieChart::new(Vec::new()).with_legend();
        chart.style_mut().tooltip = false;
        let mut this = Self {
            chart,
            rows,
            total: bytes_si(result.total),
            files: files_text(result.files),
            inbox,
        };
        this.recolour(&Theme::default());
        if let Some(id) = inspected {
            this.chart.inspect(id);
        }
        this
    }

    /// The slice colours and the middle of the donut, for this frame.
    fn prepare(&mut self, theme: &Theme) {
        self.recolour(theme);
        let hovered = self
            .chart
            .hovered()
            .and_then(|slice| self.rows.iter().find(|row| row.id == slice.id));
        let (title, subtitle) = match hovered {
            Some(row) => (bytes_si(row.bytes), row.label.clone()),
            None => (self.total.clone(), self.files.clone()),
        };
        self.chart.set_center(title, subtitle);
    }

    fn recolour(&mut self, theme: &Theme) {
        let series = &theme.tokens.colors.presence;
        let slices = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                // The grouped remainder is not a folder: it takes the quiet
                // ink rather than a hue of its own.
                let colour = if row.interactive {
                    series.series(i)
                } else {
                    theme.text_tertiary
                };
                let slice = PieSlice::new(&row.id, &row.label, row.bytes as f64, colour)
                    .with_detail(row.detail())
                    .with_legend_detail(bytes_si(row.bytes));
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

    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
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
        self.prepare(theme);
        self.chart.draw(scene, transform, text, theme, presence);
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let mut node = self.chart.accessibility()?;
        for (child, row) in node
            .children
            .iter_mut()
            .zip(self.rows.iter().filter(|row| row.bytes > 0))
        {
            child.key = Some(ui::a11y::hash_key(&row.id));
            child.semantic_key = Some(row.id.clone());
        }
        Some(node)
    }

    fn name(&self) -> &'static str {
        "SliceChart"
    }
}

pub struct DiskUsageApp {
    view: View,
    /// The window: the current view as its content, the folder as the
    /// header's title, and the shell's Back while there is a way back.
    content: NavShell,
    /// Whether Back leads anywhere: any view but home.
    can_go_back: Rc<std::cell::Cell<bool>>,
    structure_dirty: bool,
    inspected: Option<String>,
    history: Vec<PathBuf>,
    pub(crate) recent: Vec<PathBuf>,
    pub(crate) recents: Option<Setting<RecentFolders>>,
    places: Vec<Place>,
    inbox: Inbox,
    scan_cancellation: Option<ScanCancellation>,
    pub(crate) immediate: bool,
    pub(crate) dispatcher: Option<Dispatcher<DiskUsage>>,
    pub(crate) actions: Option<Actions<DiskUsage>>,
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
        let inbox: Inbox = Rc::default();
        let can_go_back = Rc::new(std::cell::Cell::new(false));
        let content = {
            let can = Rc::clone(&can_go_back);
            let inbox = Rc::clone(&inbox);
            NavShell::new().with_id("disk-usage.shell").on_back(
                move || can.get(),
                move || inbox.borrow_mut().push(Request::Back),
            )
        };
        let mut app = Self {
            view: View::Home,
            content,
            can_go_back,
            structure_dirty: false,
            inspected: None,
            history: Vec::new(),
            recent: Vec::new(),
            recents: None,
            places,
            inbox,
            scan_cancellation: None,
            immediate: false,
            dispatcher: None,
            actions: None,
        };
        app.rebuild();
        app
    }

    pub(crate) fn set_result(&mut self, result: ScanResult) {
        self.show(View::Analysis(result));
    }
    pub(crate) fn recent_folders(&self) -> Vec<PathBuf> {
        self.recents
            .as_ref()
            .map_or_else(|| self.recent.clone(), |recents| recents.get().folders)
    }

    fn remember(&mut self, folder: PathBuf) {
        if let Some(recents) = &self.recents {
            if let Err(error) = recents.update(|recent| recent.remember(folder)) {
                tracing::warn!(%error, "could not remember the analyzed folder");
            }
        } else {
            let mut recent = RecentFolders {
                folders: std::mem::take(&mut self.recent),
            };
            recent.remember(folder);
            self.recent = recent.folders;
        }
    }

    fn show(&mut self, view: View) {
        self.view = view;
        self.rebuild();
    }
    fn apply(&self, request: Request) {
        let effect = match request {
            Request::Analyze(path) => analyze(&path),
            Request::Back => DiskUsageEffect::Back,
        };
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(&effect);
        }
    }
    pub(crate) fn scan(
        &mut self,
        path: PathBuf,
        push_history: bool,
        cx: &mut EffectCx<DiskUsage, Self>,
    ) -> Effected {
        self.cancel_scan();
        if push_history && let View::Analysis(current) = &self.view {
            self.history.push(current.path.clone());
        }
        let cancellation = ScanCancellation::default();
        self.scan_cancellation = Some(cancellation.clone());
        self.show(View::Loading(path.clone()));
        let worker = cancellation.clone();
        cx.thread(
            move |_| crate::scan::scan_with_cancellation(path, &worker),
            move |app: &mut Self, result: ScanResult, _cx| {
                if cancellation.is_cancelled() {
                    return Err("The scan was stopped".into());
                }
                app.scan_cancellation = None;
                if let Some(message) = result.error.clone() {
                    app.show(View::Error(result.path, message.clone()));
                    return Err(message);
                }
                app.remember(result.path.clone());
                let answer = serde_json::json!({
                    "path": result.path,
                    "bytes": result.total,
                    "entries": result.entries.len(),
                });
                app.set_result(result);
                Ok(Effected::value(answer))
            },
        )
    }
    pub(crate) fn choose_folder(cx: &mut EffectCx<DiskUsage, Self>) -> Effected {
        let picker = ui::portal::PickRequest::directory("Choose a folder to analyze", "Analyze")
            .thread_name("disk-usage-folder-picker")
            .send();
        cx.thread(
            move |_| {
                picker.recv().unwrap_or_else(|_| {
                    ui::portal::PortalOutcome::Failed("The folder picker stopped responding".into())
                })
            },
            |app: &mut Self, outcome, cx| match outcome {
                ui::portal::PortalOutcome::Opened(path) => Ok(app.scan(path, false, cx)),
                ui::portal::PortalOutcome::Cancelled => Ok(Effected::done()),
                ui::portal::PortalOutcome::Failed(message) => {
                    app.show(View::Error(PathBuf::new(), message.clone()));
                    Err(message)
                }
            },
        )
    }
    fn cancel_scan(&mut self) {
        if let Some(cancellation) = self.scan_cancellation.take() {
            cancellation.cancel();
        }
    }
    pub(crate) fn go_home(&mut self) {
        self.cancel_scan();
        self.history.clear();
        self.show(View::Home);
    }
    pub(crate) fn go_back(&mut self, cx: &mut EffectCx<DiskUsage, Self>) -> Effected {
        self.cancel_scan();
        if let Some(path) = self.history.pop() {
            self.scan(path, false, cx)
        } else {
            self.show(View::Home);
            Effected::done()
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
        let (title, view) = match &self.view {
            View::Home => (String::new(), self.home_view()),
            View::Loading(path) => (folder_title(path), Self::loading_view()),
            View::Analysis(result) => (folder_title(&result.path), self.analysis_view(result)),
            View::Error(path, message) => (folder_title(path), self.error_view(path, message)),
        };
        let back = !matches!(self.view, View::Home);
        self.can_go_back.set(back);
        self.content.set_can_go_back(back);
        self.content.set_content(view);
        self.content.set_content_title(title);
        self.structure_dirty = true;
    }

    fn bind(&self, button: Button, effect: &DiskUsageEffect) -> Button {
        match self
            .actions
            .as_ref()
            .and_then(|actions| actions.effect(effect))
        {
            Some(binding) => button.on_action(binding),
            None => button.with_enabled(false),
        }
    }

    fn bind_folder(&self, row: ListRow, path: &std::path::Path) -> ListRow {
        match self
            .actions
            .as_ref()
            .and_then(|actions| actions.effect(&analyze(path)))
        {
            Some(binding) => row.on_action(binding),
            None => row.with_enabled(false),
        }
    }

    fn place_row(&self, place: &Place) -> Element {
        Box::new(
            self.bind_folder(
                ListRow::new(
                    format!("disk-usage.place:{}", place.path.display()),
                    &place.label,
                )
                .icon(Icon::Folder)
                .secondary(place.path.to_string_lossy())
                .disclosure(),
                &place.path,
            ),
        )
    }

    fn home_view(&self) -> Element {
        let (places, recent) = self.locations();
        let choose = self.bind(
            Button::new("disk-usage.choose", "Choose folder").with_icon(Icon::FolderOpen),
            &CHOOSE,
        );
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

    fn loading_view() -> Element {
        // The empty state's own spinner keeps itself moving.
        Box::new(EmptyState::new("Calculating folder sizes").busy())
    }

    fn error_view(&self, path: &std::path::Path, message: &str) -> Element {
        let title = if path.as_os_str().is_empty() {
            "Couldn’t choose a folder"
        } else {
            "Can’t read this folder"
        };
        let mut recover = EmptyState::error(title, message)
            .with_icon(Icon::FolderOpen)
            .with_action("Choose another folder")
            .with_action_id("disk-usage.error.choose");
        recover = match self
            .actions
            .as_ref()
            .and_then(|actions| actions.effect(&DiskUsageEffect::Choose))
        {
            Some(binding) => recover.on_action(binding),
            None => recover.with_enabled(false),
        };
        Box::new(recover)
    }

    fn folder_row(&self, row: &SliceRow) -> Element {
        let list_row = ListRow::new(format!("disk-usage.folder-row:{}", row.id), &row.label)
            .icon(Icon::Folder)
            .secondary(files_text(row.files))
            .meta(bytes_si(row.bytes));
        if !row.interactive {
            return Box::new(list_row);
        }
        Box::new(self.bind_folder(list_row.disclosure(), std::path::Path::new(&row.id)))
    }

    fn analysis_view(&self, result: &ScanResult) -> Element {
        let rows = slice_rows(&result.entries);
        let folders = Section::titled(
            "Folders",
            rows.iter().map(|row| self.folder_row(row)).collect(),
        );
        let chart = SliceChart::new(
            result,
            rows,
            self.inspected.as_deref(),
            Rc::clone(&self.inbox),
        );
        let mut page = Column::new()
            .with_intrinsic_height()
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .readable()
                    .centered()
                    .with_child(Box::new(chart)),
            ))
            .push(Box::new(folders));
        if result.unreadable > 0 {
            page = page.push(Box::new(
                Text::new(&unreadable_text(result.unreadable))
                    .with_role(TextRole::Caption)
                    .numeric(),
            ));
        }
        Box::new(Scrollable::new_vertical().with_child(Box::new(page)))
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

impl ui::widget::Decorator for DiskUsageApp {
    type Inner = NavShell;

    fn inner(&self) -> &NavShell {
        &self.content
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.content
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let recent = self.recent_folders();
        if recent != self.recent {
            self.recent = recent;
            self.refresh_home();
        }
        self.content.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let mut changed = self.content.update(state, path);
        let requests = std::mem::take(&mut *self.inbox.borrow_mut());
        for request in requests {
            self.apply(request);
            changed = true;
        }
        changed
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty) | self.content.take_structure_dirty()
    }

    fn name(&self) -> &'static str {
        "DiskUsageApp"
    }

    fn needs_animation_frame(&self) -> bool {
        matches!(self.view, View::Loading(_)) || self.content.needs_animation_frame()
    }
}

impl DiskUsageApp {
    pub(crate) fn inspect(&mut self, id: &str) {
        self.inspected = Some(id.into());
        if matches!(self.view, View::Analysis(_)) {
            self.rebuild();
        }
    }
    pub(crate) fn in_analysis(&self) -> bool {
        matches!(self.view, View::Analysis(_))
    }
    pub(crate) fn folder_rows(&self) -> Vec<LiveRow> {
        let mut rows: Vec<LiveRow> = self
            .places
            .iter()
            .map(|place| folder_row(&place.label, &place.path))
            .collect();
        rows.extend(self.recent_folders().iter().map(|path| recent_row(path)));
        if let View::Analysis(result) = &self.view {
            rows.extend(
                result
                    .entries
                    .iter()
                    .filter(|entry| entry.path.file_name().is_some_and(|name| name != "."))
                    .map(|entry| folder_row(&entry.name, &entry.path)),
            );
        }
        rows
    }
}

#[cfg(test)]
mod view_tests {
    use super::*;
    /// Counts read with thousands separators and the chart names at most a
    /// handful of folders.
    #[test]
    fn counts_are_grouped_and_the_chart_groups_small_folders() {
        assert_eq!(files_text(1), "1 file");
        assert_eq!(files_text(84_200), "84,200 files");
        assert_eq!(unreadable_text(1), "1 item couldn’t be read");
        assert_eq!(unreadable_text(1_204), "1,204 items couldn’t be read");

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
}

impl DiskUsageApp {
    pub(crate) fn refresh_home(&mut self) {
        if matches!(self.view, View::Home) {
            self.rebuild();
        }
    }
}

#[cfg(test)]
mod navigation_tests {
    use super::*;
    #[test]
    fn home_view_idles_and_keeps_common_places_visible() {
        assert!(DiskUsageApp::new().next_wake_deadline().is_none());
        let mut app = DiskUsageApp::new();
        app.recent.extend([
            PathBuf::from("/home/you/Projects/tilo"),
            PathBuf::from(tilo_paths::WORKSPACE_MOUNT).join("design-system"),
            PathBuf::from("/one"),
            PathBuf::from("/two"),
            PathBuf::from("/three"),
            PathBuf::from("/four"),
        ]);
        let (places, recent) = app.locations();
        assert!(places.iter().any(|place| place.label == "File system"));
        assert_eq!(recent.len(), 5);
        assert_eq!(recent[0].label, "tilo");
        app.recent.insert(0, PathBuf::from("/"));
        let (_, recent) = app.locations();
        assert!(recent.iter().all(|place| place.path.as_os_str() != "/"));
    }
    #[test]
    fn narrow_home_keeps_common_places_visible() {
        let mut app =
            ui::prelude::TestApp::<crate::resource::DiskUsageShell>::capture(Some("home-narrow"))
                .unwrap();
        let frame = app.harness().inspection_json().unwrap();
        for name in ["Home", "Workspace", "File system", "Choose folder"] {
            assert!(frame.contains(name), "missing {name}: {frame}");
        }
    }
}
