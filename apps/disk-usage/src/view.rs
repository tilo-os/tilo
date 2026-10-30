//! The window: the places to analyze, a folder's analysis as a donut and
//! the list of its folders, or why a folder could not be read. Scanning and
//! moving between them are the resource's effects; what the window shows,
//! the way back and the analysis last finished are the view's own state.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use ui::LocalState;
use ui::framework::{Actions, Dispatcher, EffectCx, Effected, LiveRow, Publishes, RunMode, ViewCx};
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::vello::Scene;
use ui::vello::kurbo::Affine;
use ui::widget::{AppModel, ModelPath};
use ui::widget::{
    Column, Container, Element, EmptyState, ListRow, NavShell, PieChart, PieSlice, PieStyle,
    Scrollable, Section, Text, Widget,
};
use ui::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarLocation};

use crate::resource::{
    DiskUsage, DiskUsageEffect, Folders, RecentFolders, analyze, folder_row, recent_row,
};
use crate::scan::{Entry, ScanCancellation, ScanResult};
use ui::format::{count, size};
use ui::framework::PersonRecord;
use ui::tilo_resource::AbsolutePath;

/// The app, as the surfaces name it when it asks for attention.
pub(crate) const OWNER_NAME: &str = "Disk Usage Analyzer";
const OWNER_APP: &str = "so.tilo.disk-usage";

fn owner() -> ui::attention::Owner {
    ui::attention::Owner::new(OWNER_APP, OWNER_NAME).with_icon(Icon::HardDrive)
}

/// A measurement under way, for a capture: the band says what it is
/// doing and offers Stop, exactly as a live one does.
pub(crate) fn sample_job(folder: &str) {
    if ui::jobs::is_active() {
        return;
    }
    let job = ui::jobs::start(
        ui::jobs::Spec::new(format!("Measuring {folder}"))
            .subject(ui::jobs::Subject::new(owner(), folder.to_owned()))
            .unit(ui::jobs::Unit::Bytes)
            .phase("Reading")
            .stoppable(|| {}),
    );
    job.seen(41_200_000_000);
}

/// How a scan reports the bytes it has seen to its job: the registry takes
/// the count, and the effect's own progress wakes the app to draw it.
fn reporter(job: &ui::jobs::Running) -> impl Fn(u64, u64) + Send + Sync + 'static {
    let job = job.clone();
    move |bytes, _files| job.seen(bytes)
}

/// How many folders the chart names before it groups the rest.
const NAMED_SLICES: usize = 8;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Place {
    label: String,
    path: PathBuf,
}

/// What the window shows.
#[derive(Clone, Debug, PartialEq)]
enum Page {
    Home,
    /// What wants a person, opened from the window's bar.
    Needs,
    /// A finished scan, with the slice a capture points at.
    Analysis {
        result: ScanResult,
        inspected: Option<String>,
    },
    Error(PathBuf, String),
}

/// The window bar's action that chooses a folder to analyze.
pub(crate) const CHOOSE_FOLDER: &str = "disk-usage.choose";

/// The window bar's action that opens Disks.
pub(crate) const OPEN_DISKS: &str = "disk-usage.disks";

/// The window bar's action that opens what wants a person.
pub(crate) const NEEDS_YOU: &str = "disk-usage.needs";

/// A folder as the header row names it: a place by its name (Home, not
/// the path to it), any other folder by its own name.
fn folder_title(places: &[Place], folder: &Path) -> String {
    if let Some(place) = places.iter().find(|place| place.path == folder) {
        return place.label.clone();
    }
    folder.file_name().map_or_else(
        || folder.to_string_lossy().into_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The places a capture lists: the sample's home and workspace, and the
/// file system.
fn sample_places() -> Vec<Place> {
    places(
        PathBuf::from("/home/you"),
        PathBuf::from(tilo_paths::WORKSPACE_MOUNT),
    )
}

/// This device's places: its home and workspace, and the file system.
fn device_places() -> Vec<Place> {
    let home = tilo_paths::home().unwrap_or_else(|| PathBuf::from("/"));
    let workspace = std::env::var_os("TILO_WORKSPACE_ROOTS")
        .and_then(|s| s.to_string_lossy().split(':').next().map(PathBuf::from))
        .or_else(tilo_paths::workspace_root)
        .unwrap_or_else(|| PathBuf::from(tilo_paths::WORKSPACE_MOUNT));
    places(home, workspace)
}

fn places(home: PathBuf, workspace: PathBuf) -> Vec<Place> {
    vec![
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
    ]
}

/// The recent folders the home view offers beside `places`: a recent
/// folder that is also a place is listed once, as the place.
fn recent_places(places: &[Place], recent: &[PathBuf]) -> Vec<Place> {
    recent
        .iter()
        .filter(|path| places.iter().all(|place| &place.path != *path))
        .take(5)
        .map(|path| Place {
            label: path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("File system")
                .into(),
            path: path.clone(),
        })
        .collect()
}

/// The window's navigation: what it shows, the way back, and the folders it
/// remembers.
#[derive(Clone)]
struct Nav {
    page: LocalState<Page>,
    /// The folders the analyses on screen were opened from, oldest first.
    history: Rc<RefCell<Vec<PathBuf>>>,
    /// The analysis that finished last, so a job's "Show me" returns to it
    /// instead of scanning again.
    finished: Rc<RefCell<Option<ScanResult>>>,
    /// The folders analyzed lately, by device: the record in a live
    /// window, the scenario's in a capture.
    recent: LocalState<Rc<RecentFolders>>,
    /// The record and this device's id, in a live window.
    recents: Option<(PersonRecord<RecentFolders>, String)>,
    /// The device whose folders are shown.
    device: Rc<str>,
    places: Rc<Vec<Place>>,
    /// What the person presses on the waiting stack.
    needs: ui::attention::Presses,
    dispatcher: Dispatcher<DiskUsage>,
}

impl Nav {
    fn new(cx: &ViewCx<DiskUsage>) -> Self {
        let device: Rc<str> = cx.device().unwrap_or_default().into();
        let shown = |folders: Folders| {
            Rc::new(RecentFolders {
                devices: [(device.to_string(), folders)].into_iter().collect(),
            })
        };
        let (places, recents, recent) = match cx.mode() {
            RunMode::Capture => (
                sample_places(),
                None,
                shown(
                    cx.fixture::<Folders>()
                        .map(|recent| (*recent).clone())
                        .unwrap_or_default(),
                ),
            ),
            RunMode::Live => {
                let recents = cx
                    .person::<RecentFolders>()
                    .zip(cx.device().map(str::to_owned));
                (device_places(), recents, shown(Folders::default()))
            }
            RunMode::Past => (device_places(), None, shown(Folders::default())),
        };
        let recent = recents
            .as_ref()
            .map_or_else(|| LocalState::new(recent), |(record, _)| record.state());
        let page = match cx.fixture::<ScanResult>() {
            Some(scan) => {
                if cx.mode() == RunMode::Capture {
                    sample_job("Home");
                }
                Page::Analysis {
                    result: (*scan).clone(),
                    inspected: None,
                }
            }
            None => Page::Home,
        };
        Self {
            page: cx.local(page),
            history: Rc::default(),
            finished: Rc::default(),
            recent,
            recents,
            device,
            places: Rc::new(places),
            needs: ui::attention::Presses::new(),
            dispatcher: cx.dispatcher(),
        }
    }

    fn show(&self, page: Page) {
        self.page.set(page);
    }

    fn analysis(&self) -> Option<ScanResult> {
        self.page.with(|page| match page {
            Page::Analysis { result, .. } => Some(result.clone()),
            _ => None,
        })
    }

    fn title(&self) -> String {
        self.page.with(|page| match page {
            Page::Home => "Analyze storage".to_owned(),
            Page::Needs => "Needs you".to_owned(),
            Page::Analysis { result, .. } => folder_title(&self.places, &result.path),
            Page::Error(path, _) => folder_title(&self.places, path),
        })
    }

    fn remember(&self, folder: PathBuf) {
        if let Some((record, device)) = &self.recents {
            let Ok(folder) = AbsolutePath::new(folder) else {
                return;
            };
            let remember = RecentFolders::REMEMBER.with((device.clone(), folder));
            if let Err(error) = record.invoke(&remember) {
                tracing::warn!(%error, "could not remember the analyzed folder");
            }
        } else {
            let device = self.device.to_string();
            self.recent.update(|recent| {
                Rc::make_mut(recent)
                    .devices
                    .entry(device)
                    .or_default()
                    .remember(folder);
            });
        }
    }

    /// Measure `path` as a job: the people's window keeps working, the band
    /// says what it has seen and offers Stop, and the job survives this page
    /// being left.
    fn scan(&self, path: PathBuf, push_history: bool, cx: &mut EffectCx<DiskUsage>) -> Effected {
        if push_history && let Some(current) = self.analysis() {
            self.history.borrow_mut().push(current.path);
        }
        let cancellation = ScanCancellation::default();
        let title = folder_title(&self.places, &path);
        let job = ui::jobs::start(
            ui::jobs::Spec::new(format!("Measuring {title}"))
                .subject(
                    ui::jobs::Subject::new(owner(), title.clone())
                        .opens(path.to_string_lossy().into_owned()),
                )
                .unit(ui::jobs::Unit::Bytes)
                .phase("Reading")
                .stoppable({
                    let cancellation = cancellation.clone();
                    move || cancellation.cancel()
                }),
        );
        let worker = cancellation;
        let reporting = reporter(&job);
        let nav = self.clone();
        cx.thread(
            move |progress| {
                crate::scan::scan(path, &worker, &|bytes, files| {
                    reporting(bytes, files);
                    progress.report("Reading", Some(files), None);
                })
            },
            move |(): &mut (), result: ScanResult, _cx| {
                if job.is_stopped() {
                    job.stopped();
                    ui::attention::publish(ui::attention::Notice::did(
                        owner(),
                        format!("Stopped measuring {title}"),
                    ));
                    return Ok(Effected::done());
                }
                if let Some(message) = result.error.clone() {
                    job.failed(message.clone());
                    nav.show(Page::Error(result.path, message.clone()));
                    return Err(message);
                }
                nav.remember(result.path.clone());
                let answer = serde_json::json!({
                    "path": result.path,
                    "bytes": result.total,
                    "entries": result.entries.len(),
                });
                job.done(format!("Measured {}", size(result.total)));
                *nav.finished.borrow_mut() = Some(result.clone());
                nav.show(Page::Analysis {
                    result,
                    inspected: None,
                });
                Ok(Effected::value(answer))
            },
        )
    }

    fn choose_folder(&self, cx: &mut EffectCx<DiskUsage>) -> Effected {
        let picker = ui::portal::PickRequest::directory("Choose a folder to analyze", "Analyze")
            .thread_name("disk-usage-folder-picker")
            .send();
        let nav = self.clone();
        cx.thread(
            move |_| {
                picker.recv().unwrap_or_else(|_| {
                    ui::portal::PortalOutcome::Failed("The folder picker stopped responding".into())
                })
            },
            move |(): &mut (), outcome, cx| match outcome {
                ui::portal::PortalOutcome::Opened(path) => Ok(nav.scan(path, false, cx)),
                ui::portal::PortalOutcome::Cancelled => Ok(Effected::done()),
                ui::portal::PortalOutcome::Failed(message) => {
                    nav.show(Page::Error(PathBuf::new(), message.clone()));
                    Err(message)
                }
            },
        )
    }

    fn open_disks(cx: &mut EffectCx<DiskUsage>) -> Effected {
        cx.thread(
            move |_| ui::portal::open_uri(crate::resource::DISKS_LINK).map_err(|e| e.to_string()),
            move |(): &mut (), opened: Result<(), String>, _| opened.map(|()| Effected::done()),
        )
    }

    fn go_back(&self, cx: &mut EffectCx<DiskUsage>) -> Effected {
        let previous = self.history.borrow_mut().pop();
        if let Some(path) = previous {
            self.scan(path, false, cx)
        } else {
            self.show(Page::Home);
            Effected::done()
        }
    }

    fn go_home(&self) {
        self.history.borrow_mut().clear();
        self.show(Page::Home);
    }

    /// Go to the folder a job was working on: its analysis if it is still
    /// here, else measuring it again.
    fn show_folder(&self, target: &str) {
        let path = PathBuf::from(target);
        let finished = self
            .finished
            .borrow()
            .clone()
            .filter(|finished| finished.path == path);
        match finished {
            Some(result) => self.show(Page::Analysis {
                result,
                inspected: None,
            }),
            None => {
                if let Some(effect) = analyze(&path) {
                    self.dispatcher.effect(&effect);
                }
            }
        }
    }

    /// Point at the slice of `id` in the analysis on screen.
    fn inspect(&self, id: &str) {
        self.page.update(|page| {
            if let Page::Analysis { inspected, .. } = page {
                *inspected = Some(id.into());
            }
        });
    }

    /// The common places, the folders analyzed lately, and the folders of
    /// the scan on screen.
    fn folder_rows(&self) -> Vec<LiveRow> {
        let mut rows: Vec<LiveRow> = self
            .places
            .iter()
            .map(|place| folder_row(&place.label, &place.path))
            .collect();
        self.recent.with(|recent| {
            rows.extend(recent.on(&self.device).iter().map(|path| recent_row(path)));
        });
        self.page.with(|page| {
            if let Page::Analysis { result, .. } = page {
                rows.extend(
                    result
                        .entries
                        .iter()
                        .filter(|entry| entry.path.file_name().is_some_and(|name| name != "."))
                        .map(|entry| folder_row(&entry.name, &entry.path)),
                );
            }
        });
        rows
    }
}

/// The whole window: the navigation shell around the page on screen,
/// publishing the folders it offers as live rows.
pub(crate) fn window(cx: &ViewCx<DiskUsage>) -> Element {
    let nav = Nav::new(cx);
    answer_effects(cx, &nav);
    {
        let nav = nav.clone();
        cx.on_stage(move |scenario| {
            if scenario.is_some_and(|name| name.starts_with("analysis-hover")) {
                nav.inspect("/home/you/projects");
            }
        });
    }
    // Folder controls analyze through the device's own effect, which only a
    // live window runs.
    let live = cx.mode() == RunMode::Live;
    let actions = live.then(|| cx.actions());
    let content = {
        let nav = nav.clone();
        cx.switch(&nav.page.clone(), move |cx, page| {
            page_view(cx, &nav, page, actions.as_ref())
        })
    };
    let jobs = ui::jobs::Presses::new();
    let shell = {
        let (can, back) = (nav.page.clone(), nav.dispatcher.clone());
        let mut shell = NavShell::new()
            .content_kind(ui::widget::ContentKind::List)
            .with_id("disk-usage.shell")
            .with_window_toolbar()
            // What runs, across the window's foot: it stays whatever page
            // the window shows, so a measurement outlives the page that
            // started it.
            .bottom_band(Box::new(ui::jobs::Band::with_presses(jobs.clone())))
            .on_back(
                move || can.with(|page| *page != Page::Home),
                move || back.effect(&DiskUsageEffect::Back),
            );
        shell.set_content(content);
        shell
    };
    let rows = nav.clone();
    cx.publishing(
        Publishes::new().live_rows(move || rows.folder_rows()),
        Box::new(Window {
            shell,
            nav,
            jobs,
            live,
        }),
    )
}

/// Answer the scans and navigation, whoever asks: a row, the bar, the
/// palette, an agent.
fn answer_effects(cx: &ViewCx<DiskUsage>, nav: &Nav) {
    let nav = nav.clone();
    cx.effects(move |effect: DiskUsageEffect, cx| match effect {
        DiskUsageEffect::Analyze { path } => {
            let push_history = nav.analysis().is_some();
            Ok(nav.scan(path.into_path_buf(), push_history, cx))
        }
        DiskUsageEffect::Choose => Ok(nav.choose_folder(cx)),
        DiskUsageEffect::Back => Ok(nav.go_back(cx)),
        DiskUsageEffect::OpenDisks => Ok(Nav::open_disks(cx)),
        DiskUsageEffect::Home => {
            nav.go_home();
            Ok(Effected::done())
        }
    });
}

/// The page for `page`, built again whenever it changes.
fn page_view(
    cx: &ViewCx<DiskUsage>,
    nav: &Nav,
    page: &Page,
    actions: Option<&Actions<DiskUsage>>,
) -> Element {
    let pages = Pages {
        actions: actions.cloned(),
    };
    match page {
        Page::Home => {
            let places = Rc::clone(&nav.places);
            let device = Rc::clone(&nav.device);
            cx.switch(&nav.recent, move |_, recent| {
                pages.home(&places, &recent_places(&places, &recent.on(&device)))
            })
        }
        Page::Needs => Box::new(ui::attention::Stack::with_presses(nav.needs.clone())),
        Page::Analysis { result, inspected } => {
            let dispatcher = nav.dispatcher.clone();
            let analyze: Rc<dyn Fn(PathBuf)> = Rc::new(move |path: PathBuf| {
                if let Some(effect) = analyze(&path) {
                    dispatcher.effect(&effect);
                }
            });
            pages.analysis(result, inspected.as_deref(), analyze)
        }
        Page::Error(path, message) => pages.error(path, message),
    }
}

/// Builds the pages, binding their folder controls to the analyze effect
/// while the window can run it.
struct Pages {
    actions: Option<Actions<DiskUsage>>,
}

impl Pages {
    fn bind_folder(&self, row: ListRow, path: &Path) -> ListRow {
        match self
            .actions
            .as_ref()
            .and_then(|actions| actions.effect(&analyze(path)?))
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

    fn home(&self, places: &[Place], recent: &[Place]) -> Element {
        let mut page = Column::new()
            .with_intrinsic_height()
            .push(Box::new(Section::titled(
                "Places",
                places.iter().map(|p| self.place_row(p)).collect(),
            )));
        if !recent.is_empty() {
            page = page.push(Box::new(Section::titled(
                "Recent",
                recent.iter().map(|p| self.place_row(p)).collect(),
            )));
        }
        Box::new(Scrollable::new_vertical().with_child(Box::new(page)))
    }

    fn error(&self, path: &Path, message: &str) -> Element {
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
            .meta(size(row.bytes));
        if !row.interactive {
            return Box::new(list_row);
        }
        Box::new(self.bind_folder(list_row.disclosure(), Path::new(&row.id)))
    }

    fn analysis(
        &self,
        result: &ScanResult,
        inspected: Option<&str>,
        analyze: Rc<dyn Fn(PathBuf)>,
    ) -> Element {
        let rows = slice_rows(&result.entries);
        let folders = Section::titled(
            "Folders",
            rows.iter().map(|row| self.folder_row(row)).collect(),
        );
        let chart = SliceChart::new(result, rows, inspected, analyze);
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

/// The navigation shell, with the window's slots in its bar: the shell's
/// own (Back while there is a way back), the place on screen as the
/// location, Choose folder, and what wants a person.
struct Window {
    shell: NavShell,
    nav: Nav,
    /// What the jobs band reports back.
    jobs: ui::jobs::Presses,
    live: bool,
}

impl Window {
    /// Run what the person did in the window's bar, then declare its slots.
    fn run_window_toolbar(&mut self) {
        for event in ui::window_toolbar::take_events() {
            if self.shell.window_toolbar_event(&event) {
                continue;
            }
            if let ToolbarEvent::Action { id } = &event {
                if id == CHOOSE_FOLDER && self.live {
                    self.nav.dispatcher.effect(&DiskUsageEffect::Choose);
                }
                if id == OPEN_DISKS && self.live {
                    self.nav.dispatcher.effect(&DiskUsageEffect::OpenDisks);
                }
                if id == NEEDS_YOU {
                    self.nav.show(Page::Needs);
                }
            }
        }
        let mut toolbar = self.shell.window_toolbar();
        toolbar.location = Some(ToolbarLocation {
            label: self.nav.title(),
            ..ToolbarLocation::default()
        });
        let mut actions = vec![
            ToolbarAction::new(CHOOSE_FOLDER, Icon::FolderOpen.name(), "Choose folder")
                .enabled(self.live),
            ToolbarAction::new(OPEN_DISKS, Icon::HardDrive.name(), "Disks")
                .enabled(self.live)
                .in_menu(),
        ];
        if let Some(label) = ui::attention::pill() {
            actions.push(ToolbarAction::new(NEEDS_YOU, Icon::Bell.name(), label).labelled());
        }
        NavShell::push_window_actions(&mut toolbar, actions);
        ui::window_toolbar::declare(Some(toolbar));
    }

    /// Carry out what the person pressed on the jobs band or the stack.
    fn answer_presses(&self) {
        for press in self.jobs.take() {
            if let Some(subject) = ui::jobs::apply(press)
                && let Some(target) = subject.target
            {
                self.nav.show_folder(&target);
            }
        }
        for press in self.nav.needs.take() {
            let Some(acted) = ui::attention::apply(press) else {
                continue;
            };
            if acted.action.verb != ui::attention::SHOW_JOB {
                continue;
            }
            let Some(ui::attention::Target::Job(number)) = acted.target else {
                continue;
            };
            let Some(subject) = ui::jobs::show(ui::jobs::JobId::from_number(number)) else {
                continue;
            };
            if let Some(target) = subject.target {
                self.nav.show_folder(&target);
            }
        }
    }
}

impl ui::widget::Decorator for Window {
    type Inner = NavShell;

    fn inner(&self) -> &NavShell {
        &self.shell
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.run_window_toolbar();
        let changed = self.shell.update(state, path);
        self.answer_presses();
        // The stack counts what nobody has looked at as soon as it is open.
        if self.nav.page.with(|page| *page == Page::Needs) {
            ui::attention::seen();
        }
        changed
    }

    fn name(&self) -> &'static str {
        "DiskUsageWindow"
    }
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
        format!("{}, {}", size(self.bytes), files_text(self.files))
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
    /// Analyzes the folder of a chosen slice.
    analyze: Rc<dyn Fn(PathBuf)>,
}

impl SliceChart {
    fn new(
        result: &ScanResult,
        rows: Vec<SliceRow>,
        inspected: Option<&str>,
        analyze: Rc<dyn Fn(PathBuf)>,
    ) -> Self {
        let chart = PieChart::new(Vec::new())
            .with_legend()
            .with_style(PieStyle {
                tooltip: false,
                ..PieStyle::default()
            });
        let mut this = Self {
            chart,
            rows,
            total: size(result.total),
            files: files_text(result.files),
            analyze,
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
            Some(row) => (size(row.bytes), row.label.clone()),
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
                    .with_legend_detail(size(row.bytes));
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
                (self.analyze)(PathBuf::from(id));
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

#[cfg(test)]
mod tests {
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

    /// The header names a place as the home view does and any other folder
    /// by its own name, never by the path to it.
    #[test]
    fn the_header_names_places_and_folders_not_paths() {
        let places = [
            Place {
                label: "Home".into(),
                path: PathBuf::from("/var/home/you"),
            },
            Place {
                label: "File system".into(),
                path: PathBuf::from("/"),
            },
        ];
        assert_eq!(folder_title(&places, Path::new("/var/home/you")), "Home");
        assert_eq!(folder_title(&places, Path::new("/")), "File system");
        assert_eq!(
            folder_title(&places, Path::new("/var/home/you/Projects")),
            "Projects"
        );
        assert_eq!(folder_title(&[], Path::new("/")), "/");
    }

    /// Home lists the common places and at most five recent folders, a
    /// recent folder that is a place only once.
    #[test]
    fn home_keeps_common_places_and_five_recent_folders() {
        let places = sample_places();
        let mut recent = vec![
            PathBuf::from("/home/you/Projects/tilo"),
            PathBuf::from(tilo_paths::WORKSPACE_MOUNT).join("design-system"),
            PathBuf::from("/one"),
            PathBuf::from("/two"),
            PathBuf::from("/three"),
            PathBuf::from("/four"),
        ];
        let listed = recent_places(&places, &recent);
        assert_eq!(listed.len(), 5);
        assert_eq!(listed[0].label, "tilo");
        recent.insert(0, PathBuf::from("/"));
        let listed = recent_places(&places, &recent);
        assert!(listed.iter().all(|place| place.path.as_os_str() != "/"));
    }

    #[test]
    fn narrow_home_keeps_common_places_visible() {
        let mut app =
            ui::prelude::TestApp::<crate::resource::DiskUsageShell>::capture(Some("home-narrow"))
                .unwrap();
        let frame = app.harness().inspection_json().unwrap();
        for name in ["Home", "Workspace", "File system"] {
            assert!(frame.contains(name), "missing {name}: {frame}");
        }
        let bar = ui::window_toolbar::declared().expect("the window's slots");
        assert!(
            bar.actions
                .iter()
                .any(|action| action.name == "Choose folder"),
            "{bar:?}"
        );
    }
}
