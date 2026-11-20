//! Keeping the page's desktop in the profile.
//!
//! A save writes the layout's own document ([`LayoutDocument`]): the tree the page draws, the
//! document each window shows, who made each strip and opened each window, and — in a workspace —
//! the workspace's copy of the layout, which the profile's copy follows.
//!
//! Everything a save writes is counted once, before any of it is written or serialized
//! ([`Kept`]): a frame that changed none of it writes nothing, and reads nothing back — not the
//! workspace's layout, not the profile's — to find that out.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::ops::Deref;
use std::rc::Rc;

use durable_store::{DurableStore, StorageKey, WriteOp};
use tilo_desktop_model::{Layout, TabId};
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::DesktopNotifications;
use tilo_session_core::layout::{LayoutDocument, Store};
use ui::automerge::ChangeHash;
use ui::resource::DocumentCheckout;

/// `T`, changed only through [`Counted::edit`], which counts a change only when `T` differs after
/// it. What a save writes compares this count with the one it last wrote: a change that leaves the
/// value as it was — a window shown the document it already shows — saves nothing, and one that
/// differs always does.
#[derive(Debug)]
pub(crate) struct Counted<T> {
    value: T,
    edits: u64,
}

impl<T: Default> Default for Counted<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> Counted<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self { value, edits: 0 }
    }

    /// How many times the value changed: the same count means the same value.
    pub(crate) const fn edits(&self) -> u64 {
        self.edits
    }
}

impl<T: Clone + PartialEq> Counted<T> {
    /// Let `edit` change the value, counted as a change when the value differs afterwards.
    pub(crate) fn edit<R>(&mut self, edit: impl FnOnce(&mut T) -> R) -> R {
        let before = self.value.clone();
        let answered = edit(&mut self.value);
        if self.value != before {
            self.edits = self.edits.wrapping_add(1);
        }
        answered
    }
}

impl<T> Deref for Counted<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

/// The document each window of a tree shows, in the spelling links use (empty for an app's own).
pub(crate) type DocumentsShown = Rc<dyn Fn(&Layout) -> Vec<(TabId, String)>>;

/// The workspace's copy of this page's layout: what a page in a workspace saves beside its own
/// shell, and what the profile's copy follows.
pub(crate) trait Workspace {
    /// The heads of the workspace's layout document, or `None` while it holds no layout.
    fn heads(&self) -> Option<Vec<ChangeHash>>;

    /// The workspace's layout document entire, serialized.
    fn bytes(&self) -> Option<Vec<u8>>;

    /// Write `tree`, the layout this page shows, as this person's view of the workspace's layout:
    /// the strip and group in front, the tab in front of each group, and how wide each one is.
    fn show(&self, tree: &Layout);
}

/// What a save writes, counted: the same count means the same bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Kept {
    /// The shell's revision: its windows, their places and names, how the person shows them, and —
    /// in a workspace — the layout the workspace's copy last replaced this page's with.
    shell: u64,
    /// How many times the document each window shows, who made each strip and opened each window,
    /// and which agent oponed one changed ([`Counted`] each).
    beside: u64,
    /// A window started showing a document since the last save.
    bound: bool,
}

/// Saves a mounted desktop to the profile whenever what it writes changes.
pub(crate) struct Persistence {
    store: Rc<dyn DurableStore>,
    /// Where the profile's copy of the layout's document is kept, and why the last write of it was
    /// refused.
    profile: Rc<ProfileStore>,
    /// The layout's own document: the profile's copy of it, which is the layout's history when this
    /// page is in no workspace. The rules of the document — what one change of the layout is, who
    /// made it, how a step is taken back — are the session core's, the same a native session
    /// keeps. Shared with the page's layout, which changes that layout by action.
    layout: Rc<LayoutDocument>,
    /// The document each tab shows, once its component created or opened it.
    documents: Rc<RefCell<Counted<HashMap<TabId, DocumentCheckout>>>>,
    /// The document ids of tabs this page has not bound yet, under the spelling they arrived in.
    unbound: Rc<RefCell<Counted<HashMap<TabId, String>>>>,
    /// Who made each strip and opened each window, by strip or tab id.
    authors: Rc<RefCell<Counted<BTreeMap<u64, String>>>>,
    /// The agent's invitation that opened each window an agent opened, by tab.
    opened_by: Rc<RefCell<Counted<BTreeMap<TabId, tilo_desktop_model::ownership::OpenedBy>>>>,
    /// A tab started showing another document since the desktop was last saved.
    bound_changed: Rc<Cell<bool>>,
    /// The document each window of a tree shows ([`DocumentsShown`]): what a save of a page in no
    /// workspace writes beside the tree.
    documents_shown: DocumentsShown,
    /// The workspace whose layout this page's person shows, or `None` when this page is in none.
    workspace: Option<Rc<dyn Workspace>>,
    /// What the person's page offered to answer, kept past a reload.
    pub notifications: DesktopNotifications,
    notifications_key: StorageKey,
    /// What the last save wrote, and the notifications it wrote then.
    kept: Cell<Option<Kept>>,
    saved_notifications: Cell<u64>,
}

/// What a mount hands its persistence.
pub(crate) struct Keeping {
    pub store: Rc<dyn DurableStore>,
    pub layout: Rc<LayoutDocument>,
    pub profile: Rc<ProfileStore>,
    pub documents: Rc<RefCell<Counted<HashMap<TabId, DocumentCheckout>>>>,
    pub unbound: Rc<RefCell<Counted<HashMap<TabId, String>>>>,
    pub authors: Rc<RefCell<Counted<BTreeMap<u64, String>>>>,
    pub opened_by: Rc<RefCell<Counted<BTreeMap<TabId, tilo_desktop_model::ownership::OpenedBy>>>>,
    pub bound_changed: Rc<Cell<bool>>,
    pub documents_shown: DocumentsShown,
    pub workspace: Option<Rc<dyn Workspace>>,
    pub notifications: DesktopNotifications,
    pub notifications_key: StorageKey,
}

impl Persistence {
    pub(crate) fn new(keeping: Keeping) -> Self {
        let Keeping {
            store,
            layout,
            profile,
            documents,
            unbound,
            authors,
            opened_by,
            bound_changed,
            documents_shown,
            workspace,
            notifications,
            notifications_key,
        } = keeping;
        let saved_notifications = Cell::new(notifications.revision());
        Self {
            store,
            profile,
            layout,
            documents,
            unbound,
            authors,
            opened_by,
            bound_changed,
            documents_shown,
            workspace,
            notifications,
            notifications_key,
            kept: Cell::default(),
            saved_notifications,
        }
    }

    /// The layout document, as the page's layout keeps it too.
    pub(crate) fn layout(&self) -> Rc<LayoutDocument> {
        Rc::clone(&self.layout)
    }

    /// Write the desktop if what it writes changed since the last save.
    ///
    /// # Errors
    /// The store refused the write; the next change retries.
    pub fn save(&self, handle: &DesktopHandle) -> Result<(), String> {
        self.save_notifications()?;
        let kept = self.kept_now(handle);
        if self.kept.get() == Some(kept) {
            return Ok(());
        }
        // A page in no workspace keeps how this device shows the shell — the widths, the workspace
        // and the window in front — in the layout's own document, so a reload returns it as it was
        // left; in a workspace that is the person's own view of the layout.
        let tree = handle.read(Clone::clone);
        if let Some(workspace) = &self.workspace {
            workspace.show(&tree);
        } else {
            let shown = (self.documents_shown)(&tree);
            let authors = self.authors.borrow();
            let time = ui::time::unix_now_i64();
            self.layout.record_now(&tree, time, &shown, Some(&authors));
        }
        self.mirror_workspace()?;
        if let Some(error) = self.profile.refused() {
            return Err(error);
        }
        self.bound_changed.set(false);
        self.kept.set(Some(kept));
        Ok(())
    }

    /// Everything a save writes of `handle`, counted.
    fn kept_now(&self, handle: &DesktopHandle) -> Kept {
        Kept {
            shell: handle.revision(),
            beside: self.beside_edits(),
            bound: self.bound_changed.get(),
        }
    }

    /// How many times what this page shares of its desktop beside the shell changed: the documents
    /// its windows show, who made each strip and opened each window, and which agent opened one.
    fn beside_edits(&self) -> u64 {
        self.documents
            .borrow()
            .edits()
            .wrapping_add(self.unbound.borrow().edits())
            .wrapping_add(self.authors.borrow().edits())
            .wrapping_add(self.opened_by.borrow().edits())
    }

    /// Take the workspace's layout for the profile's copy, once the page's copy is behind it: a
    /// reload opens it as the workspace has it, and what the workspace changed reaches the file as
    /// the layout's own history.
    fn mirror_workspace(&self) -> Result<(), String> {
        let Some(workspace) = &self.workspace else {
            return Ok(());
        };
        let Some(heads) = workspace.heads() else {
            return Ok(());
        };
        if self.layout.has_changes(&heads) {
            return Ok(());
        }
        let Some(bytes) = workspace.bytes() else {
            return Ok(());
        };
        self.layout
            .replace_kept(bytes)
            .map_err(|error| error.to_string())
    }

    /// Keep what the person's page offered to answer: a notice kept past a reload could no longer
    /// be answered.
    fn save_notifications(&self) -> Result<(), String> {
        let revision = self.notifications.revision();
        if revision == self.saved_notifications.get() {
            return Ok(());
        }
        let bytes = self
            .notifications
            .kept(ui::time::wall())
            .map_err(|error| error.to_string())?;
        self.store
            .write(&self.notifications_key, WriteOp::Replace(bytes), None)
            .map_err(|error| error.to_string())?;
        self.saved_notifications.set(revision);
        Ok(())
    }
}

/// Where the profile's copy of the shell's document is kept, and why the store refused the last
/// write of it.
pub(crate) struct ProfileStore {
    store: Rc<dyn DurableStore>,
    key: StorageKey,
    refused: RefCell<Option<String>>,
}

impl ProfileStore {
    pub(crate) fn new(store: Rc<dyn DurableStore>, key: StorageKey) -> Self {
        Self {
            store,
            key,
            refused: RefCell::new(None),
        }
    }

    /// Why the last write was refused, taken so the next one starts clean.
    fn refused(&self) -> Option<String> {
        self.refused.borrow_mut().take()
    }
}

impl Store for ProfileStore {
    fn keep(&self, bytes: Vec<u8>) {
        if let Err(error) = self.store.write(&self.key, WriteOp::Replace(bytes), None) {
            *self.refused.borrow_mut() = Some(error.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test these are the assertion"
    )]

    use super::*;
    use durable_store::MemoryStore;
    use tilo_desktop_model::person_view::PersonView;
    use tilo_desktop_model::{IdMinter, Tab, TileContent, WindowWidth};
    use tilo_desktop_surface::{DesktopSurface, TileSource};
    use ui::automerge::{AutoCommit, ROOT, transaction::Transactable as _};
    use ui::resource::{DocumentId, ResourceType};

    /// The toolkit's content for a window: these tests read the tree, never the screen, so a window
    /// of theirs shows a notice.
    type Tile = ui::widget::TileHost;

    /// The page's windows, drawn by nobody: these tests read the tree, never the screen.
    struct Unbuilt;

    impl TileSource for Unbuilt {
        fn tile(&mut self, _: &tilo_desktop_model::Workspace, tab: &Tab) -> Tile {
            Tile::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            })
        }
    }

    /// The workspace's copy of the layout as the page's replication holds it, and what the page
    /// asked of it: its heads, its document whole (the serialization the profile's copy costs), and
    /// the person's view written into it.
    #[derive(Default)]
    struct Theirs {
        document: RefCell<AutoCommit>,
        moves: Cell<u64>,
        heads_read: Cell<u64>,
        serialized: Cell<u64>,
        shown: Cell<u64>,
    }

    impl Theirs {
        /// What the workspace's layout holds once a change of it reached it, as a device of the
        /// person's makes.
        fn moved(&self) {
            let next = self.moves.get().saturating_add(1);
            self.moves.set(next);
            let mut document = self.document.borrow_mut();
            document.put(ROOT, "moved", next).unwrap();
            document.commit();
        }
    }

    impl Workspace for Theirs {
        fn heads(&self) -> Option<Vec<ChangeHash>> {
            self.heads_read.set(self.heads_read.get().saturating_add(1));
            let heads = self.document.borrow_mut().get_heads();
            (!heads.is_empty()).then_some(heads)
        }

        fn bytes(&self) -> Option<Vec<u8>> {
            self.serialized.set(self.serialized.get().saturating_add(1));
            let bytes = self.document.borrow_mut().save();
            (!bytes.is_empty()).then_some(bytes)
        }

        fn show(&self, _: &Layout) {
            self.shown.set(self.shown.get().saturating_add(1));
        }
    }

    /// What one save of `handle` cost: the profile written, and the workspace asked for its layout.
    struct Cost {
        written: u64,
        serialized: u64,
    }

    /// A shell of one window, the persistence that keeps it, and the store it keeps it in.
    fn shell(workspace: Option<Rc<Theirs>>) -> (DesktopHandle, Rc<Persistence>, Rc<MemoryStore>) {
        let mut minter = IdMinter::new(7);
        let tab = Tab {
            id: minter.mint(),
            title: "Tasks".into(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.tasks".into(),
                label: "Tasks".into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        let tree = tilo_desktop_surface::seed_tree(&mut minter, "Office", vec![tab]);
        let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Unbuilt));
        drop(surface);
        let store = Rc::new(MemoryStore::queued());
        let key = StorageKey::document("so.tilo.browser-desktop", "office", "desktop");
        let profile = Rc::new(ProfileStore::new(
            Rc::clone(&store) as Rc<dyn DurableStore>,
            key,
        ));
        let documents: Rc<RefCell<Counted<HashMap<TabId, DocumentCheckout>>>> = Rc::default();
        let unbound: Rc<RefCell<Counted<HashMap<TabId, String>>>> = Rc::default();
        let layout = Rc::new(LayoutDocument::new(
            AutoCommit::new(),
            "Ada",
            Some(Rc::clone(&profile) as Rc<dyn Store>),
        ));
        let persistence = Rc::new(Persistence::new(Keeping {
            store: Rc::clone(&store) as Rc<dyn DurableStore>,
            layout,
            profile,
            documents: Rc::clone(&documents),
            unbound: Rc::clone(&unbound),
            authors: Rc::default(),
            opened_by: Rc::default(),
            bound_changed: Rc::default(),
            // The document each window shows, as the page reads it: bound, else the app's own.
            documents_shown: Rc::new(move |tree: &Layout| {
                let bound = documents.borrow();
                let waiting = unbound.borrow();
                tree.tabs()
                    .map(|tab| {
                        let shown = bound
                            .get(&tab.id)
                            .map(|shown| shown.id.route().to_owned())
                            .or_else(|| waiting.get(&tab.id).cloned())
                            .unwrap_or_default();
                        (tab.id, shown)
                    })
                    .collect()
            }),
            workspace: workspace.map(|workspace| workspace as Rc<dyn Workspace>),
            notifications: DesktopNotifications::default(),
            notifications_key: StorageKey::document("so.tilo.browser-desktop", "office", "notices"),
        }));
        (handle, persistence, store)
    }

    /// Save `handle` and say what it cost.
    fn keep(
        handle: &DesktopHandle,
        persistence: &Persistence,
        store: &Rc<MemoryStore>,
        workspace: Option<&Rc<Theirs>>,
    ) -> Cost {
        let writes = Rc::new(Cell::new(0_u64));
        let counted = Rc::clone(&writes);
        store.set_write_hook(Some(Rc::new(move || {
            counted.set(counted.get().saturating_add(1));
        })));
        let before = workspace.map_or(0, |workspace| workspace.serialized.get());
        persistence.save(handle).unwrap();
        store.set_write_hook(None);
        Cost {
            written: writes.get(),
            serialized: workspace.map_or(0, |workspace| {
                workspace.serialized.get().saturating_sub(before)
            }),
        }
    }

    /// The tab the shell's one window shows.
    fn only_tab(handle: &DesktopHandle) -> TabId {
        handle.read(|tree| tree.tabs().next().unwrap().id)
    }

    /// A window shown the document it already shows, as opening an issue in a window already open
    /// does: nothing the profile keeps changed.
    fn show_in_the_same_window(handle: &DesktopHandle, persistence: &Persistence) {
        let tab = only_tab(handle);
        let bound = DocumentCheckout::new(DocumentId::for_tab(tab), ResourceType::new("note"));
        persistence
            .documents
            .borrow_mut()
            .edit(|documents| documents.insert(tab, bound.clone()));
        persistence
            .documents
            .borrow_mut()
            .edit(|documents| documents.insert(tab, bound));
    }

    #[test]
    fn a_window_shown_its_own_document_saves_nothing() {
        let (handle, persistence, store) = shell(None);
        // The window opened its document, and the save kept it.
        show_in_the_same_window(&handle, &persistence);
        assert_eq!(keep(&handle, &persistence, &store, None).written, 1);
        // Opening an issue in that window binds the document it already shows.
        show_in_the_same_window(&handle, &persistence);
        let cost = keep(&handle, &persistence, &store, None);
        assert_eq!(cost.written, 0, "nothing the profile keeps changed");
        assert_eq!(cost.serialized, 0);
    }

    #[test]
    fn a_window_shown_another_document_saves_once() {
        let (handle, persistence, store) = shell(None);
        keep(&handle, &persistence, &store, None);
        let tab = only_tab(&handle);
        persistence.documents.borrow_mut().edit(|documents| {
            documents.insert(
                tab,
                DocumentCheckout::new(
                    DocumentId::for_tab(tab.wrapping_add(1)),
                    ResourceType::new("note"),
                ),
            );
        });
        assert_eq!(keep(&handle, &persistence, &store, None).written, 1);
    }

    #[test]
    fn a_tab_opened_saves_once() {
        let (handle, persistence, store) = shell(None);
        keep(&handle, &persistence, &store, None);
        let mut two = handle.read(Clone::clone);
        let id = handle.mint_id();
        two.workspaces[0].strips[0].window_groups[0].tabs.push(Tab {
            id,
            title: "Mail".into(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.mail".into(),
                label: "Mail".into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        });
        handle.replace_tree(two);
        assert_eq!(keep(&handle, &persistence, &store, None).written, 1);
    }

    #[test]
    fn a_width_the_person_dragged_saves_once() {
        let (handle, persistence, store) = shell(None);
        keep(&handle, &persistence, &store, None);
        let workspace = handle.read(|tree| tree.workspaces[0].id());
        let widths = handle.read(|tree| {
            tree.workspaces[0]
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .map(|group| (group.id, WindowWidth::Fraction(700)))
                .collect()
        });
        let wide = PersonView {
            widths,
            ..PersonView::default()
        };
        assert!(handle.show_view(&wide, workspace));
        assert_eq!(keep(&handle, &persistence, &store, None).written, 1);
    }

    #[test]
    fn the_workspaces_layout_reaches_the_profile_once() {
        let theirs = Rc::new(Theirs::default());
        let (handle, persistence, store) = shell(Some(Rc::clone(&theirs)));
        keep(&handle, &persistence, &store, Some(&theirs));
        // The workspace's layout moved, and the page took it as its own shell: what a device of
        // the person's changing the layout looks like here.
        theirs.moved();
        let mut tree = handle.read(Clone::clone);
        tree.external_revision = tree.external_revision.wrapping_add(1);
        handle.replace_tree(tree);
        assert_eq!(
            keep(&handle, &persistence, &store, Some(&theirs)).serialized,
            1
        );
        // The profile's copy is the workspace's now: nothing is serialized again.
        assert_eq!(
            keep(&handle, &persistence, &store, Some(&theirs)).serialized,
            0
        );
    }

    #[test]
    fn a_page_in_a_workspace_that_changed_nothing_asks_it_nothing() {
        let theirs = Rc::new(Theirs::default());
        let (handle, persistence, store) = shell(Some(Rc::clone(&theirs)));
        show_in_the_same_window(&handle, &persistence);
        keep(&handle, &persistence, &store, Some(&theirs));
        show_in_the_same_window(&handle, &persistence);
        let heads = theirs.heads_read.get();
        let shown = theirs.shown.get();
        let cost = keep(&handle, &persistence, &store, Some(&theirs));
        assert_eq!(cost.written, 0);
        assert_eq!(cost.serialized, 0);
        assert_eq!(
            theirs.heads_read.get(),
            heads,
            "a save with nothing to write does not ask the workspace what it holds"
        );
        assert_eq!(theirs.shown.get(), shown);
    }
}
