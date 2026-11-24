// An app opens at once on its local replica and joins its workspace in the
// background. The fixture broker holds the primary join until the test
// answers or refuses it, as a loaded broker does; the workspace's status is what
// the test says. Nothing here waits on a clock: each step follows an event
// the broker sends.
use super::*;
use crate::collaboration::{CollaborationState, JoiningProbe};

struct Joining {
    app: App,
    owner: crate::bound_dispatch::CallbackBinding,
    broker: JoiningProbe,
}

/// The runner probe on its own record, which this device holds, joining its
/// workspace in the background. The workspace's
/// committed history is the record with `from_peer` added by another
/// device. The workspace starts as `workspace_id`.
fn joining(workspace: runtime_collab::WorkspaceStatus) -> Joining {
    let definition = crate::application_test_support::runner_probe_definition();
    let workspace_id = uuid::Uuid::new_v4().to_string();
    let mut owners = crate::resource::ResourceOwners::writing(Rc::new(
        crate::resource::MemoryWorkspace::default(),
    ));
    let records = owners.open(&definition.app_id, &workspace_id).unwrap();
    let views = Rc::new(
        crate::application_view::InstanceViewStore::in_memory(
            &definition.app_id,
            "fixture-device",
            &workspace_id,
        )
        .unwrap(),
    );
    let context = ProcessContext::prepare(
        definition,
        ProcessContextInputs {
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
    let owner = crate::bound_dispatch::callback_binding(&*app.root_widget).unwrap();
    let local = owner
        .records
        .read(&owner.checkout.id, |doc| doc.clone().save())
        .unwrap();
    // Another device's change, committed in the workspace before this one joins.
    let mut committed = automerge::AutoCommit::load(&local)
        .unwrap()
        .with_actor(automerge::ActorId::random());
    committed.put(ROOT, "from_peer", "another device").unwrap();
    // The worker wakes the app it was made for, as a launch makes it.
    let (manager, broker) = {
        let _entry = app.enter_wake_scope();
        CollaborationManager::native_joining_fixture_for_test(
            owner.broker_route().unwrap(),
            definition.resource_type.as_str(),
            committed.save(),
            workspace,
        )
    };
    app.collaboration.manager = manager;
    Joining { app, owner, broker }
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

tilo_test_env::scenario! {
    fn an_app_opens_on_its_local_replica_whatever_the_broker_answers;

    phase an_app_opens_on_its_local_replica_and_its_edits_sync_once_the_broker_answers;
    {
        let mut probe = joining(runtime_collab::WorkspaceStatus::Ready);
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
        assert_eq!(probe.body("from_peer").as_deref(), Some("another device"));
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
        // The broker refuses a change that names no action of this device, and
        // the workspace's committed history is not this device's to send: every
        // incremental that reached it names its action here.
        let route = probe.owner.broker_route().unwrap();
        for (sent_route, request) in &probe.app.document_routes.sent {
            if *sent_route != route {
                continue;
            }
            if let crate::collaboration::CollabRequest::Document {
                mutation: collab_types::DocumentMutation::Incremental(bytes),
                ..
            } = request
            {
                let changes = automerge::Automerge::parse_changes(bytes).unwrap();
                collab_types::claim::check_named(&changes).unwrap();
            }
        }
    }

    phase the_connecting_line_follows_the_brokers_waiting_and_unreachable_events;
    {
        let reconnecting = crate::i18n::I18nManager::lookup_plain("history-sync-reconnecting");
        let mut probe = joining(runtime_collab::WorkspaceStatus::Waiting {
            reason: "Placing the workspace on its host".into(),
            detail: None,
        });
        probe.pump_until("the workspace's wait", |app| {
            app.document_sync_status().as_deref() == Some(reconnecting.as_str())
        });
        assert!(matches!(
            probe.app.collaboration.manager.join_hold(),
            Some(crate::collaboration::JoinHold::WorkspaceWaiting { .. })
        ));

        // The workspace is ready: the join goes on, and the line says nothing.
        probe
            .broker
            .set_workspace_status(runtime_collab::WorkspaceStatus::Ready);
        probe.pump_until("the wait to end", |app| {
            app.document_sync_status().is_none()
        });
        assert!(probe.app.collaboration.manager.is_joining());
    }

    phase the_connecting_line_shows_while_the_broker_cannot_be_reached;
    {
        let reconnecting = crate::i18n::I18nManager::lookup_plain("history-sync-reconnecting");
        let mut probe = joining(runtime_collab::WorkspaceStatus::Waiting {
            reason: "Placing the workspace on its host".into(),
            detail: None,
        });
        probe.pump_until("the workspace's wait", |app| {
            app.collaboration.manager.join_hold().is_some()
        });
        probe.broker.workspace_unreachable("the broker went away");
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

    phase a_refused_join_leaves_the_app_up_with_its_data_and_the_reason_shown;
    {
        let mut probe = joining(runtime_collab::WorkspaceStatus::Ready);
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
        assert_eq!(probe.body("from_peer"), None);
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

    phase an_action_asked_while_the_app_joins_goes_out_once_it_joined;
    {
        let mut probe = joining(runtime_collab::WorkspaceStatus::Ready);
        let invoke = crate::collaboration::CollabRequest::RemoteAction {
            message: tilo_actions::remote::RemoteActionMessage::Invoke(
                tilo_actions::remote::InvokeRequest {
                    on_behalf_of: None,
                    invocation: tilo_actions::InvocationId::new(),
                    target: tilo_actions::remote::person_vault::target(
                        tilo_actions::remote::person_vault::LIST,
                    ),
                    argument: tilo_actions::remote::InvokeArgument::None,
                    context: None,
                    confirmation: None,
                    revision: None,
                    wait_ms: None,
                    appearance: None,
                    signed: None,
                },
            ),
            press: None,
        };
        probe.app.collaboration.held_requests.push_back(invoke);
        probe.app.pump_collab_presence();
        assert_eq!(
            probe.app.collaboration_state(),
            CollaborationState::Connecting
        );
        assert_eq!(
            probe.app.collaboration.held_requests.len(),
            1,
            "the join carries nothing else yet, and a dropped invocation is never answered"
        );
        probe.broker.answer();
        probe.pump_until("the held invocation sent", |app| {
            app.collaboration_state().is_ready() && app.collaboration.held_requests.is_empty()
        });
    }
}
