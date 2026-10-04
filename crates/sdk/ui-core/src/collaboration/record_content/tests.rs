use super::*;

fn record_store(record: &str) -> (ContentStore, Arc<ContentLane>) {
    let lane = ContentLane::of("so.tilo.record-content-test", record);
    (
        RecordContent::store(ContentStore::memory(), Arc::clone(&lane)),
        lane,
    )
}

#[test]
fn stored_content_is_offered_on_the_record_route() {
    let (store, lane) = record_store("offered");
    let hash = store.put(b"a picture").unwrap();
    let requests = lane.take_requests();
    assert_eq!(requests.offers, vec![(hash.clone(), b"a picture".to_vec())]);
    assert!(requests.wants.is_empty());
    assert_eq!(store.read(&hash).as_deref(), Some(&b"a picture"[..]));
}

#[test]
fn missing_content_is_asked_for_once_and_reads_pending_until_it_arrives() {
    let (store, lane) = record_store("wanted");
    let bytes = b"bytes on another device".to_vec();
    let hash = tilo_content::hash_bytes(&bytes);
    assert_eq!(store.read(&hash), None);
    assert_eq!(store.state(&hash), ContentState::Pending);
    assert_eq!(store.read(&hash), None);
    assert_eq!(
        lane.take_requests().wants,
        vec![hash.clone()],
        "asked once, however often it is read"
    );

    // Bytes that are not the content are dropped.
    lane.arrive(hash.clone(), b"forged".to_vec());
    assert_eq!(store.read(&hash), None);
    assert_eq!(store.state(&hash), ContentState::Pending);

    lane.arrive(hash.clone(), bytes.clone());
    assert_eq!(store.read(&hash), Some(bytes));
    assert_eq!(store.state(&hash), ContentState::Resolved);
}

#[test]
fn each_record_has_its_own_lane() {
    let (store, _) = record_store("one-record");
    let (_, other) = record_store("another-record");
    let hash = tilo_content::hash_bytes(b"x");
    assert_eq!(store.read(&hash), None);
    assert!(other.take_requests().wants.is_empty());
}
