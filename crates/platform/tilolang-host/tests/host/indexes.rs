//! The indexes a host keeps for the windows it runs: one per workspace record, read by every
//! window of it.

use super::*;

/// Two windows of one app in one host share the record's index and both read it: the second reads
/// what the first built rather than building it again, and nothing is kept per window. Copies an
/// earlier build kept per window are removed.
#[test]
fn two_windows_of_one_app_in_a_host_share_one_index_and_both_read_it() {
    let storage = tempfile::tempdir().unwrap();
    let route = RouteSpec::app(APP, uuid::Uuid::new_v4());
    let workspace = route.cluster_id.to_string();
    let record_dir = storage
        .path()
        .join(APP)
        .join("indexes")
        .join("rooms")
        .join(&workspace);
    let old_copy = record_dir.join("instance-1");
    std::fs::create_dir_all(&old_copy).unwrap();
    std::fs::write(old_copy.join(format!("{APP}.index")), b"").unwrap();
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
        // Both windows' frames, until the host owes none: the second instance opens while the
        // first runs.
        let mut shown: Vec<&mut Window> = windows.iter_mut().collect();
        settle_all(&mut host, &mut shown, |_| false);
    }
    let instances: Vec<&Hosted> = host.instances().collect();
    assert_eq!(
        instances.len(),
        2,
        "both windows of the app run in the host"
    );
    let mut builds = Vec::new();
    for instance in &instances {
        assert_eq!(value_of(instance), 0, "the window reads the counter");
        let store = instance.instance().host().unwrap();
        let store = store.store();
        assert!(store.index_ready(), "the window reads the index");
        assert!(
            store.index_contents(APP).is_some(),
            "and reads its rows from it"
        );
        builds.push(store.stats().builds);
    }
    builds.sort_unstable();
    assert!(
        builds[0] == 0 && builds[1] > 0,
        "one window built the indexes, and the other read what it built: {builds:?}"
    );
    assert!(
        record_dir.join(format!("{APP}.index")).is_file(),
        "the record's index is kept"
    );
    assert!(!old_copy.exists(), "a window's own copy is gone");
}
