use super::*;
use automerge::transaction::Transactable;
use automerge::{ROOT, ReadDoc, SaveOptions};

fn stored(value: &str) -> Vec<u8> {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "value", value).unwrap();
    doc.commit();
    doc.save_with_options(SaveOptions {
        deflate: false,
        retain_orphans: true,
    })
}

#[test]
fn documents_share_one_aggregate_byte_limit() {
    let first = stored("first");
    let second = stored("second");
    let mut batch = NativeBatch::new(first.len() + second.len() - 1);
    let document = batch.load(&first).unwrap();
    assert_eq!(
        document
            .document()
            .get(ROOT, "value")
            .unwrap()
            .unwrap()
            .0
            .into_string()
            .unwrap(),
        "first"
    );
    assert_eq!(batch.load(&second).err(), Some(LoadError::Capacity));
    // Dropping a document does not refund its share of the limit.
    drop(document);
    assert_eq!(batch.load(&second).err(), Some(LoadError::Capacity));
}

#[test]
fn malformed_storage_is_rejected_by_the_isolated_load() {
    let mut bytes = stored("value");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    assert!(matches!(
        NativeBatch::new(1024 * 1024).load(&bytes),
        Err(LoadError::Rejected(_))
    ));
}
