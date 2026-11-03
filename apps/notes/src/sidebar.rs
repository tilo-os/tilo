//! The page tree: the toolkit's [`TreeView`] over a snapshot of the
//! [`Workspace`], configured as an outliner sidebar.
//!
//! The tree owns rows, disclosure, selection, keyboard navigation, inline
//! rename, drag-and-drop, the context menu, hover row actions, scrolling and
//! semantics. This module owns only what is specific to Notes: the indexed
//! model the view reads (every page may hold children), the page menu, and
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
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
#[cfg(test)]
use ui::widget::Edits;

use ui::framework::Dispatcher;
use ui::icons::Icon;
use ui::presence::PresenceKey;
use ui::reorder::{DropPlacement, ReorderDrop};
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::tree_view::{
    NodeId, TreeModel, TreeModelChange, TreeRowAction, TreeRowDecoration, TreeView,
};

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
    rename: Option<NodeId>,
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

pub type PageTreeView = TreeView<(), PageTree>;

/// The hidden root every top-level page hangs from.
const ROOT: NodeId = 0;

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

/// The page actions, in [`menu_row`] order (a separator before Delete).
pub(crate) fn note_menu_items() -> Vec<MenuItem> {
    vec![
        MenuItem::icon_action(Icon::FilePlus, ui::i18n!("notes-menu-new-child"), || {}),
        MenuItem::icon_action(Icon::Pencil, ui::i18n!("notes-menu-rename"), || {}),
        MenuItem::icon_action(Icon::Copy, ui::i18n!("notes-menu-duplicate"), || {}),
        MenuItem::icon_action(Icon::ExternalLink, ui::i18n!("notes-menu-copy-link"), || {}),
        MenuItem::icon_action(Icon::Download, ui::i18n!("notes-menu-export"), || {}),
        MenuItem::icon_action(Icon::Image, ui::i18n!("notes-menu-insert-image"), || {}),
        MenuItem::separator(),
        MenuItem::destructive_action(ui::i18n!("notes-menu-delete"), || {}).with_icon(Icon::Trash2),
    ]
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

/// Stable tree keys for page ids. Keys survive every rebuild of the model,
/// so selection, focus, expansion and the scroll anchor stay attached to the
/// same pages while the workspace changes underneath.
#[derive(Default)]
struct PageKeys {
    by_page: HashMap<PageId, NodeId>,
    by_key: HashMap<NodeId, PageId>,
    next: NodeId,
}

impl PageKeys {
    fn key(&mut self, page: PageId) -> NodeId {
        if let Some(key) = self.by_page.get(&page) {
            return *key;
        }
        self.next = self.next.saturating_add(1);
        self.by_page.insert(page, self.next);
        self.by_key.insert(self.next, page);
        self.next
    }

    fn page(&self, key: NodeId) -> Option<PageId> {
        self.by_key.get(&key).copied()
    }

    fn existing(&self, page: PageId) -> Option<NodeId> {
        self.by_page.get(&page).copied()
    }
}

type SharedKeys = Rc<RefCell<PageKeys>>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    title: String,
    parent: NodeId,
    children: Vec<NodeId>,
}

/// An indexed, read-only snapshot of the page hierarchy in workspace order.
/// Rebuilt whenever the workspace changes; every query the view makes is a
/// hash lookup.
pub struct PageTree {
    keys: SharedKeys,
    roots: Vec<NodeId>,
    entries: HashMap<NodeId, Entry>,
}

impl PageTree {
    fn build(workspace: &Workspace, keys: &SharedKeys) -> Self {
        let mut tree = Self {
            keys: Rc::clone(keys),
            roots: Vec::new(),
            entries: HashMap::new(),
        };
        // Every note's children in notebook order, gathered in one pass:
        // asking the workspace per note scanned every note for each one.
        let mut children_of: HashMap<Option<PageId>, Vec<&crate::model::PageMeta>> = HashMap::new();
        for page in workspace.pages() {
            children_of.entry(page.parent).or_default().push(page);
        }
        let mut registry = keys.borrow_mut();
        let mut stack: Vec<(Option<PageId>, NodeId)> = vec![(None, ROOT)];
        while let Some((page, key)) = stack.pop() {
            let siblings = children_of.get(&page).map_or(&[][..], Vec::as_slice);
            let children: Vec<NodeId> = siblings
                .iter()
                .map(|child| registry.key(child.id))
                .collect();
            for child in siblings {
                let child_key = registry.key(child.id);
                tree.entries.insert(
                    child_key,
                    Entry {
                        title: child.title.clone(),
                        parent: key,
                        children: Vec::new(),
                    },
                );
                stack.push((Some(child.id), child_key));
            }
            match tree.entries.get_mut(&key) {
                Some(entry) => entry.children = children,
                None => tree.roots = children,
            }
        }
        drop(registry);
        tree
    }

    fn children(&self, key: NodeId) -> &[NodeId] {
        if key == ROOT {
            return &self.roots;
        }
        self.entries
            .get(&key)
            .map_or(&[], |entry| entry.children.as_slice())
    }

    fn page(&self, key: NodeId) -> Option<PageId> {
        self.keys.borrow().page(key)
    }

    /// The page a key's parent stands for; `None` for a top-level page.
    fn parent_page(&self, key: NodeId) -> Option<PageId> {
        let parent = self.entries.get(&key)?.parent;
        if parent == ROOT {
            None
        } else {
            self.page(parent)
        }
    }

    /// The canonical `(new_parent, before)` a validated drop resolves to.
    fn placement(&self, dropped: &ReorderDrop<NodeId>) -> (Option<PageId>, Option<PageId>) {
        let target = dropped.target.key;
        match dropped.target.placement {
            DropPlacement::Inside => (self.page(target), None),
            DropPlacement::Before => (self.parent_page(target), self.page(target)),
            DropPlacement::After => {
                let parent = self.entries.get(&target).map_or(ROOT, |entry| entry.parent);
                let before = self
                    .children(parent)
                    .iter()
                    .skip_while(|key| **key != target)
                    .skip(1)
                    .find(|key| **key != dropped.source)
                    .and_then(|key| self.page(*key));
                (self.parent_page(target), before)
            }
        }
    }

    fn same_shape(&self, other: &Self) -> bool {
        self.roots == other.roots && self.entries == other.entries
    }
}

impl TreeModel for PageTree {
    fn root_id(&self) -> NodeId {
        ROOT
    }

    fn contains(&self, id: NodeId) -> bool {
        id == ROOT || self.entries.contains_key(&id)
    }

    fn label(&self, id: NodeId) -> Option<&str> {
        if id == ROOT {
            return Some("");
        }
        self.entries.get(&id).map(|entry| entry.title.as_str())
    }

    fn is_branch(&self, id: NodeId) -> bool {
        id == ROOT || !self.children(id).is_empty()
    }

    /// Rows are addressed by page, which outlives this session's keys.
    fn semantic_key(&self, id: NodeId) -> Option<String> {
        self.page(id).map(|page| format!("notes:page:{page}"))
    }

    /// Any page can hold children: dropping onto a leaf nests into it.
    fn accepts_children(&self, id: NodeId) -> bool {
        self.contains(id)
    }

    fn parent_id(&self, id: NodeId) -> Option<NodeId> {
        self.entries.get(&id).map(|entry| entry.parent)
    }

    fn child_index(&self, parent: NodeId, child: NodeId) -> Option<usize> {
        self.children(parent).iter().position(|key| *key == child)
    }

    fn child_count(&self, parent: NodeId) -> usize {
        self.children(parent).len()
    }

    fn child_id(&self, parent: NodeId, index: usize) -> Option<NodeId> {
        self.children(parent).get(index).copied()
    }
}

/// The notes sidebar: the page tree, the door its controls submit through,
/// and what it asked of its window. Dereferences to the [`PageTreeView`] it
/// hosts.
pub struct PageSidebar {
    tree: PageTreeView,
    keys: SharedKeys,
    door: SharedDoor,
    wanted: SharedWanted,
    /// False until the first sync expands every branch, so the tree opens
    /// showing its structure; after that expansion is the user's.
    did_initial_expand: bool,
    /// The selection seen by the previous sync. When it changes, its
    /// ancestors expand so the selected row is on screen (a `[[link]]`
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
        let keys = SharedKeys::default();
        let door = SharedDoor::default();
        let wanted = SharedWanted::default();
        let model = PageTree::build(&Workspace::default(), &keys);
        let on_select = (Rc::clone(&wanted), Rc::clone(&keys));
        let on_rename = (Rc::clone(&door), Rc::clone(&keys));
        let on_delete = (Rc::clone(&door), Rc::clone(&wanted), Rc::clone(&keys));
        let on_reorder = (Rc::clone(&door), Rc::clone(&keys));
        let on_row_action = (Rc::clone(&door), Rc::clone(&wanted), Rc::clone(&keys));
        let on_menu = (Rc::clone(&door), Rc::clone(&wanted), Rc::clone(&keys));
        let tree = TreeView::from_model(model)
            .with_accessible_name(ui::i18n!("notes-title"))
            .hide_root()
            .fill()
            .with_indent_guides()
            .selection_follows_focus(true)
            .rename_on_double_click()
            .tab_indents(true)
            .with_row_actions(vec![
                TreeRowAction::new(Icon::Plus, ui::i18n!("notes-affordance-new-child"))
                    .accessible_name(ui::i18n!("notes-a11y-new-child-under")),
                TreeRowAction::new(
                    Icon::MoreHorizontal,
                    ui::i18n!("notes-affordance-note-actions"),
                )
                .accessible_name(ui::i18n!("notes-a11y-more-actions-for"))
                .opens_context_menu(),
            ])
            .with_row_decoration(|model: &PageTree, row, _| {
                let decoration = TreeRowDecoration::new();
                match model.page(row.id) {
                    // Remote peers viewing a page glow its row.
                    Some(page) => decoration.presence(PresenceKey::Tab {
                        id: page.to_string(),
                    }),
                    None => decoration,
                }
            })
            .with_context_menu(|_, target| match target {
                Some(_) => note_menu_items(),
                None => empty_area_items(),
            })
            .with_reorder(
                |_, _, _| true,
                move |model: &mut PageTree, dropped| {
                    let Some(id) = model.page(dropped.source) else {
                        return false;
                    };
                    let (parent, before) = model.placement(&dropped);
                    move_page(&on_reorder.0, id, parent, before);
                    true
                },
            )
            .on_view("page_tree", move |key| {
                let (wanted, keys) = &on_select;
                if let Some(page) = keys.borrow().page(key) {
                    wanted.borrow_mut().open.push(page);
                }
            })
            .on_rename(move |key, new_title| {
                let (door, keys) = &on_rename;
                if let Some(page) = keys.borrow().page(key) {
                    rename_page(door, page, new_title);
                }
            })
            .on_delete(move |key| {
                let (door, wanted, keys) = &on_delete;
                if let Some(page) = keys.borrow().page(key) {
                    delete_page(door, wanted, page);
                }
            })
            .on_row_action(move |key, _| {
                // Only the "new child" action reports; "more" opens the menu.
                let (door, wanted, keys) = &on_row_action;
                if let Some(page) = keys.borrow().page(key) {
                    make_note(door, wanted, Some(page), false);
                }
            })
            .on_context_menu_item(move |target, index| {
                let (door, wanted, keys) = &on_menu;
                let page = target.and_then(|key| keys.borrow().page(key));
                match (target, page) {
                    (None, _) => match index {
                        0 => make_note(door, wanted, None, false),
                        1 => ask(door, &crate::effect::NotesEffect::ImportMarkdownFile),
                        2 => ask(door, &crate::effect::NotesEffect::ImportNotionExport),
                        _ => {}
                    },
                    (Some(key), Some(page)) => match index {
                        menu_row::NEW_CHILD => make_note(door, wanted, Some(page), false),
                        menu_row::RENAME => wanted.borrow_mut().rename = Some(key),
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
                    (Some(_), None) => {}
                }
            });
        Self {
            tree,
            keys,
            door,
            wanted,
            did_initial_expand: false,
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
    pub(crate) fn open_menu_rename(&mut self) {
        let key = self.wanted.borrow_mut().rename.take();
        if let Some(key) = key {
            self.tree.begin_rename(key);
        }
    }

    /// The hosted tree, as a widget.
    pub fn tree(&self) -> &PageTreeView {
        &self.tree
    }

    pub fn tree_mut(&mut self) -> &mut PageTreeView {
        &mut self.tree
    }

    fn key(&self, page: PageId) -> Option<NodeId> {
        self.keys.borrow().existing(page)
    }

    /// Mirror the workspace into the tree. Cheap when nothing changed; a
    /// changed hierarchy rebuilds the snapshot and the tree reconciles it
    /// against stable keys.
    pub fn sync_from_workspace(&mut self, workspace: &Workspace) {
        let next = PageTree::build(workspace, &self.keys);
        if !next.same_shape(self.tree.model()) {
            self.tree
                .update_source(&[TreeModelChange::Reset], |model| *model = next);
        }
        let mut expanded: Vec<NodeId> = self.tree.expanded_ids().collect();
        let mut reveal = None;
        if !self.did_initial_expand {
            expanded.extend(
                workspace
                    .pages()
                    .iter()
                    .filter(|page| !workspace.children_of(Some(page.id)).is_empty())
                    .filter_map(|page| self.key(page.id)),
            );
            self.did_initial_expand = true;
        }
        let selected = workspace.selected();
        if selected != self.last_selection {
            let mut ancestor = selected
                .and_then(|id| workspace.get(id))
                .and_then(|p| p.parent);
            while let Some(page) = ancestor {
                expanded.extend(self.key(page));
                ancestor = workspace.get(page).and_then(|p| p.parent);
            }
            reveal = selected.and_then(|page| self.key(page));
            self.tree.set_focused(reveal);
            self.last_selection = selected;
        }
        self.tree.restore_expanded(expanded);
        match selected.and_then(|page| self.key(page)) {
            Some(key) => self.tree.selection_mut().select_only(key),
            None => self.tree.selection_mut().clear(),
        }
        if let Some(key) = reveal {
            self.tree.reveal(key);
        }
    }

    /// Expanded pages, for the application's persisted local view.
    pub fn expanded_pages(&self) -> Vec<PageId> {
        let keys = self.keys.borrow();
        let mut pages: Vec<PageId> = self
            .tree
            .expanded_ids()
            .filter_map(|key| keys.page(key))
            .collect();
        pages.sort_unstable();
        pages
    }

    pub fn restore_expanded_pages(&mut self, pages: &[PageId], workspace: &Workspace) {
        let next = PageTree::build(workspace, &self.keys);
        self.tree
            .update_source(&[TreeModelChange::Reset], |model| *model = next);
        let keys: Vec<NodeId> = pages.iter().filter_map(|page| self.key(*page)).collect();
        self.tree.restore_expanded(keys);
        self.did_initial_expand = true;
        // Re-run the ancestor revelation even for an unchanged selection.
        self.last_selection = None;
        self.sync_from_workspace(workspace);
    }

    /// Open the inline rename field on `page`.
    pub fn begin_rename(&mut self, page: PageId) -> bool {
        self.key(page)
            .is_some_and(|key| self.tree.begin_rename(key))
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

    /// Visible row `index` as `(key, title, depth)`.
    fn row(&self, index: usize) -> Option<(NodeId, &str, usize)> {
        self.tree.visible_row(index)
    }

    #[cfg(test)]
    pub fn visible_row_count(&self) -> usize {
        self.tree.visible_len()
    }

    /// Test hook: the centre of visible row `index`, for click targeting
    /// through the real event path.
    #[doc(hidden)]
    pub fn row_center_for_test(&self, index: usize) -> Option<vello::kurbo::Point> {
        let (key, _, _) = self.row(index)?;
        self.tree.row_rect(key).map(|rect| rect.center())
    }

    /// Test hook: the centre of row `index`'s affordance `action`, for click
    /// targeting through the real event path.
    #[doc(hidden)]
    pub fn row_action_center_for_test(
        &self,
        index: usize,
        action: usize,
    ) -> Option<vello::kurbo::Point> {
        let (key, _, _) = self.row(index)?;
        self.tree
            .row_action_rect(key, action)
            .map(|rect| rect.center())
    }

    /// Test hook: the x of the disclosure column's centre on row `index`.
    #[doc(hidden)]
    pub fn row_disclosure_x_for_test(&self, index: usize) -> Option<f64> {
        let (key, _, _) = self.row(index)?;
        self.tree.disclosure_rect(key).map(|rect| rect.center().x)
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
        let (key, _, _) = self.row(index)?;
        Some(self.tree.model().is_branch(key))
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

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{AutoCommit, ObjId};
    use taffy::TaffyTree;
    use taffy::prelude::TaffyMaxContent;
    use ui::Widget;
    use ui::text_render::TextRenderer;
    use ui::theme::Theme;
    use vello::kurbo::{Point, Rect};
    use winit::event::{ElementState, Modifiers, WindowEvent};
    use winit::keyboard::NamedKey;

    /// Lay the sidebar out at `rect` the way the drawer host does.
    fn place(sidebar: &mut PageSidebar, rect: Rect, text_scale: f32) {
        let theme = Theme::dark().with_text_scale(text_scale);
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map = HashMap::new();
        let node = sidebar.build_layout(&mut taffy, &theme, &mut node_map);
        let _ = taffy.compute_layout(node, taffy::Size::MAX_CONTENT);
        sidebar.place_at(rect);
        let mut scene = vello::Scene::new();
        sidebar.draw(
            &mut scene,
            vello::kurbo::Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
            &ui::presence::PresenceCtx::empty(),
        );
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
            // Branches with children auto-expand on first render, so the full
            // nesting is on screen (root -> child -> grandchild).
            assert_eq!(tree.visible_row_count(), 3);
            // Collapsing the root hides all its descendants (only the root remains).
            let key = tree.key(root).unwrap();
            tree.set_expanded(key, false);
            tree.sync_from_workspace(&ws);
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
            let key_b = tree.key(b).unwrap();
            // `b` moves before `a`.
            let moved = ws.data.pages.remove(1);
            ws.data.pages.insert(0, moved);
            tree.sync_from_workspace(&ws);
            assert_eq!(tree.key(b), Some(key_b));
            assert_eq!(tree.row(0).map(|(key, _, _)| key), Some(key_b));
        }

        eprintln!("phase high_text_rows_and_labels_keep_distinct_scaled_geometry");
        {
            let mut ws = Workspace::new();
            ws.create_page(None);
            ws.create_page(None);
            let mut tree = PageSidebar::new();
            tree.sync_from_workspace(&ws);
            place(&mut tree, Rect::new(0.0, 0.0, 528.0, 600.0), 2.0);
            let first = tree
                .row(0)
                .and_then(|(key, _, _)| tree.row_rect(key))
                .unwrap();
            let second = tree
                .row(1)
                .and_then(|(key, _, _)| tree.row_rect(key))
                .unwrap();
            let disclosure = tree
                .row(0)
                .and_then(|(key, _, _)| tree.disclosure_rect(key))
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
            let key = tree.key(root).unwrap();
            assert_eq!(
                Widget::synthetic_accessibility_rect(tree.tree(), &[0]),
                tree.row_rect(key)
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
            let target_key = tree.key(target).unwrap();
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
            let key = tree.key(page).unwrap();
            tree.open_context_menu(Some(key), Point::ZERO);
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
            let key = tree.key(page).unwrap();
            let child = tree.row_action_rect(key, 0).unwrap();
            let more = tree.row_action_rect(key, 1).unwrap();

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
            let key = tree.key(page).unwrap();
            tree.open_context_menu(Some(key), Point::ZERO);
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
