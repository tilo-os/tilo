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

#[test]
fn a_package_primary_window_asks_on_its_route_without_a_record() {
    let app = "so.tilo.record-content-primary-test";
    let window = ContentLane::of_record(app, &crate::record::RecordId::package_primary());
    assert!(
        Arc::ptr_eq(&window, &ContentLane::of(app, "")),
        "the route of a package's primary record carries no record",
    );
    let store = RecordContent::store(ContentStore::memory(), window);
    let hash = tilo_content::hash_bytes(b"a posted picture");
    assert_eq!(store.read(&hash), None);
    assert_eq!(ContentLane::of(app, "").take_requests().wants, vec![hash]);
}

#[test]
fn missing_content_reads_where_its_host_says_the_want_stands() {
    let (store, lane) = record_store("standing");
    let bytes = b"bytes on a device that is offline".to_vec();
    let hash = tilo_content::hash_bytes(&bytes);
    assert_eq!(store.read(&hash), None);
    let looked = arrivals();

    lane.stands(hash.clone(), ContentState::Waiting);
    assert_eq!(store.state(&hash), ContentState::Waiting);
    assert_ne!(arrivals(), looked, "a window waiting for it looks again");

    lane.stands(hash.clone(), ContentState::Pending);
    assert_eq!(
        store.state(&hash),
        ContentState::Pending,
        "a holder connected"
    );

    lane.arrive(hash.clone(), bytes.clone());
    assert_eq!(store.read(&hash), Some(bytes));
    assert_eq!(store.state(&hash), ContentState::Resolved);

    // Content nobody here asked for has no standing.
    let other = tilo_content::hash_bytes(b"never asked");
    lane.stands(other.clone(), ContentState::Unavailable);
    assert_eq!(lane.standing(&other), None);
}

#[test]
fn content_reads_arrive_from_the_room_and_content_here_is_ready_at_once() {
    use crate::arriving::Arrival;
    let (store, lane) = record_store("read-as-arriving");
    let here = store.put(b"a picture here").unwrap();
    let reads = ContentReads::new(store);
    assert_eq!(
        reads.read(&here).get(),
        Arrival::Ready(()),
        "content on this device opens complete"
    );

    let bytes = b"bytes on another device".to_vec();
    let away = tilo_content::hash_bytes(&bytes);
    let read = reads.read(&away);
    assert!(read.get().is_pending(), "on its way from the room");
    assert!(
        lane.take_requests().wants.contains(&away),
        "the first read asks the room for it"
    );
    lane.stands(away.clone(), ContentState::Unavailable);
    assert_eq!(reads.read(&away).get(), Arrival::Failed(ContentUnavailable));
    lane.stands(away.clone(), ContentState::Waiting);
    assert!(reads.read(&away).get().is_pending());
    lane.arrive(away.clone(), bytes);
    assert_eq!(reads.read(&away).get(), Arrival::Ready(()));
    assert_eq!(read.get(), Arrival::Ready(()), "the one read answers");
}
