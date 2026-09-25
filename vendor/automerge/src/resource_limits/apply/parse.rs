//! Incoming parse of normalized storage into canonical changes.
//!
//! The caller has run the borrowed preflight over the same bytes, so no
//! native decoder below reaches a narrowing panic, and has covered the
//! retained `Vec<Change>` (and any document/bundle expansion) under its
//! allowance or inspected limits. Every chunk kind is decoded strictly:
//! document chunks are always reconstructed, even when their heads are known,
//! so their columns, actors and canonical changes are validated.
use crate::resource_limits::NativeResourceError as Error;
use crate::storage::{self, parse};
use crate::{Change, TextEncoding};

/// Parse at most `capacity` changes (the inspected change count) from
/// normalized storage. The output Vec never grows past its reservation.
pub(crate) fn changes(bytes: &[u8], capacity: usize) -> Result<Vec<Change>, Error> {
    let mut changes: Vec<Change> = Vec::new();
    changes
        .try_reserve_exact(capacity)
        .map_err(|_| Error::WorkBytes)?;
    let push = |changes: &mut Vec<Change>, change: Change| {
        if changes.len() == capacity {
            return Err(Error::Unaccounted);
        }
        changes.push(change);
        Ok(())
    };
    let mut data = parse::Input::new(bytes);
    while !data.is_empty() {
        let (remaining, chunk) = storage::Chunk::parse(data).map_err(|_| Error::Invalid)?;
        if !chunk.checksum_valid() {
            return Err(Error::Invalid);
        }
        match chunk {
            storage::Chunk::Document(document) => {
                let reconstructed = document
                    .reconstruct_changes(TextEncoding::platform_default())
                    .map_err(|_| Error::Invalid)?;
                for change in reconstructed {
                    push(&mut changes, change)?;
                }
            }
            storage::Chunk::Change(change) => push(
                &mut changes,
                Change::new_from_unverified(change.into_owned(), None)
                    .map_err(|_| Error::Invalid)?,
            )?,
            storage::Chunk::Bundle(bundle) => {
                let bundle = storage::Bundle::new_from_unverified(bundle.into_owned())
                    .map_err(|_| Error::Invalid)?;
                for change in bundle.to_changes().map_err(|_| Error::Invalid)? {
                    push(&mut changes, change)?;
                }
            }
            // Normalization decompresses every checked change chunk.
            storage::Chunk::CompressedChange(..) => return Err(Error::Invalid),
        }
        data = remaining.reset();
    }
    Ok(changes)
}
