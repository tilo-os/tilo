//! The indexes a host keeps for the windows it runs: one file per instance, open to that
//! instance's store alone, so two windows of one app both read their own rows.

use super::*;

/// [`settle`], every window of `windows` taken: two windows of one app in one host both wait for
/// their instance's frames.
fn settle_all(host: &mut TilolangHost, windows: &mut [Window]) {
    let mut quiet_since = std::time::Instant::now();
    loop {
        let woken = WOKEN.swap(false, std::sync::atomic::Ordering::SeqCst);
        let changed = host.turn();
        for window in &mut *windows {
            window.take();
        }
        if changed || woken {
            quiet_since = std::time::Instant::now();
        } else if quiet_since.elapsed() > std::time::Duration::from_millis(300) {
            return;
        } else {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

/// Two windows of one app in one host keep an index each and both read it. An index file is open
/// to one store at a time (redb holds it), so a window that shared another's could not open it and
/// would build its rows in memory at every open.
#[test]
fn two_windows_of_one_app_in_a_host_each_keep_their_own_index_and_read_it() {
    let storage = tempfile::tempdir().unwrap();
    let route = RouteSpec::app(APP, uuid::Uuid::new_v4());
    let workspace = route.cluster_id.to_string();
    let index_file = |instance: u64| {
        storage
            .path()
            .join(APP)
            .join("indexes")
            .join("rooms")
            .join(&workspace)
            .join(format!("instance-{instance}"))
            .join(format!("{APP}.index"))
    };
    let mut host = TilolangHost::new(Some(storage.path().to_owned()), wake());
    let mut windows = Vec::new();
    for instance in [1, 2] {
        let (window, _sent, _inbox) = instance_on(
            &mut host,
            spec(instance, "ada", &route),
            counter(),
            &route,
            Rc::default(),
            Rc::default(),
            automerge::AutoCommit::new(),
        );
        windows.push(window);
    }
    settle_all(&mut host, &mut windows);
    let instances: Vec<&Hosted> = host.instances().collect();
    assert_eq!(
        instances.len(),
        2,
        "both windows of the app run in the host"
    );
    for instance in &instances {
        assert_eq!(value_of(instance), 0, "the window reads the counter");
        let store = instance.instance().host().unwrap();
        let store = store.store();
        assert!(store.index_ready(), "the window reads an index of its own");
        assert!(
            store.index_contents(APP).is_some(),
            "and reads its rows from it"
        );
        assert!(
            store.stats().builds > 0,
            "the window built the index it keeps"
        );
    }
    // The file each window keeps is where its own instance keeps it: the second window did not
    // open the first's (it would have been told the index is open elsewhere, and built its rows in
    // memory with nothing on disk).
    assert!(index_file(1).is_file(), "one window's index is kept");
    assert!(index_file(2).is_file(), "and the other's beside it");
}
