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

use crate::widget::{AppModel, ModelPath};
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
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::tokens::{Metrics, Spacing};
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

pub const FOOTER_HEIGHT: f64 = 56.0;
pub const HEADER_HEIGHT: f64 = 64.0;
pub const ROW_HEIGHT: f64 = 28.0;
/// The breadcrumb row sits one title line (`scale.base` at 1×, ~20 px)
/// under the header inset.
const CRUMB_ROW_OFFSET: f64 = 20.0;
/// Height of a breadcrumb hit rect.
const CRUMB_ROW_H: f64 = 18.0;
/// Per-glyph width guess for a crumb that has not been measured yet (before
/// the first paint); `measure_crumbs` records the real widths.
const CRUMB_GLYPH_ESTIMATE: f64 = 7.5;

/// Token-derived insets the picker lays out with. Resolved from the theme in
/// `build_layout` and refreshed at paint, cached because
/// `apply_layout_helper` runs from event handlers and `set_rect` with no
/// theme in hand; the draw path reads the same values, so layout and paint
/// agree at every `ui_scale`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PickerMetrics {
    /// From the top edge to the title line and the up button (`s2`).
    header_inset_y: f64,
    /// Vertical inset of the Save-mode filename field inside its band (`s1`).
    field_inset_y: f64,
    /// Horizontal inset of text inside the filename field and filter chip,
    /// and from a row's edge to its leading icon and on to the name (`s2`).
    inset_x: f64,
    /// The up button: a bar control wide (`bar_control_height`) and a large
    /// glyph tall (`icon_lg`). Recorded (F4 r2): below the 32 px minimum
    /// pointer target.
    up_w: f64,
    up_h: f64,
    /// From a crumb's left edge to its first glyph (`s1`).
    crumb_label_inset: f64,
    /// Room a crumb reserves after its label for the chevron separator (`s3`).
    crumb_separator_w: f64,
}

impl PickerMetrics {
    fn from_theme(theme: &Theme) -> Self {
        let t = &theme.tokens;
        Self::from_parts(&t.spacing, &t.metrics)
    }

    fn from_parts(spacing: &Spacing, metrics: &Metrics) -> Self {
        Self {
            header_inset_y: f64::from(spacing.s2),
            field_inset_y: f64::from(spacing.s1),
            inset_x: f64::from(spacing.s2),
            up_w: f64::from(metrics.bar_control_height),
            up_h: f64::from(metrics.icon_lg),
            crumb_label_inset: f64::from(spacing.s1),
            crumb_separator_w: f64::from(spacing.s3),
        }
    }
}

impl Default for PickerMetrics {
    /// The unscaled reference values, for a picker laid out before it has
    /// seen a theme.
    fn default() -> Self {
        Self::from_parts(&Spacing::BASE, &Metrics::BASE)
    }
}
/// 0 in Open mode.
const FILENAME_HEIGHT: f64 = 36.0;
const PADDING: f64 = 12.0;
const BUTTON_WIDTH: f64 = 96.0;
const BUTTON_HEIGHT: f64 = 32.0;
const BUTTON_GAP: f64 = 8.0;
const HEADER_ICON_PX: f64 = 18.0;
const ROW_ICON_PX: f64 = 16.0;

/// The file chooser is an immediate-mode modal, so its controls do not each
/// have a retained Taffy node.  Keep their keyboard order in the toolkit's
/// standard [`crate::overlay::FocusTrap`] instead of reimplementing wrapping
/// Tab logic locally.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum PickerFocus {
    Up,
    List,
    Filter,
    Filename,
    Cancel,
    Confirm,
}

impl PickerFocus {
    const fn id(self) -> u64 {
        match self {
            Self::Up => 1,
            Self::List => 2,
            Self::Filter => 3,
            Self::Filename => 4,
            Self::Cancel => 5,
            Self::Confirm => 6,
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
            label: "All files".into(),
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

    filename: String,
    first_visible: usize,

    /// Set on Open / Cancel. `None` until resolved.
    pending_outcome: Option<FilePickerOutcome>,

    // Hit-test rects, recomputed on each `apply_layout` / `draw`.
    open_rect: Rect,
    cancel_rect: Rect,
    up_rect: Rect,
    filter_rect: Rect,
    filename_rect: Rect,
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
        let mut me = Self {
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
            filename: String::new(),
            first_visible: 0,
            pending_outcome: None,
            open_rect: Rect::ZERO,
            cancel_rect: Rect::ZERO,
            up_rect: Rect::ZERO,
            filter_rect: Rect::ZERO,
            filename_rect: Rect::ZERO,
            entry_rows: Vec::new(),
            crumb_rects: Vec::new(),
            crumb_widths: HashMap::new(),
            metrics: PickerMetrics::default(),
            last_cursor: Point::ZERO,
            last_click: None,
            hover_entry: None,
            hover_button: None,
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
        self.filename = name.into();
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
            .map_or("All files", |f| f.label.as_str())
    }

    pub fn filename(&self) -> &str {
        &self.filename
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
                self.entries.push(FilePickerEntry { name, path, is_dir });
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

    fn visible_rows(&self) -> usize {
        let list_top = self.rect.y0 + HEADER_HEIGHT;
        let list_bot = self.list_bottom();
        let rows = ((list_bot - list_top) / ROW_HEIGHT).floor();
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "rows is clamped into 0.0..=f64::from(u32::MAX) immediately above, so the \
                      conversion is provably exact"
        )]
        let rows = rows.clamp(0.0, f64::from(u32::MAX)) as usize;
        rows
    }

    /// Above the filename field in Save mode, above the buttons otherwise.
    fn list_bottom(&self) -> f64 {
        let extras = if self.mode == FilePickerMode::Save {
            FILENAME_HEIGHT
        } else {
            0.0
        };
        self.rect.y1 - FOOTER_HEIGHT - extras
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
        let footer_top = self.rect.y1 - FOOTER_HEIGHT;
        let by = footer_top + (FOOTER_HEIGHT - BUTTON_HEIGHT) / 2.0;
        let by1 = by + BUTTON_HEIGHT;
        let open_x1 = self.rect.x1 - PADDING;
        let open_x0 = open_x1 - BUTTON_WIDTH;
        let cancel_x1 = open_x0 - BUTTON_GAP;
        let cancel_x0 = cancel_x1 - BUTTON_WIDTH;
        self.open_rect = Rect::new(open_x0, by, open_x1, by1);
        self.cancel_rect = Rect::new(cancel_x0, by, cancel_x1, by1);
        self.filter_rect = Rect::new(
            self.rect.x0 + PADDING,
            by,
            self.rect.x0 + PADDING + 160.0,
            by1,
        );
        let m = self.metrics;
        let filename_top = self.rect.y1 - FOOTER_HEIGHT - FILENAME_HEIGHT;
        self.filename_rect = Rect::new(
            self.rect.x0 + PADDING,
            filename_top + m.field_inset_y,
            self.rect.x1 - PADDING,
            filename_top + FILENAME_HEIGHT - m.field_inset_y,
        );

        let up_x1 = self.rect.x1 - PADDING;
        let up_x0 = up_x1 - m.up_w;
        let up_top = self.rect.y0 + m.header_inset_y;
        self.up_rect = Rect::new(up_x0, up_top, up_x1, up_top + m.up_h);

        self.crumb_rects = self.compute_crumb_rects();

        self.clamp_scroll();
        let list_top = self.rect.y0 + HEADER_HEIGHT;
        let row_x0 = self.rect.x0 + PADDING;
        let row_x1 = self.rect.x1 - PADDING;
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
            let y0 = list_top + (vis as f64) * ROW_HEIGHT;
            self.entry_rows
                .push((i, Rect::new(row_x0, y0, row_x1, y0 + ROW_HEIGHT)));
        }
        self.ensure_valid_focus();
    }

    /// Native dialog focus order, kept in one place so keyboard, AT focus
    /// and paint cannot drift.  The list is one focus target: arrow keys move
    /// its selected row, exactly like a native listbox, rather than making a
    /// long directory consume every Tab press.
    fn focus_targets(&self) -> Vec<PickerFocus> {
        let mut targets = Vec::with_capacity(6);
        if self.can_go_up() {
            targets.push(PickerFocus::Up);
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
                targets.first().copied()
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
    }

    fn can_commit_open(&self) -> bool {
        match self.mode {
            FilePickerMode::Open => self.validated_selected_files().is_some(),
            FilePickerMode::Save => {
                (!self.filename.trim().is_empty()
                    && self.safe_save_target(self.filename.trim()).is_some())
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

    fn list_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + PADDING,
            self.rect.y0 + HEADER_HEIGHT,
            self.rect.x1 - PADDING,
            self.list_bottom(),
        )
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
            Some(PickerFocus::List) => {
                self.activate_selected_entry();
                true
            }
            _ => false,
        }
    }

    /// Each segment maps a display label to the ancestor directory it
    /// navigates to. Only accessible ancestors get a segment, so the user
    /// never sees one they can't navigate to.
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
        let y0 = self.rect.y0 + m.header_inset_y + CRUMB_ROW_OFFSET;
        let mut x = self.rect.x0 + PADDING;
        let mut out = Vec::new();
        for (label, target) in segments {
            // A breadcrumb label is one path component — far below f64's
            // exact-integer range even for pathological path names.
            #[allow(
                clippy::cast_precision_loss,
                reason = "label is a single path component's display text"
            )]
            let char_count = label.chars().count() as f64;
            let text_w = self
                .crumb_widths
                .get(&label)
                .copied()
                .unwrap_or(char_count * CRUMB_GLYPH_ESTIMATE);
            let w = m.crumb_label_inset + text_w + m.crumb_separator_w;
            out.push((target, Rect::new(x, y0, x + w, y0 + CRUMB_ROW_H)));
            x += w;
        }
        out
    }

    /// Measure every breadcrumb label with the renderer the frame draws with
    /// and rebuild the crumb rects from those widths, so the hit rects match
    /// the painted glyphs — and stay matched when an event handler rebuilds
    /// the layout without a renderer. Runs once per paint.
    fn measure_crumbs(&mut self, text_renderer: &mut TextRenderer, theme: &Theme) {
        let style = theme.text(TextRole::Caption);
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
        if changed {
            self.crumb_rects = self.compute_crumb_rects();
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
                let name = self.filename.trim();
                if !name.is_empty() {
                    self.pending_outcome = Some(match self.safe_save_target(name) {
                        Some(target) => FilePickerOutcome::Selected(vec![target]),
                        None => FilePickerOutcome::Cancelled,
                    });
                } else if let Some(p) = self.first_selected_file() {
                    self.pending_outcome = Some(FilePickerOutcome::Selected(vec![p]));
                } else {
                    self.pending_outcome = Some(FilePickerOutcome::Cancelled);
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
            entry.name.clone_into(&mut self.filename);
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
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            flex_grow: 1.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        self.apply_layout_helper();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.adopt_theme(theme);
        self.apply_layout_helper();
        self.measure_crumbs(text_renderer, theme);
        let tokens = &theme.tokens;
        let radius = f64::from(tokens.radii.control);
        let frame = RoundedRect::from_rect(self.rect, radius);
        // Panel role (HIG §9.1): panel ground + the derived panel edge as
        // a crisp 1 px inset stroke; the declared `border_standard` sat
        // within rounding distance of `raised` on the dark preset.
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
        self.draw_entry_rows(scene, transform, text_renderer, theme);
        self.draw_footer(scene, transform, text_renderer, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
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
        let controls = self
            .a11y_controls()
            .into_iter()
            .map(|control| match control {
                PickerFocus::Up => AccessNode::new(Role::Button)
                    .with_name("Up")
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
                        .with_name(if self.mode == FilePickerMode::Directory {
                            "Folders"
                        } else {
                            "Files"
                        })
                        .with_key(control.id())
                        .with_bounds(self.list_rect())
                        .with_state(AccessState {
                            focusable: true,
                            focused: self.has_keyboard_focus && self.focus == Some(control),
                            ..Default::default()
                        })
                        .with_children(rows)
                }
                PickerFocus::Filter => AccessNode::new(Role::ComboBox)
                    .with_name("File type")
                    .with_value(self.active_filter_label())
                    .with_key(control.id())
                    .with_bounds(self.filter_rect)
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.has_keyboard_focus && self.focus == Some(control),
                        expanded: Some(false),
                        ..Default::default()
                    }),
                PickerFocus::Filename => AccessNode::new(Role::TextInput)
                    .with_name("Filename")
                    .with_value(self.filename.clone())
                    .with_text_selection(self.filename.len(), self.filename.len())
                    .with_key(control.id())
                    .with_bounds(self.filename_rect)
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.has_keyboard_focus && self.focus == Some(control),
                        ..Default::default()
                    }),
                PickerFocus::Cancel | PickerFocus::Confirm => {
                    let (label, rect, enabled) = if control == PickerFocus::Cancel {
                        ("Cancel", self.cancel_rect, true)
                    } else {
                        (self.confirm_label(), self.open_rect, self.can_commit_open())
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
            })
            .collect();
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
    }

    fn is_focused(&self) -> bool {
        self.has_keyboard_focus
    }

    fn traps_focus(&self) -> bool {
        true
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let (&control_index, rest) = path.split_first()?;
        let control = *self.a11y_controls().get(control_index)?;
        match (control, rest) {
            (PickerFocus::Up, []) => Some(self.up_rect),
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
        let Some((&control_index, rest)) = path.split_first() else {
            return false;
        };
        let Some(control) = self.a11y_controls().get(control_index).copied() else {
            return false;
        };
        if control == PickerFocus::Confirm && !self.can_commit_open() {
            return false;
        }
        if control == PickerFocus::List {
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
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        matches!(action, crate::a11y::Action::Activate) && self.activate_focus()
    }

    fn perform_action_at(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        use crate::a11y::{Action, hash_key};
        let Some((&control_key, rest)) = key_path.split_first() else {
            return false;
        };
        let Some(control) = PickerFocus::from_id(control_key) else {
            return false;
        };
        if control == PickerFocus::List {
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
    /// Title bar, up button and breadcrumb trail. Split out of `draw` to
    /// keep that function readable.
    fn draw_header_and_breadcrumbs(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let title_style = tokens.typography.style(TextRole::Body);
        text_renderer.draw_style(
            scene,
            &self.title,
            tokens.colors.text_primary,
            transform.pre_translate(Vec2::new(
                self.rect.x0 + PADDING,
                self.rect.y0 + self.metrics.header_inset_y,
            )),
            None,
            &title_style,
        );
        if self.can_go_up() {
            let up_hovered = self.up_rect.contains(self.last_cursor);
            if up_hovered {
                let s = RoundedRect::from_rect(self.up_rect, f64::from(tokens.radii.detail));
                scene.fill(
                    Fill::NonZero,
                    transform,
                    tokens.colors.intent.selection_hover,
                    None,
                    &s,
                );
            }
            if self.has_keyboard_focus && self.focus == Some(PickerFocus::Up) {
                let s = RoundedRect::from_rect(self.up_rect, f64::from(tokens.radii.detail));
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    tokens.colors.border_focus,
                    None,
                    &s,
                );
            }
            let ix = self.up_rect.x0 + (self.up_rect.width() - HEADER_ICON_PX) / 2.0;
            let iy = self.up_rect.y0 + (self.up_rect.height() - HEADER_ICON_PX) / 2.0;
            let icon_rect = Rect::new(ix, iy, ix + HEADER_ICON_PX, iy + HEADER_ICON_PX);
            draw_icon(
                scene,
                transform,
                icon_rect,
                Icon::ArrowUp,
                tokens.colors.text_primary,
                LUCIDE_DEFAULT_STROKE_PX,
            );
        }
        let crumb_style = tokens.typography.style(TextRole::Caption);
        let crumb_count = self.crumb_rects.len();
        for (idx, (_target, r)) in self.crumb_rects.iter().enumerate() {
            let label = self.crumb_label_for(idx).unwrap_or_default();
            let hovered = r.contains(self.last_cursor);
            let color = if hovered {
                tokens.colors.text_primary
            } else {
                tokens.colors.text_dim
            };
            text_renderer.draw_style(
                scene,
                &label,
                color,
                transform
                    .pre_translate(Vec2::new(r.x0 + self.metrics.crumb_label_inset, r.y0 + 2.0)),
                None,
                &crumb_style,
            );
            if idx.saturating_add(1) < crumb_count {
                let sep_rect = Rect::new(
                    r.x1 - self.metrics.crumb_separator_w,
                    r.y0,
                    r.x1,
                    r.y0 + ROW_ICON_PX,
                );
                draw_icon(
                    scene,
                    transform,
                    sep_rect,
                    Icon::ChevronRight,
                    tokens.colors.text_dim,
                    LUCIDE_DEFAULT_STROKE_PX,
                );
            }
        }
    }

    /// The scrollable list of visible entry rows. Split out of `draw` to
    /// keep that function readable.
    fn draw_entry_rows(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let row_style = tokens.typography.style(TextRole::Caption);
        for (i, rect) in &self.entry_rows {
            let i = *i;
            let is_selected = self.selection.is_selected(i);
            let is_hover = self.hover_entry == Some(i) && !is_selected;
            // HIG §9.3: an idle row is transparent and hover is a lift; only
            // the selected row carries a fill. The lift is a hairline wash
            // measured against the panel ground the row sits on (a
            // base-relative `Colors::lift` lands below `panel_ground` on the
            // dark preset and would darken the hovered row). A `raised` idle
            // fill was invisible on the old `raised` panel and read as a
            // stack of bands once the panel took its own ground.
            let bg = if is_selected {
                Some(tokens.colors.lift_on(tokens.colors.surfaces.overlay, 1.0))
            } else if is_hover {
                Some(crate::tokens::Colors::hairline_on(
                    crate::chrome::panel_ground(theme),
                    crate::chrome::HAIRLINE_ALPHA,
                ))
            } else {
                None
            };
            let row_shape = RoundedRect::from_rect(*rect, f64::from(tokens.radii.detail));
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

            if let Some(entry) = self.entries.get(i) {
                let fg = tokens.colors.text_primary;
                let inset_x = self.metrics.inset_x;
                let icon_y = rect.y0 + (ROW_HEIGHT - ROW_ICON_PX) / 2.0;
                let icon_rect = Rect::new(
                    rect.x0 + inset_x,
                    icon_y,
                    rect.x0 + inset_x + ROW_ICON_PX,
                    icon_y + ROW_ICON_PX,
                );
                draw_icon(
                    scene,
                    transform,
                    icon_rect,
                    icon_for_path(&entry.path, entry.is_dir),
                    fg,
                    LUCIDE_DEFAULT_STROKE_PX,
                );
                // Centre on the measured line, not the font size, so the
                // name shares the icon's vertical centre.
                let (_, name_h) = text_renderer.measure_style(&entry.name, None, &row_style);
                text_renderer.draw_style(
                    scene,
                    &entry.name,
                    fg,
                    transform.pre_translate(Vec2::new(
                        rect.x0 + inset_x + ROW_ICON_PX + inset_x,
                        rect.y0 + (ROW_HEIGHT - f64::from(name_h)) / 2.0,
                    )),
                    None,
                    &row_style,
                );
            }
        }
    }

    /// Save-mode filename field, filter chip and Cancel/Open(Save)
    /// buttons. Split out of `draw` to keep that function readable.
    fn draw_footer(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let row_style = tokens.typography.style(TextRole::Body);
        if self.mode == FilePickerMode::Save {
            self.draw_filename_field(scene, transform, text_renderer, theme, &row_style);
        }
        if self.mode != FilePickerMode::Directory && self.filters.len() > 1 {
            self.draw_filter_chip(scene, transform, text_renderer, theme, &row_style);
        }

        let cancel = footer_button(theme, self.cancel_rect, "Cancel", false)
            .hovered(self.hover_button == Some(0))
            .focused(self.has_keyboard_focus && self.focus == Some(PickerFocus::Cancel));
        crate::widget::Button::paint(scene, transform, text_renderer, theme, &cancel);
        let confirm = footer_button(theme, self.open_rect, self.confirm_label(), true)
            .hovered(self.hover_button == Some(1))
            .focused(self.has_keyboard_focus && self.focus == Some(PickerFocus::Confirm))
            .disabled(!self.can_commit_open());
        crate::widget::Button::paint(scene, transform, text_renderer, theme, &confirm);
    }

    /// Save-mode filename text field. Split out of `draw_footer` to keep
    /// that under the line cap.
    fn draw_filename_field(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        row_style: &TextStyle,
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
        let shown = if self.filename.is_empty() {
            "Filename…"
        } else {
            &self.filename
        };
        let color = if self.filename.is_empty() {
            tokens.colors.text_dim
        } else {
            tokens.colors.text_primary
        };
        // Centre on the measured line, not the font size.
        let (_, shown_h) = text_renderer.measure_style(shown, None, row_style);
        text_renderer.draw_style(
            scene,
            shown,
            color,
            transform.pre_translate(Vec2::new(
                field.x0 + self.metrics.inset_x,
                field.y0 + (field.height() - f64::from(shown_h)) / 2.0,
            )),
            None,
            row_style,
        );
    }

    /// Active-filter chip, shown only when there is more than one filter to
    /// cycle through. Split out of `draw_footer` to keep that under the
    /// line cap.
    fn draw_filter_chip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        row_style: &TextStyle,
    ) {
        let tokens = &theme.tokens;
        let hovered = self.filter_rect.contains(self.last_cursor);
        let s = RoundedRect::from_rect(self.filter_rect, f64::from(tokens.radii.detail));
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
        // Centre on the measured line, not the font size.
        let (_, filter_h) =
            text_renderer.measure_style(self.active_filter_label(), None, row_style);
        text_renderer.draw_style(
            scene,
            self.active_filter_label(),
            tokens.colors.text_primary,
            transform.pre_translate(Vec2::new(
                self.filter_rect.x0 + self.metrics.inset_x,
                self.filter_rect.y0 + (self.filter_rect.height() - f64::from(filter_h)) / 2.0,
            )),
            None,
            row_style,
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
        if new_button != self.hover_button || new_entry != self.hover_entry {
            self.hover_button = new_button;
            self.hover_entry = new_entry;
            *handled = true;
        }
    }

    /// `WindowEvent::MouseWheel` handling. Split out of `handle_event` to
    /// keep that function readable.
    fn on_mouse_wheel(&mut self, delta: &winit::event::MouseScrollDelta, handled: &mut bool) {
        let dy = match delta {
            winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
            winit::event::MouseScrollDelta::PixelDelta(pos) => pos.y / ROW_HEIGHT,
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
            self.cycle_filter();
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
        match &ke.logical_key {
            Key::Named(NamedKey::Tab) => {
                self.move_focus(shift);
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
                    Some(PickerFocus::Filter) => self.cycle_filter(),
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
                    Some(PickerFocus::Filename)
                        if matches!(ke.logical_key, Key::Named(NamedKey::Space)) =>
                    {
                        self.filename.push(' ');
                    }
                    _ => {}
                }
                *handled = true;
            }
            Key::Named(NamedKey::Backspace) if self.focus == Some(PickerFocus::Filename) => {
                self.filename.pop();
                self.ensure_valid_focus();
                *handled = true;
            }
            Key::Named(NamedKey::Backspace) if self.focus == Some(PickerFocus::List) => {
                self.go_up();
                *handled = true;
            }
            Key::Character(s) if self.focus == Some(PickerFocus::Filename) => {
                self.filename.push_str(s);
                self.ensure_valid_focus();
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
fn footer_button<'a>(
    theme: &Theme,
    rect: Rect,
    label: &'a str,
    is_primary: bool,
) -> crate::widget::ButtonFace<'a> {
    crate::widget::ButtonFace::new(rect, label)
        .radius(f64::from(theme.tokens.radii.control))
        .style(if is_primary {
            crate::widget::ButtonStyle::Primary
        } else {
            crate::widget::ButtonStyle::Neutral
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use std::fs;
    use winit::event::{DeviceId, ElementState, MouseButton};

    fn synth_device() -> DeviceId {
        // Same pattern as `permission_prompt::tests` and `slider`.
        unsafe { std::mem::zeroed() }
    }

    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: synth_device(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }

    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    fn dispatch(picker: &mut FilePicker, event: &WindowEvent) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.leaf_node(Style::default());
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        picker.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &tree,
            },
        );
    }

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
                dispatch_mods(&mut single, &left_press(), ctrl());
            }
            assert_eq!(single.selected_indices(), vec![1]);

            let mut multiple = FilePicker::new("Choose", &root, vec![root.clone()])
                .with_mode(FilePickerMode::Directory)
                .with_multiple(true);
            multiple.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            for index in [0, 1] {
                let center = multiple.entry_rect(index).unwrap().center();
                dispatch(&mut multiple, &cursor_at(center));
                dispatch_mods(&mut multiple, &left_press(), ctrl());
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
            dispatch_mods(
                &mut keyboard_single,
                &key_named(winit::keyboard::NamedKey::ArrowDown),
                shift(),
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
                dispatch_mods(&mut single, &left_press(), ctrl());
            }
            assert_eq!(single.selected_indices(), vec![2]);

            let mut default_multiple = FilePicker::new("Open", &root, vec![root.clone()]);
            default_multiple.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            for index in [1, 2] {
                let center = default_multiple.entry_rect(index).unwrap().center();
                dispatch(&mut default_multiple, &cursor_at(center));
                dispatch_mods(&mut default_multiple, &left_press(), ctrl());
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

            dispatch(&mut picker, &left_press());
            assert_eq!(picker.current_dir(), root.as_path());
            assert_eq!(picker.selection(), Some(0));

            dispatch(&mut picker, &left_press());
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
            dispatch(&mut picker, &left_press());
            let confirm = picker.open_rect().center();
            dispatch(&mut picker, &cursor_at(confirm));
            dispatch(&mut picker, &left_press());

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
            dispatch(&mut picker, &left_press());
            assert_eq!(picker.current_dir(), root.join("nested").as_path());
        }

        fn click_on_file_row_sets_selection() {
            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
            picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            let center = picker.entry_rect(1).expect("row 1").center();
            dispatch(&mut picker, &cursor_at(center));
            dispatch(&mut picker, &left_press());
            assert_eq!(picker.selection(), Some(1));
        }

        fn click_cancel_button_resolves_cancelled() {
            let (_g, root) = temp_tree();
            let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
            picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
            let center = picker.cancel_rect().center();
            dispatch(&mut picker, &cursor_at(center));
            dispatch(&mut picker, &left_press());
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
            dispatch(&mut picker, &left_press());
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
            dispatch_mods(&mut picker, &left_press(), ctrl());
            let r2 = picker.entry_rect(2).unwrap().center();
            dispatch(&mut picker, &cursor_at(r2));
            dispatch_mods(&mut picker, &left_press(), ctrl());
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
            dispatch(&mut picker, &left_press()); // first click selects
            dispatch(&mut picker, &left_press()); // second click opens
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
            // Save starts on Filename. Tab leaves it for the next modal control;
            // Shift+Tab returns to the exact editable target.
            dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Tab));
            assert_eq!(picker.focus, Some(PickerFocus::Cancel));
            dispatch_mods(
                &mut picker,
                &key_named(winit::keyboard::NamedKey::Tab),
                shift(),
            );
            assert_eq!(picker.focus, Some(PickerFocus::Filename));
            dispatch(&mut picker, &key_char("x"));
            assert_eq!(picker.filename(), "x");
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

    fn dispatch_mods(picker: &mut FilePicker, event: &WindowEvent, mods: winit::event::Modifiers) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.leaf_node(Style::default());
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        picker.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &tree,
            },
        );
    }

    fn ctrl() -> winit::event::Modifiers {
        let mut s = winit::keyboard::ModifiersState::empty();
        s.set(winit::keyboard::ModifiersState::CONTROL, true);
        winit::event::Modifiers::from(s)
    }

    fn key_named(named: winit::keyboard::NamedKey) -> WindowEvent {
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = winit::keyboard::Key::Named(named);
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
    }

    fn key_char(s: &str) -> WindowEvent {
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = winit::keyboard::Key::Character(s.into());
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
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
            // Short dialog: only a handful of rows fit.
            picker.set_rect(Rect::new(0.0, 0.0, 400.0, 240.0));
            assert!(picker.entry_rect(0).is_some());
            dispatch(
                &mut picker,
                &WindowEvent::MouseWheel {
                    device_id: synth_device(),
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
            dispatch_mods(
                &mut picker,
                &key_named(winit::keyboard::NamedKey::Tab),
                shift(),
            );
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
}
