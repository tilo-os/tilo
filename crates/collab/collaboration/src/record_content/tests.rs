use super::*;

/// The route of record `record` of app `app` in the workspace these tests
/// share.
fn route(app: &str, record: &str) -> super::super::RouteSpec {
    super::super::RouteSpec::record(app, uuid::Uuid::from_u128(1), record)
}

fn record_store(record: &str) -> (ContentStore, Arc<ContentLane>) {
    let lane = ContentLane::of(&route("so.tilo.record-content-test", record));
    (
        RecordContent::store(ContentStore::memory(), Arc::clone(&lane)),
        lane,
    )
}

#[test]
fn a_window_reads_its_record_content_on_the_lane_it_keeps() {
    let scope = crate::WakeScope::new();
    let _entry = scope.enter();
    let kept = ContentLane::of(&route("so.tilo.record-content-kept-lane-test", "a record"));
    keep_lane(&scope.handle(), Arc::clone(&kept));
    assert!(
        Arc::ptr_eq(&ContentLane::of_window(), &kept),
        "the window asks on the lane it keeps, not the process's lane for the record",
    );
}

#[test]
fn two_windows_of_one_record_each_ask_on_their_own_lane() {
    // A package's primary record travels as the route with no record, so a
    // lane per record is one lane for every window showing it: what one
    // window wants, another window would fetch.
    let mine = crate::WakeScope::new();
    let theirs = crate::WakeScope::new();
    let my_lane = {
        let _mine = mine.enter();
        let lane = ContentLane::of_window();
        keep_lane(&mine.handle(), Arc::clone(&lane));
        lane
    };
    let their_lane = {
        let _theirs = theirs.enter();
        let lane = ContentLane::of_window();
        keep_lane(&theirs.handle(), Arc::clone(&lane));
        lane
    };
    assert!(
        !Arc::ptr_eq(&my_lane, &their_lane),
        "each window keeps its own lane, not the process's one for the record",
    );
    let hash = tilo_content::hash_bytes(b"a picture one window shows");
    let my_window = RecordContent::store(ContentStore::memory(), Arc::clone(&my_lane));
    assert_eq!(my_window.read(&hash), None);
    assert_eq!(my_lane.take_requests().wants, vec![hash.clone()]);
    assert!(
        their_lane.take_requests().wants.is_empty(),
        "the other window of the same record asked for nothing",
    );
}

#[test]
fn a_window_that_closed_leaves_the_next_one_its_own_lane() {
    let mut closed = crate::WakeScope::new();
    let closed_lane = {
        let _entry = closed.enter();
        let lane = ContentLane::of_window();
        keep_lane(&closed.handle(), Arc::clone(&lane));
        lane
    };
    closed.close();
    // The lane a closed window kept is not what the next window on this
    // thread reads its record's content on.
    let next = crate::WakeScope::new();
    let _entry = next.enter();
    let next_lane = ContentLane::of_window();
    keep_lane(&next.handle(), Arc::clone(&next_lane));
    assert!(
        !Arc::ptr_eq(&closed_lane, &next_lane),
        "a closed window's lane is not the next window's lane",
    );
    let store = RecordContent::store(ContentStore::memory(), Arc::clone(&next_lane));
    let hash = tilo_content::hash_bytes(b"the next window's picture");
    assert_eq!(store.read(&hash), None);
    assert_eq!(next_lane.take_requests().wants, vec![hash]);
    assert!(
        closed_lane.take_requests().wants.is_empty(),
        "what one window wants is not another window's to fetch",
    );
}

#[test]
fn stored_content_is_offered_on_the_record_route() {
    let (store, lane) = record_store("offered");
    let hash = store.put(b"a picture").unwrap();
    assert!(
        tilo_content::is_sealed(&hash),
        "a record's content is sealed"
    );
    let address = tilo_content::content_address(&hash).unwrap();
    let requests = lane.take_requests();
    let [(offered, sealed)] = requests.offers.as_slice() else {
        panic!("one offer, got {:?}", requests.offers.len());
    };
    assert_eq!(offered, &address, "offered by the ciphertext's address");
    assert!(tilo_content::verify(&address, sealed));
    assert!(
        !sealed.windows(9).any(|w| w == b"a picture"),
        "never in the clear"
    );
    assert!(requests.wants.is_empty());
    assert_eq!(store.read(&hash).as_deref(), Some(&b"a picture"[..]));
}

/// A record's store over a lent store that is itself a record's (an app
/// worker's, which its host joins to its own route) seals once: the bytes
/// below it are kept as they come, under the address the reference names.
#[test]
fn a_record_store_over_a_sealing_store_seals_once() {
    let (lent, inner) = record_store("lent");
    let outer = ContentLane::of(&route("so.tilo.record-content-test", "outer"));
    let store = RecordContent::store(lent.clone(), Arc::clone(&outer));
    let reference = store.put(b"a picture").unwrap();
    let address = tilo_content::content_address(&reference).unwrap();
    assert_eq!(reference.matches('.').count(), 1, "{reference}");
    let [(offered, sealed)] = inner.take_requests().offers.try_into().unwrap();
    assert_eq!(
        offered, address,
        "the lent store keeps it under that address"
    );
    assert_eq!(outer.take_requests().offers, vec![(address, sealed)]);
    assert_eq!(lent.read(&reference).as_deref(), Some(&b"a picture"[..]));
    assert_eq!(store.read(&reference).as_deref(), Some(&b"a picture"[..]));
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
    let offers = lane.take_requests().offers;
    assert_eq!(
        offers
            .iter()
            .map(|(address, _)| address.clone())
            .collect::<Vec<_>>(),
        vec![tilo_content::content_address(&hash).unwrap()]
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
fn a_package_primary_window_asks_on_its_own_lane() {
    let app = "so.tilo.record-content-primary-test";
    let window = ContentLane::of_window();
    let store = RecordContent::store(ContentStore::memory(), Arc::clone(&window));
    let hash = tilo_content::hash_bytes(b"a posted picture");
    assert_eq!(store.read(&hash), None);
    assert_eq!(window.take_requests().wants, vec![hash]);
    assert!(
        ContentLane::of(&route(app, ""))
            .take_requests()
            .wants
            .is_empty(),
        "a package's primary record travels as its route, its lane is the window's",
    );
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

/// A process that carries a record's content for every window (a browser
/// page's replication) reaches each window's own lane through the record's
/// route: what a window asks for is asked, what arrives reaches the window,
/// and a window that let its lane go is not asked again.
#[test]
fn the_records_route_reaches_the_lanes_of_the_windows_showing_it() {
    let app = "so.tilo.record-content-route-test";
    let window = Arc::new(ContentLane::default());
    ContentLane::show_on_route(&route(app, ""), &window);
    let store = RecordContent::store(ContentStore::memory(), Arc::clone(&window));
    let picture = b"a picture the page lacks".to_vec();
    let hash = tilo_content::hash_bytes(&picture);
    assert_eq!(store.read(&hash), None);

    let wanted: Vec<String> = ContentLane::on_route(&route(app, ""))
        .iter()
        .flat_map(|lane| lane.take_requests().wants)
        .collect();
    assert_eq!(
        wanted,
        vec![hash.clone()],
        "the window's want is the route's"
    );

    for lane in ContentLane::on_route(&route(app, "")) {
        lane.arrive(hash.clone(), picture.clone());
    }
    assert_eq!(store.read(&hash), Some(picture), "the window has it");

    drop(store);
    drop(window);
    assert_eq!(
        ContentLane::on_route(&route(app, "")).len(),
        1,
        "only the record's own lane is left once the window let its lane go"
    );
}

#[test]
fn the_same_bytes_stored_twice_on_a_record_of_one_workspace_are_one_item() {
    const APP: &str = "so.tilo.record-content-convergent-test";
    let picture = b"a picture two windows of the workspace place".repeat(500);
    let secret = tilo_seal::SymmetricKey::generate().unwrap();
    let mine = Arc::new(ContentLane::default());
    let theirs = Arc::new(ContentLane::default());
    ContentLane::show_on_route(&route(APP, "a record"), &mine);
    ContentLane::show_on_route(&route(APP, "another record"), &theirs);
    let local = ContentStore::memory();
    let my_window = RecordContent::store(local.clone(), Arc::clone(&mine));
    let their_window = RecordContent::store(local.clone(), Arc::clone(&theirs));
    let before = my_window.put(&picture).unwrap();
    assert_ne!(
        before,
        my_window.put(&picture).unwrap(),
        "without the members' keys, each item has a fresh key"
    );
    mine.take_requests();

    ContentLane::of(&route(APP, "a record")).hold_secret(Some(secret.clone()));
    ContentLane::of(&route(APP, "another record")).hold_secret(Some(secret));
    let reference = my_window.put(&picture).unwrap();
    assert_eq!(my_window.put(&picture).unwrap(), reference);
    assert_eq!(
        their_window.put(&picture).unwrap(),
        reference,
        "another record of the same members names it alike"
    );
    let offers = mine.take_requests().offers;
    assert_eq!(offers.len(), 2);
    assert!(
        offers.iter().all(|(hash, sealed)| Some(hash.as_str())
            == tilo_content::content_address(&reference).as_deref()
            && *sealed == offers[0].1),
        "one ciphertext under one address"
    );
    assert!(
        !offers[0].1.windows(64).any(|w| w == &picture[..64]),
        "sealed all the same"
    );
    assert_eq!(my_window.read(&reference), Some(picture.clone()));
    assert_eq!(
        my_window.read(&before),
        Some(picture.clone()),
        "an item sealed under a fresh key still opens"
    );

    let elsewhere = Arc::new(ContentLane::default());
    // The same record's name, in another workspace.
    let in_another_workspace =
        super::super::RouteSpec::record(APP, uuid::Uuid::from_u128(2), "a record");
    ContentLane::show_on_route(&in_another_workspace, &elsewhere);
    ContentLane::of(&in_another_workspace)
        .hold_secret(Some(tilo_seal::SymmetricKey::generate().unwrap()));
    let other = RecordContent::store(ContentStore::memory(), elsewhere)
        .put(&picture)
        .unwrap();
    assert_ne!(
        tilo_content::content_address(&other),
        tilo_content::content_address(&reference),
        "members of another workspace seal it to other ciphertext"
    );
}

/// One process holds the same app in two workspaces (a tilolang host, a
/// page with two workspaces open): what a window stores in one is sealed
/// under a key derived from that workspace's members' secret alone. Under
/// the other workspace's secret, its members could tell which bytes the
/// first workspace holds by sealing a guess and comparing references.
#[test]
fn one_app_in_two_workspaces_seals_each_workspaces_content_under_its_own_members_secret() {
    const APP: &str = "so.tilo.record-content-two-workspaces-test";
    let primary = |workspace| super::super::RouteSpec::app(APP, uuid::Uuid::from_u128(workspace));
    let (in_a, in_b) = (primary(10), primary(11));
    let (secret_a, secret_b) = (
        tilo_seal::SymmetricKey::generate().unwrap(),
        tilo_seal::SymmetricKey::generate().unwrap(),
    );
    let window_a = Arc::new(ContentLane::default());
    let window_b = Arc::new(ContentLane::default());
    ContentLane::show_on_route(&in_a, &window_a);
    ContentLane::show_on_route(&in_b, &window_b);
    ContentLane::of(&in_a).hold_secret(Some(secret_a.clone()));
    ContentLane::of(&in_b).hold_secret(Some(secret_b.clone()));
    let store_a = RecordContent::store(ContentStore::memory(), Arc::clone(&window_a));
    let store_b = RecordContent::store(ContentStore::memory(), Arc::clone(&window_b));

    let sealed_under = |secret: &tilo_seal::SymmetricKey, bytes: &[u8]| {
        let lane = Arc::new(ContentLane::default());
        lane.hold_secret(Some(secret.clone()));
        RecordContent::store(ContentStore::memory(), lane)
            .put(bytes)
            .unwrap()
    };
    let picture = b"one picture placed in both workspaces".repeat(100);
    for (store, mine, theirs, what) in [
        (&store_a, &secret_a, &secret_b, "A"),
        (&store_b, &secret_b, &secret_a, "B"),
    ] {
        let reference = store.put(&picture).unwrap();
        assert_eq!(
            reference,
            sealed_under(mine, &picture),
            "what is placed in {what} is sealed under {what}'s members' secret"
        );
        assert_ne!(
            reference,
            sealed_under(theirs, &picture),
            "what is placed in {what} is not sealed under the other workspace's secret"
        );
        assert_eq!(store.read(&reference), Some(picture.clone()));
    }
    assert!(
        ContentLane::on_route(&in_a)
            .iter()
            .all(|lane| !Arc::ptr_eq(lane, &window_b)),
        "a window in B is not on A's route"
    );
}
