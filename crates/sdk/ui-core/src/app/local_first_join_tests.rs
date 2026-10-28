// An app opens at once on its local replica and joins its workspace in the
// background. The fixture broker holds the primary join until the test
// answers or refuses it, as a loaded broker does; the room's status is what
// the test says. Nothing here waits on a clock: each step follows an event
// the broker sends.
use super::*;
use crate::collaboration::{CollaborationState, JoiningProbe};

struct Joining {
    app: App,
    owner: crate::bound_dispatch::CallbackBinding,
    broker: JoiningProbe,
    _temp: tempfile::TempDir,
}

/// The runner probe on its own record, which this device holds, joining its
/// room in the background. The room's
/// committed history is the record with `from_room` added by another
/// device. The room starts as `room`.
fn joining(room: runtime_collab::RoomStatus) -> Joining {
    let definition = crate::application_test_support::runner_probe_definition();
    let temp = tempfile::tempdir().unwrap();
    let room_id = uuid::Uuid::new_v4().to_string();
    let mut owners = crate::record::RecordOwners::new(temp.path().into());
    let records = owners.open(&definition.app_id, &room_id).unwrap();
    let views = Rc::new(
        crate::application_view::InstanceViewStore::new(
            temp.path().into(),
            "fixture-device",
            &room_id,
        )
        .unwrap(),
    );
    let context = HostContext::prepare(
        definition,
        HostContextInputs {
            records,
            views,
            device_id: "fixture-device".into(),
            instance_id: 94,
            permissions: crate::application_test_support::runner_permissions(),
            author: crate::application_test_support::author(),
            action_context: crate::application_test_support::provenance(),
        },
        &LaunchTarget::New,
    )
    .unwrap();
    let component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let mut app =
        App::from_component(component, crate::application_test_support::runner_inputs()).unwrap();
    let owner = app.root_widget.callback_binding().unwrap();
    let local = owner
        .records
        .read(&owner.checkout.id, |doc| doc.clone().save())
        .unwrap();
    // Another device's change, committed in the room before this one joins.
    let mut committed = automerge::AutoCommit::load(&local)
        .unwrap()
        .with_actor(automerge::ActorId::random());
    committed.put(ROOT, "from_room", "another device").unwrap();
    // The worker wakes the app it was made for, as a launch makes it.
    let (manager, broker) = {
        let _entry = app.enter_wake_scope();
        CollaborationManager::native_joining_fixture_for_test(
            owner.broker_route().unwrap(),
            definition.record_kind.as_str(),
            committed.save(),
            room,
        )
    };
    app.collaboration.manager = manager;
    Joining {
        app,
        owner,
        broker,
        _temp: temp,
    }
}

impl Joining {
    /// Pump as the window does on each wake, until `done`.
    fn pump_until(&mut self, what: &str, mut done: impl FnMut(&mut App) -> bool) {
        let deadline = tilo_test_env::hang_deadline();
        while !done(&mut self.app) {
            self.app.pump_collab_presence();
            assert!(crate::time::now() < deadline, "never saw {what}");
            std::thread::yield_now();
        }
    }

    fn body(&self, key: &str) -> Option<String> {
        self.owner
            .records
            .read(&self.owner.checkout.id, |doc| {
                doc.get(ROOT, key)
                    .unwrap()
                    .and_then(|(value, _)| value.into_string().ok())
            })
            .unwrap()
    }
}

#[test]
fn an_app_opens_on_its_local_replica_and_its_edits_sync_once_the_broker_answers() {
    let mut probe = joining(runtime_collab::RoomStatus::Ready);
    assert_eq!(
        probe.app.collaboration_state(),
        CollaborationState::Connecting
    );
    assert!(probe.app.collaboration.manager.is_joining());
    // The person edits while the app joins.
    probe
        .app
        .invoke_local_resource_action(
            &tilo_actions::ActionAddress::new("probe:shared", "edit"),
            &serde_json::Map::new(),
        )
        .unwrap();
    probe.app.pump_collab_presence();
    assert_eq!(probe.body("body").as_deref(), Some("edited"));
    assert_eq!(
        probe.app.document_sync_status(),
        None,
        "a join under way that nothing holds up says nothing"
    );
    assert_eq!(
        probe.broker.changes_sent(),
        0,
        "nothing goes out before the route is joined"
    );

    probe.broker.answer();
    probe.pump_until("the route's committed history merged", |app| {
        app.collaboration_state().is_ready() && app.published_action_history_ready()
    });
    assert_eq!(probe.body("from_room").as_deref(), Some("another device"));
    assert_eq!(probe.body("body").as_deref(), Some("edited"));
    let deadline = tilo_test_env::hang_deadline();
    while probe.broker.changes_sent() == 0 {
        probe.app.pump_collab_presence();
        assert!(
            crate::time::now() < deadline,
            "never saw the edit made while joining sent"
        );
        std::thread::yield_now();
    }
}

#[test]
fn the_connecting_line_follows_the_brokers_waiting_and_unreachable_events() {
    let reconnecting = crate::i18n::I18nManager::lookup_plain("history-sync-reconnecting");
    let mut probe = joining(runtime_collab::RoomStatus::Waiting {
        reason: "Placing the workspace on its host".into(),
        detail: None,
    });
    probe.pump_until("the room's wait", |app| {
        app.document_sync_status().as_deref() == Some(reconnecting.as_str())
    });
    assert!(matches!(
        probe.app.collaboration.manager.join_hold(),
        Some(crate::collaboration::JoinHold::RoomWaiting { .. })
    ));

    // The room is ready: the join goes on, and the line says nothing.
    probe.broker.set_room(runtime_collab::RoomStatus::Ready);
    probe.pump_until("the wait to end", |app| {
        app.document_sync_status().is_none()
    });
    assert!(probe.app.collaboration.manager.is_joining());
}

#[test]
fn the_connecting_line_shows_while_the_broker_cannot_be_reached() {
    let reconnecting = crate::i18n::I18nManager::lookup_plain("history-sync-reconnecting");
    let mut probe = joining(runtime_collab::RoomStatus::Waiting {
        reason: "Placing the workspace on its host".into(),
        detail: None,
    });
    probe.pump_until("the room's wait", |app| {
        app.collaboration.manager.join_hold().is_some()
    });
    probe.broker.room_unreachable("the broker went away");
    probe.pump_until("the broker unreachable", |app| {
        matches!(
            app.collaboration.manager.join_hold(),
            Some(crate::collaboration::JoinHold::Unreachable(_))
        )
    });
    assert_eq!(
        probe.app.document_sync_status().as_deref(),
        Some(reconnecting.as_str())
    );
    // The join's answer clears it.
    probe.broker.answer();
    probe.pump_until("the route joined", |app| {
        app.collaboration_state().is_ready()
    });
    assert_eq!(probe.app.collaboration.manager.join_hold(), None);
    assert_ne!(
        probe.app.document_sync_status().as_deref(),
        Some(reconnecting.as_str())
    );
}

#[test]
fn a_refused_join_leaves_the_app_up_with_its_data_and_the_reason_shown() {
    let mut probe = joining(runtime_collab::RoomStatus::Ready);
    probe.broker.refuse();
    probe.pump_until("the refusal", |app| {
        matches!(app.collaboration_state(), CollaborationState::Offline(_))
    });
    let reason = probe
        .app
        .collaboration
        .manager
        .join_refusal()
        .map(str::to_owned)
        .expect("the broker's reason");
    assert!(reason.contains(JoiningProbe::REFUSAL), "{reason}");
    let status = probe.app.document_sync_status().expect("the line says why");
    assert!(status.contains(JoiningProbe::REFUSAL), "{status}");
    // The app keeps working on what this device holds.
    assert_eq!(probe.body("from_room"), None);
    probe
        .app
        .invoke_local_resource_action(
            &tilo_actions::ActionAddress::new("probe:shared", "edit"),
            &serde_json::Map::new(),
        )
        .unwrap();
    probe.app.pump_collab_presence();
    assert_eq!(probe.body("body").as_deref(), Some("edited"));
}
