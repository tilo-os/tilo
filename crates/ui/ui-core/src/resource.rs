//! Resources as the toolkit shows them. The live store, a resource's document and moving it
//! between workspaces are `collaboration::resource`, below the toolkit; this adds what a widget
//! needs on top: the resource a widget tree names ([`capability`]), and the handles a
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
pub(crate) mod reads;
pub use person::StoredResource;
pub mod workspace;

#[cfg(test)]
mod replica_merge_tests;
#[cfg(test)]
mod workspace_answer_tests;
#[cfg(test)]
mod workspace_branch_tests;
