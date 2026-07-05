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
use crate::fileops;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::{Column, Container, Row, ScrollAxis, Scrollable, Spacer, Text, TextInput, Widget};
use ui::{AppKind, AppWidget, IconDescriptor, SelectionModel};
use vello::kurbo::Point;

/// CRDT map key holding the toolbar search field's `Text` object. The
/// widget seeds this object in `sync` (the toolkit's `TextInput` only writes
/// into an existing `Text`, never lazily creates one), so keystrokes in the
/// search box splice here and `sync` reads the query back out to filter.
const SEARCH_QUERY_PATH: &str = "files.search_query";
/// CRDT map key holding the inline-rename field's `Text` object. Seeded with
/// the current name when a rename begins; the rename bar's `TextInput` edits
/// it and `commit_rename` reads the chosen name back out.
const RENAME_QUERY_PATH: &str = "files.rename_query";

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
    /// Multi-selection over the *displayed* (filtered) entry list.
    selection: SelectionModel,
    search_query: String,
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
    /// progress: the rename bar is shown and its `TextInput` edits
    /// [`RENAME_QUERY_PATH`]. `None` = not renaming.
    renaming: Option<usize>,
    /// Editable name mirrored from the rename field's `Text` object each
    /// `sync` while renaming; `commit_rename` renames the file to this.
    rename_buffer: String,
    /// Set when a rename just began so the next `sync` seeds the CRDT
    /// `Text` object with `rename_buffer` (the current name) before the
    /// input reads it back.
    rename_seed_pending: bool,
    rect: vello::kurbo::Rect,
    composed: Option<Box<dyn Widget>>,
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
    /// Last cursor position in logical coords (for menu placement + the
    /// context-menu hit-test).
    last_cursor: Point,
}

impl Default for FileExplorerWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl FileExplorerWidget {
    pub fn new() -> Self {
        let home = dirs::home_dir();
        let cwd = initial_path();
        let mut entries = read_dir_sync(&cwd).unwrap_or_default();
        sort_entries(SortBy::Name, SortOrder::Ascending, &mut entries);
        Self {
            current_path: cwd,
            home_path: home,
            entries,
            selection: SelectionModel::new(),
            search_query: String::new(),
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
            rect: vello::kurbo::Rect::ZERO,
            composed: None,
            cmds: new_sink(),
            structure_dirty: false,
            context_menu: ContextMenu::new(),
            menu_commands: Vec::new(),
            last_cursor: Point::ZERO,
        }
    }

    /// The entries actually shown — `entries` filtered by the search query.
    pub fn displayed_entries(&self) -> Vec<Entry> {
        if self.search_query.trim().is_empty() {
            self.entries.clone()
        } else {
            let q = self.search_query.to_lowercase();
            self.entries
                .iter()
                .filter(|e| e.name.to_lowercase().contains(&q))
                .cloned()
                .collect()
        }
    }

    /// Build the full composed widget tree, wiring every interactive child
    /// to the command sink.
    fn build_composition(&self, theme: &ui::Theme) -> Box<dyn Widget> {
        let displayed = self.displayed_entries();

        // Toolbar (nav + view + sort + search + terminal).
        let toolbar = crate::toolbar::build_toolbar(
            crate::toolbar::ToolbarSnapshot {
                search_query: &self.search_query,
                view_mode: self.view_mode,
                sort_order: self.sort_order,
                can_back: !self.history.is_empty(),
                can_forward: !self.forward.is_empty(),
                can_up: self.current_path.parent().is_some(),
            },
            &self.cmds,
        );

        // Breadcrumb — a compact, clickable path trail directly under the
        // toolbar (leading vector icon + text segments + chevrons).
        let crumbs = crate::breadcrumbs::breadcrumb_components(
            &self.current_path,
            self.home_path.as_deref(),
        );
        let breadcrumb = crate::breadcrumb_bar::BreadcrumbBar::new(crumbs, self.cmds.clone());

        // Sidebar — tiered shortcuts, each wired to Navigate. Section titles
        // are small dim labels; rows carry a themed vector glyph.
        let tiers = crate::sidebar::build_tiers(self.home_path.as_deref());
        let mut sidebar_col = Column::new()
            .with_horizontal_padding(10.0, 10.0)
            .with_vertical_padding(10.0);
        let mut flat_index = 0usize;
        for tier in &tiers {
            sidebar_col = sidebar_col.push(Box::new(
                Text::new(&tier.title.to_uppercase())
                    .with_size(11.0)
                    .with_color(theme.tokens.colors.text_dim),
            ));
            for (icon, label, target) in &tier.entries {
                let id = format!("files.sidebar.{flat_index}");
                let is_active = *target == self.current_path;
                sidebar_col = sidebar_col.push(Box::new(
                    crate::sidebar_row::SidebarFolderRow::new(&id, *icon, label, target.clone())
                        .with_active(is_active)
                        .with_sink(self.cmds.clone()),
                ));
                flat_index += 1;
            }
        }
        let sidebar: Box<dyn Widget> = Box::new(
            Container::new()
                .with_width(crate::sidebar::SIDEBAR_WIDTH_PX as f32)
                .with_no_padding()
                .with_child(Box::new(sidebar_col)),
        );

        // Main view: list or grid, bound to the displayed entries + selection.
        // Tight top padding so the list starts right under the breadcrumb —
        // no wasted vertical band — while a small left inset separates it
        // from the sidebar.
        let view_child: Box<dyn Widget> = match self.view_mode {
            ViewMode::List => crate::views::list_view::build_list(&displayed, &self.selection, &self.cmds),
            ViewMode::Grid => crate::views::grid_view::build_grid(&displayed, &self.selection, &self.cmds),
        };
        let mut view_col = Column::new()
            .with_fill_remaining_height()
            .with_vertical_padding(0.0)
            .with_horizontal_padding(8.0, 0.0);
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
        view_col = view_col.push(Box::new(
            Scrollable::new(ScrollAxis::Vertical).with_child(view_child),
        ));

        // Body fills the remaining height; sidebar is fixed-width, the view
        // pane takes the rest.
        let body = Row::new()
            .with_fill_remaining_height()
            .push(sidebar)
            .push(Box::new(view_col));

        // Footer: status (if any) else item count + path summary (dimmed).
        let (count, path_str) = self.footer_text();
        let left = self
            .status
            .clone()
            .unwrap_or_else(|| format!("{count} items"));
        let footer = Row::new()
            .push(Box::new(Text::new(&left).with_size(12.0).with_color(theme.tokens.colors.text_dim)))
            .push(Box::new(Spacer::new()))
            .push(Box::new(Text::new(&path_str).with_size(12.0).with_color(theme.tokens.colors.text_dim)));

        let mut root_col = Column::new()
            .push(toolbar)
            .push(Box::new(breadcrumb))
            .push(Box::new(body));
        // Inline-rename bar sits just above the footer while a rename is
        // active; its TextInput edits the RENAME_QUERY_PATH Text object.
        if self.renaming.is_some() {
            root_col = root_col.push(self.build_rename_bar());
        }
        root_col = root_col.push(Box::new(footer));
        Box::new(root_col)
    }

    /// The inline-rename editor row: a label, a CRDT-bound name field, and
    /// confirm/cancel buttons. Shown only while [`renaming`](Self::renaming)
    /// is set. Enter/Escape (handled in `handle_event`) mirror the buttons.
    fn build_rename_bar(&self) -> Box<dyn Widget> {
        let commit = self.cmds.clone();
        let cancel = self.cmds.clone();
        let input = TextInput::new(vec![])
            .with_id("files.rename_input")
            .with_on_change_path(RENAME_QUERY_PATH)
            .with_placeholder("New name");
        Box::new(
            Row::new()
                .no_wrap()
                .push(Box::new(Text::new("Rename:")))
                .push(Box::new(
                    Container::new()
                        .with_width(260.0)
                        .with_child(Box::new(input)),
                ))
                .push(Box::new(
                    ui::widget::Button::new("files.rename_confirm", "Rename", None)
                        .on_click(move || crate::command::push(&commit, FileCommand::RenameCommit)),
                ))
                .push(Box::new(
                    ui::widget::Button::new("files.rename_cancel", "Cancel", None)
                        .on_click(move || crate::command::push(&cancel, FileCommand::RenameCancel)),
                )),
        )
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
    pub fn search_query(&self) -> &str {
        &self.search_query
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
        match read_dir_sync(&path) {
            Ok(mut entries) => {
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

    pub fn set_search_query(&mut self, q: String) {
        self.search_query = q;
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
    /// tree structure (and thus needs a rebuild).
    fn apply_command(&mut self, cmd: FileCommand) -> bool {
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
        }
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

    /// Open `path` with `runner`: navigate into directories, launch the
    /// registered app for document types, and hand everything else to the
    /// desktop default handler (`open::that`, via `CommandRunner::open_default`).
    /// Returns whether a launch happened (`false` when it navigated a dir).
    fn open_target<R: CommandRunner>(
        &mut self,
        path: PathBuf,
        runner: &mut R,
    ) -> std::io::Result<bool> {
        if path.is_dir() {
            self.navigate(path);
            return Ok(false);
        }
        let cwd = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.current_path.clone());
        match app_for_path(&path) {
            Some(app) => runner.spawn_arg(app, &path, &cwd)?,
            // No registered qOS app for this type — open with the desktop
            // default (images, PDFs, video, audio, plain text, code, …).
            None => runner.open_default(&path)?,
        }
        Ok(true)
    }

    // --- inline rename -------------------------------------------------

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

    /// Begin an inline rename of the primary selection: open the rename bar
    /// seeded with the entry's current name. No-op without a selection.
    pub fn begin_rename(&mut self) {
        let disp = self.displayed_entries();
        if let Some(i) = self.selection.primary() {
            if let Some(e) = disp.get(i) {
                self.renaming = Some(i);
                self.rename_buffer = e.name.clone();
                self.rename_seed_pending = true;
                self.status = None;
            }
        }
    }

    /// Abandon the in-progress rename, leaving the file untouched.
    pub fn cancel_rename(&mut self) {
        self.renaming = None;
        self.rename_seed_pending = false;
    }

    /// Commit the in-progress rename: rename the target file to
    /// `rename_buffer` (trimmed). Returns whether the file was renamed. A
    /// blank or unchanged name just closes the bar; an fs error keeps the
    /// bar open with a status message so the user can retry.
    pub fn commit_rename(&mut self) -> bool {
        let Some(idx) = self.renaming else { return false };
        let disp = self.displayed_entries();
        let Some(entry) = disp.get(idx) else {
            self.renaming = None;
            return false;
        };
        let path = entry.path.clone();
        let new_name = self.rename_buffer.trim().to_string();
        let unchanged = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|cur| cur == new_name);
        if new_name.is_empty() || unchanged {
            self.renaming = None;
            self.rename_seed_pending = false;
            return false;
        }
        match fileops::rename(&path, &new_name) {
            Ok(_) => {
                self.renaming = None;
                self.rename_seed_pending = false;
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

/// Lowercase-contains filter used by the toolbar search. Returns entries in
/// their current order.
pub fn filter_entries<'a>(entries: &'a [Entry], query: &str) -> Vec<&'a Entry> {
    let q = query.to_lowercase();
    entries.iter().filter(|e| e.name.to_lowercase().contains(&q)).collect()
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

/// The app that opens `path`, by extension. Document formats — per qdoc's
/// codec registry — open in the word processor.
pub fn app_for_path(path: &std::path::Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?;
    qdoc::is_supported_ext(ext).then_some("word-processor")
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
    fn open_context_menu(&mut self, at: Point) {
        let has_sel = !self.selection.is_empty();
        let can_paste = !self.clipboard.is_empty();
        // (label, command) pairs; `None` command = separator.
        let mut spec: Vec<(&str, Option<FileCommand>)> = Vec::new();
        if has_sel {
            spec.push(("Open", self.selection.primary().map(FileCommand::Activate)));
            spec.push(("Rename", Some(FileCommand::Rename)));
            spec.push(("", None));
            spec.push(("Cut", Some(FileCommand::Cut)));
            spec.push(("Copy", Some(FileCommand::Copy)));
        }
        if can_paste {
            spec.push(("Paste", Some(FileCommand::Paste)));
        }
        spec.push(("New Folder", Some(FileCommand::NewFolder)));
        if has_sel {
            spec.push(("", None));
            spec.push(("Move to Trash", Some(FileCommand::Delete)));
        }
        // Sort section — the only place the sort *key* can be changed in grid
        // mode (which has no clickable column header) and the only way to
        // reach "Sort by Type" at all.
        spec.push(("", None));
        spec.push(("Sort by Name", Some(FileCommand::SetSort(SortBy::Name))));
        spec.push(("Sort by Size", Some(FileCommand::SetSort(SortBy::Size))));
        spec.push(("Sort by Type", Some(FileCommand::SetSort(SortBy::Type))));
        spec.push(("Sort by Date", Some(FileCommand::SetSort(SortBy::Modified))));
        spec.push(("", None));
        spec.push(("Open Terminal Here", Some(FileCommand::OpenTerminal)));

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
        self.layout_menu();
    }

    /// Resolve the context menu's rect from its anchor (it isn't part of the
    /// App's taffy tree, so drive a throwaway layout pass).
    fn layout_menu(&mut self) {
        if !self.context_menu.is_open() {
            return;
        }
        let mut taffy: taffy::TaffyTree<()> = taffy::TaffyTree::new();
        let mut node_map: std::collections::HashMap<taffy::NodeId, *mut dyn Widget> = std::collections::HashMap::new();
        let theme = ui::Theme::dark();
        let node = self.context_menu.build_layout(&mut taffy, &theme, &mut node_map);
        let _ = taffy.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(400.0),
                height: taffy::AvailableSpace::Definite(800.0),
            },
        );
        self.context_menu.apply_layout(&taffy, node, Point::ZERO);
    }

    /// If the context menu is open, route a left-press to it. Returns true
    /// when the press was consumed (item fired or menu dismissed).
    fn route_menu_click(&mut self, p: Point) -> bool {
        if !self.context_menu.is_open() {
            return false;
        }
        if let Some(idx) = self.context_menu.item_at([p.x, p.y]) {
            self.context_menu.close();
            if let Some(Some(cmd)) = self.menu_commands.get(idx).cloned() {
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
            _ => false,
        }
    }
}

impl Widget for FileExplorerWidget {
    fn sync(&mut self, state: &mut automerge::AutoCommit, path: &automerge::ObjId) {
        use automerge::{transaction::Transactable, ReadDoc};
        // Seed the backing Text objects *before* the children sync so the
        // search + rename inputs resolve a real object to write into. The
        // toolkit's TextInput binds to an existing Text but never lazily
        // creates one — the missing seed is exactly why the search box was
        // previously dead (keystrokes mutated only the input's local string).
        let search_obj = ensure_text_obj(state, path, SEARCH_QUERY_PATH);
        let rename_obj = ensure_text_obj(state, path, RENAME_QUERY_PATH);

        // A just-started rename seeds the field with the entry's current name.
        if self.rename_seed_pending {
            let cur_len = state.text(&rename_obj).map(|t| t.chars().count()).unwrap_or(0);
            let _ = state.splice_text(&rename_obj, 0, cur_len as isize, &self.rename_buffer);
            self.rename_seed_pending = false;
        }

        if let Some(c) = &mut self.composed {
            c.sync(state, path);
        }

        // Pull the search query out of its Text object; a change re-filters
        // the displayed list, which is a structural change (rebuild).
        if let Ok(text) = state.text(&search_obj) {
            if text != self.search_query {
                self.search_query = text;
                self.selection.clear();
                self.structure_dirty = true;
            }
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
        let mut composed = self.build_composition(theme);
        let child_node = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);

        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[child_node]).unwrap();
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
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node) {
                if let Some(&child_node) = children.first() {
                    c.apply_layout(taffy, child_node, vello::kurbo::Point::new(self.rect.x0, self.rect.y0));
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
        // Paint the app background so the explorer is self-contained (light
        // text reads on it, and it looks right over any compositor surface).
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        // `self.composed` is normally built inside `build_layout`. When
        // this widget is hosted inside a shell's TabBar (e.g. Files
        // tab on the qOS shell that boots Notes-active), `build_layout`
        // doesn't run for inactive tabs — so switching to Files later
        // produces a widget with `composed = None` and nothing visible
        // renders even though `self.rect` is correct. Lazy-build here
        // so the first paint after tab activation populates a tree we
        // can hand to taffy on the NEXT layout pass. We trigger a
        // structure-dirty flag so the host rebuilds taffy with the
        // composition included on the next frame.
        if self.composed.is_none() && self.rect.width() > 0.0 && self.rect.height() > 0.0 {
            // Build the composition (toolbar + breadcrumbs + file
            // list) so a structure-dirty relayout next frame includes
            // it. The current frame still won't render content (no
            // valid taffy layout for the composed tree yet) but the
            // next frame will — the host's `take_structure_dirty`
            // pump rebuilds taffy and the third frame onward is
            // populated correctly.
            self.composed = Some(self.build_composition(theme));
            self.structure_dirty = true;
        }
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn draw_overlay(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        // Composed overlays (dropdowns/popovers) first, then our top-layer
        // context menu so it composites above everything.
        if let Some(c) = &mut self.composed {
            c.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        if self.context_menu.is_open() {
            self.layout_menu();
            self.context_menu.draw(scene, transform, text_renderer, theme, presence);
        }
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

        // The open context menu gets first dibs on clicks + Escape.
        if self.context_menu.is_open() {
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
                    self.context_menu.close();
                    *handled = true;
                    self.structure_dirty = true;
                    return;
                }
                _ => {}
            }
        }

        // Right-click opens the context menu (selecting the row under the
        // cursor first if the press landed on one).
        if let WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } = event {
            self.open_context_menu(self.last_cursor);
            *handled = true;
            self.structure_dirty = true;
            return;
        }

        // While an inline rename is active, Enter commits and Escape cancels
        // — intercepted before the keystroke reaches the rename field so it
        // never falls through to app-level nav (Enter-open / Escape). Typed
        // characters are NOT intercepted here; they fall through to the field.
        if self.renaming.is_some() {
            if let WindowEvent::KeyboardInput { event: ke, .. } = event {
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
            }
        }

        // Deliver to the composed tree first (buttons, views, search field).
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node_id) {
                if let Some(&child_node) = children.first() {
                    c.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, child_node, taffy);
                }
            }
        }

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
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut d = std::mem::take(&mut self.structure_dirty);
        if let Some(c) = &mut self.composed {
            d |= c.take_structure_dirty();
        }
        d
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed.as_ref().map(|c| c.as_ref() as &dyn Widget).into_iter().collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed.as_mut().map(|c| c.as_mut() as &mut (dyn Widget + '_)).into_iter().collect()
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
        Entry { name: name.into(), path: PathBuf::from("/docs").join(name), is_dir: false, size: 0, modified: None, mime: String::new() }
    }

    #[test]
    fn document_extensions_map_to_word_processor() {
        for f in ["report.odt", "memo.docx", "notes.md", "readme.markdown"] {
            assert_eq!(app_for_path(Path::new(f)), Some("word-processor"), "{f}");
        }
        assert_eq!(app_for_path(Path::new("photo.png")), None);
        assert_eq!(app_for_path(Path::new("main.rs")), None);
    }

    #[test]
    fn open_entry_launches_word_processor_with_the_file() {
        let mut w = FileExplorerWidget::new();
        w.entries = vec![file_entry("report.odt"), file_entry("photo.png")];

        let mut runner = RecRunner::default();
        assert!(w.open_entry(0, &mut runner).unwrap(), "document spawns an app");
        assert_eq!(runner.calls, vec![("word-processor".to_string(), Some(PathBuf::from("/docs/report.odt")))]);

        // A non-document (image) opens via the system default handler — it
        // must NOT silently do nothing (the previous behaviour, and the bug).
        assert!(w.open_entry(1, &mut runner).unwrap(), "non-document opens via default handler");
        assert_eq!(runner.calls.len(), 1, "no qOS app spawn for a plain image");
        assert_eq!(runner.opened, vec![PathBuf::from("/docs/photo.png")]);
    }

    #[test]
    fn open_entry_uses_default_handler_for_every_nondocument_type() {
        // Images, PDFs, video, audio, code — none have a qOS app, so every one
        // must route to the desktop default handler rather than no-op.
        for f in ["pic.jpg", "movie.mp4", "song.flac", "doc.pdf", "main.rs"] {
            let mut w = FileExplorerWidget::new();
            w.entries = vec![file_entry(f)];
            let mut runner = RecRunner::default();
            assert!(w.open_entry(0, &mut runner).unwrap(), "{f} should open");
            assert!(runner.calls.is_empty(), "{f} must not spawn a qOS app");
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
        }
    }

    #[test]
    fn search_box_filters_through_the_crdt_text_object() {
        use automerge::{transaction::Transactable, ObjType, ReadDoc, Value};

        let mut w = FileExplorerWidget::new();
        w.entries = vec![
            ent("README.md", false, "text/markdown"),
            ent("Cargo.toml", false, "text/plain"),
            ent("src", true, "inode/directory"),
        ];
        let mut st = ui::State::new();

        // The first sync MUST create the backing Text object the search input
        // writes into — the toolkit's TextInput never lazily creates one, so
        // without this seed the search box was completely dead.
        w.sync(&mut st.doc, &st.root);
        let obj = match st.doc.get(&st.root, SEARCH_QUERY_PATH) {
            Ok(Some((Value::Object(ObjType::Text), id))) => id,
            other => panic!("search must be backed by a Text object, got {other:?}"),
        };
        assert_eq!(w.displayed_entries().len(), 3, "no query → nothing filtered");

        // Type "argo" the way the input does: splice into the shared object.
        st.doc.splice_text(&obj, 0, 0, "argo").unwrap();
        w.sync(&mut st.doc, &st.root);
        assert_eq!(w.search_query(), "argo", "sync reads the query back from the Text object");
        let shown = w.displayed_entries();
        assert_eq!(shown.len(), 1, "only Cargo.toml contains 'argo'");
        assert_eq!(shown[0].name, "Cargo.toml");

        // Clearing the field restores the full list.
        st.doc.splice_text(&obj, 0, 4, "").unwrap();
        w.sync(&mut st.doc, &st.root);
        assert_eq!(w.displayed_entries().len(), 3);
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

        fn button_count(err: bool) -> usize {
            let mut w = FileExplorerWidget::new();
            if err {
                w.error = Some("could not read directory".into());
            }
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let theme = Theme::dark();
            let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            w.build_layout(&mut taffy, &theme, &mut nm);
            nm.values()
                .filter(|&&p| unsafe { (*p).name() } == "Button")
                .count()
        }
        // The only structural difference is the error flag, so the delta is
        // precisely the "Try Again" button the error view now builds.
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
}
