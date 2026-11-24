//! The history and undo a document keeps, beside the [`State`](crate::state::State) holding it.
//!
//! The implementation is the store's ([`tilo_resource_store::history`]): the steps,
//! their revert and the process's own undo stack work on a document alone, and
//! the collab client is what holds the document. The toolkit names them here, so
//! its dependency stays the collab client rather than the store.

pub use tilo_resource_store::history::{
    DESCRIPTIONS_KEY, GROUPS_KEY, HistoryMoment, INVOCATIONS_KEY, InstanceId,
    OwnedActionAttribution, OwnedActionReceipt, UndoEntry, UndoLog, authors, entry_id_from_hash,
    feed, is_description_marker, is_group_marker, is_history_metadata, is_invocation_marker,
    revert, same_frontier, steps, typing, verbs, words, write_patches, write_revert_patches,
    write_revert_patches_where,
};
