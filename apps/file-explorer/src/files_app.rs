//! `FileExplorerWidget` — the qOS file explorer's `AppWidget`.
//!
//! The widget owns all app state (current dir, entries, multi-selection,
//! sort, view mode, navigation history) and rebuilds its child widget tree
//! from that state. Interactive children can't borrow the app back, so they
//! push [`FileCommand`]s into a shared [`CommandSink`]; `handle_event`
//! drains the sink after each event and applies the commands, marking the
//! tree structurally dirty so the App rebuilds the composition on the next
//! frame. See [`crate::command`].

use crate::command::{new_sink, CommandSink, FileCommand};
use crate::model::{initial_path, read_dir_sync, sort_entries, Entry, SortBy, SortOrder, ViewMode};
use crate::places_sidebar::{self, FavStore, PlacesSidebar};
use crate::{config, favorites, fileops, templates};
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::{
    Column, NavShell, Row, ScrollAxis, Scrollable, ShellMode, Spacer, Text, TextInput, Widget,
};
use ui::{AppKind, AppWidget, IconDescriptor, SelectionModel};
use vello::kurbo::Point;

/// CRDT map key holding the inline-rename field's `Text` object. Seeded with
/// the current name when a rename begins; the in-place editor's `TextInput`
/// edits it and `commit_rename` reads the chosen name back out.
const RENAME_QUERY_PATH: &str = "files.rename_query";

/// Width (logical px) at/above which the places list docks beside the file
/// view; below it the places list becomes an overlay drawer *over* the file
/// view, summoned by the hamburger chip. The file list is the content either
/// way — the places list is navigation, so it never takes the window from it
/// (`ui/HIG.md` §1a).
const BODY_BREAKPOINT_PX: f64 = 640.0;

/// Ensure a `Text` object exists at `key` on `root`, returning its id. The
/// toolkit's `TextInput` binds to (and writes into) an existing `Text`
/// object but never creates one, so the app must seed it — otherwise
/// keystrokes mutate only the input's local string and never reach the CRDT.
fn ensure_text_obj(
    state: &mut automerge::AutoCommit,
    root: &automerge::ObjId,
    key: &str,
) -> automerge::ObjId {
    use automerge::transaction::Transactable;
    use automerge::{ObjType, ReadDoc, Value};
    if let Ok(Some((Value::Object(ObjType::Text), id))) = state.get(root, key) {
        return id;
    }
    state
        .put_object(root, key, ObjType::Text)
        .expect("put_object Text never fails")
}

pub struct FileExplorerWidget {
    current_path: PathBuf,
    home_path: Option<PathBuf>,
    entries: Vec<Entry>,
    /// Multi-selection over the displayed entry list.
    selection: SelectionModel,
    error: Option<String>,
    is_loading: bool,
    last_click: Option<(usize, Instant)>,
    sort_by: SortBy,
    sort_order: SortOrder,
    view_mode: ViewMode,
    /// Back/forward navigation history (paths only).
    history: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    /// Clipboard for cut/copy → paste. `true` = cut (move on paste).
    clipboard: Vec<PathBuf>,
    clipboard_cut: bool,
    /// Transient status line (operation results), shown in the footer.
    status: Option<String>,
    /// When `Some(i)`, an inline rename of displayed entry `i` is in
    /// progress: the in-place editor is drawn over row/cell `i` and its
    /// `TextInput` edits [`RENAME_QUERY_PATH`]. `None` = not renaming.
    renaming: Option<usize>,
    /// Editable name mirrored from the rename field's `Text` object each
    /// `sync` while renaming; `commit_rename` renames the file to this.
    rename_buffer: String,
    /// Set when a rename just began so the next `sync` seeds the CRDT
    /// `Text` object with `rename_buffer` (the current name) before the
    /// input reads it back.
    rename_seed_pending: bool,
    /// The in-place rename editor: a persistent, CRDT-bound `TextInput` the
    /// app owns (so its caret/focus survive the per-frame rebuild) and draws
    /// **over the selected row/cell** — not in a bottom bar. It binds to
    /// [`RENAME_QUERY_PATH`], so keystrokes splice into the same `Text` object
    /// `rename_buffer` mirrors.
    rename_input: TextInput,
    /// Where the file view wants the rename editor drawn: the on-screen rect of
    /// the renaming row's name (list) or label (grid), published by `FileView`
    /// each paint. `None` when not renaming or the row is scrolled off-screen.
    rename_rect: Rc<Cell<Option<vello::kurbo::Rect>>>,
    /// Set on `begin_rename` so the first `sync` after the seed selects the
    /// whole name (typing then replaces it, Finder-style).
    rename_select_pending: bool,
    rect: vello::kurbo::Rect,
    /// The responsive body: `[ places | file view ]`. Held persistently (NOT
    /// rebuilt inside `build_layout`) so its docked/push state and eased
    /// reveal survive the per-structure-change rebuild of the panes — the two
    /// panes are swapped in via `NavShell::set_sidebar` / `set_content` each
    /// rebuild. Wide docks the places list beside the file view; narrow shows
    /// one at a time (places → tap a place → the file view pushes in with a
    /// Back arrow). See the module note on live child swap in `ui::NavShell`.
    nav: NavShell,
    /// The single top bar: nav cluster + the editable path bar + view/sort/menu
    /// controls. Held **persistently** (not rebuilt each frame) because the path
    /// bar carries live edit state — the in-progress typed path and its
    /// completion popover — that must survive the per-structure-change rebuild.
    toolbar: crate::toolbar::FileToolbar,
    /// Chrome below the body (rename bar + footer), rebuilt each structure
    /// change; spans the full width.
    chrome_below: Option<Box<dyn Widget>>,
    /// Command queue the interactive children push into.
    cmds: CommandSink,
    /// Set when applying a command changed structure; consumed by
    /// `take_structure_dirty` so the App rebuilds the composition.
    structure_dirty: bool,
    /// Right-click menu, owned here (not in the rebuilt composition) so it
    /// stays open across frames. `menu_commands[i]` is the command to run
    /// for menu item `i` (None for separators).
    context_menu: ContextMenu,
    menu_commands: Vec<Option<FileCommand>>,
    /// The "New File ▸" flyout submenu, populated from the Templates dir when
    /// the main menu opens; empty (and closed) when there are no templates.
    submenu: ContextMenu,
    submenu_commands: Vec<Option<FileCommand>>,
    /// Set while draining commands from a right-press when the press landed on
    /// a sidebar favourite, so the right-press handler opens the favourite menu
    /// (not the file context menu) at the cursor. Consumed each right-press.
    pending_fav_menu: Option<PathBuf>,
    /// Last cursor position in logical coords (for menu placement + the
    /// context-menu hit-test).
    last_cursor: Point,
    /// Shared, persistent favourites + drag-reorder state (see
    /// [`crate::places_sidebar`]). Cloned into the sidebar each rebuild.
    favs: FavStore,
    /// Per-user config dir for persisting favourites + zoom (`None` when the
    /// environment has no config dir — the UI still works, just doesn't persist).
    config_dir: Option<PathBuf>,
    /// Icon/row zoom step (see [`crate::config`]); `0` = default.
    zoom_level: i32,
    /// Whether dotfiles are shown.
    show_hidden: bool,
    /// The user's real, localized XDG user directories (Documents, Downloads,
    /// …). Drives the standard favourites so a German desktop shows
    /// *Dokumente / Bilder / Musik*, not hard-coded English names.
    user_dirs: favorites::UserDirs,
    /// The qOS collaborative-workspace roots the FUSE driver mounts, surfaced as
    /// first-class System locations in the sidebar. Resolved once at startup.
    workspace_roots: Vec<PathBuf>,
    /// A folder being dragged out of the file list toward the sidebar to pin it
    /// to Favourites. `Some(path)` while the drag is live; a floating ghost
    /// tracks the cursor and a drop over the sidebar pins it. `None` otherwise.
    pin_drag: Option<PathBuf>,
    /// The content pane's live scroll position, owned here so it survives the
    /// per-frame rebuild. Each rebuilt list/grid `Scrollable` binds to this
    /// cell (see [`ui::widget::Scrollable::with_persisted_offset`]); without
    /// it, selecting a row, typing in search, or changing sort — all of which
    /// rebuild the composition — would snap the list back to the top. Reset to
    /// `(0, 0)` whenever the directory changes (a new folder starts at top).
    scroll_offset: Rc<Cell<(f32, f32)>>,
}

impl Default for FileExplorerWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl FileExplorerWidget {
    pub fn new() -> Self {
        Self::with_persistence(config::config_dir(), dirs::home_dir())
    }

    /// Construct with an explicit persistence directory + home, resolving the
    /// XDG user dirs from the real environment. `new` uses the real per-user
    /// config dir; tests pass a `tempdir` (or `None`) so they never read or
    /// write the user's real favourites/zoom state.
    #[doc(hidden)]
    pub fn with_persistence(config_dir: Option<PathBuf>, home: Option<PathBuf>) -> Self {
        Self::with_persistence_dirs(config_dir, home, favorites::UserDirs::from_system())
    }

    /// Construct with an explicit persistence directory, home, and injected
    /// XDG user directories. Tests inject a known (e.g. German-localized) set
    /// so the standard favourites are deterministic without depending on the
    /// build machine's locale.
    #[doc(hidden)]
    pub fn with_persistence_dirs(
        config_dir: Option<PathBuf>,
        home: Option<PathBuf>,
        user_dirs: favorites::UserDirs,
    ) -> Self {
        let cwd = initial_path();
        let zoom_level = config_dir.as_deref().map(config::load_zoom).unwrap_or(0);
        // Favourites: persisted list when we have a home + config dir, else the
        // built-in defaults so the sidebar is never empty. Standard entries
        // follow the user's real, localized XDG directories.
        let favorites = match (home.as_deref(), config_dir.as_deref()) {
            (Some(h), Some(dir)) => favorites::load(dir, h, &user_dirs),
            (Some(h), None) => favorites::defaults(h, &user_dirs),
            (None, _) => Vec::new(),
        };
        let show_hidden = false;
        let mut entries = read_dir_sync(&cwd).unwrap_or_default();
        if !show_hidden {
            entries.retain(|e| !e.name.starts_with('.'));
        }
        sort_entries(SortBy::Name, SortOrder::Ascending, &mut entries);
        let cmds = new_sink();
        Self {
            current_path: cwd,
            home_path: home,
            entries,
            selection: SelectionModel::new(),
            error: None,
            is_loading: false,
            last_click: None,
            sort_by: SortBy::Name,
            sort_order: SortOrder::Ascending,
            view_mode: ViewMode::List,
            history: Vec::new(),
            forward: Vec::new(),
            clipboard: Vec::new(),
            clipboard_cut: false,
            status: None,
            renaming: None,
            rename_buffer: String::new(),
            rename_seed_pending: false,
            rename_input: TextInput::new(vec![])
                .with_id("files.rename_input")
                .with_on_change_path(RENAME_QUERY_PATH)
                .with_placeholder("New name"),
            rename_rect: Rc::new(Cell::new(None)),
            rename_select_pending: false,
            rect: vello::kurbo::Rect::ZERO,
            // Placeholder panes; the real places list + file view are swapped
            // in on the first `build_layout`. The places list is the shell's
            // *sidebar*, not a content overview: docked beside the file view
            // when wide, an overlay drawer over it when narrow. Sidebar width
            // matches the historical fixed sidebar so the docked layout is
            // pixel-stable.
            nav: NavShell::sidebar_over_content(
                Box::new(Text::new("")),
                Box::new(Text::new("")),
            )
            .with_sidebar_width(places_sidebar::SIDEBAR_WIDTH)
            .with_breakpoint(BODY_BREAKPOINT_PX)
            // The docked places list is permanent — no in-sidebar collapse chip;
            // the hamburger still summons the drawer when narrow.
            .hide_toggle_when_docked(),
            toolbar: crate::toolbar::FileToolbar::new(cmds.clone()),
            chrome_below: None,
            cmds,
            structure_dirty: false,
            context_menu: ContextMenu::new(),
            menu_commands: Vec::new(),
            submenu: ContextMenu::new(),
            submenu_commands: Vec::new(),
            pending_fav_menu: None,
            last_cursor: Point::ZERO,
            favs: places_sidebar::new_store(favorites),
            config_dir,
            zoom_level,
            show_hidden,
            user_dirs,
            workspace_roots: crate::model::workspace_roots(),
            pin_drag: None,
            scroll_offset: Rc::new(Cell::new((0.0, 0.0))),
        }
    }

    /// The current zoom scale multiplier (icon/row size).
    fn zoom_scale(&self) -> f64 {
        config::zoom_scale(self.zoom_level)
    }

    /// Persist the favourites list (best-effort).
    fn save_favorites(&self) {
        if let Some(dir) = self.config_dir.as_deref() {
            favorites::save(dir, &self.favs.borrow().favorites);
        }
    }

    /// The entries currently shown. (There is no in-app search field — search
    /// is OS-owned per `ui/HIG.md` §5 — so this is the sorted directory
    /// listing as-is.)
    pub fn displayed_entries(&self) -> Vec<Entry> {
        self.entries.clone()
    }

    /// Wrap `child` in a full-width chrome band with a consistent horizontal
    /// inset from the spacing scale (so toolbar / breadcrumb / footer content
    /// lines up across bands while the panels below still reach the window
    /// edges). `vpad` sets the band's vertical breathing room.
    fn band(theme: &ui::Theme, child: Box<dyn Widget>, vpad: f32) -> Box<dyn Widget> {
        let sp = &theme.tokens.spacing;
        Box::new(
            Column::new()
                .with_intrinsic_height()
                .with_horizontal_padding(sp.s3, sp.s3)
                .with_vertical_padding(vpad)
                .push(child),
        )
    }

    /// Refresh the persistent top bar with the current navigation context
    /// (path, history availability, view mode) before it is laid out.
    fn update_toolbar(&mut self) {
        let can_back = !self.history.is_empty();
        let can_forward = !self.forward.is_empty();
        let can_up = self.current_path.parent().is_some();
        self.toolbar.set_context(
            self.current_path.clone(),
            self.home_path.clone(),
            can_back,
            can_forward,
            can_up,
            self.view_mode,
        );
    }

    /// The **places** pane (the responsive body's master / overview): the
    /// modern Favourites / Places list. A single widget that owns
    /// drag-to-reorder for the favourites (via the shared, persistent
    /// `FavStore`) so a reorder survives the per-frame rebuild.
    fn build_places_pane(&self) -> Box<dyn Widget> {
        Box::new(
            PlacesSidebar::new(self.favs.clone(), self.current_path.clone(), self.cmds.clone())
                .with_workspaces(self.workspace_roots.clone()),
        )
    }

    /// The **content** pane (the responsive body's detail): the file list or
    /// grid (plus any error affordance), scrolling under a symmetric gutter.
    fn build_content_pane(&self, theme: &ui::Theme) -> Box<dyn Widget> {
        let displayed = self.displayed_entries();
        let scale = self.zoom_scale();
        let view_child: Box<dyn Widget> = match self.view_mode {
            ViewMode::List => crate::views::list_view::build_list(
                &displayed,
                &self.selection,
                &self.cmds,
                scale,
                self.sort_by,
                self.sort_order,
                self.renaming,
                self.rename_rect.clone(),
            ),
            ViewMode::Grid => crate::views::grid_view::build_grid(
                &displayed,
                &self.selection,
                &self.cmds,
                scale,
                self.renaming,
                self.rename_rect.clone(),
            ),
        };
        let sp = &theme.tokens.spacing;
        // In the narrow layout the shell paints the hamburger chip over the
        // content pane's top-left (it summons the places drawer); reserve a
        // strip so the list header + rows start clear of it instead of under
        // it. Wide docks the places list, with no chip over the content.
        let chip_reserve = if self.nav.mode() == ShellMode::Narrow { 44.0 } else { 0.0 };
        // A plain (percent-height) column: this pane is the root of `NavShell`'s
        // inner layout, sized to the content rect, so it must FILL that rect
        // (`percent(1.0)` height). `with_fill_remaining_height` (flex-grow from
        // zero) needs a flex parent to distribute space and would collapse to
        // its content height here — leaving the `Scrollable` zero-height. Inside
        // this bounded column the `Scrollable` grows to take the space under the
        // optional error row.
        let mut view_col = Column::new()
            .with_vertical_padding(sp.s1)
            .with_extra_top_padding(chip_reserve)
            .with_horizontal_padding(sp.s3, sp.s3);
        if let Some(err) = self.error.as_deref() {
            // Real error affordance: the message plus a "Try Again" button
            // wired to re-read the directory (Refresh).
            let sink = self.cmds.clone();
            let retry = ui::widget::Button::new("files.retry", "Try Again", None)
                .on_click(move || crate::command::push(&sink, FileCommand::Refresh));
            view_col = view_col.push(Box::new(
                Row::new()
                    .push(Box::new(Text::new(err)))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(retry)),
            ));
        }
        // The list/grid scrolls: pin the scroll viewport to the *remaining*
        // height of the content column (grow-from-zero) rather than letting it
        // claim `percent(1.0)`. `fill_parent_height` takes exactly the space
        // left under the optional error row so a long directory overflows the
        // viewport and the wheel + scrollbar thumb move the list.
        view_col = view_col.push(Box::new(
            Scrollable::new(ScrollAxis::Vertical)
                .fill_parent_height()
                .with_persisted_offset(self.scroll_offset.clone())
                .with_child(view_child),
        ));
        Box::new(view_col)
    }

    /// The chrome below the responsive body: the inline-rename bar (only while
    /// a rename is active) and the footer. Spans the full width.
    fn build_chrome_below(&self, theme: &ui::Theme) -> Box<dyn Widget> {
        let sp = &theme.tokens.spacing;
        // Footer: an operation status, else the item count. The current path is
        // shown ONCE — in the path bar at the top — so it is deliberately not
        // repeated here. The right side summarises the selection when there is
        // one (a live-state readout, not a path).
        let count = self.entries.len();
        let left = self
            .status
            .clone()
            .unwrap_or_else(|| format!("{count} {}", if count == 1 { "item" } else { "items" }));
        let sel = self.selection.selected().len();
        let right = if sel > 0 { format!("{sel} selected") } else { String::new() };
        let footer = Row::new()
            .push(Box::new(Text::new(&left).with_size(12.0).with_color(theme.tokens.colors.text_dim)))
            .push(Box::new(Spacer::new()))
            .push(Box::new(Text::new(&right).with_size(12.0).with_color(theme.tokens.colors.text_dim)));

        // The rename editor is drawn in place over the row (see `draw`), not in a
        // bottom bar — so the footer is the only chrome below the body.
        let col = Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .with_gap(sp.s0)
            .push(Self::band(theme, Box::new(footer), sp.s2));
        Box::new(col)
    }

    pub fn current_path(&self) -> PathBuf {
        self.current_path.clone()
    }
    pub fn home_path(&self) -> Option<&std::path::Path> {
        self.home_path.as_deref()
    }
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
    pub fn selected_index(&self) -> Option<usize> {
        self.selection.primary().filter(|&i| self.selection.is_selected(i))
    }
    /// All currently-selected displayed-entry indices.
    pub fn selected_indices(&self) -> Vec<usize> {
        self.selection.selected()
    }
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub fn is_loading(&self) -> bool {
        self.is_loading
    }
    pub fn sort_by(&self) -> SortBy {
        self.sort_by
    }
    pub fn sort_order(&self) -> SortOrder {
        self.sort_order
    }
    pub fn view_mode(&self) -> ViewMode {
        self.view_mode
    }

    /// Navigate to `path`, pushing the current path onto the back-history.
    pub fn navigate(&mut self, path: PathBuf) {
        if path != self.current_path {
            self.history.push(self.current_path.clone());
            self.forward.clear();
        }
        self.load(path);
    }

    /// Re-read `path` into the view without touching history (shared by
    /// navigate/back/forward/refresh).
    fn load(&mut self, path: PathBuf) {
        self.is_loading = true;
        self.selection.clear();
        self.error = None;
        self.last_click = None;
        self.status = None;
        // A new directory starts at the top; the persisted scroll offset only
        // survives *structural* rebuilds within the same directory.
        self.scroll_offset.set((0.0, 0.0));
        match read_dir_sync(&path) {
            Ok(mut entries) => {
                if !self.show_hidden {
                    entries.retain(|e| !e.name.starts_with('.'));
                }
                sort_entries(self.sort_by, self.sort_order, &mut entries);
                self.entries = entries;
            }
            Err(e) => {
                // Drop stale entries so the error view isn't backed by the old
                // directory's contents.
                self.entries.clear();
                self.error = Some(e);
            }
        }
        // Always record the path we attempted, even on error, so a Refresh
        // ("Try Again") retries the same target rather than reloading the
        // previous valid directory and silently clearing the error.
        self.current_path = path;
        self.is_loading = false;
    }

    /// Go to the parent directory (history-aware).
    pub fn back(&mut self) {
        if let Some(parent) = self.current_path.parent() {
            self.navigate(parent.to_path_buf());
        }
    }

    /// Go up to the parent directory (alias of [`back`] kept explicit for
    /// the toolbar's distinct Up affordance).
    pub fn go_up(&mut self) {
        self.back();
    }

    /// Pop the back-history.
    pub fn history_back(&mut self) {
        if let Some(prev) = self.history.pop() {
            self.forward.push(self.current_path.clone());
            self.load(prev);
        }
    }

    /// Pop the forward-history.
    pub fn history_forward(&mut self) {
        if let Some(next) = self.forward.pop() {
            self.history.push(self.current_path.clone());
            self.load(next);
        }
    }

    pub fn refresh(&mut self) {
        self.load(self.current_path.clone());
    }

    /// Single/double click handler retained for tests + the command path.
    /// A second click within 500 ms on the same row returns its path.
    pub fn select(&mut self, idx: Option<usize>) -> Option<PathBuf> {
        let now = Instant::now();
        if let (Some(i), Some((pi, pt))) = (idx, self.last_click) {
            if i == pi && now.duration_since(pt) < Duration::from_millis(500) {
                let disp = self.displayed_entries();
                if let Some(e) = disp.get(i) {
                    let path = e.path.clone();
                    self.last_click = None;
                    return Some(path);
                }
            }
        }
        match idx {
            Some(i) => self.selection.select_only(i),
            None => self.selection.clear(),
        }
        self.last_click = idx.map(|i| (i, now));
        None
    }

    pub fn set_sort_by(&mut self, s: SortBy) {
        self.sort_by = s;
        sort_entries(self.sort_by, self.sort_order, &mut self.entries);
    }
    pub fn toggle_sort_order(&mut self) {
        self.sort_order = match self.sort_order {
            SortOrder::Ascending => SortOrder::Descending,
            SortOrder::Descending => SortOrder::Ascending,
        };
        sort_entries(self.sort_by, self.sort_order, &mut self.entries);
    }
    pub fn set_view_mode(&mut self, m: ViewMode) {
        self.view_mode = m;
    }

    pub fn entries_snapshot(&self) -> &[Entry] {
        &self.entries
    }

    pub fn error_message(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn footer_text(&self) -> (usize, String) {
        (
            self.entries.len(),
            crate::model::format_path(&self.current_path, self.home_path.as_deref()),
        )
    }

    // --- Command application ------------------------------------------

    /// Apply one queued command, returning whether it changed the widget
    /// tree structure (and thus needs a rebuild). Crate-visible so sibling
    /// modules' tests can drive the real command-application path.
    pub(crate) fn apply_command(&mut self, cmd: FileCommand) -> bool {
        match cmd {
            FileCommand::Navigate(p) => {
                self.navigate(p);
                true
            }
            FileCommand::Up => {
                self.go_up();
                true
            }
            FileCommand::Back => {
                self.history_back();
                true
            }
            FileCommand::Forward => {
                self.history_forward();
                true
            }
            FileCommand::Refresh => {
                self.refresh();
                true
            }
            FileCommand::Click { index, ctrl, shift } => {
                if ctrl || shift {
                    self.selection.apply_click(index, ctrl, shift);
                    self.last_click = None;
                } else if let Some(path) = self.select(Some(index)) {
                    // Double-click → open / enter.
                    self.open_path(path);
                }
                true
            }
            FileCommand::ContextSelect(target) => {
                match target {
                    // A right-press on a row that isn't already part of the
                    // selection selects just it; one inside an existing
                    // multi-selection leaves that selection intact so the menu
                    // action applies to every highlighted item.
                    Some(i) if !self.selection.is_selected(i) => self.selection.select_only(i),
                    Some(_) => {}
                    // Right-press on empty space clears the selection.
                    None => self.selection.clear(),
                }
                self.last_click = None;
                true
            }
            FileCommand::Activate(index) => {
                let disp = self.displayed_entries();
                if let Some(e) = disp.get(index) {
                    self.open_path(e.path.clone());
                }
                true
            }
            FileCommand::SortColumn(col) => {
                let by = match col {
                    1 => SortBy::Name,
                    2 => SortBy::Size,
                    3 => SortBy::Modified,
                    _ => return false,
                };
                if self.sort_by == by {
                    self.toggle_sort_order();
                } else {
                    self.set_sort_by(by);
                }
                true
            }
            FileCommand::SetView(m) => {
                self.set_view_mode(m);
                true
            }
            FileCommand::SetSort(by) => {
                self.set_sort_by(by);
                true
            }
            FileCommand::ToggleSortOrder => {
                self.toggle_sort_order();
                true
            }
            FileCommand::SelectAll => {
                self.selection.select_all(self.displayed_entries().len());
                true
            }
            FileCommand::OpenTerminal => {
                let _ = self.open_terminal(&mut SystemRunner);
                false
            }
            FileCommand::NewFolder => {
                match fileops::new_folder(&self.current_path, "New Folder") {
                    Ok(path) => {
                        self.refresh();
                        // Select the freshly-created folder and drop into
                        // inline-rename so the user names it right away.
                        if let Some(i) = self
                            .displayed_entries()
                            .iter()
                            .position(|e| e.path == path)
                        {
                            self.selection.select_only(i);
                            self.begin_rename();
                        }
                        self.status = Some("Created folder".to_string());
                    }
                    Err(e) => self.status = Some(format!("New folder failed: {e}")),
                }
                true
            }
            FileCommand::Rename => {
                self.begin_rename();
                true
            }
            FileCommand::RenameCommit => {
                self.commit_rename();
                true
            }
            FileCommand::RenameCancel => {
                self.cancel_rename();
                true
            }
            FileCommand::Delete => {
                let paths = self.selected_paths();
                if !paths.is_empty() {
                    match fileops::delete_to_trash(&paths) {
                        Ok(()) => {
                            self.refresh();
                            self.status = Some(format!("Moved {} to Trash", paths.len()));
                        }
                        Err(e) => self.status = Some(format!("Delete failed: {e}")),
                    }
                }
                true
            }
            FileCommand::Copy => {
                self.clipboard = self.selected_paths();
                self.clipboard_cut = false;
                self.status = Some(format!("Copied {}", self.clipboard.len()));
                true
            }
            FileCommand::Cut => {
                self.clipboard = self.selected_paths();
                self.clipboard_cut = true;
                self.status = Some(format!("Cut {}", self.clipboard.len()));
                true
            }
            FileCommand::Paste => {
                self.paste();
                true
            }
            FileCommand::OpenTemplatesMenu => {
                // Opened from the menu-routing path with real geometry; if it
                // arrives via the command queue just open it at the cursor.
                self.open_templates_submenu(self.last_cursor);
                true
            }
            FileCommand::NewFromTemplate(template) => {
                match templates::create_from_template(&template, &self.current_path) {
                    Ok(path) => {
                        self.refresh();
                        if let Some(i) = self.displayed_entries().iter().position(|e| e.path == path) {
                            self.selection.select_only(i);
                            self.begin_rename();
                        }
                        self.status = Some("Created file".to_string());
                    }
                    Err(e) => self.status = Some(format!("New file failed: {e}")),
                }
                true
            }
            FileCommand::BeginDrag(path) => {
                // Start carrying the folder toward the sidebar; the ghost + the
                // drop are owned by `handle_event`.
                self.pin_drag = Some(path);
                true
            }
            FileCommand::AddFavorite(path) => {
                let added = {
                    let home = self.home_path.clone().unwrap_or_else(|| PathBuf::from("/"));
                    let mut favs = self.favs.borrow_mut();
                    favorites::add(&mut favs.favorites, path, &home, &self.user_dirs)
                };
                if added {
                    self.save_favorites();
                    self.status = Some("Added to Favourites".to_string());
                }
                true
            }
            FileCommand::RemoveFavorite(path) => {
                let removed = {
                    let mut favs = self.favs.borrow_mut();
                    favorites::remove(&mut favs.favorites, &path)
                };
                if removed {
                    self.save_favorites();
                    self.status = Some("Removed from Favourites".to_string());
                }
                true
            }
            FileCommand::ReorderFavorite { from, to } => {
                {
                    let mut favs = self.favs.borrow_mut();
                    favorites::reorder(&mut favs.favorites, from, to);
                }
                self.save_favorites();
                true
            }
            FileCommand::ZoomIn => {
                self.set_zoom(self.zoom_level + 1);
                true
            }
            FileCommand::ZoomOut => {
                self.set_zoom(self.zoom_level - 1);
                true
            }
            FileCommand::ToggleHidden => {
                self.show_hidden = !self.show_hidden;
                self.refresh();
                true
            }
            FileCommand::OpenPath(p) => {
                // A typed/accepted path from the editable path bar: open a file,
                // navigate into a directory (or a not-yet-existing path, so the
                // error affordance surfaces rather than silently doing nothing).
                if p.is_file() {
                    self.open_path(p);
                } else {
                    self.navigate(p);
                }
                true
            }
            FileCommand::OpenViewMenu => {
                self.open_view_menu();
                true
            }
            FileCommand::OpenSortMenu => {
                self.open_sort_menu();
                true
            }
            FileCommand::SetSortOrder(order) => {
                if self.sort_order != order {
                    self.toggle_sort_order();
                }
                true
            }
            FileCommand::OpenFavoriteMenu(path) => {
                // Remember the target; the right-press handler opens the menu at
                // the cursor once this event's commands are drained.
                self.pending_fav_menu = Some(path);
                false
            }
        }
    }

    /// Set the zoom level (clamped) and persist it.
    fn set_zoom(&mut self, level: i32) {
        let level = config::clamp_zoom(level);
        if level != self.zoom_level {
            self.zoom_level = level;
            if let Some(dir) = self.config_dir.as_deref() {
                config::save_zoom(dir, level);
            }
        }
    }

    /// The current zoom level (for tests).
    pub fn zoom_level(&self) -> i32 {
        self.zoom_level
    }

    /// Whether hidden files are shown (for tests).
    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    /// Borrow the favourites list (for tests).
    pub fn favorites(&self) -> Vec<crate::favorites::Favorite> {
        self.favs.borrow().favorites.clone()
    }

    /// Shared favourites store (tests / the headless shots demo).
    #[doc(hidden)]
    pub fn favorites_store(&self) -> FavStore {
        self.favs.clone()
    }

    /// Open the right-click context menu at `at` (headless shots demo).
    #[doc(hidden)]
    pub fn debug_open_context_menu(&mut self, at: Point) {
        self.open_context_menu(at);
    }

    /// Open the context menu *and* its "New File" flyout (headless shots demo).
    #[doc(hidden)]
    pub fn debug_show_templates_menu(&mut self, at: Point) {
        self.open_context_menu(at);
        let anchor = Point::new(self.context_menu.get_rect().x1 - 6.0, at.y + 66.0);
        self.open_templates_submenu(anchor);
    }

    /// Begin an active favourite drag from `from` toward `to`, for a mid-drag
    /// screenshot. Requires the sidebar to have been laid out first (so the
    /// favourites band geometry is published).
    #[doc(hidden)]
    pub fn debug_begin_favorite_drag(&mut self, from: usize, to: usize) -> bool {
        crate::places_sidebar::debug_begin_drag(&self.favs, from, to)
    }

    /// Open the places drawer over the file view (the narrow layout's
    /// hamburger action), settled deterministically for a headless
    /// screenshot. In the wide layout the places list is already docked.
    #[doc(hidden)]
    pub fn debug_open_places_drawer(&mut self) {
        self.nav.open_sidebar();
        self.nav.snap();
    }

    /// Whether the places drawer / docked sidebar is currently revealed.
    #[doc(hidden)]
    pub fn debug_places_open(&self) -> bool {
        self.nav.is_sidebar_open()
    }

    /// The file-view pane's resolved rect — what a test asserts against to
    /// prove the file list is actually on screen.
    #[doc(hidden)]
    pub fn debug_content_rect(&self) -> vello::kurbo::Rect {
        self.nav.overview_rect()
    }

    /// The docked sidebar's resolved rect — the drag-to-pin drop target.
    #[doc(hidden)]
    pub fn debug_sidebar_rect(&self) -> vello::kurbo::Rect {
        self.nav.sidebar_rect()
    }

    /// Whether a drag-to-pin is currently in flight (a folder is being carried
    /// toward the sidebar).
    #[doc(hidden)]
    pub fn debug_pin_dragging(&self) -> bool {
        self.pin_drag.is_some()
    }

    /// Start a drag-to-pin ghost carrying `path`, with the cursor at `at` — for
    /// a mid-drag screenshot.
    #[doc(hidden)]
    pub fn debug_begin_pin_drag(&mut self, path: PathBuf, at: Point) {
        self.pin_drag = Some(path);
        self.last_cursor = at;
    }

    /// The responsive body's current mode (docked-wide vs push-narrow) — for
    /// tests asserting the sidebar reflows with the window width.
    #[doc(hidden)]
    pub fn debug_body_mode(&self) -> ShellMode {
        self.nav.mode()
    }



    /// Select the given displayed-entry indices (headless shots demo).
    #[doc(hidden)]
    pub fn debug_select(&mut self, indices: &[usize]) {
        self.selection.clear();
        for (n, &i) in indices.iter().enumerate() {
            if n == 0 {
                self.selection.select_only(i);
            } else {
                self.selection.toggle(i);
            }
        }
        self.structure_dirty = true;
    }

    fn primary_path(&self) -> Option<PathBuf> {
        let disp = self.displayed_entries();
        self.selection
            .primary()
            .and_then(|i| disp.get(i).map(|e| e.path.clone()))
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        let disp = self.displayed_entries();
        self.selection
            .selected()
            .into_iter()
            .filter_map(|i| disp.get(i).map(|e| e.path.clone()))
            .collect()
    }

    fn paste(&mut self) {
        if self.clipboard.is_empty() {
            return;
        }
        let dest = self.current_path.clone();
        let cut = self.clipboard_cut;
        let items = self.clipboard.clone();
        let mut errors = 0usize;
        for src in &items {
            let res = if cut {
                fileops::move_into(src, &dest)
            } else {
                fileops::copy_into(src, &dest)
            };
            if res.is_err() {
                errors += 1;
            }
        }
        if cut {
            self.clipboard.clear();
        }
        self.refresh();
        self.status = Some(if errors == 0 {
            format!("Pasted {}", items.len())
        } else {
            format!("Pasted {} ({errors} failed)", items.len() - errors)
        });
    }

    /// Open a path: navigate into directories, else launch its app.
    fn open_path(&mut self, path: PathBuf) {
        let mut runner = SystemRunner;
        let _ = self.open_target(path, &mut runner);
    }

    /// Open `path`: navigate into directories, and hand a file to the
    /// desktop to open in whichever app owns it.
    ///
    /// The explorer picks no app of its own. It asks the host
    /// ([`set_document_opener`]), which resolves the owner, grants the
    /// read, focuses the document if it is already open, and launches —
    /// all in one place, shared with every other surface that can open a
    /// file. Only when no host installed a route (a standalone run, off a
    /// qOS session) does this fall back to the desktop default handler
    /// (`open::that`, via `CommandRunner::open_default`).
    ///
    /// A refusal from the host is shown in the status bar naming the
    /// file; activation never silently does nothing.
    ///
    /// Returns whether the path was handed off (`false` when it navigated
    /// a dir).
    fn open_target<R: CommandRunner>(
        &mut self,
        path: PathBuf,
        runner: &mut R,
    ) -> std::io::Result<bool> {
        if path.is_dir() {
            self.navigate(path);
            return Ok(false);
        }
        match ask_host_to_open(&path) {
            Some(Ok(())) => {}
            Some(Err(reason)) => self.status = Some(reason),
            None => runner.open_default(&path)?,
        }
        Ok(true)
    }

    // --- inline rename -------------------------------------------------

    /// The transient status line shown in the footer — an operation
    /// result, or the reason an open was refused. Readable so a host (or
    /// a test standing in for one) can confirm the explorer told the user
    /// something rather than swallowing it.
    pub fn status_line(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// Whether an inline rename is currently in progress.
    pub fn is_renaming(&self) -> bool {
        self.renaming.is_some()
    }

    /// The current editable rename text (the field's live contents).
    pub fn rename_buffer(&self) -> &str {
        &self.rename_buffer
    }

    /// Replace the editable rename text. The next `sync` seeds this into the
    /// rename field's CRDT `Text` object; used by tests to drive the flow
    /// without a keystroke round-trip through the widget tree.
    pub fn set_rename_buffer(&mut self, s: String) {
        self.rename_buffer = s;
        self.rename_seed_pending = true;
    }

    /// Begin an inline rename of the primary selection: open the in-place editor
    /// over the row, seeded with the entry's current name. No-op without a
    /// selection.
    pub fn begin_rename(&mut self) {
        let disp = self.displayed_entries();
        if let Some(i) = self.selection.primary() {
            if let Some(e) = disp.get(i) {
                self.renaming = Some(i);
                self.rename_buffer = e.name.clone();
                self.rename_seed_pending = true;
                self.rename_select_pending = true;
                self.rename_input.set_focus(true);
                self.status = None;
            }
        }
    }

    /// Tear down the in-place editor: drop focus and clear its published rect so
    /// no stale field is drawn after a rename ends.
    fn end_rename_editor(&mut self) {
        self.renaming = None;
        self.rename_seed_pending = false;
        self.rename_select_pending = false;
        self.rename_input.set_focus(false);
        self.rename_rect.set(None);
    }

    /// Abandon the in-progress rename, leaving the file untouched.
    pub fn cancel_rename(&mut self) {
        self.end_rename_editor();
    }

    /// Paint the app background and lazily build the composition if it is
    /// missing (a tab activated after boot never ran `build_layout`). Shared
    /// by [`Widget::draw`] and [`Widget::draw_with_presence`].
    fn paint_background(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        theme: &ui::Theme,
    ) {
        // Paint the app background so the explorer is self-contained (light
        // text reads on it, and it looks right over any compositor surface).
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        // The chrome + body panes are normally built inside `build_layout`.
        // When this widget is hosted inside a shell's TabBar (e.g. the Files
        // tab on a qOS shell that boots Notes-active), `build_layout` doesn't
        // run for inactive tabs — so switching to Files later produces a widget
        // with no chrome built and nothing visible renders even though
        // `self.rect` is correct. Flag structure-dirty here so the host runs a
        // full rebuild (which populates the chrome + swaps the real panes into
        // the persistent `nav`) on the next frame.
        let _ = theme;
        if self.chrome_below.is_none() && self.rect.width() > 0.0 && self.rect.height() > 0.0 {
            self.structure_dirty = true;
        }
    }

    /// Draw the in-place rename editor over the row/cell being renamed. The
    /// file view publishes the target rect (in on-screen coords) during its own
    /// paint and omits that row's label, so the field slots exactly where the
    /// name was. Nothing is drawn when not renaming or when the row is scrolled
    /// out of view (the published rect is then `None`).
    fn draw_rename_editor(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if self.renaming.is_none() {
            return;
        }
        if let Some(rect) = self.rename_rect.get() {
            self.rename_input.apply_layout_external(rect);
            self.rename_input.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    /// Draw the floating folder ghost that tracks the cursor during a
    /// drag-to-pin, so the drag reads as carrying the folder toward the sidebar.
    fn draw_pin_ghost(
        &self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
    ) {
        use vello::kurbo::{Affine, Rect, RoundedRect, Stroke};
        use vello::peniko::Fill;
        let Some(path) = &self.pin_drag else { return };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let tokens = &theme.tokens;
        let size = tokens.typography.scale.base;
        let (tw, _) = text_renderer.measure(&name, size, None);
        let (pad, icon, gap, h) = (10.0_f64, 16.0_f64, 8.0_f64, 30.0_f64);
        let w = pad + icon + gap + tw as f64 + pad;
        // Offset from the cursor so the ghost sits beside, not under, the pointer.
        let x0 = self.last_cursor.x + 14.0;
        let y0 = self.last_cursor.y + 6.0;
        let rect = Rect::new(x0, y0, x0 + w, y0 + h);
        let shape = RoundedRect::from_rect(rect, tokens.radii.md as f64);
        scene.fill(Fill::NonZero, transform, tokens.colors.surfaces.raised, None, &shape);
        scene.stroke(&Stroke::new(1.0), transform, tokens.colors.border_standard, None, &shape);
        let accent = tokens.colors.accent.stops[ui::tokens::colors::ACCENT_PRIMARY_IDX];
        let iy = y0 + (h - icon) / 2.0;
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::new(x0 + pad, iy, x0 + pad + icon, iy + icon),
            ui::icons::Icon::Folder,
            accent,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        let ty = crate::text_layout::centered_text_y(y0, h, size);
        text_renderer.draw_text(
            scene,
            &name,
            size,
            tokens.colors.text_primary,
            transform * Affine::translate((x0 + pad + icon + gap, ty)),
            None,
        );
    }

    /// Commit the in-progress rename: rename the target file to
    /// `rename_buffer` (trimmed). Returns whether the file was renamed. A
    /// blank or unchanged name just closes the bar; an fs error keeps the
    /// bar open with a status message so the user can retry.
    pub fn commit_rename(&mut self) -> bool {
        let Some(idx) = self.renaming else { return false };
        let disp = self.displayed_entries();
        let Some(entry) = disp.get(idx) else {
            self.end_rename_editor();
            return false;
        };
        let path = entry.path.clone();
        let new_name = self.rename_buffer.trim().to_string();
        let unchanged = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|cur| cur == new_name);
        if new_name.is_empty() || unchanged {
            self.end_rename_editor();
            return false;
        }
        match fileops::rename(&path, &new_name) {
            Ok(_) => {
                self.end_rename_editor();
                self.refresh();
                self.status = Some("Renamed".to_string());
                true
            }
            Err(e) => {
                // Keep the bar open so the user can fix the name.
                self.status = Some(format!("Rename failed: {e}"));
                false
            }
        }
    }
}

/// Resolve a standalone [`ContextMenu`]'s rect from its anchor (it isn't part
/// of the App's taffy tree, so drive a throwaway layout pass). Shared by the
/// main menu and the "New File" flyout.
fn layout_menu_widget(menu: &mut ContextMenu) {
    if !menu.is_open() {
        return;
    }
    let mut taffy: taffy::TaffyTree<()> = taffy::TaffyTree::new();
    let mut node_map: std::collections::HashMap<taffy::NodeId, *mut dyn Widget> =
        std::collections::HashMap::new();
    let theme = ui::Theme::dark();
    let node = menu.build_layout(&mut taffy, &theme, &mut node_map);
    let _ = taffy.compute_layout(
        node,
        taffy::Size {
            width: taffy::AvailableSpace::Definite(400.0),
            height: taffy::AvailableSpace::Definite(800.0),
        },
    );
    menu.apply_layout(&taffy, node, Point::ZERO);
}

/// Thin mockable wrapper around process launch + the desktop "open with
/// default app" handler. Mocked in tests so the open/launch paths are
/// verifiable without actually spawning anything.
pub trait CommandRunner {
    fn spawn(&mut self, program: &str, cwd: &std::path::Path) -> std::io::Result<()>;
    fn spawn_arg(&mut self, program: &str, arg: &std::path::Path, cwd: &std::path::Path) -> std::io::Result<()>;
    /// Open `path` with the system default handler. This is the fallback for
    /// every file type qOS has no registered app for (images, PDFs, video,
    /// audio, plain text, …) so double-click/Enter/Open never silently do
    /// nothing.
    fn open_default(&mut self, path: &std::path::Path) -> std::io::Result<()>;
}

pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn spawn(&mut self, program: &str, cwd: &std::path::Path) -> std::io::Result<()> {
        std::process::Command::new(program).current_dir(cwd).spawn().map(|_c| ())
    }
    fn spawn_arg(&mut self, program: &str, arg: &std::path::Path, cwd: &std::path::Path) -> std::io::Result<()> {
        std::process::Command::new(program).arg(arg).current_dir(cwd).spawn().map(|_c| ())
    }
    fn open_default(&mut self, path: &std::path::Path) -> std::io::Result<()> {
        open::that(path)
    }
}

/// Whether `path` is an editable document, per qdoc's codec registry.
///
/// A presentation fact only — it drives the document glyph in the icon
/// column. It deliberately says nothing about *which* app opens the file:
/// that is the desktop's answer, from the manifests of what is installed,
/// and the explorer asks for it rather than deciding it (see
/// [`set_document_opener`]).
pub fn is_document(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(qdoc::is_supported_ext)
}

/// How the explorer hands a document to the application that owns it.
///
/// `Ok(())` means the host took it. `Err(reason)` is a failure the user
/// must be told about, in words naming the file — the explorer shows it
/// in its status bar.
pub type DocumentOpener = Box<dyn FnMut(&std::path::Path) -> Result<(), String>>;

thread_local! {
    static DOCUMENT_OPENER: std::cell::RefCell<Option<DocumentOpener>> =
        const { std::cell::RefCell::new(None) };
}

/// Install the route by which this explorer opens documents.
///
/// The explorer does not decide which app opens a `.odt`, and does not
/// launch one. That decision belongs to the desktop — which app declares
/// the type, which sandbox it runs in, which read capability it needs,
/// and whether the document is already open somewhere — and it lives in
/// one place, the compositor's `shell::shell::doc_open`. An explorer
/// hosted by the compositor is wired straight to it here; nothing about
/// opening a document is reimplemented on this side.
///
/// Thread-local rather than a global because what it points at is the
/// compositor's chrome, which is thread-affine, and because it then
/// reaches *every* explorer instance the host creates without threading a
/// parameter through each widget constructor.
///
/// With nothing installed — the explorer run standalone, with no
/// compositor to ask — activation falls back to the desktop's own
/// handler. That is the honest answer off a qOS session, not a second
/// implementation of the decision.
pub fn set_document_opener<F>(opener: F)
where
    F: FnMut(&std::path::Path) -> Result<(), String> + 'static,
{
    DOCUMENT_OPENER.with(|slot| *slot.borrow_mut() = Some(Box::new(opener)));
}

/// Remove the installed route, restoring the standalone fallback. Used by
/// tests, which share the thread-local with each other.
pub fn clear_document_opener() {
    DOCUMENT_OPENER.with(|slot| *slot.borrow_mut() = None);
}

/// Ask the host to open `path`. `None` when no host installed a route.
fn ask_host_to_open(path: &std::path::Path) -> Option<Result<(), String>> {
    DOCUMENT_OPENER.with(|slot| slot.borrow_mut().as_mut().map(|open| open(path)))
}

impl FileExplorerWidget {
    pub fn open_terminal<R: CommandRunner>(&self, runner: &mut R) -> std::io::Result<()> {
        runner.spawn("terminal", &self.current_path)
    }

    /// Activate entry `idx` (by displayed index): navigate into a directory
    /// or launch the associated app.
    pub fn open_entry<R: CommandRunner>(&mut self, idx: usize, runner: &mut R) -> std::io::Result<bool> {
        let disp = self.displayed_entries();
        let Some(entry) = disp.get(idx) else { return Ok(false) };
        let path = entry.path.clone();
        self.open_target(path, runner)
    }

    /// Build + open the right-click menu at `at`, with items appropriate to
    /// the current selection and clipboard.
    /// The template files currently available for the "New File ▸" submenu.
    pub fn available_templates(&self) -> Vec<PathBuf> {
        templates::templates_dir()
            .map(|d| templates::list_templates(&d))
            .unwrap_or_default()
    }

    /// The menu item specs for the current selection / clipboard / state, given
    /// the available `templates`. Returns `(label, command)` pairs; a `None`
    /// command with an empty label is a separator. Pure (templates injected) so
    /// the menu contents — including the "New File ▸" gating — are unit-testable.
    fn context_menu_spec(&self, templates: &[PathBuf]) -> Vec<(String, Option<FileCommand>)> {
        let has_sel = !self.selection.is_empty();
        let can_paste = !self.clipboard.is_empty();
        let has_templates = !templates.is_empty();
        let mut spec: Vec<(String, Option<FileCommand>)> = Vec::new();

        if has_sel {
            spec.push(("Open".into(), self.selection.primary().map(FileCommand::Activate)));
            spec.push(("Rename".into(), Some(FileCommand::Rename)));
            spec.push((String::new(), None));
            spec.push(("Cut".into(), Some(FileCommand::Cut)));
            spec.push(("Copy".into(), Some(FileCommand::Copy)));
        }
        if can_paste {
            spec.push(("Paste".into(), Some(FileCommand::Paste)));
        }
        spec.push((String::new(), None));
        spec.push(("New Folder".into(), Some(FileCommand::NewFolder)));
        // Only offer "New File ▸" when the Templates dir actually has files.
        if has_templates {
            spec.push(("New File  \u{203A}".into(), Some(FileCommand::OpenTemplatesMenu)));
        }
        if has_sel {
            spec.push((String::new(), None));
            spec.push(("Move to Trash".into(), Some(FileCommand::Delete)));
        }

        // Favourites: add the single selected folder, or remove the current
        // directory when it is a (non-home) favourite.
        if let Some(dir) = self.single_selected_dir() {
            spec.push((String::new(), None));
            spec.push(("Add to Favourites".into(), Some(FileCommand::AddFavorite(dir))));
        } else if self.current_is_removable_favourite() {
            spec.push((String::new(), None));
            spec.push((
                "Remove from Favourites".into(),
                Some(FileCommand::RemoveFavorite(self.current_path.clone())),
            ));
        }

        // View section: show/hide hidden files.
        spec.push((String::new(), None));
        spec.push((
            if self.show_hidden { "Hide Hidden Files".into() } else { "Show Hidden Files".into() },
            Some(FileCommand::ToggleHidden),
        ));

        // Sort section — the only place the sort *key* can be changed in grid
        // mode (which has no clickable column header) and the only way to
        // reach "Sort by Type" at all.
        spec.push((String::new(), None));
        spec.push(("Sort by Name".into(), Some(FileCommand::SetSort(SortBy::Name))));
        spec.push(("Sort by Size".into(), Some(FileCommand::SetSort(SortBy::Size))));
        spec.push(("Sort by Type".into(), Some(FileCommand::SetSort(SortBy::Type))));
        spec.push(("Sort by Date".into(), Some(FileCommand::SetSort(SortBy::Modified))));
        spec.push((String::new(), None));
        spec.push(("Open Terminal Here".into(), Some(FileCommand::OpenTerminal)));
        spec
    }

    /// The path of the single selected entry when it is a directory (drives the
    /// "Add to Favourites" action).
    fn single_selected_dir(&self) -> Option<PathBuf> {
        let disp = self.displayed_entries();
        let sel = self.selection.selected();
        if sel.len() != 1 {
            return None;
        }
        disp.get(sel[0]).filter(|e| e.is_dir).map(|e| e.path.clone())
    }

    /// Whether the current directory is a favourite that may be removed (i.e.
    /// present in the list and not the pinned Home entry).
    fn current_is_removable_favourite(&self) -> bool {
        self.favs
            .borrow()
            .favorites
            .iter()
            .any(|f| !f.is_home && f.path == self.current_path)
    }

    fn open_context_menu(&mut self, at: Point) {
        self.submenu.close();
        let spec = self.context_menu_spec(&self.available_templates());
        let mut menu = ContextMenu::new();
        let mut cmds = Vec::with_capacity(spec.len());
        for (label, cmd) in spec {
            if cmd.is_none() && label.is_empty() {
                menu.push(MenuItem::separator());
                cmds.push(None);
            } else {
                // No-op action — the app maps the clicked index to a command
                // itself (the sink is !Send, so it can't live in the action).
                menu.push(MenuItem::action(label, || {}));
                cmds.push(cmd);
            }
        }
        menu.open_at([at.x as f32, at.y as f32]);
        self.context_menu = menu;
        self.menu_commands = cmds;
        layout_menu_widget(&mut self.context_menu);
    }

    /// Open the "New File" flyout listing one "New <name>" entry per template.
    fn open_templates_submenu(&mut self, at: Point) {
        let tpls = self.available_templates();
        let mut menu = ContextMenu::new();
        let mut cmds = Vec::with_capacity(tpls.len());
        for t in tpls {
            menu.push(MenuItem::action(templates::menu_label(&t), || {}));
            cmds.push(Some(FileCommand::NewFromTemplate(t)));
        }
        menu.open_at([at.x as f32, at.y as f32]);
        self.submenu = menu;
        self.submenu_commands = cmds;
        layout_menu_widget(&mut self.submenu);
    }

    /// Open `self.context_menu` from a `(label, command)` spec, right-aligned so
    /// its right edge sits at `anchor` (a toolbar control's bottom-right). Used
    /// by the labelled View / Sort menus, whose triggers hug the window's right
    /// edge where a left-anchored menu would spill off-screen. A `None` command
    /// with an empty label is a separator.
    fn open_menu_right_aligned(
        &mut self,
        spec: Vec<(String, Option<FileCommand>)>,
        anchor: Point,
    ) {
        self.submenu.close();
        let mut menu = ContextMenu::new();
        let mut cmds = Vec::with_capacity(spec.len());
        for (label, cmd) in spec {
            if cmd.is_none() && label.is_empty() {
                menu.push(MenuItem::separator());
                cmds.push(None);
            } else {
                menu.push(MenuItem::action(label, || {}));
                cmds.push(cmd);
            }
        }
        // Right-align: lay out once to learn the menu width, then re-anchor so
        // the right edge lands on `anchor.x`, clamped to the window's left edge.
        menu.open_at([anchor.x as f32, anchor.y as f32]);
        layout_menu_widget(&mut menu);
        let w = menu.get_rect().width();
        let x = (anchor.x - w).max(0.0);
        menu.open_at([x as f32, anchor.y as f32]);
        self.context_menu = menu;
        self.menu_commands = cmds;
        layout_menu_widget(&mut self.context_menu);
    }

    /// The labelled "View options" overflow menu (the ⋯ button): new folder /
    /// file, a clearly-labelled show-hidden toggle with a check, and a terminal.
    /// Everything here is spelled out in words — no mystery symbolic controls.
    fn open_view_menu(&mut self) {
        let anchor = self.toolbar.view_menu_anchor();
        let mut spec: Vec<(String, Option<FileCommand>)> = Vec::new();
        spec.push(("New Folder".into(), Some(FileCommand::NewFolder)));
        if !self.available_templates().is_empty() {
            spec.push(("New File  \u{203A}".into(), Some(FileCommand::OpenTemplatesMenu)));
        }
        spec.push((String::new(), None));
        spec.push((
            if self.show_hidden {
                "\u{2713} Show Hidden Files".into()
            } else {
                "Show Hidden Files".into()
            },
            Some(FileCommand::ToggleHidden),
        ));
        spec.push((String::new(), None));
        spec.push(("Open Terminal Here".into(), Some(FileCommand::OpenTerminal)));
        self.open_menu_right_aligned(spec, anchor);
    }

    /// The labelled "Sort by" menu (the grid-mode Sort button): the sort key,
    /// then the direction, each with a check on the active choice. This is the
    /// grid's equivalent of clicking a list column header.
    fn open_sort_menu(&mut self) {
        let anchor = self.toolbar.sort_menu_anchor();
        let check = |on: bool| if on { "\u{2713} " } else { "     " };
        let mut spec: Vec<(String, Option<FileCommand>)> = Vec::new();
        for (label, by) in [
            ("Name", SortBy::Name),
            ("Size", SortBy::Size),
            ("Type", SortBy::Type),
            ("Date", SortBy::Modified),
        ] {
            spec.push((
                format!("{}{label}", check(self.sort_by == by)),
                Some(FileCommand::SetSort(by)),
            ));
        }
        spec.push((String::new(), None));
        let asc = self.sort_order == SortOrder::Ascending;
        spec.push((
            format!("{}Ascending", check(asc)),
            Some(FileCommand::SetSortOrder(SortOrder::Ascending)),
        ));
        spec.push((
            format!("{}Descending", check(!asc)),
            Some(FileCommand::SetSortOrder(SortOrder::Descending)),
        ));
        self.open_menu_right_aligned(spec, anchor);
    }

    /// The right-click menu for a sidebar favourite `path`: open it, plus the
    /// standard "Remove from Favourites" (suppressed for the pinned Home entry,
    /// which cannot be removed). Anchored at the cursor like any context menu.
    fn open_favorite_menu(&mut self, path: PathBuf, at: Point) {
        let is_home = self
            .favs
            .borrow()
            .favorites
            .iter()
            .any(|f| f.is_home && f.path == path);
        let mut spec: Vec<(String, Option<FileCommand>)> = Vec::new();
        spec.push(("Open".into(), Some(FileCommand::Navigate(path.clone()))));
        if !is_home {
            spec.push((String::new(), None));
            spec.push((
                "Remove from Favourites".into(),
                Some(FileCommand::RemoveFavorite(path)),
            ));
        }
        // A favourite menu opens at the cursor (left-anchored is fine — the
        // sidebar sits at the window's left edge).
        self.submenu.close();
        let mut menu = ContextMenu::new();
        let mut cmds = Vec::with_capacity(spec.len());
        for (label, cmd) in spec {
            if cmd.is_none() && label.is_empty() {
                menu.push(MenuItem::separator());
                cmds.push(None);
            } else {
                menu.push(MenuItem::action(label, || {}));
                cmds.push(cmd);
            }
        }
        menu.open_at([at.x as f32, at.y as f32]);
        self.context_menu = menu;
        self.menu_commands = cmds;
        layout_menu_widget(&mut self.context_menu);
    }

    /// If either menu is open, route a left-press to it. Returns true when the
    /// press was consumed (item fired, submenu opened, or a menu dismissed).
    fn route_menu_click(&mut self, p: Point) -> bool {
        // The submenu takes priority while it is up.
        if self.submenu.is_open() {
            if let Some(idx) = self.submenu.item_at([p.x, p.y]) {
                let cmd = self.submenu_commands.get(idx).cloned().flatten();
                self.submenu.close();
                self.context_menu.close();
                if let Some(cmd) = cmd {
                    self.apply_command(cmd);
                }
                return true;
            }
            // A click outside the submenu closes both menus.
            self.submenu.close();
            self.context_menu.close();
            return true;
        }
        if !self.context_menu.is_open() {
            return false;
        }
        if let Some(idx) = self.context_menu.item_at([p.x, p.y]) {
            let cmd = self.menu_commands.get(idx).cloned().flatten();
            if let Some(FileCommand::OpenTemplatesMenu) = cmd {
                // Open the flyout to the right of the clicked row; keep the
                // main menu open behind it.
                let anchor = Point::new(self.context_menu.get_rect().x1 - 6.0, p.y - 10.0);
                self.open_templates_submenu(anchor);
                return true;
            }
            self.context_menu.close();
            if let Some(cmd) = cmd {
                self.apply_command(cmd);
            }
            return true;
        }
        // Click outside the open menu dismisses it.
        self.context_menu.close();
        true
    }

    /// Drain and apply all queued commands. Returns whether any command
    /// changed the structure.
    fn drain_commands(&mut self) -> bool {
        let queued: Vec<FileCommand> = self.cmds.borrow_mut().drain(..).collect();
        let mut dirty = false;
        for cmd in queued {
            dirty |= self.apply_command(cmd);
        }
        dirty
    }

    /// Forward an event to the three body children in visual order — chrome
    /// above, the responsive `nav` body, chrome below — stopping at the first
    /// that consumes it. `node_id`'s taffy children are `[above, nav, below]`
    /// (see `build_layout`).
    #[allow(clippy::too_many_arguments)]
    fn forward_to_body(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        handled: &mut bool,
        focus_target: &mut Option<taffy::NodeId>,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) {
        let Ok(children) = taffy.children(node_id) else { return };
        let (Some(&above_n), Some(&nav_n), Some(&below_n)) =
            (children.first(), children.get(1), children.get(2))
        else {
            return;
        };
        if !*handled {
            self.toolbar.handle_event(
                event, scale_factor, modifiers, state, path, handled, focus_target, above_n, taffy,
            );
        }
        if !*handled {
            self.nav.handle_event(
                event, scale_factor, modifiers, state, path, handled, focus_target, nav_n, taffy,
            );
        }
        if !*handled {
            if let Some(c) = &mut self.chrome_below {
                c.handle_event(
                    event, scale_factor, modifiers, state, path, handled, focus_target, below_n,
                    taffy,
                );
            }
        }
    }

    /// App-level keyboard navigation, applied when a key isn't consumed by
    /// a focused child (search field, etc.). Returns whether it acted.
    fn handle_app_key(&mut self, ke: &winit::event::KeyEvent, modifiers: &winit::event::Modifiers) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if ke.state != winit::event::ElementState::Pressed {
            return false;
        }
        // While renaming, the rename field owns the keyboard (Enter/Escape are
        // handled upstream in `handle_event`); don't let list nav / Delete /
        // clipboard shortcuts fire underneath the editor.
        if self.renaming.is_some() {
            return false;
        }
        let shift = modifiers.state().shift_key();
        let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
        let len = self.displayed_entries().len();
        match &ke.logical_key {
            // F2 renames the current selection in place.
            Key::Named(NamedKey::F2) => {
                self.begin_rename();
                true
            }
            Key::Named(NamedKey::ArrowDown) if len > 0 => {
                let next = self.selection.primary().map(|i| (i + 1).min(len - 1)).unwrap_or(0);
                self.selection.move_to(next, shift);
                true
            }
            Key::Named(NamedKey::ArrowUp) if len > 0 => {
                let next = self.selection.primary().map(|i| i.saturating_sub(1)).unwrap_or(0);
                self.selection.move_to(next, shift);
                true
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(p) = self.primary_path() {
                    self.open_path(p);
                }
                true
            }
            Key::Named(NamedKey::Backspace) => {
                self.go_up();
                true
            }
            Key::Named(NamedKey::Delete) => {
                self.apply_command(FileCommand::Delete);
                true
            }
            Key::Character(s) if (s == "a" || s == "A") && ctrl => {
                self.selection.select_all(len);
                true
            }
            Key::Character(s) if (s == "c" || s == "C") && ctrl => {
                self.apply_command(FileCommand::Copy);
                true
            }
            Key::Character(s) if (s == "x" || s == "X") && ctrl => {
                self.apply_command(FileCommand::Cut);
                true
            }
            Key::Character(s) if (s == "v" || s == "V") && ctrl => {
                self.apply_command(FileCommand::Paste);
                true
            }
            // Ctrl + / Ctrl - zoom the icon/row size (both "+" and "=" cover
            // keyboards where "+" is the shifted "=").
            Key::Character(s) if ctrl && (s == "+" || s == "=") => {
                self.set_zoom(self.zoom_level + 1);
                true
            }
            Key::Character(s) if ctrl && (s == "-" || s == "_") => {
                self.set_zoom(self.zoom_level - 1);
                true
            }
            _ => false,
        }
    }
}

impl Widget for FileExplorerWidget {
    fn sync(&mut self, state: &mut automerge::AutoCommit, path: &automerge::ObjId) {
        use automerge::ReadDoc;
        // Seed the inline-rename field's backing Text object *before* the
        // children sync so the rename input resolves a real object to write
        // into (the toolkit's TextInput binds to an existing Text but never
        // lazily creates one).
        let rename_obj = ensure_text_obj(state, path, RENAME_QUERY_PATH);

        // A just-started rename seeds the field with the entry's current name.
        if self.rename_seed_pending {
            ui::doc::splice_text_diffed(state, &rename_obj, &self.rename_buffer);
            self.rename_seed_pending = false;
        }

        // Keep the persistent in-place rename editor reading from the seeded
        // object; select the whole name on the first sync after a rename begins
        // so the first keystroke replaces it (Finder-style).
        self.rename_input.sync(state, path);
        if self.rename_select_pending {
            self.rename_input.select_all();
            self.rename_select_pending = false;
        }

        self.toolbar.sync(state, path);
        self.nav.sync(state, path);
        if let Some(c) = &mut self.chrome_below {
            c.sync(state, path);
        }

        // While renaming, mirror the field's live text into our buffer so a
        // commit renames to what the user actually typed.
        if self.renaming.is_some() {
            if let Ok(text) = state.text(&rename_obj) {
                self.rename_buffer = text;
            }
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        // Refresh the persistent top bar, then rebuild the footer + swap the
        // freshly-built body panes into the persistent `nav`. `nav` and the
        // `toolbar` keep their state across the swap (docked/push state; the
        // path bar's in-progress edit), so a rebuild triggered by navigation /
        // sort doesn't snap the body or the path bar back mid-interaction.
        self.update_toolbar();
        let mut below = self.build_chrome_below(theme);
        self.nav.set_sidebar(Some(self.build_places_pane()));
        self.nav.set_content(self.build_content_pane(theme));

        let above_node = self.toolbar.build_layout(taffy, theme, node_map);
        let nav_node = self.nav.build_layout(taffy, theme, node_map);
        let below_node = below.build_layout(taffy, theme, node_map);
        self.chrome_below = Some(below);

        // The body fills the height left between the intrinsic-height chrome
        // bands (which `build_*_chrome` size with `with_intrinsic_height`).
        // `NavShell::build_layout` hard-codes a `percent(1.0)` height leaf; in
        // a column of three that would claim the full height, so override it to
        // grow into the remaining space instead.
        taffy
            .set_style(
                nav_node,
                taffy::Style {
                    flex_grow: 1.0,
                    flex_shrink: 1.0,
                    flex_basis: taffy::Dimension::length(0.0),
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::auto(),
                    },
                    min_size: taffy::Size {
                        width: taffy::Dimension::auto(),
                        height: taffy::Dimension::length(0.0),
                    },
                    ..Default::default()
                },
            )
            .unwrap();

        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        };
        let node = taffy
            .new_with_children(style, &[above_node, nav_node, below_node])
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        let l = taffy.layout(node).unwrap();
        self.rect = vello::kurbo::Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        let base = vello::kurbo::Point::new(self.rect.x0, self.rect.y0);
        if let Ok(children) = taffy.children(node) {
            if let (Some(&above_n), Some(&nav_n), Some(&below_n)) =
                (children.first(), children.get(1), children.get(2))
            {
                self.toolbar.apply_layout(taffy, above_n, base);
                self.nav.apply_layout(taffy, nav_n, base);
                if let Some(c) = &mut self.chrome_below {
                    c.apply_layout(taffy, below_n, base);
                }
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.paint_background(scene, transform, theme);
        self.toolbar.draw(scene, transform, text_renderer, theme, presence);
        self.nav.draw(scene, transform, text_renderer, theme, presence);
        if let Some(c) = &mut self.chrome_below {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
        self.draw_rename_editor(scene, transform, text_renderer, theme, presence);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
        permissions: &ui::permissions::PermissionContext,
    ) {
        self.paint_background(scene, transform, theme);
        // Recurse with `draw_with_presence` (not the plain `draw`) so the
        // inner `Scrollable` pushes its viewport rect and the file list culls
        // rows outside it. The default trait impl would call our own `draw`,
        // which breaks that chain — a long directory would then emit every
        // row's glyphs every frame regardless of what's visible.
        self.toolbar
            .draw_with_presence(scene, transform, text_renderer, theme, presence, permissions);
        self.nav
            .draw_with_presence(scene, transform, text_renderer, theme, presence, permissions);
        if let Some(c) = &mut self.chrome_below {
            c.draw_with_presence(scene, transform, text_renderer, theme, presence, permissions);
        }
        self.draw_rename_editor(scene, transform, text_renderer, theme, presence);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        // Child overlays (the path bar's completion popover, dropdowns) first,
        // then our top-layer context menu (and its flyout) so they composite
        // above everything.
        self.toolbar.draw_overlay(scene, transform, text_renderer, theme, presence);
        self.nav.draw_overlay(scene, transform, text_renderer, theme, presence);
        if let Some(c) = &mut self.chrome_below {
            c.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        if self.context_menu.is_open() {
            layout_menu_widget(&mut self.context_menu);
            self.context_menu.draw(scene, transform, text_renderer, theme, presence);
        }
        if self.submenu.is_open() {
            layout_menu_widget(&mut self.submenu);
            self.submenu.draw(scene, transform, text_renderer, theme, presence);
        }
        // The drag-to-pin ghost floats above everything else.
        self.draw_pin_ghost(scene, transform, text_renderer, theme);
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        handled: &mut bool,
        focus_target: &mut Option<taffy::NodeId>,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        // Track the cursor for menu placement + hit-testing.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
        }

        // A live drag-to-pin owns the pointer: the ghost follows the cursor and
        // a release over the sidebar pins the folder to Favourites.
        if self.pin_drag.is_some() {
            match event {
                WindowEvent::CursorMoved { .. } => {
                    *handled = true;
                    self.structure_dirty = true;
                    return;
                }
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: MouseButton::Left,
                    ..
                } => {
                    let over_sidebar = self.nav.is_sidebar_open()
                        && self.nav.sidebar_rect().contains(self.last_cursor);
                    if let Some(path) = self.pin_drag.take() {
                        if over_sidebar {
                            self.apply_command(FileCommand::AddFavorite(path));
                        }
                    }
                    *handled = true;
                    self.structure_dirty = true;
                    return;
                }
                _ => {}
            }
        }

        // The open context menu (or its flyout) gets first dibs on clicks + Escape.
        if self.context_menu.is_open() || self.submenu.is_open() {
            match event {
                WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                    if self.route_menu_click(self.last_cursor) {
                        *handled = true;
                        self.structure_dirty = true;
                        let _ = self.drain_commands();
                        return;
                    }
                }
                WindowEvent::KeyboardInput { event: ke, .. }
                    if ke.state == ElementState::Pressed
                        && matches!(ke.logical_key, winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)) =>
                {
                    self.submenu.close();
                    self.context_menu.close();
                    *handled = true;
                    self.structure_dirty = true;
                    return;
                }
                _ => {}
            }
        }

        // Right-click opens the context menu. First deliver the press to the
        // body so the content pane selects the row under the cursor (it pushes
        // `ContextSelect`); draining that before building the menu is what makes
        // the menu's actions target what was actually clicked rather than a
        // stale selection from an earlier click.
        if let WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } = event {
            let mut child_handled = false;
            self.forward_to_body(
                event, scale_factor, modifiers, state, path, &mut child_handled, focus_target,
                node_id, taffy,
            );
            self.drain_commands();
            // A right-press over a sidebar favourite pushes `OpenFavoriteMenu`,
            // which the drain records here; open the favourite's own menu at the
            // cursor rather than the file-view context menu.
            if let Some(fav) = self.pending_fav_menu.take() {
                self.open_favorite_menu(fav, self.last_cursor);
            } else {
                self.open_context_menu(self.last_cursor);
            }
            *handled = true;
            self.structure_dirty = true;
            return;
        }

        // Ctrl + mouse-wheel zooms the icon/row size (before the wheel reaches
        // the Scrollable, which would otherwise consume it for scrolling).
        if let WindowEvent::MouseWheel { delta, .. } = event {
            if modifiers.state().control_key() || modifiers.state().super_key() {
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => *y as f64,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y,
                };
                if dy > 0.0 {
                    self.set_zoom(self.zoom_level + 1);
                } else if dy < 0.0 {
                    self.set_zoom(self.zoom_level - 1);
                }
                *handled = true;
                self.structure_dirty = true;
                return;
            }
        }

        // While an inline rename is active the in-place editor (drawn over the
        // row, not in a bottom bar) owns input: Enter commits, Escape cancels,
        // every other keystroke edits the field, and a press outside the field
        // commits. Handled before the event reaches app-level nav / the file
        // view so typing never leaks into list navigation or Enter-opens a row.
        if self.renaming.is_some() {
            if let Some(rect) = self.rename_rect.get() {
                self.rename_input.apply_layout_external(rect);
            }
            match event {
                WindowEvent::KeyboardInput { event: ke, .. } => {
                    use winit::keyboard::{Key, NamedKey};
                    if ke.state == ElementState::Pressed {
                        match &ke.logical_key {
                            Key::Named(NamedKey::Enter) => {
                                // Pull the freshest field text from the CRDT, then
                                // commit — the last keystroke may not have synced.
                                let rename_obj = ensure_text_obj(state, path, RENAME_QUERY_PATH);
                                if let Ok(text) = {
                                    use automerge::ReadDoc;
                                    state.text(&rename_obj)
                                } {
                                    self.rename_buffer = text;
                                }
                                self.commit_rename();
                                *handled = true;
                                self.structure_dirty = true;
                                return;
                            }
                            Key::Named(NamedKey::Escape) => {
                                self.cancel_rename();
                                *handled = true;
                                self.structure_dirty = true;
                                return;
                            }
                            _ => {}
                        }
                    }
                    // Typing / editing keys go to the field (it splices the CRDT).
                    self.rename_input.handle_event(
                        event, scale_factor, modifiers, state, path, handled, focus_target,
                        node_id, taffy,
                    );
                    *handled = true;
                    self.structure_dirty = true;
                    return;
                }
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                } => {
                    let inside = self
                        .rename_rect
                        .get()
                        .map(|r| r.contains(self.last_cursor))
                        .unwrap_or(false);
                    if inside {
                        // Reposition the caret within the field.
                        self.rename_input.handle_event(
                            event, scale_factor, modifiers, state, path, handled, focus_target,
                            node_id, taffy,
                        );
                    } else {
                        // A click away commits the rename (then this click is
                        // consumed; a second click acts on its target).
                        self.commit_rename();
                    }
                    *handled = true;
                    self.structure_dirty = true;
                    return;
                }
                _ => {}
            }
        }

        // Remember where we are so a place-navigation this event can push the
        // file view in (narrow / push layout).
        let path_before = self.current_path.clone();

        // Deliver to the body first (toolbar/breadcrumb, places list, file
        // view), stopping at the first that consumes it.
        self.forward_to_body(
            event, scale_factor, modifiers, state, path, handled, focus_target, node_id, taffy,
        );

        // App-level keyboard nav for keys the children didn't consume.
        if !*handled {
            if let winit::event::WindowEvent::KeyboardInput { event: ke, .. } = event {
                if self.handle_app_key(ke, modifiers) {
                    *handled = true;
                    self.structure_dirty = true;
                }
            }
        }

        // Apply any commands the children queued during this event.
        if self.drain_commands() {
            self.structure_dirty = true;
        }

        // Picking a destination dismisses the narrow drawer so the file list
        // it navigated to is what the user is left looking at — the Adwaita
        // overlay-sidebar behaviour. Wide keeps the places list docked, so
        // there is nothing to dismiss there.
        if self.current_path != path_before && self.nav.mode() == ShellMode::Narrow {
            self.nav.close_sidebar();
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut d = std::mem::take(&mut self.structure_dirty);
        d |= self.toolbar.take_structure_dirty();
        d |= self.nav.take_structure_dirty();
        if let Some(c) = &mut self.chrome_below {
            d |= c.take_structure_dirty();
        }
        d
    }

    fn needs_animation_frame(&self) -> bool {
        // The body's docked→push / reveal slide must keep the host pumping
        // frames while it settles.
        self.nav.needs_animation_frame()
            || self.toolbar.needs_animation_frame()
            || self
                .chrome_below
                .as_ref()
                .map(|c| c.needs_animation_frame())
                .unwrap_or(false)
    }

    fn needs_idle_sync(&self) -> bool {
        self.nav.needs_idle_sync()
            || self.toolbar.needs_idle_sync()
            || self
                .chrome_below
                .as_ref()
                .map(|c| c.needs_idle_sync())
                .unwrap_or(false)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut v: Vec<&dyn Widget> = Vec::new();
        v.push(&self.toolbar);
        v.push(&self.nav);
        if let Some(c) = &self.chrome_below {
            v.push(c.as_ref());
        }
        v
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut v: Vec<&mut (dyn Widget + '_)> = Vec::new();
        v.push(&mut self.toolbar);
        v.push(&mut self.nav);
        if let Some(c) = &mut self.chrome_below {
            v.push(c.as_mut());
        }
        v
    }

    fn name(&self) -> &str {
        "FileExplorerWidget"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }
}

impl AppWidget for FileExplorerWidget {
    fn app_kind(&self) -> AppKind {
        AppKind::Files
    }
    fn app_title(&self) -> &str {
        "Files"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("󰉋")
    }
    fn permission_hint(&self) -> Option<AppKind> {
        Some(AppKind::Files)
    }
}

#[cfg(test)]
mod open_with_tests {
    use super::*;
    use crate::model::Entry;
    use std::path::{Path, PathBuf};

    #[derive(Default)]
    struct RecRunner {
        calls: Vec<(String, Option<PathBuf>)>,
        /// Paths handed to the system default handler (non-document opens).
        opened: Vec<PathBuf>,
    }
    impl CommandRunner for RecRunner {
        fn spawn(&mut self, program: &str, _cwd: &Path) -> std::io::Result<()> {
            self.calls.push((program.to_string(), None));
            Ok(())
        }
        fn spawn_arg(&mut self, program: &str, arg: &Path, _cwd: &Path) -> std::io::Result<()> {
            self.calls.push((program.to_string(), Some(arg.to_path_buf())));
            Ok(())
        }
        fn open_default(&mut self, path: &Path) -> std::io::Result<()> {
            self.opened.push(path.to_path_buf());
            Ok(())
        }
    }

    fn file_entry(name: &str) -> Entry {
        Entry { name: name.into(), path: PathBuf::from("/docs").join(name), is_dir: false, size: 0, modified: None, mime: String::new(), sync_state: None }
    }

    #[test]
    fn document_extensions_are_recognised_as_documents() {
        for f in ["report.odt", "memo.docx", "notes.md", "readme.markdown"] {
            assert!(is_document(Path::new(f)), "{f}");
        }
        assert!(!is_document(Path::new("photo.png")));
        assert!(!is_document(Path::new("main.rs")));
    }

    /// Every activated file goes to the host, whatever its type. The
    /// explorer must not sort files into "documents" and "everything
    /// else" — which app owns a `.pdf` or a `.png` is the desktop's
    /// answer, from what is installed, and a table here would be wrong
    /// the moment anybody installs anything.
    #[test]
    fn activating_a_file_hands_the_path_to_the_host() {
        let asked: std::rc::Rc<std::cell::RefCell<Vec<PathBuf>>> = Default::default();
        let sink = asked.clone();
        set_document_opener(move |p| {
            sink.borrow_mut().push(p.to_path_buf());
            Ok(())
        });

        let mut w = FileExplorerWidget::new();
        w.entries = ["report.odt", "photo.png", "slides.pdf", "main.rs"]
            .iter()
            .map(|f| file_entry(f))
            .collect();
        let mut runner = RecRunner::default();
        for i in 0..4 {
            assert!(w.open_entry(i, &mut runner).unwrap(), "entry {i} opens");
        }

        assert_eq!(
            *asked.borrow(),
            vec![
                PathBuf::from("/docs/report.odt"),
                PathBuf::from("/docs/photo.png"),
                PathBuf::from("/docs/slides.pdf"),
                PathBuf::from("/docs/main.rs"),
            ],
            "the host is asked for every file, with its own path"
        );
        assert!(
            runner.calls.is_empty(),
            "the explorer launches nothing itself: {:?}",
            runner.calls
        );
        assert!(
            runner.opened.is_empty(),
            "and does not go behind the host's back to xdg-open"
        );
        assert_eq!(w.status, None, "a successful open reports nothing");
        clear_document_opener();
    }

    /// A host that cannot open the file says why, and the explorer shows
    /// it. Activation is never silent.
    #[test]
    fn a_refused_open_is_shown_to_the_user() {
        set_document_opener(|_| Err("no installed app opens .zzz files (thing.zzz)".into()));
        let mut w = FileExplorerWidget::new();
        w.entries = vec![file_entry("thing.zzz")];
        let mut runner = RecRunner::default();
        assert!(w.open_entry(0, &mut runner).unwrap());
        let status = w.status.clone().expect("the refusal reaches the status bar");
        assert!(status.contains("thing.zzz"), "must name the file: {status}");
        assert!(runner.opened.is_empty(), "a refusal is not a fallback");
        clear_document_opener();
    }

    /// With no host installed — the explorer run outside a qOS session —
    /// activation still opens the file, via the desktop's own handler,
    /// rather than doing nothing.
    #[test]
    fn without_a_host_every_type_falls_back_to_the_desktop_handler() {
        clear_document_opener();
        for f in ["pic.jpg", "movie.mp4", "song.flac", "doc.pdf", "main.rs", "report.odt"] {
            let mut w = FileExplorerWidget::new();
            w.entries = vec![file_entry(f)];
            let mut runner = RecRunner::default();
            assert!(w.open_entry(0, &mut runner).unwrap(), "{f} should open");
            assert!(runner.calls.is_empty(), "{f} must not spawn a bare binary");
            assert_eq!(runner.opened, vec![PathBuf::from("/docs").join(f)], "{f}");
        }
    }

    #[test]
    fn context_menu_opens_with_selection_aware_items() {
        let mut w = FileExplorerWidget::new();
        w.entries = vec![file_entry("a.txt"), file_entry("b.txt")];
        w.selection.select_only(0);
        w.open_context_menu(Point::new(40.0, 40.0));
        assert!(w.context_menu.is_open());
        let labels: Vec<String> = w
            .context_menu
            .items()
            .iter()
            .filter_map(|i| i.label().map(|s| s.to_string()))
            .collect();
        // Selection-aware actions are present.
        for expected in ["Open", "Rename", "Cut", "Copy", "Move to Trash", "New Folder"] {
            assert!(labels.contains(&expected.to_string()), "menu missing {expected}: {labels:?}");
        }
        // No clipboard yet → no Paste.
        assert!(!labels.contains(&"Paste".to_string()));
    }

    #[test]
    fn empty_selection_menu_omits_item_actions() {
        let mut w = FileExplorerWidget::new();
        w.entries = vec![file_entry("a.txt")];
        // No selection.
        w.open_context_menu(Point::new(10.0, 10.0));
        let labels: Vec<String> = w
            .context_menu
            .items()
            .iter()
            .filter_map(|i| i.label().map(|s| s.to_string()))
            .collect();
        assert!(!labels.contains(&"Move to Trash".to_string()));
        assert!(labels.contains(&"New Folder".to_string()));
        assert!(labels.contains(&"Open Terminal Here".to_string()));
    }

    #[test]
    fn copy_then_paste_menu_includes_paste() {
        let mut w = FileExplorerWidget::new();
        w.entries = vec![file_entry("a.txt")];
        w.selection.select_only(0);
        // Simulate a copy populating the clipboard.
        w.apply_command(crate::command::FileCommand::Copy);
        assert_eq!(w.clipboard.len(), 1);
        w.open_context_menu(Point::new(10.0, 10.0));
        let labels: Vec<String> = w
            .context_menu
            .items()
            .iter()
            .filter_map(|i| i.label().map(|s| s.to_string()))
            .collect();
        assert!(labels.contains(&"Paste".to_string()));
    }
}

/// Tests for the real command-application paths — the things a user actually
/// triggers from the toolbar / context menu / keyboard, exercised through
/// `apply_command`, `sync`, and the fs mutations (never through a tautological
/// helper). These are the paths the review flagged as untested.
#[cfg(test)]
mod command_path_tests {
    use super::*;
    use crate::model::Entry;
    use std::path::PathBuf;

    /// In-memory entry with a controllable name/mime (no fs touch).
    fn ent(name: &str, is_dir: bool, mime: &str) -> Entry {
        Entry {
            name: name.into(),
            path: PathBuf::from(name),
            is_dir,
            size: 0,
            modified: None,
            mime: mime.into(),
            sync_state: None,
        }
    }

    #[test]
    fn set_sort_command_selects_the_key_including_type() {
        let mut w = FileExplorerWidget::new();
        w.entries = vec![
            ent("b.png", false, "image/png"),
            ent("a.txt", false, "text/plain"),
        ];
        // SortBy::Type is ONLY reachable via this command (no column/button),
        // so exercising it proves the previously-unreachable key now works.
        assert!(w.apply_command(FileCommand::SetSort(SortBy::Type)));
        assert_eq!(w.sort_by(), SortBy::Type);
        assert_eq!(w.entries_snapshot()[0].name, "b.png", "image/png < text/plain");

        assert!(w.apply_command(FileCommand::SetSort(SortBy::Name)));
        assert_eq!(w.sort_by(), SortBy::Name);
        assert_eq!(w.entries_snapshot()[0].name, "a.txt");
    }

    #[test]
    fn context_menu_exposes_the_sort_section_including_type() {
        let mut w = FileExplorerWidget::new();
        w.entries = vec![ent("a.txt", false, "text/plain")];
        w.open_context_menu(Point::new(5.0, 5.0));
        let labels: Vec<String> = w
            .context_menu
            .items()
            .iter()
            .filter_map(|i| i.label().map(str::to_string))
            .collect();
        for s in ["Sort by Name", "Sort by Size", "Sort by Type", "Sort by Date"] {
            assert!(labels.contains(&s.to_string()), "menu missing {s}: {labels:?}");
        }
    }

    #[test]
    fn error_state_composition_adds_exactly_the_retry_button() {
        use std::collections::HashMap;
        use taffy::{NodeId, TaffyTree};
        use ui::theme::Theme;

        // Build the real content pane (the file-view detail, where the retry
        // affordance lives) and count its buttons. The error flag is the only
        // structural difference, so the delta is precisely the "Try Again"
        // button `build_content_pane` adds.
        fn button_count(err: bool) -> usize {
            let mut w = FileExplorerWidget::new();
            if err {
                w.error = Some("could not read directory".into());
            }
            let theme = Theme::dark();
            let mut pane = w.build_content_pane(&theme);
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            pane.build_layout(&mut taffy, &theme, &mut nm);
            nm.values()
                .filter(|&&p| unsafe { (*p).name() } == "Button")
                .count()
        }
        assert_eq!(
            button_count(true),
            button_count(false) + 1,
            "error view must add the retry button"
        );
    }

    #[test]
    fn retry_refresh_recovers_from_error_once_the_dir_exists() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("appears-later");
        let mut w = FileExplorerWidget::new();
        w.navigate(target.clone());
        assert!(w.error_message().is_some(), "missing dir sets an error");
        // The "Try Again" button pushes Refresh — still errors while missing.
        assert!(w.apply_command(FileCommand::Refresh));
        assert!(w.error_message().is_some());
        // Once the directory exists, the same retry clears the error.
        std::fs::create_dir(&target).unwrap();
        assert!(w.apply_command(FileCommand::Refresh));
        assert!(w.error_message().is_none(), "retry succeeds once the dir exists");
    }

    #[test]
    fn rename_flow_renames_the_selected_file_on_disk() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("old.txt"), b"x").unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        let idx = w.displayed_entries().iter().position(|e| e.name == "old.txt").unwrap();
        w.select(Some(idx));

        // Begin the rename via the real command; the bar seeds the name.
        assert!(w.apply_command(FileCommand::Rename));
        assert!(w.is_renaming());
        assert_eq!(w.rename_buffer(), "old.txt");

        // Type a new name + commit through the command path.
        w.set_rename_buffer("new.txt".into());
        assert!(w.apply_command(FileCommand::RenameCommit));
        assert!(!w.is_renaming(), "commit closes the rename bar");
        assert!(d.path().join("new.txt").exists(), "file renamed on disk");
        assert!(!d.path().join("old.txt").exists());
    }

    #[test]
    fn reseeding_the_rename_field_splices_only_the_changed_span() {
        // Every rename re-seeds the *same* backing Text object with the new
        // entry's name. Clearing and refilling it tombstones the whole name
        // each time, so a session that renames a few dozen files pays for all
        // of them forever.
        let d = tempfile::tempdir().unwrap();
        for i in 0..40 {
            std::fs::write(d.path().join(format!("report-{i:03}.txt")), b"x").unwrap();
        }
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());

        use automerge::ReadDoc;
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        // First seed creates the object; measure growth from there.
        w.set_rename_buffer("report-000.txt".into());
        w.sync(&mut doc, &root);
        let baseline = doc.save().len();

        for i in 1..40 {
            w.set_rename_buffer(format!("report-{i:03}.txt"));
            w.sync(&mut doc, &root);
        }
        let grown = doc.save().len() - baseline;

        let obj = ensure_text_obj(&mut doc, &root, RENAME_QUERY_PATH);
        assert_eq!(
            doc.text(&obj).unwrap(),
            "report-039.txt",
            "the field must hold the last seeded name"
        );
        assert!(
            grown < 39 * "report-000.txt".len(),
            "39 rename re-seeds grew the document by {grown} bytes — the seed is \
             rewriting the whole field instead of splicing the diff"
        );
    }

    #[test]
    fn rename_commit_is_a_noop_for_blank_or_unchanged_names() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("keep.txt"), b"x").unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        let idx = w.displayed_entries().iter().position(|e| e.name == "keep.txt").unwrap();

        w.select(Some(idx));
        w.begin_rename();
        w.set_rename_buffer("   ".into());
        assert!(!w.commit_rename(), "blank name does not rename");
        assert!(!w.is_renaming());
        assert!(d.path().join("keep.txt").exists());

        w.select(Some(idx));
        w.begin_rename();
        w.set_rename_buffer("keep.txt".into());
        assert!(!w.commit_rename(), "unchanged name does not rename");
        assert!(d.path().join("keep.txt").exists());
    }

    #[test]
    fn rename_cancel_leaves_the_file_untouched() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("stay.txt"), b"x").unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        let idx = w.displayed_entries().iter().position(|e| e.name == "stay.txt").unwrap();
        w.select(Some(idx));
        w.apply_command(FileCommand::Rename);
        w.set_rename_buffer("other.txt".into());
        assert!(w.apply_command(FileCommand::RenameCancel));
        assert!(!w.is_renaming());
        assert!(d.path().join("stay.txt").exists());
        assert!(!d.path().join("other.txt").exists());
    }

    #[test]
    fn new_folder_creates_the_folder_and_opens_inline_rename() {
        let d = tempfile::tempdir().unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        assert!(w.apply_command(FileCommand::NewFolder));
        assert!(d.path().join("New Folder").is_dir(), "folder actually created");
        assert!(w.is_renaming(), "new folder drops straight into rename");
        assert_eq!(w.rename_buffer(), "New Folder");
    }

    #[test]
    fn copy_then_paste_duplicates_the_file_into_the_current_dir() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("a.txt"), b"hello").unwrap();
        let dest = d.path().join("dest");
        std::fs::create_dir(&dest).unwrap();

        let mut w = FileExplorerWidget::new();
        w.navigate(src.clone());
        let idx = w.displayed_entries().iter().position(|e| e.name == "a.txt").unwrap();
        w.select(Some(idx));
        assert!(w.apply_command(FileCommand::Copy));
        w.navigate(dest.clone());
        assert!(w.apply_command(FileCommand::Paste));
        assert!(dest.join("a.txt").exists(), "copied into dest");
        assert!(src.join("a.txt").exists(), "original kept (copy, not move)");
    }

    #[test]
    fn cut_then_paste_moves_the_file_into_the_current_dir() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("m.txt"), b"x").unwrap();
        let dest = d.path().join("dest");
        std::fs::create_dir(&dest).unwrap();

        let mut w = FileExplorerWidget::new();
        w.navigate(src.clone());
        let idx = w.displayed_entries().iter().position(|e| e.name == "m.txt").unwrap();
        w.select(Some(idx));
        assert!(w.apply_command(FileCommand::Cut));
        w.navigate(dest.clone());
        assert!(w.apply_command(FileCommand::Paste));
        assert!(dest.join("m.txt").exists(), "moved into dest");
        assert!(!src.join("m.txt").exists(), "original removed (move)");
    }

    #[test]
    fn history_back_and_forward_traverse_visited_dirs() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        std::fs::create_dir(&a).unwrap();
        let b = d.path().join("b");
        std::fs::create_dir(&b).unwrap();

        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        w.navigate(a.clone());
        w.navigate(b.clone());
        assert_eq!(w.current_path(), b);
        w.history_back();
        assert_eq!(w.current_path(), a, "back to the previously-visited dir");
        w.history_back();
        assert_eq!(w.current_path(), d.path());
        w.history_forward();
        assert_eq!(w.current_path(), a, "forward re-visits");
    }

    /// A hermetic widget whose favourites/zoom persist to a throwaway dir, so
    /// tests never touch the user's real `~/.config`.
    fn hermetic() -> (FileExplorerWidget, tempfile::TempDir) {
        let cfg = tempfile::tempdir().unwrap();
        let w = FileExplorerWidget::with_persistence(
            Some(cfg.path().to_path_buf()),
            Some(PathBuf::from("/home/tester")),
        );
        (w, cfg)
    }

    #[test]
    fn double_click_click_command_navigates_into_a_directory() {
        // Drives the real pointer path: a `Click` command, then a second within
        // the double-click window → the app opens (navigates into) the folder.
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let (mut w, _cfg) = hermetic();
        w.navigate(d.path().to_path_buf());
        let idx = w.displayed_entries().iter().position(|e| e.name == "sub").unwrap();

        w.apply_command(FileCommand::Click { index: idx, ctrl: false, shift: false });
        assert_eq!(w.current_path(), d.path(), "first click only selects");
        assert_eq!(w.selected_index(), Some(idx));

        w.apply_command(FileCommand::Click { index: idx, ctrl: false, shift: false });
        assert_eq!(w.current_path(), d.path().join("sub"), "double-click opens the directory");
    }

    #[test]
    fn zoom_commands_step_the_item_size_and_clamp() {
        let (mut w, _cfg) = hermetic();
        let base = w.zoom_level();
        w.apply_command(FileCommand::ZoomIn);
        assert_eq!(w.zoom_level(), base + 1, "zoom in raises the step");
        w.apply_command(FileCommand::ZoomOut);
        w.apply_command(FileCommand::ZoomOut);
        assert_eq!(w.zoom_level(), base - 1, "zoom out lowers the step");
        // Clamps at the extremes.
        for _ in 0..50 {
            w.apply_command(FileCommand::ZoomOut);
        }
        assert_eq!(w.zoom_level(), config::ZOOM_MIN, "zoom clamps at the minimum");
    }

    #[test]
    fn zoom_level_persists_across_reconstruction() {
        let cfg = tempfile::tempdir().unwrap();
        {
            let mut w = FileExplorerWidget::with_persistence(
                Some(cfg.path().to_path_buf()),
                Some(PathBuf::from("/home/tester")),
            );
            w.apply_command(FileCommand::ZoomIn);
            w.apply_command(FileCommand::ZoomIn);
            assert_eq!(w.zoom_level(), 2);
        }
        // A fresh widget reads the persisted level back.
        let w2 = FileExplorerWidget::with_persistence(
            Some(cfg.path().to_path_buf()),
            Some(PathBuf::from("/home/tester")),
        );
        assert_eq!(w2.zoom_level(), 2, "zoom level survives a restart");
    }

    #[test]
    fn favorite_commands_add_reorder_and_remove_and_persist() {
        let cfg = tempfile::tempdir().unwrap();
        let make = || {
            FileExplorerWidget::with_persistence(
                Some(cfg.path().to_path_buf()),
                Some(PathBuf::from("/home/tester")),
            )
        };
        let sub = PathBuf::from("/home/tester/Projects/qos");
        let mut w = make();
        let before = w.favorites().len();
        w.apply_command(FileCommand::AddFavorite(sub.clone()));
        assert_eq!(w.favorites().len(), before + 1, "added one favourite");
        assert_eq!(w.favorites().last().unwrap().path, sub);

        // Reorder: drag the new favourite (last) up to index 1.
        let last = w.favorites().len() - 1;
        w.apply_command(FileCommand::ReorderFavorite { from: last, to: 1 });
        assert_eq!(w.favorites()[1].path, sub, "reorder moved it to slot 1");

        // The new order + membership persists to a fresh widget.
        let w2 = make();
        assert_eq!(w2.favorites()[1].path, sub, "reordered favourite persists");

        // Remove drops it (and persists).
        let mut w3 = make();
        w3.apply_command(FileCommand::RemoveFavorite(sub.clone()));
        assert!(!w3.favorites().iter().any(|f| f.path == sub));
        let w4 = make();
        assert!(!w4.favorites().iter().any(|f| f.path == sub), "removal persists");
    }

    #[test]
    fn favourites_follow_localized_xdg_user_dirs() {
        use crate::favorites::{FavKind, UserDirs};
        let cfg = tempfile::tempdir().unwrap();
        let home = PathBuf::from("/home/tester");
        // A German desktop: the XDG user dirs resolve to localized folder names.
        let dirs = UserDirs {
            home: Some(home.clone()),
            documents: Some(home.join("Dokumente")),
            downloads: Some(home.join("Downloads")),
            pictures: Some(home.join("Bilder")),
            music: Some(home.join("Musik")),
            videos: Some(home.join("Videos")),
            ..Default::default()
        };
        let w = FileExplorerWidget::with_persistence_dirs(
            Some(cfg.path().to_path_buf()),
            Some(home.clone()),
            dirs,
        );
        let labels: Vec<String> = w.favorites().iter().map(|f| f.label.clone()).collect();
        // The sidebar shows the user's real localized names, not hard-coded
        // English ones.
        assert!(labels.contains(&"Dokumente".to_string()), "labels: {labels:?}");
        assert!(labels.contains(&"Bilder".to_string()), "labels: {labels:?}");
        assert!(labels.contains(&"Musik".to_string()), "labels: {labels:?}");
        assert!(!labels.contains(&"Documents".to_string()), "must not show hard-coded English Documents");
        // The Documents favourite points at the actual localized directory (so
        // clicking it opens ~/Dokumente, where the files really are) and keeps
        // its evocative glyph despite the localized label.
        let docs = w.favorites().into_iter().find(|f| f.kind == FavKind::Documents).unwrap();
        assert_eq!(docs.path, home.join("Dokumente"));
        assert_eq!(docs.icon(), ui::icons::Icon::FileText);
    }

    #[test]
    fn new_from_template_creates_the_file_and_opens_rename() {
        let d = tempfile::tempdir().unwrap();
        let tmpl = d.path().join("Doc.md");
        std::fs::write(&tmpl, b"template body").unwrap();
        let work = d.path().join("work");
        std::fs::create_dir(&work).unwrap();

        let (mut w, _cfg) = hermetic();
        w.navigate(work.clone());
        w.apply_command(FileCommand::NewFromTemplate(tmpl));
        assert!(work.join("Doc.md").exists(), "template copied into the current dir");
        assert_eq!(std::fs::read(work.join("Doc.md")).unwrap(), b"template body");
        assert!(w.is_renaming(), "new file drops straight into inline-rename");
    }

    #[test]
    fn toggle_hidden_reveals_and_hides_dotfiles() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".secret"), b"x").unwrap();
        std::fs::write(d.path().join("visible.txt"), b"x").unwrap();
        let (mut w, _cfg) = hermetic();
        w.navigate(d.path().to_path_buf());
        assert!(!w.entries_snapshot().iter().any(|e| e.name == ".secret"), "dotfiles hidden by default");
        w.apply_command(FileCommand::ToggleHidden);
        assert!(w.show_hidden());
        assert!(w.entries_snapshot().iter().any(|e| e.name == ".secret"), "dotfiles shown after toggle");
        w.apply_command(FileCommand::ToggleHidden);
        assert!(!w.entries_snapshot().iter().any(|e| e.name == ".secret"), "toggled back off");
    }

    #[test]
    fn scroll_position_survives_a_structural_rebuild_but_resets_on_navigation() {
        // The real fix: selecting a row / re-sorting rebuilds the whole
        // composition (a fresh Scrollable), which used to snap the list back to
        // the top. The persisted offset cell — the exact one the rebuilt
        // Scrollable binds to via `with_persisted_offset` — must carry the
        // position across those rebuilds, and only reset when the directory
        // actually changes.
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        std::fs::create_dir(&a).unwrap();
        for i in 0..6 {
            std::fs::write(a.join(format!("f{i}.txt")), b"x").unwrap();
        }
        let b = d.path().join("b");
        std::fs::create_dir(&b).unwrap();

        let (mut w, _cfg) = hermetic();
        w.navigate(a.clone());
        // Simulate the list having been scrolled down 240px.
        w.scroll_offset.set((0.0, 240.0));

        // Selecting a row is a structural rebuild — scroll must hold.
        w.apply_command(FileCommand::Click { index: 0, ctrl: false, shift: false });
        assert_eq!(w.scroll_offset.get().1, 240.0, "selecting a row keeps the scroll");
        // Re-sorting is a structural rebuild — scroll must hold.
        w.apply_command(FileCommand::SetSort(SortBy::Size));
        assert_eq!(w.scroll_offset.get().1, 240.0, "re-sorting keeps the scroll");

        // Navigating to a different directory starts at the top.
        w.navigate(b.clone());
        assert_eq!(w.scroll_offset.get(), (0.0, 0.0), "a new directory scrolls to the top");
    }

    #[test]
    fn context_select_targets_the_clicked_row_not_a_stale_selection() {
        // Guards the dangerous bug: a right-click's menu must act on the row it
        // landed on, not whatever happened to be selected. `ContextSelect` is
        // the command the content pane pushes on a right-press.
        let (mut w, _cfg) = hermetic();
        w.entries = vec![
            ent("a.txt", false, "text/plain"),
            ent("b.txt", false, "text/plain"),
            ent("c.txt", false, "text/plain"),
        ];
        // Stale selection on row 0.
        w.selection.select_only(0);
        // Right-press on row 2 moves the selection there.
        assert!(w.apply_command(FileCommand::ContextSelect(Some(2))));
        assert_eq!(w.selected_indices(), vec![2], "context select targets the clicked row");

        // A right-press inside an existing multi-selection preserves all of it
        // (so a menu action applies to every highlighted item).
        w.selection.select_only(1);
        w.selection.toggle(2);
        w.apply_command(FileCommand::ContextSelect(Some(1)));
        let mut got = w.selected_indices();
        got.sort_unstable();
        assert_eq!(got, vec![1, 2], "right-click within a multi-selection keeps it");

        // A right-press on empty space clears the selection.
        w.apply_command(FileCommand::ContextSelect(None));
        assert!(w.selected_indices().is_empty(), "empty-space right-click clears selection");
    }

    #[test]
    fn context_menu_gates_new_file_on_templates_existing() {
        // The "New File ▸" entry appears iff the Templates dir has files, and
        // "New Folder" is always present. Templates are injected so the check is
        // hermetic (no reliance on the ambient Templates dir).
        let (w, _cfg) = hermetic();
        let has_new_file = |spec: &[(String, Option<FileCommand>)]| {
            spec.iter().any(|(label, _)| label.starts_with("New File"))
        };
        let has_new_folder = |spec: &[(String, Option<FileCommand>)]| {
            spec.iter().any(|(label, _)| label == "New Folder")
        };

        // Empty Templates dir → no "New File", but "New Folder" stays.
        let none = w.context_menu_spec(&[]);
        assert!(!has_new_file(&none), "no templates → submenu hidden");
        assert!(has_new_folder(&none), "New Folder is always offered");

        // Two templates → the "New File ▸" entry appears.
        let two = w.context_menu_spec(&[PathBuf::from("Doc.md"), PathBuf::from("Sheet.csv")]);
        assert!(has_new_file(&two), "templates present → New File entry shown");
        assert!(has_new_folder(&two));
    }
}


#[cfg(test)]
mod responsive_body_tests {
    //! The places list is a *navigation* sidebar over the file view, on the
    //! shared `NavShell`: a comfortably-wide window docks it beside the files;
    //! a narrow window turns it into an overlay drawer over them. The file
    //! list is the content at every width — it is never pushed off the window
    //! to make room for the list of places. Drives the real production path
    //! (`build_layout` → `apply_layout` at a size, then `handle_event`).
    use super::*;
    use std::collections::HashMap;
    use taffy::prelude::*;
    use ui::theme::Theme;

    struct H {
        w: FileExplorerWidget,
        taffy: TaffyTree<()>,
        node: NodeId,
        state: ui::State,
        tr: ui::text_render::TextRenderer,
    }

    impl H {
        fn new(dir: Option<&std::path::Path>, width: f32, height: f32) -> Self {
            let mut w = FileExplorerWidget::with_persistence(None, None);
            if let Some(d) = dir {
                w.navigate(d.to_path_buf());
            }
            Self::from_widget(w, width, height)
        }

        /// Lay out a pre-built widget (e.g. one with persisted favourites, which
        /// `new`'s home-less widget lacks).
        fn from_widget(w: FileExplorerWidget, width: f32, height: f32) -> Self {
            let mut h = H {
                w,
                taffy: TaffyTree::new(),
                node: NodeId::new(0),
                state: ui::State::new(),
                tr: ui::text_render::TextRenderer::new(),
            };
            h.lay_out(width, height);
            h
        }

        fn lay_out(&mut self, width: f32, height: f32) {
            let theme = Theme::dark();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            self.w.sync(&mut self.state.doc, &self.state.root);
            let node = self.w.build_layout(&mut taffy, &theme, &mut nm);
            let measure_map = nm.clone();
            let trm = &mut self.tr;
            taffy
                .compute_layout_with_measure(
                    node,
                    Size {
                        width: AvailableSpace::Definite(width),
                        height: AvailableSpace::Definite(height),
                    },
                    |k, a, id, _, _| {
                        if let Some(&p) = measure_map.get(&id) {
                            unsafe { (*p).measure(k, a, trm) }
                        } else {
                            Size::ZERO
                        }
                    },
                )
                .unwrap();
            self.w.apply_layout(&taffy, node, Point::ORIGIN);
            self.taffy = taffy;
            self.node = node;
        }

        fn dispatch(&mut self, ev: &winit::event::WindowEvent) {
            let mut handled = false;
            let mut focus = None;
            self.w.handle_event(
                ev,
                1.0,
                &winit::event::Modifiers::default(),
                &mut self.state.doc,
                &self.state.root,
                &mut handled,
                &mut focus,
                self.node,
                &self.taffy,
            );
        }

        fn cursor(&mut self, x: f64, y: f64) {
            self.dispatch(&winit::event::WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(x, y),
            });
        }

        fn right_press(&mut self) {
            self.dispatch(&winit::event::WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Right,
            });
        }

        fn left_release(&mut self) {
            self.dispatch(&winit::event::WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
            });
        }

        /// The labels of the currently-open context/favourite menu, if any.
        fn open_menu_labels(&self) -> Vec<String> {
            if !self.w.context_menu.is_open() {
                return Vec::new();
            }
            self.w
                .context_menu
                .items()
                .iter()
                .filter_map(|i| i.label().map(str::to_string))
                .collect()
        }

        /// The laid-out rect of a named widget in the live tree. Asserting on
        /// the *file view's* own rect is what makes the narrow test about the
        /// files rather than about whichever pane happens to be full-width.
        fn rect_of(&self, name: &str) -> Option<vello::kurbo::Rect> {
            fn find(w: &dyn Widget, name: &str) -> Option<vello::kurbo::Rect> {
                if w.name() == name {
                    return Some(w.get_rect());
                }
                for c in w.children() {
                    if let Some(r) = find(c, name) {
                        return Some(r);
                    }
                }
                None
            }
            find(&self.w, name)
        }

        /// Run a real draw pass so immediate-mode children (the file view)
        /// publish their per-frame geometry — e.g. the in-place rename field's
        /// rect.
        fn draw(&mut self) {
            let theme = Theme::dark();
            let presence = ui::presence::PresenceCtx::empty();
            let permissions = ui::permissions::PermissionContext::permissive();
            let mut scene = vello::Scene::new();
            self.w.draw_with_presence(
                &mut scene,
                vello::kurbo::Affine::IDENTITY,
                &mut self.tr,
                &theme,
                &presence,
                &permissions,
            );
        }

        fn press_named(&mut self, key: winit::keyboard::NamedKey) {
            let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
            ev.logical_key = winit::keyboard::Key::Named(key);
            ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
                winit::keyboard::NativeKeyCode::Unidentified,
            );
            ev.location = winit::keyboard::KeyLocation::Standard;
            ev.state = winit::event::ElementState::Pressed;
            ev.repeat = false;
            ev.text = None;
            self.dispatch(&winit::event::WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event: ev,
                is_synthetic: false,
            });
        }

        fn escape(&mut self) {
            self.press_named(winit::keyboard::NamedKey::Escape);
        }

        fn type_char(&mut self, s: &str) {
            let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
            ev.logical_key = winit::keyboard::Key::Character(winit::keyboard::SmolStr::new(s));
            ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
                winit::keyboard::NativeKeyCode::Unidentified,
            );
            ev.location = winit::keyboard::KeyLocation::Standard;
            ev.state = winit::event::ElementState::Pressed;
            ev.repeat = false;
            ev.text = Some(winit::keyboard::SmolStr::new(s));
            self.dispatch(&winit::event::WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event: ev,
                is_synthetic: false,
            });
        }
    }

    #[test]
    fn body_docks_wide_and_reflows_to_push_when_narrowed() {
        let mut h = H::new(None, 1200.0, 720.0);
        assert_eq!(
            h.w.debug_body_mode(),
            ShellMode::Wide,
            "a wide window docks the places list beside the file view"
        );
        // Resize DOWN on the same instance (a real window resize re-runs
        // apply_layout) — the body must reflow to a single pane, not freeze.
        h.lay_out(520.0, 720.0);
        assert_eq!(
            h.w.debug_body_mode(),
            ShellMode::Narrow,
            "a narrow window collapses the body to a single pane"
        );
        // ...and back up again proves it isn't one-way.
        h.lay_out(1200.0, 720.0);
        assert_eq!(h.w.debug_body_mode(), ShellMode::Wide);
    }

    /// The regression: at ~600 px the window showed the breadcrumb and the
    /// FAVOURITES / SYSTEM list and **no files at all**, while the footer
    /// underneath read "27 items". The places list had been declared a content
    /// overview, so the narrow layout promoted it to the full-screen page and
    /// pushed the file view off the window.
    #[test]
    fn narrow_shows_the_files_not_the_places_list() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), b"x").unwrap();
        }
        let mut h = H::new(Some(dir.path()), 600.0, 720.0);
        assert_eq!(h.w.debug_body_mode(), ShellMode::Narrow);

        assert!(
            !h.w.debug_places_open(),
            "the places drawer starts closed, so the files are what you see",
        );
        // The file view's OWN rect, not "whichever pane is full-width": under
        // the original bug the places list was the full-width pane and the file
        // view was laid out off the right edge of the window.
        let files = h
            .rect_of("FileView")
            .expect("the file view must be in the tree at a narrow width");
        assert!(
            files.width() > 0.0 && files.height() > 0.0,
            "the file view must be laid out at a real size; got {files:?}",
        );
        assert!(
            files.x0 >= 0.0 && files.x1 <= 600.0,
            "and inside the window rather than pushed off it: {files:?}",
        );
        assert_eq!(
            h.w.debug_content_rect().width(),
            600.0,
            "the content pane spans the whole narrow window",
        );
    }

    /// The drawer draws *over* the files rather than displacing them, and the
    /// files stay full-width behind it (HIG §1a).
    #[test]
    fn narrow_places_drawer_overlays_the_files_without_displacing_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        let mut h = H::new(Some(dir.path()), 600.0, 720.0);
        let before = h.w.debug_content_rect();

        h.w.debug_open_places_drawer();
        h.lay_out(600.0, 720.0);
        h.w.debug_open_places_drawer();
        assert!(h.w.debug_places_open(), "the drawer is revealed");
        assert_eq!(
            h.w.debug_content_rect(),
            before,
            "the file view must not move or shrink when the drawer opens",
        );
    }

    /// Picking a destination in the narrow drawer dismisses it, leaving the
    /// user on the files they navigated to.
    #[test]
    fn narrow_navigation_dismisses_the_drawer_and_leaves_the_files_showing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let mut h = H::new(Some(dir.path()), 520.0, 720.0);
        assert_eq!(h.w.debug_body_mode(), ShellMode::Narrow);
        h.w.debug_open_places_drawer();
        assert!(h.w.debug_places_open());

        // A place-navigation (what tapping a place in the list emits). Inject
        // the command the places list pushes, then pump an event so
        // `handle_event` drains it and applies the dismissal wiring.
        crate::command::push(&h.w.cmds, FileCommand::Navigate(dir.path().join("sub")));
        h.cursor(10.0, 10.0);
        assert_eq!(h.w.current_path(), dir.path().join("sub"), "the navigation applied");
        assert!(
            !h.w.debug_places_open(),
            "navigating dismisses the drawer so the files are what remains",
        );
        assert_eq!(
            h.w.debug_content_rect().width(),
            520.0,
            "and the file view still owns the whole window",
        );
    }

    /// An inline rename is edited *in place* — a field placed over the row's
    /// name inside the file view — not in a bottom bar. The file view publishes
    /// the field rect during paint; it must sit within the file rows.
    #[test]
    fn rename_is_edited_in_place_over_the_row() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), b"x").unwrap();
        let mut h = H::new(Some(dir.path()), 1000.0, 720.0);
        let idx = h.w.displayed_entries().iter().position(|e| e.name == "old.txt").unwrap();
        h.w.select(Some(idx));
        assert!(h.w.apply_command(FileCommand::Rename));
        assert!(h.w.is_renaming());

        // Rebuild with the rename active, then paint so the file view publishes
        // the editor's on-screen rect.
        h.lay_out(1000.0, 720.0);
        h.draw();

        let field = h.w.rename_rect.get().expect("the in-place editor is placed over the row");
        let files = h.rect_of("FileView").expect("the file view is present");
        assert!(
            field.y0 >= files.y0 - 1.0 && field.y1 <= files.y1 + 1.0,
            "the rename field sits within the file rows (in place), not a bottom bar: \
             field={field:?} files={files:?}",
        );
        assert!(field.width() > 40.0, "the field spans the name column: {field:?}");

        // Cancelling clears the published rect (nothing left drawn).
        h.w.cancel_rename();
        h.lay_out(1000.0, 720.0);
        h.draw();
        assert!(h.w.rename_rect.get().is_none(), "cancel removes the in-place editor");
    }

    /// Typing into the in-place editor and pressing Enter renames the file on
    /// disk — driving the real keystroke path through the app into the shared
    /// `TextInput` and its CRDT-backed field. F2 opens the editor.
    #[test]
    fn typing_in_the_in_place_editor_renames_on_enter() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        let mut h = H::new(Some(dir.path()), 1000.0, 720.0);
        let idx = h.w.displayed_entries().iter().position(|e| e.name == "a.txt").unwrap();
        h.w.select(Some(idx));

        // F2 begins the rename; rebuild + paint places + seeds the editor.
        h.press_named(winit::keyboard::NamedKey::F2);
        assert!(h.w.is_renaming(), "F2 starts an inline rename");
        h.lay_out(1000.0, 720.0);
        h.draw();

        // The seed selected the whole name, so typing replaces it.
        for ch in ["b", ".", "m", "d"] {
            h.type_char(ch);
        }
        h.press_named(winit::keyboard::NamedKey::Enter);

        assert!(!h.w.is_renaming(), "Enter commits and closes the editor");
        assert!(dir.path().join("b.md").exists(), "the typed name renamed the file on disk");
        assert!(!dir.path().join("a.txt").exists(), "the old name is gone");
    }

    /// Dragging a folder out of the list and dropping it on the docked sidebar
    /// pins it to Favourites; a release off the sidebar pins nothing.
    #[test]
    fn dropping_a_folder_on_the_sidebar_pins_it() {
        let cfg = tempfile::tempdir().unwrap();
        let w = FileExplorerWidget::with_persistence(
            Some(cfg.path().to_path_buf()),
            Some(PathBuf::from("/home/tester")),
        );
        let mut h = H::from_widget(w, 1200.0, 720.0);
        assert_eq!(h.w.debug_body_mode(), ShellMode::Wide);
        let before = h.w.favorites().len();
        let folder = PathBuf::from("/home/tester/Projects/qos");

        // Begin the drag (what the file view emits once a folder press travels).
        assert!(h.w.apply_command(FileCommand::BeginDrag(folder.clone())));
        assert!(h.w.debug_pin_dragging(), "the drag is in flight");

        // Move over the sidebar, then release → pin.
        let sb = h.w.debug_sidebar_rect();
        h.cursor(sb.center().x, sb.center().y);
        h.left_release();
        assert!(!h.w.debug_pin_dragging(), "the drop ends the drag");
        assert_eq!(h.w.favorites().len(), before + 1, "the folder was pinned");
        assert!(h.w.favorites().iter().any(|f| f.path == folder), "and it is the dropped folder");
    }

    #[test]
    fn releasing_a_pin_drag_off_the_sidebar_pins_nothing() {
        let cfg = tempfile::tempdir().unwrap();
        let w = FileExplorerWidget::with_persistence(
            Some(cfg.path().to_path_buf()),
            Some(PathBuf::from("/home/tester")),
        );
        let mut h = H::from_widget(w, 1200.0, 720.0);
        let before = h.w.favorites().len();
        h.w.apply_command(FileCommand::BeginDrag(PathBuf::from("/home/tester/Downloads/tmp")));
        // Release over the content pane (not the sidebar).
        let content = h.w.debug_content_rect();
        h.cursor(content.center().x, content.center().y);
        h.left_release();
        assert!(!h.w.debug_pin_dragging());
        assert_eq!(h.w.favorites().len(), before, "a drop off the sidebar pins nothing");
    }

    /// A right-click on a sidebar favourite opens *its* menu (Open + Remove from
    /// Favourites), not the file-view context menu. The pinned Home entry offers
    /// no Remove. Drives the real right-press path end to end.
    #[test]
    fn right_clicking_a_favourite_opens_its_menu() {
        let cfg = tempfile::tempdir().unwrap();
        let w = FileExplorerWidget::with_persistence(
            Some(cfg.path().to_path_buf()),
            Some(PathBuf::from("/home/tester")),
        );
        let mut h = H::from_widget(w, 1200.0, 720.0);
        assert_eq!(h.w.debug_body_mode(), ShellMode::Wide, "wide docks the sidebar");

        let band = h.w.favorites_store().borrow().band.expect("sidebar laid out");
        // Row 1 is a removable favourite (row 0 is the pinned Home).
        let (x, y) = (band.center_x(), band.row_center_y(1));
        h.cursor(x, y);
        h.right_press();
        let labels = h.open_menu_labels();
        assert!(
            labels.iter().any(|l| l == "Remove from Favourites"),
            "the favourite's own menu is shown: {labels:?}",
        );
        assert!(labels.iter().any(|l| l == "Open"), "with the Open folder action: {labels:?}");
        // It is NOT the file-view context menu (which would offer New Folder).
        assert!(!labels.iter().any(|l| l == "New Folder"), "not the file menu: {labels:?}");

        // The pinned Home favourite (row 0) has no Remove.
        let (hx, hy) = (band.center_x(), band.row_center_y(0));
        h.cursor(hx, hy);
        h.right_press();
        let home = h.open_menu_labels();
        assert!(
            !home.iter().any(|l| l == "Remove from Favourites"),
            "Home is pinned and cannot be removed: {home:?}",
        );
    }

    #[test]
    fn wide_docks_the_places_list_beside_the_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let mut h = H::new(Some(dir.path()), 1200.0, 720.0);
        assert_eq!(h.w.debug_body_mode(), ShellMode::Wide);
        assert!(h.w.debug_places_open(), "wide docks the places list open");

        let content = h.w.debug_content_rect();
        assert_eq!(
            content.x1, 1200.0,
            "the file view runs to the window's right edge",
        );
        assert!(
            content.x0 > 0.0,
            "and starts after the docked places list: {content:?}",
        );

        crate::command::push(&h.w.cmds, FileCommand::Navigate(dir.path().join("sub")));
        h.cursor(10.0, 10.0);
        assert!(
            h.w.debug_places_open(),
            "wide navigation leaves the docked list in place — nothing to dismiss",
        );
    }
}
