//! File-picker dialog widget: the system file chooser, which the shell shows
//! for the `FileChooser` portal and for an app's path request, and any "open /
//! save file from app menu" flow. It draws and hit-tests itself; the host
//! positions it as a modal sheet and drains [`FilePicker::take_outcome`].
//!
//! Choosing a file in it gives the asking app that file, so the sheet says
//! who is asking: the app's icon and name, with what it wants ("TOTP wants
//! to open a file") or the app's own purpose ("Choose a QR code image")
//! ([`FilePicker::with_requester`]). The primary button stays the outcome
//! (Open, Save, Choose folder).
//!
//! Top to bottom: who is asking; a toolbar with Up, the location and the
//! search field; the places sidebar beside the file list; a filename field
//! in Save mode, the type filter (only when the app named more than one
//! type) and Cancel and the primary button.
//!
//! * The location is a path bar: the shared [`Breadcrumb`] trail from the
//!   place it is in ("Home › Projects"), each ancestor a click away. A click
//!   on the trail's empty space, or Ctrl+L, turns it into a field to type a
//!   path into; Enter goes there, Escape puts the trail back.
//! * The sidebar leads with Recent (what was chosen here before, when the
//!   host says what that is) and then Home, the usual folders under it and
//!   every other place the person can reach.
//! * The file list is the desktop's one list of files
//!   ([`crate::widget::file_table::FileTable`]), the same one the file
//!   manager shows: its columns, sorting, scrolling, touch and rows.
//!   Directory click navigates in file modes and selects in directory mode;
//!   double-click navigates or chooses; Ctrl/Shift multi-select when
//!   enabled.
//! * Typing searches. The shared [`SearchField`] narrows the folder in view
//!   as the person types ("3 of 40"); Enter searches every place the person
//!   can reach through the system search the host hands in
//!   ([`FilePicker::with_search`]). Typing while the list has the keyboard
//!   types into the search field. The same search is the picker's `search`
//!   operation ([`semantic::PickerOperation::Search`]), for the keyboard and
//!   for agents.
//!
//! Keyboard: Tab moves between the controls, Up/Down move the selection,
//! Enter opens/enters, Backspace goes to the parent, Escape cancels (or
//! clears the search first).
//!
//! Constructed with "accessible roots" — the paths the caller may be given.
//! [`FilePicker::can_navigate_to`] gates every path mutation against them so
//! the person can't `..` out of them, and every chosen path is checked
//! against their real paths again before it is answered.

mod reading;
pub mod semantic;

use reading::{Kind, Real};

use crate::widget::ModelPath;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tilo_sort::CompactSort as _;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX, draw_icon, icon_for_path};
use crate::presence::PresenceCtx;
use crate::selection::SelectionModel;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::ButtonRole;
use crate::widget::TaffyTreeExt;
use crate::widget::file_table::{
    FileColumn, FileRow, FileTable, FileTableInputs, FileTableMetrics, NameCell,
    paint_icon_and_name,
};
use crate::widget::{Breadcrumb, SearchField, TextInput, Widget};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

/// Token-derived sizes the picker lays out with. Resolved from the theme in
/// `build_layout` and refreshed at paint, cached because
/// `apply_layout_helper` runs from event handlers and `set_rect` with no
/// theme in hand; the draw path reads the same values, so layout and paint
/// agree at every `ui_scale` and text scale.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PickerMetrics {
    /// The dialog's outer padding and the gutter between its regions (`s4`).
    pad: f64,
    /// Between related controls: buttons, a label and its field (`s2`).
    gap: f64,
    /// Between the dialog's bands (`s3`).
    section: f64,
    /// One list or sidebar row (`nav_row_height`).
    row_h: f64,
    /// A button, field or path bar (`min_pointer_target`).
    control_h: f64,
    /// The heading's line box.
    title_h: f64,
    /// The line under the heading that names the asking app.
    subtitle_h: f64,
    /// The places sidebar (`sidebar_width`, narrowed for a dialog).
    sidebar_w: f64,
    /// Below this dialog width the sidebar gives way to the list.
    sidebar_min_dialog_w: f64,
    icon_sm: f64,
    icon_md: f64,
    icon_lg: f64,
    /// Horizontal inset of text inside a field, chip or row (`s2`).
    inset_x: f64,
    /// The asking app's icon beside the heading.
    app_icon: f64,
    /// The search field at the toolbar's end.
    search_w: f64,
    /// A footer button is never narrower than this.
    min_button_w: f64,
    name_label_w: f64,
    /// The band under the Save field that says why a name can't be used.
    error_h: f64,
    /// The type dropdown's width before its labels have been measured.
    filter_w: f64,
}

impl PickerMetrics {
    fn from_theme(theme: &Theme) -> Self {
        let t = &theme.tokens;
        let (sp, m) = (&t.spacing, &t.metrics);
        let icon_lg = f64::from(m.icon_lg);
        let line = |role: TextRole| {
            f64::from(t.typography.size(role)) * f64::from(theme.text(role).line_height)
        };
        Self {
            pad: f64::from(sp.s4),
            gap: f64::from(sp.s2),
            section: f64::from(sp.s3),
            row_h: f64::from(m.nav_row_height),
            control_h: f64::from(m.min_pointer_target),
            title_h: line(TextRole::Heading),
            subtitle_h: line(TextRole::Caption),
            sidebar_w: f64::from(m.sidebar_width) * 0.72,
            sidebar_min_dialog_w: f64::from(m.panel_width_sheet) * 0.9,
            icon_sm: f64::from(m.icon_sm),
            icon_md: f64::from(m.icon_md),
            icon_lg,
            inset_x: f64::from(sp.s2),
            app_icon: icon_lg * 1.5,
            search_w: f64::from(m.sidebar_width) * 0.9,
            min_button_w: icon_lg * 4.0,
            name_label_w: icon_lg * 3.5,
            error_h: line(TextRole::Caption) + f64::from(sp.s2),
            filter_w: icon_lg * 8.0,
        }
    }
}

impl Default for PickerMetrics {
    /// The unscaled reference values, for a picker laid out before it has
    /// seen a theme.
    fn default() -> Self {
        Self::from_theme(&Theme::dark())
    }
}

/// A shortcut in the sidebar: somewhere the person goes often, limited to
/// what the caller's sandbox allows.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Place {
    /// Translation key of a well-known place; `None` shows `name` as is.
    key: Option<&'static str>,
    name: String,
    /// Where it goes; empty for Recent, which is a list, not a folder.
    path: PathBuf,
    icon: Icon,
    /// What was chosen here before.
    recent: bool,
}

impl Place {
    fn label(&self) -> String {
        self.key.map_or_else(
            || self.name.clone(),
            |key| crate::i18n::widget_format(key, &[]),
        )
    }
}

/// Recent (when there is something to show), Home, the usual folders under
/// it that are `present` (read off the UI thread), then every other
/// accessible root (the workspace folders), each only if the caller may
/// enter it.
fn default_places(
    roots: &[PathBuf],
    home: Option<&Path>,
    recent: bool,
    present: &[PathBuf],
) -> Vec<Place> {
    let allowed = |p: &Path| roots.iter().any(|root| p.starts_with(root));
    let mut places: Vec<Place> = Vec::new();
    if recent {
        places.push(Place {
            key: Some("widget-filepicker-place-recent"),
            name: String::new(),
            path: PathBuf::new(),
            icon: Icon::History,
            recent: true,
        });
    }
    if let Some(home) = home.filter(|h| h.is_absolute()).map(Path::to_path_buf) {
        let well_known = [
            (
                None,
                "widget-filepicker-place-home",
                Icon::Home,
                home.clone(),
            ),
            (
                Some("Documents"),
                "widget-filepicker-place-documents",
                Icon::FileText,
                home.join("Documents"),
            ),
            (
                Some("Downloads"),
                "widget-filepicker-place-downloads",
                Icon::Download,
                home.join("Downloads"),
            ),
            (
                Some("Pictures"),
                "widget-filepicker-place-pictures",
                Icon::Image,
                home.join("Pictures"),
            ),
            (
                Some("Music"),
                "widget-filepicker-place-music",
                Icon::Music,
                home.join("Music"),
            ),
            (
                Some("Videos"),
                "widget-filepicker-place-videos",
                Icon::Video,
                home.join("Videos"),
            ),
        ];
        for (folder, key, icon, path) in well_known {
            if folder.is_some() && !present.contains(&path) {
                continue;
            }
            if allowed(&path) {
                places.push(Place {
                    key: Some(key),
                    name: String::new(),
                    path,
                    icon,
                    recent: false,
                });
            }
        }
    }
    for root in roots {
        if places.iter().any(|p| &p.path == root) {
            continue;
        }
        let name = root.file_name().map_or_else(
            || root.to_string_lossy().into_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        places.push(Place {
            key: None,
            name,
            path: root.clone(),
            icon: Icon::Folder,
            recent: false,
        });
    }
    places
}

/// The Fluent key of a toolkit message, resolved in the desktop's language.
fn tr(key: &str) -> String {
    crate::i18n::widget_format(key, &[])
}

/// Translation keys of the labels the footer buttons draw; a button label is
/// a key, resolved when it is painted.
const CANCEL_KEY: &str = "widget-filepicker-cancel";
const REPLACE_KEY: &str = "widget-filepicker-replace";
/// The label of [`FileFilter::all`], shown translated.
const ALL_FILES_LABEL: &str = "All files";

/// A logical size as the text renderer's width argument.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a width of a few hundred logical pixels fits f32 exactly enough for wrapping"
)]
fn px(width: f64) -> f32 {
    width as f32
}

/// The file chooser is an immediate-mode modal, so its controls do not each
/// have a retained Taffy node.  Keep their keyboard order in the toolkit's
/// standard [`crate::overlay::FocusTrap`] instead of reimplementing wrapping
/// Tab logic locally.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum PickerFocus {
    Up,
    /// The path bar: the trail, or the field typing a path.
    Location,
    /// The search field.
    Search,
    /// The places sidebar.
    Places,
    List,
    Filter,
    Filename,
    Cancel,
    Confirm,
    /// The replace prompt's Cancel: keep the existing file, back to the name.
    KeepExisting,
    /// The replace prompt's destructive Replace.
    Replace,
}

/// Semantic key of the replace prompt that parents its two buttons.
const REPLACE_PROMPT_KEY: u64 = 9;

impl PickerFocus {
    const fn id(self) -> u64 {
        match self {
            Self::Up => 1,
            Self::List => 2,
            Self::Filter => 3,
            Self::Filename => 4,
            Self::Cancel => 5,
            Self::Confirm => 6,
            Self::KeepExisting => 7,
            Self::Replace => 8,
            Self::Places => 10,
            Self::Location => 11,
            Self::Search => 12,
        }
    }

    const fn from_id(id: u64) -> Option<Self> {
        match id {
            1 => Some(Self::Up),
            2 => Some(Self::List),
            3 => Some(Self::Filter),
            4 => Some(Self::Filename),
            5 => Some(Self::Cancel),
            6 => Some(Self::Confirm),
            7 => Some(Self::KeepExisting),
            8 => Some(Self::Replace),
            10 => Some(Self::Places),
            11 => Some(Self::Location),
            12 => Some(Self::Search),
            _ => None,
        }
    }
}

/// A named file-type filter. An empty `extensions` list matches every
/// file (the conventional "All files" entry).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileFilter {
    pub label: String,
    /// Lower-cased extensions without the dot (e.g. `["png", "jpg"]`).
    pub extensions: Vec<String>,
}

impl FileFilter {
    pub fn new(label: impl Into<String>, extensions: &[&str]) -> Self {
        Self {
            label: label.into(),
            extensions: extensions.iter().map(|e| e.to_ascii_lowercase()).collect(),
        }
    }

    pub fn all() -> Self {
        Self {
            label: ALL_FILES_LABEL.into(),
            extensions: Vec::new(),
        }
    }

    fn matches(&self, path: &Path) -> bool {
        if self.extensions.is_empty() {
            return true;
        }
        match path.extension().and_then(|e| e.to_str()) {
            Some(ext) => self.extensions.contains(&ext.to_ascii_lowercase()),
            None => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePickerEntry {
    pub name: String,
    pub path: PathBuf,
    /// Directories navigate on click; files select.
    pub is_dir: bool,
    /// Bytes; `0` for a folder or when the size could not be read.
    pub size: u64,
    /// Last change, in seconds since the Unix epoch.
    pub modified: Option<i64>,
    /// What it really is and whether the chooser may give it, as read
    /// with the listing; confirming reads it again.
    real: Real,
}

/// The app that asked for the chooser, as the sheet names it: choosing a
/// file gives it to this app.
#[derive(Clone)]
pub struct Requester {
    /// What the app is called ("TOTP").
    pub name: String,
    /// Its symbol, shown when it has no artwork of its own.
    pub glyph: Icon,
    /// Its own artwork, when it has some.
    pub image: Option<crate::icons::AppImage>,
}

/// The system search the host hands the picker: the paths matching a query
/// among the places the person can reach. The picker calls it on a worker
/// thread, never on the UI thread, shows only what its roots allow, and
/// checks a chosen one again before answering.
pub type PickerSearch = Arc<dyn Fn(&str) -> Vec<PathBuf> + Send + Sync>;

/// A listing being read on the worker, for the listing and folder that
/// asked.
struct Lookup {
    asked: (Listing, PathBuf),
    pending: reading::Pending<reading::Listed>,
}

/// A path about to be given or gone to, being checked on the worker; the
/// chooser does what `intent` says once it knows.
struct Check {
    intent: Intent,
    pending: reading::Pending<Vec<Real>>,
}

/// What a check is for, with the paths it was asked about in the order
/// they were checked.
enum Intent {
    /// Open the selected files.
    Open(Vec<PathBuf>),
    /// Choose the selected folders, or the folder in view.
    Directory(Vec<PathBuf>),
    /// Save over the selected file: checks the file.
    SaveSelected(PathBuf),
    /// Save under a typed name: checks its folder, then the file.
    SaveName(SaveName),
    /// Go where the typed location says: checks the path.
    Location(PathBuf),
}

/// Most rows a search across places shows.
pub const SEARCH_LIMIT: usize = 200;

/// What the list shows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Listing {
    /// The folder in view (`current_dir`).
    Folder,
    /// What was chosen here before.
    Recent,
    /// What the system search found for this query across reachable places.
    Search(String),
}

struct PickerRow {
    entry: FilePickerEntry,
    /// The folder it is in, for a row that is not in the folder in view (a
    /// recent file, a search result).
    location: Option<String>,
}

impl FileRow for PickerRow {
    fn path(&self) -> &Path {
        &self.entry.path
    }

    fn name(&self) -> &str {
        &self.entry.name
    }

    fn is_dir(&self) -> bool {
        self.entry.is_dir
    }

    fn size_text(&self) -> String {
        if self.entry.is_dir {
            String::new()
        } else {
            crate::format::size(self.entry.size)
        }
    }

    fn modified(&self) -> Option<chrono::DateTime<chrono::Local>> {
        self.entry
            .modified
            .and_then(|at| chrono::DateTime::from_timestamp(at, 0))
            .map(|utc| utc.with_timezone(&chrono::Local))
    }

    /// The icon and the name, then, for a row from elsewhere, the folder it
    /// is in, in the quieter ink.
    fn paint_name(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        cell: Rect,
        metrics: &FileTableMetrics,
    ) {
        let name_end = paint_icon_and_name(
            scene,
            transform,
            theme,
            text_renderer,
            cell,
            metrics,
            NameCell {
                icon: icon_for_path(&self.entry.path, self.entry.is_dir),
                name: &self.entry.name,
                is_dir: self.entry.is_dir,
            },
        );
        let Some(location) = &self.location else {
            return;
        };
        let tokens = &theme.tokens;
        let x = name_end + f64::from(tokens.spacing.s3);
        let room = cell.x1 - f64::from(tokens.spacing.s2) - x;
        if room <= f64::from(tokens.metrics.icon_md) {
            return;
        }
        let style = theme.text(TextRole::Label);
        let label = text_renderer.elide_to_width_style(location, px(room), &style);
        let (_, h) = text_renderer.measure_style(&label, None, &style);
        text_renderer.draw_style(
            scene,
            &label,
            tokens.colors.text_dim,
            transform.pre_translate(Vec2::new(x, cell.center().y - f64::from(h) / 2.0)),
            None,
            &style,
        );
    }
}

/// Drained by the host via [`FilePicker::take_outcome`] once per frame;
/// `None` means "user hasn't decided yet". The terminal variants resolve
/// the host's `oneshot::Sender<Option<Vec<PathBuf>>>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilePickerOutcome {
    Selected(Vec<PathBuf>),
    Cancelled,
}

/// Open picks existing files; Save adds a filename field whose contents
/// resolve to `current_dir/<name>`; Directory picks existing folders.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum FilePickerMode {
    #[default]
    Open,
    Save,
    Directory,
}

/// Where a typed Save name points.
#[derive(Debug, PartialEq, Eq)]
enum SaveName {
    /// A folder to open (the name ended in `/`).
    Folder(PathBuf),
    /// A file `name` in `folder`.
    File { folder: PathBuf, name: String },
}

/// Why a typed Save name can't be used.
#[derive(Debug, PartialEq, Eq)]
enum SaveNameError {
    /// Not a file name (`..`, `/`, `~` without a home).
    Invalid,
    /// The folder it names doesn't exist.
    NoFolder(PathBuf),
    /// The folder is outside what the asking app may write.
    NotAllowed(PathBuf),
}

pub struct FilePicker {
    rect: Rect,
    /// The app's own purpose ("Choose a QR code image"); empty when it gave
    /// none, and the heading says what the app wants instead.
    title: String,
    /// Who asked, when the host knows.
    requester: Option<Requester>,
    mode: FilePickerMode,
    multiple: bool,
    accept_label: Option<String>,

    current_dir: PathBuf,
    /// What the list shows: the folder in view, Recent or a search.
    listing: Listing,
    /// Everything the listing holds, before the search field narrows it.
    listed: Vec<FilePickerEntry>,
    /// What the list shows, in display order.
    entries: Vec<FilePickerEntry>,
    /// File indices in Open/Save mode; directory indices in Directory mode.
    selection: SelectionModel,
    /// The column the list is sorted by, and which way.
    sort: FileColumn,
    ascending: bool,

    /// Paths the host has marked as accessible. Navigation outside every
    /// root is silently ignored.
    accessible_roots: Vec<PathBuf>,
    /// What was chosen here before, newest first: the Recent place.
    recent: Vec<PathBuf>,
    /// The system search across the places the person can reach.
    search: Option<PickerSearch>,
    /// Wakes the host when a lookup finishes, so it shows without waiting
    /// for the next input.
    waker: Option<std::task::Waker>,
    /// The listing being read, while it is.
    lookup: Option<Lookup>,
    read: Vec<reading::Row>,
    /// What the folder in view is, as last read.
    here: Option<Real>,
    /// What to select once the listing being read is in.
    then_select: Option<PathBuf>,
    /// A path being checked before the chooser acts on it.
    check: Option<Check>,
    /// The usual folders under home that exist, and the check reading them.
    present: Vec<PathBuf>,
    places_check: Option<reading::Pending<Vec<Real>>>,

    filters: Vec<FileFilter>,
    active_filter: usize,

    /// The Save-mode filename: the shared text field, so caret, selection,
    /// clipboard and input methods behave as in every other field.
    filename: TextInput,
    /// The search field: the shared one, narrowing the list as it is typed.
    query: SearchField,
    /// The file list: the desktop's one list of files.
    table: FileTable<PickerRow>,
    /// The rows changed since the table last took them.
    rows_dirty: bool,
    /// The path bar's trail, rebuilt when the place changes.
    crumbs: Breadcrumb,
    /// Where each trail segment goes, in trail order.
    crumb_targets: Vec<PathBuf>,
    crumb_clicked: Rc<Cell<Option<usize>>>,
    /// The person clicked the trail's empty space to type a path.
    crumb_edit: Rc<Cell<bool>>,
    /// The path being typed, while the path bar is a field.
    location: Option<TextInput>,
    /// The typed path goes nowhere the picker may go.
    location_invalid: bool,
    /// The theme of the latest layout, for the parts that lay themselves
    /// out on a tree of their own from event handlers.
    theme: Theme,

    /// Set on Open / Cancel. `None` until resolved.
    pending_outcome: Option<FilePickerOutcome>,
    /// Why the typed Save name can't be used, shown under the field until
    /// the name changes. Save never closes the dialog on a name it refuses.
    name_error: Option<String>,
    /// The person's home folder, for a typed `~/…` name.
    home: Option<PathBuf>,
    /// A Save target that already exists, waiting for the person to say
    /// whether to replace it. While set, the prompt is the only thing on the
    /// picker that takes input.
    replace_prompt: Option<PathBuf>,

    /// The sidebar's shortcuts, limited to what the sandbox allows.
    places: Vec<Place>,
    /// The sidebar row under the keyboard cursor.
    place_cursor: Option<usize>,
    hover_place: Option<usize>,
    /// The type menu is open over its chip.
    filter_open: bool,
    filter_hover: Option<usize>,

    // Hit-test rects, recomputed on each `apply_layout` / `draw`.
    place_rects: Vec<Rect>,
    filter_menu_rect: Rect,
    filter_rows: Vec<Rect>,
    /// Cancel, confirm and Replace button widths, measured at paint.
    button_w: [f64; 3],
    /// The type dropdown chip's width, measured at paint.
    filter_w: f64,
    /// The "File type" label before it, measured at paint.
    filter_label_w: f64,
    open_rect: Rect,
    cancel_rect: Rect,
    up_rect: Rect,
    filter_rect: Rect,
    filename_rect: Rect,
    search_rect: Rect,
    replace_card_rect: Rect,
    keep_existing_rect: Rect,
    replace_rect: Rect,
    /// `(entry_index, rect)` for the rows the list shows now.
    entry_rows: Vec<(usize, Rect)>,
    /// Token-derived insets from the theme of the latest layout build, so
    /// the renderer-less layout pass and the draw pass read the same
    /// `ui_scale`d values. `PickerMetrics::default()` until a theme is seen.
    metrics: PickerMetrics,

    /// `MouseInput` carries no position payload, so this tracks the cursor
    /// for click resolution.
    last_cursor: Point,
    /// Last entry pressed + when, for double-click open/navigation detection.
    last_click: Option<(usize, crate::time::Instant)>,

    /// `0` = Cancel, `1` = the mode-specific confirmation action.
    hover_button: Option<u8>,
    /// The replace prompt button under the pointer.
    hover_prompt: Option<PickerFocus>,
    /// The active control inside this immediate-mode modal.  It is reflected
    /// in both paint and accessibility semantics.
    focus: Option<PickerFocus>,
    has_keyboard_focus: bool,
}

impl FilePicker {
    /// `accessible_roots` limits navigation — if empty, defaults to
    /// `[initial_dir]`. An `initial_dir` outside them opens at the first
    /// root instead. `title` is the app's own purpose, if it gave one.
    pub fn new<P: AsRef<Path>>(
        title: impl Into<String>,
        initial_dir: P,
        accessible_roots: Vec<PathBuf>,
    ) -> Self {
        let asked: PathBuf = initial_dir.as_ref().to_path_buf();
        let roots = if accessible_roots.is_empty() {
            vec![asked.clone()]
        } else {
            accessible_roots
        };
        // Nothing can be opened or saved outside the roots, so a folder
        // there would refuse every confirmation.
        let initial = if roots.iter().any(|root| asked.starts_with(root)) {
            asked
        } else {
            roots.first().cloned().unwrap_or(asked)
        };
        let places = default_places(&roots, None, false, &[]);
        let metrics = PickerMetrics::default();
        let mut me = Self {
            places,
            place_cursor: None,
            hover_place: None,
            filter_open: false,
            filter_hover: None,
            place_rects: Vec::new(),
            filter_menu_rect: Rect::ZERO,
            filter_rows: Vec::new(),
            button_w: [metrics.min_button_w; 3],
            filter_w: metrics.filter_w,
            filter_label_w: 0.0,
            rect: Rect::ZERO,
            title: title.into(),
            requester: None,
            mode: FilePickerMode::default(),
            // Preserve the historical direct-widget Open behavior. Portal
            // callers explicitly hand their single/multiple intent through.
            multiple: true,
            accept_label: None,
            current_dir: initial,
            listing: Listing::Folder,
            listed: Vec::new(),
            entries: Vec::new(),
            selection: SelectionModel::new(),
            sort: FileColumn::Name,
            ascending: true,
            accessible_roots: roots,
            recent: Vec::new(),
            search: None,
            waker: None,
            lookup: None,
            read: Vec::new(),
            here: None,
            then_select: None,
            check: None,
            present: Vec::new(),
            places_check: None,
            filters: vec![FileFilter::all()],
            active_filter: 0,
            // A file name is not prose: no spelling marks, and never an
            // autocorrection rewriting a word of it.
            filename: TextInput::local()
                .with_kind(crate::widget::TextInputKind::Name)
                .with_plain_chrome()
                .with_accessible_name(tr("widget-filepicker-name-accessible"))
                .with_placeholder(tr("widget-filepicker-name-placeholder")),
            query: SearchField::new().with_placeholder(&tr("widget-filepicker-search")),
            table: FileTable::new("file_picker_list", tr("widget-filepicker-files"))
                .handles_clicks(),
            rows_dirty: true,
            crumbs: Breadcrumb::new(Vec::new()),
            crumb_targets: Vec::new(),
            crumb_clicked: Rc::new(Cell::new(None)),
            crumb_edit: Rc::new(Cell::new(false)),
            location: None,
            location_invalid: false,
            theme: Theme::dark(),
            pending_outcome: None,
            name_error: None,
            home: None,
            replace_prompt: None,
            open_rect: Rect::ZERO,
            cancel_rect: Rect::ZERO,
            up_rect: Rect::ZERO,
            filter_rect: Rect::ZERO,
            filename_rect: Rect::ZERO,
            search_rect: Rect::ZERO,
            replace_card_rect: Rect::ZERO,
            keep_existing_rect: Rect::ZERO,
            replace_rect: Rect::ZERO,
            entry_rows: Vec::new(),
            metrics,
            last_cursor: Point::ZERO,
            last_click: None,
            hover_button: None,
            hover_prompt: None,
            focus: None,
            has_keyboard_focus: false,
        };
        me.refresh_entries();
        me
    }

    /// Who asked for the chooser: the sheet names the app, with its icon,
    /// because choosing a file gives it to that app.
    #[must_use]
    pub fn with_requester(mut self, requester: Option<Requester>) -> Self {
        self.requester = requester;
        self
    }

    /// What was chosen here before, newest first. Recent leads the sidebar
    /// when there is any.
    #[must_use]
    pub fn with_recent(mut self, recent: Vec<PathBuf>) -> Self {
        self.recent = recent;
        self.rebuild_places();
        self
    }

    /// The system search across the places the person can reach: Enter in
    /// the search field, and the `search` operation, list what it finds.
    #[must_use]
    pub fn with_search(mut self, search: Option<PickerSearch>) -> Self {
        self.search = search;
        self
    }

    /// What wakes the host when the worker answers (a listing, a check,
    /// the usual folders); without one the answer shows at the next frame.
    /// What the chooser already asked for is asked again, so that answer
    /// wakes this host too.
    #[must_use]
    pub fn with_waker(mut self, waker: Option<std::task::Waker>) -> Self {
        self.waker = waker;
        if self.lookup.is_some() {
            self.refresh_entries();
        }
        if self.places_check.is_some() {
            self.places_check = reading::check(
                self.usual_folders(),
                self.accessible_roots.clone(),
                self.waker.clone(),
            );
        }
        self
    }

    #[must_use]
    pub fn with_mode(mut self, mode: FilePickerMode) -> Self {
        self.mode = mode;
        self.selection.clear();
        self.table = FileTable::new(
            "file_picker_list",
            tr(if mode == FilePickerMode::Directory {
                "widget-filepicker-folders"
            } else {
                "widget-filepicker-files"
            }),
        )
        .handles_clicks();
        self.relist();
        // Construction begins in Open mode so a later `.with_mode(Save)` must
        // reset the initial target from the listbox to Filename.
        self.focus = None;
        self.ensure_valid_focus();
        self
    }

    /// The person's home folder, which puts Home and the usual folders under
    /// it (Documents, Downloads, …) in the sidebar. Only those the caller's
    /// sandbox allows are shown. The toolkit reads no environment, so the
    /// host says where home is.
    #[must_use]
    pub fn with_home(mut self, home: Option<PathBuf>) -> Self {
        self.home = home;
        self.present.clear();
        self.places_check = reading::check(
            self.usual_folders(),
            self.accessible_roots.clone(),
            self.waker.clone(),
        );
        self.rebuild_places();
        self.rebuild_crumbs();
        self
    }

    /// The usual folders under home the sidebar shows when they exist.
    fn usual_folders(&self) -> Vec<PathBuf> {
        let Some(home) = self.home.as_deref().filter(|home| home.is_absolute()) else {
            return Vec::new();
        };
        ["Documents", "Downloads", "Pictures", "Music", "Videos"]
            .iter()
            .map(|folder| home.join(folder))
            .collect()
    }

    fn rebuild_places(&mut self) {
        self.places = default_places(
            &self.accessible_roots,
            self.home.as_deref(),
            !self.recent.is_empty(),
            &self.present,
        );
        self.place_cursor = None;
    }

    #[must_use]
    pub fn with_multiple(mut self, multiple: bool) -> Self {
        self.multiple = multiple;
        if !multiple && let Some(primary) = self.selection.primary() {
            self.selection.select_only(primary);
        }
        self
    }

    #[must_use]
    pub fn with_accept_label(mut self, label: Option<String>) -> Self {
        self.accept_label = label.filter(|label| !label.trim().is_empty());
        self
    }

    /// The types the app can take; the first becomes active. The type
    /// menu is only there when the app named more than one, and then ends
    /// with "All files". One type filters the list with no menu; none shows
    /// every file.
    #[must_use]
    pub fn with_filters(mut self, filters: Vec<FileFilter>) -> Self {
        let mut types: Vec<FileFilter> = filters
            .into_iter()
            .filter(|filter| !filter.extensions.is_empty())
            .collect();
        if types.len() != 1 {
            types.push(FileFilter::all());
        }
        self.filters = types;
        self.active_filter = 0;
        self.relist();
        self
    }

    #[must_use]
    pub fn with_filename(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        self.set_filename(&name);
        self
    }

    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
        self.apply_layout_helper();
    }

    pub fn current_dir(&self) -> &Path {
        &self.current_dir
    }

    pub fn entries(&self) -> &[FilePickerEntry] {
        &self.entries
    }

    /// Back-compat accessor alongside [`Self::selected_indices`].
    pub fn selection(&self) -> Option<usize> {
        self.selection
            .primary()
            .filter(|&i| self.selection.is_selected(i))
    }

    pub fn selected_indices(&self) -> Vec<usize> {
        self.selection.selected()
    }

    pub fn active_filter_label(&self) -> &str {
        self.filters
            .get(self.active_filter)
            .map_or(ALL_FILES_LABEL, |f| f.label.as_str())
    }

    /// Put `name` in the filename field with the name selected but not its
    /// extension, so typing replaces the name and keeps the type.
    fn set_filename(&mut self, name: &str) {
        self.name_error = None;
        self.filename.set_text(name);
        let stem = name.rfind('.').filter(|&i| i > 0).unwrap_or(name.len());
        self.filename.set_selection(0, stem);
    }

    pub fn filename(&self) -> &str {
        self.filename.value()
    }

    pub fn confirm_label(&self) -> &str {
        self.accept_label.as_deref().unwrap_or(match self.mode {
            FilePickerMode::Open => "Open",
            FilePickerMode::Save => "Save",
            FilePickerMode::Directory => "Choose folder",
        })
    }

    /// Set the selection without a click, for tests/hosts.
    pub fn set_selection(&mut self, index: usize) {
        if index < self.entries.len() {
            self.selection.select_only(index);
            self.scroll_to(index);
        }
    }

    pub fn select_by_path(&mut self, path: &Path) -> bool {
        if let Some(i) = self.entries.iter().position(|e| e.path == path) {
            self.selection.select_only(i);
            self.scroll_to(i);
            true
        } else {
            false
        }
    }

    pub fn click_open(&mut self) {
        if self.can_commit_open() {
            self.commit_open();
        }
    }

    pub fn click_cancel(&mut self) {
        self.pending_outcome = Some(FilePickerOutcome::Cancelled);
    }

    /// The existing file a Save is waiting to replace, while the picker asks
    /// whether to.
    pub fn replace_prompt(&self) -> Option<&Path> {
        self.replace_prompt.as_deref()
    }

    /// Answer the replace prompt with Replace: the Save resolves to the
    /// existing file.
    pub fn confirm_replace(&mut self) {
        if let Some(target) = self.replace_prompt.take() {
            self.hover_prompt = None;
            self.pending_outcome = Some(FilePickerOutcome::Selected(vec![target]));
        }
    }

    /// Answer the replace prompt with Cancel: nothing is chosen, and the
    /// name is back in front of the person to change.
    pub fn keep_existing(&mut self) {
        if self.replace_prompt.take().is_some() {
            self.hover_prompt = None;
            self.focus = None;
            self.ensure_valid_focus();
            self.sync_text_focus();
        }
    }

    pub fn take_outcome(&mut self) -> Option<FilePickerOutcome> {
        self.pending_outcome.take()
    }

    pub fn outcome(&self) -> Option<&FilePickerOutcome> {
        self.pending_outcome.as_ref()
    }

    pub fn open_rect(&self) -> Rect {
        self.open_rect
    }

    pub fn cancel_rect(&self) -> Rect {
        self.cancel_rect
    }

    pub fn up_rect(&self) -> Rect {
        self.up_rect
    }

    pub fn filename_rect(&self) -> Rect {
        self.filename_rect
    }

    /// The entry drawn at `p`, among those scrolled into view.
    pub fn entry_at(&self, p: Point) -> Option<&FilePickerEntry> {
        self.entry_rows
            .iter()
            .find(|(_, rect)| rect.contains(p))
            .and_then(|(i, _)| self.entries.get(*i))
    }

    /// `None` unless `i` is currently scrolled into view.
    pub fn entry_rect(&self, i: usize) -> Option<Rect> {
        self.entry_rows
            .iter()
            .find(|(idx, _)| *idx == i)
            .map(|(_, r)| *r)
    }

    pub fn can_navigate_to(&self, candidate: &Path) -> bool {
        let cand = candidate.to_path_buf();
        self.accessible_roots
            .iter()
            .any(|root| cand.starts_with(root))
    }

    /// Up goes to the parent of the folder in view; Recent and a search
    /// have no parent.
    pub fn can_go_up(&self) -> bool {
        self.listing == Listing::Folder
            && self
                .current_dir
                .parent()
                .is_some_and(|p| self.can_navigate_to(p))
    }

    /// No-op when rejected by the roots.
    pub fn navigate_to<P: AsRef<Path>>(&mut self, target: P) {
        let t = target.as_ref().to_path_buf();
        if !self.can_navigate_to(&t) {
            return;
        }
        if self.current_dir != t {
            self.here = None;
        }
        self.current_dir = t;
        self.listing = Listing::Folder;
        self.leave_search();
        self.filter_open = false;
        self.selection.clear();
        self.table.reset_scroll();
        self.list_anew();
    }

    pub fn go_up(&mut self) {
        if !self.can_go_up() {
            return;
        }
        if let Some(parent) = self.current_dir.parent().map(std::path::Path::to_path_buf) {
            self.navigate_to(parent);
        }
    }

    /// Show what was chosen here before.
    pub fn show_recent(&mut self) {
        self.listing = Listing::Recent;
        self.leave_search();
        self.filter_open = false;
        self.selection.clear();
        self.table.reset_scroll();
        self.list_anew();
    }

    /// Search every place the person can reach for `query` with the system
    /// search, and list what it finds. Without a system search, the folder
    /// in view narrows to `query` instead.
    pub fn search_for(&mut self, query: &str) {
        let query = query.trim().to_owned();
        if self.query.value() != query {
            self.query.set_query(&query);
            let _ = self.query.take_dirty();
        }
        if query.is_empty() {
            self.end_search();
            return;
        }
        self.filter_open = false;
        self.selection.clear();
        self.table.reset_scroll();
        if self.search.is_some() {
            self.listing = Listing::Search(query);
            self.list_anew();
        } else {
            // Without a system search the folder in view narrows instead.
            self.narrow();
        }
    }

    /// Empty the search field and go back to the folder it searched from.
    fn end_search(&mut self) {
        self.leave_search();
        self.selection.clear();
        if matches!(self.listing, Listing::Search(_)) {
            self.listing = Listing::Folder;
            self.list_anew();
        } else {
            self.narrow();
        }
    }

    /// Empty the search field, for a listing of somewhere new.
    fn leave_search(&mut self) {
        if !self.query.is_empty() {
            self.query.set_query("");
            let _ = self.query.take_dirty();
        }
        self.query.set_count(None);
    }

    /// What the search field holds now narrows the listing.
    fn on_query_typed(&mut self) {
        if matches!(self.listing, Listing::Search(_)) {
            // Typing again narrows the folder the search started from.
            self.listing = Listing::Folder;
            self.list_anew();
            return;
        }
        let selected: Vec<PathBuf> = self.selected_paths();
        self.narrow();
        self.reselect(&selected);
        self.table.reset_scroll();
        // Type-to-find: the first match is the one Enter or Open takes.
        if self.selection.selected().is_empty() && !self.query.is_empty() {
            self.set_selection(0);
        }
    }

    pub fn cycle_filter(&mut self) {
        if self.filters.len() > 1 {
            // wrapping_add is exactly the right operator for cyclic index
            // arithmetic: even in the unreachable case where active_filter
            // is usize::MAX, wrapping to 0 before the modulo still yields a
            // valid index rather than panicking. `filters.len() > 1` above
            // guards the `checked_rem` divisor.
            self.active_filter = self
                .active_filter
                .wrapping_add(1)
                .checked_rem(self.filters.len())
                .unwrap_or(0);
            self.selection.clear();
            self.table.reset_scroll();
            self.relist();
        }
    }

    /// Whether an entry belongs in the list in this mode under the active
    /// type filter: folders always (to go into, or to choose), files only
    /// in a file mode and when the filter takes them.
    fn listable(&self, path: &Path, is_dir: bool) -> bool {
        if is_dir {
            return true;
        }
        let idx = self.active_filter.min(self.filters.len().saturating_sub(1));
        self.mode != FilePickerMode::Directory
            && self
                .filters
                .get(idx)
                .is_some_and(|filter| filter.matches(path))
    }

    fn entry_for(row: &reading::Row) -> FilePickerEntry {
        let (path, is_dir, meta) = (row.path.clone(), row.is_dir, row.meta.as_ref());
        let name = path.file_name().map_or_else(
            || path.to_string_lossy().into_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        FilePickerEntry {
            name,
            path,
            is_dir,
            size: meta.map_or(0, fs::Metadata::len),
            modified: meta
                .and_then(|m| m.modified().ok())
                .map(|t| chrono::DateTime::<chrono::Utc>::from(t).timestamp()),
            real: row.real,
        }
    }

    /// The listing from the rows last read: in a folder every row the mode
    /// and filter take; for Recent and a search, also only rows the roots
    /// allow, each once, at most [`SEARCH_LIMIT`].
    fn relist(&mut self) {
        let folder = self.listing == Listing::Folder;
        let mut seen = std::collections::HashSet::new();
        let listed: Vec<FilePickerEntry> = self
            .read
            .iter()
            .filter(|row| {
                folder || (self.can_navigate_to(&row.path) && seen.insert(row.path.clone()))
            })
            .filter(|row| self.listable(&row.path, row.is_dir))
            .map(Self::entry_for)
            .take(if folder { usize::MAX } else { SEARCH_LIMIT })
            .collect();
        self.listed = listed;
        self.show_listed();
    }

    /// Whether the listing in view is still being read.
    fn looking_up(&self) -> bool {
        self.lookup.is_some()
    }

    /// Whether the worker still owes the chooser an answer: a listing, a
    /// check before it acts, the usual folders. A host that renders once
    /// (a screenshot) waits for this to clear, polling with
    /// [`Self::poll_lookups`].
    pub fn reading(&self) -> bool {
        self.lookup.is_some() || self.check.is_some() || self.places_check.is_some()
    }

    /// Take what the worker has answered: show a listing, act on a check,
    /// show the usual folders. Answers whether anything changed; the host
    /// calls it each frame it is woken.
    pub fn poll_lookups(&mut self) -> bool {
        let mut changed = false;
        if let Some(answer) = self
            .lookup
            .as_ref()
            .and_then(|lookup| lookup.pending.take())
        {
            let asked = self.lookup.take().map(|lookup| lookup.asked);
            if asked == Some((self.listing.clone(), self.current_dir.clone())) {
                let listed = answer.unwrap_or(reading::Listed {
                    rows: Vec::new(),
                    here: None,
                });
                if listed.here.is_some() {
                    self.here = listed.here;
                }
                self.read = listed.rows;
                let selected = self.selected_paths();
                self.relist();
                self.reselect(&selected);
                if let Some(target) = self.then_select.take()
                    && self.select_by_path(&target)
                    && self.mode == FilePickerMode::Save
                {
                    let name = target
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.set_filename(&name);
                }
                // The rows came after the last layout; place them where they hit-test.
                self.apply_layout_helper();
                changed = true;
            }
        }
        if let Some(answer) = self.places_check.as_ref().and_then(reading::Pending::take) {
            self.places_check = None;
            let reals = answer.unwrap_or_default();
            self.present = self
                .usual_folders()
                .into_iter()
                .zip(reals)
                .filter(|(_, real)| real.kind == Kind::Folder)
                .map(|(path, _)| path)
                .collect();
            self.rebuild_places();
            changed = true;
        }
        if let Some(answer) = self.check.as_ref().and_then(|check| check.pending.take())
            && let Some(check) = self.check.take()
        {
            self.act(check.intent, &answer.unwrap_or_default());
            changed = true;
        }
        changed
    }

    /// Read the listing again: the folder in view, Recent or the search.
    /// What was listed stays until the new rows are in.
    pub fn refresh_entries(&mut self) {
        let source = match self.listing.clone() {
            Listing::Folder => Some(reading::Source::Folder(self.current_dir.clone())),
            Listing::Recent => Some(reading::Source::Paths(self.recent.clone())),
            Listing::Search(query) => self
                .search
                .clone()
                .map(|search| reading::Source::Search(search, query)),
        };
        let asked = (self.listing.clone(), self.current_dir.clone());
        self.lookup = source
            .and_then(|source| {
                reading::list(
                    source,
                    self.accessible_roots.clone(),
                    SEARCH_LIMIT,
                    self.waker.clone(),
                )
            })
            .map(|pending| Lookup { asked, pending });
        self.relist();
    }

    /// List somewhere new: nothing is listed until its rows are in.
    fn list_anew(&mut self) {
        self.read.clear();
        self.then_select = None;
        self.refresh_entries();
    }

    /// Sort, narrow and label what is listed.
    fn show_listed(&mut self) {
        self.sort_listed();
        self.narrow();
        self.rebuild_crumbs();
        self.ensure_valid_focus();
    }

    /// Folders first, then files, each in the column's order.
    fn sort_listed(&mut self) {
        let (column, ascending) = (self.sort, self.ascending);
        // Search results and Recent come in the order found or chosen,
        // until the person sorts by a column.
        let keep_order = self.listing != Listing::Folder && column == FileColumn::Name && ascending;
        let by_column = |a: &FilePickerEntry, b: &FilePickerEntry| {
            let order = match column {
                FileColumn::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                FileColumn::Size => a.size.cmp(&b.size),
                FileColumn::Modified => a.modified.cmp(&b.modified),
            };
            if ascending { order } else { order.reverse() }
        };
        self.listed
            .compact_sort_by(|a, b| match (a.is_dir, b.is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ if keep_order => std::cmp::Ordering::Equal,
                _ => by_column(a, b),
            });
    }

    /// Show the listing narrowed to what the search field holds (all of it
    /// when the field is empty), and say how many of how many in the field.
    fn narrow(&mut self) {
        let query = self.query.value().trim().to_lowercase();
        let narrowing = !query.is_empty() && !matches!(self.listing, Listing::Search(_));
        self.entries = if narrowing {
            self.listed
                .iter()
                .filter(|entry| entry.name.to_lowercase().contains(&query))
                .cloned()
                .collect()
        } else {
            self.listed.clone()
        };
        self.query.set_count(narrowing.then(|| {
            crate::widget::search_field::FilterCount::new(self.entries.len(), self.listed.len())
        }));
        let len = self.entries.len();
        let kept: Vec<usize> = self
            .selection
            .selected()
            .into_iter()
            .filter(|&i| i < len)
            .collect();
        if kept.len() != self.selection.selected().len() {
            self.selection.clear();
        }
        self.rows_dirty = true;
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        self.selection
            .selected()
            .into_iter()
            .filter_map(|i| self.entries.get(i).map(|e| e.path.clone()))
            .collect()
    }

    /// Select the rows at `paths` again, after the rows moved.
    fn reselect(&mut self, paths: &[PathBuf]) {
        self.selection.clear();
        for path in paths {
            if let Some(i) = self.entries.iter().position(|e| &e.path == path) {
                self.selection.apply_click(i, true, false);
            }
        }
    }

    /// Sort by `column`: a second press on the same column turns it round.
    fn sort_by(&mut self, column: FileColumn) {
        if self.sort == column {
            self.ascending = !self.ascending;
        } else {
            self.sort = column;
            self.ascending = true;
        }
        let selected = self.selected_paths();
        self.sort_listed();
        self.narrow();
        self.reselect(&selected);
    }

    /// The path bar's trail for the place in view: each ancestor the person
    /// may enter, Home named as Home; Recent and a search are one segment.
    fn rebuild_crumbs(&mut self) {
        let (labels, targets, icon) = match &self.listing {
            Listing::Recent => (
                vec![tr("widget-filepicker-place-recent")],
                vec![PathBuf::new()],
                Some(Icon::History),
            ),
            Listing::Search(_) => (
                vec![tr("widget-filepicker-search-results")],
                vec![PathBuf::new()],
                Some(Icon::Search),
            ),
            Listing::Folder => {
                let mut labels = Vec::new();
                let mut targets = Vec::new();
                let mut acc = PathBuf::new();
                for component in self.current_dir.components() {
                    acc.push(component.as_os_str());
                    if !self.can_navigate_to(&acc) {
                        continue;
                    }
                    // Above home only the folders a person thinks in: home
                    // is where the trail starts when the folder is in it.
                    if let Some(home) = self.home.as_deref()
                        && self.current_dir.starts_with(home)
                        && home.starts_with(&acc)
                        && acc != home
                    {
                        continue;
                    }
                    let label = if self.home.as_deref() == Some(acc.as_path()) {
                        tr("widget-filepicker-place-home")
                    } else {
                        let name = component.as_os_str().to_string_lossy().into_owned();
                        if name.is_empty() || name == "/" {
                            "/".to_owned()
                        } else {
                            name
                        }
                    };
                    labels.push(label);
                    targets.push(acc.clone());
                }
                let icon = targets
                    .first()
                    .filter(|first| self.home.as_deref() == Some(first.as_path()))
                    .map(|_| Icon::Home);
                (labels, targets, icon)
            }
        };
        if labels == self.crumbs.segments() && targets == self.crumb_targets {
            return;
        }
        let clicked = Rc::clone(&self.crumb_clicked);
        let edit = Rc::clone(&self.crumb_edit);
        let mut crumbs = Breadcrumb::new(labels)
            .with_role(TextRole::Body)
            .on_navigate(move |index| clicked.set(Some(index)))
            .on_edit_requested(move || edit.set(true));
        if let Some(icon) = icon {
            crumbs = crumbs.root_icon(icon);
        }
        self.crumbs = crumbs;
        self.crumb_targets = targets;
    }

    /// What the path bar shows as text while it is a field: the folder in
    /// view, from home as `~/…`.
    fn location_text(&self) -> String {
        if let Some(home) = self.home.as_deref()
            && let Ok(rest) = self.current_dir.strip_prefix(home)
        {
            if rest.as_os_str().is_empty() {
                return "~/".to_owned();
            }
            return format!("~/{}/", rest.display());
        }
        format!(
            "{}/",
            self.current_dir.display().to_string().trim_end_matches('/')
        )
    }

    /// Turn the path bar into a field holding the folder in view, all of it
    /// selected, so typing replaces it.
    pub fn edit_location(&mut self) {
        let mut field = TextInput::local()
            .with_kind(crate::widget::TextInputKind::Name)
            .with_plain_chrome()
            .with_accessible_name(tr("widget-filepicker-location"));
        field.set_text(&self.location_text());
        field.select_all();
        self.location = Some(field);
        self.location_invalid = false;
        self.focus = Some(PickerFocus::Location);
        self.has_keyboard_focus = true;
        self.sync_text_focus();
        self.apply_layout_helper();
    }

    /// Put the trail back.
    fn stop_editing_location(&mut self) {
        self.location = None;
        self.location_invalid = false;
        self.sync_text_focus();
    }

    /// Go where the typed path says, once the worker has checked what it
    /// really is (`Self::go_to_location`).
    pub fn commit_location(&mut self) {
        if self.check.is_some() {
            return;
        }
        let Some(typed) = self.location.as_ref().map(|f| f.value().trim().to_owned()) else {
            return;
        };
        let Some(target) = self.typed_path(&typed) else {
            self.location_invalid = true;
            return;
        };
        self.ask(Intent::Location(target.clone()), vec![target]);
    }

    /// Go to the typed `target`, now that it is known what it really is:
    /// into a folder, or, for a file, into its folder with the file
    /// selected. A path the picker may not go to keeps the field up,
    /// marked.
    fn go_to_location(&mut self, target: PathBuf, real: Real) {
        if self.location.is_none() {
            return;
        }
        if !(real.exists_inside() && self.can_navigate_to(&target)) {
            self.location_invalid = true;
            return;
        }
        if real.kind == Kind::Folder {
            self.stop_editing_location();
            self.navigate_to(&target);
            self.focus_control(PickerFocus::List);
            return;
        }
        let Some(folder) = target.parent().map(Path::to_path_buf) else {
            self.location_invalid = true;
            return;
        };
        self.stop_editing_location();
        self.navigate_to(&folder);
        self.then_select = Some(target);
        self.focus_control(if self.mode == FilePickerMode::Save {
            PickerFocus::Filename
        } else {
            PickerFocus::List
        });
    }

    /// A typed path, lexically: from home (`~/`), absolute, or relative to
    /// the folder in view. The real path is checked by the caller.
    fn typed_path(&self, typed: &str) -> Option<PathBuf> {
        use std::path::Component;
        if typed.is_empty() {
            return None;
        }
        let expanded = match typed.strip_prefix('~') {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => {
                self.home.as_ref()?.join(rest.trim_start_matches('/'))
            }
            _ => PathBuf::from(typed),
        };
        let mut path = if expanded.is_absolute() {
            PathBuf::new()
        } else {
            self.current_dir.clone()
        };
        for component in expanded.components() {
            match component {
                Component::RootDir | Component::Prefix(_) => path.push(component),
                Component::CurDir => {}
                Component::ParentDir => {
                    path.pop();
                }
                Component::Normal(part) => path.push(part),
            }
        }
        Some(path)
    }

    /// The heading names the app and what it wants, or gives the app's own
    /// purpose; then the app's name sits under it.
    fn has_subtitle(&self) -> bool {
        self.requester.is_some() && !self.title.trim().is_empty()
    }

    /// The heading: the app's own purpose when it gave one, else what the
    /// app wants ("TOTP wants to open a file").
    fn heading(&self) -> String {
        let purpose = self.title.trim();
        if !purpose.is_empty() {
            return purpose.to_owned();
        }
        let (wants, plain) = match self.mode {
            FilePickerMode::Open if self.multiple => (
                "widget-filepicker-wants-open-files",
                "widget-filepicker-title-open-files",
            ),
            FilePickerMode::Open => (
                "widget-filepicker-wants-open-file",
                "widget-filepicker-title-open-file",
            ),
            FilePickerMode::Save => (
                "widget-filepicker-wants-save-file",
                "widget-filepicker-title-save-file",
            ),
            FilePickerMode::Directory => (
                "widget-filepicker-wants-choose-folder",
                "widget-filepicker-title-choose-folder",
            ),
        };
        match &self.requester {
            Some(requester) => {
                crate::i18n::widget_format(wants, &[("app", requester.name.clone().into())])
            }
            None => tr(plain),
        }
    }

    /// The heading's band: the app's icon beside the heading (and the app's
    /// name under it when the heading is the app's own purpose).
    fn heading_h(&self) -> f64 {
        let m = self.metrics;
        let text = m.title_h
            + if self.has_subtitle() {
                m.subtitle_h
            } else {
                0.0
            };
        if self.requester.is_some() {
            text.max(m.app_icon)
        } else {
            text
        }
    }

    /// The heading, then the toolbar, in the dialog's top band.
    fn header_h(&self) -> f64 {
        let m = self.metrics;
        m.pad + self.heading_h() + m.gap + m.control_h + m.section
    }

    /// The Save name row (when there is one) and the button row.
    fn footer_h(&self) -> f64 {
        let m = self.metrics;
        let buttons = 2.0 * m.pad + m.control_h;
        if self.mode == FilePickerMode::Save {
            let error = if self.name_error.is_some() {
                m.error_h
            } else {
                0.0
            };
            buttons + m.control_h + m.section + error
        } else {
            buttons
        }
    }

    /// Whether the places sidebar is drawn: there is somewhere to go and the
    /// dialog is wide enough to keep a readable list beside it.
    fn has_sidebar(&self) -> bool {
        !self.places.is_empty() && self.rect.width() >= self.metrics.sidebar_min_dialog_w
    }

    fn sidebar_rect(&self) -> Rect {
        let width = if self.has_sidebar() {
            self.metrics.sidebar_w
        } else {
            0.0
        };
        Rect::new(
            self.rect.x0,
            self.rect.y0 + self.header_h(),
            self.rect.x0 + width,
            self.rect.y1 - self.footer_h(),
        )
    }

    fn content_x0(&self) -> f64 {
        if self.has_sidebar() {
            self.sidebar_rect().x1 + self.metrics.pad
        } else {
            self.rect.x0 + self.metrics.pad
        }
    }

    /// The file list, with its column header, beside the sidebar.
    fn list_rect(&self) -> Rect {
        let top = self.rect.y0 + self.header_h();
        Rect::new(
            self.content_x0(),
            top,
            (self.rect.x1 - self.metrics.pad).max(self.content_x0()),
            (self.rect.y1 - self.footer_h() - self.metrics.gap).max(top),
        )
    }

    /// The toolbar row: Up, the path bar and the search field.
    fn toolbar_y0(&self) -> f64 {
        let m = self.metrics;
        self.rect.y0 + m.pad + self.heading_h() + m.gap
    }

    /// The path bar: the trail, or the field typing a path, in a well
    /// between Up and the search field.
    fn path_rect(&self) -> Rect {
        let m = self.metrics;
        let y0 = self.toolbar_y0();
        Rect::new(
            self.rect.x0 + m.pad + m.control_h + m.gap,
            y0,
            (self.search_rect.x0 - m.gap).max(self.rect.x0 + m.pad + m.control_h + m.gap),
            y0 + m.control_h,
        )
    }

    /// The trail or field inside the path bar's well.
    fn path_inner(&self) -> Rect {
        let bar = self.path_rect();
        let inset = self.metrics.inset_x;
        Rect::new(
            bar.x0 + inset,
            bar.y0,
            (bar.x1 - inset).max(bar.x0 + inset),
            bar.y1,
        )
    }

    /// Scroll the list so the row of entry `i` shows.
    fn scroll_to(&mut self, i: usize) {
        self.sync_table();
        self.table.reveal_row(i);
        self.place_table();
    }

    /// Hand the table the rows, when they changed, and the selection.
    fn sync_table(&mut self) {
        if self.rows_dirty {
            self.rows_dirty = false;
            let elsewhere = self.listing != Listing::Folder;
            let rows = self
                .entries
                .iter()
                .map(|entry| PickerRow {
                    location: elsewhere.then(|| self.folder_label(&entry.path)),
                    entry: entry.clone(),
                })
                .collect();
            self.table.sync(FileTableInputs {
                rows,
                selection: &self.selection,
                sort: self.sort,
                ascending: self.ascending,
                metrics: FileTableMetrics::of(&self.theme, 1.0),
                theme: &self.theme,
            });
        } else {
            self.table.select(&self.selection);
        }
    }

    /// Lay the table out in the list's rect and read back where its rows
    /// are. A width that wants other columns builds them and lays out again.
    fn place_table(&mut self) {
        let list = self.list_rect();
        if self.table.place(list, &self.theme) {
            self.rows_dirty = true;
            self.sync_table();
            self.table.place(list, &self.theme);
        }
        self.entry_rows = self.table.row_rects();
    }

    /// The folder a row from elsewhere is in, as the sidebar would name it.
    fn folder_label(&self, path: &Path) -> String {
        let Some(folder) = path.parent() else {
            return String::new();
        };
        if self.home.as_deref() == Some(folder) {
            return tr("widget-filepicker-place-home");
        }
        folder.file_name().map_or_else(
            || folder.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    /// Put a part that lays itself out on a tree into `rect`, on a tree of
    /// its own: the picker places its parts by hand.
    fn place_part(part: &mut dyn Widget, rect: Rect, theme: &Theme) {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut nodes = HashMap::new();
        let node = part.build_layout(&mut taffy, theme, &mut nodes);
        let mut style = taffy.style(node).cloned().unwrap_or_default();
        style.size = Size {
            width: taffy::Dimension::length(px(rect.width())),
            height: taffy::Dimension::length(px(rect.height())),
        };
        style.min_size = Size::auto();
        let _ = taffy.set_style(node, style);
        if taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(px(rect.width())),
                    height: taffy::AvailableSpace::Definite(px(rect.height())),
                },
            )
            .is_ok()
        {
            part.apply_layout(&taffy, node, rect.origin());
        }
    }

    pub fn apply_layout_helper(&mut self) {
        let m = self.metrics;
        let footer_top = self.rect.y1 - self.footer_h();

        // Buttons: Cancel then the primary action, right-aligned, sized to
        // their labels.
        let row_y1 = self.rect.y1 - m.pad;
        let row_y0 = row_y1 - m.control_h;
        let open_x1 = self.rect.x1 - m.pad;
        let open_x0 = open_x1 - self.button_w[1];
        let cancel_x1 = open_x0 - m.gap;
        let cancel_x0 = cancel_x1 - self.button_w[0];
        self.open_rect = Rect::new(open_x0, row_y0, open_x1, row_y1);
        self.cancel_rect = Rect::new(cancel_x0, row_y0, cancel_x1, row_y1);
        let filter_x0 = self.rect.x0 + m.pad + self.filter_label_w + m.gap;
        self.filter_rect = Rect::new(
            filter_x0,
            row_y0,
            (filter_x0 + self.filter_w).min((cancel_x0 - m.gap).max(filter_x0)),
            row_y1,
        );
        self.layout_filter_menu();

        if self.mode == FilePickerMode::Save {
            let field_y0 = footer_top + m.pad;
            self.filename_rect = Rect::new(
                self.rect.x0 + m.pad + m.name_label_w,
                field_y0,
                self.rect.x1 - m.pad,
                field_y0 + m.control_h,
            );
            self.filename.apply_layout_external(self.filename_rect);
        } else {
            self.filename_rect = Rect::ZERO;
        }

        let toolbar_y0 = self.toolbar_y0();
        let up_x0 = self.rect.x0 + m.pad;
        self.up_rect = Rect::new(
            up_x0,
            toolbar_y0,
            up_x0 + m.control_h,
            toolbar_y0 + m.control_h,
        );
        let search_w = m.search_w.min((self.rect.width() * 0.32).max(0.0));
        self.search_rect = Rect::new(
            self.rect.x1 - m.pad - search_w,
            toolbar_y0,
            self.rect.x1 - m.pad,
            toolbar_y0 + m.control_h,
        );
        self.query.apply_layout_external(self.search_rect);
        let inner = self.path_inner();
        match self.location.as_mut() {
            Some(field) => field.apply_layout_external(inner),
            None => Self::place_part(&mut self.crumbs, inner, &self.theme),
        }
        self.layout_places();

        self.sync_table();
        self.place_table();
        self.layout_replace_prompt();
        self.ensure_valid_focus();
    }

    /// One sidebar row per place, top to bottom, as many as fit.
    fn layout_places(&mut self) {
        self.place_rects.clear();
        if !self.has_sidebar() {
            return;
        }
        let m = self.metrics;
        let side = self.sidebar_rect();
        let mut y = side.y0 + m.gap;
        for _ in &self.places {
            if y + m.row_h > side.y1 {
                break;
            }
            self.place_rects
                .push(Rect::new(side.x0 + m.gap, y, side.x1 - m.gap, y + m.row_h));
            y += m.row_h;
        }
    }

    /// The type menu opens upward from its chip: one row per filter.
    fn layout_filter_menu(&mut self) {
        let m = self.metrics;
        self.filter_rows.clear();
        let pad = m.gap / 2.0;
        let width = self.filter_rect.width().max(m.filter_w);
        #[allow(
            clippy::cast_precision_loss,
            reason = "a filter list is a handful of entries"
        )]
        let height = self.filters.len() as f64 * m.row_h + 2.0 * pad;
        let x0 = self.filter_rect.x0;
        let y1 = self.filter_rect.y0 - pad;
        self.filter_menu_rect = Rect::new(x0, y1 - height, x0 + width, y1);
        let mut y = self.filter_menu_rect.y0 + pad;
        for _ in &self.filters {
            self.filter_rows
                .push(Rect::new(x0 + pad, y, x0 + width - pad, y + m.row_h));
            y += m.row_h;
        }
    }

    /// The replace prompt: a card centred on the picker, its buttons in the
    /// footer's order (Cancel, then the action) at its bottom edge.
    fn layout_replace_prompt(&mut self) {
        let m = self.metrics;
        let w = (m.icon_lg * 20.0)
            .min(self.rect.width() - 2.0 * m.pad)
            .max(0.0);
        let body_lines = 2.0 * m.row_h * 0.75;
        let h = (2.0 * m.pad + m.title_h + m.gap + body_lines + m.section + m.control_h)
            .min(self.rect.height())
            .max(0.0);
        let x0 = self.rect.x0 + (self.rect.width() - w) / 2.0;
        let y0 = self.rect.y0 + (self.rect.height() - h) / 2.0;
        self.replace_card_rect = Rect::new(x0, y0, x0 + w, y0 + h);
        let by1 = self.replace_card_rect.y1 - m.pad;
        let by0 = by1 - m.control_h;
        let replace_x1 = self.replace_card_rect.x1 - m.pad;
        let replace_x0 = replace_x1 - self.button_w[2];
        let keep_x1 = replace_x0 - m.gap;
        self.replace_rect = Rect::new(replace_x0, by0, replace_x1, by1);
        self.keep_existing_rect = Rect::new(keep_x1 - self.button_w[0], by0, keep_x1, by1);
    }

    /// Native dialog focus order, kept in one place so keyboard, AT focus
    /// and paint cannot drift.  The list is one focus target: arrow keys move
    /// its selected row, exactly like a native listbox, rather than making a
    /// long directory consume every Tab press.
    fn focus_targets(&self) -> Vec<PickerFocus> {
        if self.replace_prompt.is_some() {
            return vec![PickerFocus::KeepExisting, PickerFocus::Replace];
        }
        let mut targets = Vec::with_capacity(9);
        if self.can_go_up() {
            targets.push(PickerFocus::Up);
        }
        targets.extend([PickerFocus::Location, PickerFocus::Search]);
        if self.has_sidebar() {
            targets.push(PickerFocus::Places);
        }
        // The list holds focus while its rows are being read, so the
        // chooser opens on it rather than moving there when they arrive.
        if !self.entries.is_empty() || self.looking_up() {
            targets.push(PickerFocus::List);
        }
        if self.shows_filter() {
            targets.push(PickerFocus::Filter);
        }
        if self.mode == FilePickerMode::Save {
            targets.push(PickerFocus::Filename);
        }
        targets.push(PickerFocus::Cancel);
        if self.can_commit_open() {
            targets.push(PickerFocus::Confirm);
        }
        targets
    }

    /// The type menu is there only when the app named more than one type.
    fn shows_filter(&self) -> bool {
        self.mode != FilePickerMode::Directory && self.filters.len() > 1
    }

    fn focus_trap(&self) -> crate::overlay::FocusTrap {
        crate::overlay::FocusTrap::new(
            self.focus_targets()
                .into_iter()
                .map(PickerFocus::id)
                .collect(),
        )
    }

    fn ensure_valid_focus(&mut self) {
        let targets = self.focus_targets();
        if self.focus.is_none_or(|focused| !targets.contains(&focused)) {
            // Save dialogs start where text entry is expected; Open dialogs
            // start on the listbox so arrows and typing work immediately.
            self.focus = if self.mode == FilePickerMode::Save {
                targets
                    .iter()
                    .copied()
                    .find(|target| *target == PickerFocus::Filename)
                    .or_else(|| targets.first().copied())
            } else {
                targets
                    .iter()
                    .copied()
                    .find(|target| *target == PickerFocus::List)
                    .or_else(|| targets.iter().copied().find(|t| *t == PickerFocus::Search))
                    .or_else(|| targets.first().copied())
            };
        }
        if self.focus != Some(PickerFocus::Location) && self.location.is_some() {
            self.location = None;
            self.location_invalid = false;
        }
    }

    fn move_focus(&mut self, backwards: bool) {
        self.ensure_valid_focus();
        let Some(current) = self.focus else {
            return;
        };
        let trap = self.focus_trap();
        let next = if backwards {
            trap.prev_from(current.id())
        } else {
            trap.next_from(current.id())
        };
        self.focus = next.and_then(PickerFocus::from_id);
        self.ensure_valid_focus();
    }

    fn focus_control(&mut self, focus: PickerFocus) {
        if self.focus_targets().contains(&focus) {
            self.focus = Some(focus);
            self.has_keyboard_focus = true;
        }
        self.ensure_valid_focus();
        self.sync_text_focus();
    }

    /// Each text field holds the caret exactly while it is the focused
    /// control of a focused picker.
    fn sync_text_focus(&mut self) {
        let on = |control| self.has_keyboard_focus && self.focus == Some(control);
        let (filename, search, location) = (
            on(PickerFocus::Filename),
            on(PickerFocus::Search),
            on(PickerFocus::Location),
        );
        if self.filename.is_focused() != filename {
            Widget::set_focus(&mut self.filename, filename);
        }
        self.query.set_focused(search);
        if let Some(field) = self.location.as_mut()
            && field.is_focused() != location
        {
            Widget::set_focus(field, location);
        }
    }

    /// The text field `event` is typing into, if it is typing rather than
    /// the picker's own navigation (Tab, Escape, Enter).
    fn typing_into(&self, event: &WindowEvent) -> Option<PickerFocus> {
        use winit::keyboard::{Key, NamedKey};
        if self.replace_prompt.is_some() {
            return None;
        }
        let field = match self.focus {
            Some(PickerFocus::Filename) if self.mode == FilePickerMode::Save => {
                PickerFocus::Filename
            }
            Some(PickerFocus::Search) => PickerFocus::Search,
            Some(PickerFocus::Location) if self.location.is_some() => PickerFocus::Location,
            _ => return None,
        };
        let typing = match event {
            WindowEvent::Ime(_) => true,
            WindowEvent::KeyboardInput { event, .. } => {
                let navigation = matches!(
                    event.logical_key,
                    Key::Named(NamedKey::Tab | NamedKey::Escape | NamedKey::Enter)
                );
                // Down from the search field goes to the list it narrowed.
                let into_list = field == PickerFocus::Search
                    && matches!(event.logical_key, Key::Named(NamedKey::ArrowDown));
                !navigation && !into_list
            }
            _ => false,
        };
        typing.then_some(field)
    }

    /// The text field holding the caret, if one does: what an input method
    /// composes into.
    fn focused_text(&self) -> Option<&dyn Widget> {
        if !self.has_keyboard_focus {
            return None;
        }
        match self.focus {
            Some(PickerFocus::Filename) if self.mode == FilePickerMode::Save => {
                Some(&self.filename)
            }
            Some(PickerFocus::Search) => Some(&self.query),
            Some(PickerFocus::Location) => self.location.as_ref().map(|f| f as &dyn Widget),
            _ => None,
        }
    }

    fn can_commit_open(&self) -> bool {
        match self.mode {
            FilePickerMode::Open => self.validated_selected_files().is_some(),
            // A typed name is always worth trying: one that can't be used
            // is explained under the field, never silently refused.
            FilePickerMode::Save => {
                !self.filename().trim().is_empty() || self.first_selected_file().is_some()
            }
            FilePickerMode::Directory => self.validated_directory_choice().is_some(),
        }
    }

    /// Every visible control belongs in the semantic tree.  Unlike
    /// `focus_targets`, this includes a disabled confirmation button so AT
    /// can explain why it cannot yet be activated.
    fn a11y_controls(&self) -> Vec<PickerFocus> {
        let mut controls = Vec::with_capacity(9);
        if self.can_go_up() {
            controls.push(PickerFocus::Up);
        }
        controls.extend([PickerFocus::Location, PickerFocus::Search]);
        if self.has_sidebar() {
            controls.push(PickerFocus::Places);
        }
        if !self.entries.is_empty() {
            controls.push(PickerFocus::List);
        }
        if self.shows_filter() {
            controls.push(PickerFocus::Filter);
        }
        if self.mode == FilePickerMode::Save {
            controls.push(PickerFocus::Filename);
        }
        controls.extend([PickerFocus::Cancel, PickerFocus::Confirm]);
        controls
    }

    /// The place the list shows: Recent, or the place the folder in view is
    /// inside, the deepest if several.
    fn active_place(&self) -> Option<usize> {
        match self.listing {
            Listing::Recent => self.places.iter().position(|place| place.recent),
            Listing::Search(_) => None,
            Listing::Folder => self
                .places
                .iter()
                .enumerate()
                .filter(|(_, place)| !place.recent && self.current_dir.starts_with(&place.path))
                .max_by_key(|(_, place)| place.path.components().count())
                .map(|(i, _)| i),
        }
    }

    fn go_place(&mut self, index: usize) {
        let Some(place) = self.places.get(index) else {
            return;
        };
        let (recent, path) = (place.recent, place.path.clone());
        self.place_cursor = Some(index);
        if recent {
            self.show_recent();
        } else {
            self.navigate_to(path);
        }
    }

    /// Arrow keys in the sidebar move through the places, entering each.
    fn move_place(&mut self, delta: isize) {
        let count = self.places.len();
        if count == 0 {
            return;
        }
        let from = self
            .place_cursor
            .or_else(|| self.active_place())
            .unwrap_or(0);
        let next = from
            .saturating_add_signed(delta)
            .min(count.saturating_sub(1));
        self.go_place(next);
    }

    /// Choose filter `index` from the type menu.
    pub fn set_filter(&mut self, index: usize) {
        if index < self.filters.len() && index != self.active_filter {
            self.active_filter = index;
            self.selection.clear();
            self.table.reset_scroll();
            self.relist();
        }
    }

    fn toggle_filter_menu(&mut self) {
        self.filter_open = !self.filter_open && self.filters.len() > 1;
        self.filter_hover = self.filter_open.then_some(self.active_filter);
    }

    /// Keys while the type menu is open: it owns the keyboard until it closes.
    fn on_filter_menu_key(&mut self, key: &winit::keyboard::Key) {
        use winit::keyboard::{Key, NamedKey};
        let last = self.filters.len().saturating_sub(1);
        let at = self.filter_hover.unwrap_or(self.active_filter);
        match key {
            Key::Named(NamedKey::ArrowDown) => {
                self.filter_hover = Some(at.saturating_add(1).min(last))
            }
            Key::Named(NamedKey::ArrowUp) => self.filter_hover = Some(at.saturating_sub(1)),
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                self.set_filter(at);
                self.filter_open = false;
            }
            Key::Named(NamedKey::Escape | NamedKey::Tab) => self.filter_open = false,
            _ => {}
        }
    }

    fn activate_focus(&mut self) -> bool {
        match self.focus {
            Some(PickerFocus::Cancel) => {
                self.click_cancel();
                true
            }
            Some(PickerFocus::Confirm | PickerFocus::Filename) if self.can_commit_open() => {
                self.click_open();
                true
            }
            Some(PickerFocus::Up) => {
                self.go_up();
                true
            }
            Some(PickerFocus::Location) => {
                if self.location.is_some() {
                    self.commit_location();
                } else {
                    self.edit_location();
                }
                true
            }
            Some(PickerFocus::Search) => {
                let query = self.query.value().to_owned();
                self.search_for(&query);
                true
            }
            Some(PickerFocus::Filter) => {
                self.cycle_filter();
                true
            }
            Some(PickerFocus::Places) => {
                if let Some(i) = self.place_cursor.or_else(|| self.active_place()) {
                    self.go_place(i);
                }
                true
            }
            Some(PickerFocus::List) => {
                self.activate_selected_entry();
                true
            }
            Some(PickerFocus::KeepExisting) => {
                self.keep_existing();
                true
            }
            Some(PickerFocus::Replace) => {
                self.confirm_replace();
                true
            }
            _ => false,
        }
    }

    /// Measure what has no size until text is shaped — the button labels
    /// and the type menu's widest entry — with the renderer the frame draws
    /// with, and re-lay the picker out from those widths. Hit rects
    /// therefore match the painted glyphs, and stay matched when an event
    /// handler rebuilds the layout without a renderer. Runs once per paint.
    fn measure_text(&mut self, text_renderer: &mut TextRenderer, theme: &Theme) {
        let m = self.metrics;
        let mut changed = false;
        let mut face = |label: &str, primary: bool| {
            footer_button(Rect::ZERO, label, primary).intrinsic_width(text_renderer, theme)
        };
        let widths = [
            face(CANCEL_KEY, false),
            face(&self.confirm_key(), true),
            face(REPLACE_KEY, false),
        ]
        .map(|w| w.max(m.min_button_w));
        #[allow(
            clippy::float_cmp,
            reason = "cache key: relayout only when a measured width actually changed"
        )]
        let widths_changed = widths != self.button_w;
        if widths_changed {
            self.button_w = widths;
            changed = true;
        }
        let labels: Vec<String> = self
            .filters
            .iter()
            .map(|f| self.filter_display(f))
            .collect();
        let active = labels.get(self.active_filter).cloned().unwrap_or_default();
        let filter_w = crate::widget::DropdownFace::new(Rect::ZERO, &active)
            .intrinsic_width(labels.iter().map(String::as_str), text_renderer, theme)
            .max(m.min_button_w);
        if (filter_w - self.filter_w).abs() > f64::EPSILON {
            self.filter_w = filter_w;
            changed = true;
        }
        let (label_w, _) = text_renderer.measure_style(
            &tr("widget-filepicker-file-type"),
            None,
            &theme.text(TextRole::Label),
        );
        let label_w = f64::from(label_w).ceil();
        if (label_w - self.filter_label_w).abs() > f64::EPSILON {
            self.filter_label_w = label_w;
            changed = true;
        }
        if changed {
            self.apply_layout_helper();
        }
    }

    /// The translation key (or the app's own wording) of the confirm button.
    fn confirm_key(&self) -> String {
        self.accept_label.clone().unwrap_or_else(|| {
            match self.mode {
                FilePickerMode::Open => "widget-filepicker-open",
                FilePickerMode::Save => "widget-filepicker-save",
                FilePickerMode::Directory => "widget-filepicker-choose-folder",
            }
            .to_string()
        })
    }

    /// A filter's name as shown: the built-in "All files" follows the
    /// desktop's language, a caller's own label is shown as given.
    fn filter_display(&self, filter: &FileFilter) -> String {
        if filter.extensions.is_empty() && filter.label == ALL_FILES_LABEL {
            tr("widget-filepicker-all-files")
        } else {
            filter.label.clone()
        }
    }

    /// Adopt `theme`'s insets; re-lay the picker out when they moved.
    fn adopt_theme(&mut self, theme: &Theme) {
        let metrics = PickerMetrics::from_theme(theme);
        #[allow(
            clippy::float_cmp,
            reason = "cache key: the text scale either changed or it did not"
        )]
        let rescaled = theme.text_scale != self.theme.text_scale;
        if metrics != self.metrics || rescaled {
            self.metrics = metrics;
            self.theme = theme.clone();
            self.rows_dirty = true;
            self.apply_layout_helper();
        }
    }

    /// Confirm: what is about to be given (the selection, or the typed
    /// Save name's folder and file) is checked again on the worker, and
    /// the chooser acts on the answer ([`Self::act`]).
    fn commit_open(&mut self) {
        if self.check.is_some() {
            return;
        }
        match self.mode {
            FilePickerMode::Save => {
                let name = self.filename().trim().to_owned();
                if name.is_empty() {
                    if let Some(target) = self.first_selected_file() {
                        self.ask(Intent::SaveSelected(target.clone()), vec![target]);
                    }
                    return;
                }
                match self.resolve_save_name(&name) {
                    Ok(SaveName::Folder(folder)) => {
                        let paths = vec![folder.clone()];
                        self.ask(Intent::SaveName(SaveName::Folder(folder)), paths);
                    }
                    Ok(SaveName::File { folder, name }) => {
                        let paths = vec![folder.clone(), folder.join(&name)];
                        self.ask(Intent::SaveName(SaveName::File { folder, name }), paths);
                    }
                    Err(error) => self.show_name_error(&error),
                }
            }
            FilePickerMode::Open => {
                if let Some(paths) = self.validated_selected_files() {
                    self.ask(Intent::Open(paths.clone()), paths);
                }
            }
            FilePickerMode::Directory => {
                if let Some(paths) = self.validated_directory_choice() {
                    self.ask(Intent::Directory(paths.clone()), paths);
                }
            }
        }
    }

    /// Check `paths` on the worker for `intent`.
    fn ask(&mut self, intent: Intent, paths: Vec<PathBuf>) {
        self.check = reading::check(paths, self.accessible_roots.clone(), self.waker.clone())
            .map(|pending| Check { intent, pending });
    }

    /// Do what a check was for, now that the worker says what each path
    /// really is. A path that vanished, changed kind or turned into a link
    /// out of the roots since it was listed is refused.
    fn act(&mut self, intent: Intent, reals: &[Real]) {
        if self.pending_outcome.is_some() {
            // Cancelled while the check was out: nothing more to do.
            return;
        }
        let real = |i: usize| reals.get(i).copied().unwrap_or_default();
        match intent {
            Intent::Open(paths) => {
                if (0..paths.len()).all(|i| real(i).is_file_inside()) {
                    self.pending_outcome = Some(FilePickerOutcome::Selected(paths));
                }
            }
            Intent::Directory(paths) => {
                if (0..paths.len()).all(|i| real(i).is_folder_inside()) {
                    self.pending_outcome = Some(FilePickerOutcome::Selected(paths));
                }
            }
            Intent::SaveSelected(target) => self.save_to(target, real(0)),
            Intent::SaveName(SaveName::Folder(folder)) => {
                match self.save_folder_allowed(&folder, real(0)) {
                    Ok(()) => {
                        self.navigate_to(&folder);
                        self.set_filename("");
                        self.focus_control(PickerFocus::Filename);
                    }
                    Err(error) => self.show_name_error(&error),
                }
            }
            Intent::SaveName(SaveName::File { folder, name }) => {
                if let Err(error) = self.save_folder_allowed(&folder, real(0)) {
                    self.show_name_error(&error);
                    return;
                }
                if folder != self.current_dir {
                    // Show where the file goes, so a replace prompt (or the
                    // next Save) is about the folder in view.
                    self.navigate_to(&folder);
                    self.set_filename(&name);
                }
                self.save_to(folder.join(&name), real(1));
            }
            Intent::Location(target) => self.go_to_location(target, real(0)),
        }
    }

    /// A Save name's folder must exist and really lie beneath one of the
    /// chooser's roots, so neither `..` nor a link can take a save outside
    /// what the caller may write.
    fn save_folder_allowed(&self, folder: &Path, real: Real) -> Result<(), SaveNameError> {
        if real.kind != Kind::Folder {
            Err(SaveNameError::NoFolder(folder.to_path_buf()))
        } else if real.inside && self.can_navigate_to(folder) {
            Ok(())
        } else {
            Err(SaveNameError::NotAllowed(folder.to_path_buf()))
        }
    }

    /// Save to `target` — unless something is already there. A folder is
    /// opened, as typing a folder's name does in every save dialog; an
    /// existing file is replaced only once the person says so.
    fn save_to(&mut self, target: PathBuf, real: Real) {
        match real.kind {
            Kind::Folder => {
                if self.can_navigate_to(&target) {
                    self.navigate_to(&target);
                    self.set_filename("");
                    self.focus_control(PickerFocus::Filename);
                }
            }
            // A dangling link is still a name taken in this folder.
            Kind::File | Kind::Other | Kind::Dangling => {
                self.replace_prompt = Some(target);
                self.hover_prompt = None;
                // Cancel holds focus: Enter on its own never destroys a file.
                self.focus = Some(PickerFocus::KeepExisting);
                self.has_keyboard_focus = true;
                self.sync_text_focus();
                self.apply_layout_helper();
            }
            Kind::Nothing => {
                self.pending_outcome = Some(FilePickerOutcome::Selected(vec![target]));
            }
        }
    }

    /// Resolve a typed Save name, lexically: a plain name in the folder in
    /// view, or a path to another folder — relative to this one, from home
    /// (`~/`) or absolute — with the file's name as its last part. A name
    /// ending in `/` is a folder to open. Whether the folder exists and may
    /// be written is checked on confirming ([`Self::save_folder_allowed`]).
    fn resolve_save_name(&self, typed: &str) -> Result<SaveName, SaveNameError> {
        use std::path::Component;
        let wants_folder = typed.ends_with('/');
        let expanded = match typed.strip_prefix('~') {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => {
                let home = self.home.as_ref().ok_or(SaveNameError::Invalid)?;
                home.join(rest.trim_start_matches('/'))
            }
            _ => PathBuf::from(typed),
        };
        // Lexical, so the folder shown is the one the person named; the real
        // path is checked against the roots on confirming.
        let mut path = if expanded.is_absolute() {
            PathBuf::new()
        } else {
            self.current_dir.clone()
        };
        for component in expanded.components() {
            match component {
                Component::RootDir | Component::Prefix(_) => path.push(component),
                Component::CurDir => {}
                Component::ParentDir => {
                    path.pop();
                }
                Component::Normal(part) => path.push(part),
            }
        }
        // A folder outside the roots is refused as typed; one inside them
        // is checked for what it really is on confirming.
        if wants_folder {
            return if self.can_navigate_to(&path) {
                Ok(SaveName::Folder(path))
            } else {
                Err(SaveNameError::NotAllowed(path))
            };
        }
        let ends_in_name = matches!(
            expanded.components().next_back(),
            Some(Component::Normal(_))
        );
        let (Some(name), Some(folder), true) = (path.file_name(), path.parent(), ends_in_name)
        else {
            return Err(SaveNameError::Invalid);
        };
        if !self.can_navigate_to(folder) {
            return Err(SaveNameError::NotAllowed(folder.to_path_buf()));
        }
        Ok(SaveName::File {
            folder: folder.to_path_buf(),
            name: name.to_string_lossy().into_owned(),
        })
    }

    fn show_name_error(&mut self, error: &SaveNameError) {
        let folder_name = |folder: &Path| {
            folder.file_name().map_or_else(
                || folder.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
        };
        let message = match error {
            SaveNameError::Invalid => tr("widget-filepicker-name-invalid"),
            SaveNameError::NoFolder(folder) => crate::i18n::widget_format(
                "widget-filepicker-name-no-folder",
                &[("folder", folder_name(folder).into())],
            ),
            SaveNameError::NotAllowed(folder) => crate::i18n::widget_format(
                "widget-filepicker-name-not-allowed",
                &[("folder", folder_name(folder).into())],
            ),
        };
        self.name_error = Some(message);
        self.focus_control(PickerFocus::Filename);
        self.apply_layout_helper();
    }

    /// Why the typed name can't be saved, while it is shown.
    pub fn name_error(&self) -> Option<&str> {
        self.name_error.as_deref()
    }

    /// The first selected file, as listed, if the chooser may give it.
    fn first_selected_file(&self) -> Option<PathBuf> {
        self.selection
            .selected()
            .into_iter()
            .filter_map(|i| self.entries.get(i))
            .find(|e| !e.is_dir)
            .filter(|e| e.real.is_file_inside())
            .map(|e| e.path.clone())
    }

    /// The selected files, as listed, if every one is a file the chooser
    /// may give.
    fn validated_selected_files(&self) -> Option<Vec<PathBuf>> {
        let entries = self
            .selection
            .selected()
            .into_iter()
            .map(|index| self.entries.get(index))
            .collect::<Option<Vec<_>>>()?;
        if entries.is_empty() {
            return None;
        }
        entries
            .into_iter()
            .map(|entry| (!entry.is_dir && entry.real.is_file_inside()).then(|| entry.path.clone()))
            .collect()
    }

    /// The selected folders, as listed, or the folder in view when none is
    /// selected, if each is a folder the chooser may give.
    fn validated_directory_choice(&self) -> Option<Vec<PathBuf>> {
        let selected = self.selection.selected();
        if selected.is_empty() {
            return self
                .here
                .is_some_and(Real::is_folder_inside)
                .then(|| vec![self.current_dir.clone()]);
        }
        selected
            .into_iter()
            .map(|index| {
                let entry = self.entries.get(index)?;
                (entry.is_dir && entry.real.is_folder_inside()).then(|| entry.path.clone())
            })
            .collect()
    }

    /// Whether `entry`, as listed, is something the chooser may give.
    fn within_scope(entry: &FilePickerEntry) -> bool {
        if entry.is_dir {
            entry.real.is_folder_inside()
        } else {
            entry.real.is_file_inside()
        }
    }

    /// Resolve a click on entry `i`: file modes navigate through directories;
    /// directory mode selects on one click and navigates on a double click.
    fn activate_entry(&mut self, i: usize, ctrl: bool, shift: bool) {
        let Some(entry) = self.entries.get(i).cloned() else {
            return;
        };
        if entry.is_dir && self.mode != FilePickerMode::Directory {
            self.navigate_to(&entry.path);
            return;
        }
        let now = crate::time::now();
        let is_double = matches!(
            self.last_click,
            Some((pi, t)) if pi == i && now.duration_since(t) < crate::gesture::multi_click_interval()
        );
        if self.multiple {
            self.selection.apply_click(i, ctrl, shift);
        } else {
            self.selection.select_only(i);
        }
        // In Save mode, clicking a file copies its name into the field.
        if self.mode == FilePickerMode::Save {
            let name = entry.name.clone();
            self.set_filename(&name);
        }
        if is_double && self.mode == FilePickerMode::Directory {
            self.last_click = None;
            self.navigate_to(&entry.path);
        } else if is_double {
            self.last_click = None;
            self.commit_open();
        } else {
            self.last_click = Some((i, now));
        }
    }

    /// Move the keyboard selection by `delta` rows (clamped), extending
    /// the range when `shift` is held.
    fn move_selection(&mut self, delta: isize, shift: bool) {
        let len = self.entries.len();
        if len == 0 {
            return;
        }
        let cur = self
            .selection
            .primary()
            .map_or(-1, |i| isize::try_from(i).unwrap_or(isize::MAX));
        let max_index = isize::try_from(len).unwrap_or(isize::MAX).saturating_sub(1);
        let next = cur.saturating_add(delta).clamp(0, max_index);
        #[allow(
            clippy::cast_sign_loss,
            reason = "next is clamped into 0..=max_index immediately above, so it is never negative"
        )]
        let next = next as usize;
        self.selection.move_to(next, shift && self.multiple);
        self.scroll_to(next);
    }
}

impl Widget for FilePicker {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.adopt_theme(theme);
        let style = Style {
            size: Size::from_percent(1.0, 1.0),
            flex_grow: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.apply_layout_helper();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.adopt_theme(theme);
        self.poll_lookups();
        self.apply_layout_helper();
        self.measure_text(text_renderer, theme);
        // The system sheet: the one floating card, on its opaque ground
        // rather than the translucent panel material, since a file list
        // stands on it; its controls lift off that ground, so a secondary
        // button and a field read the same in light and dark.
        crate::widget::floating_surface::paint_card(scene, transform, self.rect, theme);
        let sheet = theme.on_ground(crate::widget::floating_surface::card_ground(theme));
        let theme = &sheet;

        self.draw_header(scene, transform, text_renderer, theme);
        self.draw_toolbar(scene, transform, text_renderer, theme, presence);
        self.draw_sidebar(scene, transform, text_renderer, theme);
        self.draw_list(scene, transform, text_renderer, theme, presence);
        self.draw_footer(scene, transform, text_renderer, theme, presence);
        self.draw_filter_menu(scene, transform, text_renderer, theme);
        self.draw_replace_prompt(scene, transform, text_renderer, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.replace_prompt.is_some() {
            self.on_replace_prompt_event(event, ctx);
            return;
        }
        // Text typed while the list has the keyboard searches.
        if self.focus == Some(PickerFocus::List)
            && matches!(event, WindowEvent::Ime(winit::event::Ime::Commit(_)))
        {
            self.focus_control(PickerFocus::Search);
        }
        if let Some(field) = self.typing_into(event) {
            self.has_keyboard_focus = true;
            self.sync_text_focus();
            self.type_into(field, event, ctx);
            self.ensure_valid_focus();
            *ctx.handled = true;
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = ctx.logical(position);
                self.on_cursor_moved(p, ctx);
            }
            WindowEvent::MouseWheel { .. } => {
                if self.list_rect().contains(self.last_cursor) {
                    self.table.handle_event(event, ctx);
                    self.entry_rows = self.table.row_rects();
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.on_left_click(event, ctx);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            } => {
                // A drag on the scrollbar or a column's edge ends wherever
                // the pointer is.
                let outcome = self.table.handle_event(event, ctx);
                if let Some(column) = outcome.sort {
                    self.sort_by(column);
                }
                self.entry_rows = self.table.row_rects();
                self.forward_to_fields(event, ctx);
            }
            WindowEvent::KeyboardInput { event: ke, .. } if ke.state == ElementState::Pressed => {
                self.on_key_pressed(ke, event, ctx);
            }
            _ => {}
        }
        self.sync_text_focus();
        if self.mode == FilePickerMode::Save
            && matches!(
                event,
                WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
            )
        {
            // The field places its caret and drags its selection itself; the
            // picker still sees the press to move focus onto the field.
            let mut field_handled = false;
            self.filename.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    handled: &mut field_handled,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
        }
    }

    fn name(&self) -> &'static str {
        "FilePicker"
    }

    /// Reports the modal and every currently visible interactive target. The
    /// same `focus` field drives the painted ring and these semantics.
    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive accessibility projection keeps focus, bounds, and actions aligned"
    )]
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let filename_cursor = self.filename.cursor_pos();
        let filename_anchor =
            self.filename
                .selection_range_public()
                .map_or(filename_cursor, |(start, end)| {
                    if start == filename_cursor { end } else { start }
                });
        let controls = self
            .a11y_controls()
            .into_iter()
            .filter_map(|control| {
                Some(match control {
                    PickerFocus::Up => AccessNode::new(Role::Button)
                        .with_name(tr("widget-filepicker-up"))
                        .with_key(control.id())
                        .with_bounds(self.up_rect)
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.has_keyboard_focus && self.focus == Some(control)),
                        ),
                    PickerFocus::Location => {
                        let focused = self.has_keyboard_focus && self.focus == Some(control);
                        match &self.location {
                            Some(field) => AccessNode::new(Role::TextInput)
                                .with_name(tr("widget-filepicker-location"))
                                .with_value(field.value().to_owned())
                                .with_key(control.id())
                                .with_bounds(self.path_rect())
                                .with_state(AccessState {
                                    invalid: self.location_invalid,
                                    ..AccessState::new().focusable(true).focused(focused)
                                }),
                            None => AccessNode::new(Role::Button)
                                .with_name(tr("widget-filepicker-location"))
                                .with_value(match self.listing {
                                    Listing::Folder => self.location_text(),
                                    _ => self.crumbs.segments().concat(),
                                })
                                .with_key(control.id())
                                .with_bounds(self.path_rect())
                                .with_state(AccessState::new().focusable(true).focused(focused)),
                        }
                    }
                    PickerFocus::Search => AccessNode::new(Role::TextInput)
                        .with_name(tr("widget-filepicker-search"))
                        .with_value(self.query.value().to_owned())
                        .with_key(control.id())
                        .with_bounds(self.search_rect)
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.has_keyboard_focus && self.focus == Some(control)),
                        ),
                    PickerFocus::List => {
                        let total = self.entries.len();
                        let rows = self
                            .entry_rows
                            .iter()
                            .filter_map(|(index, rect)| {
                                let entry = self.entries.get(*index)?;
                                Some(
                                    AccessNode::new(Role::ListItem)
                                        .with_name(entry.name.clone())
                                        .with_hashed_key(&entry.path)
                                        .with_bounds(*rect)
                                        .with_position(index.saturating_add(1), total)
                                        .with_state(AccessState {
                                            focusable: true,
                                            focused: self.has_keyboard_focus
                                                && self.focus == Some(PickerFocus::List)
                                                && self.selection.primary().or_else(|| {
                                                    self.entry_rows.first().map(|(first, _)| *first)
                                                }) == Some(*index),
                                            selected: self.selection.is_selected(*index),
                                            expanded: entry.is_dir.then_some(false),
                                            ..Default::default()
                                        }),
                                )
                            })
                            .collect();
                        AccessNode::new(Role::List)
                            .with_name(tr(if self.mode == FilePickerMode::Directory {
                                "widget-filepicker-folders"
                            } else {
                                "widget-filepicker-files"
                            }))
                            .with_key(control.id())
                            .with_bounds(self.list_rect())
                            .with_state(
                                AccessState::new().focusable(true).focused(
                                    self.has_keyboard_focus && self.focus == Some(control),
                                ),
                            )
                            .with_children(rows)
                    }
                    PickerFocus::Places => {
                        let total = self.places.len();
                        let active = self.active_place();
                        let rows = self
                            .places
                            .iter()
                            .enumerate()
                            .zip(&self.place_rects)
                            .map(|((index, place), rect)| {
                                AccessNode::new(Role::ListItem)
                                    .with_name(place.label())
                                    .with_hashed_key(&place.path)
                                    .with_bounds(*rect)
                                    .with_position(index.saturating_add(1), total)
                                    .with_state(
                                        AccessState::new()
                                            .focusable(true)
                                            .selected(active == Some(index)),
                                    )
                            })
                            .collect();
                        AccessNode::new(Role::List)
                            .with_name(tr("widget-filepicker-places"))
                            .with_key(control.id())
                            .with_bounds(self.sidebar_rect())
                            .with_state(
                                AccessState::new().focusable(true).focused(
                                    self.has_keyboard_focus && self.focus == Some(control),
                                ),
                            )
                            .with_children(rows)
                    }
                    PickerFocus::Filter => AccessNode::new(Role::ComboBox)
                        .with_name(tr("widget-filepicker-file-type"))
                        .with_value(self.active_filter_label())
                        .with_key(control.id())
                        .with_bounds(self.filter_rect)
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.has_keyboard_focus && self.focus == Some(control))
                                .expanded(self.filter_open),
                        ),
                    PickerFocus::Filename => {
                        let node = AccessNode::new(Role::TextInput)
                            .with_name(tr("widget-filepicker-name-accessible"))
                            .with_value(self.filename().to_owned())
                            .with_text_selection(filename_anchor, filename_cursor)
                            .with_key(control.id())
                            .with_bounds(self.filename_rect)
                            .with_state(AccessState {
                                invalid: self.name_error.is_some(),
                                ..AccessState::new()
                                    .focusable(true)
                                    .focused(self.has_keyboard_focus && self.focus == Some(control))
                            });
                        match &self.name_error {
                            Some(error) => node.with_description(error.clone()),
                            None => node,
                        }
                    }
                    PickerFocus::Cancel | PickerFocus::Confirm => {
                        let (label, rect, enabled) = if control == PickerFocus::Cancel {
                            (tr(CANCEL_KEY), self.cancel_rect, true)
                        } else {
                            (
                                tr(&self.confirm_key()),
                                self.open_rect,
                                self.can_commit_open(),
                            )
                        };
                        AccessNode::new(Role::Button)
                            .with_name(label)
                            .with_key(control.id())
                            .with_bounds(rect)
                            .with_state(
                                AccessState::new()
                                    .focusable(enabled)
                                    .focused(
                                        enabled
                                            && self.has_keyboard_focus
                                            && self.focus == Some(control),
                                    )
                                    .disabled(!enabled),
                            )
                    }
                    // Only on the replace prompt, which replaces this list.
                    PickerFocus::KeepExisting | PickerFocus::Replace => return None,
                })
            })
            .collect();
        // The replace prompt is modal over the picker: the controls behind
        // it cannot be reached until it is answered.
        let controls = match self.replace_prompt_accessibility() {
            Some(prompt) => vec![prompt],
            None => controls,
        };
        let dialog = AccessNode::new(Role::Dialog)
            .with_name(self.heading())
            .with_state(
                AccessState::new()
                    .modal(true)
                    .focusable(true)
                    .focused(self.has_keyboard_focus),
            )
            .with_children(controls);
        // The app the chosen file goes to.
        Some(match &self.requester {
            Some(requester) => dialog.with_description(requester.name.clone()),
            None => dialog,
        })
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.has_keyboard_focus = focused;
        if focused {
            self.ensure_valid_focus();
        }
        self.sync_text_focus();
    }

    fn is_focused(&self) -> bool {
        self.has_keyboard_focus
    }

    fn traps_focus(&self) -> bool {
        true
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.focused_text().and_then(Widget::ime_cursor_rect)
    }

    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        self.focused_text().and_then(Widget::ime_content)
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        let field = self.focused_text()?;
        field.ime_cursor_rect()?;
        field.ime_edit_fingerprint(key)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        if self.replace_prompt.is_some() {
            return match path {
                [0] => Some(self.replace_card_rect),
                [0, 0] => Some(self.keep_existing_rect),
                [0, 1] => Some(self.replace_rect),
                _ => None,
            };
        }
        let (&control_index, rest) = path.split_first()?;
        let control = *self.a11y_controls().get(control_index)?;
        match (control, rest) {
            (PickerFocus::Up, []) => Some(self.up_rect),
            (PickerFocus::Location, []) => Some(self.path_rect()),
            (PickerFocus::Search, []) => Some(self.search_rect),
            (PickerFocus::Places, []) => Some(self.sidebar_rect()),
            (PickerFocus::Places, [row_index]) => self.place_rects.get(*row_index).copied(),
            (PickerFocus::List, []) => Some(self.list_rect()),
            (PickerFocus::List, [row_index]) => {
                self.entry_rows.get(*row_index).map(|(_, rect)| *rect)
            }
            (PickerFocus::Filter, []) => Some(self.filter_rect),
            (PickerFocus::Filename, []) => Some(self.filename_rect),
            (PickerFocus::Cancel, []) => Some(self.cancel_rect),
            (PickerFocus::Confirm, []) => Some(self.open_rect),
            _ => None,
        }
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        if self.replace_prompt.is_some() {
            let control = match path {
                [0] => return true,
                [0, 0] => PickerFocus::KeepExisting,
                [0, 1] => PickerFocus::Replace,
                _ => return false,
            };
            self.focus_control(control);
            return true;
        }
        let Some((&control_index, rest)) = path.split_first() else {
            return false;
        };
        let Some(control) = self.a11y_controls().get(control_index).copied() else {
            return false;
        };
        if control == PickerFocus::Confirm && !self.can_commit_open() {
            return false;
        }
        if control == PickerFocus::Places {
            if let [row_index] = rest {
                if *row_index >= self.places.len() {
                    return false;
                }
                self.place_cursor = Some(*row_index);
            } else if !rest.is_empty() {
                return false;
            }
        } else if control == PickerFocus::List {
            if let [row_index] = rest {
                let Some((entry_index, _)) = self.entry_rows.get(*row_index) else {
                    return false;
                };
                self.selection.select_only(*entry_index);
            } else if !rest.is_empty() {
                return false;
            }
        } else if !rest.is_empty() {
            return false;
        }
        self.focus_control(control);
        true
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if key_path.is_empty() {
            return matches!(action, crate::a11y::Action::Activate) && self.activate_focus();
        }

        use crate::a11y::{Action, hash_key};
        if self.replace_prompt.is_some() {
            let control = match key_path {
                [REPLACE_PROMPT_KEY, key] => PickerFocus::from_id(*key)
                    .filter(|c| matches!(c, PickerFocus::KeepExisting | PickerFocus::Replace)),
                _ => None,
            };
            let Some(control) = control else {
                return false;
            };
            return match action {
                Action::Focus => {
                    self.focus_control(control);
                    true
                }
                Action::Activate => {
                    self.focus_control(control);
                    self.activate_focus()
                }
                _ => false,
            };
        }
        let Some((&control_key, rest)) = key_path.split_first() else {
            return false;
        };
        let Some(control) = PickerFocus::from_id(control_key)
            .filter(|c| !matches!(c, PickerFocus::KeepExisting | PickerFocus::Replace))
        else {
            return false;
        };
        if control == PickerFocus::Places {
            if let [place_key] = rest {
                let Some(index) = self
                    .places
                    .iter()
                    .position(|place| hash_key(&place.path) == *place_key)
                else {
                    return false;
                };
                self.place_cursor = Some(index);
            } else if !rest.is_empty() {
                return false;
            }
        } else if control == PickerFocus::List {
            if let [entry_key] = rest {
                let Some(index) = self
                    .entries
                    .iter()
                    .position(|entry| hash_key(&entry.path) == *entry_key)
                else {
                    return false;
                };
                self.selection.select_only(index);
            } else if !rest.is_empty() {
                return false;
            }
        } else if !rest.is_empty() {
            return false;
        }
        match action {
            Action::Focus => {
                self.focus_control(control);
                true
            }
            Action::Activate => {
                self.focus_control(control);
                self.activate_focus()
            }
            _ => false,
        }
    }

    fn test_id(&self) -> Option<&str> {
        Some("file_picker")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl FilePicker {
    fn replace_prompt_name(&self) -> Option<String> {
        let target = self.replace_prompt.as_ref()?;
        Some(target.file_name().map_or_else(
            || target.to_string_lossy().into_owned(),
            |name| name.to_string_lossy().into_owned(),
        ))
    }

    /// The replace prompt as assistive technology and agents read it: a
    /// modal dialog that asks the question, with Cancel and Replace.
    fn replace_prompt_accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let name = self.replace_prompt_name()?;
        let button = |control: PickerFocus, label: &str, rect: Rect| {
            AccessNode::new(Role::Button)
                .with_name(label)
                .with_key(control.id())
                .with_bounds(rect)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.has_keyboard_focus && self.focus == Some(control)),
                )
        };
        Some(
            AccessNode::new(Role::Dialog)
                .with_name(crate::i18n::widget_format(
                    "widget-filepicker-replace-question",
                    &[("name", name.into())],
                ))
                .with_description(tr("widget-filepicker-replace-detail"))
                .with_key(REPLACE_PROMPT_KEY)
                .with_bounds(self.replace_card_rect)
                .with_state(
                    AccessState::new()
                        .modal(true)
                        .focusable(true)
                        .focused(self.has_keyboard_focus),
                )
                .with_children(vec![
                    button(
                        PickerFocus::KeepExisting,
                        &tr(CANCEL_KEY),
                        self.keep_existing_rect,
                    ),
                    button(PickerFocus::Replace, &tr(REPLACE_KEY), self.replace_rect),
                ]),
        )
    }

    /// Paint the replace prompt over the dimmed picker.
    fn draw_replace_prompt(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(name) = self.replace_prompt_name() else {
            return;
        };
        let tokens = &theme.tokens;
        let m = self.metrics;
        let radius = f64::from(tokens.radii.panel);
        let frame = RoundedRect::from_rect(self.rect, radius);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &frame,
        );
        crate::chrome::fill_scrim(scene, transform, self.rect, 1.0, theme);
        scene.pop_layer();

        let card = self.replace_card_rect;
        let card_shape = RoundedRect::from_rect(card, radius);
        crate::chrome::shell::panel_shadow(scene, transform, card, radius, theme, 1.0);
        scene.fill(
            Fill::NonZero,
            transform,
            crate::chrome::panel_ground(theme),
            None,
            &card_shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::chrome::panel_border(theme),
            None,
            &crate::surface::edge_outline(card, radius),
        );

        let text_w = px((card.width() - 2.0 * m.pad).max(0.0));
        let heading_style = theme.text(TextRole::Heading);
        let heading = crate::i18n::widget_format(
            "widget-filepicker-replace-title",
            &[("name", name.clone().into())],
        );
        let (_, heading_h) = text_renderer.measure_style(&heading, Some(text_w), &heading_style);
        text_renderer.draw_style(
            scene,
            &heading,
            tokens.colors.text_primary,
            transform.pre_translate(Vec2::new(card.x0 + m.pad, card.y0 + m.pad)),
            Some(text_w),
            &heading_style,
        );
        let body =
            crate::i18n::widget_format("widget-filepicker-replace-body", &[("name", name.into())]);
        text_renderer.draw_style(
            scene,
            &body,
            tokens.colors.text_dim,
            transform.pre_translate(Vec2::new(
                card.x0 + m.pad,
                card.y0 + m.pad + f64::from(heading_h) + m.gap,
            )),
            Some(text_w),
            &theme.text(TextRole::Body),
        );

        let keep = footer_button(self.keep_existing_rect, CANCEL_KEY, false)
            .hovered(self.hover_prompt == Some(PickerFocus::KeepExisting))
            .focused(self.has_keyboard_focus && self.focus == Some(PickerFocus::KeepExisting));
        crate::widget::Button::paint(scene, transform, text_renderer, theme, &keep);
        let replace = footer_button(self.replace_rect, REPLACE_KEY, false)
            .role(ButtonRole::Destructive)
            .hovered(self.hover_prompt == Some(PickerFocus::Replace))
            .focused(self.has_keyboard_focus && self.focus == Some(PickerFocus::Replace));
        crate::widget::Button::paint(scene, transform, text_renderer, theme, &replace);
    }

    /// Who is asking: the app's icon, the heading (what the app wants, or
    /// its own purpose) and, under a purpose, the app's name.
    fn draw_header(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let m = self.metrics;
        let top = self.rect.y0 + m.pad;
        let band = self.heading_h();
        let mut x = self.rect.x0 + m.pad;
        if let Some(requester) = &self.requester {
            let icon = Rect::new(
                x,
                top + (band - m.app_icon) / 2.0,
                x + m.app_icon,
                top + (band + m.app_icon) / 2.0,
            );
            let drawn = requester
                .image
                .as_ref()
                .is_some_and(|image| crate::icons::draw_app_image(scene, transform, icon, image));
            if !drawn {
                draw_icon(
                    scene,
                    transform,
                    icon,
                    requester.glyph,
                    tokens.colors.text_primary,
                    LUCIDE_DEFAULT_STROKE_PX,
                );
            }
            x = icon.x1 + m.section;
        }
        let width = (self.rect.x1 - m.pad - x).max(0.0);
        let heading_style = theme.text(TextRole::Heading);
        let heading =
            text_renderer.elide_to_width_style(&self.heading(), px(width), &heading_style);
        let text_h = m.title_h
            + if self.has_subtitle() {
                m.subtitle_h
            } else {
                0.0
            };
        let text_top = top + (band - text_h) / 2.0;
        text_renderer.draw_style(
            scene,
            &heading,
            tokens.colors.text_primary,
            transform.pre_translate(Vec2::new(x, text_top)),
            None,
            &heading_style,
        );
        if self.has_subtitle()
            && let Some(requester) = &self.requester
        {
            let style = theme.text(TextRole::Caption);
            let name = text_renderer.elide_to_width_style(&requester.name, px(width), &style);
            text_renderer.draw_style(
                scene,
                &name,
                tokens.colors.text_dim,
                transform.pre_translate(Vec2::new(x, text_top + m.title_h)),
                None,
                &style,
            );
        }
    }

    /// Up, the path bar (the trail, or the field typing a path) and the
    /// search field.
    fn draw_toolbar(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;
        let up_label = tr("widget-filepicker-up");
        let up = crate::widget::ButtonFace::icon_only(self.up_rect, Icon::ArrowUp, &up_label)
            .role(ButtonRole::Plain)
            .hovered(self.up_rect.contains(self.last_cursor))
            .focused(self.has_keyboard_focus && self.focus == Some(PickerFocus::Up))
            .disabled(!self.can_go_up());
        crate::widget::Button::paint(scene, transform, text_renderer, theme, &up);

        // The path bar is a field in the same row as the search field, so
        // it is the same raised well with no edge, and the trail reads as a
        // place rather than as loose text. Forced colours and a refused
        // path draw its outline.
        let bar = self.path_rect();
        let well = RoundedRect::from_rect(bar, f64::from(tokens.radii.control));
        let fill = tokens.colors.surfaces.raised;
        scene.fill(Fill::NonZero, transform, fill, None, &well);
        if self.location_invalid {
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                tokens.colors.semantic.danger,
                None,
                &well,
            );
        } else if theme.colors_are_forced() {
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                crate::surface::edge_on(fill),
                None,
                &well,
            );
        }
        if self.has_keyboard_focus && self.focus == Some(PickerFocus::Location) {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                tokens.colors.border_focus,
                None,
                &well,
            );
        }
        match self.location.as_mut() {
            Some(field) => Widget::draw(field, scene, transform, text_renderer, theme, presence),
            None => self
                .crumbs
                .draw(scene, transform, text_renderer, theme, presence),
        }
        self.query
            .draw(scene, transform, text_renderer, theme, presence);
    }

    /// The places sidebar: a quiet wash beside the list, one row per place.
    fn draw_sidebar(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.has_sidebar() {
            return;
        }
        let tokens = &theme.tokens;
        let m = self.metrics;
        let ground = crate::chrome::panel_ground(theme);
        let side = self.sidebar_rect();
        scene.fill(
            Fill::NonZero,
            transform,
            crate::tokens::Colors::hairline_on(ground, 10),
            None,
            &side,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            crate::chrome::panel_border(theme),
            None,
            &Rect::new(side.x1 - 1.0, side.y0, side.x1, side.y1),
        );
        let style = theme.text(TextRole::Body);
        let active = self.active_place();
        for (i, rect) in self.place_rects.iter().enumerate() {
            let Some(place) = self.places.get(i) else {
                break;
            };
            let shape = RoundedRect::from_rect(*rect, f64::from(tokens.radii.control));
            if active == Some(i) {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    tokens.colors.lift_on(ground, 1.0),
                    None,
                    &shape,
                );
            } else if self.hover_place == Some(i) {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    crate::tokens::Colors::hairline_on(ground, crate::chrome::HAIRLINE_ALPHA),
                    None,
                    &shape,
                );
            }
            if self.has_keyboard_focus
                && self.focus == Some(PickerFocus::Places)
                && self.place_cursor == Some(i)
            {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    tokens.colors.border_focus,
                    None,
                    &shape,
                );
            }
            let icon = Rect::new(
                rect.x0 + m.inset_x,
                rect.center().y - m.icon_md / 2.0,
                rect.x0 + m.inset_x + m.icon_md,
                rect.center().y + m.icon_md / 2.0,
            );
            draw_icon(
                scene,
                transform,
                icon,
                place.icon,
                tokens.colors.text_dim,
                LUCIDE_DEFAULT_STROKE_PX,
            );
            let x = icon.x1 + m.gap;
            let label = text_renderer.elide_to_width_style(
                &place.label(),
                (rect.x1 - m.inset_x - x).max(0.0) as f32,
                &style,
            );
            let (_, th) = text_renderer.measure_style(&label, None, &style);
            text_renderer.draw_style(
                scene,
                &label,
                tokens.colors.text_primary,
                transform.pre_translate(Vec2::new(x, rect.center().y - f64::from(th) / 2.0)),
                None,
                &style,
            );
        }
    }

    /// The file list, or what it says when there is nothing to show, with
    /// the keyboard's ring on the row it is on.
    fn draw_list(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;
        self.table
            .draw(scene, transform, text_renderer, theme, presence);
        let list = self.list_rect();
        if self.entries.is_empty() && !self.looking_up() {
            let key = match &self.listing {
                Listing::Recent => "widget-filepicker-recent-empty",
                Listing::Search(_) => "widget-filepicker-no-matches",
                Listing::Folder if !self.query.is_empty() => "widget-filepicker-no-matches",
                Listing::Folder => "widget-filepicker-empty",
            };
            let style = theme.text(TextRole::Body);
            let message = tr(key);
            let (w, h) = text_renderer.measure_style(&message, None, &style);
            let body_top = list.y0 + self.table.header_height();
            let centre_y = f64::midpoint(body_top, list.y1);
            text_renderer.draw_style(
                scene,
                &message,
                tokens.colors.text_dim,
                transform.pre_translate(Vec2::new(
                    list.center().x - f64::from(w) / 2.0,
                    centre_y - f64::from(h) / 2.0,
                )),
                None,
                &style,
            );
            return;
        }
        if self.has_keyboard_focus && self.focus == Some(PickerFocus::List) {
            let cursor = self
                .selection
                .primary()
                .or_else(|| self.entry_rows.first().map(|(first, _)| *first));
            if let Some((_, rect)) = self.entry_rows.iter().find(|(i, _)| Some(*i) == cursor) {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    tokens.colors.border_focus,
                    None,
                    &RoundedRect::from_rect(*rect, f64::from(tokens.radii.control)),
                );
            }
        }
    }

    /// The footer: Save's name row, the type dropdown and the Cancel and
    /// confirm buttons. Split out of `draw` to keep that function readable.
    fn draw_footer(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let m = self.metrics;
        let ground = crate::chrome::panel_ground(theme);
        let top = self.rect.y1 - self.footer_h();
        scene.fill(
            Fill::NonZero,
            transform,
            crate::tokens::Colors::hairline_on(ground, crate::chrome::HAIRLINE_ALPHA),
            None,
            &Rect::new(self.rect.x0, top, self.rect.x1, top + 1.0),
        );
        if self.mode == FilePickerMode::Save {
            let label = tr("widget-filepicker-name-label");
            let style = theme.text(TextRole::Label);
            let (_, th) = text_renderer.measure_style(&label, None, &style);
            text_renderer.draw_style(
                scene,
                &label,
                theme.tokens.colors.text_dim,
                transform.pre_translate(Vec2::new(
                    self.rect.x0 + m.pad,
                    self.filename_rect.center().y - f64::from(th) / 2.0,
                )),
                None,
                &style,
            );
            self.draw_filename_field(scene, transform, text_renderer, theme, presence);
            if let Some(error) = &self.name_error {
                let style = theme.text(TextRole::Caption);
                text_renderer.draw_style(
                    scene,
                    error,
                    crate::widget::Tone::Danger.colour(theme),
                    transform.pre_translate(Vec2::new(
                        self.filename_rect.x0 + m.inset_x,
                        self.filename_rect.y1 + m.gap / 2.0,
                    )),
                    Some(px(self.filename_rect.width() - m.inset_x)),
                    &style,
                );
            }
        }
        if self.shows_filter() {
            // The type menu says what it filters: a "File type" label, then
            // the type ("Documents", "Images", "All files").
            let label = tr("widget-filepicker-file-type");
            let style = theme.text(TextRole::Label);
            let (_, th) = text_renderer.measure_style(&label, None, &style);
            text_renderer.draw_style(
                scene,
                &label,
                theme.tokens.colors.text_dim,
                transform.pre_translate(Vec2::new(
                    self.rect.x0 + m.pad,
                    self.filter_rect.center().y - f64::from(th) / 2.0,
                )),
                None,
                &style,
            );
            let active = self
                .filters
                .get(self.active_filter)
                .map(|f| self.filter_display(f))
                .unwrap_or_default();
            crate::widget::DropdownFace::new(self.filter_rect, &active)
                .open(self.filter_open)
                .hovered(self.filter_rect.contains(self.last_cursor))
                .focused(self.has_keyboard_focus && self.focus == Some(PickerFocus::Filter))
                .paint(scene, transform, text_renderer, theme);
        }

        let cancel = footer_button(self.cancel_rect, CANCEL_KEY, false)
            .hovered(self.hover_button == Some(0))
            .focused(self.has_keyboard_focus && self.focus == Some(PickerFocus::Cancel));
        crate::widget::Button::paint(scene, transform, text_renderer, theme, &cancel);
        let confirm_key = self.confirm_key();
        let confirm = footer_button(self.open_rect, &confirm_key, true)
            .hovered(self.hover_button == Some(1))
            .focused(self.has_keyboard_focus && self.focus == Some(PickerFocus::Confirm))
            .disabled(!self.can_commit_open());
        crate::widget::Button::paint(scene, transform, text_renderer, theme, &confirm);
    }

    /// The open type menu, floating above its chip like any other menu.
    fn draw_filter_menu(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.filter_open || self.replace_prompt.is_some() {
            return;
        }
        let tokens = &theme.tokens;
        let m = self.metrics;
        let radius = f64::from(tokens.radii.control);
        let menu = self.filter_menu_rect;
        crate::chrome::shell::panel_shadow(scene, transform, menu, radius, theme, 1.0);
        let shape = RoundedRect::from_rect(menu, radius);
        scene.fill(
            Fill::NonZero,
            transform,
            crate::chrome::panel_ground(theme),
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::chrome::panel_border(theme),
            None,
            &shape,
        );
        let style = theme.text(TextRole::Body);
        for (i, (filter, row)) in self.filters.iter().zip(&self.filter_rows).enumerate() {
            let highlighted = self.filter_hover == Some(i);
            if highlighted {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    tokens
                        .colors
                        .lift_on(crate::chrome::panel_ground(theme), 1.0),
                    None,
                    &RoundedRect::from_rect(*row, f64::from(tokens.radii.detail)),
                );
            }
            let label = text_renderer.elide_to_width_style(
                &self.filter_display(filter),
                px((row.width() - 2.0 * m.inset_x - m.icon_sm - m.gap).max(0.0)),
                &style,
            );
            let (_, th) = text_renderer.measure_style(&label, None, &style);
            text_renderer.draw_style(
                scene,
                &label,
                tokens.colors.text_primary,
                transform.pre_translate(Vec2::new(
                    row.x0 + m.inset_x,
                    row.center().y - f64::from(th) / 2.0,
                )),
                None,
                &style,
            );
            if i == self.active_filter {
                draw_icon(
                    scene,
                    transform,
                    Rect::new(
                        row.x1 - m.inset_x - m.icon_sm,
                        row.center().y - m.icon_sm / 2.0,
                        row.x1 - m.inset_x,
                        row.center().y + m.icon_sm / 2.0,
                    ),
                    Icon::Check,
                    tokens.colors.text_primary,
                    LUCIDE_DEFAULT_STROKE_PX,
                );
            }
        }
    }

    /// Save-mode filename text field. Split out of `draw_footer` to keep
    /// that under the line cap.
    fn draw_filename_field(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;
        let field = self.filename_rect;
        let s = RoundedRect::from_rect(field, f64::from(tokens.radii.detail));
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.surfaces.base,
            None,
            &s,
        );
        // Rest edge derived from the field fill (HIG §9.5); focus keeps
        // `border_focus`.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            if self.name_error.is_some() {
                tokens.colors.semantic.danger
            } else if self.has_keyboard_focus && self.focus == Some(PickerFocus::Filename) {
                tokens.colors.border_focus
            } else {
                crate::surface::edge_on(tokens.colors.surfaces.base)
            },
            None,
            &s,
        );
        if self.has_keyboard_focus && self.focus == Some(PickerFocus::Filename) {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                tokens.colors.border_focus,
                None,
                &s,
            );
        }
        Widget::draw(
            &mut self.filename,
            scene,
            transform,
            text_renderer,
            theme,
            presence,
        );
    }

    /// Hand `event` to the text field it types into, and act on what it
    /// typed: a new search narrows the list, a typed path is no longer
    /// marked, a changed name clears its error.
    fn type_into(
        &mut self,
        field: PickerFocus,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        match field {
            PickerFocus::Filename => {
                let before = self
                    .name_error
                    .is_some()
                    .then(|| self.filename().to_owned());
                self.filename.handle_event(event, ctx);
                if before.is_some_and(|before| before != self.filename()) {
                    self.name_error = None;
                    self.apply_layout_helper();
                }
            }
            PickerFocus::Search => {
                self.query.handle_event(event, ctx);
                if self.query.take_dirty().is_some() {
                    self.on_query_typed();
                }
            }
            PickerFocus::Location => {
                if let Some(location) = self.location.as_mut() {
                    location.handle_event(event, ctx);
                    self.location_invalid = false;
                }
            }
            _ => {}
        }
    }

    /// Pointer input the text fields follow wherever it lands: a drag
    /// selecting text, a release ending it.
    fn forward_to_fields(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let mut handled = false;
        let mut inner = crate::widget::EventCtx {
            handled: &mut handled,
            ..ctx.reborrow(ctx.path, ctx.node_id)
        };
        self.query.handle_event(event, &mut inner);
        if let Some(location) = self.location.as_mut() {
            location.handle_event(event, &mut inner);
        }
        if self.query.take_dirty().is_some() {
            self.on_query_typed();
        }
    }

    /// `WindowEvent::CursorMoved` handling. Split out of `handle_event` to
    /// keep that function readable.
    fn on_cursor_moved(&mut self, p: Point, ctx: &mut crate::widget::EventCtx<'_>) {
        self.last_cursor = p;
        let new_button = if self.cancel_rect.contains(p) {
            Some(0)
        } else if self.open_rect.contains(p) {
            Some(1)
        } else {
            None
        };
        let new_place = self.place_rects.iter().position(|r| r.contains(p));
        if self.filter_open
            && let Some(row) = self.filter_rows.iter().position(|r| r.contains(p))
            && self.filter_hover != Some(row)
        {
            self.filter_hover = Some(row);
            *ctx.handled = true;
        }
        if new_button != self.hover_button || new_place != self.hover_place {
            self.hover_button = new_button;
            self.hover_place = new_place;
            *ctx.handled = true;
        }
        // The parts follow the pointer themselves: the list's hover and its
        // scrollbar or column drags, the trail's hover, a field's selection.
        let moved =
            crate::synthetic_input::cursor_moved(p.x * ctx.scale_factor, p.y * ctx.scale_factor);
        let _ = self.table.handle_event(&moved, ctx);
        if self.location.is_none() {
            self.crumbs.handle_event(&moved, ctx);
        }
        self.forward_to_fields(&moved, ctx);
    }

    /// Left-button `WindowEvent::MouseInput` handling. Split out of
    /// `handle_event` to keep that function readable.
    #[allow(
        clippy::too_many_lines,
        reason = "one press dispatch over every part of the sheet, in hit order"
    )]
    fn on_left_click(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let p = self.last_cursor;
        let modifiers = *ctx.modifiers;
        if self.filter_open {
            self.filter_open = false;
            if let Some(row) = self.filter_rows.iter().position(|r| r.contains(p)) {
                self.set_filter(row);
                *ctx.handled = true;
                return;
            }
            // A press on the chip that opened the menu closes it.
            if self.filter_rect.contains(p) {
                *ctx.handled = true;
                return;
            }
        }
        if self.cancel_rect.contains(p) {
            self.focus_control(PickerFocus::Cancel);
            self.click_cancel();
            *ctx.handled = true;
            return;
        }
        if self.open_rect.contains(p) && self.can_commit_open() {
            self.focus_control(PickerFocus::Confirm);
            self.click_open();
            *ctx.handled = true;
            return;
        }
        if self.can_go_up() && self.up_rect.contains(p) {
            self.focus_control(PickerFocus::Up);
            self.go_up();
            *ctx.handled = true;
            return;
        }
        if self.shows_filter() && self.filter_rect.contains(p) {
            self.focus_control(PickerFocus::Filter);
            self.toggle_filter_menu();
            *ctx.handled = true;
            return;
        }
        if self.search_rect.contains(p) {
            self.focus_control(PickerFocus::Search);
            self.forward_to_fields(event, ctx);
            *ctx.handled = true;
            return;
        }
        if self.path_rect().contains(p) {
            self.focus_control(PickerFocus::Location);
            if self.location.is_some() {
                self.forward_to_fields(event, ctx);
            } else {
                self.crumbs.handle_event(event, ctx);
                if let Some(index) = self.crumb_clicked.take()
                    && let Some(target) = self.crumb_targets.get(index).cloned()
                    && !target.as_os_str().is_empty()
                {
                    self.navigate_to(&target);
                } else if self.crumb_edit.take() {
                    self.edit_location();
                }
            }
            *ctx.handled = true;
            return;
        }
        if let Some(i) = self.place_rects.iter().position(|r| r.contains(p)) {
            self.focus_control(PickerFocus::Places);
            self.go_place(i);
            *ctx.handled = true;
            return;
        }
        if self.mode == FilePickerMode::Save && self.filename_rect.contains(p) {
            self.focus_control(PickerFocus::Filename);
            *ctx.handled = true;
            return;
        }
        // A press on a row is the picker's (it selects, enters a folder or
        // chooses on a double click); the header and the scrollbar are the
        // list's own.
        if let Some((i, _)) = self.entry_rows.iter().find(|(_, r)| r.contains(p)).copied()
            && !self.table.in_scrollbar(p)
        {
            self.focus_control(PickerFocus::List);
            let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
            let shift = modifiers.state().shift_key();
            self.activate_entry(i, ctrl, shift);
            *ctx.handled = true;
            return;
        }
        if self.list_rect().contains(p) {
            self.table.point_at(p, ctx);
            let outcome = self.table.handle_event(event, ctx);
            if let Some(column) = outcome.sort {
                self.sort_by(column);
            }
            self.entry_rows = self.table.row_rects();
            *ctx.handled = true;
        }
    }

    /// `WindowEvent::KeyboardInput` handling. Split out of `handle_event`
    /// to keep that function readable.
    #[allow(
        clippy::too_many_lines,
        reason = "one key dispatch over the sheet's controls, in focus order"
    )]
    fn on_key_pressed(
        &mut self,
        ke: &winit::event::KeyEvent,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        use winit::keyboard::{Key, NamedKey};
        let modifiers = *ctx.modifiers;
        let shift = modifiers.state().shift_key();
        let command = modifiers.state().control_key() || modifiers.state().super_key();
        self.has_keyboard_focus = true;
        self.ensure_valid_focus();
        if self.filter_open {
            self.on_filter_menu_key(&ke.logical_key);
            *ctx.handled = true;
            return;
        }
        if command && let Key::Character(c) = &ke.logical_key {
            match c.as_str() {
                "l" | "L" => {
                    self.edit_location();
                    *ctx.handled = true;
                    return;
                }
                "f" | "F" => {
                    self.focus_control(PickerFocus::Search);
                    self.query.select_all();
                    *ctx.handled = true;
                    return;
                }
                _ => {}
            }
        }
        match &ke.logical_key {
            Key::Named(NamedKey::Tab) => {
                self.move_focus(shift);
            }
            Key::Named(NamedKey::ArrowDown) if self.focus == Some(PickerFocus::Places) => {
                self.move_place(1);
            }
            Key::Named(NamedKey::ArrowUp) if self.focus == Some(PickerFocus::Places) => {
                self.move_place(-1);
            }
            Key::Named(NamedKey::ArrowDown) if self.focus == Some(PickerFocus::Search) => {
                self.focus_control(PickerFocus::List);
                if self.selection.primary().is_none() && !self.entries.is_empty() {
                    self.move_selection(1, false);
                }
            }
            Key::Named(NamedKey::Escape) => match self.focus {
                Some(PickerFocus::Location) if self.location.is_some() => {
                    self.stop_editing_location();
                }
                Some(PickerFocus::Search | PickerFocus::List)
                    if !self.query.is_empty() || matches!(self.listing, Listing::Search(_)) =>
                {
                    self.end_search();
                }
                _ => self.click_cancel(),
            },
            Key::Named(NamedKey::ArrowDown) if self.focus == Some(PickerFocus::List) => {
                self.move_selection(1, shift);
            }
            Key::Named(NamedKey::ArrowUp) if self.focus == Some(PickerFocus::List) => {
                self.move_selection(-1, shift);
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => match self.focus {
                Some(PickerFocus::Cancel) => self.click_cancel(),
                Some(PickerFocus::Confirm) if self.can_commit_open() => self.click_open(),
                Some(PickerFocus::Up) => self.go_up(),
                Some(PickerFocus::Filter) => self.toggle_filter_menu(),
                Some(PickerFocus::Location) if self.location.is_some() => {
                    if matches!(ke.logical_key, Key::Named(NamedKey::Enter)) {
                        self.commit_location();
                    }
                }
                Some(PickerFocus::Location) => self.edit_location(),
                Some(PickerFocus::Search)
                    if matches!(ke.logical_key, Key::Named(NamedKey::Enter)) =>
                {
                    let query = self.query.value().to_owned();
                    self.search_for(&query);
                }
                Some(PickerFocus::Places) => {
                    if let Some(i) = self.place_cursor.or_else(|| self.active_place()) {
                        self.go_place(i);
                    }
                }
                Some(PickerFocus::List)
                    if matches!(ke.logical_key, Key::Named(NamedKey::Enter)) =>
                {
                    self.activate_selected_entry();
                }
                Some(PickerFocus::Filename)
                    if matches!(ke.logical_key, Key::Named(NamedKey::Enter))
                        && self.can_commit_open() =>
                {
                    self.click_open();
                }
                _ => {}
            },
            Key::Named(NamedKey::Backspace) if self.focus == Some(PickerFocus::List) => {
                if self.listing == Listing::Folder {
                    self.go_up();
                } else {
                    self.listing = Listing::Folder;
                    self.end_search();
                }
            }
            // Typing while the list has the keyboard searches: the text
            // goes into the search field, which narrows the list.
            Key::Character(_) if self.focus == Some(PickerFocus::List) && !command => {
                self.focus_control(PickerFocus::Search);
                self.type_into(PickerFocus::Search, event, ctx);
            }
            _ => return,
        }
        *ctx.handled = true;
    }

    /// Input while the replace prompt is up: it is modal over the picker, so
    /// it takes every event and only its own two buttons respond.
    fn on_replace_prompt_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        use winit::keyboard::{Key, NamedKey};
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = ctx.logical(position);
                self.last_cursor = p;
                let hover = if self.keep_existing_rect.contains(p) {
                    Some(PickerFocus::KeepExisting)
                } else if self.replace_rect.contains(p) {
                    Some(PickerFocus::Replace)
                } else {
                    None
                };
                self.hover_prompt = hover;
                self.hover_button = None;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.keep_existing_rect.contains(self.last_cursor) {
                    self.keep_existing();
                } else if self.replace_rect.contains(self.last_cursor) {
                    self.focus = Some(PickerFocus::Replace);
                    self.confirm_replace();
                }
            }
            WindowEvent::KeyboardInput { event: ke, .. } if ke.state == ElementState::Pressed => {
                self.has_keyboard_focus = true;
                self.ensure_valid_focus();
                let shift = ctx.modifiers.state().shift_key();
                match &ke.logical_key {
                    Key::Named(NamedKey::Tab) => self.move_focus(shift),
                    Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) => self.move_focus(true),
                    Key::Named(NamedKey::ArrowRight | NamedKey::ArrowDown) => {
                        self.move_focus(false);
                    }
                    Key::Named(NamedKey::Escape) => self.keep_existing(),
                    Key::Named(NamedKey::Enter | NamedKey::Space) => {
                        self.activate_focus();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        *ctx.handled = true;
    }

    fn activate_selected_entry(&mut self) {
        if let Some(i) = self.selection.primary() {
            // Capture what we need before calling into &mut self methods, so
            // the immutable borrow of `self.entries` ends before the action.
            let dir_target = self
                .entries
                .get(i)
                .and_then(|e| e.is_dir.then(|| e.path.clone()));
            match dir_target {
                Some(path) => self.navigate_to(&path),
                None if self.can_commit_open() => self.click_open(),
                None => {}
            }
        }
    }
}

/// A footer button in the canonical immediate-mode button face, so the
/// picker footer stays visually locked to the retained widget and every
/// converted app. Callers add the hover, focus and disabled state.
fn footer_button(rect: Rect, label: &str, is_primary: bool) -> crate::widget::ButtonFace<'_> {
    crate::widget::ButtonFace::new(rect, label).role(if is_primary {
        ButtonRole::Primary
    } else {
        ButtonRole::Secondary
    })
}

#[cfg(test)]
mod tests;
