use collab_types::DocumentMutation;
use collab_types::op::{DocumentMutationEncodeError, MAX_DOCUMENT_MUTATION_BYTES};

#[test]
fn snapshot_hash_preserves_literal_durable_encoding() {
    let literal = [b"tilo-document-mutation-v2\0".as_slice(), &[1, 0xde, 0xad]].concat();
    assert_eq!(
        DocumentMutation::Snapshot(vec![0xde, 0xad])
            .encode()
            .unwrap(),
        literal
    );
    assert_eq!(
        super::snapshot_hash(&[0xde, 0xad]).unwrap(),
        *blake3::hash(&literal).as_bytes()
    );
}

#[test]
fn snapshot_hash_matches_existing_encoder_at_varint_and_size_boundaries() {
    for length in [0, 1, 127, 128, 16383, 16384, MAX_DOCUMENT_MUTATION_BYTES] {
        let bytes = vec![0x83; length];
        let expected =
            *blake3::hash(&DocumentMutation::Snapshot(bytes.clone()).encode().unwrap()).as_bytes();
        assert_eq!(super::snapshot_hash(&bytes).unwrap(), expected);
    }
}

#[test]
fn snapshot_hash_preserves_too_large_error() {
    let bytes = vec![0; MAX_DOCUMENT_MUTATION_BYTES + 1];
    assert!(
        matches!(super::snapshot_hash(&bytes), Err(DocumentMutationEncodeError::TooLarge {actual,max}) if actual==bytes.len() && max==MAX_DOCUMENT_MUTATION_BYTES)
    );
    assert!(
        matches!(DocumentMutation::Snapshot(bytes.clone()).encode(), Err(DocumentMutationEncodeError::TooLarge {actual,max}) if actual==bytes.len() && max==MAX_DOCUMENT_MUTATION_BYTES)
    );
}

#[test]
fn document_mutation_serde_matches_independent_wire_oracle() {
    // An independent definition of the DocumentMutation serde shape.
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
