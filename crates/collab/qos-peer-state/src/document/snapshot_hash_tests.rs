use crate::native_footprint_vendor_tests::assert_precharged;
use crate::resources::NativeAllowance;
use collab_types::op::{DocumentMutationEncodeError, MAX_DOCUMENT_MUTATION_BYTES};
use collab_types::{DocumentMutation, DocumentWireVersion};

#[test]
fn snapshot_hash_preserves_literal_all_four_durable_encodings_without_allocation() {
    let allowance = NativeAllowance::new(4096).unwrap();
    for (version, prefix, body) in [
        (
            DocumentWireVersion::QosV1,
            b"qos-document-mutation-v1\0".as_slice(),
            &[0, 2, 0xde, 0xad][..],
        ),
        (
            DocumentWireVersion::QosV2,
            b"qos-document-mutation-v2\0".as_slice(),
            &[1, 0xde, 0xad][..],
        ),
        (
            DocumentWireVersion::TiloV1,
            b"tilo-document-mutation-v1\0".as_slice(),
            &[0, 2, 0xde, 0xad][..],
        ),
        (
            DocumentWireVersion::TiloV2,
            b"tilo-document-mutation-v2\0".as_slice(),
            &[1, 0xde, 0xad][..],
        ),
    ] {
        let literal = [prefix, body].concat();
        assert_eq!(
            DocumentMutation::Snapshot(vec![0xde, 0xad])
                .encode_for(version)
                .unwrap(),
            literal
        );
        let expected = *blake3::hash(&literal).as_bytes();
        assert_eq!(
            assert_precharged(&allowance, || super::snapshot_hash_for(
                &[0xde, 0xad],
                version
            ))
            .unwrap(),
            expected
        );
    }
}

#[test]
fn snapshot_hash_matches_existing_encoder_at_varint_and_size_boundaries() {
    let allowance = NativeAllowance::new(4096).unwrap();
    for length in [0, 1, 127, 128, 16383, 16384, MAX_DOCUMENT_MUTATION_BYTES] {
        let bytes = vec![0x83; length];
        for version in DocumentWireVersion::ALL {
            let expected = *blake3::hash(
                &DocumentMutation::Snapshot(bytes.clone())
                    .encode_for(version)
                    .unwrap(),
            )
            .as_bytes();
            assert_eq!(
                assert_precharged(&allowance, || super::snapshot_hash_for(&bytes, version))
                    .unwrap(),
                expected
            );
        }
    }
}

#[test]
fn snapshot_hash_preserves_too_large_error_without_payload_copy() {
    let bytes = vec![0; MAX_DOCUMENT_MUTATION_BYTES + 1];
    let allowance = NativeAllowance::new(4096).unwrap();
    for version in DocumentWireVersion::ALL {
        assert!(
            matches!(assert_precharged(&allowance, || super::snapshot_hash_for(&bytes,version)), Err(DocumentMutationEncodeError::TooLarge {actual,max}) if actual==bytes.len() && max==MAX_DOCUMENT_MUTATION_BYTES)
        );
        assert!(
            matches!(DocumentMutation::Snapshot(bytes.clone()).encode_for(version), Err(DocumentMutationEncodeError::TooLarge {actual,max}) if actual==bytes.len() && max==MAX_DOCUMENT_MUTATION_BYTES)
        );
    }
}

#[test]
fn snapshot_hash_shared_serializer_preserves_frozen_owned_mutation_wire_oracle() {
    // The pre-extraction DocumentMutation serialization definition. Keep this
    // independent of the new production borrowed serializer.
    // serde_bytes::serialize delegates exactly to Serializer::serialize_bytes.
    // Keep the test oracle independent without adding a qps dependency.
    fn original_bytes<S: serde::Serializer>(
        bytes: &[u8],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(bytes)
    }
    #[derive(serde::Serialize)]
    #[serde(rename = "DocumentMutation", deny_unknown_fields)]
    enum OriginalMutation {
        Snapshot(#[serde(serialize_with = "original_bytes")] Vec<u8>),
        Incremental(#[serde(serialize_with = "original_bytes")] Vec<u8>),
    }
    for length in [0, 1, 127, 128, 16383, 16384] {
        let bytes = vec![0x91; length];
        for (original, current) in [
            (
                OriginalMutation::Snapshot(bytes.clone()),
                DocumentMutation::Snapshot(bytes.clone()),
            ),
            (
                OriginalMutation::Incremental(bytes.clone()),
                DocumentMutation::Incremental(bytes.clone()),
            ),
        ] {
            assert_eq!(
                postcard::to_allocvec(&original).unwrap(),
                postcard::to_allocvec(&current).unwrap()
            );
            assert_eq!(
                serde_json::to_vec(&original).unwrap(),
                serde_json::to_vec(&current).unwrap()
            );
        }
    }
}
