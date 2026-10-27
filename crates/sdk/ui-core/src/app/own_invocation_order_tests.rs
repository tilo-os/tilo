// An app's invocation of its own action reaches the session action host only
// after the publication that offers it, on the same socket-backed route. The
// fixture's host resolves an own invocation against the surface it last
// received, as the session's does.
use super::*;

type Invocations = Box<dyn Fn() -> Vec<(tilo_actions::ActionAddress, bool)>>;

struct Probe {
    app: App,
    owner: crate::bound_dispatch::CallbackBinding,
    invocations: Invocations,
    /// A record the window keeps beside the one it shows, when it keeps one.
    beside: Option<crate::record::workspace::WorkspaceRecord>,
    _temp: tempfile::TempDir,
}

/// A window that keeps a record open beside what it shows, as every
/// framework window keeps the workspace's emoji.
struct KeepsBeside {
    content: Box<dyn crate::widget::Widget>,
    owner: crate::history_mode::HistoryOwner,
}

impl crate::widget::Decorator for KeepsBeside {
    type Inner = dyn crate::widget::Widget;
    fn inner(&self) -> &(dyn crate::widget::Widget + 'static) {
        self.content.as_ref()
    }
    fn inner_mut(&mut self) -> &mut (dyn crate::widget::Widget + 'static) {
        self.content.as_mut()
    }
    fn record_owners(&self) -> Vec<crate::history_mode::HistoryOwner> {
        let mut owners = self.content.record_owners();
        owners.push(self.owner.clone());
        owners
    }
}

/// The runner probe on its own record route, with the route's original
/// history not yet applied: the app has not pumped since it connected.
fn probe() -> Probe {
    probe_keeping_beside(false)
}

/// [`probe`]; with `beside`, the window also keeps the room's workspace
/// emoji record beside its own, on a route whose reads the host refuses,
/// so it never opens.
fn probe_keeping_beside(beside: bool) -> Probe {
    let definition = crate::application_test_support::runner_probe_definition();
    let temp = tempfile::tempdir().unwrap();
    let room = uuid::Uuid::new_v4().to_string();
    let mut owners = crate::record::RecordOwners::new(temp.path().into());
    let records = owners.open(&definition.app_id, &room).unwrap();
    let views = Rc::new(
        crate::application_view::InstanceViewStore::new(
            temp.path().into(),
            "fixture-device",
            &room,
        )
        .unwrap(),
    );
    let context = HostContext::prepare(
        definition,
        HostContextInputs {
            records,
            views,
            device_id: "fixture-device".into(),
            instance_id: 93,
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
    let beside = beside.then(|| {
        crate::record::workspace::WorkspaceRecords::in_room(
            owners.clone(),
            room.clone(),
            crate::application_test_support::author(),
        )
        .open_record("so.tilo.chat", "tilo.workspace.emoji", &|_| Ok(()))
        .unwrap()
    });
    if let Some(record) = &beside {
        let content =
            std::mem::replace(&mut app.root_widget, Box::new(crate::widget::Text::new("")));
        app.root_widget = Box::new(KeepsBeside {
            content,
            owner: record.history_owner(),
        });
    }
    let refused: Vec<_> = beside.iter().filter_map(|record| record.route()).collect();
    let original = owner
        .records
        .read(&owner.checkout.id, |doc| doc.clone().save())
        .unwrap();
    let (manager, invocations) =
        CollaborationManager::native_initial_named_invocation_fixture_for_test(
            owner.broker_route().unwrap(),
            original,
            &refused,
        );
    app.collaboration.manager = manager;
    Probe {
        app,
        owner,
        invocations,
        beside,
        _temp: temp,
    }
}

impl Probe {
    /// The probe's Effect, which the host admits before it runs here.
    fn invoke_effect(&mut self) -> tilo_actions::ActionAddress {
        let started = self
            .app
            .invoke_local_resource_action(
                &tilo_actions::ActionAddress::new("probe:shared", "effect"),
                &serde_json::Map::new(),
            )
            .unwrap();
        assert!(matches!(
            started,
            crate::widget::ResourceOperationStart::Accepted
        ));
        tilo_actions::ActionAddress::new(
            self.app
                .actions
                .bound_publication
                .alias(&self.owner, "probe:shared")
                .unwrap(),
            "effect",
        )
    }

    /// Pump as the window does until the host has answered every request.
    fn pump_until_answered(&mut self) {
        let deadline = tilo_test_env::hang_deadline();
        while !self.app.actions.broker_requests.is_empty() {
            self.app.pump_collab_presence();
            if let Some(reason) = self.app.controlled_refused_action_for_test() {
                panic!("the host refused the probe's own action: {reason}");
            }
            assert!(
                crate::time::now() < deadline,
                "the host never answered the probe's own action"
            );
            std::thread::yield_now();
        }
    }
}

#[test]
fn an_own_invocation_made_before_its_route_is_ready_follows_the_surface_that_offers_it() {
    let mut probe = probe();
    assert!(probe.app.collaboration.manager.is_connected());
    assert!(
        !probe.app.published_action_history_ready(),
        "the route's original history must not be applied yet"
    );
    let first = probe.invoke_effect();
    let second = probe.invoke_effect();
    assert!(
        (probe.invocations)().is_empty(),
        "nothing reaches the host before the surface that offers it"
    );
    probe.pump_until_answered();
    assert!(probe.app.published_action_history_ready());
    assert_eq!(
        (probe.invocations)(),
        vec![(first, true), (second, true)],
        "each invocation arrives once, in order, offered by the surface before it"
    );
    // Once the route is ready an invocation goes out as it is made: it waits
    // on nothing.
    let third = probe.invoke_effect();
    assert!(probe.app.actions.held_own_invocations.is_empty());
    probe.pump_until_answered();
    assert_eq!((probe.invocations)().last(), Some(&(third, true)));
}

#[test]
fn an_own_invocation_waiting_on_a_route_that_closes_fails_with_the_route_error() {
    let mut probe = probe();
    assert!(!probe.app.published_action_history_ready());
    probe.invoke_effect();
    probe.app.collaboration.manager.fail_publication_for_test();
    assert!(!probe.app.collaboration.manager.is_connected());
    let closed = probe.app.collaboration.manager.state();
    let route_error = closed.reason();
    probe.app.pump_collab_presence();
    let reason = probe
        .app
        .controlled_refused_action_for_test()
        .expect("a held invocation must be answered when its route closes");
    assert!(
        reason.contains(route_error),
        "the refusal must carry the route's own error: {reason}"
    );
    assert!(
        !reason.contains("no such action is offered"),
        "a closed route is not an unknown action: {reason}"
    );
    assert!(probe.app.actions.broker_requests.is_empty());
    assert!(probe.app.actions.held_own_invocations.is_empty());
    assert!((probe.invocations)().is_empty());
}

/// A record kept beside the shown one is read context: the window's
/// actions never act through it, so its route holds back neither the
/// surface that offers them nor an accepted invocation, even while it
/// never opens.
#[test]
fn a_window_whose_beside_record_never_opens_still_offers_its_actions() {
    let mut probe = probe_keeping_beside(true);
    let beside = probe
        .beside
        .as_ref()
        .and_then(crate::record::workspace::WorkspaceRecord::route)
        .expect("the record kept beside syncs on its own route");
    let first = probe.invoke_effect();
    probe.pump_until_answered();
    assert_eq!(
        (probe.invocations)(),
        vec![(first, true)],
        "the surface went out and offered the invoked action"
    );
    assert!(
        probe
            .app
            .document_routes
            .owners
            .iter()
            .any(|entry| entry.route == beside),
        "the window joined the beside record's route"
    );
    assert!(
        !probe
            .app
            .collaboration
            .manager
            .document_route_publishable_for_test(&beside),
        "the beside record's route never opened"
    );
    assert!(probe.app.published_action_history_ready());
    let waited_on = probe
        .app
        .collaboration
        .manager
        .invocation_document_routes_for_test();
    assert!(
        waited_on.contains(&probe.owner.broker_route().unwrap().channel_id()),
        "an accepted invocation waits for the record it acts through"
    );
    assert!(
        !waited_on.contains(&beside.channel_id()),
        "an accepted invocation never waits for a record only kept beside"
    );
}
