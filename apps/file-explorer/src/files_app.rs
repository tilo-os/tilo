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
    fn build_composition(&self) -> Box<dyn Widget> {
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

        // Breadcrumb — one button per path component, wired to Navigate.
        let crumbs = crate::breadcrumbs::breadcrumb_components(
            &self.current_path,
            self.home_path.as_deref(),
        );
        let mut breadcrumb_row = Row::new();
        for (i, (label, target)) in crumbs.iter().enumerate() {
            let id = format!("files.crumb.{i}");
            let sink = self.cmds.clone();
            let target = target.clone();
            breadcrumb_row = breadcrumb_row.push(Box::new(
                ui::widget::Button::new(&id, label, None)
                    .on_click(move || crate::command::push(&sink, FileCommand::Navigate(target.clone()))),
            ));
            if i + 1 < crumbs.len() {
                breadcrumb_row = breadcrumb_row.push(Box::new(Text::new("›")));
            }
        }

        // Sidebar — tiered shortcuts, each wired to Navigate.
        let tiers = crate::sidebar::build_tiers(self.home_path.as_deref());
        let mut sidebar_col = Column::new();
        let mut flat_index = 0usize;
        for tier in &tiers {
            sidebar_col = sidebar_col.push(Box::new(Text::new(tier.title)));
            for (label, target) in &tier.entries {
                let id = format!("files.sidebar.{flat_index}");
                let is_active = *target == self.current_path;
                sidebar_col = sidebar_col.push(Box::new(
                    crate::sidebar_row::SidebarFolderRow::new(&id, label, target.clone())
                        .with_active(is_active)
                        .with_sink(self.cmds.clone()),
                ));
                flat_index += 1;
            }
        }
        let sidebar: Box<dyn Widget> = Box::new(
            Container::new()
                .with_width(crate::sidebar::SIDEBAR_WIDTH_PX as f32)
                .with_child(Box::new(sidebar_col)),
        );

        // Main view: list or grid, bound to the displayed entries + selection.
        let view_child: Box<dyn Widget> = match self.view_mode {
            ViewMode::List => crate::views::list_view::build_list(&displayed, &self.selection, &self.cmds),
            ViewMode::Grid => crate::views::grid_view::build_grid(&displayed, &self.selection, &self.cmds),
        };
        let mut view_col = Column::new().with_fill_remaining_height();
        if let Some(err) = self.error.as_deref() {
            view_col = view_col.push(Box::new(Text::new(err)));
        }
        view_col = view_col.push(Box::new(
            Scrollable::new(ScrollAxis::Vertical).with_child(view_child),
        ));

        // Body fills the remaining height; sidebar is fixed-width, the view
        // pane takes the rest.
        let body = Row::new()
            .with_fill_remaining_height()
            .push(sidebar)
            .push(Box::new(Column::new().with_fill_remaining_height().push(Box::new(view_col))));

        // Footer: status (if any) else item count + path summary.
        let (count, path_str) = self.footer_text();
        let left = self
            .status
            .clone()
            .unwrap_or_else(|| format!("{count} items"));
        let footer = Row::new()
            .push(Box::new(Text::new(&left)))
            .push(Box::new(Spacer::new()))
            .push(Box::new(Text::new(&path_str)));

        // Hidden search anchor binding the CRDT path the toolbar's search
        // field writes to, so `sync` can pick the query back up.
        let search_anchor = TextInput::new(vec![])
            .with_id("files.search_anchor")
            .with_on_change_path("files.search_query");

        Box::new(
            Column::new()
                .push(toolbar)
                .push(Box::new(breadcrumb_row))
                .push(Box::new(body))
                .push(Box::new(footer))
                .push(Box::new(search_anchor)),
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
                self.current_path = path;
            }
            Err(e) => {
                self.error = Some(e);
            }
        }
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
                    Ok(_) => {
                        self.refresh();
                        self.status = Some("Created folder".to_string());
                    }
                    Err(e) => self.status = Some(format!("New folder failed: {e}")),
                }
                true
            }
            FileCommand::Rename => {
                // A full inline-rename editor is a follow-up; for now the
                // command appends " (renamed)" so the wiring is real and
                // testable. The interactive editor lands with the context
                // menu's text field.
                if let Some(p) = self.primary_path() {
                    if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                        let new = format!("{name} (renamed)");
                        match fileops::rename(&p, &new) {
                            Ok(_) => {
                                self.refresh();
                                self.status = Some("Renamed".to_string());
                            }
                            Err(e) => self.status = Some(format!("Rename failed: {e}")),
                        }
                    }
                }
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
        if path.is_dir() {
            self.navigate(path);
        } else if let Some(app) = app_for_path(&path) {
            let cwd = path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| self.current_path.clone());
            let _ = SystemRunner.spawn_arg(app, &path, &cwd);
        }
    }
}

/// Lowercase-contains filter used by the toolbar search. Returns entries in
/// their current order.
pub fn filter_entries<'a>(entries: &'a [Entry], query: &str) -> Vec<&'a Entry> {
    let q = query.to_lowercase();
    entries.iter().filter(|e| e.name.to_lowercase().contains(&q)).collect()
}

/// Thin mockable wrapper around `std::process::Command::spawn`.
pub trait CommandRunner {
    fn spawn(&mut self, program: &str, cwd: &std::path::Path) -> std::io::Result<()>;
    fn spawn_arg(&mut self, program: &str, arg: &std::path::Path, cwd: &std::path::Path) -> std::io::Result<()>;
}

pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn spawn(&mut self, program: &str, cwd: &std::path::Path) -> std::io::Result<()> {
        std::process::Command::new(program).current_dir(cwd).spawn().map(|_c| ())
    }
    fn spawn_arg(&mut self, program: &str, arg: &std::path::Path, cwd: &std::path::Path) -> std::io::Result<()> {
        std::process::Command::new(program).arg(arg).current_dir(cwd).spawn().map(|_c| ())
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
        if entry.is_dir {
            let p = entry.path.clone();
            self.navigate(p);
            return Ok(false);
        }
        let path = entry.path.clone();
        let Some(app) = app_for_path(&path) else { return Ok(false) };
        let cwd = path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| self.current_path.clone());
        runner.spawn_arg(app, &path, &cwd)?;
        Ok(true)
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
        if let Some(c) = &mut self.composed {
            c.sync(state, path);
        }
        // Pull the search query back out of the CRDT (the toolbar's search
        // field + hidden anchor write it there). A change re-filters the
        // displayed list, which is a structural change.
        use automerge::ReadDoc;
        if let Ok(Some((automerge::Value::Scalar(s), _))) =
            state.get(path, "files.search_query")
        {
            if let automerge::ScalarValue::Str(text) = s.as_ref() {
                if text.as_str() != self.search_query {
                    self.search_query = text.to_string();
                    self.selection.clear();
                    self.structure_dirty = true;
                }
            }
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let mut composed = self.build_composition();
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

        assert!(!w.open_entry(1, &mut runner).unwrap());
        assert_eq!(runner.calls.len(), 1);
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
