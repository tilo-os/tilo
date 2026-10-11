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

#[cfg(feature = "native")]
#[test]
fn record_lane_preserves_only_its_backing_stores_current_durability() {
    use tilo_content::ContentDurability;

    let directory = tempfile::tempdir().unwrap();
    let lane = ContentLane::of("so.tilo.record-content-test", "durability");
    let local = ContentStore::in_dir(directory.path().join("content"));
    let store = RecordContent::store(local.clone(), Arc::clone(&lane));
    let bytes = b"approved masked image bytes";
    let hash = store.put(bytes).unwrap();
    assert_eq!(store.durability(&hash).unwrap(), ContentDurability::Durable);
    assert_eq!(
        lane.take_requests().offers,
        vec![(hash.clone(), bytes.to_vec())]
    );

    local.collect_unreferenced(&BTreeSet::new()).unwrap();
    assert_eq!(store.durability(&hash).unwrap(), ContentDurability::Absent);
    assert!(
        lane.take_requests().wants.is_empty(),
        "attesting does not fetch"
    );

    lane.arrive(hash.clone(), b"wrong content".to_vec());
    assert_eq!(store.durability(&hash).unwrap(), ContentDurability::Absent);
    lane.arrive(hash.clone(), bytes.to_vec());
    assert_eq!(store.durability(&hash).unwrap(), ContentDurability::Durable);
    assert_eq!(local.read(&hash).as_deref(), Some(bytes.as_slice()));

    let (memory, memory_lane) = record_store("memory-durability");
    memory_lane.serve();
    let hash = memory.put(bytes).unwrap();
    assert!(memory.shared());
    assert_eq!(
        memory.durability(&hash).unwrap(),
        ContentDurability::Unsupported
    );
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

#[test]
fn a_record_that_is_not_the_windows_own_keeps_its_content_on_its_route() {
    let app = "so.tilo.person-content-test";
    let person = super::super::RouteSpec::record(app, uuid::Uuid::from_u128(7), "history");
    let store = ContentLane::record_store(&person);
    let hash = store.put(b"a poster").unwrap();
    let again = ContentLane::record_store(&person);
    assert_eq!(
        again.read(&hash).as_deref(),
        Some(&b"a poster"[..]),
        "one store per record"
    );
    let routed = ContentLane::routed(app);
    let [(route, lane)] = routed.as_slice() else {
        panic!("one routed lane, got {}", routed.len());
    };
    assert_eq!(route, &person);
    assert_eq!(
        lane.take_requests().offers,
        vec![(hash, b"a poster".to_vec())]
    );
    assert!(
        ContentLane::routed("so.tilo.record-content-test").is_empty(),
        "a window's own record sends on the window's route"
    );
}
