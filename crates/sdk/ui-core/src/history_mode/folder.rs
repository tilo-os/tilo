//! A folder's changes as History reads them, and a file's: the changes of the one node.
//!
//! A window whose History lists a folder's changes shows the workspace's tree. The runner
//! that opens the window holds the tree's records and fills the window's source with them
//! once its storage is open, so History reads the same records the window's rows read rather
//! than a second copy, and names the people and agents its changes name with the same
//! directory the rows use. The source is the window's from its launch on: a view built
//! again, or built late because its record was still arriving, reads through the same one.
//!
//! A window that shows one file of the folder (Files, with a file selected) narrows the
//! source to it ([`FolderHistorySource::focus`]): History then lists that file's changes.
//! Taking a change back and restoring a file to a version are History's own verbs
//! ([`HistoryEdit`]) applied to the record the change is in, as one change of their own.

use std::cell::RefCell;
use std::rc::Rc;

use fs_protocol::files::{FileDocumentId, TreeChunk};

use crate::history_source::folder::FolderChange;
use crate::history_source::verbs::HistoryEdit;
use crate::{HistoryEntry, HistoryEntryId};

/// What History needs of the folder it lists: the tree's changes as rows, a number that
/// says when to read them again, and the record to apply a History verb to.
pub trait FolderVersions {
    /// The folder's changes, oldest first; only those of the file whose document id is
    /// `file`, when one is given ([`crate::history_source::folder::changes`]). Empty when
    /// the records cannot be read now.
    fn folder_changes(&self, file: Option<FileDocumentId>) -> Vec<FolderChange>;

    /// A number that changes when the folder's changes do: the tree records' revision,
    /// so History reads the changes again only when there are new ones.
    fn folder_revision(&self) -> u64;

    /// Apply `edit` to the document of chunk `chunk`, as the person at this window, the
    /// change naming `node` as what it acted on.
    ///
    /// # Errors
    /// Why nothing was done: the grants refuse it, or History's verb does.
    fn apply(&self, chunk: TreeChunk, edit: HistoryEdit, node: Option<&str>) -> Result<(), String>;

    /// The whole tree, as the folder's documents hold it now. `None` when it cannot be read.
    fn tree(&self) -> Option<fs_protocol::tree::Tree>;

    /// Do `actions` to the tree as `invoker`, each one change of the document of the chunk its
    /// node is in, as the executor writes any action: History names them as that invoker's.
    ///
    /// # Errors
    /// Why an action was not done: the grants refuse it, or the tree does. The ones before it
    /// are done.
    fn act(
        &self,
        actions: &[fs_protocol::FileAction],
        invoker: &tilo_actions::ActionInvoker,
    ) -> Result<(), String>;
}

#[derive(Default)]
struct Listing {
    versions: Option<Rc<dyn FolderVersions>>,
    /// The file the window shows, whose changes History lists instead of the folder's.
    focus: Option<FileDocumentId>,
    /// Moves on with every change of [`Self::focus`].
    focus_generation: u64,
    /// The changes last listed, by their rows' ids: what a verb on a row is applied to.
    listed: Vec<FolderChange>,
    /// Whether the window asked History to open.
    open: bool,
}

/// A folder's History source, as a registration holds it: the window's own listing, which
/// its runner fills with the tree records once the window's storage is open
/// ([`Self::show`]). Until then it lists nothing.
#[derive(Clone, Default)]
pub struct FolderHistorySource(Rc<RefCell<Listing>>);

impl FolderHistorySource {
    /// The folder `versions` reads, from now on.
    pub fn show(&self, versions: Rc<dyn FolderVersions>) {
        self.0.borrow_mut().versions = Some(versions);
    }

    /// List the changes of the file whose document id is `file` rather than the folder's;
    /// `None` lists the folder's again.
    pub fn focus(&self, file: Option<FileDocumentId>) {
        let mut listing = self.0.borrow_mut();
        if listing.focus != file {
            listing.focus = file;
            listing.focus_generation = listing.focus_generation.wrapping_add(1);
        }
    }

    /// The file History lists the changes of, when it lists one file's.
    #[must_use]
    pub fn focused(&self) -> Option<FileDocumentId> {
        self.0.borrow().focus
    }

    /// Ask History to open on this source.
    pub fn open(&self) {
        self.0.borrow_mut().open = true;
        super::note_news();
    }

    /// Whether the window asked History to open, since this was last answered.
    pub(crate) fn take_open(&self) -> bool {
        std::mem::take(&mut self.0.borrow_mut().open)
    }

    /// Whether the window asked History to open.
    pub(crate) fn wants_open(&self) -> bool {
        self.0.borrow().open
    }

    fn versions(&self) -> Option<Rc<dyn FolderVersions>> {
        self.0.borrow().versions.clone()
    }

    /// The changes listed now: the focused file's, else the folder's.
    #[must_use]
    pub fn entries(&self) -> Vec<HistoryEntry> {
        let focus = self.0.borrow().focus;
        let listed = self
            .versions()
            .map(|versions| versions.folder_changes(focus))
            .unwrap_or_default();
        let entries = listed.iter().map(|change| change.entry.clone()).collect();
        self.0.borrow_mut().listed = listed;
        entries
    }

    /// A number that changes when the listed changes do. The source being filled counts as a
    /// change: History reads the folder once its records are there.
    #[must_use]
    pub fn revision(&self) -> u64 {
        let generation = self.0.borrow().focus_generation;
        self.versions().map_or(0, |versions| {
            versions
                .folder_revision()
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add(generation)
                .saturating_add(1)
        })
    }

    /// The whole tree the window shows, when its documents are open and read.
    #[must_use]
    pub fn tree(&self) -> Option<fs_protocol::tree::Tree> {
        self.versions()?.tree()
    }

    /// Do `actions` to the window's tree as `invoker`: what an app carrying out someone else's
    /// call on the workspace's files does, so the files' History names who called it
    /// ([`FolderVersions::act`]).
    ///
    /// # Errors
    /// The folder is not open, or an action was refused.
    pub fn act(
        &self,
        actions: &[fs_protocol::FileAction],
        invoker: &tilo_actions::ActionInvoker,
    ) -> Result<(), String> {
        let versions = self.versions().ok_or("the folder is not open")?;
        versions.act(actions, invoker)?;
        super::note_news();
        Ok(())
    }

    /// Whether two sources are the same window's: what a runner checks when it hands a
    /// History to a window it did not build.
    #[must_use]
    pub fn same_owner(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    /// The listed change whose row is `id`.
    fn listed(&self, id: &HistoryEntryId) -> Option<FolderChange> {
        self.0
            .borrow()
            .listed
            .iter()
            .find(|change| change.entry.id == *id)
            .cloned()
    }

    /// Whether the change of row `id` acted on one node, which can be put back as it was then.
    #[must_use]
    pub fn restorable(&self, id: &HistoryEntryId) -> bool {
        self.listed(id).is_some_and(|change| change.node.is_some())
    }

    /// Take back the change of row `id`, wherever it stands: one change of its own, which
    /// History lists and which can be taken back in turn.
    ///
    /// # Errors
    /// The row is no listed change, or History's undo refused.
    pub fn undo(&self, id: &HistoryEntryId) -> Result<(), String> {
        let change = self.listed(id).ok_or("that change is no longer listed")?;
        let by = change
            .entry
            .context
            .as_ref()
            .and_then(|context| context.invoker.as_ref())
            .map(|invoker| invoker.acting.by.clone());
        let edit = HistoryEdit::Undo {
            steps: None,
            invocation: None,
            by,
            to: None,
            step: Some(change.hash.to_string()),
        };
        self.apply(&change, edit)
    }

    /// Put the node the change of row `id` acted on back as it was right after it: its
    /// content, name and place. One change of its own, which can be taken back.
    ///
    /// # Errors
    /// The row is no listed change of one node, or History's restore refused.
    pub fn restore(&self, id: &HistoryEntryId) -> Result<(), String> {
        let change = self.listed(id).ok_or("that change is no longer listed")?;
        let node = change
            .node
            .clone()
            .ok_or("that change acted on more than one file")?;
        let edit = HistoryEdit::Restore {
            version: change.hash.to_string(),
            ids: Some(vec![node]),
        };
        self.apply(&change, edit)
    }

    fn apply(&self, change: &FolderChange, edit: HistoryEdit) -> Result<(), String> {
        let versions = self.versions().ok_or("the folder is not open")?;
        versions.apply(change.chunk, edit, change.node.as_deref())?;
        super::note_news();
        Ok(())
    }
}
