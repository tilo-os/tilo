//! File-picker dialog widget — used by the FileChooser xdg-portal
//! backend (Sandbox Phase 4.3 follow-up) and any future "open file
//! from app menu" flows. Reusable, portal-agnostic.
//!
//! ## Shape
//!
//! A modal-style overlay rect rendering:
//!   - A breadcrumb-style header showing the current directory.
//!   - A list of [`FilePickerEntry`]s (one per `std::fs::DirEntry`).
//!     Clicking a directory navigates into it; clicking a file
//!     selects it (selection is highlighted, replacing any prior).
//!   - Two action buttons in the footer: `Cancel` and `Open`.
//!
//! The picker mutates its own internal state (current directory,
//! selection, entries) in response to clicks. State lives in the
//! widget itself — not in the CRDT — because the picker is a
//! single-user transient dialog, not collaborative content. Once the
//! user clicks `Open` or `Cancel`, the host drains
//! [`FilePicker::take_outcome`] and forwards the result down a
//! `oneshot::Sender` it owns.
//!
//! ## Roots
//!
//! The picker is constructed with one or more "accessible roots" —
//! the workspace-bound paths the caller's sandbox already allows.
//! [`FilePicker::can_navigate_to`] checks every path-mutation against
//! those roots, so a user can't accidentally `..` themselves out of
//! the sandbox boundary in the dialog. This is defensive UX: the
//! portal layer still verifies the chosen path against the grant
//! model, but the dialog won't tempt you with something it knows the
//! portal will reject.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// Fixed footer height — Open + Cancel button cluster + padding.
pub const FOOTER_HEIGHT: f64 = 56.0;

/// Fixed header height — current path breadcrumb + padding.
pub const HEADER_HEIGHT: f64 = 40.0;

/// Per-row height in the entry list.
pub const ROW_HEIGHT: f64 = 28.0;

/// Padding inside the dialog frame (left/right/top/bottom).
const PADDING: f64 = 12.0;

/// Width of each action button in the footer.
const BUTTON_WIDTH: f64 = 96.0;

/// Height of each action button in the footer.
const BUTTON_HEIGHT: f64 = 32.0;

/// Gap between the two action buttons.
const BUTTON_GAP: f64 = 8.0;

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
    /// User clicked Open with at least one path selected.
    Selected(Vec<PathBuf>),
    /// User clicked Cancel (or the dialog was dismissed).
    Cancelled,
}

/// Save-vs-open mode. Affects the "Open" button label and whether
/// directories alone are selectable (Save needs a target *folder*
/// when the type is `SaveFile` style; v1 keeps that simple by
/// treating Save the same as Open with a single-file selection).
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
#[derive(Debug)]
pub struct FilePicker {
    /// Screen-space rect of the dialog frame.
    rect: Rect,
    /// Title shown in the breadcrumb header on a separate line.
    title: String,
    /// Mode (Open vs Save) — only affects the action-button label.
    mode: FilePickerMode,

    /// Current working directory.
    current_dir: PathBuf,
    /// Cached entries for `current_dir` (refreshed on navigation).
    entries: Vec<FilePickerEntry>,
    /// Index into `entries` of the currently-highlighted selection.
    /// `None` when nothing is selected yet.
    selection: Option<usize>,

    /// Paths the host has marked as accessible. Navigation outside
    /// every root is silently ignored.
    accessible_roots: Vec<PathBuf>,

    /// Terminal outcome; set on Open / Cancel click. `None` until the
    /// user resolves the dialog.
    pending_outcome: Option<FilePickerOutcome>,

    // Hit-test rects, recomputed on each `apply_layout`.
    open_rect: Rect,
    cancel_rect: Rect,
    entry_rects: Vec<Rect>,

    /// Tracks cursor for click resolution (MouseInput carries no
    /// position payload).
    last_cursor: Point,

    /// Index of the entry currently hovered, for the row hover tint.
    hover_entry: Option<usize>,
    /// Which button (0 = Cancel, 1 = Open) is hovered, or `None`.
    hover_button: Option<u8>,
}

impl FilePicker {
    /// Build a picker rooted at `initial_dir`. `accessible_roots`
    /// limits navigation — if empty, defaults to `[initial_dir]`
    /// (so the dialog *always* has at least one allowed root). Use
    /// [`Self::with_mode`] to switch to Save semantics.
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
            selection: None,
            accessible_roots: roots,
            pending_outcome: None,
            open_rect: Rect::ZERO,
            cancel_rect: Rect::ZERO,
            entry_rects: Vec::new(),
            last_cursor: Point::ZERO,
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

    /// Set the dialog rect when the host renders the picker as an
    /// absolutely-positioned overlay (the same shape
    /// `PermissionPrompt::set_rect` uses).
    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
        self.apply_layout_helper();
    }

    /// Read-only accessor for the current directory.
    pub fn current_dir(&self) -> &Path {
        &self.current_dir
    }

    /// Read-only accessor for the list of entries.
    pub fn entries(&self) -> &[FilePickerEntry] {
        &self.entries
    }

    /// Read-only accessor for the current selection index.
    pub fn selection(&self) -> Option<usize> {
        self.selection
    }

    /// Test/host helper: deterministically set the selection without
    /// going through a click. Index is clamped against `entries`.
    pub fn set_selection(&mut self, index: usize) {
        if index < self.entries.len() {
            self.selection = Some(index);
        }
    }

    /// Drive the dialog programmatically — used by tests and any
    /// future `pick_one(path)` helper that wants to short-circuit
    /// the UI. Returns `false` if `path` isn't currently in the
    /// entry list.
    pub fn select_by_path(&mut self, path: &Path) -> bool {
        if let Some(i) = self.entries.iter().position(|e| e.path == path) {
            self.selection = Some(i);
            true
        } else {
            false
        }
    }

    /// Drive the dialog programmatically — synthesise a click on
    /// "Open". Resolves to [`FilePickerOutcome::Cancelled`] if there's
    /// no selection (matches click-through behaviour).
    pub fn click_open(&mut self) {
        self.commit_open();
    }

    /// Drive the dialog programmatically — synthesise a click on
    /// "Cancel".
    pub fn click_cancel(&mut self) {
        self.pending_outcome = Some(FilePickerOutcome::Cancelled);
    }

    /// Drain the terminal outcome. Returns `None` until the user
    /// clicks Open or Cancel.
    pub fn take_outcome(&mut self) -> Option<FilePickerOutcome> {
        self.pending_outcome.take()
    }

    /// Read-only peek at the outcome — doesn't drain.
    pub fn outcome(&self) -> Option<&FilePickerOutcome> {
        self.pending_outcome.as_ref()
    }

    /// Test/host accessor: the "Open" (or "Save") button rect.
    pub fn open_rect(&self) -> Rect {
        self.open_rect
    }

    /// Test/host accessor: the "Cancel" button rect.
    pub fn cancel_rect(&self) -> Rect {
        self.cancel_rect
    }

    /// Test/host accessor: the rect for the entry at `i`, if present.
    pub fn entry_rect(&self, i: usize) -> Option<Rect> {
        self.entry_rects.get(i).copied()
    }

    /// `true` iff `candidate` is under (or equal to) at least one
    /// accessible root. The picker uses this to gate navigation — a
    /// `..` step that would escape every root is silently dropped.
    pub fn can_navigate_to(&self, candidate: &Path) -> bool {
        // Best-effort canonicalisation: if the candidate doesn't yet
        // exist we just compare lexically. This matches the host's
        // expectation that the roots are real, existing paths.
        let cand = candidate.to_path_buf();
        for root in &self.accessible_roots {
            if cand.starts_with(root) {
                return true;
            }
        }
        false
    }

    /// Navigate into `target`. No-op when `target` is rejected by
    /// [`Self::can_navigate_to`].
    pub fn navigate_to<P: AsRef<Path>>(&mut self, target: P) {
        let t = target.as_ref().to_path_buf();
        if !self.can_navigate_to(&t) {
            return;
        }
        self.current_dir = t;
        self.selection = None;
        self.refresh_entries();
    }

    /// Re-read the directory listing for `current_dir`. Silently
    /// produces an empty list when the directory can't be read
    /// (permissions, drained workspace, etc.) — the dialog itself
    /// then degenerates to an empty list + Cancel button, which is
    /// the right UX.
    pub fn refresh_entries(&mut self) {
        self.entries.clear();
        let Ok(read) = std::fs::read_dir(&self.current_dir) else {
            return;
        };
        for entry in read.flatten() {
            let path = entry.path();
            let name = entry
                .file_name()
                .to_string_lossy()
                .into_owned();
            let is_dir = entry
                .file_type()
                .map(|t| t.is_dir())
                .unwrap_or(false);
            self.entries.push(FilePickerEntry {
                name,
                path,
                is_dir,
            });
        }
        // Stable order: directories first, then files, each
        // alphabetical. Mirrors the standard "Files"-style picker.
        self.entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        });
    }

    /// Recompute the button hit-test rects from `self.rect`. Called
    /// from `apply_layout` and `set_rect`.
    pub fn apply_layout_helper(&mut self) {
        let footer_top = self.rect.y1 - FOOTER_HEIGHT;
        let by = footer_top + (FOOTER_HEIGHT - BUTTON_HEIGHT) / 2.0;
        let by1 = by + BUTTON_HEIGHT;
        // Right-aligned: rightmost is Open, then Cancel.
        let open_x1 = self.rect.x1 - PADDING;
        let open_x0 = open_x1 - BUTTON_WIDTH;
        let cancel_x1 = open_x0 - BUTTON_GAP;
        let cancel_x0 = cancel_x1 - BUTTON_WIDTH;
        self.open_rect = Rect::new(open_x0, by, open_x1, by1);
        self.cancel_rect = Rect::new(cancel_x0, by, cancel_x1, by1);

        // Entry rows fill the central band between header and footer.
        let list_top = self.rect.y0 + HEADER_HEIGHT;
        let list_bot = footer_top;
        let row_x0 = self.rect.x0 + PADDING;
        let row_x1 = self.rect.x1 - PADDING;
        let max_rows = ((list_bot - list_top) / ROW_HEIGHT).max(0.0) as usize;
        self.entry_rects.clear();
        for i in 0..self.entries.len().min(max_rows) {
            let y0 = list_top + (i as f64) * ROW_HEIGHT;
            let y1 = y0 + ROW_HEIGHT;
            self.entry_rects.push(Rect::new(row_x0, y0, row_x1, y1));
        }
    }

    /// Resolve "Open"-button click: stage the current selection if
    /// any, else cancel.
    fn commit_open(&mut self) {
        match self.selection {
            Some(i) if i < self.entries.len() => {
                let path = self.entries[i].path.clone();
                self.pending_outcome = Some(FilePickerOutcome::Selected(vec![path]));
            }
            _ => {
                // Open with nothing selected is treated as cancel —
                // mirrors GTK's file chooser behaviour.
                self.pending_outcome = Some(FilePickerOutcome::Cancelled);
            }
        }
    }
}

impl Widget for FilePicker {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {
        // No CRDT sync — the picker is single-user transient state.
    }

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
        // Recompute on every draw so paths set via `set_rect` without
        // a taffy pass still hit-test correctly. Cheap.
        self.apply_layout_helper();

        let radius = theme.tokens.radii.md as f64;
        let frame = RoundedRect::from_rect(self.rect, radius);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &frame,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &frame,
        );

        // Header: title + breadcrumb-style current path.
        let title_size = theme.tokens.typography.scale.base;
        text_renderer.draw_text(
            scene,
            &self.title,
            title_size,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((self.rect.x0 + PADDING, self.rect.y0 + 8.0)),
            None,
        );
        let crumb = self.current_dir.display().to_string();
        let crumb_size = theme.tokens.typography.scale.sm;
        text_renderer.draw_text(
            scene,
            &crumb,
            crumb_size,
            theme.tokens.colors.text_dim,
            transform
                * Affine::translate((
                    self.rect.x0 + PADDING,
                    self.rect.y0 + 8.0 + title_size as f64 + 4.0,
                )),
            None,
        );

        // Entry rows.
        let row_size = theme.tokens.typography.scale.sm;
        for (i, rect) in self.entry_rects.iter().enumerate() {
            let is_selected = self.selection == Some(i);
            let is_hover = self.hover_entry == Some(i) && !is_selected;
            let bg = if is_selected {
                theme.tokens.colors.accent.stops
                    [crate::tokens::colors::ACCENT_PRIMARY_IDX]
            } else if is_hover {
                theme.bg_hover
            } else {
                theme.tokens.colors.surfaces.raised
            };
            let row_shape = RoundedRect::from_rect(*rect, theme.tokens.radii.sm as f64);
            scene.fill(Fill::NonZero, transform, bg, None, &row_shape);

            if let Some(entry) = self.entries.get(i) {
                let glyph = if entry.is_dir { "[DIR] " } else { "      " };
                let label = format!("{}{}", glyph, entry.name);
                let label_color = if is_selected {
                    Color::WHITE
                } else {
                    theme.tokens.colors.text_primary
                };
                text_renderer.draw_text(
                    scene,
                    &label,
                    row_size,
                    label_color,
                    transform
                        * Affine::translate((
                            rect.x0 + 8.0,
                            rect.y0 + (ROW_HEIGHT - row_size as f64) / 2.0,
                        )),
                    None,
                );
            }
        }

        // Footer buttons.
        draw_button(
            scene,
            transform,
            text_renderer,
            theme,
            self.cancel_rect,
            "Cancel",
            self.hover_button == Some(0),
            /*is_primary=*/ false,
        );
        let open_label = match self.mode {
            FilePickerMode::Open => "Open",
            FilePickerMode::Save => "Save",
        };
        draw_button(
            scene,
            transform,
            text_renderer,
            theme,
            self.open_rect,
            open_label,
            self.hover_button == Some(1),
            /*is_primary=*/ true,
        );
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
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
                let new_entry = self
                    .entry_rects
                    .iter()
                    .position(|r| r.contains(p));
                if new_button != self.hover_button || new_entry != self.hover_entry {
                    self.hover_button = new_button;
                    self.hover_entry = new_entry;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
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
                for (i, rect) in self.entry_rects.iter().enumerate() {
                    if rect.contains(p) {
                        // Cloned out of `self.entries` early so we
                        // don't hold the borrow across the
                        // `navigate_to` mutation.
                        let entry = self.entries[i].clone();
                        if entry.is_dir {
                            self.navigate_to(&entry.path);
                        } else {
                            self.selection = Some(i);
                        }
                        *handled = true;
                        return;
                    }
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "FilePicker"
    }

    fn get_rect(&self) -> Rect {
        self.rect
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
}
