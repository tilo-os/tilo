//! Strict incoming validation deliberately has no current-graph argument.
use super::*;

pub(crate) fn load_all_changes_bounded(
    mut data: parse::Input<'_>,
    text_encoding: TextEncoding,
    permit: &crate::resource_limits::reconstruct::Permit<'_>,
) -> Result<Vec<Change>, crate::resource_limits::NativeResourceError> {
    use crate::resource_limits::NativeResourceError as Error;
    let mut changes = Vec::new();
    while !data.is_empty() {
        let (remaining, chunk) = storage::Chunk::parse(data).map_err(|_| Error::Invalid)?;
        if !chunk.checksum_valid() {
            return Err(Error::Invalid);
        }
        match chunk {
            storage::Chunk::Document(document) => {
                // Validate columns, actors, operation order, canonical changes
                // and exact derived heads even when every head is already known.
                changes.extend(document.reconstruct_changes_bounded(text_encoding, permit)?);
            }
            storage::Chunk::Change(change) => changes.push(
                Change::new_from_unverified(change.into_owned(), None)
                    .map_err(|_| Error::Invalid)?,
            ),
            storage::Chunk::Bundle(bundle) => {
                let bundle =
                    Bundle::new_from_unverified(bundle.into_owned()).map_err(|_| Error::Invalid)?;
                changes.extend(bundle.to_changes_bounded(permit)?);
            }
            // The exact checked input is normalized before this private call.
            storage::Chunk::CompressedChange(..) => return Err(Error::Invalid),
        }
        data = remaining.reset();
    }
    Ok(changes)
}
