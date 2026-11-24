/// A cluster-scope route (an empty app id, the workspace-wide discovery
/// route an agent joins) names no app, so the identity service refuses
/// it keys — and a route left waiting for an answer it cannot get
/// is asked again every second by the key timer for the whole life of the
/// connection. It carries nothing and writes nothing, so it asks for no
/// keys at all and the timer never names it. Nor does it follow the changes
/// the broker hands it, every app's, sealed under keys it never holds:
/// pre-fix it took the op stream and refused every sealed change.
///
/// The broker here pushes channel changes, so the timer asks nothing once
/// every route either holds its keys or is plaintext. Before the join the
/// app's own route settles to holding its keys, which disarms the timer;
/// after a cluster route joins, the timer must stay disarmed and no more
/// question may be asked. Pre-fix the cluster route waits for keys it can
/// never get, which arms the timer and asks it every round.
#[test]
#[cfg(unix)]
fn a_cluster_route_asks_for_no_keys_follows_no_changes_and_polls_nothing() {
    let changes = Arc::new(tokio::sync::watch::Sender::new(0_u64));
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(Captures {
            key_changes: Some(Arc::clone(&changes)),
            ..Captures::default()
        })),
        ..FakeBroker::default()
    };
    let (fd, captures, thread) = stand_up_fake_broker(broker);
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // Like the real service, it refuses keys for a route that names no app.
    let mut manager = certified_manager(
        fd,
        FixtureIdentity {
            asked: Arc::clone(&asked),
            ..FixtureIdentity::answering(|app_id, _| {
                if app_id.is_empty() {
                    return Err(runtime_portal::NotAvailable::ServiceFdMissing);
                }
                Ok(ready_channel_keys())
            })
        },
        harness_presentation(),
        ("so.tilo.test", Uuid::new_v4()),
    );
    // The app's own route learns its keys at its join round and holds
    // them; the timer is then disarmed (a pushing broker asks nothing
    // that holds keys and nothing that is plaintext).
    tilo_test_env::wait_until("the app route's join round to be applied", || {
        !key_schedule(&manager).armed
    });
    let settled = asked.load(std::sync::atomic::Ordering::Acquire);
    assert!(settled > 0, "the app's own route asked for its keys");
    let cluster = RouteSpec::cluster(Uuid::new_v4());
    manager.join_route(
        cluster.clone(),
        collab_types::document::ResourceType::new(""),
    );
    // The join's key round, were there one, has run before this probe is
    // answered (the shovel serves its commands in order, and the probe
    // applies any round still out). A cluster route asks nothing, so the
    // timer stays disarmed and no route was asked for keys.
    let seen = key_schedule(&manager);
    assert!(
        !seen.armed && seen.timer_rounds == 0,
        "a cluster route leaves the key timer armed to ask forever: {seen:?}"
    );
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Acquire),
        settled,
        "a cluster route asked the identity service for keys it cannot be granted"
    );
    let key = cluster.channel_id();
    let topics = || -> Vec<Topic> {
        captures
            .lock()
            .unwrap()
            .subscribed
            .iter()
            .filter(|(route, _)| *route == key)
            .map(|(_, topic)| topic.clone())
            .collect()
    };
    tilo_test_env::wait_until("the cluster route's presence stream", || {
        topics().contains(&Topic::Presence)
    });
    assert!(
        !captures.lock().unwrap().record_reads.contains(&key),
        "a cluster route follows every app's changes, never one record's: {:?}",
        topics()
    );
    drop(manager);
    thread.join().unwrap();
}

/// The identity service lends a join proof for the person's own workspace
/// only to an app that keeps something of theirs there, so an app asks for it
/// when a route there first joins, once, and an app that joins none never
/// asks. Pre-fix every app asked at connect, and each one that keeps nothing
/// of the person's was refused and logged it.
#[test]
#[cfg(unix)]
fn the_persons_own_workspace_is_proved_when_a_route_there_joins() {
    let (fd, captures, thread) = stand_up_fake_broker(FakeBroker::default());
    let root = [1_u8; 32];
    let launch = Uuid::new_v4();
    let personal = collab_types::person_workspace(&root);
    let joins = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut manager = certified_manager(
        fd,
        FixtureIdentity {
            joins: Arc::clone(&joins),
            ..FixtureIdentity::keyed()
        },
        runtime_collab::CertifiedJoinIdentity {
            workspace_id: launch.to_string(),
            device_certificate: serde_json::to_vec(&serde_json::json!({
                "user_public_key": root,
            }))
            .unwrap(),
            proof: vec![0; 64],
        }
        .encode()
        .unwrap(),
        ("so.tilo.test", launch),
    );
    assert_eq!(
        joins.load(std::sync::atomic::Ordering::Acquire),
        0,
        "an app that joined nothing of the person's asked for their workspace's proof"
    );
    let presented = |route: &RouteSpec| {
        let key = route.channel_id();
        let joined = || {
            captures
                .lock()
                .unwrap()
                .joined
                .iter()
                .find(|(joined, _)| *joined == key)
                .map(|(_, identity)| identity.clone())
        };
        tilo_test_env::wait_until("the route in the person's workspace to join", || {
            joined().is_some()
        });
        runtime_collab::CertifiedJoinIdentity::decode(&joined().unwrap())
            .unwrap()
            .unwrap()
            .workspace_id
    };
    for route in [
        RouteSpec::app("so.tilo.test", personal),
        RouteSpec::document("so.tilo.test", personal, "derived:history"),
    ] {
        manager.join_route(route.clone(), collab_types::document::ResourceType::new(""));
        assert_eq!(presented(&route), personal.to_string());
    }
    assert_eq!(
        joins.load(std::sync::atomic::Ordering::Acquire),
        1,
        "the proof is asked once"
    );
    drop(manager);
    thread.join().unwrap();
}
