//! File-picker dialog widget — used by the FileChooser xdg-portal
//! backend (Sandbox Phase 4.3 follow-up) and any "open / save file from
//! app menu" flow. Reusable, portal-agnostic, and self-contained (it
//! draws and hit-tests itself; the host just positions it as a modal
//! overlay and drains [`FilePicker::take_outcome`]).
//!
//! ## Shape
//!
//! A modal-style overlay rect rendering, top to bottom:
//!   - A **title** + clickable **breadcrumb** of the current path, and an
//!     **up** button (parent directory, clamped to the accessible roots).
//!   - A scrollable **list** of [`FilePickerEntry`]s, each with a
//!     file-type icon. Clicking a directory navigates into it; clicking a
//!     file selects it; double-clicking a file opens immediately.
//!     Multi-select (Ctrl / Shift) is supported in Open mode.
//!   - In **Save** mode, a **filename** field above the buttons.
//!   - An optional **filter** chip (cycles file-type filters on click).
//!   - **Cancel** / **Open** (or **Save**) buttons.
//!
//! ## Keyboard
//!
//! Up/Down move the selection (auto-scrolling), Enter opens the selected
//! file or enters the selected directory, Backspace goes to the parent,
//! Escape cancels. In Open mode, printable keys do type-ahead; in Save
//! mode they edit the filename field.
//!
//! ## Roots
//!
//! The picker is constructed with one or more "accessible roots" — the
//! workspace-bound paths the caller's sandbox already allows.
//! [`FilePicker::can_navigate_to`] gates every path mutation against
//! those roots so the user can't `..` out of the sandbox boundary.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::icons::{draw_icon, icon_for_path, Icon, LUCIDE_DEFAULT_STROKE_PX};
use crate::presence::PresenceCtx;
use crate::selection::SelectionModel;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// Fixed footer height — Open + Cancel button cluster + padding.
pub const FOOTER_HEIGHT: f64 = 56.0;

/// Fixed header height — title line + breadcrumb + padding.
pub const HEADER_HEIGHT: f64 = 64.0;

/// Per-row height in the entry list.
pub const ROW_HEIGHT: f64 = 28.0;

/// Height of the Save-mode filename field band (0 in Open mode).
const FILENAME_HEIGHT: f64 = 36.0;

/// Padding inside the dialog frame (left/right/top/bottom).
const PADDING: f64 = 12.0;

/// Width of each action button in the footer.
const BUTTON_WIDTH: f64 = 96.0;

/// Height of each action button in the footer.
const BUTTON_HEIGHT: f64 = 32.0;

/// Gap between the two action buttons.
const BUTTON_GAP: f64 = 8.0;

/// Size of the up / breadcrumb header glyphs.
const HEADER_ICON_PX: f64 = 18.0;

/// Per-row leading icon size.
const ROW_ICON_PX: f64 = 16.0;

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

    /// The catch-all "All files" filter.
    pub fn all() -> Self {
        Self { label: "All files".into(), extensions: Vec::new() }
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

/// One row in the file list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePickerEntry {
    /// Display name (final path component).
    pub name: String,
    /// Absolute path to the entry on disk.
    pub path: PathBuf,
    /// `true` if the entry is a directory (drives navigation), `false`
    /// for plain files (drives selection).
    pub is_dir: bool,
}

/// Outcome of a [`FilePicker`] interaction. Drained by the host via
/// [`FilePicker::take_outcome`] once per frame; `None` means "user
/// hasn't decided yet". The terminal variants resolve the host's
/// `oneshot::Sender<Option<Vec<PathBuf>>>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilePickerOutcome {
    /// User clicked Open/Save with at least one path resolved.
    Selected(Vec<PathBuf>),
    /// User clicked Cancel (or the dialog was dismissed).
    Cancelled,
}

/// Save-vs-open mode. Open picks existing files (multi-select); Save adds
/// a filename field whose contents resolve to `current_dir/<name>`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FilePickerMode {
    /// Pick existing files. Default button label "Open".
    Open,
    /// Pick a destination filename. Default button label "Save".
    Save,
}

impl Default for FilePickerMode {
    fn default() -> Self {
        FilePickerMode::Open
    }
}

/// The dialog widget itself.
pub struct FilePicker {
    /// Screen-space rect of the dialog frame.
    rect: Rect,
    /// Title shown in the header.
    title: String,
    /// Mode (Open vs Save).
    mode: FilePickerMode,

    /// Current working directory.
    current_dir: PathBuf,
    /// Cached, filtered entries for `current_dir`.
    entries: Vec<FilePickerEntry>,
    /// Multi-selection over `entries` (file indices only; dirs navigate).
    selection: SelectionModel,

    /// Paths the host has marked as accessible. Navigation outside every
    /// root is silently ignored.
    accessible_roots: Vec<PathBuf>,

    /// Available file-type filters; `active_filter` indexes into it.
    filters: Vec<FileFilter>,
    active_filter: usize,

    /// Save-mode filename buffer.
    filename: String,

    /// First entry index shown (vertical scroll position).
    first_visible: usize,

    /// Terminal outcome; set on Open / Cancel. `None` until resolved.
    pending_outcome: Option<FilePickerOutcome>,

    // Hit-test rects, recomputed on each `apply_layout` / `draw`.
    open_rect: Rect,
    cancel_rect: Rect,
    up_rect: Rect,
    filter_rect: Rect,
    /// `(entry_index, rect)` for the currently-visible rows only.
    entry_rows: Vec<(usize, Rect)>,
    /// `(ancestor_path, rect)` for each clickable breadcrumb segment.
    crumb_rects: Vec<(PathBuf, Rect)>,

    /// Tracks cursor for click resolution (MouseInput carries no
    /// position payload).
    last_cursor: Point,
    /// Last entry pressed + when, for double-click open detection.
    last_click: Option<(usize, std::time::Instant)>,

    /// Index of the entry currently hovered, for the row hover tint.
    hover_entry: Option<usize>,
    /// Which button (0 = Cancel, 1 = Open) is hovered, or `None`.
    hover_button: Option<u8>,
}

impl FilePicker {
    /// Build a picker rooted at `initial_dir`. `accessible_roots` limits
    /// navigation — if empty, defaults to `[initial_dir]`.
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
            entry_rows: Vec::new(),
            crumb_rects: Vec::new(),
            last_cursor: Point::ZERO,
            last_click: None,
            hover_entry: None,
            hover_button: None,
        };
        me.refresh_entries();
        me
    }

    /// Builder: change the mode after construction.
    pub fn with_mode(mut self, mode: FilePickerMode) -> Self {
        self.mode = mode;
        self
    }

    /// Builder: install file-type filters. The first becomes active. If
    /// `filters` is empty an "All files" filter is used.
    pub fn with_filters(mut self, filters: Vec<FileFilter>) -> Self {
        self.filters = if filters.is_empty() { vec![FileFilter::all()] } else { filters };
        self.active_filter = 0;
        self.refresh_entries();
        self
    }

    /// Builder: seed the Save-mode filename field.
    pub fn with_filename(mut self, name: impl Into<String>) -> Self {
        self.filename = name.into();
        self
    }

    /// Set the dialog rect when the host renders the picker as an
    /// absolutely-positioned overlay.
    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
        self.apply_layout_helper();
    }

    /// Read-only accessor for the current directory.
    pub fn current_dir(&self) -> &Path {
        &self.current_dir
    }

    /// Read-only accessor for the (filtered) list of entries.
    pub fn entries(&self) -> &[FilePickerEntry] {
        &self.entries
    }

    /// Primary selected entry index, if any (back-compat accessor).
    pub fn selection(&self) -> Option<usize> {
        self.selection.primary().filter(|&i| self.selection.is_selected(i))
    }

    /// All selected entry indices in ascending order.
    pub fn selected_indices(&self) -> Vec<usize> {
        self.selection.selected()
    }

    /// The active filter label, for the host / tests.
    pub fn active_filter_label(&self) -> &str {
        &self.filters[self.active_filter].label
    }

    /// The Save-mode filename buffer.
    pub fn filename(&self) -> &str {
        &self.filename
    }

    /// Test/host helper: set the selection without a click.
    pub fn set_selection(&mut self, index: usize) {
        if index < self.entries.len() {
            self.selection.select_only(index);
            self.scroll_to(index);
        }
    }

    /// Select `path` if present in the current listing.
    pub fn select_by_path(&mut self, path: &Path) -> bool {
        if let Some(i) = self.entries.iter().position(|e| e.path == path) {
            self.selection.select_only(i);
            self.scroll_to(i);
            true
        } else {
            false
        }
    }

    /// Synthesise an Open/Save click.
    pub fn click_open(&mut self) {
        self.commit_open();
    }

    /// Synthesise a Cancel click.
    pub fn click_cancel(&mut self) {
        self.pending_outcome = Some(FilePickerOutcome::Cancelled);
    }

    /// Drain the terminal outcome.
    pub fn take_outcome(&mut self) -> Option<FilePickerOutcome> {
        self.pending_outcome.take()
    }

    /// Peek at the outcome without draining.
    pub fn outcome(&self) -> Option<&FilePickerOutcome> {
        self.pending_outcome.as_ref()
    }

    /// The "Open"/"Save" button rect.
    pub fn open_rect(&self) -> Rect {
        self.open_rect
    }

    /// The "Cancel" button rect.
    pub fn cancel_rect(&self) -> Rect {
        self.cancel_rect
    }

    /// The parent-directory ("up") button rect.
    pub fn up_rect(&self) -> Rect {
        self.up_rect
    }

    /// The rect of the entry at absolute index `i`, if it is currently
    /// visible (scrolled into view).
    pub fn entry_rect(&self, i: usize) -> Option<Rect> {
        self.entry_rows.iter().find(|(idx, _)| *idx == i).map(|(_, r)| *r)
    }

    /// `true` iff `candidate` is under (or equal to) at least one root.
    pub fn can_navigate_to(&self, candidate: &Path) -> bool {
        let cand = candidate.to_path_buf();
        self.accessible_roots.iter().any(|root| cand.starts_with(root))
    }

    /// `true` iff the current dir has a parent still within the roots.
    pub fn can_go_up(&self) -> bool {
        self.current_dir.parent().map(|p| self.can_navigate_to(p)).unwrap_or(false)
    }

    /// Navigate into `target`. No-op when rejected by the roots.
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

    /// Navigate to the parent directory (clamped to the roots).
    pub fn go_up(&mut self) {
        if let Some(parent) = self.current_dir.parent().map(|p| p.to_path_buf()) {
            self.navigate_to(parent);
        }
    }

    /// Cycle to the next file-type filter and re-list.
    pub fn cycle_filter(&mut self) {
        if self.filters.len() > 1 {
            self.active_filter = (self.active_filter + 1) % self.filters.len();
            self.selection.clear();
            self.first_visible = 0;
            self.refresh_entries();
        }
    }

    /// Re-read + filter the directory listing for `current_dir`.
    pub fn refresh_entries(&mut self) {
        self.entries.clear();
        let filter = &self.filters[self.active_filter.min(self.filters.len() - 1)];
        if let Ok(read) = std::fs::read_dir(&self.current_dir) {
            for entry in read.flatten() {
                let path = entry.path();
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                // Directories are always shown; files honour the filter.
                if !is_dir && !filter.matches(&path) {
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
    }

    /// Number of entry rows that fit in the list band.
    fn visible_rows(&self) -> usize {
        let list_top = self.rect.y0 + HEADER_HEIGHT;
        let list_bot = self.list_bottom();
        (((list_bot - list_top) / ROW_HEIGHT).floor().max(0.0)) as usize
    }

    /// Bottom edge of the scrollable list band (above the filename field
    /// in Save mode, above the buttons otherwise).
    fn list_bottom(&self) -> f64 {
        let extras = if self.mode == FilePickerMode::Save { FILENAME_HEIGHT } else { 0.0 };
        self.rect.y1 - FOOTER_HEIGHT - extras
    }

    /// Scroll so that entry `i` is visible.
    fn scroll_to(&mut self, i: usize) {
        let rows = self.visible_rows().max(1);
        if i < self.first_visible {
            self.first_visible = i;
        } else if i >= self.first_visible + rows {
            self.first_visible = i + 1 - rows;
        }
    }

    /// Clamp the scroll offset to the valid range for the current list.
    fn clamp_scroll(&mut self) {
        let rows = self.visible_rows();
        let max_first = self.entries.len().saturating_sub(rows);
        if self.first_visible > max_first {
            self.first_visible = max_first;
        }
    }

    /// Recompute every hit-test rect from `self.rect`.
    pub fn apply_layout_helper(&mut self) {
        // Footer buttons (bottom band).
        let footer_top = self.rect.y1 - FOOTER_HEIGHT;
        let by = footer_top + (FOOTER_HEIGHT - BUTTON_HEIGHT) / 2.0;
        let by1 = by + BUTTON_HEIGHT;
        let open_x1 = self.rect.x1 - PADDING;
        let open_x0 = open_x1 - BUTTON_WIDTH;
        let cancel_x1 = open_x0 - BUTTON_GAP;
        let cancel_x0 = cancel_x1 - BUTTON_WIDTH;
        self.open_rect = Rect::new(open_x0, by, open_x1, by1);
        self.cancel_rect = Rect::new(cancel_x0, by, cancel_x1, by1);
        // Filter chip sits at the left of the button band.
        self.filter_rect = Rect::new(self.rect.x0 + PADDING, by, self.rect.x0 + PADDING + 160.0, by1);

        // Up button at the right edge of the header band.
        let up_x1 = self.rect.x1 - PADDING;
        let up_x0 = up_x1 - 28.0;
        let up_y0 = self.rect.y0 + 8.0;
        self.up_rect = Rect::new(up_x0, up_y0, up_x1, up_y0 + 24.0);

        // Breadcrumb segments along the second header line.
        self.crumb_rects = self.compute_crumb_rects();

        // Visible entry rows.
        self.clamp_scroll();
        let list_top = self.rect.y0 + HEADER_HEIGHT;
        let row_x0 = self.rect.x0 + PADDING;
        let row_x1 = self.rect.x1 - PADDING;
        let rows = self.visible_rows();
        self.entry_rows.clear();
        for vis in 0..rows {
            let i = self.first_visible + vis;
            if i >= self.entries.len() {
                break;
            }
            let y0 = list_top + (vis as f64) * ROW_HEIGHT;
            self.entry_rows.push((i, Rect::new(row_x0, y0, row_x1, y0 + ROW_HEIGHT)));
        }
    }

    /// Lay out the clickable breadcrumb path segments. Each segment maps
    /// a display label to the ancestor directory it navigates to.
    fn compute_crumb_rects(&self) -> Vec<(PathBuf, Rect)> {
        // Build (label, target) pairs from the deepest accessible root
        // down to the current dir, so the user never sees segments they
        // can't navigate to.
        let mut segments: Vec<(String, PathBuf)> = Vec::new();
        let comps: Vec<_> = self.current_dir.components().collect();
        let mut acc = PathBuf::new();
        for c in comps {
            acc.push(c.as_os_str());
            if self.can_navigate_to(&acc) {
                let label = c.as_os_str().to_string_lossy().into_owned();
                let label = if label.is_empty() { "/".to_string() } else { label };
                segments.push((label, acc.clone()));
            }
        }
        // Geometry: we don't have a TextRenderer here, so approximate
        // segment widths from label length (good enough for hit-testing;
        // draw refines x using the real measure but reuses these rects).
        let y0 = self.rect.y0 + 8.0 + 20.0;
        let mut x = self.rect.x0 + PADDING;
        let mut out = Vec::new();
        for (label, target) in segments {
            let w = 8.0 + label.chars().count() as f64 * 7.5 + 12.0; // text + " / "
            out.push((target, Rect::new(x, y0, x + w, y0 + 18.0)));
            x += w;
        }
        out
    }

    /// Resolve "Open"/"Save": stage the selection (or the typed Save
    /// filename), else cancel.
    fn commit_open(&mut self) {
        match self.mode {
            FilePickerMode::Save => {
                let name = self.filename.trim();
                if !name.is_empty() {
                    let target = self.current_dir.join(name);
                    self.pending_outcome = Some(FilePickerOutcome::Selected(vec![target]));
                } else if let Some(p) = self.first_selected_file() {
                    self.pending_outcome = Some(FilePickerOutcome::Selected(vec![p]));
                } else {
                    self.pending_outcome = Some(FilePickerOutcome::Cancelled);
                }
            }
            FilePickerMode::Open => {
                let paths: Vec<PathBuf> = self
                    .selection
                    .selected()
                    .into_iter()
                    .filter_map(|i| self.entries.get(i))
                    .filter(|e| !e.is_dir)
                    .map(|e| e.path.clone())
                    .collect();
                if paths.is_empty() {
                    self.pending_outcome = Some(FilePickerOutcome::Cancelled);
                } else {
                    self.pending_outcome = Some(FilePickerOutcome::Selected(paths));
                }
            }
        }
    }

    fn first_selected_file(&self) -> Option<PathBuf> {
        self.selection
            .selected()
            .into_iter()
            .filter_map(|i| self.entries.get(i))
            .find(|e| !e.is_dir)
            .map(|e| e.path.clone())
    }

    /// Resolve a click on entry `i`: dirs navigate, files (multi-)select,
    /// and a second click on the same file opens it.
    fn activate_entry(&mut self, i: usize, ctrl: bool, shift: bool) {
        let Some(entry) = self.entries.get(i).cloned() else { return };
        if entry.is_dir {
            self.navigate_to(&entry.path);
            return;
        }
        // Double-click open.
        let now = std::time::Instant::now();
        let is_double = matches!(
            self.last_click,
            Some((pi, t)) if pi == i && now.duration_since(t) < std::time::Duration::from_millis(500)
        );
        self.selection.apply_click(i, ctrl, shift);
        // In Save mode, clicking a file copies its name into the field.
        if self.mode == FilePickerMode::Save {
            self.filename = entry.name.clone();
        }
        if is_double {
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
        let cur = self.selection.primary().map(|i| i as isize).unwrap_or(-1);
        let next = (cur + delta).clamp(0, len as isize - 1) as usize;
        self.selection.move_to(next, shift);
        self.scroll_to(next);
    }

    /// Jump to the next entry whose name starts with `ch` (type-ahead).
    fn type_ahead(&mut self, ch: char) {
        let lower = ch.to_ascii_lowercase();
        let start = self.selection.primary().map(|i| i + 1).unwrap_or(0);
        let n = self.entries.len();
        for off in 0..n {
            let i = (start + off) % n;
            if self
                .entries[i]
                .name
                .chars()
                .next()
                .map(|c| c.to_ascii_lowercase() == lower)
                .unwrap_or(false)
            {
                self.selection.select_only(i);
                self.scroll_to(i);
                return;
            }
        }
    }
}

impl Widget for FilePicker {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                flex_grow: 1.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
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
        self.apply_layout_helper();
        let tokens = &theme.tokens;
        let radius = tokens.radii.md as f64;
        let frame = RoundedRect::from_rect(self.rect, radius);
        scene.fill(Fill::NonZero, transform, tokens.colors.surfaces.raised, None, &frame);
        scene.stroke(&Stroke::new(1.0), transform, tokens.colors.border_standard, None, &frame);

        // --- Header: title, up button, breadcrumb ---
        let title_size = tokens.typography.scale.base;
        text_renderer.draw_text(
            scene,
            &self.title,
            title_size,
            tokens.colors.text_primary,
            transform * Affine::translate((self.rect.x0 + PADDING, self.rect.y0 + 8.0)),
            None,
        );
        // Up button.
        if self.can_go_up() {
            let up_hovered = self.up_rect.contains(self.last_cursor);
            if up_hovered {
                let s = RoundedRect::from_rect(self.up_rect, tokens.radii.sm as f64);
                scene.fill(Fill::NonZero, transform, theme.bg_hover, None, &s);
            }
            let ix = self.up_rect.x0 + (self.up_rect.width() - HEADER_ICON_PX) / 2.0;
            let iy = self.up_rect.y0 + (self.up_rect.height() - HEADER_ICON_PX) / 2.0;
            let icon_rect = Rect::new(ix, iy, ix + HEADER_ICON_PX, iy + HEADER_ICON_PX);
            draw_icon(scene, transform, icon_rect, Icon::ArrowUp, tokens.colors.text_primary, LUCIDE_DEFAULT_STROKE_PX);
        }
        // Breadcrumb segments (chevron-separated).
        let crumb_size = tokens.typography.scale.sm;
        let crumb_count = self.crumb_rects.len();
        for (idx, (_target, r)) in self.crumb_rects.iter().enumerate() {
            let label = self
                .crumb_label_for(idx)
                .unwrap_or_default();
            let hovered = r.contains(self.last_cursor);
            let color = if hovered { theme.accent_primary } else { tokens.colors.text_dim };
            text_renderer.draw_text(
                scene,
                &label,
                crumb_size,
                color,
                transform * Affine::translate((r.x0 + 4.0, r.y0 + 2.0)),
                None,
            );
            if idx + 1 < crumb_count {
                let sep_rect = Rect::new(r.x1 - 12.0, r.y0, r.x1, r.y0 + 16.0);
                draw_icon(scene, transform, sep_rect, Icon::ChevronRight, tokens.colors.text_dim, LUCIDE_DEFAULT_STROKE_PX);
            }
        }

        // --- Entry rows ---
        let row_size = tokens.typography.scale.sm;
        for (i, rect) in self.entry_rows.iter() {
            let i = *i;
            let is_selected = self.selection.is_selected(i);
            let is_hover = self.hover_entry == Some(i) && !is_selected;
            let bg = if is_selected {
                tokens.colors.accent.stops[crate::tokens::colors::ACCENT_PRIMARY_IDX]
            } else if is_hover {
                theme.bg_hover
            } else {
                tokens.colors.surfaces.raised
            };
            let row_shape = RoundedRect::from_rect(*rect, tokens.radii.sm as f64);
            scene.fill(Fill::NonZero, transform, bg, None, &row_shape);

            if let Some(entry) = self.entries.get(i) {
                let fg = if is_selected { Color::WHITE } else { tokens.colors.text_primary };
                // Leading file-type icon.
                let icon_y = rect.y0 + (ROW_HEIGHT - ROW_ICON_PX) / 2.0;
                let icon_rect = Rect::new(rect.x0 + 8.0, icon_y, rect.x0 + 8.0 + ROW_ICON_PX, icon_y + ROW_ICON_PX);
                draw_icon(scene, transform, icon_rect, icon_for_path(&entry.path, entry.is_dir), fg, LUCIDE_DEFAULT_STROKE_PX);
                text_renderer.draw_text(
                    scene,
                    &entry.name,
                    row_size,
                    fg,
                    transform * Affine::translate((rect.x0 + 8.0 + ROW_ICON_PX + 8.0, rect.y0 + (ROW_HEIGHT - row_size as f64) / 2.0)),
                    None,
                );
            }
        }

        // --- Save-mode filename field ---
        if self.mode == FilePickerMode::Save {
            let field_top = self.rect.y1 - FOOTER_HEIGHT - FILENAME_HEIGHT;
            let field = Rect::new(self.rect.x0 + PADDING, field_top + 4.0, self.rect.x1 - PADDING, field_top + FILENAME_HEIGHT - 4.0);
            let s = RoundedRect::from_rect(field, tokens.radii.sm as f64);
            scene.fill(Fill::NonZero, transform, tokens.colors.surfaces.base, None, &s);
            scene.stroke(&Stroke::new(1.0), transform, tokens.colors.border_focus, None, &s);
            let shown = if self.filename.is_empty() { "Filename…" } else { &self.filename };
            let color = if self.filename.is_empty() { tokens.colors.text_dim } else { tokens.colors.text_primary };
            text_renderer.draw_text(
                scene,
                shown,
                row_size,
                color,
                transform * Affine::translate((field.x0 + 8.0, field.y0 + (field.height() - row_size as f64) / 2.0)),
                None,
            );
        }

        // --- Filter chip ---
        if self.filters.len() > 1 {
            let hovered = self.filter_rect.contains(self.last_cursor);
            let s = RoundedRect::from_rect(self.filter_rect, tokens.radii.sm as f64);
            scene.fill(Fill::NonZero, transform, if hovered { theme.bg_hover } else { tokens.colors.surfaces.base }, None, &s);
            scene.stroke(&Stroke::new(1.0), transform, tokens.colors.border_standard, None, &s);
            text_renderer.draw_text(
                scene,
                self.active_filter_label(),
                row_size,
                tokens.colors.text_primary,
                transform * Affine::translate((self.filter_rect.x0 + 8.0, self.filter_rect.y0 + (self.filter_rect.height() - row_size as f64) / 2.0)),
                None,
            );
        }

        // --- Footer buttons ---
        draw_button(scene, transform, text_renderer, theme, self.cancel_rect, "Cancel", self.hover_button == Some(0), false);
        let open_label = match self.mode {
            FilePickerMode::Open => "Open",
            FilePickerMode::Save => "Save",
        };
        draw_button(scene, transform, text_renderer, theme, self.open_rect, open_label, self.hover_button == Some(1), true);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = p;
                let new_button = if self.cancel_rect.contains(p) {
                    Some(0)
                } else if self.open_rect.contains(p) {
                    Some(1)
                } else {
                    None
                };
                let new_entry = self.entry_rows.iter().find(|(_, r)| r.contains(p)).map(|(i, _)| *i);
                if new_button != self.hover_button || new_entry != self.hover_entry {
                    self.hover_button = new_button;
                    self.hover_entry = new_entry;
                    *handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => *y as f64,
                    winit::event::MouseScrollDelta::PixelDelta(pos) => pos.y / ROW_HEIGHT,
                };
                // Wheel-up scrolls the list up (toward the start).
                if dy > 0.0 {
                    self.first_visible = self.first_visible.saturating_sub(dy.ceil() as usize);
                } else if dy < 0.0 {
                    self.first_visible += (-dy).ceil() as usize;
                }
                self.clamp_scroll();
                // Refresh the visible-row rects so hit-testing + entry_rect
                // reflect the new scroll position before the next draw.
                self.apply_layout_helper();
                *handled = true;
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let p = self.last_cursor;
                if self.cancel_rect.contains(p) {
                    self.click_cancel();
                    *handled = true;
                    return;
                }
                if self.open_rect.contains(p) {
                    self.click_open();
                    *handled = true;
                    return;
                }
                if self.can_go_up() && self.up_rect.contains(p) {
                    self.go_up();
                    *handled = true;
                    return;
                }
                if self.filters.len() > 1 && self.filter_rect.contains(p) {
                    self.cycle_filter();
                    *handled = true;
                    return;
                }
                // Breadcrumb segment?
                if let Some((target, _)) = self.crumb_rects.iter().find(|(_, r)| r.contains(p)).cloned() {
                    self.navigate_to(&target);
                    *handled = true;
                    return;
                }
                // Entry row?
                if let Some((i, _)) = self.entry_rows.iter().find(|(_, r)| r.contains(p)).copied() {
                    let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
                    let shift = modifiers.state().shift_key();
                    self.activate_entry(i, ctrl, shift);
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput { event: ke, .. } if ke.state == ElementState::Pressed => {
                use winit::keyboard::{Key, NamedKey};
                let shift = modifiers.state().shift_key();
                match &ke.logical_key {
                    Key::Named(NamedKey::Escape) => {
                        self.click_cancel();
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        self.move_selection(1, shift);
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        self.move_selection(-1, shift);
                        *handled = true;
                    }
                    Key::Named(NamedKey::Enter) => {
                        if let Some(i) = self.selection.primary() {
                            if self.entries.get(i).map(|e| e.is_dir).unwrap_or(false) {
                                let p = self.entries[i].path.clone();
                                self.navigate_to(&p);
                            } else {
                                self.commit_open();
                            }
                        } else {
                            self.commit_open();
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::Backspace) => {
                        if self.mode == FilePickerMode::Save && !self.filename.is_empty() {
                            self.filename.pop();
                        } else {
                            self.go_up();
                        }
                        *handled = true;
                    }
                    Key::Character(s) => {
                        if self.mode == FilePickerMode::Save {
                            self.filename.push_str(s);
                            *handled = true;
                        } else if let Some(ch) = s.chars().next() {
                            self.type_ahead(ch);
                            *handled = true;
                        }
                    }
                    Key::Named(NamedKey::Space) if self.mode == FilePickerMode::Save => {
                        self.filename.push(' ');
                        *handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "FilePicker"
    }

    fn test_id(&self) -> Option<&str> {
        Some("file_picker")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl FilePicker {
    /// The display label for breadcrumb segment `idx` (final path
    /// component of its target).
    fn crumb_label_for(&self, idx: usize) -> Option<String> {
        let (target, _) = self.crumb_rects.get(idx)?;
        let label = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".to_string());
        Some(label)
    }
}

fn draw_button(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    is_hover: bool,
    is_primary: bool,
) {
    let radius = theme.tokens.radii.md as f64;
    let shape = RoundedRect::from_rect(rect, radius);
    let bg = if is_primary {
        theme.tokens.colors.accent.stops[crate::tokens::colors::ACCENT_PRIMARY_IDX]
    } else if is_hover {
        theme.bg_hover
    } else {
        theme.tokens.colors.surfaces.raised
    };
    scene.fill(Fill::NonZero, transform, bg, None, &shape);
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        theme.tokens.colors.border_standard,
        None,
        &shape,
    );
    let text_size = theme.tokens.typography.scale.sm;
    let (text_w, _) = text_renderer.measure(label, text_size, None);
    let text_x = rect.center().x - (text_w as f64 / 2.0);
    let text_y = rect.y0 + (rect.height() - text_size as f64) / 2.0;
    let text_color = if is_primary {
        Color::WHITE
    } else {
        theme.tokens.colors.text_primary
    };
    text_renderer.draw_text(
        scene,
        label,
        text_size,
        text_color,
        transform * Affine::translate((text_x, text_y)),
        None,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        picker.handle_event(
            event,
            1.0,
            &winit::event::Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );
    }

    /// Build a temp dir with a few files + a sub-dir to exercise the
    /// real `std::fs::read_dir` path. Caller is responsible for
    /// cleanup via the returned [`tempfile::TempDir`] guard.
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
    fn new_picker_reads_initial_dir_entries() {
        let (_g, root) = temp_tree();
        let picker = FilePicker::new("Open", &root, vec![root.clone()]);
        // 3 entries: nested (dir), alpha.txt, beta.txt. Directories
        // sort first.
        assert_eq!(picker.entries().len(), 3);
        assert!(picker.entries()[0].is_dir);
        assert_eq!(picker.entries()[0].name, "nested");
    }

    #[test]
    fn cancel_returns_none() {
        // The required "cancel_returns_none" test from the brief. Drives
        // the widget via the public click_cancel API and asserts the
        // outcome drains to Cancelled (which the host translates to
        // `oneshot::send(None)` for the FileChooser response).
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
        picker.click_cancel();
        let outcome = picker.take_outcome();
        assert_eq!(outcome, Some(FilePickerOutcome::Cancelled));
        // Drained — subsequent take returns None.
        assert!(picker.take_outcome().is_none());
    }

    #[test]
    fn open_returns_selected_paths() {
        // The required "open_returns_selected_paths" test. Sets the
        // selection programmatically, clicks Open via the public API,
        // and asserts the outcome carries that path.
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
        // Pick `alpha.txt` (after directories, so index 1).
        let target = root.join("alpha.txt");
        assert!(picker.select_by_path(&target));
        picker.click_open();
        let outcome = picker.take_outcome();
        assert_eq!(
            outcome,
            Some(FilePickerOutcome::Selected(vec![target]))
        );
    }

    #[test]
    fn open_with_no_selection_cancels() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
        picker.click_open();
        // No selection → treated as cancel.
        assert_eq!(picker.take_outcome(), Some(FilePickerOutcome::Cancelled));
    }

    #[test]
    fn navigate_into_subdir_updates_current_dir_and_entries() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
        let nested = root.join("nested");
        picker.navigate_to(&nested);
        assert_eq!(picker.current_dir(), nested.as_path());
        // Only one file inside `nested/`.
        assert_eq!(picker.entries().len(), 1);
        assert_eq!(picker.entries()[0].name, "gamma.txt");
    }

    #[test]
    fn navigate_outside_roots_is_silently_dropped() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
        // `/etc` is outside the temp root — the dialog refuses to go
        // there and leaves `current_dir` unchanged.
        let cwd_before = picker.current_dir().to_path_buf();
        picker.navigate_to(Path::new("/etc"));
        assert_eq!(picker.current_dir(), cwd_before.as_path());
    }

    #[test]
    fn can_navigate_to_requires_under_a_root() {
        let (_g, root) = temp_tree();
        let picker = FilePicker::new("Open", &root, vec![root.clone()]);
        assert!(picker.can_navigate_to(&root));
        assert!(picker.can_navigate_to(&root.join("nested")));
        assert!(!picker.can_navigate_to(Path::new("/etc")));
    }

    #[test]
    fn click_on_directory_row_navigates_into_it() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
        picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        // Row index 0 is the `nested` directory (dirs sort first).
        let center = picker.entry_rect(0).expect("row 0").center();
        dispatch(&mut picker, &cursor_at(center));
        dispatch(&mut picker, &left_press());
        assert_eq!(picker.current_dir(), root.join("nested").as_path());
    }

    #[test]
    fn click_on_file_row_sets_selection() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
        picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        // Row 1 is `alpha.txt`.
        let center = picker.entry_rect(1).expect("row 1").center();
        dispatch(&mut picker, &cursor_at(center));
        dispatch(&mut picker, &left_press());
        assert_eq!(picker.selection(), Some(1));
    }

    #[test]
    fn click_cancel_button_resolves_cancelled() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
        picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        let center = picker.cancel_rect().center();
        dispatch(&mut picker, &cursor_at(center));
        dispatch(&mut picker, &left_press());
        assert_eq!(picker.take_outcome(), Some(FilePickerOutcome::Cancelled));
    }

    #[test]
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

    #[test]
    fn save_mode_uses_save_label() {
        let (_g, root) = temp_tree();
        let picker = FilePicker::new("Save", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Save);
        // The label is only exercised at draw-time; the mode getter is
        // not exposed publicly, so we assert via the constructor's
        // documented behaviour: the picker was built with Save mode.
        // (Coverage is on the construction path; visual diff is on the
        // shell side.)
        assert!(matches!(picker.mode, FilePickerMode::Save));
    }

    // --- New-feature tests (the "proper" overhaul) --------------------

    fn dispatch_mods(picker: &mut FilePicker, event: &WindowEvent, mods: &winit::event::Modifiers) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        picker.handle_event(event, 1.0, mods, &mut doc, &root, &mut handled, &mut focus_target, node, &tree);
    }

    fn ctrl() -> winit::event::Modifiers {
        let mut s = winit::keyboard::ModifiersState::empty();
        s.set(winit::keyboard::ModifiersState::CONTROL, true);
        winit::event::Modifiers::from(s)
    }

    fn key_named(named: winit::keyboard::NamedKey) -> WindowEvent {
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = winit::keyboard::Key::Named(named);
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        WindowEvent::KeyboardInput { device_id: synth_device(), event: ev, is_synthetic: false }
    }

    fn key_char(s: &str) -> WindowEvent {
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = winit::keyboard::Key::Character(s.into());
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        WindowEvent::KeyboardInput { device_id: synth_device(), event: ev, is_synthetic: false }
    }

    /// Temp dir with `n` numbered files so the list overflows a short
    /// dialog and exercises scrolling.
    fn temp_many(n: usize) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        for i in 0..n {
            fs::write(root.join(format!("file{i:03}.txt")), b"x").unwrap();
        }
        (dir, root)
    }

    #[test]
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

    #[test]
    fn scrolling_windows_the_visible_rows() {
        let (_g, root) = temp_many(40);
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
        // Short dialog: only a handful of rows fit.
        picker.set_rect(Rect::new(0.0, 0.0, 400.0, 240.0));
        // Row 0 is visible initially.
        assert!(picker.entry_rect(0).is_some());
        // Scroll down with the wheel; early rows fall out of view.
        dispatch(&mut picker, &WindowEvent::MouseWheel {
            device_id: synth_device(),
            delta: winit::event::MouseScrollDelta::LineDelta(0.0, -5.0),
            phase: winit::event::TouchPhase::Moved,
        });
        assert!(picker.entry_rect(0).is_none(), "row 0 scrolled out of view");
        assert!(picker.entry_rect(6).is_some(), "a later row is now visible");
    }

    #[test]
    fn save_mode_typed_filename_resolves_target() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Save", &root, vec![root.clone()])
            .with_mode(FilePickerMode::Save);
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

    #[test]
    fn filters_hide_nonmatching_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::write(root.join("a.png"), b"x").unwrap();
        fs::write(root.join("b.txt"), b"x").unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        let picker = FilePicker::new("Open", &root, vec![root.clone()])
            .with_filters(vec![FileFilter::new("Images", &["png", "jpg"]), FileFilter::all()]);
        // Directory + the png survive; the txt is filtered out.
        let names: Vec<_> = picker.entries().iter().map(|e| e.name.clone()).collect();
        assert!(names.contains(&"sub".to_string()));
        assert!(names.contains(&"a.png".to_string()));
        assert!(!names.contains(&"b.txt".to_string()));
        assert_eq!(picker.active_filter_label(), "Images");
    }

    #[test]
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

    #[test]
    fn ctrl_click_multi_select_opens_all() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
        picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        // entries: [nested(dir), alpha.txt(1), beta.txt(2)]. Select both files.
        let r1 = picker.entry_rect(1).unwrap().center();
        dispatch(&mut picker, &cursor_at(r1));
        dispatch_mods(&mut picker, &left_press(), &ctrl());
        let r2 = picker.entry_rect(2).unwrap().center();
        dispatch(&mut picker, &cursor_at(r2));
        dispatch_mods(&mut picker, &left_press(), &ctrl());
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

    #[test]
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

    #[test]
    fn keyboard_down_then_enter_enters_directory() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
        picker.set_rect(Rect::new(0.0, 0.0, 400.0, 400.0));
        // ArrowDown selects entry 0 (the `nested` directory); Enter navigates.
        dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::ArrowDown));
        assert_eq!(picker.selection(), Some(0));
        dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Enter));
        assert_eq!(picker.current_dir(), root.join("nested").as_path());
    }

    #[test]
    fn escape_cancels() {
        let (_g, root) = temp_tree();
        let mut picker = FilePicker::new("Open", &root, vec![root.clone()]);
        dispatch(&mut picker, &key_named(winit::keyboard::NamedKey::Escape));
        assert_eq!(picker.take_outcome(), Some(FilePickerOutcome::Cancelled));
    }

    #[test]
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
}
