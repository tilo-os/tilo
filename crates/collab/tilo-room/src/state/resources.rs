//! Outer-state wire schemas accepted by the resource-limited decoder.
//! Native document reconstruction has a separate admission boundary.

use super::DurableState;
use crate::resources::outer::{EncodeSchema, Schema};

impl EncodeSchema for DurableState {}
impl Schema<'_> for DurableState {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NeighbourMsg;
    use crate::resources::outer::{self, Error, Limits};

    fn limits() -> Limits {
        Limits {
            allocation: 16 * 1024 * 1024,
            collection_entries: 1024 * 1024,
        }
    }
    #[test]
    fn outer_resources_preserve_stored_bytes_with_and_without_exact_evidence() {
        let (mut peer, _, _, _, _) = crate::admission_tests::seeded();
        let v4 = peer.durable().unwrap();
        let decoded: DurableState = outer::decode(&v4, limits()).unwrap();
        assert_eq!(postcard::to_allocvec(&decoded).unwrap(), v4);
        peer.select_admission_v5().unwrap();
        let v5 = peer.durable().unwrap();
        let decoded: DurableState = outer::decode(&v5, limits()).unwrap();
        assert_eq!(postcard::to_allocvec(&decoded).unwrap(), v5);
        assert!(decoded.exact.is_some());
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
                Limits {
                    allocation: 128,
                    ..limits()
                }
            ),
            Err(Error::Capacity)
        );
    }
}
