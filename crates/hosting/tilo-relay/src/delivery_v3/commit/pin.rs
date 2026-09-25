//! Immutable payload custody only; never authority, admission, or ACK evidence.
use super::resources::PayloadBacking;
use super::{CommitError, Digest, PublicationPool};

/// An opaque root of an existing immutable payload allocation.
/// Resource-aware decoded roots carry their original byte credit with this handle.
/// Legacy roots still require the embedding owner to retain external custody.
pub struct PublicationPayload(PayloadBacking);
impl PublicationPayload {
    /// Lend the exact original payload; no raw Arc or Weak can escape.
    pub fn bytes(&self) -> &[u8] {
        self.0.as_slice()
    }
}
impl PublicationPool {
    /// Actual retained allocation for this immutable payload, including its Arc
    /// control/data allocation and Vec backing capacity. No reference count multiplier.
    pub fn retained_allocation_bytes(&self, id: &Digest) -> Result<usize, CommitError> {
        let bytes = self
            .wire
            .payloads
            .get(id)
            .ok_or(CommitError::MissingPayload)?;
        bytes.retained_allocation_bytes()
    }
    /// Pin one existing allocation without copying payload or map metadata.
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
        pool.wire.payloads.insert(id, PayloadBacking::legacy(bytes));
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
