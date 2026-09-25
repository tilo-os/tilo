//! Immutable payload handles only; never authority, admission, or ACK evidence.
use super::{CommitError, Digest, PublicationPool};
use std::sync::Arc;

/// An opaque root of an existing immutable payload allocation. It keeps the
/// shared bytes alive after every pool holding them has been dropped.
pub struct PublicationPayload(Arc<Vec<u8>>);
impl PublicationPayload {
    /// Lend the exact original payload; no raw Arc or Weak can escape.
    pub fn bytes(&self) -> &[u8] {
        self.0.as_slice()
    }
}
impl PublicationPool {
    /// Actual retained allocation for this immutable payload, including its Arc
    /// control/data allocation and Vec backing capacity. No reference count multiplier.
    ///
    /// # Errors
    ///
    /// Returns `CommitError::MissingPayload` if the pool does not hold `id`.
    pub fn retained_allocation_bytes(&self, id: &Digest) -> Result<usize, CommitError> {
        let bytes = self
            .wire
            .payloads
            .get(id)
            .ok_or(CommitError::MissingPayload)?;
        // ArcInner is two atomic counters followed by the Vec header; the Vec
        // backing is its full capacity, not only its length.
        let header = std::alloc::Layout::new::<[usize; 2]>()
            .extend(std::alloc::Layout::new::<Vec<u8>>())
            .map_err(|_| CommitError::Quota)?
            .0
            .pad_to_align()
            .size();
        bytes
            .capacity()
            .checked_add(header)
            .ok_or(CommitError::Quota)
    }
    /// Pin one existing allocation without copying payload or map metadata.
    ///
    /// # Errors
    ///
    /// Returns `CommitError::MissingPayload` if the pool does not hold `id`.
    pub fn pin(&self, id: &Digest) -> Result<PublicationPayload, CommitError> {
        self.wire
            .payloads
            .get(id)
            .map(|bytes| PublicationPayload(bytes.clone()))
            .ok_or(CommitError::MissingPayload)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn publication_payload_pin_preserves_exact_backing_after_pool_destruction() {
        let mut pool = PublicationPool::new(1024).unwrap();
        let id = pool
            .intern(b"original immutable signed publication")
            .unwrap();
        let original = pool.resolve(&id).unwrap().as_ptr();
        let pin = pool.pin(&id).unwrap();
        assert_eq!(pin.bytes().as_ptr(), original);
        let fork = pool.fork();
        drop(pool);
        drop(fork);
        assert_eq!(pin.bytes(), b"original immutable signed publication");
        assert_eq!(pin.bytes().as_ptr(), original);
    }
    #[test]
    fn publication_payload_pin_cannot_invent_missing_payload() {
        let pool = PublicationPool::new(1024).unwrap();
        assert!(matches!(
            pool.pin(&[91; 32]),
            Err(CommitError::MissingPayload)
        ));
        assert!(pool.is_empty());
    }
    #[test]
    fn publication_payload_allocation_counts_actual_spare_backing_and_arc_once() {
        let mut pool = PublicationPool::new(1024).unwrap();
        let id = pool.intern(b"abc").unwrap();
        let mut bytes = Vec::with_capacity(97);
        bytes.extend_from_slice(b"abc");
        assert_eq!(bytes.capacity(), 97);
        pool.wire.payloads.insert(id, Arc::new(bytes));
        // ArcInner has two atomic usize counters followed by the three-word Vec.
        // Every supported 64-bit host therefore retains 40 bytes plus capacity.
        let expected = 97 + 5 * size_of::<usize>();
        assert_eq!(pool.retained_allocation_bytes(&id).unwrap(), expected);
        let pin = pool.pin(&id).unwrap();
        let fork = pool.fork();
        assert_eq!(pool.retained_allocation_bytes(&id).unwrap(), expected);
        assert_eq!(fork.retained_allocation_bytes(&id).unwrap(), expected);
        assert_eq!(pin.bytes().as_ptr(), pool.resolve(&id).unwrap().as_ptr());
        assert!(matches!(
            pool.retained_allocation_bytes(&[91; 32]),
            Err(CommitError::MissingPayload)
        ));
    }
}
