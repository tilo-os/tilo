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

/// Content reaching a record's store moves the count a waiting window
/// compares, whichever way it came; a read that finds nothing does not
/// need to.
#[test]
fn content_reaching_a_store_moves_the_arrival_count() {
    let (store, lane) = record_store("arrivals");
    let bytes = b"arriving later".to_vec();
    let hash = tilo_content::hash_bytes(&bytes);
    // Other tests run in parallel and move the count too: only that it
    // moved is asserted.
    let before = arrivals();
    assert_eq!(store.read(&hash), None);
    lane.arrive(hash.clone(), bytes.clone());
    let arrived = arrivals();
    assert!(arrived > before, "content that arrived on the route counts");
    assert_eq!(store.read(&hash), Some(bytes));

    store.put(b"stored here").unwrap();
    assert!(arrivals() > arrived, "content an app stored counts");
}
