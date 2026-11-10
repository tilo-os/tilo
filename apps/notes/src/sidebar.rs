//! The page tree: the toolkit list's tree rows over a snapshot of the
//! [`Workspace`], read as an outliner sidebar.
//!
//! The list owns rows, disclosure, selection, keyboard navigation, inline
//! rename, drag-and-drop, the row menu, hover row actions, scrolling,
//! indentation and semantics. This module owns only what is specific to Notes:
//! the model the list reads (every page may hold children), the page menu, and
//! what its controls mean as the notebook's own
//! [actions](crate::notebook::Notebook): every one of them is submitted
//! through the notebook's dispatcher, the one door the palette, agents,
//! history, undo and the permission check share. Which note is open is the
//! person's view of the notebook, so choosing a row is kept as a view intent
//! for the window ([`Wanted`]).
//!
//! Keyboard, while the tree holds focus (these keys otherwise belong to the
//! editor): ↑ / ↓ select, ← / → collapse / expand (or move to the parent /
//! first child), F2 renames, Delete deletes (never Backspace — the editor
//! and the tree can both hold focus), Alt-↑ / Alt-↓ reorder among siblings,
//! Tab / Shift-Tab (or Alt-→ / Alt-←) indent / outdent, and the Menu key or
//! Shift-F10 open the page menu. New notes (Ctrl+N at the top level,
//! Ctrl+Shift+N inside the selection) are handled one level up in
//! `NotesApp`, so they work whichever pane has focus.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use ui::prelude::*;
#[cfg(test)]
use ui::widget::Edits;

use ui::framework::Dispatcher;
use ui::icons::Icon;
use ui::presence::PresenceKey;
use ui::reorder::{DropPlacement, ReorderDrop};
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::{ListModel, ListRow, TreeMarks, TreeRow, VirtualList};
use ui::window_toolbar::ToolbarAction;

use crate::model::{PageId, Workspace};
use crate::notebook::{self, Notebook};
use crate::resource::Notebook as NotebookResource;

/// The notebook's door, once the window is hosted. A tree shown without one
/// changes nothing.
type SharedDoor = Rc<RefCell<Option<Dispatcher<NotebookResource>>>>;

/// What the tree asked of its window, shared with the callbacks.
type SharedWanted = Rc<RefCell<Wanted>>;

/// What the page tree's controls asked of the window besides the notebook
/// action they submitted: the view state only the window can take.
#[derive(Default)]
pub(crate) struct Wanted {
    /// Notes to show, in the order they were chosen.
    open: Vec<PageId>,
    /// The note a create or a copy this tree submitted makes, opened once
    /// that submission lands.
    made: Option<Made>,
    /// What a deletion this tree submitted says, said once it lands.
    gone: Option<Gone>,
    /// A menu's "Rename": the field opens once the menu has closed.
    rename: Option<PageId>,
    /// The row whose "more" control asked for the page menu.
    menu: Option<PageId>,
}

/// A note a submission will make: the landing revision it was submitted
/// after, so only its own answer opens it, the action that makes it, and
/// whether its title takes the keyboard first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Made {
    after: u64,
    action: &'static str,
    pub(crate) naming: bool,
}

impl Made {
    /// The landing revision this make was submitted after.
    pub(crate) fn after(self) -> u64 {
        self.after
    }

    /// The action this make submitted.
    pub(crate) fn action(self) -> &'static str {
        self.action
    }
}

/// A deletion a submission will make: the note it removes, the landing
/// revision it was submitted after, and the action it submitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Gone {
    page: PageId,
    after: u64,
    action: &'static str,
}

impl Gone {
    /// The note this deletion removes.
    pub(crate) fn page(self) -> PageId {
        self.page
    }

    /// The landing revision this deletion was submitted after.
    pub(crate) fn after(self) -> u64 {
        self.after
    }

    /// The action this deletion submitted.
    pub(crate) fn action(self) -> &'static str {
        self.action
    }
}

impl Wanted {
    /// The notes chosen since the last call.
    pub(crate) fn take_open(&mut self) -> Vec<PageId> {
        std::mem::take(&mut self.open)
    }

    /// The note to open once a submission lands, if one is waiting.
    pub(crate) fn take_made(&mut self) -> Option<Made> {
        self.made.take()
    }

    /// The deletion to say once it lands, if one is waiting.
    pub(crate) fn take_gone(&mut self) -> Option<Gone> {
        self.gone.take()
    }
}

/// Make a note under `parent` (a top-level note when none), as the sidebar's
/// "+" and the page menu's New note do; the window opens it once the
/// notebook keeps it.
fn make_note(door: &SharedDoor, wanted: &SharedWanted, parent: Option<PageId>, naming: bool) {
    let door = door.borrow();
    let Some(dispatcher) = door.as_ref() else {
        return;
    };
    let after = dispatcher.landed().revision();
    let action = if let Some(parent) = parent {
        dispatcher.submit(Notebook::NEW_CHILD.with(notebook::notebook::NewChild {
            page: notebook::page_id(parent),
            title: crate::notebook::UNTITLED.to_owned(),
        }));
        Notebook::NEW_CHILD.id()
    } else {
        dispatcher.submit(Notebook::NEW_NOTE.with(notebook::notebook::NewNote {
            title: crate::notebook::UNTITLED.to_owned(),
            parent: None,
        }));
        Notebook::NEW_NOTE.id()
    };
    wanted.borrow_mut().made = Some(Made {
        after,
        action,
        naming,
    });
}

/// Give `page` a new title.
fn rename_page(door: &SharedDoor, page: PageId, title: String) {
    if let Some(dispatcher) = door.borrow().as_ref() {
        dispatcher.submit(Notebook::RENAME.with(notebook::notebook::Rename {
            page: notebook::page_id(page),
            title,
        }));
    }
}

/// Delete `page` and everything inside it; the window says what went, once
/// the notebook keeps the deletion.
fn delete_page(door: &SharedDoor, wanted: &SharedWanted, page: PageId) {
    let door = door.borrow();
    let Some(dispatcher) = door.as_ref() else {
        return;
    };
    let after = dispatcher.landed().revision();
    dispatcher.submit(Notebook::DELETE.with(notebook::page_id(page)));
    wanted.borrow_mut().gone = Some(Gone {
        page,
        after,
        action: Notebook::DELETE.id(),
    });
}

/// Copy `page` and everything inside it; the window opens the copy once the
/// notebook keeps it.
fn duplicate_page(door: &SharedDoor, wanted: &SharedWanted, page: PageId) {
    let door = door.borrow();
    let Some(dispatcher) = door.as_ref() else {
        return;
    };
    let after = dispatcher.landed().revision();
    dispatcher.submit(Notebook::DUPLICATE.with(notebook::page_id(page)));
    wanted.borrow_mut().made = Some(Made {
        after,
        action: Notebook::DUPLICATE.id(),
        naming: false,
    });
}

/// Put `page` under `parent` (top level when none), before `before` (last
/// when none): dragging, Alt-↑ / Alt-↓, indent and outdent.
fn move_page(door: &SharedDoor, page: PageId, parent: Option<PageId>, before: Option<PageId>) {
    if let Some(dispatcher) = door.borrow().as_ref() {
        dispatcher.submit(Notebook::MOVE_NOTE.with(notebook::notebook::MoveNote {
            page: notebook::page_id(page),
            parent: parent.map(notebook::page_id),
            before: before.map(notebook::page_id),
        }));
    }
}

/// Ask the window for `effect`, as the menu's link, export and picture rows
/// do.
fn ask(door: &SharedDoor, effect: &crate::effect::NotesEffect) {
    if let Some(dispatcher) = door.borrow().as_ref() {
        crate::effect::request(dispatcher, effect);
    }
}

pub type PageTreeView = VirtualList<PageList>;

/// Row indices of [`note_menu_items`], shared by the sidebar's page menu and
/// the note menu `NotesApp` opens over the editor.
pub(crate) mod menu_row {
    pub const NEW_CHILD: usize = 0;
    pub const RENAME: usize = 1;
    pub const DUPLICATE: usize = 2;
    pub const COPY_LINK: usize = 3;
    pub const EXPORT: usize = 4;
    pub const INSERT_IMAGE: usize = 5;
    pub const DELETE: usize = 7;
}

/// One page command, declared once: the id the window's bar sends back when
/// its row is chosen, the icon set's name, the message it reads by, its row
/// in [`menu_row`], and whether it ends something.
struct NoteCommand {
    id: &'static str,
    icon: &'static str,
    message: &'static str,
    row: usize,
    destructive: bool,
}

/// The page commands, in [`menu_row`] order.
fn note_commands() -> Vec<NoteCommand> {
    vec![
        NoteCommand {
            id: "notes-menu-new-child",
            icon: "file-plus",
            message: "notes-menu-new-child",
            row: menu_row::NEW_CHILD,
            destructive: false,
        },
        NoteCommand {
            id: "notes-menu-rename",
            icon: "pencil",
            message: "notes-menu-rename",
            row: menu_row::RENAME,
            destructive: false,
        },
        NoteCommand {
            id: "notes-menu-duplicate",
            icon: "copy",
            message: "notes-menu-duplicate",
            row: menu_row::DUPLICATE,
            destructive: false,
        },
        NoteCommand {
            id: "notes-menu-copy-link",
            icon: "external-link",
            message: "notes-menu-copy-link",
            row: menu_row::COPY_LINK,
            destructive: false,
        },
        NoteCommand {
            id: "notes-menu-export",
            icon: "download",
            message: "notes-menu-export",
            row: menu_row::EXPORT,
            destructive: false,
        },
        NoteCommand {
            id: "notes-menu-insert-image",
            icon: "image",
            message: "notes-menu-insert-image",
            row: menu_row::INSERT_IMAGE,
            destructive: false,
        },
        NoteCommand {
            id: "notes-menu-delete",
            icon: "trash-2",
            message: "notes-menu-delete",
            row: menu_row::DELETE,
            destructive: true,
        },
    ]
}

/// The page actions, in [`menu_row`] order (a separator before Delete).
pub(crate) fn note_menu_items() -> Vec<MenuItem> {
    let mut items = Vec::new();
    for command in note_commands() {
        if command.row == menu_row::DELETE {
            items.push(MenuItem::separator());
        }
        let label = ui::i18n!(command.message);
        let Some(icon) = Icon::from_name(command.icon) else {
            continue;
        };
        items.push(if command.destructive {
            MenuItem::destructive_action(label, || {}).with_icon(icon)
        } else {
            MenuItem::icon_action(icon, label, || {})
        });
    }
    items
}

/// The page commands as the window's bar declares them: the same rows, in
/// the same order, behind the one "More" menu a window bar has instead of an
/// ellipsis of the app's own beside it.
pub(crate) fn note_menu_commands() -> Vec<ToolbarAction> {
    note_commands()
        .into_iter()
        .map(|command| {
            ToolbarAction::new(command.id, command.icon, ui::i18n!(command.message))
                .in_menu()
                .destructive_if(command.destructive)
        })
        .collect()
}

/// The row [`menu_row`] position of the page command `id` names, when the
/// window's bar sends one: the row the page menu holds it in.
pub(crate) fn note_command_row(id: &str) -> Option<usize> {
    note_commands()
        .into_iter()
        .find(|command| command.id == id)
        .map(|command| command.row)
}

pub(crate) fn note_context_menu() -> ContextMenu {
    let mut menu = ContextMenu::new();
    for item in note_menu_items() {
        menu.push(item);
    }
    menu
}

/// The menu on empty sidebar space: a new top-level note, or an import.
fn empty_area_items() -> Vec<MenuItem> {
    vec![
        MenuItem::icon_action(Icon::FilePlus, ui::i18n!("notes-empty-action"), || {}),
        MenuItem::icon_action(Icon::CloudDownload, ui::i18n!("notes-import-title"), || {}),
        MenuItem::icon_action(
            Icon::CloudDownload,
            ui::i18n!("notes-notion-menu-import"),
            || {},
        ),
    ]
}

/// An indexed snapshot of the page hierarchy in notebook order: every note
/// once, each before the notes inside it, so the list can read it as its rows.
/// Rebuilt whenever the workspace changes; every query the view makes is a
/// hash lookup.
#[derive(Clone, Debug, Default)]
pub struct PageList {
    rows: Vec<PageRow>,
    /// Where each note stands in `rows`.
    by_page: HashMap<PageId, usize>,
    /// Every note's children in notebook order; top-level notes under `None`.
    children: HashMap<Option<PageId>, Vec<PageId>>,
}

/// One note of [`PageList`]: what its row reads and the note it stands under.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PageRow {
    page: PageId,
    title: String,
    parent: Option<PageId>,
}

impl PageList {
    fn build(workspace: &Workspace) -> Self {
        // Every note's children in notebook order, gathered in one pass:
        // asking the workspace per note scanned every note for each one.
        let mut children: HashMap<Option<PageId>, Vec<PageId>> = HashMap::new();
        let mut titles: HashMap<PageId, (String, Option<PageId>)> = HashMap::new();
        for page in workspace.pages() {
            children.entry(page.parent).or_default().push(page.id);
            titles.insert(page.id, (page.title.clone(), page.parent));
        }
        let mut list = Self {
            rows: Vec::with_capacity(titles.len()),
            by_page: HashMap::with_capacity(titles.len()),
            children,
        };
        let mut pending: Vec<(PageId, Option<PageId>)> = list
            .children
            .get(&None)
            .map(|roots| roots.iter().rev().map(|page| (*page, None)).collect())
            .unwrap_or_default();
        while let Some((page, parent)) = pending.pop() {
            let Some((title, _)) = titles.get(&page) else {
                continue;
            };
            list.by_page.insert(page, list.rows.len());
            list.rows.push(PageRow {
                page,
                title: title.clone(),
                parent,
            });
            if let Some(nested) = list.children.get(&Some(page)) {
                pending.extend(nested.iter().rev().map(|child| (*child, Some(page))));
            }
        }
        list
    }

    fn row(&self, index: usize) -> Option<&PageRow> {
        self.rows.get(index)
    }

    /// The note standing at `index`.
    fn page(&self, index: usize) -> Option<PageId> {
        Some(self.row(index)?.page)
    }

    /// Whether the hierarchy is `other`'s: the rows, their titles and their
    /// places.
    fn same_shape(&self, other: &Self) -> bool {
        self.rows == other.rows
    }

    /// The note `page` stands under; `None` for a top-level note.
    fn parent_of(&self, page: PageId) -> Option<PageId> {
        self.by_page
            .get(&page)
            .and_then(|at| self.rows.get(*at))
            .and_then(|row| row.parent)
    }

    /// The notes in `parent` (top-level notes when none), in notebook order.
    fn siblings(&self, parent: Option<PageId>) -> &[PageId] {
        self.children.get(&parent).map_or(&[], Vec::as_slice)
    }

    /// The canonical `(new_parent, before)` a validated drop resolves to.
    fn placement(&self, dropped: &ReorderDrop<PageId>) -> (Option<PageId>, Option<PageId>) {
        let target = dropped.target.key;
        let parent = self.parent_of(target);
        match dropped.target.placement {
            DropPlacement::Inside => (Some(target), None),
            DropPlacement::Before => (parent, Some(target)),
            DropPlacement::After => {
                let before = self
                    .siblings(parent)
                    .iter()
                    .skip_while(|page| **page != target)
                    .skip(1)
                    .find(|page| **page != dropped.source)
                    .copied();
                (parent, before)
            }
        }
    }
}

impl ListModel for PageList {
    type Key = PageId;

    fn len(&self) -> usize {
        self.rows.len()
    }

    fn key_at(&self, index: usize) -> Option<PageId> {
        self.page(index)
    }

    fn index_of(&self, key: &PageId) -> Option<usize> {
        self.by_page.get(key).copied()
    }

    fn label(&self, index: usize) -> Option<String> {
        Some(self.row(index)?.title.clone())
    }

    /// Each note stands where the notebook puts it: under the note it is
    /// inside.
    fn tree_at(&self, index: usize) -> Option<TreeRow<PageId>> {
        Some(TreeRow {
            parent: self.row(index)?.parent,
            depth: None,
        })
    }

    /// Any note holds others: dropping onto one nests into it.
    fn accepts_children(&self, index: usize) -> bool {
        self.row(index).is_some()
    }

    /// A note with notes inside it shows the disclosure that folds them away.
    fn has_subtasks(&self, index: usize) -> bool {
        self.row(index)
            .is_some_and(|row| !self.siblings(Some(row.page)).is_empty())
    }
}

/// The notes sidebar: the page tree, the door its controls submit through,
/// and what it asked of its window. Dereferences to the [`PageTreeView`] it
/// hosts.
pub struct PageSidebar {
    tree: PageTreeView,
    /// Which notes have their notes folded away, kept by the list as it mounts
    /// rows so a fold survives every rebuild of the snapshot.
    marks: Rc<TreeMarks<PageId>>,
    door: SharedDoor,
    wanted: SharedWanted,
    /// The selection seen by the previous sync. When it changes, its
    /// ancestors unfold so the selected row is on screen (a `[[link]]`
    /// jump into a nested page).
    last_selection: Option<PageId>,
}

impl Deref for PageSidebar {
    type Target = PageTreeView;

    fn deref(&self) -> &Self::Target {
        &self.tree
    }
}

impl DerefMut for PageSidebar {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.tree
    }
}

impl Default for PageSidebar {
    fn default() -> Self {
        Self::new()
    }
}

impl PageSidebar {
    #[allow(
        clippy::too_many_lines,
        reason = "one builder chain wires every tree signal to its page command"
    )]
    pub fn new() -> Self {
        crate::metadata::register_messages();
        let door = SharedDoor::default();
        let wanted = SharedWanted::default();
        let marks = Rc::new(TreeMarks::default());
        let on_reorder = Rc::clone(&door);
        let on_rename = Rc::clone(&door);
        let on_delete = (Rc::clone(&door), Rc::clone(&wanted));
        let on_open = Rc::clone(&wanted);
        let on_menu = (Rc::clone(&door), Rc::clone(&wanted));
        let rows = (Rc::clone(&door), Rc::clone(&wanted));
        let depths = Rc::clone(&marks);
        let tree = VirtualList::new(
            PageList::build(&Workspace::default()),
            ui::tokens::Metrics::BASE.row_single,
            move |model, index, page| {
                let Some(row) = model.row(index) else {
                    return Box::new(ListRow::new(format!("note-{page}"), "")) as Element;
                };
                let title = row.title.as_str();
                let page = *page;
                let (plus_door, plus_wanted) = (Rc::clone(&rows.0), Rc::clone(&rows.1));
                let more_wanted = Rc::clone(&rows.1);
                let row = ListRow::new(format!("note-{page}"), title)
                    .depth(depths.depth(&page))
                    .action(
                        Icon::Plus,
                        format!("{} {title}", ui::i18n!("notes-a11y-new-child-under")),
                        move || make_note(&plus_door, &plus_wanted, Some(page), false),
                    )
                    .action(
                        Icon::MoreHorizontal,
                        format!("{} {title}", ui::i18n!("notes-a11y-more-actions-for")),
                        // The list opens the page menu on the row once it has
                        // the request: a row's control cannot reach it.
                        move || more_wanted.borrow_mut().menu = Some(page),
                    );
                Box::new(row)
            },
        )
        .with_accessible_name(ui::i18n!("notes-title"))
        .with_tree(Rc::clone(&marks))
        .with_indent_guides()
        .filling()
        .selection_follows_focus()
        .rename_on_double_click()
        .tab_indents(true)
        .with_row_presence(|_, _, page| {
            // Remote peers viewing a note glow its row.
            Some(PresenceKey::Tab {
                id: page.to_string(),
            })
        })
        .with_semantic_key(|page| format!("notes:page:{page}"))
        .with_context_menu(
            |_, target| match target {
                Some(_) => note_menu_items(),
                None => empty_area_items(),
            },
            move |target, index| {
                let (door, wanted) = &on_menu;
                match target {
                    None => match index {
                        0 => make_note(door, wanted, None, false),
                        1 => ask(door, &crate::effect::NotesEffect::ImportMarkdownFile),
                        2 => ask(door, &crate::effect::NotesEffect::ImportNotionExport),
                        _ => {}
                    },
                    Some(page) => match index {
                        menu_row::NEW_CHILD => make_note(door, wanted, Some(page), false),
                        menu_row::RENAME => wanted.borrow_mut().rename = Some(page),
                        menu_row::DUPLICATE => duplicate_page(door, wanted, page),
                        menu_row::COPY_LINK => {
                            ask(door, &crate::effect::NotesEffect::CopyLink { page });
                        }
                        menu_row::EXPORT => {
                            ask(door, &crate::effect::NotesEffect::ExportMarkdown { page });
                        }
                        menu_row::INSERT_IMAGE => {
                            ask(door, &crate::effect::NotesEffect::InsertImage { page });
                        }
                        menu_row::DELETE => delete_page(door, wanted, page),
                        _ => {}
                    },
                }
            },
        )
        .with_reorder(
            |_, _, _| true,
            move |model: &mut PageList, dropped: ReorderDrop<PageId>| {
                let (parent, before) = model.placement(&dropped);
                move_page(&on_reorder, dropped.source, parent, before);
                true
            },
        )
        .on_view("page_tree", move |page| {
            on_open.borrow_mut().open.push(page);
        })
        .on_rename(move |page, new_title| {
            rename_page(&on_rename, page, new_title);
        })
        .on_delete(move |page| {
            let (door, wanted) = &on_delete;
            delete_page(door, wanted, page);
        });
        Self {
            tree,
            marks,
            door,
            wanted,
            last_selection: None,
        }
    }

    /// Submit through `dispatcher` from now on: every control in the tree
    /// reaches the notebook by the one door its actions are published at.
    pub(crate) fn attach_notebook(&self, dispatcher: Dispatcher<NotebookResource>) {
        *self.door.borrow_mut() = Some(dispatcher);
    }

    /// Make a note, as the "+" and New note controls do: inside `parent`
    /// when given, at the top level otherwise. `naming`: the note opens with
    /// its title taking the keyboard.
    pub(crate) fn new_note(&self, parent: Option<PageId>, naming: bool) {
        make_note(&self.door, &self.wanted, parent, naming);
    }

    /// Rename `page` to `title`, as the inline field and the title band do.
    pub(crate) fn rename(&self, page: PageId, title: String) {
        rename_page(&self.door, page, title);
    }

    /// Copy `page` and everything inside it, as the menu's Duplicate does.
    pub(crate) fn duplicate(&self, page: PageId) {
        duplicate_page(&self.door, &self.wanted, page);
    }

    /// Delete `page` and everything inside it, as Delete does.
    pub(crate) fn delete(&self, page: PageId) {
        delete_page(&self.door, &self.wanted, page);
    }

    /// Ask the window for `effect`, as the menu's link, export and picture
    /// rows do.
    pub(crate) fn request(&self, effect: &crate::effect::NotesEffect) {
        ask(&self.door, effect);
    }

    /// The notes chosen since the last call.
    pub(crate) fn take_open(&self) -> Vec<PageId> {
        self.wanted.borrow_mut().take_open()
    }

    /// The note to open once a submission lands, if one is waiting.
    pub(crate) fn take_made(&self) -> Option<Made> {
        self.wanted.borrow_mut().take_made()
    }

    /// The deletion to say once it lands, if one is waiting.
    pub(crate) fn take_gone(&self) -> Option<Gone> {
        self.wanted.borrow_mut().take_gone()
    }

    /// Open the inline rename field a menu's "Rename" asked for, once the
    /// menu has closed.
    pub fn open_menu_rename(&mut self) {
        if self.tree.context_menu_is_open() {
            return;
        }
        let Some(page) = self.wanted.borrow_mut().rename else {
            return;
        };
        let mut wanted = self.wanted.borrow_mut();
        wanted.rename = None;
        drop(wanted);
        self.tree.begin_rename(&page);
    }

    /// Open the page menu on the row whose "more" control asked for it, where
    /// its control stands.
    pub fn open_requested_menu(&mut self) {
        let Some(page) = self.wanted.borrow_mut().menu.take() else {
            return;
        };
        let at = self
            .tree
            .row_action_rect(&page, MORE_ACTION)
            .or_else(|| self.tree.row_rect(&page))
            .map_or_else(|| self.tree.get_rect().center(), |rect| rect.center());
        self.tree.open_row_menu(page, at);
    }

    /// The hosted tree, as a widget.
    pub fn tree(&self) -> &PageTreeView {
        &self.tree
    }

    pub fn tree_mut(&mut self) -> &mut PageTreeView {
        &mut self.tree
    }

    /// Fold `page`'s notes away, or show them again.
    pub fn set_expanded(&mut self, page: PageId, expanded: bool) {
        self.tree.fold_row(&page, !expanded);
    }

    /// Whether `page`'s notes show.
    pub fn is_expanded(&self, page: PageId) -> bool {
        !self.tree.row_folded(&page)
    }

    /// The note the tree has selected.
    pub fn selected(&self) -> Option<PageId> {
        self.tree.focused_key().copied()
    }

    /// Mirror the workspace into the tree. Cheap when nothing changed; a
    /// changed hierarchy rebuilds the snapshot and the list reconciles it
    /// against the notes' stable keys.
    pub fn sync_from_workspace(&mut self, workspace: &Workspace) {
        let next = PageList::build(workspace);
        if !next.same_shape(self.tree.model()) {
            self.tree.update_model(|model| *model = next);
        }
        let mut reveal = None;
        let selected = workspace.selected();
        if selected != self.last_selection {
            let mut ancestor = selected
                .and_then(|id| workspace.get(id))
                .and_then(|p| p.parent);
            while let Some(page) = ancestor {
                self.marks.fold(&page, false);
                ancestor = workspace.get(page).and_then(|p| p.parent);
            }
            reveal = selected;
            self.last_selection = selected;
        }
        match selected {
            Some(page) => self.tree.selection_mut().select_only(page),
            None => self.tree.selection_mut().clear(),
        }
        if let Some(page) = reveal {
            self.tree.focus_row(page);
            self.tree.reveal(&page);
        }
    }

    /// Expanded notes, for the application's persisted local view.
    pub fn expanded_pages(&self) -> Vec<PageId> {
        let model = self.tree.model();
        let mut pages: Vec<PageId> = (0..model.len())
            .filter(|index| model.has_subtasks(*index))
            .filter_map(|index| model.page(index))
            .filter(|page| !self.tree.row_folded(page))
            .collect();
        pages.sort_unstable();
        pages
    }

    pub fn restore_expanded_pages(&mut self, pages: &[PageId], workspace: &Workspace) {
        let next = PageList::build(workspace);
        let shown: HashSet<PageId> = pages.iter().copied().collect();
        let folded: HashSet<PageId> = (0..next.len())
            .filter(|index| next.has_subtasks(*index))
            .filter_map(|index| next.page(index))
            .filter(|page| !shown.contains(page))
            .collect();
        self.marks.restore_folded(folded);
        self.tree.update_model(|model| *model = next);
        // Re-run the ancestor revelation even for an unchanged selection.
        self.last_selection = None;
        self.sync_from_workspace(workspace);
    }

    /// Open the inline rename field on `page`.
    pub fn begin_rename(&mut self, page: PageId) -> bool {
        self.tree.begin_rename(&page)
    }

    /// True while an inline rename field is open.
    pub fn is_renaming(&self) -> bool {
        self.tree.renaming().is_some()
    }

    /// Whether the page menu is open. Read by the parent app so it routes
    /// keyboard navigation to the menu (and away from the editor) while it
    /// is up.
    pub fn menu_is_open(&self) -> bool {
        self.tree.context_menu_is_open()
    }

    /// Visible row `index` as `(page, title, depth)`.
    fn row(&self, index: usize) -> Option<(PageId, &str, usize)> {
        let page = self.tree.visible_key(index)?;
        let model = self.tree.model();
        let title = model.row(model.index_of(&page)?)?.title.as_str();
        Some((page, title, self.tree.row_depth(&page).try_into().ok()?))
    }

    #[cfg(test)]
    pub fn visible_row_count(&self) -> usize {
        self.tree.visible_len()
    }

    /// Test hook: the centre of visible row `index`, for click targeting
    /// through the real event path.
    #[doc(hidden)]
    pub fn row_center_for_test(&self, index: usize) -> Option<vello::kurbo::Point> {
        let (page, _, _) = self.row(index)?;
        self.tree.row_rect(&page).map(|rect| rect.center())
    }

    /// Test hook: the point `from` the top of visible row `index`, over its
    /// height `fraction` of the way down, for targeting a drop there.
    #[doc(hidden)]
    pub fn row_point_for_test(&self, index: usize, fraction: f64) -> Option<vello::kurbo::Point> {
        let (page, _, _) = self.row(index)?;
        let rect = self.tree.row_rect(&page)?;
        Some(vello::kurbo::Point::new(
            rect.center().x,
            rect.y0 + rect.height() * fraction,
        ))
    }

    /// Test hook: the centre of row `index`'s affordance `action`, for click
    /// targeting through the real event path.
    #[doc(hidden)]
    pub fn row_action_center_for_test(
        &self,
        index: usize,
        action: usize,
    ) -> Option<vello::kurbo::Point> {
        let (page, _, _) = self.row(index)?;
        self.tree
            .row_action_rect(&page, action)
            .map(|rect| rect.center())
    }

    /// Test hook: the x of the disclosure column's centre on row `index`.
    #[doc(hidden)]
    pub fn row_disclosure_x_for_test(&self, index: usize) -> Option<f64> {
        let (page, _, _) = self.row(index)?;
        self.tree.disclosure_rect(&page).map(|rect| rect.center().x)
    }

    /// Test hook: the depth of visible row `index` (0 = top level).
    #[doc(hidden)]
    pub fn row_depth_for_test(&self, index: usize) -> Option<usize> {
        self.row(index).map(|(_, _, depth)| depth)
    }

    /// Test hook: the title of visible row `index`.
    #[doc(hidden)]
    pub fn row_title_for_test(&self, index: usize) -> Option<&str> {
        self.row(index).map(|(_, title, _)| title)
    }

    /// Test hook: whether visible row `index` has children (a disclosure).
    #[doc(hidden)]
    pub fn row_has_children_for_test(&self, index: usize) -> Option<bool> {
        let (page, _, _) = self.row(index)?;
        Some(self.tree.row_is_branch(&page))
    }

    /// Test hook: whether the open rename field is plain chrome (styled as
    /// the row). `None` when no rename is open.
    #[doc(hidden)]
    pub fn rename_is_plain_for_test(&self) -> Option<bool> {
        self.tree
            .rename_field()
            .map(ui::widget::TextInput::is_plain)
    }

    /// Test hook: the text the open rename field holds. `None` when no
    /// rename is open.
    #[doc(hidden)]
    pub fn rename_draft_for_test(&self) -> Option<String> {
        self.tree
            .rename_field()
            .map(|field| field.value().to_owned())
    }

    /// Test hook: whether a drag past the threshold is in flight.
    #[doc(hidden)]
    pub fn drag_active_for_test(&self) -> bool {
        self.tree.is_dragging()
    }

    /// Test hook: the drop indicator the tree paints for the current drag
    /// (an insertion line, or the outline of the row it would nest into).
    #[doc(hidden)]
    pub fn drop_indicator_for_test(&self) -> Option<vello::kurbo::Rect> {
        self.tree.reorder_preview()
    }

    /// Test hook: whether the page menu is open.
    #[doc(hidden)]
    pub fn context_menu_open_for_test(&self) -> bool {
        self.tree.context_menu_is_open()
    }
}

/// The index of the "more" row action, in the order every row shows its
/// controls.
pub const MORE_ACTION: usize = 1;

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{AutoCommit, ObjId};
    use taffy::TaffyTree;
    use ui::Widget;
    use ui::text_render::TextRenderer;
    use ui::theme::Theme;
    use vello::kurbo::{Point, Rect};
    use winit::event::{ElementState, Modifiers, WindowEvent};
    use winit::keyboard::NamedKey;

    /// One declared command drives both of the note's menus: the page menu's
    /// rows, in [`menu_row`] order with a separator before Delete, and the
    /// rows the window's bar offers in its one "More" menu.
    #[test]
    fn the_bars_rows_are_the_page_menus_rows() {
        let commands = note_commands();
        assert_eq!(
            commands
                .iter()
                .map(|command| command.row)
                .collect::<Vec<_>>(),
            vec![
                menu_row::NEW_CHILD,
                menu_row::RENAME,
                menu_row::DUPLICATE,
                menu_row::COPY_LINK,
                menu_row::EXPORT,
                menu_row::INSERT_IMAGE,
                menu_row::DELETE,
            ]
        );
        for command in &commands {
            assert_eq!(note_command_row(command.id), Some(command.row));
        }
        assert_eq!(
            note_menu_items().len(),
            commands.len() + 1,
            "the page menu holds the same rows and one separator before Delete"
        );
        let declared = note_menu_commands();
        assert_eq!(declared.len(), commands.len());
        assert!(
            declared.iter().all(|action| action.menu),
            "every one of them stands in the window's More menu, not in its row"
        );
        assert_eq!(
            declared
                .iter()
                .map(|action| action.name.clone())
                .collect::<Vec<_>>(),
            commands
                .iter()
                .map(|command| ui::i18n!(command.message))
                .collect::<Vec<_>>()
        );
    }

    /// Lay the sidebar out at `rect` the way the drawer host does, over the frames a
    /// host takes to settle it: a row measures itself only once it has the theme.
    fn place(sidebar: &mut PageSidebar, rect: Rect, text_scale: f32) {
        let theme = Theme::dark().with_text_scale(text_scale);
        sidebar.set_viewport_height(ui::num::f32_of_f64(rect.height()));
        let mut text = TextRenderer::new();
        for _ in 0..2 {
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map = HashMap::new();
            let node = sidebar.build_layout(&mut taffy, &theme, &mut node_map);
            let mut bindings = ui::bound_dispatch::BoundNodeMap::new();
            // SAFETY: `node_map` points at the widgets the live `sidebar` holds, under
            // the layout just built.
            unsafe {
                bindings.rebuild(&**sidebar, &taffy, node, &node_map);
                ui::widget::compute_widget_layout(
                    &mut taffy,
                    node,
                    taffy::Size {
                        width: ui::taffy::AvailableSpace::Definite(ui::num::f32_of_f64(
                            rect.width(),
                        )),
                        height: ui::taffy::AvailableSpace::Definite(ui::num::f32_of_f64(
                            rect.height(),
                        )),
                    },
                    &node_map,
                    &bindings,
                    &mut text,
                )
            }
            .expect("the sidebar lays out");
            sidebar.apply_layout(&taffy, node, rect.origin());
            let mut scene = vello::Scene::new();
            sidebar.draw(
                &mut scene,
                vello::kurbo::Affine::IDENTITY,
                &mut text,
                &theme,
                &ui::presence::PresenceCtx::empty(),
            );
        }
    }

    fn event(sidebar: &mut PageSidebar, event: &WindowEvent) -> bool {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        let mut handled = false;
        let mut focus = None;
        sidebar.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: Edits::lend(&mut AutoCommit::new()),
                path: &ObjId::Root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    fn moved(point: Point) -> WindowEvent {
        ui::synthetic_input::cursor_moved(point.x, point.y)
    }

    fn button(state: ElementState) -> WindowEvent {
        ui::synthetic_input::mouse_input(state, ui::winit::event::MouseButton::Left)
    }

    /// The tree as the window hosts it: its controls submit through a
    /// detached notebook door, whose queue the test reads.
    fn with_door() -> (PageSidebar, Dispatcher<NotebookResource>) {
        let tree = PageSidebar::new();
        let door = Dispatcher::detached("so.tilo.notes", "test-author");
        tree.attach_notebook(door.clone());
        (tree, door)
    }

    /// The actions `door` was handed since the last call.
    fn asked(door: &Dispatcher<NotebookResource>) -> Vec<crate::actions::Action> {
        door.take_asked().expect("what was asked builds")
    }

    use ui::synthetic_input::named_key;

    #[test]
    fn deleting_a_note_is_the_one_danger_row_and_sits_apart_at_the_end() {
        let items = note_menu_items();
        let danger: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                matches!(
                    item,
                    MenuItem::Action {
                        destructive: true,
                        ..
                    }
                )
            })
            .map(|(index, _)| index)
            .collect();
        assert_eq!(danger, vec![menu_row::DELETE]);
        assert_eq!(menu_row::DELETE, items.len() - 1);
        assert!(
            matches!(items.get(menu_row::DELETE - 1), Some(MenuItem::Separator)),
            "a separator sets the destructive row apart"
        );
    }

    #[test]
    fn heading_resolves_before_any_other_notes_widget_registers_the_catalog() {
        // The catalog is per thread; a fresh one has seen no Notes widget yet.
        let (name, catalog) = std::thread::spawn(|| {
            let tree = PageSidebar::new();
            let name = Widget::accessibility(tree.tree())
                .and_then(|node| node.name)
                .expect("the page tree is named by its heading");
            (name, ui::i18n!("notes-title"))
        })
        .join()
        .expect("sidebar thread");
        assert_ne!(name, "notes-title");
        assert_eq!(name, catalog);
    }

    #[test]
    fn sidebar_rows_track_pages_and_scale_at_high_text_on_every_preset() {
        eprintln!("phase sync_from_workspace_flattens_to_visible_rows");
        {
            let mut ws = Workspace::new();
            let root = ws.create_page(None);
            let child = ws.create_page(Some(root));
            let _grand = ws.create_page(Some(child));
            let mut tree = PageSidebar::new();
            tree.sync_from_workspace(&ws);
            place(&mut tree, Rect::new(0.0, 0.0, 240.0, 400.0), 1.0);
            // Branches with children auto-expand on first render, so the full
            // nesting is on screen (root -> child -> grandchild).
            assert_eq!(tree.visible_row_count(), 3);
            // Collapsing the root hides all its descendants (only the root remains).
            let key = root;
            tree.set_expanded(key, false);
            tree.sync_from_workspace(&ws);
            place(&mut tree, Rect::new(0.0, 0.0, 240.0, 400.0), 1.0);
            assert_eq!(tree.visible_row_count(), 1);
            assert_eq!(tree.expanded_pages(), {
                let mut open = vec![child];
                open.sort_unstable();
                open
            });
        }

        eprintln!("phase keys_stay_attached_to_pages_across_rebuilds");
        {
            let mut ws = Workspace::new();
            ws.create_page(None);
            let b = ws.create_page(None);
            let mut tree = PageSidebar::new();
            tree.sync_from_workspace(&ws);
            // `b` moves before `a`.
            let moved = ws.data.pages.remove(1);
            ws.data.pages.insert(0, moved);
            tree.sync_from_workspace(&ws);
            assert_eq!(tree.row(0).map(|(key, _, _)| key), Some(b));
        }

        eprintln!("phase high_text_rows_and_labels_keep_distinct_scaled_geometry");
        {
            let mut ws = Workspace::new();
            let parent = ws.create_page(None);
            ws.create_page(Some(parent));
            let mut tree = PageSidebar::new();
            tree.sync_from_workspace(&ws);
            place(&mut tree, Rect::new(0.0, 0.0, 528.0, 600.0), 2.0);
            let first = tree
                .row(0)
                .and_then(|(key, _, _)| tree.row_rect(&key))
                .unwrap();
            let second = tree
                .row(1)
                .and_then(|(key, _, _)| tree.row_rect(&key))
                .unwrap();
            let disclosure = tree
                .row(0)
                .and_then(|(key, _, _)| tree.disclosure_rect(&key))
                .unwrap();

            assert!(first.y1 <= second.y0, "adjacent rows must not overlap");
            assert!(first.height() > 30.0, "rows grow with the text");
            assert!(disclosure.x1 < first.x1 - 4.0, "the label keeps room");
        }

        eprintln!("phase page_tree_semantics_follow_visible_rows_and_their_hit_rects");
        {
            let mut ws = Workspace::new();
            let root = ws.create_page(None);
            ws.data.pages[0].title = "Project plan".into();
            let (mut tree, _door) = with_door();
            tree.sync_from_workspace(&ws);
            place(&mut tree, Rect::new(0.0, 0.0, 200.0, 300.0), 1.0);

            let node = Widget::accessibility(tree.tree()).expect("page tree is semantic");
            assert_eq!(node.role, ui::a11y::Role::Tree);
            assert_eq!(
                node.name.as_deref(),
                Some(ui::i18n!("notes-title").as_str())
            );
            assert_eq!(node.children.len(), 1, "the tree exposes its rows only");
            assert_eq!(node.children[0].name.as_deref(), Some("Project plan"));
            let key = root;
            let rect = tree.row_rect(&key).expect("the row is laid out");
            let bounds = node.children[0].bounds.expect("the row says where it is");
            assert!(
                (bounds.y1 - bounds.y0 - rect.height()).abs() < 0.01,
                "the exposed row is as tall as it is drawn"
            );
            assert!(
                rect.x0 <= bounds.x0 && bounds.x1 <= rect.x1,
                "and stands inside the row's column: {bounds:?} in {rect:?}"
            );
            tree.selection_mut().clear();
            assert!(Widget::focus_synthetic_accessibility(tree.tree_mut(), &[0]));
            assert_eq!(
                tree.selected(),
                Some(key),
                "AT focus selects the visible page"
            );
            assert_eq!(
                tree.take_open().as_slice(),
                [root],
                "choosing a row asks the window to open its note"
            );
        }
    }

    #[test]
    fn sidebar_drag_and_scroll_reveal_hidden_targets() {
        use ui::time::testing::with_monotonic_time;
        eprintln!("phase dragging_over_collapsed_branch_springs_it_open_after_dwell");
        with_monotonic_time(ui::time::now(), || {
            let mut ws = Workspace::new();
            let target = ws.create_page(None);
            let _child = ws.create_page(Some(target));
            let _dragged = ws.create_page(None);
            let mut tree = PageSidebar::new();
            tree.sync_from_workspace(&ws);
            let target_key = target;
            tree.set_expanded(target_key, false);
            tree.sync_from_workspace(&ws);
            place(&mut tree, Rect::new(0.0, 0.0, 220.0, 300.0), 1.0);

            let from = tree.row_center_for_test(1).unwrap();
            let over = tree.row_center_for_test(0).unwrap();
            event(&mut tree, &moved(from));
            event(&mut tree, &button(ElementState::Pressed));
            event(&mut tree, &moved(over));
            assert!(tree.is_dragging());
            assert_eq!(
                tree.reorder_target()
                    .map(|target| (target.key, target.placement)),
                Some((target_key, DropPlacement::Inside))
            );
            let doc = AutoCommit::new();
            tree.update(&doc, &ObjId::Root);
            assert!(!tree.is_expanded(target_key), "no spring before the dwell");
            let dwelt = ui::time::now() + std::time::Duration::from_millis(700);
            with_monotonic_time(dwelt, || tree.update(&doc, &ObjId::Root));
            assert!(tree.is_expanded(target_key), "the dwell opens the branch");
        });

        eprintln!("phase dragging_in_bottom_edge_band_autoscrolls_toward_hidden_targets");
        with_monotonic_time(ui::time::now(), || {
            let mut workspace = Workspace::new();
            for _ in 0..30 {
                workspace.create_page(None);
            }
            let mut tree = PageSidebar::new();
            tree.sync_from_workspace(&workspace);
            place(&mut tree, Rect::new(0.0, 0.0, 240.0, 300.0), 1.0);
            let start = tree.row_center_for_test(0).unwrap();
            event(&mut tree, &moved(start));
            event(&mut tree, &button(ElementState::Pressed));
            event(&mut tree, &moved(Point::new(start.x, 299.0)));
            assert!(tree.is_dragging());
            let doc = AutoCommit::new();
            tree.update(&doc, &ObjId::Root);
            let later = ui::time::now() + std::time::Duration::from_millis(30);
            with_monotonic_time(later, || tree.update(&doc, &ObjId::Root));
            assert!(
                tree.scroll_offset() > 0.0,
                "edge dragging reveals lower drop targets without ending the gesture"
            );
        });

        eprintln!("phase large_tree_scroll_is_clamped_and_keyboard_reveals_the_selection");
        {
            let mut ws = Workspace::new();
            let pages: Vec<_> = (0..30).map(|_| ws.create_page(None)).collect();
            let mut tree = PageSidebar::new();
            tree.sync_from_workspace(&ws);
            place(&mut tree, Rect::new(0.0, 0.0, 240.0, 300.0), 1.0);
            let max = tree.max_scroll_offset();
            assert!(max > 0.0);
            tree.set_scroll_offset(90.0);
            assert!((tree.scroll_offset() - 90.0).abs() < f64::EPSILON);
            tree.set_scroll_offset(10_000.0);
            assert!((tree.scroll_offset() - max).abs() < f64::EPSILON);
            tree.set_scroll_offset(-10.0);
            assert!(tree.scroll_offset().abs() < f64::EPSILON);

            ws.set_selected(pages.last().copied());
            tree.sync_from_workspace(&ws);
            assert!(
                tree.scroll_offset() > 0.0,
                "a selection below the viewport is scrolled into view"
            );
            ws.set_selected(pages.first().copied());
            tree.sync_from_workspace(&ws);
            assert!(tree.scroll_offset().abs() < f64::EPSILON);
        }
    }

    #[test]
    fn sidebar_rename_submits_the_notebooks_rename_once_committed() {
        eprintln!("phase begin_and_commit_rename_submits_one_rename");
        {
            let mut ws = Workspace::new();
            let id = ws.create_page(None);
            ws.data.pages[0].title = "Untitled".into();
            let (mut tree, door) = with_door();
            tree.sync_from_workspace(&ws);
            assert!(tree.begin_rename(id));
            tree.commit_rename();
            let asked = asked(&door);
            assert_eq!(asked.len(), 1);
            match &asked[0].operation {
                crate::notebook::NotebookAction::Rename(rename) => {
                    assert_eq!(rename.page, notebook::page_id(id));
                    assert_eq!(rename.title, "Untitled");
                }
                other => panic!("expected the notebook's rename, got {other:?}"),
            }
            assert!(
                tree.take_open().is_empty(),
                "a rename moves no note into the window"
            );
        }

        eprintln!("phase cancel_rename_submits_nothing");
        {
            let mut ws = Workspace::new();
            let id = ws.create_page(None);
            let (mut tree, door) = with_door();
            tree.sync_from_workspace(&ws);
            assert!(tree.begin_rename(id));
            event(&mut tree, &named_key(NamedKey::Escape));
            assert!(!tree.is_renaming());
            assert!(asked(&door).is_empty());
            assert!(tree.take_open().is_empty());
        }

        eprintln!("phase menu_rename_opens_the_field_once_the_menu_has_closed");
        {
            let mut workspace = Workspace::new();
            let page = workspace.create_page(None);
            let (mut tree, _door) = with_door();
            tree.sync_from_workspace(&workspace);
            let key = page;
            tree.open_context_menu(Some(&key), Point::ZERO);
            let target = format!("menu:context-menu:item:{}", menu_row::RENAME);
            assert!(Widget::perform_semantic_action(
                tree.tree_mut(),
                &target,
                &ui::a11y::Action::Activate,
                &mut Edits::lend(&mut AutoCommit::new()),
                &ObjId::Root
            ));
            assert!(
                !tree.is_renaming(),
                "the field waits while the menu is still up"
            );
            tree.open_menu_rename();
            assert!(tree.is_renaming());
            let before = tree.rename_draft_for_test().unwrap_or_default();
            event(&mut tree, &ui::synthetic_input::char_key("H"));
            assert_eq!(
                tree.rename_draft_for_test().as_deref(),
                Some(format!("{before}H").as_str()),
                "the fresh field takes the typed key"
            );
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn sidebar_page_menu_and_row_actions_submit_the_notebooks_actions() {
        eprintln!("phase hover_row_actions_create_a_child_or_open_the_existing_note_menu");
        {
            let mut workspace = Workspace::new();
            let page = workspace.create_page(None);
            let (mut tree, door) = with_door();
            tree.sync_from_workspace(&workspace);
            place(&mut tree, Rect::new(0.0, 0.0, 240.0, 200.0), 1.0);
            let key = page;
            // The row's controls show under the pointer, as they do for a person.
            let row = tree.row_rect(&key).unwrap();
            event(&mut tree, &moved(row.center()));
            let child = tree.row_action_rect(&key, 0).unwrap();
            let more = tree.row_action_rect(&key, 1).unwrap();

            event(&mut tree, &moved(child.center()));
            assert!(event(&mut tree, &button(ElementState::Pressed)));
            event(&mut tree, &button(ElementState::Released));
            let created = asked(&door);
            assert_eq!(created.len(), 1);
            match &created[0].operation {
                crate::notebook::NotebookAction::NewChild(new_child) => {
                    assert_eq!(new_child.page, notebook::page_id(page));
                }
                other => panic!("expected the notebook's new-child, got {other:?}"),
            }
            let made = tree.take_made().expect("the made note opens once it lands");
            assert!(!made.naming, "a row's plus leaves naming to the tree");

            event(&mut tree, &moved(more.center()));
            assert!(event(&mut tree, &button(ElementState::Pressed)));
            assert!(event(&mut tree, &button(ElementState::Released)));
            tree.open_requested_menu();
            assert!(tree.context_menu_open_for_test());
            assert_eq!(
                tree.take_open().as_slice(),
                [page],
                "the menu's note is the window's note"
            );
        }

        eprintln!("phase page_menu_semantics_submit_the_same_page_action");
        {
            let mut workspace = Workspace::new();
            let page = workspace.create_page(None);
            let (mut tree, door) = with_door();
            tree.sync_from_workspace(&workspace);
            let key = page;
            tree.open_context_menu(Some(&key), Point::ZERO);
            let node = Widget::accessibility(tree.tree()).unwrap();
            let menu = node
                .children
                .iter()
                .find(|node| node.role == ui::a11y::Role::Menu)
                .unwrap();
            assert_eq!(menu.state.expanded, Some(true));
            assert_eq!(
                menu.children
                    .iter()
                    .map(|item| item.name.clone().unwrap())
                    .collect::<Vec<_>>(),
                vec![
                    ui::i18n!("notes-menu-new-child"),
                    ui::i18n!("notes-menu-rename"),
                    ui::i18n!("notes-menu-duplicate"),
                    ui::i18n!("notes-menu-copy-link"),
                    ui::i18n!("notes-menu-export"),
                    ui::i18n!("notes-menu-insert-image"),
                    ui::i18n!("notes-menu-delete")
                ]
            );
            assert!(
                menu.children
                    .iter()
                    .all(|item| item.role == ui::a11y::Role::MenuItem)
            );
            let target = menu.children.last().unwrap().action_target.clone().unwrap();
            let mut doc = AutoCommit::new();
            let before = doc.get_heads();
            assert!(!Widget::perform_semantic_action(
                tree.tree_mut(),
                &target,
                &ui::a11y::Action::Increment,
                &mut Edits::lend(&mut doc),
                &ObjId::Root
            ));
            assert!(asked(&door).is_empty());
            assert!(Widget::perform_semantic_action(
                tree.tree_mut(),
                &target,
                &ui::a11y::Action::Activate,
                &mut Edits::lend(&mut doc),
                &ObjId::Root
            ));
            let removed = asked(&door);
            assert_eq!(removed.len(), 1);
            match &removed[0].operation {
                crate::notebook::NotebookAction::Delete(delete) => {
                    assert_eq!(delete.page, notebook::page_id(page));
                }
                other => panic!("expected the notebook's delete, got {other:?}"),
            }
            let gone = tree
                .take_gone()
                .expect("the deletion is said once it lands");
            assert_eq!(gone.action(), Notebook::DELETE.id());
            assert_eq!(
                doc.get_heads(),
                before,
                "the tree only submits; the notebook owns admission"
            );
            assert!(
                !Widget::perform_semantic_action(
                    tree.tree_mut(),
                    &target,
                    &ui::a11y::Action::Activate,
                    &mut Edits::lend(&mut doc),
                    &ObjId::Root
                ),
                "closed menu target is stale"
            );
        }
    }

    /// A populated tree laid out shorter than its heading plus a minimum
    /// scrollbar thumb (a startup frame, a squeezed drawer) still draws.
    #[test]
    fn populated_sidebar_draws_in_short_startup_viewports() {
        let mut workspace = Workspace::new();
        for _ in 0..12 {
            workspace.create_page(None);
        }
        for scale in [1.0, 2.0] {
            for height in [1.0, 9.0, 16.0, 40.0] {
                let mut tree = PageSidebar::new();
                tree.sync_from_workspace(&workspace);
                place(&mut tree, Rect::new(0.0, 0.0, 240.0, height), scale);
            }
        }
    }
}
