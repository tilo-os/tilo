//! The page tree: the toolkit's [`TreeView`] over a snapshot of the
//! [`Workspace`], configured as an outliner sidebar.
//!
//! The tree owns rows, disclosure, selection, keyboard navigation, inline
//! rename, drag-and-drop, the context menu, hover row actions, scrolling and
//! semantics. This module owns only what is specific to Notes: the indexed
//! model the view reads (every page may hold children), the page menu, and
//! the translation of the tree's signals into [`OutEvent`]s the parent
//! `NotesApp` applies through its replayable action path.
//!
//! Keyboard, while the tree holds focus (these keys otherwise belong to the
//! editor): ↑ / ↓ select, ← / → collapse / expand (or move to the parent /
//! first child), F2 renames, Delete deletes (never Backspace — the editor
//! and the tree can both hold focus), Alt-↑ / Alt-↓ reorder among siblings,
//! Tab / Shift-Tab (or Alt-→ / Alt-←) indent / outdent, and the Menu key or
//! Shift-F10 open the page menu. Creating a child of the selection
//! (Cmd-N / Ctrl-N) is handled one level up in `NotesApp`, so it works
//! whichever pane has focus.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
#[cfg(test)]
use ui::widget::Edits;

use ui::icons::Icon;
use ui::presence::PresenceKey;
use ui::reorder::{DropPlacement, ReorderDrop};
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::tree_view::{
    NodeId, TreeModel, TreeModelChange, TreeRowAction, TreeRowDecoration, TreeView,
};

use crate::model::{PageId, Workspace};

/// Outbound commands from the sidebar to the parent `NotesApp`. The
/// parent owns the workspace model and storage — the sidebar can only
/// *ask* for things to happen.
#[derive(Debug, Clone)]
pub enum OutEvent {
    Select(PageId),
    Rename {
        id: PageId,
        new_title: String,
    },
    Delete(PageId),
    /// Create a child of `parent` (None = root-level).
    CreateChild {
        parent: Option<PageId>,
    },
    /// Re-parent / reorder `id` to be a child of `new_parent` (None =
    /// root), inserted immediately before sibling `before` (None =
    /// append to the group). Backs dragging, Alt-↑ / Alt-↓ and indent /
    /// outdent.
    Reparent {
        id: PageId,
        new_parent: Option<PageId>,
        before: Option<PageId>,
    },
    /// Deep-copy `id` and its subtree, placing the copy right after it.
    /// Backs the context menu's "Duplicate".
    Duplicate(PageId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarTransfer {
    ImportMarkdown,
    CopyLink(PageId),
    ExportMarkdown(PageId),
    InsertImage(PageId),
}

/// The tree type the sidebar hosts.
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
        MenuItem::icon_action(Icon::FilePlus, "New note", || {}),
        MenuItem::icon_action(Icon::CloudDownload, "Import Markdown", || {}),
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
        let mut registry = keys.borrow_mut();
        let mut stack: Vec<(Option<PageId>, NodeId)> = vec![(None, ROOT)];
        while let Some((page, key)) = stack.pop() {
            let children: Vec<NodeId> = workspace
                .children_of(page)
                .iter()
                .map(|child| registry.key(child.id))
                .collect();
            for child in workspace.children_of(page) {
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

/// What the tree's callbacks queue for the app to drain.
#[derive(Default)]
struct Outbox {
    events: Vec<OutEvent>,
    transfer: Option<SidebarTransfer>,
    /// A menu's "Rename": the field opens once the callback has returned.
    rename: Option<NodeId>,
}

type SharedOutbox = Rc<RefCell<Outbox>>;

/// The notes sidebar: the page tree plus the queue its signals land in.
/// Dereferences to the [`PageTreeView`] it hosts.
pub struct PageSidebar {
    tree: PageTreeView,
    keys: SharedKeys,
    outbox: SharedOutbox,
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

/// Queue `event` for the page behind `key`.
fn push_for(
    outbox: &SharedOutbox,
    keys: &SharedKeys,
    key: NodeId,
    event: impl FnOnce(PageId) -> OutEvent,
) {
    let page = keys.borrow().page(key);
    if let Some(page) = page {
        outbox.borrow_mut().events.push(event(page));
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
        let outbox = SharedOutbox::default();
        let model = PageTree::build(&Workspace::default(), &keys);
        let on_select = (Rc::clone(&outbox), Rc::clone(&keys));
        let on_rename = (Rc::clone(&outbox), Rc::clone(&keys));
        let on_delete = (Rc::clone(&outbox), Rc::clone(&keys));
        let on_reorder = Rc::clone(&outbox);
        let on_row_action = (Rc::clone(&outbox), Rc::clone(&keys));
        let on_menu = (Rc::clone(&outbox), Rc::clone(&keys));
        let tree = TreeView::from_model(model)
            .with_id("page_tree")
            .with_accessible_name(ui::i18n!("notes-title"))
            .hide_root()
            .fill()
            .with_indent_guides()
            .selection_follows_focus(true)
            .rename_on_double_click()
            .tab_indents(true)
            .with_row_hint(ui::i18n!("notes-affordance-note-row"))
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
                    let (new_parent, before) = model.placement(&dropped);
                    on_reorder.borrow_mut().events.push(OutEvent::Reparent {
                        id,
                        new_parent,
                        before,
                    });
                    true
                },
            )
            .on_select(move |key| push_for(&on_select.0, &on_select.1, key, OutEvent::Select))
            .on_rename(move |key, new_title| {
                push_for(&on_rename.0, &on_rename.1, key, |id| OutEvent::Rename {
                    id,
                    new_title,
                });
            })
            .on_delete(move |key| push_for(&on_delete.0, &on_delete.1, key, OutEvent::Delete))
            .on_row_action(move |key, _| {
                // Only the "new child" action reports; "more" opens the menu.
                push_for(&on_row_action.0, &on_row_action.1, key, |page| {
                    OutEvent::CreateChild { parent: Some(page) }
                });
            })
            .on_context_menu_item(move |target, index| {
                let (outbox, keys) = &on_menu;
                let page = target.and_then(|key| keys.borrow().page(key));
                let mut outbox = outbox.borrow_mut();
                match (target, page) {
                    (None, _) => match index {
                        0 => outbox.events.push(OutEvent::CreateChild { parent: None }),
                        1 => outbox.transfer = Some(SidebarTransfer::ImportMarkdown),
                        _ => {}
                    },
                    (Some(key), Some(page)) => match index {
                        menu_row::NEW_CHILD => outbox
                            .events
                            .push(OutEvent::CreateChild { parent: Some(page) }),
                        menu_row::RENAME => outbox.rename = Some(key),
                        menu_row::DUPLICATE => outbox.events.push(OutEvent::Duplicate(page)),
                        menu_row::COPY_LINK => {
                            outbox.transfer = Some(SidebarTransfer::CopyLink(page));
                        }
                        menu_row::EXPORT => {
                            outbox.transfer = Some(SidebarTransfer::ExportMarkdown(page));
                        }
                        menu_row::INSERT_IMAGE => {
                            outbox.transfer = Some(SidebarTransfer::InsertImage(page));
                        }
                        menu_row::DELETE => outbox.events.push(OutEvent::Delete(page)),
                        _ => {}
                    },
                    (Some(_), None) => {}
                }
            });
        Self {
            tree,
            keys,
            outbox,
            did_initial_expand: false,
            last_selection: None,
        }
    }

    /// The hosted tree, as a widget.
    pub fn tree(&self) -> &PageTreeView {
        &self.tree
    }

    pub fn tree_mut(&mut self) -> &mut PageTreeView {
        &mut self.tree
    }

    /// Drain the commands the tree queued since the last call. A menu's
    /// "Rename" opens the inline field here, after the menu has closed.
    pub fn take_events(&mut self) -> Vec<OutEvent> {
        let rename = self.outbox.borrow_mut().rename.take();
        if let Some(key) = rename {
            self.tree.begin_rename(key);
        }
        std::mem::take(&mut self.outbox.borrow_mut().events)
    }

    pub fn take_transfer(&mut self) -> Option<SidebarTransfer> {
        self.outbox.borrow_mut().transfer.take()
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

    /// Test hook: the sidebar's laid-out rect.
    #[doc(hidden)]
    pub fn rect_for_test(&self) -> vello::kurbo::Rect {
        ui::Widget::get_rect(&self.tree)
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
    use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
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
        WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(point.x, point.y),
        }
    }

    fn button(state: ElementState) -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state,
            button: MouseButton::Left,
        }
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

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
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
            let a = ws.create_page(None);
            let b = ws.create_page(None);
            let mut tree = PageSidebar::new();
            tree.sync_from_workspace(&ws);
            let key_b = tree.key(b).unwrap();
            ws.move_to(b, None, Some(a));
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
            ws.rename(root, "Project plan");
            let mut tree = PageSidebar::new();
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
            assert!(matches!(tree.take_events().as_slice(), [OutEvent::Select(id)] if *id == root));
        }
    }

    #[test]
    fn sidebar_drag_and_scroll_reveal_hidden_targets() {
        eprintln!("phase dragging_over_collapsed_branch_springs_it_open_after_dwell");
        {
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
            std::thread::sleep(std::time::Duration::from_millis(700));
            tree.update(&doc, &ObjId::Root);
            assert!(tree.is_expanded(target_key), "the dwell opens the branch");
        }

        eprintln!("phase dragging_in_bottom_edge_band_autoscrolls_toward_hidden_targets");
        {
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
            std::thread::sleep(std::time::Duration::from_millis(30));
            tree.update(&doc, &ObjId::Root);
            assert!(
                tree.scroll_offset() > 0.0,
                "edge dragging reveals lower drop targets without ending the gesture"
            );
        }

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
    fn sidebar_rename_emits_only_on_commit() {
        eprintln!("phase begin_and_commit_rename_emits_event");
        {
            let mut ws = Workspace::new();
            let id = ws.create_page(None);
            ws.rename(id, "Untitled");
            let mut tree = PageSidebar::new();
            tree.sync_from_workspace(&ws);
            assert!(tree.begin_rename(id));
            tree.commit_rename();
            let events = tree.take_events();
            assert_eq!(events.len(), 1);
            match &events[0] {
                OutEvent::Rename { id: rid, new_title } => {
                    assert_eq!(*rid, id);
                    assert_eq!(new_title, "Untitled");
                }
                _ => panic!("expected Rename event"),
            }
        }

        eprintln!("phase cancel_rename_does_not_emit");
        {
            let mut ws = Workspace::new();
            let id = ws.create_page(None);
            let mut tree = PageSidebar::new();
            tree.sync_from_workspace(&ws);
            assert!(tree.begin_rename(id));
            event(&mut tree, &named_key(NamedKey::Escape));
            assert!(!tree.is_renaming());
            assert!(tree.take_events().is_empty());
        }

        eprintln!("phase menu_rename_opens_the_field_once_the_menu_has_closed");
        {
            let mut workspace = Workspace::new();
            let page = workspace.create_page(None);
            let mut tree = PageSidebar::new();
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
            assert!(tree.take_events().is_empty());
            assert!(tree.is_renaming());
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn sidebar_page_menu_and_row_actions_queue_page_scoped_actions() {
        eprintln!("phase page_menu_copy_link_uses_the_selected_page_identity");
        {
            let mut workspace = Workspace::new();
            let page = workspace.create_page(None);
            let mut tree = PageSidebar::new();
            tree.sync_from_workspace(&workspace);
            let key = tree.key(page).unwrap();
            assert!(tree.open_context_menu(Some(key), Point::ZERO));
            let menu = Widget::accessibility(tree.tree()).unwrap();
            let copy = menu
                .children
                .iter()
                .find(|node| node.role == ui::a11y::Role::Menu)
                .and_then(|menu| {
                    menu.children.iter().find(|item| {
                        item.name.as_deref() == Some(&ui::i18n!("notes-menu-copy-link"))
                    })
                })
                .and_then(|item| item.action_target.clone())
                .unwrap();
            assert!(Widget::perform_semantic_action(
                tree.tree_mut(),
                &copy,
                &ui::a11y::Action::Activate,
                &mut Edits::lend(&mut AutoCommit::new()),
                &ObjId::Root
            ));
            assert_eq!(tree.take_transfer(), Some(SidebarTransfer::CopyLink(page)));
        }

        eprintln!("phase hover_row_actions_create_a_child_or_open_the_existing_note_menu");
        {
            let mut workspace = Workspace::new();
            let page = workspace.create_page(None);
            let mut tree = PageSidebar::new();
            tree.sync_from_workspace(&workspace);
            place(&mut tree, Rect::new(0.0, 0.0, 240.0, 200.0), 1.0);
            let key = tree.key(page).unwrap();
            let child = tree.row_action_rect(key, 0).unwrap();
            let more = tree.row_action_rect(key, 1).unwrap();

            event(&mut tree, &moved(child.center()));
            assert!(event(&mut tree, &button(ElementState::Pressed)));
            event(&mut tree, &button(ElementState::Released));
            assert!(matches!(
                tree.take_events().as_slice(),
                [OutEvent::CreateChild { parent: Some(id) }] if *id == page
            ));

            event(&mut tree, &moved(more.center()));
            assert!(event(&mut tree, &button(ElementState::Pressed)));
            assert!(tree.context_menu_open_for_test());
            assert!(matches!(
                tree.take_events().as_slice(),
                [OutEvent::Select(id)] if *id == page
            ));
        }

        eprintln!("phase page_menu_semantics_activate_the_same_queued_page_action");
        {
            let mut workspace = Workspace::new();
            let page = workspace.create_page(None);
            let mut tree = PageSidebar::new();
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
            assert!(tree.take_events().is_empty());
            assert!(Widget::perform_semantic_action(
                tree.tree_mut(),
                &target,
                &ui::a11y::Action::Activate,
                &mut Edits::lend(&mut doc),
                &ObjId::Root
            ));
            assert!(matches!(tree.take_events().as_slice(), [OutEvent::Delete(id)] if *id == page));
            assert_eq!(
                doc.get_heads(),
                before,
                "sidebar activation only queues; app owns document admission"
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
