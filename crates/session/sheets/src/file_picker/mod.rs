//! The file chooser a system service asks on an app's behalf: open files,
//! save one, or choose a folder.
//!
//! The service holds the request and its one answer and decides where the
//! chooser may go ([`FileRequest`]: where it starts, the folders it may
//! reach); the sheet shows the toolkit's [`FilePicker`] for it and gives the
//! answer back through the seat ([`FilePickerSeat::take_answer`]). Every
//! step a person takes in it is a published action
//! ([`FilePickerEffect`]); what the chooser lists, where it is and why it
//! last refused reaches the service as plain data
//! ([`FilePickerSeat::listing`]). Giving the app files needs the person:
//! agents may look around and cancel, but never confirm.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use ui::framework::{Effected, ResourceApp, ViewCx};
use ui::prelude::*;
use ui::widget::file_picker::semantic::{PickerOperation, PickerRefusal, entry_id};
use ui::widget::file_picker::{FileFilter, Requester};
use ui::widget::{Element, FilePicker, FilePickerMode, FilePickerOutcome, Widget};

use crate::presentation::Presentation;
use crate::t;

/// The app the session mounts the chooser as.
pub const APP_ID: &str = "so.tilo.file-chooser";

/// The sheet's resource: the door its steps are published on. It holds
/// nothing; the request is the service's.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct FileChooser {}

#[resource(kind = "file-chooser.request", label = "File chooser")]
impl FileChooser {}

/// What a person does in the chooser.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "so.tilo.file-chooser.step")]
pub enum FilePickerEffect {
    /// Open a folder: one in the list, or any the chooser may reach
    #[effect(own_device, kind = "read")]
    Navigate {
        #[param("The folder's path")]
        path: String,
    },
    /// Open the folder above the one shown
    #[effect(own_device, kind = "read")]
    Up,
    /// Choose one entry of the list, and only it
    #[effect(own_device, kind = "read")]
    Select {
        #[param("The entry's path, as listed")]
        path: String,
    },
    /// Add an entry of the list to the choice, or take it out of it
    #[effect(own_device, kind = "read")]
    AlsoSelect {
        #[param("The entry's path, as listed")]
        path: String,
    },
    /// Name the file to save
    #[effect(own_device, kind = "read")]
    SetFilename {
        #[param("The file's name")]
        name: String,
    },
    /// Show only one type of file
    #[effect(own_device, kind = "read")]
    Filter {
        #[param("The type, counted from zero in the list of types")]
        index: u32,
    },
    /// Search every place the chooser may reach; empty goes back to the folder
    #[effect(own_device, kind = "read")]
    Search {
        #[param("What to look for")]
        query: String,
    },
    /// Show what was chosen here before
    #[effect(own_device, kind = "read")]
    Recent,
    /// Give the app what is chosen
    #[effect(own_device, attended)]
    Confirm,
    /// Save over the file that already has the name
    #[effect(own_device, attended)]
    Replace,
    /// Keep the file that has the name, and choose another
    #[effect(own_device, kind = "read")]
    KeepExisting,
    /// Close the chooser and give the app nothing
    #[effect(own_device, idempotent)]
    Cancel,
}

/// What the chooser asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileMode {
    /// Existing files.
    #[default]
    Open,
    /// One file to write, new or existing.
    Save,
    /// Existing folders.
    Folder,
}

/// A type of file the app takes.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileType {
    /// What the person reads ("Images").
    pub label: String,
    /// Extensions without the dot; none takes every file.
    pub extensions: Vec<String>,
}

/// One request of the chooser: plain data the service fills in. Where the
/// chooser starts and how far it may go are the service's decision, never
/// the asking app's.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileRequest {
    /// The request; another one starts the chooser afresh.
    pub handle: String,
    /// What the chooser is for, as the sheet says it.
    pub title: String,
    /// The asking app, as the sheet names it, and its id for its icon.
    pub app_name: Option<String>,
    pub app_id: Option<String>,
    pub mode: FileMode,
    /// More than one entry may be chosen.
    pub multiple: bool,
    /// What the confirm button says, when not Open, Save or Choose folder.
    pub accept_label: Option<String>,
    /// The types the app takes; the first is shown first.
    pub filters: Vec<FileType>,
    /// Where the chooser opens.
    pub initial_dir: PathBuf,
    /// Every folder the chooser may reach, and nothing outside them.
    pub roots: Vec<PathBuf>,
    /// The person's home, which offers it and its usual folders.
    pub home: Option<PathBuf>,
    /// The name a Save starts with.
    pub suggested_name: Option<String>,
    /// What this app was given before, newest first.
    pub recent: Vec<PathBuf>,
}

/// The chooser's one answer.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileAnswer {
    /// What the person chose: one path unless the request allows several.
    Chosen(Vec<PathBuf>),
    Cancelled,
}

/// What the list shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Showing {
    #[default]
    Folder,
    Recent,
    Search,
}

/// One entry of the list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub directory: bool,
    pub selected: bool,
}

/// What the chooser shows now, as plain data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    /// The folder shown.
    pub folder: PathBuf,
    pub showing: Showing,
    pub entries: Vec<Entry>,
    /// The name a Save would write.
    pub filename: String,
    /// The types the list may show, and the one it shows.
    pub filters: Vec<String>,
    pub filter: usize,
    /// Confirming would answer now.
    pub can_confirm: bool,
    pub can_up: bool,
    /// An existing file a Save waits to replace.
    pub replace: Option<PathBuf>,
    /// The chooser is still reading the folder (or checking a path).
    pub reading: bool,
    /// Why the last step was refused.
    pub refused: Option<String>,
}

impl Listing {
    fn entry(&self, path: &Path) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.path == path)
    }
}

/// A step waiting for the chooser to take it.
#[derive(Clone, Debug)]
enum Step {
    /// A folder the chooser may reach, though not in the list.
    Go(PathBuf),
    Picker(PickerOperation),
}

#[derive(Default)]
struct Held {
    request: Option<FileRequest>,
    listing: Listing,
    steps: VecDeque<Step>,
    answer: Option<FileAnswer>,
    /// Starts the chooser afresh when the request changes.
    opened: Option<LocalState<Option<FileRequest>>>,
}

/// What the session lends the chooser: whether it is up, what it is asked,
/// what it lists and what it answered.
#[derive(Clone, Default)]
pub struct FilePickerSeat {
    presentation: Presentation,
    held: Rc<RefCell<Held>>,
}

impl std::fmt::Debug for FilePickerSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilePickerSeat")
            .field("presentation", &self.presentation)
            .field("request", &self.held.borrow().request)
            .finish_non_exhaustive()
    }
}

impl crate::Seat for FilePickerSeat {
    fn presentation(&self) -> &Presentation {
        &self.presentation
    }
}

impl FilePickerSeat {
    /// Ask `request`, or nothing. Another request starts the chooser
    /// afresh and forgets the last one's answer.
    pub fn ask(&self, request: Option<FileRequest>) {
        let opened = {
            let mut held = self.held.borrow_mut();
            if held.request == request {
                return;
            }
            held.request.clone_from(&request);
            held.answer = None;
            held.steps.clear();
            held.listing = Listing::default();
            held.opened.clone()
        };
        if let Some(opened) = opened {
            opened.set(request);
        }
        self.presentation.wake();
    }

    /// What the chooser is asked now.
    pub fn request(&self) -> Option<FileRequest> {
        self.held.borrow().request.clone()
    }

    /// What the chooser shows now.
    pub fn listing(&self) -> Listing {
        self.held.borrow().listing.clone()
    }

    /// The chooser's answer, once the person gave it: taken once.
    pub fn take_answer(&self) -> Option<FileAnswer> {
        self.held.borrow_mut().answer.take()
    }

    /// Whether `other` is this same seat, lent again.
    pub fn is(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.held, &other.held)
    }

    fn queue(&self, step: Step) {
        self.held.borrow_mut().steps.push_back(step);
        self.presentation.wake();
    }

    fn take_steps(&self) -> VecDeque<Step> {
        std::mem::take(&mut self.held.borrow_mut().steps)
    }

    fn answered(&self, outcome: FilePickerOutcome) {
        let multiple = self
            .held
            .borrow()
            .request
            .as_ref()
            .is_some_and(|request| request.multiple);
        let answer = match outcome {
            FilePickerOutcome::Selected(mut paths) => {
                if !multiple {
                    paths.truncate(1);
                }
                FileAnswer::Chosen(paths)
            }
            FilePickerOutcome::Cancelled => FileAnswer::Cancelled,
        };
        self.held.borrow_mut().answer = Some(answer);
        self.presentation.dismissed();
    }

    /// Take the chooser's step `effect`: what can be refused now is.
    fn step(&self, effect: FilePickerEffect) -> Result<(), String> {
        let (listing, roots) = {
            let held = self.held.borrow();
            let Some(request) = held.request.as_ref() else {
                return Err(t("file-chooser-nothing-asked"));
            };
            if held.answer.is_some() {
                return Err(t("file-chooser-answered"));
            }
            (held.listing.clone(), request.roots.clone())
        };
        let listed = |path: &str| {
            listing
                .entry(Path::new(path))
                .map(|entry| entry_id(&entry.path))
                .ok_or_else(|| t("file-chooser-not-listed"))
        };
        let step = match effect {
            FilePickerEffect::Navigate { path } => match listing.entry(Path::new(&path)) {
                Some(entry) if entry.directory => Step::Picker(PickerOperation::Navigate {
                    entry: entry_id(&entry.path),
                }),
                Some(_) => return Err(t("file-chooser-not-a-folder")),
                None => {
                    let path = PathBuf::from(path);
                    if !path.is_absolute() || !roots.iter().any(|root| path.starts_with(root)) {
                        return Err(t("file-chooser-outside"));
                    }
                    Step::Go(path)
                }
            },
            FilePickerEffect::Up => Step::Picker(PickerOperation::Up),
            FilePickerEffect::Select { path } => Step::Picker(PickerOperation::Select {
                entry: listed(&path)?,
            }),
            FilePickerEffect::AlsoSelect { path } => Step::Picker(PickerOperation::AlsoSelect {
                entry: listed(&path)?,
            }),
            FilePickerEffect::SetFilename { name } => {
                Step::Picker(PickerOperation::SetFilename { filename: name })
            }
            FilePickerEffect::Filter { index } => Step::Picker(PickerOperation::Filter {
                index: usize::try_from(index).unwrap_or(usize::MAX),
            }),
            FilePickerEffect::Search { query } => Step::Picker(PickerOperation::Search { query }),
            FilePickerEffect::Recent => Step::Picker(PickerOperation::Recent),
            FilePickerEffect::Confirm => Step::Picker(PickerOperation::Confirm),
            FilePickerEffect::Replace => Step::Picker(PickerOperation::Replace),
            FilePickerEffect::KeepExisting => Step::Picker(PickerOperation::KeepExisting),
            FilePickerEffect::Cancel => Step::Picker(PickerOperation::Cancel),
        };
        self.queue(step);
        Ok(())
    }
}

/// Why the chooser refused a step, in the person's words.
fn refusal(refusal: PickerRefusal) -> String {
    t(match refusal {
        PickerRefusal::Stale => "file-chooser-changed",
        PickerRefusal::InvalidPage | PickerRefusal::UnavailableEntry => "file-chooser-not-listed",
        PickerRefusal::OutsideScope => "file-chooser-outside",
        PickerRefusal::Unavailable => "file-chooser-unavailable",
    })
}

/// The chooser for `request`, as the toolkit draws it.
fn picker(request: &FileRequest, wake: Option<std::task::Waker>) -> FilePicker {
    let mode = match request.mode {
        FileMode::Open => FilePickerMode::Open,
        FileMode::Save => FilePickerMode::Save,
        FileMode::Folder => FilePickerMode::Directory,
    };
    let filters = request
        .filters
        .iter()
        .map(|filter| {
            let extensions: Vec<&str> = filter.extensions.iter().map(String::as_str).collect();
            FileFilter::new(filter.label.clone(), &extensions)
        })
        .collect();
    let requester = request.app_name.as_ref().map(|name| Requester {
        name: name.clone(),
        glyph: ui::icons::app_glyph(request.app_id.as_deref().unwrap_or_default(), None),
        image: None,
    });
    let mut picker = FilePicker::new(
        request.title.clone(),
        &request.initial_dir,
        request.roots.clone(),
    )
    .with_waker(wake)
    .with_mode(mode)
    .with_multiple(request.multiple)
    .with_accept_label(request.accept_label.clone())
    .with_home(request.home.clone())
    .with_filters(filters)
    .with_requester(requester)
    .with_recent(request.recent.clone());
    if let Some(name) = &request.suggested_name {
        picker = picker.with_filename(name.clone());
    }
    // The sheet is modal: it holds the keyboard from the moment it is up.
    picker.set_focus(true);
    picker
}

/// What the chooser shows, read from the toolkit's picker.
fn listing_of(picker: &FilePicker, refused: Option<String>) -> Listing {
    let selected = picker.selected_indices();
    let inspected = picker.semantic_inspection(0).ok();
    let showing = match inspected
        .as_ref()
        .and_then(|value| value.get("listing"))
        .and_then(|value| value.as_str())
    {
        Some("recent") => Showing::Recent,
        Some("search") => Showing::Search,
        _ => Showing::Folder,
    };
    let filters = picker.filter_labels();
    let filter = filters
        .iter()
        .position(|label| label == picker.active_filter_label())
        .unwrap_or_default();
    Listing {
        folder: picker.current_dir().to_path_buf(),
        showing,
        entries: picker
            .entries()
            .iter()
            .enumerate()
            .map(|(at, entry)| Entry {
                name: entry.name.clone(),
                path: entry.path.clone(),
                directory: entry.is_dir,
                selected: selected.contains(&at),
            })
            .collect(),
        filename: picker.filename().to_owned(),
        filters,
        filter,
        can_confirm: picker.can_confirm(),
        can_up: picker.can_go_up(),
        replace: picker.replace_prompt().map(Path::to_path_buf),
        reading: picker.reading(),
        refused,
    }
}

/// The toolkit's picker, taking the steps asked through the door and
/// handing what it answers to the seat.
struct Chooser {
    picker: FilePicker,
    seat: FilePickerSeat,
}

impl Chooser {
    /// Take the steps waiting, take what the worker read, and tell the
    /// seat what the chooser shows and answered. `true` when anything
    /// changed.
    fn serve(&mut self) -> bool {
        let mut changed = self.picker.poll_lookups();
        let mut refused = None;
        let steps = self.seat.take_steps();
        let stepped = !steps.is_empty();
        for step in steps {
            changed = true;
            match step {
                Step::Go(path) => self.picker.navigate_to(path),
                Step::Picker(operation) => {
                    let state = self
                        .picker
                        .semantic_inspection(0)
                        .ok()
                        .and_then(|value| value.get("state")?.as_str().map(str::to_owned));
                    if let Err(why) = self.picker.perform_semantic(state.as_deref(), operation) {
                        refused = Some(refusal(why));
                    }
                }
            }
        }
        changed |= self.picker.poll_lookups();
        if let Some(outcome) = self.picker.take_outcome() {
            self.seat.answered(outcome);
            changed = true;
        }
        let refused = if stepped {
            refused
        } else {
            self.seat.held.borrow().listing.refused.clone()
        };
        let listing = listing_of(&self.picker, refused);
        let mut held = self.seat.held.borrow_mut();
        if held.listing != listing {
            held.listing = listing;
            changed = true;
        }
        changed
    }
}

impl ui::widget::Decorator for Chooser {
    type Inner = FilePicker;

    fn inner(&self) -> &FilePicker {
        &self.picker
    }

    fn inner_mut(&mut self) -> &mut FilePicker {
        &mut self.picker
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        let served = self.serve();
        Widget::update(&mut self.picker, state, path) | served
    }

    fn handle_event(
        &mut self,
        event: &ui::advanced::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        Widget::handle_event(&mut self.picker, event, ctx);
        self.serve();
        // The sheet is modal: no key reaches the app beneath.
        if matches!(event, ui::advanced::WindowEvent::KeyboardInput { .. }) {
            *ctx.handled = true;
        }
    }
}

/// Wakes the sheet's app when the chooser's worker answers.
struct Wake(ui::WakeHandle);

impl std::task::Wake for Wake {
    fn wake(self: Arc<Self>) {
        self.0.wake();
    }
}

/// The chooser as an app.
pub struct FilePickerApp;

impl ResourceApp for FilePickerApp {
    type Resource = FileChooser;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = "File chooser";
    const ICON: Option<&'static str> = Some("folder-open");
    // Over the desktop: the sheet's scrim shows the desktop under it.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: a request and its answer are the service's.
    const DOCUMENT_HISTORY: bool = false;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::CATALOG)
    }

    fn view(cx: &mut ViewCx<FileChooser>) -> Element {
        let seat = cx.take_lent::<FilePickerSeat>().unwrap_or_else(|| {
            // Headless, a scenario stands in for the service.
            let seat = FilePickerSeat::default();
            if let Some(request) = cx.fixture::<FileRequest>() {
                seat.ask(Some((*request).clone()));
            }
            seat
        });
        let opened = cx.local(seat.request());
        seat.held.borrow_mut().opened = Some(opened.clone());
        let wake = cx.waker();
        seat.presentation.wake_with(wake.clone());
        let stepping = seat.clone();
        cx.effects(move |effect: FilePickerEffect, _cx| {
            stepping.step(effect)?;
            Ok(Effected::done())
        });
        cx.switch(&opened, move |_cx, request| match request {
            Some(request) => {
                let waker = std::task::Waker::from(Arc::new(Wake(wake.clone())));
                let mut chooser = Chooser {
                    picker: picker(request, Some(waker)),
                    seat: seat.clone(),
                };
                chooser.serve();
                Box::new(chooser) as Element
            }
            None => Box::new(ui::widget::Column::new()),
        })
    }

    fn scenarios() -> &'static [&'static str] {
        &["open", "open-several", "save", "folder"]
    }

    fn fixture(fixture: &mut ui::framework::Fixture<FileChooser>) {
        fixture.provide(scenario(fixture.scenario()));
    }
}

/// What the chooser is asked in one of its scenarios, for captures.
pub fn scenario(name: Option<&str>) -> FileRequest {
    let home = PathBuf::from("/var/home/you");
    let request = FileRequest {
        handle: "scenario".to_owned(),
        title: "Choose a picture".to_owned(),
        app_name: Some("Photos".to_owned()),
        app_id: Some("so.tilo.photos".to_owned()),
        initial_dir: home.join("Pictures"),
        roots: vec![home.clone()],
        home: Some(home),
        filters: vec![FileType {
            label: "Images".to_owned(),
            extensions: vec!["png".to_owned(), "jpg".to_owned()],
        }],
        ..FileRequest::default()
    };
    match name {
        Some("open-several") => FileRequest {
            multiple: true,
            ..request
        },
        Some("save") => FileRequest {
            title: "Export the picture".to_owned(),
            mode: FileMode::Save,
            suggested_name: Some("Holiday.png".to_owned()),
            ..request
        },
        Some("folder") => FileRequest {
            title: "Choose where to import from".to_owned(),
            mode: FileMode::Folder,
            filters: Vec::new(),
            ..request
        },
        _ => request,
    }
}

/// The app's definition, which the session mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(FilePickerApp)
}
