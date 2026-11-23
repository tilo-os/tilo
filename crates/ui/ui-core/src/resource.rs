//! Resources as the toolkit shows them. The live store, a resource's document and moving it
//! between workspaces are `collaboration::resource`, below the toolkit; this adds what a widget
//! needs on top: the resource a widget tree names ([`capability`], [`tick`]), and the handles a
//! window holds on the resources it shows ([`StoredResource`], [`workspace::WorkspaceResources`]),
//! whose edits the toolkit's history owns.

pub use ::collaboration::resource::*;
pub use collab_types::document::{
    DOCUMENT_ID_XATTR, MAX_DOCUMENT_RELATION_BYTES, chain_of_resource_id, chain_resource_id,
    valid_label,
};
pub use collab_types::person_workspace_of;
pub mod capability;
pub mod person;
pub use person::StoredResource;
pub mod workspace;

use automerge::AutoCommit;

/// Run one record tick: if the widget tree names a record other than the
/// live one, park it and check the named one out. `tilo_ui::App` calls this
/// once per frame, before `sync`, so a widget that opened a file last
/// frame is editing that file's document by the time it next reads state.
/// The checkout it returns is the caller's cue to re-announce its
/// collaboration channel — a checkout is a change of address.
pub fn tick(
    set: &mut ResourceSet,
    state: &mut crate::state::State,
    workspace: Option<&mut AutoCommit>,
    widget: &dyn crate::widget::Widget,
) -> Option<DocumentCheckout> {
    let want = capability::record_intent(widget)?;
    set.checkout(state, workspace, &want).then_some(want)
}

#[cfg(test)]
mod replica_merge_tests;
#[cfg(test)]
mod workspace_answer_tests;
#[cfg(test)]
mod workspace_branch_tests;
