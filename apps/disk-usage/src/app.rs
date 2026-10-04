use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use ui::widget::{AppModel, ModelPath};

use ui::framework::{Actions, Dispatcher, EffectCx, Effected, LiveRow};
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::{TextKind, TextRenderer};
use ui::theme::Theme;
use ui::tilo_actions::ActionBinding;
use ui::tilo_resource::ActionRef;
use ui::tokens::TextRole;
use ui::vello::Scene;
use ui::vello::kurbo::Affine;
use ui::widget::{
    Button, Column, Container, Element, EmptyState, ListRow, PieChart, PieSlice, Row, Scrollable,
    Section, SectionHeader, Spinner, Text, Widget,
};

use crate::resource::{DiskUsage, DiskUsageAction, DiskUsageEffect, analyze, folder_row};
use crate::scan::{Entry, ScanCancellation, ScanResult, UNREADABLE_FOLDER};
use ui::format::bytes_si;

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
const CHOOSE: ActionRef<crate::resource::DiskUsage, ()> = ActionRef::new("choose", &[]);
const BACK: ActionRef<crate::resource::DiskUsage, ()> = ActionRef::new("back", &[]);

/// What a control in the current view asks for. Controls run inside the
/// widget tree; the app applies their requests on its next update.
enum Request {
    Analyze(PathBuf),
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

fn unreadable_text(n: u64) -> String {
    if n == 1 {
        "1 item couldn’t be read".into()
    } else {
        format!("{} items couldn’t be read", count_text(n))
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
/// whole scan otherwise, so a wedge never needs a tooltip over the legend.
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
                    .with_detail(row.detail());
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
        self.prepare(theme);
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
        self.prepare(theme);
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
    pub(crate) recent: Vec<PathBuf>,
    places: Vec<Place>,
    inbox: Inbox,
    scan_cancellation: Option<ScanCancellation>,
    pub(crate) immediate: bool,
    pub(crate) dispatcher: Option<Dispatcher<DiskUsage>>,
    published: Option<Actions<DiskUsage>>,
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
            scan_cancellation: None,
            immediate: false,
            dispatcher: None,
            published: None,
        };
        app.rebuild();
        app
    }

    pub(crate) fn set_result(&mut self, result: ScanResult) {
        self.show(View::Analysis(result));
    }

    pub(crate) fn stage_loading(&mut self) {
        self.show(View::Loading(PathBuf::from("/home/you")));
    }

    pub(crate) fn stage_error(&mut self) {
        self.show(View::Error(
            PathBuf::from("/root"),
            UNREADABLE_FOLDER.into(),
        ));
    }

    pub(crate) fn present_recent(&mut self, recent: &[PathBuf]) {
        if self.recent != recent {
            self.recent = recent.to_vec();
            if matches!(self.view, View::Home) {
                self.rebuild();
            }
        }
    }

    fn show(&mut self, view: View) {
        self.view = view;
        self.rebuild();
    }

    /// Invoke `effect` through the door every caller uses.
    fn invoke(&self, effect: &DiskUsageEffect) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(effect);
        }
    }

    /// Scan `path` in the background and show it when done, remembering the
    /// folder in the shared recents.
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
            move |app: &mut Self, result: ScanResult, cx| {
                if cancellation.is_cancelled() {
                    return Err("The scan was stopped".into());
                }
                app.scan_cancellation = None;
                if let Some(message) = result.error.clone() {
                    app.show(View::Error(result.path, message.clone()));
                    return Err(message);
                }
                cx.apply(&DiskUsageAction::Remember {
                    path: result.path.to_string_lossy().into_owned(),
                });
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

    /// Ask for a folder, then analyze it.
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

    /// The folder analyzed before this one, or the locations.
    pub(crate) fn go_back(&mut self, cx: &mut EffectCx<DiskUsage, Self>) -> Effected {
        self.cancel_scan();
        if let Some(path) = self.history.pop() {
            self.scan(path, false, cx)
        } else {
            self.show(View::Home);
            Effected::done()
        }
    }

    fn apply(&mut self, request: Request) {
        let Request::Analyze(path) = request;
        self.invoke(&analyze(&path));
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

    /// Invoke the published navigation operations through `actions`, the
    /// resource the framework hosts.
    pub(crate) fn host_actions(&mut self, actions: Actions<crate::resource::DiskUsage>) {
        self.published = Some(actions);
        self.rebuild();
    }

    /// The binding of the published `action`, or `None` when nothing
    /// publishes it.
    fn binding(
        &self,
        action: &ActionRef<crate::resource::DiskUsage, ()>,
    ) -> Option<ActionBinding<()>> {
        match &self.published {
            Some(actions) => actions.invocation(action.invoke()),
            None => None,
        }
    }

    /// `button`, bound to the published `action`; disabled when nothing
    /// publishes it.
    fn bind(&self, button: Button, action: &ActionRef<crate::resource::DiskUsage, ()>) -> Button {
        match self.binding(action) {
            Some(binding) => button.on_action(binding),
            None => button.with_enabled(false),
        }
    }

    fn back_button(&self) -> Button {
        self.bind(
            Button::icon_only("disk-usage.back", Icon::ChevronLeft, "Back"),
            &BACK,
        )
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
        let choose = self.bind(
            Button::new("disk-usage.choose", "Choose folder", None).with_icon(Icon::FolderOpen),
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

    /// The header every view but the home view opens with: Back, then the
    /// folder the view is about. One structure, so Back never moves.
    fn header(&self, folder: &std::path::Path) -> Row {
        let mut header = Row::new().no_wrap().push(Box::new(self.back_button()));
        if !folder.as_os_str().is_empty() {
            header = header.push(Box::new(
                Text::new(&folder.to_string_lossy())
                    .with_role(TextRole::Title)
                    .with_kind(TextKind::Identifier),
            ));
        }
        header
    }

    fn loading_view(&self, path: &std::path::Path) -> Element {
        let spinner = Rc::new(RefCell::new(Spinner::new()));
        let busy =
            EmptyState::new("Calculating folder sizes").with_illustration(move |ctx, slot| {
                let mut spinner = spinner.borrow_mut();
                spinner.set_rect(slot);
                spinner.draw(
                    ctx.scene,
                    ctx.transform,
                    ctx.text,
                    ctx.theme,
                    &PresenceCtx::empty(),
                );
            });
        Box::new(
            Column::new()
                .push(Box::new(self.header(path)))
                .push(Box::new(busy)),
        )
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
        if let Some(binding) = self.binding(&CHOOSE) {
            recover = recover.on_action(binding);
        }
        Box::new(
            Column::new()
                .push(Box::new(self.header(path)))
                .push(Box::new(recover)),
        )
    }

    fn folder_row(&self, row: &SliceRow) -> Element {
        let list_row = ListRow::new(format!("disk-usage.folder-row:{}", row.id), &row.label)
            .icon(Icon::Folder)
            .secondary(files_text(row.files))
            .meta(bytes_si(row.bytes));
        if !row.interactive {
            return Box::new(list_row);
        }
        let inbox = Rc::clone(&self.inbox);
        let path = PathBuf::from(&row.id);
        Box::new(
            list_row
                .disclosure()
                .on_activate(move || inbox.borrow_mut().push(Request::Analyze(path.clone()))),
        )
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
        let lane = Theme::default().tokens.metrics.panel_width_sheet;
        let mut page = Column::new()
            .with_intrinsic_height()
            .push(Box::new(self.header(&result.path)))
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_centered_max_width(lane)
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
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
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
        self.rebuild();
    }

    pub(crate) fn in_analysis(&self) -> bool {
        matches!(self.view, View::Analysis(_))
    }

    /// The common places, and the folders of the scan on screen: live
    /// folders on this machine that an agent or the palette can analyze.
    pub(crate) fn folder_rows(&self) -> Vec<LiveRow> {
        let mut rows: Vec<LiveRow> = self
            .places
            .iter()
            .map(|place| folder_row(&place.label, &place.path))
            .collect();
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
mod tests {
    use super::*;
    #[test]
    fn narrow_home_keeps_common_places_visible() {
        let mut app = DiskUsageApp::new();
        app.recent.extend([
            PathBuf::from("/home/you/Projects/tilo"),
            PathBuf::from(tilo_paths::WORKSPACE_MOUNT).join("design-system"),
            PathBuf::from("/one"),
            PathBuf::from("/two"),
            PathBuf::from("/three"),
        ]);
        let (places, recent) = app.locations();
        assert_eq!(places.len(), 3);
        assert!(places.iter().any(|place| place.label == "File system"));
        assert_eq!(recent.len(), 5);
        assert_eq!(recent[0].label, "tilo");
        app.recent.insert(0, PathBuf::from("/"));
        let (_, recent) = app.locations();
        assert!(recent.iter().all(|place| place.path.as_os_str() != "/"));
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
