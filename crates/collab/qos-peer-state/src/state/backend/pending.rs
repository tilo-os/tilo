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
    if pending.len() > 64 {
        return Err(PendingError::Count);
    }
    // The closed NeighbourMsg Serialize graph borrows its fixed scalars and
    // owned fields. Counting writes neither allocates nor copies payloads.
    let mut measured = 0usize;
    for (_, message, _) in pending {
        let size =
            postcard::serialize_with_flavor(message, Count(0)).map_err(PendingError::Encoding)?;
        measured = measured.checked_add(size).ok_or(PendingError::Bytes)?;
    }
    if measured != declared || declared > 4 * 1024 * 1024 {
        return Err(PendingError::Bytes);
    }
    Ok(())
}

struct Count(usize);
impl postcard::ser_flavors::Flavor for Count {
    type Output = usize;
    fn try_push(&mut self, _: u8) -> postcard::Result<()> {
        self.add(1)
    }
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        self.add(bytes.len())
    }
    fn finalize(self) -> postcard::Result<usize> {
        Ok(self.0)
    }
}
impl Count {
    fn add(&mut self, bytes: usize) -> postcard::Result<()> {
        self.0 = self
            .0
            .checked_add(bytes)
            .ok_or(postcard::Error::SerializeBufferFull)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
