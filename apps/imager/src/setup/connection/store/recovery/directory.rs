//! Exact Directory prefix receipt derivation; no native proof is reconstructed.
use super::super::{EntryState, RelativePath, StoreError, format};
use std::collections::BTreeMap;

pub(in crate::setup::connection::store) fn ancestor_patch(
    operation: &format::Operation,
    old: &BTreeMap<RelativePath, EntryState>,
) -> Result<Option<format::DirectoryAncestorPatchV1>, StoreError> {
    operation.decode()?;
    let leaf = operation.path.to_relative()?;
    if operation.native_kind != Some(format::NativeOperationKind::DirectoryV1)
        || operation.baseline.is_none()
        || old.keys().any(|path| path.logical() == leaf.logical())
    {
        return Err(StoreError::RecoveryRequired);
    }
    let requirement =
        crate::setup::connection::transaction::ExistingAncestorRequirement::from_baseline(
            &leaf, old,
        )?;
    let bytes = operation
        .native_identity
        .as_deref()
        .ok_or(StoreError::Corrupt)?;
    Ok(
        (requirement.prefixes() != 0).then(|| format::DirectoryAncestorPatchV1 {
            version: 1,
            admission: format::digest(bytes),
            prefixes: requirement.prefixes(),
        }),
    )
}
