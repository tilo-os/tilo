//! Outer-state wire schemas accepted by the resource-limited decoder.
//! Native document reconstruction has a separate admission boundary.

use super::*;
use crate::resources::outer::{EncodeSchema, Schema};

impl EncodeSchema for DurableV2 {}
impl EncodeSchema for Durable {}
impl EncodeSchema for DurableV4 {}
impl EncodeSchema for DurableV5<'_> {}
impl<'de> Schema<'de> for DurableV2 {}
impl<'de> Schema<'de> for Durable {}
impl<'de> Schema<'de> for DurableV4 {}
impl<'de> Schema<'de> for DurableV5<'de> {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::outer::{self, Error, Limits};

    fn limits() -> Limits {
        Limits {
            allocation: 16 * 1024 * 1024,
            collection_entries: 1024 * 1024,
        }
    }
    #[test]
    fn outer_resources_preserve_actual_v2_v3_v4_v5_stored_bytes() {
        let (mut peer, _, _, _, _) = crate::admission_tests::seeded();
        let allowance = crate::resources::NativeAllowance::new(16 * 1024 * 1024).unwrap();
        let v2 = peer.durable_v2_for_test().unwrap();
        let decoded: DurableV2 = outer::decode(&v2, limits()).unwrap();
        assert_eq!(
            crate::resources::encode_known(&decoded, v2.len(), &allowance)
                .unwrap()
                .as_bytes(),
            v2
        );
        let v4 = peer.durable().unwrap();
        let decoded: DurableV4 = outer::decode(&v4, limits()).unwrap();
        assert_eq!(
            crate::resources::encode_known(&decoded, v4.len(), &allowance)
                .unwrap()
                .as_bytes(),
            v4
        );
        let v3 = postcard::to_allocvec(&decoded.base).unwrap();
        let decoded: Durable = outer::decode(&v3, limits()).unwrap();
        assert_eq!(
            crate::resources::encode_known(&decoded, v3.len(), &allowance)
                .unwrap()
                .as_bytes(),
            v3
        );
        peer.select_admission_v5().unwrap();
        let v5 = peer.durable().unwrap();
        let decoded: DurableV5<'_> = outer::decode(&v5, limits()).unwrap();
        assert_eq!(
            crate::resources::encode_known(&decoded, v5.len(), &allowance)
                .unwrap()
                .as_bytes(),
            v5
        );
        // The exact index remains a borrow of the literal original envelope.
        assert!(decoded.exact.as_ptr() >= v5.as_ptr());
        assert!(decoded.exact.as_ptr_range().end <= v5.as_ptr_range().end);
    }

    #[test]
    fn outer_resources_preserve_signed_boxed_publication_and_refuse_small_allowance() {
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
