//! File-picker dialog widget — used by the `FileChooser` xdg-portal backend
//! and any "open / save file from app menu" flow. Self-contained: it draws
//! and hit-tests itself, and the host just positions it as a modal overlay
//! and drains [`FilePicker::take_outcome`].
//!
//! Top to bottom: title + breadcrumb + up button, a scrollable list of
//! [`FilePickerEntry`]s (directory click navigates in file modes and selects
//! in directory mode; double-click navigates; Ctrl/Shift multi-select when
//! enabled), a filename
//! field in Save mode, an optional filter chip, and Cancel/Open(Save)
//! buttons. Keyboard: Up/Down move the selection, Enter opens/enters,
//! Backspace goes to the parent, Escape cancels; printable keys type-ahead
//! in Open mode or edit the filename in Save mode.
//!
//! Constructed with "accessible roots" — the workspace-bound paths the
//! caller's sandbox already allows. [`FilePicker::can_navigate_to`] gates
//! every path mutation against them so the user can't `..` out of the
//! sandbox boundary.

use crate::widget::ModelPath;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
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
use crate::widget::{TextInput, Widget};

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
    /// The column header's band: a caption line in an `s2` inset.
    column_h: f64,
    /// The places sidebar (`sidebar_width`, narrowed for a dialog).
    sidebar_w: f64,
    /// Below this dialog width the sidebar gives way to the list.
    sidebar_min_dialog_w: f64,
    icon_sm: f64,
    icon_md: f64,
    icon_lg: f64,
    /// Horizontal inset of text inside a field, chip or row (`s2`).
    inset_x: f64,
    /// The chevron between two breadcrumbs.
    crumb_separator_w: f64,
    /// A breadcrumb's width before it has been measured.
    crumb_glyph_estimate: f64,
    /// The scroll thumb's thickness (`s1`).
    scrollbar_w: f64,
    /// A footer button is never narrower than this.
    min_button_w: f64,
    /// The Save field's label column.
    name_label_w: f64,
    /// The Size and Modified columns, and the list width that shows them.
    size_w: f64,
    date_w: f64,
    columns_min_w: f64,
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
            column_h: line(TextRole::Caption) + 2.0 * f64::from(sp.s2),
            sidebar_w: f64::from(m.sidebar_width) * 0.72,
            sidebar_min_dialog_w: f64::from(m.panel_width_sheet) * 0.9,
            icon_sm: f64::from(m.icon_sm),
            icon_md: f64::from(m.icon_md),
            icon_lg,
            inset_x: f64::from(sp.s2),
            crumb_separator_w: f64::from(m.icon_sm),
            crumb_glyph_estimate: f64::from(t.typography.size(TextRole::Label)) * 0.6,
            scrollbar_w: f64::from(sp.s1),
            min_button_w: icon_lg * 4.0,
            name_label_w: icon_lg * 3.5,
            size_w: icon_lg * 3.5,
            date_w: icon_lg * 6.5,
            columns_min_w: icon_lg * 20.0,
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
    path: PathBuf,
    icon: Icon,
}

impl Place {
    fn label(&self) -> String {
        self.key.map_or_else(
            || self.name.clone(),
            |key| crate::i18n::widget_format(key, &[]),
        )
    }
}

/// Home, the usual folders under it, then every other accessible root (the
/// workspace folders), each only if the caller may enter it.
fn default_places(roots: &[PathBuf], home: Option<&Path>) -> Vec<Place> {
    let allowed = |p: &Path| roots.iter().any(|root| p.starts_with(root));
    let mut places: Vec<Place> = Vec::new();
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
            if folder.is_some() && !path.is_dir() {
                continue;
            }
            if allowed(&path) {
                places.push(Place {
                    key: Some(key),
                    name: String::new(),
                    path,
                    icon,
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

/// A `size`-square rect centred in `rect`.
fn centered(rect: Rect, size: f64) -> Rect {
    let c = rect.center();
    Rect::new(
        c.x - size / 2.0,
        c.y - size / 2.0,
        c.x + size / 2.0,
        c.y + size / 2.0,
    )
}

/// The file chooser is an immediate-mode modal, so its controls do not each
/// have a retained Taffy node.  Keep their keyboard order in the toolkit's
/// standard [`crate::overlay::FocusTrap`] instead of reimplementing wrapping
/// Tab logic locally.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum PickerFocus {
    Up,
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

pub struct FilePicker {
    rect: Rect,
    title: String,
    mode: FilePickerMode,
    multiple: bool,
    accept_label: Option<String>,

    current_dir: PathBuf,
    /// Filtered for `current_dir`.
    entries: Vec<FilePickerEntry>,
    /// File indices in Open/Save mode; directory indices in Directory mode.
    selection: SelectionModel,

    /// Paths the host has marked as accessible. Navigation outside every
    /// root is silently ignored.
    accessible_roots: Vec<PathBuf>,

    filters: Vec<FileFilter>,
    active_filter: usize,

    /// The Save-mode filename: the shared text field, so caret, selection,
    /// clipboard and input methods behave as in every other field.
    filename: TextInput,
    first_visible: usize,

    /// Set on Open / Cancel. `None` until resolved.
    pending_outcome: Option<FilePickerOutcome>,
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
    open_rect: Rect,
    cancel_rect: Rect,
    up_rect: Rect,
    filter_rect: Rect,
    filename_rect: Rect,
    replace_card_rect: Rect,
    keep_existing_rect: Rect,
    replace_rect: Rect,
    /// `(entry_index, rect)` for the currently-visible rows only.
    entry_rows: Vec<(usize, Rect)>,
    /// `(ancestor_path, rect)` for each clickable breadcrumb segment.
    crumb_rects: Vec<(PathBuf, Rect)>,
    /// Breadcrumb label widths measured at paint with the frame's renderer.
    /// `compute_crumb_rects` (which has no renderer, and also runs from the
    /// event path) prefers these to its per-glyph estimate, so a hit rect
    /// matches the painted glyphs from the first paint on.
    crumb_widths: HashMap<String, f64>,
    /// Token-derived insets from the theme of the latest layout build, so
    /// the renderer-less layout pass and the draw pass read the same
    /// `ui_scale`d values. `PickerMetrics::default()` until a theme is seen.
    metrics: PickerMetrics,

    /// `MouseInput` carries no position payload, so this tracks the cursor
    /// for click resolution.
    last_cursor: Point,
    /// Last entry pressed + when, for double-click open/navigation detection.
    last_click: Option<(usize, crate::time::Instant)>,

    hover_entry: Option<usize>,
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
    /// `[initial_dir]`.
    pub fn new<P: AsRef<Path>>(
        title: impl Into<String>,
        initial_dir: P,
        accessible_roots: Vec<PathBuf>,
    ) -> Self {
        let initial: PathBuf = initial_dir.as_ref().to_path_buf();
        let roots = if accessible_roots.is_empty() {
            vec![initial.clone()]
        } else {
            accessible_roots
        };
        let places = default_places(&roots, None);
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
            rect: Rect::ZERO,
            title: title.into(),
            mode: FilePickerMode::default(),
            // Preserve the historical direct-widget Open behavior. Portal
            // callers explicitly hand their single/multiple intent through.
            multiple: true,
            accept_label: None,
            current_dir: initial,
            entries: Vec::new(),
            selection: SelectionModel::new(),
            accessible_roots: roots,
            filters: vec![FileFilter::all()],
            active_filter: 0,
            // A file name is not prose: no spelling marks, and never an
            // autocorrection rewriting a word of it.
            filename: TextInput::local()
                .with_kind(crate::widget::TextInputKind::Name)
                .with_plain_chrome()
                .with_accessible_name(tr("widget-filepicker-name-accessible"))
                .with_placeholder(&tr("widget-filepicker-name-placeholder")),
            first_visible: 0,
            pending_outcome: None,
            replace_prompt: None,
            open_rect: Rect::ZERO,
            cancel_rect: Rect::ZERO,
            up_rect: Rect::ZERO,
            filter_rect: Rect::ZERO,
            filename_rect: Rect::ZERO,
            replace_card_rect: Rect::ZERO,
            keep_existing_rect: Rect::ZERO,
            replace_rect: Rect::ZERO,
            entry_rows: Vec::new(),
            crumb_rects: Vec::new(),
            crumb_widths: HashMap::new(),
            metrics,
            last_cursor: Point::ZERO,
            last_click: None,
            hover_entry: None,
            hover_button: None,
            hover_prompt: None,
            focus: None,
            has_keyboard_focus: false,
        };
        me.refresh_entries();
        me
    }

    #[must_use]
    pub fn with_mode(mut self, mode: FilePickerMode) -> Self {
        self.mode = mode;
        self.selection.clear();
        self.first_visible = 0;
        self.refresh_entries();
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
        self.places = default_places(&self.accessible_roots, home.as_deref());
        self.place_cursor = None;
        self
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

    /// The first filter becomes active. If `filters` is empty an "All
    /// files" filter is used.
    #[must_use]
    pub fn with_filters(mut self, filters: Vec<FileFilter>) -> Self {
        self.filters = if filters.is_empty() {
            vec![FileFilter::all()]
        } else {
            filters
        };
        self.active_filter = 0;
        self.refresh_entries();
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
            self.sync_filename_focus();
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

    pub fn can_go_up(&self) -> bool {
        self.current_dir
            .parent()
            .is_some_and(|p| self.can_navigate_to(p))
    }

    /// No-op when rejected by the roots.
    pub fn navigate_to<P: AsRef<Path>>(&mut self, target: P) {
        let t = target.as_ref().to_path_buf();
        if !self.can_navigate_to(&t) {
            return;
        }
        self.current_dir = t;
        self.filter_open = false;
        self.selection.clear();
        self.first_visible = 0;
        self.refresh_entries();
    }

    pub fn go_up(&mut self) {
        if let Some(parent) = self.current_dir.parent().map(std::path::Path::to_path_buf) {
            self.navigate_to(parent);
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
            self.first_visible = 0;
            self.refresh_entries();
        }
    }

    pub fn refresh_entries(&mut self) {
        self.entries.clear();
        let idx = self.active_filter.min(self.filters.len().saturating_sub(1));
        let Some(filter) = self.filters.get(idx) else {
            return;
        };
        if let Ok(read) = std::fs::read_dir(&self.current_dir) {
            for entry in read.flatten() {
                let path = entry.path();
                let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
                // Directory selection exposes folders only. File modes keep
                // directories for navigation and filter ordinary files.
                if !is_dir && (self.mode == FilePickerMode::Directory || !filter.matches(&path)) {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                let meta = entry.metadata().ok();
                self.entries.push(FilePickerEntry {
                    name,
                    path,
                    is_dir,
                    size: meta.as_ref().map_or(0, |m| m.len()),
                    modified: meta
                        .and_then(|m| m.modified().ok())
                        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).timestamp()),
                });
            }
        }
        // Directories first, then files, each alphabetical.
        self.entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        });
        self.ensure_valid_focus();
    }

    /// Title, then the path bar, in the dialog's top band.
    fn header_h(&self) -> f64 {
        let m = self.metrics;
        m.pad + m.title_h + m.gap + m.control_h + m.section
    }

    /// The Save name row (when there is one) and the button row.
    fn footer_h(&self) -> f64 {
        let m = self.metrics;
        let buttons = 2.0 * m.pad + m.control_h;
        if self.mode == FilePickerMode::Save {
            buttons + m.control_h + m.section
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

    /// The column headings' band.
    fn columns_rect(&self) -> Rect {
        let top = self.rect.y0 + self.header_h();
        Rect::new(
            self.content_x0(),
            top,
            self.rect.x1 - self.metrics.pad,
            top + self.metrics.column_h,
        )
    }

    /// The rows' area, under the column headings.
    fn list_rect(&self) -> Rect {
        let columns = self.columns_rect();
        Rect::new(
            columns.x0,
            columns.y1,
            columns.x1,
            (self.rect.y1 - self.footer_h() - self.metrics.gap).max(columns.y1),
        )
    }

    /// The path bar: breadcrumbs in a field, right of the up button.
    fn path_rect(&self) -> Rect {
        let m = self.metrics;
        let y0 = self.rect.y0 + m.pad + m.title_h + m.gap;
        Rect::new(
            self.rect.x0 + m.pad + m.control_h + m.gap,
            y0,
            self.rect.x1 - m.pad,
            y0 + m.control_h,
        )
    }

    /// Which Name / Size / Modified columns the list is wide enough for.
    fn shows_columns(&self) -> (bool, bool) {
        let width = self.list_rect().width();
        let m = self.metrics;
        (width >= m.columns_min_w * 0.7, width >= m.columns_min_w)
    }

    fn visible_rows(&self) -> usize {
        let rows = (self.list_rect().height() / self.metrics.row_h).floor();
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "rows is clamped into 0.0..=f64::from(u32::MAX) immediately below, so the \
                      conversion is provably exact"
        )]
        let rows = rows.clamp(0.0, f64::from(u32::MAX)) as usize;
        rows
    }

    fn scroll_to(&mut self, i: usize) {
        let rows = self.visible_rows().max(1);
        if i < self.first_visible {
            self.first_visible = i;
        } else if i >= self.first_visible.saturating_add(rows) {
            self.first_visible = i.saturating_add(1).saturating_sub(rows);
        }
    }

    fn clamp_scroll(&mut self) {
        let rows = self.visible_rows();
        let max_first = self.entries.len().saturating_sub(rows);
        if self.first_visible > max_first {
            self.first_visible = max_first;
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
        let filter_x0 = self.rect.x0 + m.pad;
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

        let up_x0 = self.rect.x0 + m.pad;
        let up_y0 = self.rect.y0 + m.pad + m.title_h + m.gap;
        self.up_rect = Rect::new(up_x0, up_y0, up_x0 + m.control_h, up_y0 + m.control_h);

        self.crumb_rects = self.compute_crumb_rects();
        self.layout_places();

        self.clamp_scroll();
        let list = self.list_rect();
        let rows = self.visible_rows();
        self.entry_rows.clear();
        for vis in 0..rows {
            let i = self.first_visible.saturating_add(vis);
            if i >= self.entries.len() {
                break;
            }
            // vis is bounded by `rows`, an on-screen row count derived
            // from the dialog's pixel height — nowhere near f64's
            // exact-integer range.
            #[allow(
                clippy::cast_precision_loss,
                reason = "vis is a small on-screen row index"
            )]
            let y0 = list.y0 + (vis as f64) * m.row_h;
            self.entry_rows
                .push((i, Rect::new(list.x0, y0, list.x1, y0 + m.row_h)));
        }
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
        let mut targets = Vec::with_capacity(6);
        if self.can_go_up() {
            targets.push(PickerFocus::Up);
        }
        if self.has_sidebar() {
            targets.push(PickerFocus::Places);
        }
        if !self.entries.is_empty() {
            targets.push(PickerFocus::List);
        }
        if self.mode != FilePickerMode::Directory && self.filters.len() > 1 {
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
            // start on the listbox so arrows/type-ahead work immediately.
            self.focus = if self.mode == FilePickerMode::Save {
                targets
                    .iter()
                    .copied()
                    .find(|target| *target == PickerFocus::Filename)
                    .or_else(|| targets.first().copied())
            } else {
                // Open dialogs start on the list so arrows and type-ahead
                // work at once.
                targets
                    .iter()
                    .copied()
                    .find(|target| *target == PickerFocus::List)
                    .or_else(|| targets.first().copied())
            };
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
    }

    fn focus_control(&mut self, focus: PickerFocus) {
        if self.focus_targets().contains(&focus) {
            self.focus = Some(focus);
            self.has_keyboard_focus = true;
        }
        self.sync_filename_focus();
    }

    /// The filename field holds the caret exactly while it is the focused
    /// control of a focused picker.
    fn sync_filename_focus(&mut self) {
        let focused = self.has_keyboard_focus && self.focus == Some(PickerFocus::Filename);
        if self.filename.is_focused() != focused {
            Widget::set_focus(&mut self.filename, focused);
        }
    }

    /// Whether `event` is typing for the focused filename field rather than
    /// the picker's own navigation (Tab, Escape, Enter).
    fn is_filename_input(&self, event: &WindowEvent) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if self.mode != FilePickerMode::Save
            || self.focus != Some(PickerFocus::Filename)
            || self.replace_prompt.is_some()
        {
            return false;
        }
        match event {
            WindowEvent::Ime(_) => true,
            WindowEvent::KeyboardInput { event, .. } => !matches!(
                event.logical_key,
                Key::Named(NamedKey::Tab | NamedKey::Escape | NamedKey::Enter)
            ),
            _ => false,
        }
    }

    fn can_commit_open(&self) -> bool {
        match self.mode {
            FilePickerMode::Open => self.validated_selected_files().is_some(),
            FilePickerMode::Save => {
                (!self.filename().trim().is_empty()
                    && self.safe_save_target(self.filename().trim()).is_some())
                    || self.first_selected_file().is_some()
            }
            FilePickerMode::Directory => self.validated_directory_choice().is_some(),
        }
    }

    /// Every visible control belongs in the semantic tree.  Unlike
    /// `focus_targets`, this includes a disabled confirmation button so AT
    /// can explain why it cannot yet be activated.
    fn a11y_controls(&self) -> Vec<PickerFocus> {
        let mut controls = Vec::with_capacity(6);
        if self.can_go_up() {
            controls.push(PickerFocus::Up);
        }
        if self.has_sidebar() {
            controls.push(PickerFocus::Places);
        }
        if !self.entries.is_empty() {
            controls.push(PickerFocus::List);
        }
        if self.mode != FilePickerMode::Directory && self.filters.len() > 1 {
            controls.push(PickerFocus::Filter);
        }
        if self.mode == FilePickerMode::Save {
            controls.push(PickerFocus::Filename);
        }
        controls.extend([PickerFocus::Cancel, PickerFocus::Confirm]);
        controls
    }

    /// The place the current folder is inside, the deepest if several.
    fn active_place(&self) -> Option<usize> {
        self.places
            .iter()
            .enumerate()
            .filter(|(_, place)| self.current_dir.starts_with(&place.path))
            .max_by_key(|(_, place)| place.path.components().count())
            .map(|(i, _)| i)
    }

    fn go_place(&mut self, index: usize) {
        if let Some(path) = self.places.get(index).map(|p| p.path.clone()) {
            self.place_cursor = Some(index);
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
            self.first_visible = 0;
            self.refresh_entries();
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

    /// Each segment maps a display label to the ancestor directory it
    /// navigates to. Only accessible ancestors get a segment, so the user
    /// never sees one they can't navigate to. A path too long for the bar
    /// drops its leading segments; the current folder always shows.
    fn compute_crumb_rects(&self) -> Vec<(PathBuf, Rect)> {
        let mut segments: Vec<(String, PathBuf)> = Vec::new();
        let comps: Vec<_> = self.current_dir.components().collect();
        let mut acc = PathBuf::new();
        for c in comps {
            acc.push(c.as_os_str());
            if self.can_navigate_to(&acc) {
                let label = c.as_os_str().to_string_lossy().into_owned();
                let label = if label.is_empty() {
                    "/".to_string()
                } else {
                    label
                };
                segments.push((label, acc.clone()));
            }
        }
        // No TextRenderer here: a segment measured at a previous paint uses
        // its recorded width, anything else the per-glyph estimate until the
        // first paint measures it.
        let m = self.metrics;
        let bar = self.path_rect();
        let widths: Vec<f64> = segments
            .iter()
            .map(|(label, _)| {
                // A breadcrumb label is one path component — far below f64's
                // exact-integer range even for pathological path names.
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "label is a single path component's display text"
                )]
                let char_count = label.chars().count() as f64;
                let text_w = self
                    .crumb_widths
                    .get(label)
                    .copied()
                    .unwrap_or(char_count * m.crumb_glyph_estimate);
                text_w + m.crumb_separator_w + m.gap
            })
            .collect();
        let avail = (bar.width() - m.inset_x).max(0.0);
        let mut total: f64 = widths.iter().sum();
        let mut first: usize = 0;
        while first.saturating_add(1) < segments.len() && total > avail {
            total -= widths.get(first).copied().unwrap_or(0.0);
            first = first.saturating_add(1);
        }
        let mut x = bar.x0 + m.inset_x;
        let mut out = Vec::new();
        for ((_, target), w) in segments.into_iter().zip(widths).skip(first) {
            let x1 = (x + w).min(bar.x1);
            out.push((target, Rect::new(x, bar.y0, x1, bar.y1)));
            x += w;
        }
        out
    }

    /// Measure what has no size until text is shaped — every breadcrumb
    /// label, the button labels and the type menu's widest entry — with the
    /// renderer the frame draws with, and re-lay the picker out from those
    /// widths. Hit rects therefore match the painted glyphs, and stay matched
    /// when an event handler rebuilds the layout without a renderer. Runs once
    /// per paint.
    fn measure_text(&mut self, text_renderer: &mut TextRenderer, theme: &Theme) {
        let m = self.metrics;
        let style = theme
            .text(TextRole::Label)
            .with_weight(crate::text_render::TextWeight::Medium);
        let labels: Vec<String> = (0..self.crumb_rects.len())
            .filter_map(|idx| self.crumb_label_for(idx))
            .collect();
        let mut changed = false;
        for label in labels {
            let (tw, _) = text_renderer.measure_style(&label, None, &style);
            let measured = f64::from(tw);
            if self.crumb_widths.get(&label) != Some(&measured) {
                self.crumb_widths.insert(label, measured);
                changed = true;
            }
        }
        let mut face = |label: &str, primary: bool| {
            footer_button(Rect::ZERO, label, primary).intrinsic_width(text_renderer, theme)
        };
        let widths = [
            face(CANCEL_KEY, false),
            face(&self.confirm_key(), true),
            face(REPLACE_KEY, false),
        ]
        .map(|w| w.max(m.min_button_w));
        if widths != self.button_w {
            self.button_w = widths;
            changed = true;
        }
        let chip = theme.text(TextRole::Body);
        let widest = self
            .filters
            .iter()
            .map(|f| {
                let (w, _) = text_renderer.measure_style(&self.filter_display(f), None, &chip);
                f64::from(w)
            })
            .fold(0.0_f64, f64::max);
        let filter_w = (widest + 2.0 * m.inset_x + m.icon_sm + m.gap).max(m.min_button_w);
        if (filter_w - self.filter_w).abs() > f64::EPSILON {
            self.filter_w = filter_w;
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
        if metrics != self.metrics {
            self.metrics = metrics;
            self.apply_layout_helper();
        }
    }

    /// Resolve the mode-specific confirmation action after revalidating the
    /// selected paths (or the typed Save filename).
    fn commit_open(&mut self) {
        match self.mode {
            FilePickerMode::Save => {
                let name = self.filename().trim();
                let target = if name.is_empty() {
                    self.first_selected_file()
                } else {
                    self.safe_save_target(name)
                };
                match target {
                    Some(target) => self.commit_save(target),
                    None => self.pending_outcome = Some(FilePickerOutcome::Cancelled),
                }
            }
            FilePickerMode::Open => {
                if let Some(paths) = self.validated_selected_files() {
                    self.pending_outcome = Some(FilePickerOutcome::Selected(paths));
                }
            }
            FilePickerMode::Directory => {
                if let Some(paths) = self.validated_directory_choice() {
                    self.pending_outcome = Some(FilePickerOutcome::Selected(paths));
                }
            }
        }
    }

    /// Save to `target` — unless something is already there. A folder is
    /// opened, as typing a folder's name does in every save dialog; an
    /// existing file is replaced only once the person says so.
    fn commit_save(&mut self, target: PathBuf) {
        match fs::metadata(&target) {
            Ok(meta) if meta.is_dir() => {
                if self.can_navigate_to(&target) {
                    self.navigate_to(&target);
                    self.set_filename("");
                    self.focus_control(PickerFocus::Filename);
                }
            }
            // A dangling symlink is still a name taken in this folder.
            _ if fs::symlink_metadata(&target).is_ok() => {
                self.replace_prompt = Some(target);
                self.hover_prompt = None;
                // Cancel holds focus: Enter on its own never destroys a file.
                self.focus = Some(PickerFocus::KeepExisting);
                self.has_keyboard_focus = true;
                self.sync_filename_focus();
                self.apply_layout_helper();
            }
            _ => self.pending_outcome = Some(FilePickerOutcome::Selected(vec![target])),
        }
    }

    /// Resolve a Save filename without allowing the editable footer to turn
    /// into an unrestricted path input. The name must be one ordinary path
    /// component, and the real parent directory must still be beneath one of
    /// the real chooser roots (symlink navigation must not escape the scope).
    fn safe_save_target(&self, name: &str) -> Option<PathBuf> {
        let mut components = Path::new(name).components();
        let component = components.next()?;
        if components.next().is_some() || !matches!(component, std::path::Component::Normal(_)) {
            return None;
        }
        let current = fs::canonicalize(&self.current_dir).ok()?;
        let inside_root = current.is_dir() && self.canonical_path_is_inside_roots(&current);
        inside_root.then(|| self.current_dir.join(name))
    }

    fn first_selected_file(&self) -> Option<PathBuf> {
        self.selection
            .selected()
            .into_iter()
            .filter_map(|i| self.entries.get(i))
            .find(|e| !e.is_dir)
            .and_then(|e| self.safe_existing_file(&e.path))
    }

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
            .map(|entry| {
                (!entry.is_dir)
                    .then(|| self.safe_existing_file(&entry.path))
                    .flatten()
            })
            .collect()
    }

    fn validated_directory_choice(&self) -> Option<Vec<PathBuf>> {
        let selected = self.selection.selected();
        if selected.is_empty() {
            return self
                .safe_existing_directory(&self.current_dir)
                .map(|path| vec![path]);
        }
        selected
            .into_iter()
            .map(|index| {
                let entry = self.entries.get(index)?;
                entry
                    .is_dir
                    .then(|| self.safe_existing_directory(&entry.path))
                    .flatten()
            })
            .collect()
    }

    fn safe_existing_file(&self, candidate: &Path) -> Option<PathBuf> {
        let canonical = fs::canonicalize(candidate).ok()?;
        (canonical.is_file() && self.canonical_path_is_inside_roots(&canonical))
            .then(|| candidate.to_path_buf())
    }

    fn safe_existing_directory(&self, candidate: &Path) -> Option<PathBuf> {
        let canonical = fs::canonicalize(candidate).ok()?;
        (canonical.is_dir() && self.canonical_path_is_inside_roots(&canonical))
            .then(|| candidate.to_path_buf())
    }

    fn canonical_path_is_inside_roots(&self, canonical: &Path) -> bool {
        self.accessible_roots.iter().any(|root| {
            fs::canonicalize(root).is_ok_and(|canonical_root| {
                canonical_root.is_dir() && canonical.starts_with(canonical_root)
            })
        })
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

    fn type_ahead(&mut self, ch: char) {
        let lower = ch.to_ascii_lowercase();
        let start = self.selection.primary().map_or(0, |i| i.saturating_add(1));
        let n = self.entries.len();
        for off in 0..n {
            let i = start.saturating_add(off).checked_rem(n).unwrap_or(0);
            let Some(entry) = self.entries.get(i) else {
                continue;
            };
            if entry
                .name
                .chars()
                .next()
                .is_some_and(|c| c.to_ascii_lowercase() == lower)
            {
                self.selection.select_only(i);
                self.scroll_to(i);
                return;
            }
        }
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
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
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
        self.apply_layout_helper();
        self.measure_text(text_renderer, theme);
        let tokens = &theme.tokens;
        // A floating dialog: the panel radius, the theme's elevation and the
        // same ground and edge as every other panel (HIG §9.1/§9.2).
        let radius = f64::from(tokens.radii.panel);
        let frame = RoundedRect::from_rect(self.rect, radius);
        crate::chrome::shell::panel_shadow(scene, transform, self.rect, radius, theme, 1.0);
        scene.fill(
            Fill::NonZero,
            transform,
            crate::chrome::panel_ground(theme),
            None,
            &frame,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::chrome::panel_border(theme),
            None,
            &crate::surface::edge_outline(self.rect, radius),
        );

        self.draw_header_and_breadcrumbs(scene, transform, text_renderer, theme);
        self.draw_sidebar(scene, transform, text_renderer, theme);
        self.draw_entry_rows(scene, transform, text_renderer, theme);
        self.draw_footer(scene, transform, text_renderer, theme, presence);
        self.draw_filter_menu(scene, transform, text_renderer, theme);
        self.draw_replace_prompt(scene, transform, text_renderer, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.replace_prompt.is_some() {
            self.on_replace_prompt_event(event, ctx);
            return;
        }
        if self.is_filename_input(event) {
            self.has_keyboard_focus = true;
            self.sync_filename_focus();
            self.filename.handle_event(event, ctx);
            self.ensure_valid_focus();
            *ctx.handled = true;
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.on_cursor_moved(p, ctx.handled);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.on_mouse_wheel(delta, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.on_left_click(*ctx.modifiers, ctx.handled);
            }
            WindowEvent::KeyboardInput { event: ke, .. } if ke.state == ElementState::Pressed => {
                self.on_key_pressed(ke, *ctx.modifiers, ctx.handled);
            }
            _ => {}
        }
        self.sync_filename_focus();
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
                        .with_state(AccessState {
                            focusable: true,
                            focused: self.has_keyboard_focus && self.focus == Some(control),
                            ..Default::default()
                        }),
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
                            .with_state(AccessState {
                                focusable: true,
                                focused: self.has_keyboard_focus && self.focus == Some(control),
                                ..Default::default()
                            })
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
                                    .with_state(AccessState {
                                        focusable: true,
                                        selected: active == Some(index),
                                        ..Default::default()
                                    })
                            })
                            .collect();
                        AccessNode::new(Role::List)
                            .with_name(tr("widget-filepicker-places"))
                            .with_key(control.id())
                            .with_bounds(self.sidebar_rect())
                            .with_state(AccessState {
                                focusable: true,
                                focused: self.has_keyboard_focus && self.focus == Some(control),
                                ..Default::default()
                            })
                            .with_children(rows)
                    }
                    PickerFocus::Filter => AccessNode::new(Role::ComboBox)
                        .with_name(tr("widget-filepicker-file-type"))
                        .with_value(self.active_filter_label())
                        .with_key(control.id())
                        .with_bounds(self.filter_rect)
                        .with_state(AccessState {
                            focusable: true,
                            focused: self.has_keyboard_focus && self.focus == Some(control),
                            expanded: Some(self.filter_open),
                            ..Default::default()
                        }),
                    PickerFocus::Filename => AccessNode::new(Role::TextInput)
                        .with_name(tr("widget-filepicker-name-accessible"))
                        .with_value(self.filename().to_owned())
                        .with_text_selection(filename_anchor, filename_cursor)
                        .with_key(control.id())
                        .with_bounds(self.filename_rect)
                        .with_state(AccessState {
                            focusable: true,
                            focused: self.has_keyboard_focus && self.focus == Some(control),
                            ..Default::default()
                        }),
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
                            .with_state(AccessState {
                                focusable: enabled,
                                focused: enabled
                                    && self.has_keyboard_focus
                                    && self.focus == Some(control),
                                disabled: !enabled,
                                ..Default::default()
                            })
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
        Some(
            AccessNode::new(Role::Dialog)
                .with_name(self.title.clone())
                .with_state(AccessState {
                    modal: true,
                    focusable: true,
                    focused: self.has_keyboard_focus,
                    ..Default::default()
                })
                .with_children(controls),
        )
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.has_keyboard_focus = focused;
        if focused {
            self.ensure_valid_focus();
        }
        self.sync_filename_focus();
    }

    fn is_focused(&self) -> bool {
        self.has_keyboard_focus
    }

    fn traps_focus(&self) -> bool {
        true
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        Widget::ime_cursor_rect(&self.filename)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        self.filename.ime_purpose()
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.filename.ime_cursor_rect()?;
        self.filename.ime_edit_fingerprint(key)
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
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        matches!(action, crate::a11y::Action::Activate) && self.activate_focus()
    }

    fn perform_action_at(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
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
    /// The name of the file the replace prompt is about.
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
                .with_state(AccessState {
                    focusable: true,
                    focused: self.has_keyboard_focus && self.focus == Some(control),
                    ..Default::default()
                })
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
                .with_state(AccessState {
                    modal: true,
                    focusable: true,
                    focused: self.has_keyboard_focus,
                    ..Default::default()
                })
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

    /// Title, up button and the path bar's breadcrumbs. Split out of `draw`
    /// to keep that function readable.
    fn draw_header_and_breadcrumbs(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let m = self.metrics;
        text_renderer.draw_style(
            scene,
            &self.title,
            tokens.colors.text_primary,
            transform.pre_translate(Vec2::new(self.rect.x0 + m.pad, self.rect.y0 + m.pad)),
            Some((self.rect.width() - 2.0 * m.pad).max(0.0) as f32),
            &theme.text(TextRole::Heading),
        );

        let can_up = self.can_go_up();
        let up = self.up_rect;
        let shape = RoundedRect::from_rect(up, f64::from(tokens.radii.control));
        if can_up && up.contains(self.last_cursor) {
            scene.fill(
                Fill::NonZero,
                transform,
                tokens.colors.intent.selection_hover,
                None,
                &shape,
            );
        }
        if can_up && self.has_keyboard_focus && self.focus == Some(PickerFocus::Up) {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                tokens.colors.border_focus,
                None,
                &shape,
            );
        }
        draw_icon(
            scene,
            transform,
            centered(up, m.icon_md),
            Icon::ArrowUp,
            if can_up {
                tokens.colors.text_primary
            } else {
                tokens.colors.text_dim
            },
            LUCIDE_DEFAULT_STROKE_PX,
        );

        // The path bar is a field-like well, so the crumbs read as a place
        // rather than as loose text.
        let bar = self.path_rect();
        let well = RoundedRect::from_rect(bar, f64::from(tokens.radii.control));
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.surfaces.base,
            None,
            &well,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::surface::edge_on(tokens.colors.surfaces.base),
            None,
            &well,
        );
        let crumb_style = theme.text(TextRole::Label);
        let current = crumb_style
            .clone()
            .with_weight(crate::text_render::TextWeight::Medium);
        let crumb_count = self.crumb_rects.len();
        for (idx, (_target, r)) in self.crumb_rects.iter().enumerate() {
            let label = self.crumb_label_for(idx).unwrap_or_default();
            let is_current = idx.saturating_add(1) == crumb_count;
            let color = if is_current || r.contains(self.last_cursor) {
                tokens.colors.text_primary
            } else {
                tokens.colors.text_dim
            };
            let style = if is_current { &current } else { &crumb_style };
            let (_, th) = text_renderer.measure_style(&label, None, style);
            text_renderer.draw_style(
                scene,
                &label,
                color,
                transform.pre_translate(Vec2::new(r.x0, r.center().y - f64::from(th) / 2.0)),
                None,
                style,
            );
            if !is_current {
                let sep = Rect::new(
                    r.x1 - m.crumb_separator_w - m.gap / 2.0,
                    r.center().y - m.icon_sm / 2.0,
                    r.x1 - m.gap / 2.0,
                    r.center().y + m.icon_sm / 2.0,
                );
                draw_icon(
                    scene,
                    transform,
                    sep,
                    Icon::ChevronRight,
                    tokens.colors.text_dim,
                    LUCIDE_DEFAULT_STROKE_PX,
                );
            }
        }
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

    /// The column headings and the visible entry rows, or the empty state.
    /// Split out of `draw` to keep that function readable.
    fn draw_entry_rows(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let m = self.metrics;
        let ground = crate::chrome::panel_ground(theme);
        let (show_size, show_date) = self.shows_columns();
        let columns = self.columns_rect();
        let list = self.list_rect();
        let date_x0 = if show_date {
            list.x1 - m.inset_x - m.date_w
        } else {
            list.x1
        };
        let size_x1 = if show_date {
            date_x0 - m.pad
        } else {
            list.x1 - m.inset_x
        };
        let name_x0 = list.x0 + m.inset_x + m.icon_md + m.gap;
        let name_x1 = if show_size {
            size_x1 - m.size_w - m.gap
        } else {
            list.x1 - m.inset_x
        };

        // Column headings.
        let head = theme.text(TextRole::Caption);
        let head = head.with_weight(crate::text_render::TextWeight::Medium);
        let head_y = columns.center().y;
        let mut heading = |scene: &mut Scene, key: &str, x: f64, right: Option<f64>| {
            let label = tr(key);
            let (w, h) = text_renderer.measure_style(&label, None, &head);
            let x = right.map_or(x, |r| r - f64::from(w));
            text_renderer.draw_style(
                scene,
                &label,
                tokens.colors.text_dim,
                transform.pre_translate(Vec2::new(x, head_y - f64::from(h) / 2.0)),
                None,
                &head,
            );
        };
        heading(scene, "widget-filepicker-column-name", name_x0, None);
        if show_size {
            heading(scene, "widget-filepicker-column-size", 0.0, Some(size_x1));
        }
        if show_date {
            heading(scene, "widget-filepicker-column-modified", date_x0, None);
        }
        scene.fill(
            Fill::NonZero,
            transform,
            crate::tokens::Colors::hairline_on(ground, crate::chrome::HAIRLINE_ALPHA),
            None,
            &Rect::new(columns.x0, columns.y1 - 1.0, columns.x1, columns.y1),
        );

        if self.entries.is_empty() {
            let style = theme.text(TextRole::Body);
            let message = tr("widget-filepicker-empty");
            let (w, h) = text_renderer.measure_style(&message, None, &style);
            text_renderer.draw_style(
                scene,
                &message,
                tokens.colors.text_dim,
                transform.pre_translate(Vec2::new(
                    list.center().x - f64::from(w) / 2.0,
                    list.center().y - f64::from(h) / 2.0,
                )),
                None,
                &style,
            );
            return;
        }

        let name_style = theme.text(TextRole::Body);
        let meta_style = theme
            .text(TextRole::Label)
            .with_kind(crate::text_render::TextKind::Numeric);
        for (i, rect) in &self.entry_rows {
            let i = *i;
            let is_selected = self.selection.is_selected(i);
            let is_hover = self.hover_entry == Some(i) && !is_selected;
            // HIG §9.3: an idle row is transparent and hover is a lift; only
            // the selected row carries a fill. The lift is a hairline wash
            // measured against the panel ground the row sits on.
            let bg = if is_selected {
                Some(tokens.colors.lift_on(tokens.colors.surfaces.overlay, 1.0))
            } else if is_hover {
                Some(crate::tokens::Colors::hairline_on(
                    ground,
                    crate::chrome::HAIRLINE_ALPHA,
                ))
            } else {
                None
            };
            let row_shape = RoundedRect::from_rect(*rect, f64::from(tokens.radii.control));
            if let Some(bg) = bg {
                scene.fill(Fill::NonZero, transform, bg, None, &row_shape);
            }
            let list_cursor = self
                .selection
                .primary()
                .or_else(|| self.entry_rows.first().map(|(first, _)| *first));
            if self.has_keyboard_focus
                && self.focus == Some(PickerFocus::List)
                && list_cursor == Some(i)
            {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    tokens.colors.border_focus,
                    None,
                    &row_shape,
                );
            }
            let Some(entry) = self.entries.get(i) else {
                continue;
            };
            let fg = tokens.colors.text_primary;
            let icon_rect = Rect::new(
                rect.x0 + m.inset_x,
                rect.center().y - m.icon_md / 2.0,
                rect.x0 + m.inset_x + m.icon_md,
                rect.center().y + m.icon_md / 2.0,
            );
            draw_icon(
                scene,
                transform,
                icon_rect,
                icon_for_path(&entry.path, entry.is_dir),
                if entry.is_dir {
                    fg
                } else {
                    tokens.colors.text_dim
                },
                LUCIDE_DEFAULT_STROKE_PX,
            );
            let name = text_renderer.elide_to_width_style(
                &entry.name,
                px((name_x1 - name_x0).max(0.0)),
                &name_style,
            );
            let (_, name_h) = text_renderer.measure_style(&name, None, &name_style);
            text_renderer.draw_style(
                scene,
                &name,
                fg,
                transform.pre_translate(Vec2::new(
                    name_x0,
                    rect.center().y - f64::from(name_h) / 2.0,
                )),
                None,
                &name_style,
            );
            let mut meta = |scene: &mut Scene, text: &str, x: f64, right: bool| {
                let (w, h) = text_renderer.measure_style(text, None, &meta_style);
                let x = if right { x - f64::from(w) } else { x };
                text_renderer.draw_style(
                    scene,
                    text,
                    tokens.colors.text_dim,
                    transform.pre_translate(Vec2::new(x, rect.center().y - f64::from(h) / 2.0)),
                    None,
                    &meta_style,
                );
            };
            if show_size && !entry.is_dir {
                meta(scene, &crate::format::bytes_si(entry.size), size_x1, true);
            }
            if show_date && let Some(modified) = entry.modified {
                let Some(when) = chrono::DateTime::from_timestamp(modified, 0)
                    .map(|utc| utc.with_timezone(&chrono::Local))
                else {
                    continue;
                };
                meta(
                    scene,
                    &when.format("%Y-%m-%d %H:%M").to_string(),
                    date_x0,
                    false,
                );
            }
        }

        // A thin scroll thumb in the right margin when the folder overflows.
        let rows = self.visible_rows();
        let total = self.entries.len();
        if total > rows && rows > 0 {
            #[allow(
                clippy::cast_precision_loss,
                reason = "entry counts are far below f64's exact-integer range"
            )]
            let (rows_f, total_f, first_f) = (rows as f64, total as f64, self.first_visible as f64);
            let thumb_h = (list.height() * rows_f / total_f).max(m.row_h / 2.0);
            let travel = (list.height() - thumb_h).max(0.0);
            let max_first = (total_f - rows_f).max(1.0);
            let y0 = list.y0 + travel * (first_f / max_first).clamp(0.0, 1.0);
            let x = self.rect.x1 - m.pad / 2.0 - m.scrollbar_w / 2.0;
            scene.fill(
                Fill::NonZero,
                transform,
                crate::tokens::Colors::hairline_on(ground, 72),
                None,
                &RoundedRect::from_rect(
                    Rect::new(x, y0, x + m.scrollbar_w, y0 + thumb_h),
                    m.scrollbar_w / 2.0,
                ),
            );
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
        }
        if self.mode != FilePickerMode::Directory && self.filters.len() > 1 {
            self.draw_filter_chip(scene, transform, text_renderer, theme);
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
            if self.has_keyboard_focus && self.focus == Some(PickerFocus::Filename) {
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

    /// The type dropdown's chip: the active filter and a chevron, shown only
    /// when there is more than one filter to choose from.
    fn draw_filter_chip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let m = self.metrics;
        let hovered = self.filter_rect.contains(self.last_cursor) || self.filter_open;
        let s = RoundedRect::from_rect(self.filter_rect, f64::from(tokens.radii.control));
        let fill = if hovered {
            tokens.colors.lift_on(tokens.colors.surfaces.raised, 0.5)
        } else {
            tokens.colors.surfaces.raised
        };
        scene.fill(Fill::NonZero, transform, fill, None, &s);
        // Rest edge derived from the face actually painted (HIG §9.5).
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::surface::edge_on(fill),
            None,
            &s,
        );
        if self.has_keyboard_focus && self.focus == Some(PickerFocus::Filter) {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                tokens.colors.border_focus,
                None,
                &s,
            );
        }
        let style = theme.text(TextRole::Body);
        let chevron = Rect::new(
            self.filter_rect.x1 - m.inset_x - m.icon_sm,
            self.filter_rect.center().y - m.icon_sm / 2.0,
            self.filter_rect.x1 - m.inset_x,
            self.filter_rect.center().y + m.icon_sm / 2.0,
        );
        draw_icon(
            scene,
            transform,
            chevron,
            if self.filter_open {
                Icon::ChevronUp
            } else {
                Icon::ChevronDown
            },
            tokens.colors.text_dim,
            LUCIDE_DEFAULT_STROKE_PX,
        );
        let active = self
            .filters
            .get(self.active_filter)
            .map(|f| self.filter_display(f))
            .unwrap_or_default();
        let label = text_renderer.elide_to_width_style(
            &active,
            px((chevron.x0 - m.gap - self.filter_rect.x0 - m.inset_x).max(0.0)),
            &style,
        );
        let (_, th) = text_renderer.measure_style(&label, None, &style);
        text_renderer.draw_style(
            scene,
            &label,
            tokens.colors.text_primary,
            transform.pre_translate(Vec2::new(
                self.filter_rect.x0 + m.inset_x,
                self.filter_rect.center().y - f64::from(th) / 2.0,
            )),
            None,
            &style,
        );
    }

    /// The display label for breadcrumb segment `idx` (final path
    /// component of its target).
    fn crumb_label_for(&self, idx: usize) -> Option<String> {
        let (target, _) = self.crumb_rects.get(idx)?;
        let label = target
            .file_name()
            .map_or_else(|| "/".to_string(), |n| n.to_string_lossy().into_owned());
        Some(label)
    }

    /// `WindowEvent::CursorMoved` handling. Split out of `handle_event` to
    /// keep that function readable.
    fn on_cursor_moved(&mut self, p: Point, handled: &mut bool) {
        self.last_cursor = p;
        let new_button = if self.cancel_rect.contains(p) {
            Some(0)
        } else if self.open_rect.contains(p) {
            Some(1)
        } else {
            None
        };
        let new_entry = self
            .entry_rows
            .iter()
            .find(|(_, r)| r.contains(p))
            .map(|(i, _)| *i);
        let new_place = self.place_rects.iter().position(|r| r.contains(p));
        if self.filter_open
            && let Some(row) = self.filter_rows.iter().position(|r| r.contains(p))
            && self.filter_hover != Some(row)
        {
            self.filter_hover = Some(row);
            *handled = true;
        }
        if new_button != self.hover_button
            || new_entry != self.hover_entry
            || new_place != self.hover_place
        {
            self.hover_button = new_button;
            self.hover_entry = new_entry;
            self.hover_place = new_place;
            *handled = true;
        }
    }

    /// `WindowEvent::MouseWheel` handling. Split out of `handle_event` to
    /// keep that function readable.
    fn on_mouse_wheel(&mut self, delta: &winit::event::MouseScrollDelta, handled: &mut bool) {
        let dy = match delta {
            winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
            winit::event::MouseScrollDelta::PixelDelta(pos) => pos.y / self.metrics.row_h,
        };
        // Wheel-up scrolls the list up (toward the start). `rows` is
        // clamped into 0.0..=f64::from(u32::MAX) immediately before each
        // cast below, so the conversion is provably exact.
        if dy > 0.0 {
            let rows = dy.ceil().clamp(0.0, f64::from(u32::MAX));
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "rows is clamped into 0.0..=f64::from(u32::MAX) immediately above"
            )]
            let rows = rows as usize;
            self.first_visible = self.first_visible.saturating_sub(rows);
        } else if dy < 0.0 {
            let rows = (-dy).ceil().clamp(0.0, f64::from(u32::MAX));
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "rows is clamped into 0.0..=f64::from(u32::MAX) immediately above"
            )]
            let rows = rows as usize;
            self.first_visible = self.first_visible.saturating_add(rows);
        }
        self.clamp_scroll();
        // Refresh the visible-row rects so hit-testing + entry_rect
        // reflect the new scroll position before the next draw.
        self.apply_layout_helper();
        *handled = true;
    }

    /// Left-button `WindowEvent::MouseInput` handling. Split out of
    /// `handle_event` to keep that function readable.
    fn on_left_click(&mut self, modifiers: winit::event::Modifiers, handled: &mut bool) {
        let p = self.last_cursor;
        if self.filter_open {
            self.filter_open = false;
            if let Some(row) = self.filter_rows.iter().position(|r| r.contains(p)) {
                self.set_filter(row);
                *handled = true;
                return;
            }
            // A press on the chip that opened the menu closes it.
            if self.filter_rect.contains(p) {
                *handled = true;
                return;
            }
        }
        if self.cancel_rect.contains(p) {
            self.focus_control(PickerFocus::Cancel);
            self.click_cancel();
            *handled = true;
            return;
        }
        if self.open_rect.contains(p) && self.can_commit_open() {
            self.focus_control(PickerFocus::Confirm);
            self.click_open();
            *handled = true;
            return;
        }
        if self.can_go_up() && self.up_rect.contains(p) {
            self.focus_control(PickerFocus::Up);
            self.go_up();
            *handled = true;
            return;
        }
        if self.mode != FilePickerMode::Directory
            && self.filters.len() > 1
            && self.filter_rect.contains(p)
        {
            self.focus_control(PickerFocus::Filter);
            self.toggle_filter_menu();
            *handled = true;
            return;
        }
        if let Some(i) = self.place_rects.iter().position(|r| r.contains(p)) {
            self.focus_control(PickerFocus::Places);
            self.go_place(i);
            *handled = true;
            return;
        }
        if self.mode == FilePickerMode::Save && self.filename_rect.contains(p) {
            self.focus_control(PickerFocus::Filename);
            *handled = true;
            return;
        }
        if let Some((target, _)) = self
            .crumb_rects
            .iter()
            .find(|(_, r)| r.contains(p))
            .cloned()
        {
            self.navigate_to(&target);
            *handled = true;
            return;
        }
        if let Some((i, _)) = self.entry_rows.iter().find(|(_, r)| r.contains(p)).copied() {
            self.focus_control(PickerFocus::List);
            let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
            let shift = modifiers.state().shift_key();
            self.activate_entry(i, ctrl, shift);
            *handled = true;
        }
    }

    /// `WindowEvent::KeyboardInput` handling. Split out of `handle_event`
    /// to keep that function readable.
    fn on_key_pressed(
        &mut self,
        ke: &winit::event::KeyEvent,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) {
        use winit::keyboard::{Key, NamedKey};
        let shift = modifiers.state().shift_key();
        self.has_keyboard_focus = true;
        self.ensure_valid_focus();
        if self.filter_open {
            self.on_filter_menu_key(&ke.logical_key);
            *handled = true;
            return;
        }
        match &ke.logical_key {
            Key::Named(NamedKey::Tab) => {
                self.move_focus(shift);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowDown) if self.focus == Some(PickerFocus::Places) => {
                self.move_place(1);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) if self.focus == Some(PickerFocus::Places) => {
                self.move_place(-1);
                *handled = true;
            }
            Key::Named(NamedKey::Escape) => {
                self.click_cancel();
                *handled = true;
            }
            Key::Named(NamedKey::ArrowDown) if self.focus == Some(PickerFocus::List) => {
                self.move_selection(1, shift);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) if self.focus == Some(PickerFocus::List) => {
                self.move_selection(-1, shift);
                *handled = true;
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                match self.focus {
                    Some(PickerFocus::Cancel) => self.click_cancel(),
                    Some(PickerFocus::Confirm) if self.can_commit_open() => self.click_open(),
                    Some(PickerFocus::Up) => self.go_up(),
                    Some(PickerFocus::Filter) => self.toggle_filter_menu(),
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
                }
                *handled = true;
            }
            Key::Named(NamedKey::Backspace) if self.focus == Some(PickerFocus::List) => {
                self.go_up();
                *handled = true;
            }
            Key::Character(s) if self.focus == Some(PickerFocus::List) => {
                if let Some(ch) = s.chars().next() {
                    self.type_ahead(ch);
                    *handled = true;
                }
            }
            _ => {}
        }
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
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
                self.hover_entry = None;
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

// Standard immediate-mode paint-helper signature for this vello renderer
// (scene + transform + text_renderer + theme, plus this draw's own
// content); every parameter is independently load-bearing.
#[allow(
    clippy::fn_params_excessive_bools,
    reason = "immediate-mode paint helper mirrors the canonical ButtonFace state"
)]
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
mod tests {
    use super::*;
    use crate::testing::{Dispatch, dispatch};
    use automerge::AutoCommit;
    use std::fs;
    use winit::event::ElementState;

    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }

    use crate::synthetic_input::mouse_press;

    /// Exercises the real `std::fs::read_dir` path rather than a mock.
    fn temp_tree() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        fs::write(root.join("alpha.txt"), b"a").unwrap();
        fs::write(root.join("beta.txt"), b"b").unwrap();
        fs::create_dir(root.join("nested")).unwrap();
        fs::write(root.join("nested").join("gamma.txt"), b"c").unwrap();
        (dir, root)
    }

    #[test]
    fn picker_opens_as_modal_and_resolves_open_or_cancel() {
        fn accessibility_reports_modal_dialog_with_title_name() {
            use crate::a11y::Role;
            let (_g, root) = temp_tree();
            let picker = FilePicker::new("Open file", &root, vec![root.clone()]);
            let node = Widget::accessibility(&picker).expect("file picker is a dialog");
            assert_eq!(node.role, Role::Dialog);
            assert_eq!(node.name.as_deref(), Some("Open file"));
            assert!(node.state.modal, "the file chooser traps focus");
        }

        fn new_picker_reads_initial_dir_entries() {
            let (_g, root) = temp_tree();
            let picker = FilePicker::new("Open", &root, vec![root.clone()]);
            // nested (dir), alpha.txt, beta.txt — directories sort first.
            assert_eq!(picker.entries().len(), 3);
            assert!(picker.entries()[0].is_dir);
            assert_eq!(picker.entries()[0].name, "nested");
        }

        fn cancel_returns_none() {
            // Cancelled is what the host translates to `oneshot::send(None)`
            // for the FileChooser response.
            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
            picker.click_cancel();
            let outcome = picker.take_outcome();
            assert_eq!(outcome, Some(FilePickerOutcome::Cancelled));
            assert!(picker.take_outcome().is_none());
        }

        fn open_returns_selected_paths() {
            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
            // Pick `alpha.txt` (after directories, so index 1).
            let target = root.join("alpha.txt");
            assert!(picker.select_by_path(&target));
            picker.click_open();
            let outcome = picker.take_outcome();
            assert_eq!(outcome, Some(FilePickerOutcome::Selected(vec![target])));
        }

        fn open_with_no_selection_is_disabled_and_does_not_cancel_the_dialog() {
            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
            picker.click_open();
            assert!(picker.take_outcome().is_none());
            let node = Widget::accessibility(&picker).expect("dialog semantics");
            let open = node
                .children
                .iter()
                .find(|child| child.name.as_deref() == Some("Open"))
                .expect("Open action");
            assert!(open.state.disabled);
            assert!(!open.state.focusable);
        }

        fn escape_cancels() {
            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
            dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Escape));
            assert_eq!(picker.take_outcome(), Some(FilePickerOutcome::Cancelled));
        }

        accessibility_reports_modal_dialog_with_title_name();
        new_picker_reads_initial_dir_entries();
        cancel_returns_none();
        open_returns_selected_paths();
        open_with_no_selection_is_disabled_and_does_not_cancel_the_dialog();
        escape_cancels();
    }

    #[test]
    fn directory_mode_lists_selects_and_chooses_folders() {
        fn directory_mode_can_choose_the_current_empty_folder() {
            let dir = tempfile::tempdir().expect("tempdir");
            let root = dir.path().to_path_buf();
            let mut picker = FilePicker::new("Choose folder", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Directory);

            picker.click_open();

            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![root]))
            );
        }

        fn directory_mode_lists_only_folders_and_chooses_a_selected_child() {
            let (_guard, root) = temp_tree();
            let child = root.join("nested");
            let mut picker = FilePicker::new("Choose folder", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Directory);

            assert_eq!(picker.entries().len(), 1);
            assert_eq!(picker.entries()[0].path, child);
            assert!(picker.entries()[0].is_dir);
            assert!(!picker.select_by_path(&root.join("alpha.txt")));
            assert!(picker.select_by_path(&child));
            picker.click_open();

            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![child]))
            );
        }

        fn directory_mode_single_and_multiple_selection_are_enforced_in_the_widget() {
            let dir = tempfile::tempdir().expect("tempdir");
            let root = dir.path().to_path_buf();
            let first = root.join("a");
            let second = root.join("b");
            fs::create_dir(&first).unwrap();
            fs::create_dir(&second).unwrap();

            let mut single = FilePicker::new("Choose", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Directory)
                .with_multiple(false);
            single.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            for index in [0, 1] {
                let center = single.entry_rect(index).unwrap().center();
                dispatch(&mut single, &cursor_at(center));
                Dispatch::new()
                    .modifiers(ctrl())
                    .send(&mut single, &mouse_press());
            }
            assert_eq!(single.selected_indices(), vec![1]);

            let mut multiple = FilePicker::new("Choose", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Directory)
                .with_multiple(true);
            multiple.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            for index in [0, 1] {
                let center = multiple.entry_rect(index).unwrap().center();
                dispatch(&mut multiple, &cursor_at(center));
                Dispatch::new()
                    .modifiers(ctrl())
                    .send(&mut multiple, &mouse_press());
            }
            assert_eq!(multiple.selected_indices(), vec![0, 1]);

            let mut keyboard_single = FilePicker::new("Choose", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Directory)
                .with_multiple(false);
            keyboard_single.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            dispatch(
                &mut keyboard_single,
                &key_named(winit::keyboard::NamedKey::ArrowDown),
            );
            Dispatch::new().modifiers(shift()).send(
                &mut keyboard_single,
                &key_named(winit::keyboard::NamedKey::ArrowDown),
            );
            assert_eq!(keyboard_single.selected_indices(), vec![1]);
        }

        fn directory_mode_uses_custom_and_default_labels_with_folder_semantics() {
            use crate::a11y::Role;

            let (_guard, root) = temp_tree();
            let mut picker = FilePicker::new("Choose", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Directory)
                .with_filters(vec![FileFilter::new("Images", &["png"]), FileFilter::all()])
                .with_accept_label(Some("Attach folder".into()));
            picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            let node = Widget::accessibility(&picker).expect("dialog semantics");
            assert!(node.children.iter().any(|child| {
                child.role == Role::List && child.name.as_deref() == Some("Folders")
            }));
            assert!(node.children.iter().any(|child| {
                child.role == Role::Button && child.name.as_deref() == Some("Attach folder")
            }));
            assert!(
                !node
                    .children
                    .iter()
                    .any(|child| { matches!(child.role, Role::TextInput | Role::ComboBox) })
            );

            let default = FilePicker::new("Choose", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Directory)
                .with_accept_label(Some("   ".into()));
            let node = Widget::accessibility(&default).expect("dialog semantics");
            assert!(node.children.iter().any(|child| {
                child.role == Role::Button && child.name.as_deref() == Some("Choose folder")
            }));
        }

        fn single_open_mode_constrains_selection_without_changing_the_default() {
            let (_guard, root) = temp_tree();
            let mut single =
                FilePicker::new("Open", &root, vec![root.clone()]).with_multiple(false);
            single.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            for index in [1, 2] {
                let center = single.entry_rect(index).unwrap().center();
                dispatch(&mut single, &cursor_at(center));
                Dispatch::new()
                    .modifiers(ctrl())
                    .send(&mut single, &mouse_press());
            }
            assert_eq!(single.selected_indices(), vec![2]);

            let mut default_multiple = FilePicker::new("Open", &root, vec![root.clone()]);
            default_multiple.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            for index in [1, 2] {
                let center = default_multiple.entry_rect(index).unwrap().center();
                dispatch(&mut default_multiple, &cursor_at(center));
                Dispatch::new()
                    .modifiers(ctrl())
                    .send(&mut default_multiple, &mouse_press());
            }
            assert_eq!(default_multiple.selected_indices(), vec![1, 2]);
        }

        directory_mode_can_choose_the_current_empty_folder();
        directory_mode_lists_only_folders_and_chooses_a_selected_child();
        directory_mode_single_and_multiple_selection_are_enforced_in_the_widget();
        directory_mode_uses_custom_and_default_labels_with_folder_semantics();
        single_open_mode_constrains_selection_without_changing_the_default();
    }

    #[test]
    fn directory_mode_pointer_and_keyboard_confirmation() {
        fn directory_mode_click_selects_and_double_click_navigates() {
            let (_guard, root) = temp_tree();
            let child = root.join("nested");
            let mut picker = FilePicker::new("Choose", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Directory);
            picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            let center = picker.entry_rect(0).unwrap().center();
            dispatch(&mut picker, &cursor_at(center));

            dispatch(&mut picker, &mouse_press());
            assert_eq!(picker.current_dir(), root.as_path());
            assert_eq!(picker.selection(), Some(0));

            dispatch(&mut picker, &mouse_press());
            assert_eq!(picker.current_dir(), child.as_path());
        }

        fn directory_mode_mouse_selects_and_footer_confirms_a_child() {
            let (_guard, root) = temp_tree();
            let child = root.join("nested");
            let mut picker = FilePicker::new("Choose", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Directory);
            picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));

            let row = picker.entry_rect(0).unwrap().center();
            dispatch(&mut picker, &cursor_at(row));
            dispatch(&mut picker, &mouse_press());
            let confirm = picker.open_rect().center();
            dispatch(&mut picker, &cursor_at(confirm));
            dispatch(&mut picker, &mouse_press());

            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![child]))
            );
        }

        fn directory_mode_keyboard_can_navigate_and_confirm() {
            let dir = tempfile::tempdir().expect("tempdir");
            let root = dir.path().to_path_buf();
            let apple = root.join("apple");
            let cherry = root.join("cherry");
            fs::create_dir(&apple).unwrap();
            fs::create_dir(&cherry).unwrap();
            let mut picker = FilePicker::new("Choose", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Directory);
            picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));

            dispatch(&mut picker, &key_char("c"));
            assert_eq!(picker.selection(), Some(1));
            dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
            dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
            assert_eq!(picker.focus, Some(PickerFocus::Confirm));
            dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Enter));

            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![cherry]))
            );

            let mut navigate = FilePicker::new("Choose", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Directory);
            navigate.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            dispatch(
                &mut navigate,
                &key_named(winit::keyboard::NamedKey::ArrowDown),
            );
            dispatch(&mut navigate, &key_named(winit::keyboard::NamedKey::Enter));
            assert_eq!(navigate.current_dir(), apple.as_path());
        }

        fn directory_mode_empty_folder_keyboard_confirmation_and_cancel_remain_usable() {
            let dir = tempfile::tempdir().expect("tempdir");
            let root = dir.path().to_path_buf();
            let mut picker = FilePicker::new("Choose", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Directory);

            dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
            assert_eq!(picker.focus, Some(PickerFocus::Confirm));
            dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Enter));
            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![root.clone()]))
            );

            let mut cancelled = FilePicker::new("Choose", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Directory);
            dispatch(
                &mut cancelled,
                &key_named(winit::keyboard::NamedKey::Escape),
            );
            assert_eq!(cancelled.take_outcome(), Some(FilePickerOutcome::Cancelled));
        }

        directory_mode_click_selects_and_double_click_navigates();
        directory_mode_mouse_selects_and_footer_confirms_a_child();
        directory_mode_keyboard_can_navigate_and_confirm();
        directory_mode_empty_folder_keyboard_confirmation_and_cancel_remain_usable();
    }

    #[test]
    fn directory_confirmation_revalidates_the_filesystem() {
        fn directory_confirmation_refuses_a_vanished_or_replaced_selection() {
            let dir = tempfile::tempdir().expect("tempdir");
            let root = dir.path().to_path_buf();
            let child = root.join("child");
            fs::create_dir(&child).unwrap();
            let mut picker = FilePicker::new("Choose", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Directory);
            assert!(picker.select_by_path(&child));

            fs::remove_dir(&child).unwrap();
            fs::write(&child, b"now a file").unwrap();
            picker.click_open();

            assert!(picker.take_outcome().is_none());
            picker.click_cancel();
            assert_eq!(picker.take_outcome(), Some(FilePickerOutcome::Cancelled));
        }

        fn directory_confirmation_rechecks_the_current_directory() {
            let dir = tempfile::tempdir().expect("tempdir");
            let root = dir.path().to_path_buf();
            let current = root.join("current");
            fs::create_dir(&current).unwrap();
            let mut picker = FilePicker::new("Choose", &current, vec![root.clone()])
                .with_mode(FilePickerMode::Directory);

            fs::remove_dir(&current).unwrap();
            fs::write(&current, b"now a file").unwrap();
            picker.click_open();

            assert!(picker.take_outcome().is_none());
        }

        #[cfg(unix)]
        fn directory_confirmation_refuses_a_selection_changed_to_an_escape_symlink() {
            use std::os::unix::fs::symlink;

            let dir = tempfile::tempdir().expect("tempdir");
            let outside = tempfile::tempdir().expect("outside");
            let root = dir.path().to_path_buf();
            let child = root.join("child");
            fs::create_dir(&child).unwrap();
            let mut picker = FilePicker::new("Choose", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Directory);
            assert!(picker.select_by_path(&child));

            fs::remove_dir(&child).unwrap();
            symlink(outside.path(), &child).unwrap();
            picker.click_open();

            assert!(picker.take_outcome().is_none());
        }

        #[cfg(target_os = "linux")]
        fn directory_selection_preserves_non_utf8_native_paths() {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt as _;

            let dir = tempfile::tempdir().expect("tempdir");
            let root = dir.path().to_path_buf();
            let child = root.join(OsStr::from_bytes(b"folder-\xff"));
            fs::create_dir(&child).unwrap();
            let mut picker = FilePicker::new("Choose", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Directory);
            assert!(picker.select_by_path(&child));

            picker.click_open();

            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![child]))
            );
        }

        directory_confirmation_refuses_a_vanished_or_replaced_selection();
        directory_confirmation_rechecks_the_current_directory();
        #[cfg(unix)]
        directory_confirmation_refuses_a_selection_changed_to_an_escape_symlink();
        #[cfg(target_os = "linux")]
        directory_selection_preserves_non_utf8_native_paths();
    }

    #[test]
    fn navigation_stays_under_roots() {
        fn navigate_into_subdir_updates_current_dir_and_entries() {
            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
            let nested = root.join("nested");
            picker.navigate_to(&nested);
            assert_eq!(picker.current_dir(), nested.as_path());
            assert_eq!(picker.entries().len(), 1);
            assert_eq!(picker.entries()[0].name, "gamma.txt");
        }

        fn navigate_outside_roots_is_silently_dropped() {
            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
            let cwd_before = picker.current_dir().to_path_buf();
            picker.navigate_to(Path::new("/etc"));
            assert_eq!(picker.current_dir(), cwd_before.as_path());
        }

        fn can_navigate_to_requires_under_a_root() {
            let (_g, root) = temp_tree();
            let picker = FilePicker::new("Open", &root, vec![root.clone()]);
            assert!(picker.can_navigate_to(&root));
            assert!(picker.can_navigate_to(&root.join("nested")));
            assert!(!picker.can_navigate_to(Path::new("/etc")));
        }

        fn up_button_navigates_to_parent() {
            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
            let nested = root.join("nested");
            picker.navigate_to(&nested);
            assert_eq!(picker.current_dir(), nested.as_path());
            assert!(picker.can_go_up());
            picker.go_up();
            assert_eq!(picker.current_dir(), root.as_path());
        }

        navigate_into_subdir_updates_current_dir_and_entries();
        navigate_outside_roots_is_silently_dropped();
        can_navigate_to_requires_under_a_root();
        up_button_navigates_to_parent();
    }

    #[test]
    fn pointer_rows_and_footer_buttons_navigate_select_and_resolve() {
        fn click_on_directory_row_navigates_into_it() {
            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
            picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            // Row 0 is the `nested` directory (dirs sort first).
            let center = picker.entry_rect(0).expect("row 0").center();
            dispatch(&mut picker, &cursor_at(center));
            dispatch(&mut picker, &mouse_press());
            assert_eq!(picker.current_dir(), root.join("nested").as_path());
        }

        fn click_on_file_row_sets_selection() {
            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
            picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            let center = picker.entry_rect(1).expect("row 1").center();
            dispatch(&mut picker, &cursor_at(center));
            dispatch(&mut picker, &mouse_press());
            assert_eq!(picker.selection(), Some(1));
        }

        fn click_cancel_button_resolves_cancelled() {
            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
            picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            let center = picker.cancel_rect().center();
            dispatch(&mut picker, &cursor_at(center));
            dispatch(&mut picker, &mouse_press());
            assert_eq!(picker.take_outcome(), Some(FilePickerOutcome::Cancelled));
        }

        fn click_open_button_with_selection_resolves_selected() {
            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
            picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            let target = root.join("alpha.txt");
            picker.select_by_path(&target);
            let center = picker.open_rect().center();
            dispatch(&mut picker, &cursor_at(center));
            dispatch(&mut picker, &mouse_press());
            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![target]))
            );
        }

        fn ctrl_click_multi_select_opens_all() {
            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
            picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            // entries: [nested(dir), alpha.txt(1), beta.txt(2)]. Select both files.
            let r1 = picker.entry_rect(1).unwrap().center();
            dispatch(&mut picker, &cursor_at(r1));
            Dispatch::new()
                .modifiers(ctrl())
                .send(&mut picker, &mouse_press());
            let r2 = picker.entry_rect(2).unwrap().center();
            dispatch(&mut picker, &cursor_at(r2));
            Dispatch::new()
                .modifiers(ctrl())
                .send(&mut picker, &mouse_press());
            assert_eq!(picker.selected_indices(), vec![1, 2]);
            picker.click_open();
            match picker.take_outcome() {
                Some(FilePickerOutcome::Selected(paths)) => {
                    assert_eq!(paths.len(), 2);
                    assert!(paths.contains(&root.join("alpha.txt")));
                    assert!(paths.contains(&root.join("beta.txt")));
                }
                other => panic!("expected two selected paths, got {other:?}"),
            }
        }

        fn double_click_file_opens_immediately() {
            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
            picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            let center = picker.entry_rect(1).unwrap().center();
            dispatch(&mut picker, &cursor_at(center));
            dispatch(&mut picker, &mouse_press()); // first click selects
            dispatch(&mut picker, &mouse_press()); // second click opens
            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![root.join("alpha.txt")]))
            );
        }

        click_on_directory_row_navigates_into_it();
        click_on_file_row_sets_selection();
        click_cancel_button_resolves_cancelled();
        click_open_button_with_selection_resolves_selected();
        ctrl_click_multi_select_opens_all();
        double_click_file_opens_immediately();
    }

    #[test]
    fn save_mode_filename_resolves_and_rejects_escapes() {
        fn save_mode_uses_save_label() {
            let (_g, root) = temp_tree();
            let picker =
                FilePicker::new("Save", &root, vec![root.clone()]).with_mode(FilePickerMode::Save);
            // The label itself is only exercised at draw-time; the mode
            // getter isn't public, so this pins the construction path.
            assert!(matches!(picker.mode, FilePickerMode::Save));
        }

        fn save_mode_typed_filename_resolves_target() {
            let (_g, root) = temp_tree();
            let mut picker =
                FilePicker::new("Save", &root, vec![root.clone()]).with_mode(FilePickerMode::Save);
            for ch in ["n", "e", "w"] {
                dispatch(&mut picker, &key_char(ch));
            }
            dispatch(&mut picker, &key_char("."));
            for ch in ["t", "x", "t"] {
                dispatch(&mut picker, &key_char(ch));
            }
            assert_eq!(picker.filename(), "new.txt");
            // Backspace takes back one character as a person sees it, and an
            // input method's commit lands at the caret.
            dispatch(
                &mut picker,
                &key_named(winit::keyboard::NamedKey::Backspace),
            );
            dispatch(
                &mut picker,
                &key_named(winit::keyboard::NamedKey::Backspace),
            );
            dispatch(
                &mut picker,
                &key_named(winit::keyboard::NamedKey::Backspace),
            );
            dispatch(
                &mut picker,
                &key_named(winit::keyboard::NamedKey::Backspace),
            );
            dispatch(&mut picker, &key_char("👍🏽"));
            dispatch(&mut picker, &key_char("e\u{301}"));
            dispatch(
                &mut picker,
                &key_named(winit::keyboard::NamedKey::Backspace),
            );
            assert_eq!(picker.filename(), "new👍🏽");
            dispatch(
                &mut picker,
                &key_named(winit::keyboard::NamedKey::Backspace),
            );
            assert_eq!(picker.filename(), "new");
            dispatch(
                &mut picker,
                &WindowEvent::Ime(winit::event::Ime::Preedit("tx".into(), Some((2, 2)))),
            );
            assert_eq!(picker.filename(), "new", "a composition is not the value");
            dispatch(
                &mut picker,
                &WindowEvent::Ime(winit::event::Ime::Commit(".txt".into())),
            );
            assert_eq!(picker.filename(), "new.txt");
            picker.click_open();
            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![root.join("new.txt")]))
            );
        }

        fn save_filename_uses_the_same_focus_ring_and_accessibility_target() {
            use crate::a11y::Role;
            let (_g, root) = temp_tree();
            let mut picker =
                FilePicker::new("Save", &root, vec![root.clone()]).with_mode(FilePickerMode::Save);
            picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            Widget::set_focus(&mut picker, true);
            assert!(crate::ime::ImeWindowState::from_widget(&picker).is_some());
            let original = picker.ime_edit_fingerprint(&[7; 32]).unwrap();
            // Save starts on Filename. Tab leaves it for the next modal control;
            // Shift+Tab returns to the exact editable target.
            dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
            assert_eq!(picker.focus, Some(PickerFocus::Cancel));
            assert!(crate::ime::ImeWindowState::from_widget(&picker).is_none());
            Dispatch::new()
                .modifiers(shift())
                .send(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
            assert_eq!(picker.focus, Some(PickerFocus::Filename));
            dispatch(&mut picker, &key_char("x"));
            assert_eq!(picker.filename(), "x");
            assert_ne!(picker.ime_edit_fingerprint(&[7; 32]).unwrap(), original);
            assert!(crate::ime::ImeWindowState::from_widget(&picker).is_some());
            let node = Widget::accessibility(&picker).expect("dialog semantics");
            let filename = node
                .children
                .iter()
                .find(|child| child.role == Role::TextInput)
                .expect("Filename field");
            assert!(filename.state.focused);
            assert_eq!(filename.value.as_deref(), Some("x"));
        }

        fn save_mode_rejects_absolute_and_traversal_filenames() {
            let (_guard, root) = temp_tree();
            for unsafe_name in ["/tmp/stolen", "../outside", "sub/file", ".", ".."] {
                let mut picker = FilePicker::new("Save", &root, vec![root.clone()])
                    .with_mode(FilePickerMode::Save)
                    .with_filename(unsafe_name);
                // The disabled UI action cannot be activated; exercise the
                // resolver directly so this remains a security regression test.
                picker.commit_open();
                assert_eq!(
                    picker.take_outcome(),
                    Some(FilePickerOutcome::Cancelled),
                    "save name {unsafe_name:?} escaped the picker scope"
                );
            }
        }

        #[cfg(unix)]
        fn save_mode_revalidates_a_symlinked_current_directory() {
            use std::os::unix::fs::symlink;

            let (_guard, root) = temp_tree();
            let outside = tempfile::tempdir().unwrap();
            let escape = root.join("escape");
            symlink(outside.path(), &escape).unwrap();
            let mut picker = FilePicker::new("Save", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Save)
                .with_filename("identity.tilo");
            // Navigation is lexically within `root`; commit must catch where it
            // actually resolves before returning a privileged save target.
            picker.navigate_to(&escape);
            picker.commit_open();
            assert_eq!(picker.take_outcome(), Some(FilePickerOutcome::Cancelled));
        }

        save_mode_uses_save_label();
        save_mode_typed_filename_resolves_target();
        save_filename_uses_the_same_focus_ring_and_accessibility_target();
        save_mode_rejects_absolute_and_traversal_filenames();
        #[cfg(unix)]
        save_mode_revalidates_a_symlinked_current_directory();
    }

    #[test]
    fn saving_over_an_existing_file_asks_first() {
        use crate::a11y::{Action, Role};
        use winit::keyboard::NamedKey;

        fn save_picker(root: &Path, name: &str) -> FilePicker {
            let mut picker = FilePicker::new("Export", root, vec![root.to_path_buf()])
                .with_mode(FilePickerMode::Save)
                .with_filename(name);
            picker.set_rect(Rect::new(0.0, 0.0, 600.0, 420.0));
            Widget::set_focus(&mut picker, true);
            picker
        }

        fn prompt_node(picker: &FilePicker) -> crate::a11y::AccessNode {
            let dialog = Widget::accessibility(picker).expect("picker semantics");
            assert_eq!(dialog.children.len(), 1, "only the prompt is reachable");
            let prompt = dialog.children[0].clone();
            assert_eq!(prompt.role, Role::Dialog);
            assert!(prompt.state.modal);
            prompt
        }

        fn enter_asks_and_escape_keeps_the_file() {
            let (_g, root) = temp_tree();
            let mut picker = save_picker(&root, "alpha.txt");
            dispatch(&mut picker, &key_named(NamedKey::Enter));
            assert_eq!(picker.take_outcome(), None, "nothing is chosen yet");
            assert_eq!(
                picker.replace_prompt(),
                Some(root.join("alpha.txt").as_path())
            );
            let prompt = prompt_node(&picker);
            assert_eq!(
                prompt.name.as_deref(),
                Some("A file named \u{201c}alpha.txt\u{201d} already exists here. Replace it?")
            );
            let names: Vec<_> = prompt
                .children
                .iter()
                .map(|button| {
                    (
                        button.name.clone().unwrap_or_default(),
                        button.state.focused,
                    )
                })
                .collect();
            assert_eq!(
                names,
                vec![("Cancel".to_owned(), true), ("Replace".to_owned(), false)],
                "Cancel holds focus, so Enter alone never replaces"
            );
            // Typing does not reach the name behind the prompt.
            dispatch(&mut picker, &key_char("x"));
            assert_eq!(picker.filename(), "alpha.txt");

            dispatch(&mut picker, &key_named(NamedKey::Escape));
            assert_eq!(picker.replace_prompt(), None);
            assert_eq!(picker.take_outcome(), None, "Escape leaves the picker open");
            assert_eq!(picker.focus, Some(PickerFocus::Filename));
            assert_eq!(fs::read(root.join("alpha.txt")).unwrap(), b"a");

            dispatch(&mut picker, &key_named(NamedKey::Enter));
            assert!(picker.replace_prompt().is_some());
            dispatch(&mut picker, &key_named(NamedKey::Enter));
            assert_eq!(
                picker.replace_prompt(),
                None,
                "Enter on Cancel keeps the file"
            );
            assert_eq!(picker.take_outcome(), None);
        }

        fn keyboard_reaches_replace() {
            let (_g, root) = temp_tree();
            let mut picker = save_picker(&root, "alpha.txt");
            dispatch(&mut picker, &key_named(NamedKey::Enter));
            dispatch(&mut picker, &key_named(NamedKey::Tab));
            assert_eq!(picker.focus, Some(PickerFocus::Replace));
            Dispatch::new()
                .modifiers(shift())
                .send(&mut picker, &key_named(NamedKey::Tab));
            assert_eq!(picker.focus, Some(PickerFocus::KeepExisting));
            dispatch(&mut picker, &key_named(NamedKey::ArrowRight));
            assert_eq!(picker.focus, Some(PickerFocus::Replace));
            dispatch(&mut picker, &key_named(NamedKey::Enter));
            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![root.join("alpha.txt")]))
            );
        }

        fn pointer_answers_the_prompt() {
            let (_g, root) = temp_tree();
            let mut picker = save_picker(&root, "alpha.txt");
            let at = picker.open_rect().center();
            dispatch(&mut picker, &cursor_at(at));
            dispatch(&mut picker, &mouse_press());
            assert!(picker.replace_prompt().is_some(), "Save asks first");
            // The picker's own Save button is behind the prompt now.
            dispatch(&mut picker, &mouse_press());
            assert_eq!(picker.take_outcome(), None);
            let at = picker.keep_existing_rect.center();
            dispatch(&mut picker, &cursor_at(at));
            dispatch(&mut picker, &mouse_press());
            assert_eq!(picker.replace_prompt(), None);
            assert_eq!(picker.take_outcome(), None);

            let at = picker.open_rect().center();
            dispatch(&mut picker, &cursor_at(at));
            dispatch(&mut picker, &mouse_press());
            let at = picker.replace_rect.center();
            dispatch(&mut picker, &cursor_at(at));
            dispatch(&mut picker, &mouse_press());
            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![root.join("alpha.txt")]))
            );
        }

        fn semantic_actions_answer_the_prompt() {
            let (_g, root) = temp_tree();
            let mut picker = save_picker(&root, "alpha.txt");
            let mut doc = AutoCommit::new();
            let path = automerge::ROOT;
            assert!(Widget::perform_action_at(
                &mut picker,
                &Action::Activate,
                &[PickerFocus::Confirm.id()],
                &mut crate::widget::Edits::lend(&mut doc),
                &path,
            ));
            assert!(picker.replace_prompt().is_some());
            assert!(
                !Widget::perform_action_at(
                    &mut picker,
                    &Action::Activate,
                    &[PickerFocus::Confirm.id()],
                    &mut crate::widget::Edits::lend(&mut doc),
                    &path,
                ),
                "the picker behind the prompt cannot be operated"
            );
            let prompt = prompt_node(&picker);
            let replace = prompt.children[1].key.expect("Replace key");
            assert!(Widget::perform_action_at(
                &mut picker,
                &Action::Activate,
                &[prompt.key.expect("prompt key"), replace],
                &mut crate::widget::Edits::lend(&mut doc),
                &path,
            ));
            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![root.join("alpha.txt")]))
            );
        }

        fn new_names_save_without_asking_and_folders_open() {
            let (_g, root) = temp_tree();
            let mut picker = save_picker(&root, "fresh.txt");
            dispatch(&mut picker, &key_named(NamedKey::Enter));
            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![root.join("fresh.txt")]))
            );

            let mut picker = save_picker(&root, "nested");
            dispatch(&mut picker, &key_named(NamedKey::Enter));
            assert_eq!(picker.take_outcome(), None);
            assert_eq!(picker.current_dir(), root.join("nested").as_path());
            assert_eq!(picker.filename(), "");
        }

        fn double_clicking_an_existing_file_asks_too() {
            let (_g, root) = temp_tree();
            let mut picker = save_picker(&root, "");
            let row = picker.entry_rect(1).expect("alpha.txt row").center();
            dispatch(&mut picker, &cursor_at(row));
            dispatch(&mut picker, &mouse_press());
            dispatch(&mut picker, &mouse_press());
            assert_eq!(picker.take_outcome(), None);
            assert_eq!(
                picker.replace_prompt(),
                Some(root.join("alpha.txt").as_path())
            );
        }

        enter_asks_and_escape_keeps_the_file();
        keyboard_reaches_replace();
        pointer_answers_the_prompt();
        semantic_actions_answer_the_prompt();
        new_names_save_without_asking_and_folders_open();
        double_clicking_an_existing_file_asks_too();
    }

    #[test]
    fn the_name_field_selects_all_like_every_text_field() {
        use winit::keyboard::NamedKey;
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Export", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Save)
            .with_filename("gradient.png");
        picker.set_rect(Rect::new(0.0, 0.0, 600.0, 420.0));
        Widget::set_focus(&mut picker, true);

        Dispatch::new()
            .modifiers(ctrl())
            .send(&mut picker, &key_char("a"));
        dispatch(&mut picker, &key_char("x"));
        assert_eq!(
            picker.filename(),
            "x",
            "Ctrl+A selects the name, typing replaces it"
        );

        // Away from the field, then three clicks on it select its line.
        dispatch(&mut picker, &key_named(NamedKey::Tab));
        let field = picker.filename_rect().center();
        dispatch(&mut picker, &cursor_at(field));
        for _ in 0..3 {
            dispatch(&mut picker, &mouse_press());
            dispatch(&mut picker, &mouse_release());
        }
        dispatch(&mut picker, &key_char("y"));
        assert_eq!(
            picker.filename(),
            "y",
            "triple-click selects the whole name"
        );

        assert!(!picker.filename.spellcheck_enabled());
        assert!(!picker.filename.autocorrect_enabled());
    }

    use crate::synthetic_input::mouse_release;

    fn ctrl() -> winit::event::Modifiers {
        let mut s = winit::keyboard::ModifiersState::empty();
        s.set(winit::keyboard::ModifiersState::CONTROL, true);
        winit::event::Modifiers::from(s)
    }

    fn key_named(named: winit::keyboard::NamedKey) -> WindowEvent {
        crate::synthetic_input::key_input(
            &winit::keyboard::Key::Named(named),
            ElementState::Pressed,
        )
    }

    fn key_char(s: &str) -> WindowEvent {
        crate::synthetic_input::key_input(
            &winit::keyboard::Key::Character(s.into()),
            ElementState::Pressed,
        )
    }

    fn shift() -> winit::event::Modifiers {
        let mut state = winit::keyboard::ModifiersState::empty();
        state.set(winit::keyboard::ModifiersState::SHIFT, true);
        winit::event::Modifiers::from(state)
    }

    /// `n` numbered files so the list overflows a short dialog and
    /// exercises scrolling.
    fn temp_many(n: usize) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        for i in 0..n {
            fs::write(root.join(format!("file{i:03}.txt")), b"x").unwrap();
        }
        (dir, root)
    }

    #[test]
    fn listing_filters_and_scroll_window_rows() {
        fn scrolling_windows_the_visible_rows() {
            let (_g, root) = temp_many(40);
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
            // A modest dialog: only a handful of rows fit.
            picker.set_rect(Rect::new(0.0, 0.0, 560.0, 420.0));
            assert!(picker.entry_rect(0).is_some());
            dispatch(
                &mut picker,
                &WindowEvent::MouseWheel {
                    device_id: winit::event::DeviceId::dummy(),
                    delta: winit::event::MouseScrollDelta::LineDelta(0.0, -5.0),
                    phase: winit::event::TouchPhase::Moved,
                },
            );
            assert!(picker.entry_rect(0).is_none(), "row 0 scrolled out of view");
            assert!(picker.entry_rect(6).is_some(), "a later row is now visible");
        }

        fn filters_hide_nonmatching_files() {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().to_path_buf();
            fs::write(root.join("a.png"), b"x").unwrap();
            fs::write(root.join("b.txt"), b"x").unwrap();
            fs::create_dir(root.join("sub")).unwrap();
            let picker = FilePicker::new("Open", &root, vec![root.clone()]).with_filters(vec![
                FileFilter::new("Images", &["png", "jpg"]),
                FileFilter::all(),
            ]);
            // Directory + the png survive; the txt is filtered out.
            let names: Vec<_> = picker.entries().iter().map(|e| e.name.clone()).collect();
            assert!(names.contains(&"sub".to_string()));
            assert!(names.contains(&"a.png".to_string()));
            assert!(!names.contains(&"b.txt".to_string()));
            assert_eq!(picker.active_filter_label(), "Images");
        }

        fn cycle_filter_switches_listing() {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().to_path_buf();
            fs::write(root.join("a.png"), b"x").unwrap();
            fs::write(root.join("b.txt"), b"x").unwrap();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()])
                .with_filters(vec![FileFilter::new("Images", &["png"]), FileFilter::all()]);
            assert_eq!(picker.entries().len(), 1); // just the png
            picker.cycle_filter();
            assert_eq!(picker.active_filter_label(), "All files");
            assert_eq!(picker.entries().len(), 2); // png + txt
        }

        /// Writes native-size and accessibility-state GPU renders for the manual visual audit. Kept
        /// ignored so ordinary unit runs remain hermetic; run with
        /// `cargo test -p ui file_picker_visual_audit -- --ignored --nocapture`.
        #[ignore = "developer visual audit writes /tmp/tilo-file-picker-focus"]
        fn file_picker_visual_audit() {
            use vello::peniko::Color;

            let (_guard, root) = temp_tree();
            let output = Path::new("/tmp/tilo-file-picker-focus");
            fs::create_dir_all(output).expect("visual-audit output directory");
            for (name, width, height, mode, theme) in [
                (
                    "open-800x600",
                    800,
                    600,
                    FilePickerMode::Open,
                    Theme::dark(),
                ),
                (
                    "save-800x600",
                    800,
                    600,
                    FilePickerMode::Save,
                    Theme::dark(),
                ),
                (
                    "open-1280x800",
                    1280,
                    800,
                    FilePickerMode::Open,
                    Theme::dark(),
                ),
                (
                    "save-1280x800",
                    1280,
                    800,
                    FilePickerMode::Save,
                    Theme::dark(),
                ),
                (
                    "open-800x600-large-text",
                    800,
                    600,
                    FilePickerMode::Open,
                    Theme::dark().with_text_scale(1.5),
                ),
                (
                    "save-800x600-large-text",
                    800,
                    600,
                    FilePickerMode::Save,
                    Theme::dark().with_text_scale(1.5),
                ),
                (
                    "open-800x600-high-contrast",
                    800,
                    600,
                    FilePickerMode::Open,
                    Theme::high_contrast(),
                ),
                (
                    "save-800x600-high-contrast",
                    800,
                    600,
                    FilePickerMode::Save,
                    Theme::high_contrast(),
                ),
            ] {
                let mut picker =
                    FilePicker::new("Choose a file", &root, vec![root.clone()]).with_mode(mode);
                let pixel_width = width;
                let pixel_height = height;
                let width = f64::from(pixel_width);
                let height = f64::from(pixel_height);
                picker.set_rect(Rect::new(
                    width * 0.19,
                    height * 0.14,
                    width * 0.81,
                    height * 0.86,
                ));
                Widget::set_focus(&mut picker, true);
                let mut scene = Scene::new();
                let mut text = TextRenderer::new();
                picker.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                    &PresenceCtx::default(),
                );
                let mut renderer =
                    crate::offscreen::OffscreenRenderer::new(pixel_width, pixel_height);
                renderer.render_png(
                    &scene,
                    Color::from_rgba8(20, 24, 32, 255),
                    output.join(format!("{name}.png")),
                );
            }
        }

        scrolling_windows_the_visible_rows();
        filters_hide_nonmatching_files();
        cycle_filter_switches_listing();
        file_picker_visual_audit();
    }

    #[test]
    fn keyboard_focus_cycles_selects_and_types_ahead() {
        fn tab_and_shift_tab_cycle_only_enabled_modal_controls() {
            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
            picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));

            // The Open action is disabled with no selected file, so focus wraps
            // List → Cancel → List instead of landing on a destructive-looking
            // button that cannot act.
            dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
            assert_eq!(picker.focus, Some(PickerFocus::Cancel));
            Dispatch::new()
                .modifiers(shift())
                .send(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
            assert_eq!(picker.focus, Some(PickerFocus::List));

            picker.set_selection(1);
            dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
            assert_eq!(picker.focus, Some(PickerFocus::Cancel));
            dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
            assert_eq!(picker.focus, Some(PickerFocus::Confirm));
        }

        fn list_arrows_select_rows_and_enter_accepts_the_selected_file() {
            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
            picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            dispatch(
                &mut picker,
                &key_named(winit::keyboard::NamedKey::ArrowDown),
            );
            dispatch(
                &mut picker,
                &key_named(winit::keyboard::NamedKey::ArrowDown),
            );
            assert_eq!(picker.selection(), Some(1), "second row is alpha.txt");
            dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Enter));
            assert_eq!(
                picker.take_outcome(),
                Some(FilePickerOutcome::Selected(vec![root.join("alpha.txt")]))
            );
        }

        fn keyboard_down_then_enter_enters_directory() {
            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
            picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            // ArrowDown selects entry 0 (the `nested` directory); Enter navigates.
            dispatch(
                &mut picker,
                &key_named(winit::keyboard::NamedKey::ArrowDown),
            );
            assert_eq!(picker.selection(), Some(0));
            dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Enter));
            assert_eq!(picker.current_dir(), root.join("nested").as_path());
        }

        fn type_ahead_jumps_to_matching_entry() {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().to_path_buf();
            fs::write(root.join("apple.txt"), b"x").unwrap();
            fs::write(root.join("banana.txt"), b"x").unwrap();
            fs::write(root.join("cherry.txt"), b"x").unwrap();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
            picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            dispatch(&mut picker, &key_char("c")); // jump to cherry.txt
            let sel = picker.selection().unwrap();
            assert_eq!(picker.entries()[sel].name, "cherry.txt");
        }

        tab_and_shift_tab_cycle_only_enabled_modal_controls();
        list_arrows_select_rows_and_enter_accepts_the_selected_file();
        keyboard_down_then_enter_enters_directory();
        type_ahead_jumps_to_matching_entry();
    }

    #[test]
    fn the_redesigned_dialog_names_selects_and_navigates_by_token_sized_parts() {
        fn save_preselects_the_name_without_its_extension() {
            let (_g, root) = temp_tree();
            let picker = FilePicker::new("Save", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Save)
                .with_filename("report.final.pdf");
            assert_eq!(
                picker.filename.selection_range_public(),
                Some((0, "report.final".len())),
                "typing replaces the name and keeps the type"
            );
            let hidden = FilePicker::new("Save", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Save)
                .with_filename(".bashrc");
            assert_eq!(
                hidden.filename.selection_range_public(),
                Some((0, ".bashrc".len())),
                "a dotfile has no extension to keep"
            );
        }

        fn the_sidebar_lists_allowed_roots_and_a_click_enters_one() {
            let (_g, root) = temp_tree();
            let inner = root.join("docs");
            fs::create_dir(&inner).unwrap();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone(), inner.clone()]);
            picker.set_rect(Rect::new(0.0, 0.0, 900.0, 620.0));
            assert!(picker.has_sidebar());
            let at = picker
                .places
                .iter()
                .position(|p| p.path == inner)
                .expect("an accessible root is a place");
            let rect = picker.place_rects[at];
            dispatch(&mut picker, &cursor_at(rect.center()));
            dispatch(&mut picker, &mouse_press());
            assert_eq!(picker.current_dir(), inner);
            assert_eq!(picker.active_place(), Some(at));

            let mut narrow = FilePicker::new("Open", &root, vec![root.clone()]);
            narrow.set_rect(Rect::new(0.0, 0.0, 420.0, 420.0));
            assert!(!narrow.has_sidebar(), "a narrow dialog keeps the list");
        }

        fn the_type_menu_opens_picks_with_the_keyboard_and_closes() {
            use winit::keyboard::NamedKey;
            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()])
                .with_filters(vec![FileFilter::new("Images", &["png"]), FileFilter::all()]);
            picker.set_rect(Rect::new(0.0, 0.0, 900.0, 620.0));
            Widget::set_focus(&mut picker, true);
            picker.focus_control(PickerFocus::Filter);
            dispatch(&mut picker, &key_named(NamedKey::Enter));
            assert!(picker.filter_open);
            dispatch(&mut picker, &key_named(NamedKey::ArrowDown));
            dispatch(&mut picker, &key_named(NamedKey::Enter));
            assert!(!picker.filter_open);
            assert_eq!(picker.active_filter_label(), "All files");
            dispatch(&mut picker, &key_named(NamedKey::Enter));
            dispatch(&mut picker, &key_named(NamedKey::Escape));
            assert!(
                !picker.filter_open && picker.outcome().is_none(),
                "Escape closes the menu, not the dialog"
            );
        }

        fn a_deep_path_keeps_the_current_folder_inside_the_path_bar() {
            let (_g, root) = temp_tree();
            let mut deep = root.clone();
            for level in 0..9 {
                deep.push(format!("a-rather-long-folder-name-{level}"));
            }
            fs::create_dir_all(&deep).unwrap();
            let mut picker = FilePicker::new("Open", &deep, vec![root.clone()]);
            picker.set_rect(Rect::new(0.0, 0.0, 560.0, 420.0));
            let bar = picker.path_rect();
            let last = picker.crumb_rects.last().expect("crumbs");
            assert_eq!(last.0, deep, "the current folder always shows");
            assert!(picker.crumb_rects.len() < 10, "leading segments drop out");
            assert!(
                picker
                    .crumb_rects
                    .iter()
                    .all(|(_, r)| r.x1 <= bar.x1 + 1e-6)
            );
        }

        save_preselects_the_name_without_its_extension();
        the_sidebar_lists_allowed_roots_and_a_click_enters_one();
        the_type_menu_opens_picks_with_the_keyboard_and_closes();
        a_deep_path_keeps_the_current_folder_inside_the_path_bar();
    }
}
