//! `FileExplorerWidget` — the nirios-shell file explorer's `AppWidget`.
//!
//! Task 10 lands the struct shape, the `AppWidget` metadata, the navigation
//! helpers, and a no-op `draw` pass; Tasks 11–13 build out the concrete
//! sidebar/toolbar/views composition, the selection/double-click behaviour,
//! and the real drawing.

use crate::model::{initial_path, read_dir_sync, sort_entries, Entry, SortBy, SortOrder, ViewMode};
use ui::widget::{
    Button, Column, Container, Row, ScrollAxis, Scrollable, Spacer, Text, TextInput, Widget,
};
use ui::{AppKind, AppWidget, IconDescriptor};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct FileExplorerWidget {
    current_path: PathBuf,
    home_path: Option<PathBuf>,
    entries: Vec<Entry>,
    selected_index: Option<usize>,
    search_query: String,
    error: Option<String>,
    is_loading: bool,
    last_click: Option<(usize, Instant)>,
    sort_by: SortBy,
    sort_order: SortOrder,
    view_mode: ViewMode,
    rect: vello::kurbo::Rect,
    /// Composed child widget tree, built on the first `build_layout` call
    /// from the toolbar/breadcrumbs/sidebar/view/footer builders. The five
    /// trait passes (`sync`, `build_layout`, `apply_layout`, `draw`,
    /// `handle_event`) delegate into this subtree.
    composed: Option<Box<dyn Widget>>,
}

impl Default for FileExplorerWidget {
    fn default() -> Self { Self::new() }
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
            selected_index: None,
            search_query: String::new(),
            error: None,
            is_loading: false,
            last_click: None,
            sort_by: SortBy::Name,
            sort_order: SortOrder::Ascending,
            view_mode: ViewMode::List,
            rect: vello::kurbo::Rect::ZERO,
            composed: None,
        }
    }

    /// Builds the full composed widget tree — toolbar, breadcrumbs, sidebar,
    /// the active (list/grid) view wrapped in a `Scrollable`, and the footer.
    /// Used by `Widget::build_layout` and — critically — by any caller that
    /// needs to rebuild the tree after model-state changes (future work).
    fn build_composition(&self) -> Box<dyn Widget> {
        // Toolbar (search box + back/refresh/view/sort/terminal buttons).
        let toolbar = crate::toolbar::build_toolbar(
            crate::toolbar::ToolbarSnapshot {
                search_query: &self.search_query,
                view_mode: self.view_mode,
                sort_by: self.sort_by,
                sort_order: self.sort_order,
            },
            Box::new(|| {}),
            Box::new(|| {}),
            Box::new(|_| {}),
            Box::new(|_| {}),
            Box::new(|_| {}),
            Box::new(|| {}),
            Box::new(|| {}),
        );

        // Breadcrumbs — one button per component.
        let crumbs = crate::breadcrumbs::breadcrumb_components(
            &self.current_path,
            self.home_path.as_deref(),
        );
        let mut breadcrumb_row = Row::new();
        for (i, (label, _target)) in crumbs.iter().enumerate() {
            let id = format!("files.crumb.{}", i);
            breadcrumb_row = breadcrumb_row.push(Box::new(Button::new(&id, label, None)));
        }

        // Sidebar — fixed 180 px Column laid out as tiered groups
        // (Favorites / System) matching `nirios-concept`. Each tier
        // renders an uppercase label above its rows; `files.sidebar.<i>`
        // ids stay sequential across tiers so the flat `build_shortcuts`
        // index matches the rendered button order.
        let tiers = crate::sidebar::build_tiers(self.home_path.as_deref());
        let mut sidebar_col = Column::new();
        let mut flat_index = 0usize;
        for tier in &tiers {
            sidebar_col = sidebar_col.push(Box::new(Text::new(tier.title)));
            for (label, target) in &tier.entries {
                let id = format!("files.sidebar.{}", flat_index);
                let is_active = *target == self.current_path;
                sidebar_col = sidebar_col.push(Box::new(
                    crate::sidebar_row::SidebarFolderRow::new(&id, label, target.clone())
                        .with_active(is_active),
                ));
                flat_index += 1;
            }
        }
        let sidebar: Box<dyn Widget> =
            Box::new(Container::new().with_child(Box::new(sidebar_col)));

        // Main view: list or grid (plus optional error banner/TextInput for
        // the search field is covered by the toolbar).
        let view_child: Box<dyn Widget> = match self.view_mode {
            ViewMode::List => {
                crate::views::list_view::build_list(&self.entries, self.selected_index)
            }
            ViewMode::Grid => {
                crate::views::grid_view::build_grid(&self.entries, self.selected_index)
            }
        };
        // Optional error banner surfaced above the view.
        let mut view_col = Column::new();
        if let Some(err) = self.error.as_deref() {
            view_col = view_col.push(Box::new(Text::new(err)));
        }
        view_col = view_col.push(Box::new(
            Scrollable::new(ScrollAxis::Vertical).with_child(view_child),
        ));

        // Body: sidebar | active view.
        let body = Row::new()
            .push(sidebar)
            .push(Box::new(Column::new().push(Box::new(view_col))));

        // Footer: item count + path summary.
        let (count, path_str) = self.footer_text();
        let footer = Row::new()
            .push(Box::new(Text::new(&format!("{} items", count))))
            .push(Box::new(Spacer::new()))
            .push(Box::new(Text::new(&path_str)));

        // Also keep the search-query visible in a dedicated TextInput so the
        // CRDT path is reachable (the toolbar already has one; this anchor
        // lets tests/future wiring bind to a stable CRDT path).
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

    pub fn current_path(&self) -> PathBuf { self.current_path.clone() }
    pub fn home_path(&self) -> Option<&std::path::Path> { self.home_path.as_deref() }
    pub fn entries(&self) -> &[Entry] { &self.entries }
    pub fn selected_index(&self) -> Option<usize> { self.selected_index }
    pub fn search_query(&self) -> &str { &self.search_query }
    pub fn error(&self) -> Option<&str> { self.error.as_deref() }
    pub fn is_loading(&self) -> bool { self.is_loading }
    pub fn sort_by(&self) -> SortBy { self.sort_by }
    pub fn sort_order(&self) -> SortOrder { self.sort_order }
    pub fn view_mode(&self) -> ViewMode { self.view_mode }

    pub fn navigate(&mut self, path: PathBuf) {
        self.is_loading = true;
        self.selected_index = None;
        self.error = None;
        self.last_click = None;
        match read_dir_sync(&path) {
            Ok(mut entries) => {
                sort_entries(self.sort_by, self.sort_order, &mut entries);
                self.entries = entries;
                self.current_path = path;
            }
            Err(e) => { self.error = Some(e); }
        }
        self.is_loading = false;
    }

    pub fn back(&mut self) {
        if let Some(parent) = self.current_path.parent() {
            self.navigate(parent.to_path_buf());
        }
    }

    pub fn refresh(&mut self) { self.navigate(self.current_path.clone()); }

    /// Returns the path to open on a double-click (i.e. second click within
    /// 500 ms on the same row). Single clicks update `selected_index` and
    /// return `None`.
    pub fn select(&mut self, idx: Option<usize>) -> Option<PathBuf> {
        let now = Instant::now();
        if let (Some(i), Some((pi, pt))) = (idx, self.last_click) {
            if i == pi && now.duration_since(pt) < Duration::from_millis(500) {
                let path = self.entries[i].path.clone();
                self.last_click = None;
                return Some(path);
            }
        }
        self.selected_index = idx;
        self.last_click = idx.map(|i| (i, now));
        None
    }

    pub fn set_search_query(&mut self, q: String) { self.search_query = q; }
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
    pub fn set_view_mode(&mut self, m: ViewMode) { self.view_mode = m; }

    /// Returns a borrow of the currently loaded directory entries, in their
    /// current sort order. Used by the Task 11 parity tests (and the Task 13
    /// `draw` pass) to inspect widget state without cloning.
    pub fn entries_snapshot(&self) -> &[Entry] { &self.entries }

    /// Returns the current error message, if any. Distinct from `error()`
    /// (which is also kept for symmetry) only in naming — the Task 11 plan
    /// spells it `error_message`.
    pub fn error_message(&self) -> Option<&str> { self.error.as_deref() }

    /// Footer summary — the number of entries plus the home-substituted path
    /// string that the footer displays.
    pub fn footer_text(&self) -> (usize, String) {
        (
            self.entries.len(),
            crate::model::format_path(&self.current_path, self.home_path.as_deref()),
        )
    }
}

/// Lowercase-contains filter used by the toolbar search. Returns entries in
/// their current order.
pub fn filter_entries<'a>(entries: &'a [Entry], query: &str) -> Vec<&'a Entry> {
    let q = query.to_lowercase();
    entries.iter().filter(|e| e.name.to_lowercase().contains(&q)).collect()
}

/// Thin mockable wrapper around `std::process::Command::spawn` so the
/// "Open Terminal" button (AUDIT.md T7) can be exercised under test without
/// actually spawning the terminal binary.
pub trait CommandRunner {
    /// Spawn `program` with its working directory set to `cwd`.
    fn spawn(&mut self, program: &str, cwd: &std::path::Path) -> std::io::Result<()>;
    /// Spawn `program` with a single path argument (e.g. open a document),
    /// working directory `cwd`.
    fn spawn_arg(&mut self, program: &str, arg: &std::path::Path, cwd: &std::path::Path) -> std::io::Result<()>;
}

/// Default runner used in production — invokes `std::process::Command`.
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn spawn(&mut self, program: &str, cwd: &std::path::Path) -> std::io::Result<()> {
        std::process::Command::new(program)
            .current_dir(cwd)
            .spawn()
            .map(|_child| ())
    }
    fn spawn_arg(&mut self, program: &str, arg: &std::path::Path, cwd: &std::path::Path) -> std::io::Result<()> {
        std::process::Command::new(program)
            .arg(arg)
            .current_dir(cwd)
            .spawn()
            .map(|_child| ())
    }
}

/// The app that opens `path`, by extension. Document formats — per qdoc's codec
/// registry, the OS-wide source of truth — open in the word processor.
pub fn app_for_path(path: &std::path::Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?;
    qdoc::is_supported_ext(ext).then_some("word-processor")
}

impl FileExplorerWidget {
    /// Launch a terminal rooted at the currently displayed directory. Uses
    /// the supplied `runner` so tests can assert the intended invocation
    /// without spawning a real subprocess.
    pub fn open_terminal<R: CommandRunner>(&self, runner: &mut R) -> std::io::Result<()> {
        runner.spawn("terminal", &self.current_path)
    }

    /// Activate entry `idx`: navigate into a directory, or launch the app
    /// associated with a document (the word processor). Returns whether an app
    /// was spawned. The wiring point for a row double-click.
    pub fn open_entry<R: CommandRunner>(&mut self, idx: usize, runner: &mut R) -> std::io::Result<bool> {
        let Some(entry) = self.entries.get(idx) else { return Ok(false) };
        if entry.is_dir {
            let p = entry.path.clone();
            self.navigate(p);
            return Ok(false);
        }
        let path = entry.path.clone();
        let Some(app) = app_for_path(&path) else { return Ok(false) };
        let cwd = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.current_path.clone());
        runner.spawn_arg(app, &path, &cwd)?;
        Ok(true)
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

        // A non-document does not spawn anything.
        assert!(!w.open_entry(1, &mut runner).unwrap());
        assert_eq!(runner.calls.len(), 1);
    }
}

impl Widget for FileExplorerWidget {
    fn sync(&mut self, state: &mut automerge::AutoCommit, path: &automerge::ObjId) {
        if let Some(c) = &mut self.composed {
            c.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        // Build (or rebuild) the composed child tree so subsequent passes
        // operate on a fresh snapshot of the model.
        let mut composed = self.build_composition();
        let child_node = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);

        // Wrap the child in a percent-sized container node so the five-pass
        // pipeline has a stable root owned by this widget.
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
                    c.apply_layout(
                        taffy,
                        child_node,
                        vello::kurbo::Point::new(self.rect.x0, self.rect.y0),
                    );
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
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
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
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node_id) {
                if let Some(&child_node) = children.first() {
                    c.handle_event(
                        event,
                        scale_factor,
                        modifiers,
                        state,
                        path,
                        handled,
                        focus_target,
                        child_node,
                        taffy,
                    );
                }
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &str { "FileExplorerWidget" }
    fn get_rect(&self) -> vello::kurbo::Rect { self.rect }
}

impl AppWidget for FileExplorerWidget {
    fn app_kind(&self) -> AppKind { AppKind::Files }
    fn app_title(&self) -> &str { "Files" }
    fn app_icon(&self) -> IconDescriptor { IconDescriptor::Glyph("󰉋") }
    fn permission_hint(&self) -> Option<AppKind> { Some(AppKind::Files) }
}
