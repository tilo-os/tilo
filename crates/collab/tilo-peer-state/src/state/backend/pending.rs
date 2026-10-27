//! Shared validation of the existing durable deferred-message queue.
use super::{NeighbourMsg, VecDeque};

#[derive(Debug, PartialEq, Eq)]
pub(in crate::state) enum PendingError {
    Count,
    Bytes,
    Encoding(postcard::Error),
}
impl std::fmt::Display for PendingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Count => formatter.write_str("durable pending queue exceeds entry limit"),
            Self::Bytes => formatter.write_str("durable pending queue byte accounting mismatch"),
            Self::Encoding(error) => error.fmt(formatter),
        }
    }
}

pub(in crate::state) fn validate(
    pending: &VecDeque<([u8; 32], NeighbourMsg, bool)>,
    declared: usize,
) -> Result<(), PendingError> {
    if pending.len() > crate::state::MAX_HELD_INPUTS {
        return Err(PendingError::Count);
    }
    // The closed NeighbourMsg Serialize graph borrows its fixed scalars and
    // owned fields. Counting writes neither allocates nor copies payloads.
    let mut measured = 0usize;
    for (_, message, _) in pending {
        let size = crate::postcard_sink::encoded_len(message, usize::MAX)
            .map_err(PendingError::Encoding)?;
        measured = measured.checked_add(size).ok_or(PendingError::Bytes)?;
    }
    if measured != declared || declared > crate::state::MAX_HELD_BYTES {
        return Err(PendingError::Bytes);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
