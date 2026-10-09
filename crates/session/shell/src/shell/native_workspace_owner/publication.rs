//! The state file's share of the owner's encoded publication bounds.
use super::store;
#[cfg(test)]
use crate::shell::state::WorkspaceTree;
pub(crate) use tilo_joined_room::validate_file_layout;

/// The most bytes a state file holding `authority` may encode to: the cap,
/// less the recovery its enrolled rooms reserved.
pub(crate) fn state_file_limit(
    authority: &tilo_joined_room::NativeAuthorityStorage,
) -> Result<usize, String> {
    tilo_joined_room::preflight_storage(authority)?;
    store::MAX_STATE_BYTES
        .checked_sub(tilo_joined_room::reserved_file_bytes(authority)?)
        .ok_or_else(|| "native state recovery reserve exceeds capacity".into())
}

/// Refuse a state file that leaves no room for the recovery its enrolled
/// rooms reserved.
#[cfg(test)]
pub(crate) fn preflight_tree(tree: &WorkspaceTree) -> Result<(), String> {
    let limit = state_file_limit(&tree.native_authority)?;
    store::preflight(tree, limit).map_err(|e| e.to_string())
}

#[cfg(test)]
pub(super) fn required_tree_bytes(tree: &WorkspaceTree) -> Result<usize, String> {
    let encoded = serde_json::to_vec_pretty(tree).map_err(|e| e.to_string())?;
    if encoded.len() > store::MAX_STATE_BYTES {
        return Err("native encoded recovery reserve exceeds capacity".into());
    }
    encoded
        .len()
        .checked_add(tilo_joined_room::reserved_file_bytes(
            &tree.native_authority,
        )?)
        .ok_or_else(|| "native test encoded reserve overflow".into())
}
