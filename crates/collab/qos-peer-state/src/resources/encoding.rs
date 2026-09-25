//! Fixed-capacity Postcard output retaining its actual reservation.

use super::*;

pub struct BoundedSerialized {
    bytes: Vec<u8>,
    _reservation: NativeReservation,
}
impl BoundedSerialized {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}
impl BoundedPeerMessage {
    /// Count first, then reserve and write into one fixed-capacity destination.
    /// No signed bytes or wire version are rewritten by resource accounting.
    pub fn encode(
        &self,
        maximum_bytes: usize,
        allowance: &NativeAllowance,
    ) -> Result<BoundedSerialized, PeerResourceError> {
        encode_known(&self.held.value, maximum_bytes, allowance)
    }
}

struct Count<'a> {
    bytes: usize,
    maximum: usize,
    exceeded: &'a mut bool,
}
impl Count<'_> {
    fn add(&mut self, length: usize) -> postcard::Result<()> {
        match self
            .bytes
            .checked_add(length)
            .filter(|n| *n <= self.maximum)
        {
            Some(bytes) => {
                self.bytes = bytes;
                Ok(())
            }
            None => {
                *self.exceeded = true;
                Err(postcard::Error::SerializeBufferFull)
            }
        }
    }
}
impl postcard::ser_flavors::Flavor for Count<'_> {
    type Output = usize;
    fn try_push(&mut self, _: u8) -> postcard::Result<()> {
        self.add(1)
    }
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        self.add(bytes.len())
    }
    fn finalize(self) -> postcard::Result<usize> {
        Ok(self.bytes)
    }
}

/// Only the closed set of peer wire DTOs may use this nonallocating preflight.
/// An arbitrary Serialize implementation could allocate internally and is not
/// a permitted input. Native save/export must produce its own guarded bytes
/// before those bytes can become an outer DTO field here.
pub(crate) fn encode_known<T: outer::EncodeSchema>(
    value: &T,
    maximum_bytes: usize,
    allowance: &NativeAllowance,
) -> Result<BoundedSerialized, PeerResourceError> {
    let mut exceeded = false;
    let length = postcard::serialize_with_flavor(
        value,
        Count {
            bytes: 0,
            maximum: maximum_bytes,
            exceeded: &mut exceeded,
        },
    )
    .map_err(|_| {
        if exceeded {
            PeerResourceError::Capacity
        } else {
            PeerResourceError::Malformed
        }
    })?;
    // Reservation happens before even the first output allocation. Slice
    // encoding can never grow its destination; the two passes borrow the same
    // immutable DTO and must agree on its complete encoded length.
    let reservation = allowance
        .reserve(length)
        .map_err(|_| PeerResourceError::Capacity)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| PeerResourceError::Capacity)?;
    bytes.resize(length, 0);
    let actual = postcard::to_slice(value, &mut bytes)
        .map_err(|_| PeerResourceError::Malformed)?
        .len();
    if actual != length {
        return Err(PeerResourceError::Malformed);
    }
    Ok(BoundedSerialized {
        bytes,
        _reservation: reservation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outer_resources_encode_exact_limit_preserves_signed_bytes_and_holds_output() {
        let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
        peer.select_admission_v5().unwrap();
        let message = peer
            .publish_document(
                "test.app",
                "tab:1",
                peer.document("test.app", "tab:1").unwrap(),
                &key,
                now,
            )
            .unwrap();
        let original = postcard::to_allocvec(&message).unwrap();
        let allowance = NativeAllowance::new(1024 * 1024).unwrap();
        let held = BoundedPeerMessage::decode(
            &original,
            OuterLimits {
                allocation_bytes: 128 * 1024,
                collection_entries: 4096,
            },
            &allowance,
        )
        .unwrap();
        let before = allowance.available_bytes();
        assert!(matches!(
            held.encode(original.len() - 1, &allowance),
            Err(PeerResourceError::Capacity)
        ));
        assert_eq!(allowance.available_bytes(), before);
        let output = held.encode(original.len(), &allowance).unwrap();
        assert_eq!(output.as_bytes(), original);
        assert_eq!(allowance.available_bytes(), before - original.len());
        drop(held);
        assert_eq!(output.as_bytes(), original);
        assert_eq!(
            allowance.available_bytes(),
            before + 128 * 1024 - original.len()
        );
        drop(output);
        assert_eq!(allowance.available_bytes(), before + 128 * 1024);
    }

    #[test]
    fn outer_resources_encode_refuses_shared_allowance_before_allocating_output() {
        let allowance = NativeAllowance::new(64 * 1024).unwrap();
        let message = crate::NeighbourMsg::SyncReq {
            since: vec![1; 32],
            room: "room".into(),
            invite_id: None,
        };
        let original = postcard::to_allocvec(&message).unwrap();
        let held = BoundedPeerMessage::decode(
            &original,
            OuterLimits {
                allocation_bytes: 32 * 1024,
                collection_entries: 4096,
            },
            &allowance,
        )
        .unwrap();
        let blocker = allowance.reserve(allowance.available_bytes()).unwrap();
        assert!(matches!(
            held.encode(original.len(), &allowance),
            Err(PeerResourceError::Capacity)
        ));
        assert_eq!(allowance.available_bytes(), 0);
        drop(blocker);
        assert_eq!(
            held.encode(original.len(), &allowance).unwrap().as_bytes(),
            original
        );
    }
}
