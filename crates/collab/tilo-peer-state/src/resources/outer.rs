//! The closed set of postcard wire schemas the budgeted decoder admits.
use super::{OuterLimits, PeerResourceError};

/// Closed crate-internal set, implemented only for audited peer wire DTOs.
/// Native documents and arbitrary user Deserialize code cannot enter here.
pub(crate) trait EncodeSchema: serde::Serialize {}
pub(crate) trait Schema<'de>: serde::Deserialize<'de> + EncodeSchema {}

impl EncodeSchema for crate::NeighbourMsg {}
impl Schema<'_> for crate::NeighbourMsg {}

/// Decode one complete value of a closed wire schema within `limits`.
pub(crate) fn decode<'de, T: Schema<'de>>(
    bytes: &'de [u8],
    limits: OuterLimits,
) -> Result<T, PeerResourceError> {
    super::guard::postcard(bytes, limits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NeighbourMsg;

    fn limits(allocation_bytes: usize, collection_entries: usize) -> OuterLimits {
        OuterLimits {
            allocation_bytes,
            collection_entries,
        }
    }

    #[test]
    fn outer_resources_refuse_announced_collection_before_element_decode() {
        // Actual SyncResp discriminant followed by an oversized changes count,
        // with no element bytes. Capacity must win over malformed element EOF.
        let mut bytes = postcard::to_allocvec(&NeighbourMsg::SyncResp {
            changes: vec![],
            room: String::new(),
        })
        .unwrap();
        bytes.truncate(1);
        bytes.extend(postcard::to_allocvec(&4096usize).unwrap());
        assert_eq!(
            decode::<NeighbourMsg>(&bytes, limits(1024 * 1024, 16)),
            Err(PeerResourceError::Capacity)
        );
    }

    #[test]
    fn outer_resources_bound_owned_buffers_across_all_chunks() {
        let message = NeighbourMsg::SyncResp {
            changes: vec![vec![1; 2048], vec![2; 2048]],
            room: "room".into(),
        };
        let bytes = postcard::to_allocvec(&message).unwrap();
        let original = bytes.clone();
        let one_chunk = NeighbourMsg::SyncResp {
            changes: vec![vec![1; 2048]],
            room: "room".into(),
        };
        assert_eq!(
            decode::<NeighbourMsg>(
                &postcard::to_allocvec(&one_chunk).unwrap(),
                limits(14 * 1024, 4096)
            )
            .unwrap(),
            one_chunk
        );
        assert_eq!(
            decode::<NeighbourMsg>(&bytes, limits(14 * 1024, 4096)),
            Err(PeerResourceError::Capacity)
        );
        assert_eq!(bytes, original);
        assert_eq!(
            decode::<NeighbourMsg>(&bytes, limits(1024 * 1024, 4096)).unwrap(),
            message
        );
    }

    #[test]
    fn outer_resources_refuse_string_copy_and_preserve_valid_wire() {
        let message = NeighbourMsg::SyncReq {
            since: vec![],
            room: "r".repeat(4096),
            invite_id: None,
            nonce: 0,
        };
        let bytes = postcard::to_allocvec(&message).unwrap();
        assert_eq!(
            decode::<NeighbourMsg>(&bytes, limits(1024, 4096)),
            Err(PeerResourceError::Capacity)
        );
        assert_eq!(
            decode::<NeighbourMsg>(&bytes, limits(1024 * 1024, 4096)).unwrap(),
            message
        );
    }
}
