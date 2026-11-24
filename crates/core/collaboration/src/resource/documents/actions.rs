//! What the store keeps with each open resource beside its document and history: the generation of
//! its action frame, the writers this replica wrote it as, what the workspace refused or was told
//! of it, and the part a toolkit keeps of the action it has in flight on it.

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use automerge::AutoCommit;
use automerge::transaction::Transactable as _;

use super::{ResourceRefusal, WorkspaceNotices};
use crate::state::WritingActors;
use collab_types::history::UndoLog;

/// The part of a resource's action state a toolkit keeps: the action it has open on the resource and
/// what it answered of its history. The store never reads it; it closes the open action through it
/// before a save and before foreign changes join the document.
pub trait ToolkitActions: Any {
    /// Commit the open action's changes for a save, keeping the action itself open. Returns
    /// whether there was one.
    fn commit_for_save(&self, doc: &mut AutoCommit) -> bool;
    /// Close the open action into `history`: foreign changes are about to join the document.
    fn finish_external(&mut self, doc: &mut AutoCommit, history: &mut UndoLog);
    /// The value, to be read as its own type.
    fn as_any(&self) -> &dyn Any;
    /// The value, to be changed as its own type.
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// What the store keeps with one open resource beside its document and history.
pub struct ResourceActions {
    /// The generation of the resource's action frame: it moves whenever an operation resolved
    /// before must no longer apply. `None` once spent.
    pub generation: Option<u64>,
    /// Changes the workspace refused and the resource withdrew, not yet told to the app that has it
    /// ([`super::ResourceStore::take_refusals`]).
    pub(crate) refusals: Vec<ResourceRefusal>,
    /// What windows said they did, held until the workspace keeps it
    /// ([`super::ResourceStore::hold_notice`]).
    pub(crate) notices: WorkspaceNotices,
    /// The writers this replica has written the resource as: the resource writer and each writer
    /// derived for an author an action ran as.
    pub writers: Rc<RefCell<WritingActors>>,
    toolkit: Option<Box<dyn ToolkitActions>>,
}

impl Default for ResourceActions {
    fn default() -> Self {
        Self {
            generation: Some(0),
            refusals: Vec::new(),
            notices: WorkspaceNotices::default(),
            writers: Rc::default(),
            toolkit: None,
        }
    }
}

impl ResourceActions {
    /// Commit what is open on the resource for a save. A toolkit's open action keeps its own
    /// provenance; edits made outside any action are committed as the framework's own.
    pub fn commit_for_save(&self, doc: &mut AutoCommit) {
        if self
            .toolkit
            .as_ref()
            .is_some_and(|toolkit| toolkit.commit_for_save(doc))
        {
            return;
        }
        if doc.pending_ops() != 0 {
            // Input not wrapped in an action frame still has to be closed: a writer would see
            // unchanged heads and write only the earlier snapshot.
            doc.commit_with(
                automerge::transaction::CommitOptions::default()
                    .with_message(tilo_actions::FrameworkChange::Native.change_message()),
            );
        }
    }

    /// Close the toolkit's open action: an explicit external loan (merge, import, revision) is a
    /// boundary before foreign changes can join a local burst.
    pub fn finish_external(&mut self, doc: &mut AutoCommit, history: &mut UndoLog) {
        if let Some(toolkit) = self.toolkit.as_mut() {
            toolkit.finish_external(doc, history);
        }
    }

    /// The toolkit's part, made the first time it is asked for. A part of another type is
    /// replaced: one process has one toolkit.
    #[expect(
        clippy::expect_used,
        reason = "the part is a `T` once this checked or made one"
    )]
    pub fn toolkit<T: ToolkitActions + Default>(&mut self) -> &mut T {
        let toolkit = self.toolkit.get_or_insert_with(|| Box::new(T::default()));
        if !toolkit.as_any().is::<T>() {
            *toolkit = Box::new(T::default());
        }
        toolkit
            .as_any_mut()
            .downcast_mut::<T>()
            .expect("the part is a `T`")
    }

    /// The toolkit's part, when it made one.
    #[must_use]
    pub fn toolkit_ref<T: ToolkitActions>(&self) -> Option<&T> {
        self.toolkit
            .as_ref()
            .and_then(|toolkit| toolkit.as_any().downcast_ref::<T>())
    }
}
