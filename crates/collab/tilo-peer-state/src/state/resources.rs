//! Outer-state wire schemas accepted by the resource-limited decoder.
//! Native document reconstruction has a separate admission boundary.

use super::{DurableV4, DurableV5, DurableV6};
use crate::resources::outer::{EncodeSchema, Schema};

impl EncodeSchema for DurableV4 {}
impl EncodeSchema for DurableV5<'_> {}
impl EncodeSchema for DurableV6<'_> {}
impl Schema<'_> for DurableV4 {}
impl<'de> Schema<'de> for DurableV5<'de> {}
impl<'de> Schema<'de> for DurableV6<'de> {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NeighbourMsg;
    use crate::resources::{OuterLimits, PeerResourceError, outer};

    fn limits() -> OuterLimits {
        OuterLimits {
            allocation_bytes: 16 * 1024 * 1024,
            collection_entries: 1024 * 1024,
        }
    }
    #[test]
    fn outer_resources_preserve_v4_v5_stored_bytes() {
        let (mut peer, _, _, _, _) = crate::admission_tests::seeded();
        let v4 = peer.durable().unwrap();
        let decoded: DurableV4 = outer::decode(&v4, limits()).unwrap();
        assert_eq!(postcard::to_allocvec(&decoded).unwrap(), v4);
        peer.select_admission_v5().unwrap();
        let v5 = peer.durable().unwrap();
        let decoded: DurableV5<'_> = outer::decode(&v5, limits()).unwrap();
        assert_eq!(postcard::to_allocvec(&decoded).unwrap(), v5);
        // The exact index remains a borrow of the literal original envelope.
        assert!(decoded.exact.as_ptr() >= v5.as_ptr());
        assert!(decoded.exact.as_ptr_range().end <= v5.as_ptr_range().end);
    }

    #[test]
    fn outer_resources_preserve_signed_boxed_publication_and_refuse_small_limits() {
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
        let bytes = postcard::to_allocvec(&message).unwrap();
        let decoded: NeighbourMsg = outer::decode(&bytes, limits()).unwrap();
        assert_eq!(postcard::to_allocvec(&decoded).unwrap(), bytes);
        assert_eq!(decoded, message);
        assert_eq!(
            outer::decode::<NeighbourMsg>(
                &bytes,
                OuterLimits {
                    allocation_bytes: 128,
                    ..limits()
                }
            ),
            Err(PeerResourceError::Capacity)
        );
    }
}
