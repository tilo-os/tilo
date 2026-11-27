//! Synthetic execution fixtures: no model, transport grants, UI input or capture.
use super::*;
use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};
use tilo_action_policy::ActingPolicy as _;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, PendingPoll, RemoteActionTarget,
};

fn fixture() -> (
    Arc<AppBrokerState>,
    Caller,
    mpsc::UnboundedReceiver<InvocationEnvelope>,
) {
    fixture_tagged(Vec::new())
}

/// [`fixture`] with the published action's contract carrying `tags`.
fn fixture_tagged(
    tags: Vec<String>,
) -> (
    Arc<AppBrokerState>,
    Caller,
    mpsc::UnboundedReceiver<InvocationEnvelope>,
) {
    fixture_with(tags, Vec::new())
}

/// [`fixture_tagged`], the action taking `params`.
fn fixture_with(
    tags: Vec<String>,
    params: Vec<tilo_actions::ActionParam>,
) -> (
    Arc<AppBrokerState>,
    Caller,
    mpsc::UnboundedReceiver<InvocationEnvelope>,
) {
    fixture_granting(tags, params, |_| Vec::new())
}

/// [`fixture_tagged`], the action taking `params`, the workspace holding the
/// grants `grants` gives for its workspace.
fn fixture_granting(
    tags: Vec<String>,
    params: Vec<tilo_actions::ActionParam>,
    grants: impl FnOnce(&str) -> Vec<tilo_action_policy::Grant>,
) -> (
    Arc<AppBrokerState>,
    Caller,
    mpsc::UnboundedReceiver<InvocationEnvelope>,
) {
    fixture_owned_by(&"a1".repeat(32), tags, params, grants)
}

/// [`fixture_granting`], the workspace owned by `owner`, who is the caller it returns.
fn fixture_owned_by(
    owner: &str,
    tags: Vec<String>,
    params: Vec<tilo_actions::ActionParam>,
    grants: impl FnOnce(&str) -> Vec<tilo_action_policy::Grant>,
) -> (
    Arc<AppBrokerState>,
    Caller,
    mpsc::UnboundedReceiver<InvocationEnvelope>,
) {
    let mut workspace = Workspace::new(WorkspaceId::from_u128(52), "Synthetic execution presence");
    workspace.policy.set_owner(owner);
    let workspace_id = workspace.id().to_string();
    workspace.policy.grants.extend(grants(&workspace_id));
    let mut tree = WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    let membership = tree.shared();
    let broker =
        AppBrokerState::test_broker_with_authority(membership.authority(), membership.services());
    let state = Arc::clone(broker.state());
    state.replace_published_surfaces_for_test(vec![PublishedSurface {
        peer_id: 7,
        app_id: "so.tilo.test".into(),
        route: CrdtRoute {
            app_id: "so.tilo.test".into(),
            cluster_id: workspace_id.clone(),
            document_id: "record-a".into(),
        },
        actions: vec![PublishedAction {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            resource: "test:block-a".into(),
            action: "update".into(),
            resource_kind: "test.block".into(),
            resource_label: "Block".into(),
            action_label: "Update".into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            operation_kind: tilo_actions::OperationKind::Effect,
            params,
            contract: tilo_actions::ActionContract {
                execution: tilo_actions::ExecutionMode::Background { cancellable: true },
                result: tilo_actions::ResultSpec::Job,
                tags,
                ..Default::default()
            },
            input: PublishedInput::None,
            view: None,
        }],
        live: true,
    }]);
    let receiver = state.capture_invocations_for_test(7);
    // This fixture consumes the full InvocationEnvelope UUID, like a stable subscriber.
    state
        .stable_invocation_subscribers
        .lock()
        .unwrap()
        .insert(7);
    let caller = Caller::local_person(owner)
        .in_workspace(workspace_id)
        .in_new_execution();
    (state, caller, receiver)
}

fn request() -> InvokeRequest {
    InvokeRequest {
        asks: false,
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: "7".into(),
            address: tilo_actions::ActionAddress::new("test:block-a", "update"),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

#[tokio::test]
async fn only_admitted_execution_and_validated_terminal_emit_activity() {
    let (state, caller, mut app) = fixture();
    let mut events = state.subscribe_execution_activity();
    let _ = state
        .action_query(&caller, &CatalogueQuery::default())
        .poll_ready();
    assert!(events.try_recv().is_err(), "resource discovery is not work");
    let request = request();
    assert!(matches!(
        state
            .action_invoke(&caller, None, None, &request)
            .poll_ready(),
        PendingPoll::Ready(Ok(_))
    ));
    let delivered = app.try_recv().unwrap();
    let accepted = events.try_recv().unwrap();
    assert_eq!(accepted.execution, caller.execution_id().unwrap());
    assert_eq!(accepted.invocation, request.invocation);
    assert_eq!(
        accepted.actor,
        tilo_actions::Actor::from(tilo_actions::MemberId::person("a1".repeat(32)))
    );
    assert_eq!(accepted.peer_id, 7);
    assert_eq!(accepted.route.document_id, "record-a");
    assert_eq!(accepted.address, request.target.address);
    assert_eq!(accepted.status, ExecutionStatus::Accepted);
    assert!(matches!(
        state
            .action_invoke(&caller, None, None, &request)
            .poll_ready(),
        PendingPoll::Ready(Ok(_))
    ));
    assert!(
        events.try_recv().is_err(),
        "a receipt retry is not a new operation"
    );
    assert!(app.try_recv().is_err());
    state
        .apply_invocation_update(
            7,
            delivered.id,
            tilo_actions::InvocationUpdate::Failed {
                invocation: delivered.invocation,
                error: tilo_actions::ActionFailure {
                    code: "synthetic_failure".into(),
                    message: "Synthetic failure".into(),
                    details: None,
                    retryable: false,
                },
            },
            None,
        )
        .await
        .unwrap();
    let failed = events.try_recv().unwrap();
    assert_eq!(failed.status, ExecutionStatus::Failed);
    assert_eq!(failed.invocation, accepted.invocation);
    assert_eq!(failed.execution, accepted.execution);
    assert!(failed.sequence > accepted.sequence);
    assert!(accepted.source_time_ms > 0);
    assert!(failed.source_time_ms > 0);
    assert!(events.try_recv().is_err());
}

/// An invocation of an action whose contract declares untrusted content (a
/// web page's text) is observed as untrusted while it runs and once it has
/// finished, so every caller reading its answer can tell; any other
/// action's is not.
#[tokio::test]
async fn an_untrusted_contract_marks_every_observation_of_its_invocation() {
    for untrusted in [false, true] {
        let tags = if untrusted {
            vec![tilo_actions::UNTRUSTED_CONTENT_TAG.to_owned()]
        } else {
            Vec::new()
        };
        let (state, caller, mut app) = fixture_tagged(tags);
        let request = request();
        assert!(matches!(
            state
                .action_invoke(&caller, None, None, &request)
                .poll_ready(),
            PendingPoll::Ready(Ok(_))
        ));
        let delivered = app.try_recv().unwrap();
        let running = state.action_observe(&caller, &request.invocation).unwrap();
        assert!(!running.is_terminal());
        assert_eq!(running.untrusted, untrusted, "while it runs");
        state
            .apply_invocation_update(
                7,
                delivered.id,
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation: delivered.invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                None,
            )
            .await
            .unwrap();
        let finished = state.action_observe(&caller, &request.invocation).unwrap();
        assert!(finished.is_terminal());
        assert_eq!(finished.untrusted, untrusted, "once it finished");
    }
}

tilo_test_env::scenario! {
    fn an_agents_action_waits_for_and_follows_its_persons_answer;

    phase an_attended_action_waits_for_the_persons_confirmation_before_an_agent_runs_it;
    // An attended action asks the person before it runs for an agent acting
    // for them: without the person's confirmation it is refused before the app
    // hears of it, while the person at the device runs it.
    {
        let (state, person, mut app) = fixture_tagged(vec![tilo_actions::ATTENDED_TAG.to_owned()]);
        let workspace = person.workspace().unwrap().to_owned();
        let agent = Caller::local_person("a1".repeat(32))
            .in_workspace(workspace)
            .in_new_execution()
            .delegated_to("scribe");
        let asked = state
            .action_invoke(&agent, None, None, &request())
            .poll_ready();
        assert!(
            matches!(
                &asked,
                PendingPoll::Ready(Err(
                    tilo_actions::remote::ActionCallError::ConfirmationRequired { .. }
                ))
            ),
            "the agent waits for the person: {asked:?}"
        );
        assert!(app.try_recv().is_err(), "the app never hears of it");
        assert!(matches!(
            state
                .action_invoke(&person, None, None, &request())
                .poll_ready(),
            PendingPoll::Ready(Ok(_))
        ));
        assert!(app.try_recv().is_ok(), "the person at the device runs it");
    }

    phase an_agents_change_the_person_denied_is_refused;
    // The person told the agent not to change the app: its change is refused
    // before the app hears of it, the action not offered to the agent at all.
    {
        let (answered, heard) = agent_answered(tilo_action_policy::Answer::Deny, mcp_server);
        assert!(
            matches!(
                answered,
                PendingPoll::Ready(Err(ActionCallError::UnknownAction))
            ),
            "{answered:?}"
        );
        assert!(!heard, "the app never hears of it");
    }

    phase an_agents_change_the_person_asks_about_waits_for_them;
    // The person told the agent to ask first: its change waits for the
    // person's confirmation.
    {
        let (answered, heard) = agent_answered(tilo_action_policy::Answer::Ask, mcp_server);
        assert!(
            matches!(
                answered,
                PendingPoll::Ready(Err(ActionCallError::ConfirmationRequired { .. }))
            ),
            "{answered:?}"
        );
        assert!(!heard, "the app never hears of it");
    }

    phase an_agents_change_the_person_allowed_runs;
    // The person allowed the agent the app's changes: its change runs.
    {
        let (answered, heard) = agent_answered(tilo_action_policy::Answer::Allow, mcp_server);
        assert!(
            matches!(answered, PendingPoll::Ready(Ok(_))),
            "{answered:?}"
        );
        assert!(heard, "the app runs it");
    }

    phase an_admitted_agent_acting_for_its_person_holds_only_its_own_grants;
    // An agent acting for the person who admitted it may do no more than the
    // workspace grants it: the owner's authority does not widen it, and it
    // carries none of the delegation the person's own device gives the agents it
    // hands a session to. That device-handed session, with the same member
    // grants, does carry it: its change is offered and asks first.
    {
        let owner = "a1".repeat(32);
        let (state, owner_caller, mut app) = fixture_granting(Vec::new(), Vec::new(), |workspace| {
            vec![tilo_action_policy::Grant::new(
                "agent-member:scribe",
                tilo_action_policy::Grantee::agent("scribe", &owner),
                tilo_action_policy::TreePath::workspace(workspace),
                tilo_action_types::KindSet::READ,
                tilo_action_policy::Answer::Allow,
                tilo_action_policy::Grantee::person(&owner),
            )]
        });
        let workspace = owner_caller
            .workspace()
            .expect("the fixture's workspace")
            .to_owned();
        let member = Caller::local_agent("scribe", owner.clone())
            .in_workspace(workspace.clone())
            .in_new_execution();
        assert_eq!(member.acting_for_person(), Some(owner.as_str()));
        assert!(!member.is_session_delegated());
        let answered = state
            .action_invoke(&member, None, None, &request())
            .poll_ready();
        assert!(
            matches!(
                answered,
                PendingPoll::Ready(Err(ActionCallError::UnknownAction))
            ),
            "an observer member is not offered a change: {answered:?}"
        );
        assert!(app.try_recv().is_err(), "the app never hears of it");

        let handed = Caller::local_person(owner)
            .in_workspace(workspace)
            .in_new_execution()
            .delegated_to("scribe");
        assert!(handed.is_session_delegated());
        let answered = state
            .action_invoke(&handed, None, None, &request())
            .poll_ready();
        assert!(
            !matches!(
                answered,
                PendingPoll::Ready(Err(ActionCallError::UnknownAction))
            ),
            "the device-handed session carries the device's delegation: {answered:?}"
        );
    }

    phase an_agent_carries_the_turn_that_caused_its_ops;
    // An agent bound to its roster entry carries the message that caused its turn
    // to every op it makes, so the receiving device decides it against that message
    // and presence can show what the agent would click. An agent no turn carries
    // holds no authority of its own: agents are not members.
    {
        let turn = tilo_action_policy::turn::Turn::new("chat/workspace/channels/1", "m1");
        let person = [0x33; 32];
        let handed = Caller::delegated_agent("scribe", person, [0x34; 32])
            .on_roster(0, person)
            .on_roster_turn(turn.clone())
            .in_workspace("w");
        assert_eq!(handed.turn(), Some(&turn));
        assert_eq!(
            handed.turn_person(),
            Some(&person),
            "the turn answers the person whose message caused it"
        );
        let actor = super::BrokerActor::for_caller(&handed);
        assert_eq!(actor.turn(), Some(&turn), "the broker keeps the cause");

        let alone = super::BrokerActor::for_caller(&Caller::local_agent("scribe", "ada").in_workspace("w"));
        assert!(alone.turn().is_none(), "no turn carries it");
    }
}

/// What an agent asks its person to confirm waits for them stating the invocation field by field,
/// as it would run; their approve runs exactly that.
#[tokio::test]
async fn an_agents_ask_shows_its_person_what_would_run() {
    let (state, person, mut app) = fixture_with(
        vec![tilo_actions::ATTENDED_TAG.to_owned()],
        vec![tilo_actions::ActionParam::required(
            "text",
            tilo_actions::ParamType::Text,
            "Text",
        )],
    );
    let agent = Caller::local_person("a1".repeat(32))
        .in_workspace(person.workspace().unwrap().to_owned())
        .in_new_execution()
        .delegated_to("scribe");
    let mut invoke = request();
    let mut arguments = serde_json::Map::new();
    arguments.insert("text".into(), "Buy milk".into());
    invoke.argument = InvokeArgument::Json(arguments.clone());
    let waits = state
        .action_invoke(&agent, None, None, &invoke)
        .poll_ready();
    let PendingPoll::Ready(Err(ActionCallError::ConfirmationRequired(facts))) = &waits else {
        panic!("it waits for the person: {waits:?}");
    };
    assert_eq!(facts.verb, "Update", "{facts:?}");
    assert_eq!(
        facts
            .facts
            .iter()
            .map(|fact| fact.value.as_str())
            .collect::<Vec<_>>(),
        ["Buy milk"],
        "{facts:?}"
    );
    // What the person may change before approving, as the action's contract draws it.
    assert_eq!(
        facts
            .form
            .iter()
            .map(|field| (field.path.clone(), field.value.line(), field.editable))
            .collect::<Vec<_>>(),
        [(vec!["text".to_owned()], "Buy milk".to_owned(), true)],
        "{facts:?}"
    );
    assert!(
        matches!(&facts.by, tilo_actions::remote::asked::AskedBy::Agent(named) if named.id == "scribe"),
        "{facts:?}"
    );
    assert!(
        app.try_recv().is_err(),
        "nothing runs while the person decides"
    );
    assert!(matches!(
        state
            .action_invoke(&agent, None, None, &asked_again(&invoke))
            .poll_ready(),
        PendingPoll::Ready(Ok(_))
    ));
    let ran = app.try_recv().expect("it runs on the person's approve");
    assert_eq!(
        delivered_arguments(&ran),
        serde_json::Value::Object(arguments),
        "exactly what they were shown"
    );
}

/// The arguments `envelope` hands its application.
fn delivered_arguments(envelope: &InvocationEnvelope) -> serde_json::Value {
    let slot = envelope
        .argument
        .as_deref()
        .expect("it carries its arguments");
    let json = slot
        .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
        .unwrap_or(slot);
    serde_json::from_str(json).unwrap()
}

/// `request` as the dispatch sends it again once its person approved the call's asked record.
fn asked_again(request: &InvokeRequest) -> InvokeRequest {
    use tilo_actions::remote::asked::{Approval, RecordRef};
    InvokeRequest {
        confirmation: Some(tilo_actions::remote::Confirmation::Asked(Approval::new(
            RecordRef {
                workspace: "workspace".into(),
                record: format!("invocations/{}", request.invocation.0),
                device: None,
            },
            None,
        ))),
        ..request.clone()
    }
}

tilo_test_env::scenario! {
    fn execution_activity_reaches_observers_bounded_and_terminal;

    phase refused_actions_and_observer_disconnect_do_not_schedule_work;
    {
        let (state, caller, mut app) = fixture();
        let mut events = state.subscribe_execution_activity();
        let mut refused = request();
        refused.target.provider = "999".into();
        assert!(matches!(
            state
                .action_invoke(&caller, None, None, &refused)
                .poll_ready(),
            PendingPoll::Ready(Err(_))
        ));
        assert!(events.try_recv().is_err());
        assert!(app.try_recv().is_err());
        drop(events);
        assert!(matches!(
            state
                .action_invoke(&caller, None, None, &request())
                .poll_ready(),
            PendingPoll::Ready(Ok(_))
        ));
        assert!(
            app.try_recv().is_ok(),
            "dropping an observer must not end the broker"
        );
    }

    phase actions_without_a_host_minted_run_do_not_invent_execution_presence;
    {
        let (state, caller, mut app) = fixture();
        let ordinary = Caller::local_person("a1".repeat(32)).in_workspace(caller.workspace().unwrap());
        let mut events = state.subscribe_execution_activity();
        assert!(matches!(
            state
                .action_invoke(&ordinary, None, None, &request())
                .poll_ready(),
            PendingPoll::Ready(Ok(_))
        ));
        assert!(app.try_recv().is_ok());
        assert!(events.try_recv().is_err());
    }

    phase disconnect_and_silence_produce_terminal_observations_and_new_brokers_have_new_epochs;
    {
        let (state, caller, mut app) = fixture();
        let mut events = state.subscribe_execution_activity();
        let _ = state
            .action_invoke(&caller, None, None, &request())
            .poll_ready();
        app.try_recv().unwrap();
        let first = events.try_recv().unwrap();
        state.expire_stale_invocations_at(std::time::Instant::now(), Duration::ZERO, Duration::ZERO);
        assert_eq!(events.try_recv().unwrap().status, ExecutionStatus::Failed);
        let _ = state
            .action_invoke(&caller, None, None, &request())
            .poll_ready();
        app.try_recv().unwrap();
        events.try_recv().unwrap();
        state.remove_connection(7);
        assert_eq!(events.try_recv().unwrap().status, ExecutionStatus::Failed);
        assert!(state.pending_invocations.lock().unwrap().is_empty());
        let (other, caller, mut app) = fixture();
        let mut other_events = other.subscribe_execution_activity();
        let _ = other
            .action_invoke(&caller, None, None, &request())
            .poll_ready();
        app.try_recv().unwrap();
        assert_ne!(other_events.try_recv().unwrap().epoch, first.epoch);
    }

    phase closed_application_stream_never_emits_acceptance;
    {
        let (state, caller, app) = fixture();
        let mut events = state.subscribe_execution_activity();
        drop(app);
        assert!(matches!(
            state
                .action_invoke(&caller, None, None, &request())
                .poll_ready(),
            PendingPoll::Ready(Err(_))
        ));
        assert!(events.try_recv().is_err());
        assert!(state.pending_invocations.lock().unwrap().is_empty());
    }

    phase broker_shutdown_closes_existing_and_later_observers_with_state_retained;
    {
        let (state, caller, mut app) = fixture();
        let mut events = state.subscribe_execution_activity();
        let _ = state
            .action_invoke(&caller, None, None, &request())
            .poll_ready();
        app.try_recv().unwrap();
        assert_eq!(events.try_recv().unwrap().status, ExecutionStatus::Accepted);
        state.stop();
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Closed)
        ));
        assert!(matches!(
            state.subscribe_execution_activity().try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Closed)
        ));
        state.stop();
    }
}

#[tokio::test]
async fn wrong_peer_stale_progress_and_post_terminal_updates_emit_nothing() {
    let (state, caller, mut app) = fixture();
    let mut events = state.subscribe_execution_activity();
    let request = request();
    let _ = state
        .action_invoke(&caller, None, None, &request)
        .poll_ready();
    let delivered = app.try_recv().unwrap();
    events.try_recv().unwrap();
    let progress = tilo_actions::InvocationUpdate::Progress {
        invocation: delivered.invocation,
        sequence: 2,
        phase: "Synthetic operation".into(),
        completed: Some(1),
        total: Some(2),
        message: None,
    };
    assert!(
        state
            .apply_invocation_update(8, delivered.id, progress.clone(), None)
            .await
            .is_err()
    );
    assert!(events.try_recv().is_err());
    state
        .apply_invocation_update(7, delivered.id, progress.clone(), None)
        .await
        .unwrap();
    assert_eq!(events.try_recv().unwrap().status, ExecutionStatus::Progress);
    assert!(
        state
            .apply_invocation_update(7, delivered.id, progress.clone(), None)
            .await
            .is_err()
    );
    assert!(events.try_recv().is_err());
    state
        .apply_invocation_update(
            7,
            delivered.id,
            tilo_actions::InvocationUpdate::Cancelled {
                invocation: delivered.invocation,
                message: None,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        events.try_recv().unwrap().status,
        ExecutionStatus::Cancelled
    );
    assert!(
        state
            .apply_invocation_update(7, delivered.id, progress, None)
            .await
            .is_err()
    );
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn eighty_synthetic_runs_keep_exact_record_and_invocation_identity() {
    let (state, caller, mut first_app) = fixture();
    let mut surfaces = state.published_surfaces();
    let mut second = surfaces[0].clone();
    second.peer_id = 8;
    second.route.document_id = "record-b".into();
    surfaces.push(second);
    state.replace_published_surfaces_for_test(surfaces);
    let mut second_app = state.capture_invocations_for_test(8);
    state
        .stable_invocation_subscribers
        .lock()
        .unwrap()
        .insert(8);
    let mut events = state.subscribe_execution_activity();
    let mut pending = Vec::new();
    let mut runs = std::collections::HashSet::new();
    for index in 0..80 {
        let run = caller.clone().in_new_execution();
        assert!(runs.insert(run.execution_id().unwrap()));
        let mut request = request();
        let peer = if index % 2 == 0 { 7 } else { 8 };
        request.target.provider = peer.to_string();
        assert!(matches!(
            state.action_invoke(&run, None, None, &request).poll_ready(),
            PendingPoll::Ready(Ok(_))
        ));
        let delivered = if peer == 7 {
            first_app.try_recv().unwrap()
        } else {
            second_app.try_recv().unwrap()
        };
        let admitted = events.try_recv().unwrap();
        assert_eq!(admitted.execution, run.execution_id().unwrap());
        assert_eq!(admitted.peer_id, peer);
        assert_eq!(
            admitted.route.document_id,
            if peer == 7 { "record-a" } else { "record-b" }
        );
        assert_eq!(admitted.invocation, request.invocation);
        pending.push((peer, delivered, admitted));
    }
    assert_eq!(
        state.pending_invocations.lock().unwrap().len(),
        80,
        "all eighty synthetic runs are pending together"
    );
    for (peer, delivered, admitted) in pending {
        state
            .apply_invocation_update(
                peer,
                delivered.id,
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation: delivered.invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                None,
            )
            .await
            .unwrap();
        let done = events.try_recv().unwrap();
        assert_eq!(done.status, ExecutionStatus::Succeeded);
        assert_eq!(done.execution, admitted.execution);
        assert_eq!(done.invocation, admitted.invocation);
        assert_eq!(done.route, admitted.route);
    }
    assert_eq!(runs.len(), 80);
    assert!(state.pending_invocations.lock().unwrap().is_empty());
    assert!(events.try_recv().is_err());
}

/// The catalogue lists the apps' actions in the order their connections came, whatever order
/// they published in: a caller asking twice, or in another session, is offered the same list.
#[test]
fn the_catalogue_lists_connections_in_the_order_they_came() {
    let (state, caller, _app) = fixture();
    let published = state.live_surfaces(action_provider::RowScope::All);
    let surface = published.first().expect("the fixture's surface").clone();
    let peers: Vec<u64> = (0..24).map(|i| (i * 7) % 24 + 10).collect();
    state.replace_published_surfaces_for_test(
        peers
            .iter()
            .map(|&peer_id| PublishedSurface {
                peer_id,
                ..surface.clone()
            })
            .collect(),
    );
    let listed = || -> Vec<String> {
        state
            .eager_catalogue(&caller, action_provider::RowScope::All)
            .0
            .into_iter()
            .filter(|row| row.action.is_some())
            .map(|row| row.provider)
            .collect()
    };
    let mut ordered = peers;
    ordered.sort_unstable();
    let ordered: Vec<String> = ordered.iter().map(u64::to_string).collect();
    assert_eq!(listed(), ordered);
    assert_eq!(listed(), ordered);
}

#[test]
fn observer_backpressure_is_bounded_and_reports_loss_without_replaying_actions() {
    let (state, caller, mut app) = fixture();
    let mut events = state.subscribe_execution_activity();
    for _ in 0..300 {
        assert!(matches!(
            state
                .action_invoke(&caller, None, None, &request())
                .poll_ready(),
            PendingPoll::Ready(Ok(_))
        ));
        assert!(app.try_recv().is_ok());
    }
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(44))
    ));
    let first = events.try_recv().unwrap();
    assert_eq!(first.sequence, 45);
    let mut count = 1;
    while events.try_recv().is_ok() {
        count += 1;
    }
    assert_eq!(count, 256);
    assert!(
        app.try_recv().is_err(),
        "observation does not execute or replay actions"
    );
}

#[tokio::test]
async fn cancellation_is_a_request_until_the_application_confirms_it() {
    let (state, caller, mut app) = fixture();
    let mut events = state.subscribe_execution_activity();
    let request = request();
    let _ = state
        .action_invoke(&caller, None, None, &request)
        .poll_ready();
    let delivered = app.try_recv().unwrap();
    events.try_recv().unwrap();
    assert!(
        crate::collab_app_broker::answered(state.action_cancel(&caller, &request.invocation))
            .is_ok()
    );
    assert_eq!(
        events.try_recv().unwrap().status,
        ExecutionStatus::CancellationRequested
    );
    assert!(
        crate::collab_app_broker::answered(state.action_cancel(&caller, &request.invocation))
            .is_ok()
    );
    assert!(
        events.try_recv().is_err(),
        "a repeated cancel does not create another activity"
    );
    state
        .apply_invocation_update(
            7,
            delivered.id,
            tilo_actions::InvocationUpdate::Progress {
                invocation: delivered.invocation,
                sequence: 1,
                phase: "Finishing".into(),
                completed: None,
                total: None,
                message: None,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        events.try_recv().unwrap().status,
        ExecutionStatus::CancellationRequested
    );
    state
        .apply_invocation_update(
            7,
            delivered.id,
            tilo_actions::InvocationUpdate::Cancelled {
                invocation: delivered.invocation,
                message: None,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        events.try_recv().unwrap().status,
        ExecutionStatus::Cancelled
    );
}

#[tokio::test]
async fn failed_terminal_journal_barrier_cannot_publish_success() {
    let (state, caller, mut app) = fixture();
    let mut events = state.subscribe_execution_activity();
    let _ = state
        .action_invoke(&caller, None, None, &request())
        .poll_ready();
    let delivered = app.try_recv().unwrap();
    assert_eq!(events.try_recv().unwrap().status, ExecutionStatus::Accepted);
    let root = tempfile::tempdir().unwrap();
    let mut journal = InvocationJournal::open(&root.path().join("journal")).unwrap();
    {
        let mut registry = state.pending_invocations.lock().unwrap();
        let pending = registry.get_mut(&delivered.id).unwrap();
        let key = InvocationKey {
            workspace: pending.cluster_id.clone(),
            host: journal.host(),
            invocation: delivered.invocation,
            owner: pending.owner.clone(),
        };
        journal
            .admit(key.clone(), serde_json::json!({"synthetic":true}), 1)
            .unwrap();
        pending.journal_key = Some(key);
        drop(registry);
    }
    journal.fail_at = Some(tilo_invocation_journal::BarrierFailure::File);
    *state.journal.lock().unwrap() = Ok(Some(journal));
    assert!(
        state
            .apply_invocation_update(
                7,
                delivered.id,
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation: delivered.invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                None
            )
            .await
            .is_err()
    );
    assert!(
        events.try_recv().is_err(),
        "uncommitted success must never escape"
    );
    assert!(matches!(
        state.pending_invocations.lock().unwrap()[&delivered.id].update,
        tilo_actions::InvocationUpdate::Accepted { .. }
    ));
    state.stop();
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Closed)
    ));
}

/// A workspace whose agent `scribe` may write to the test app but is denied
/// its `erase` action, with a second writing person `b2…`, and the app
/// publishing `update`, `edit` and `erase`.
fn continuation_fixture() -> (
    Arc<AppBrokerState>,
    Caller,
    mpsc::UnboundedReceiver<InvocationEnvelope>,
) {
    use crate::test_support::{Grantee, Grants as _, KindSet, Role, TreePath};
    let mut workspace = Workspace::new(WorkspaceId::from_u128(53), "Effect continuation");
    workspace.policy.set_owner("a1".repeat(32));
    let workspace_id = workspace.id().to_string();
    workspace.allow_agent("scribe", KindSet::READ);
    workspace.allow_app(
        Grantee::agent("scribe", "ada"),
        "so.tilo.test",
        KindSet::ALL,
    );
    workspace.policy.add_member("b2".repeat(32), Role::Editor);
    // The agent's person, who gave it its grants.
    workspace.policy.add_member("ada", Role::Editor);
    workspace.deny(
        Grantee::agent("scribe", "ada"),
        &TreePath::root()
            .app("so.tilo.test")
            .any(tilo_action_policy::Level::Kind)
            .resource("test:block-a")
            .action("erase"),
        KindSet::ALL,
    );
    let mut tree = WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    let membership = tree.shared();
    let broker =
        AppBrokerState::test_broker_with_authority(membership.authority(), membership.services());
    let state = Arc::clone(broker.state());
    let action = |action: &str, kind: tilo_actions::OperationKind| PublishedAction {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        resource: "test:block-a".into(),
        action: action.into(),
        resource_kind: "test.block".into(),
        resource_label: "Block".into(),
        action_label: action.into(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        operation_kind: kind,
        params: Vec::new(),
        contract: tilo_actions::ActionContract {
            execution: tilo_actions::ExecutionMode::Background { cancellable: true },
            result: tilo_actions::ResultSpec::Job,
            ..Default::default()
        },
        input: PublishedInput::None,
        view: None,
    };
    state.replace_published_surfaces_for_test(vec![PublishedSurface {
        peer_id: 7,
        app_id: "so.tilo.test".into(),
        route: CrdtRoute {
            app_id: "so.tilo.test".into(),
            cluster_id: workspace_id.clone(),
            document_id: "record-a".into(),
        },
        actions: vec![
            action("update", tilo_actions::OperationKind::Effect),
            action("edit", tilo_actions::OperationKind::Effect),
            action("erase", tilo_actions::OperationKind::Effect),
        ],
        live: true,
    }]);
    let app = state.capture_invocations_for_test(7);
    let agent = Caller::local_agent("scribe", "ada").in_workspace(workspace_id);
    (state, agent, app)
}

/// An edit an effect submits while running for an agent is admitted as
/// that agent, with their grants: an action their role allows but a grant
/// denies them stays denied, through the effect as everywhere.
#[tokio::test]
async fn an_effects_edit_is_admitted_for_its_invoker_with_their_grants() {
    let (state, agent, mut app) = continuation_fixture();
    let invoke = |action: &str| InvokeRequest {
        target: RemoteActionTarget {
            provider: "7".into(),
            address: tilo_actions::ActionAddress::new("test:block-a", action),
        },
        ..request()
    };

    assert!(matches!(
        state
            .action_invoke(&agent, None, None, &invoke("update"))
            .poll_ready(),
        PendingPoll::Ready(Ok(_))
    ));
    let running = app.try_recv().unwrap();
    assert_eq!(
        running.actor,
        tilo_actions::Actor::Member(tilo_actions::MemberId::agent("scribe", "ada"))
    );

    // The app's connection names the effect it runs; nobody else's does.
    let continued = state
        .continuing_caller(7, running.invocation)
        .expect("the running effect's caller");
    assert_eq!(continued, agent);
    assert!(state.continuing_caller(8, running.invocation).is_none());

    // What the role allows is admitted for the agent; what a grant denies
    // them is refused.
    assert!(matches!(
        state
            .action_invoke(&continued, Some(7), None, &invoke("edit"))
            .poll_ready(),
        PendingPoll::Ready(Ok(_))
    ));
    assert_eq!(
        app.try_recv().unwrap().actor,
        tilo_actions::Actor::Member(tilo_actions::MemberId::agent("scribe", "ada"))
    );
    let refused = state
        .action_invoke(&continued, Some(7), None, &invoke("erase"))
        .poll_ready();
    // An action a grant denies is absent to the caller, as in the catalogue.
    assert!(
        matches!(
            refused,
            PendingPoll::Ready(Err(tilo_actions::remote::ActionCallError::UnknownAction))
        ),
        "the denied edit was refused"
    );
    assert!(app.try_recv().is_err(), "the denied edit reaches no app");
    let owner = Caller::local_person("a1".repeat(32))
        .in_workspace(continued.workspace().unwrap().to_owned());
    assert!(matches!(
        state
            .action_invoke(&owner, Some(7), None, &invoke("erase"))
            .poll_ready(),
        PendingPoll::Ready(Ok(_))
    ));
    assert!(app.try_recv().is_ok(), "the action itself is offered");

    // Once the effect ended it continues nothing.
    state
        .apply_invocation_update(
            7,
            running.id,
            tilo_actions::InvocationUpdate::Succeeded {
                invocation: running.invocation,
                output: tilo_actions::ActionOutput::default(),
            },
            None,
        )
        .await
        .unwrap();
    assert!(state.continuing_caller(7, running.invocation).is_none());
}

tilo_test_env::scenario! {
    fn execution_identity_is_host_owned_and_ends_with_retirement;

    phase an_invocation_id_two_callers_share_continues_neither;
    // Apis that take the caller's own invocation id can admit two callers'
    // invocations under one id on the same app. An edit submitted on behalf of
    // that id names no single caller, so it is admitted for neither, never for
    // whichever one the registry happens to find first.
    {
        let (state, agent, _app) = continuation_fixture();
        let workspace = agent.workspace().unwrap().to_owned();
        let other = "b2".repeat(32);
        state
            .stable_invocation_subscribers
            .lock()
            .unwrap()
            .insert(7);
        let shared = tilo_actions::InvocationId::new();
        let start = |person: &str| {
            let caller = Caller::local_person(person)
                .in_workspace(workspace.clone())
                .in_new_execution();
            state
                .start_invocation_confirmed_for_origin(
                    7,
                    "test:block-a",
                    "update",
                    InvocationInputs {
                        original_request: None,
                        requested: Some(shared),
                        caller: Some(&caller),
                        execution: ExecutionScope::for_call(&caller, shared),
                        argument: None,
                        actor: &BrokerActor::person(person),
                        confirmation_token: None,
                        context: None,
                    },
                    None,
                )
                .unwrap()
                .start_blocking(&state)
                .unwrap();
            caller
        };
        let first = start(&"a1".repeat(32));
        assert_eq!(state.continuing_caller(7, shared), Some(first));
        start(&other);
        assert!(
            state.continuing_caller(7, shared).is_none(),
            "an ambiguous invocation continues for nobody"
        );
    }

    phase same_actor_runs_receive_distinct_host_owned_presence_views;
    {
        let (state, first, mut app) = fixture();
        let second = first.clone().in_new_execution();
        let forged = tilo_actions::PresenceId::new("person-window-forged").unwrap();
        let mut presences = Vec::new();
        for caller in [&first, &second, &first] {
            let mut request = request();
            request.context = Some(tilo_actions::ActionContext::default().with_invoker(
                tilo_actions::ActionInvoker::person("a1".repeat(32)).with_presence(forged.clone()),
            ));
            assert!(matches!(
                state
                    .action_invoke(caller, None, None, &request)
                    .poll_ready(),
                PendingPoll::Ready(Ok(_))
            ));
            let delivery = app.try_recv().unwrap();
            let invoker =
                tilo_actions::ActionInvoker::delivered(&delivery.actor, delivery.context.as_ref());
            assert_eq!(
                invoker.acting.by,
                tilo_actions::Actor::member(tilo_actions::MemberId::person("a1".repeat(32)))
            );
            let presence = invoker.presence.expect("a run owns a presence view");
            assert_ne!(presence, forged);
            presences.push(presence);
        }
        assert_ne!(presences[0], presences[1]);
        assert_eq!(presences[0], presences[2]);
        first.retire_execution();
        assert!(matches!(
            state
                .action_invoke(&first, None, None, &request())
                .poll_ready(),
            PendingPoll::Ready(Err(_))
        ));
        assert!(
            app.try_recv().is_err(),
            "closed run cannot fall back to the person's view"
        );
    }

    phase retired_execution_cannot_query_observe_or_cancel_but_legacy_callers_can;
    {
        let (state, caller, mut app) = fixture();
        let scoped_request = request();
        assert!(matches!(
            state
                .action_invoke(&caller, None, None, &scoped_request)
                .poll_ready(),
            PendingPoll::Ready(Ok(_))
        ));
        let delivered = app.try_recv().unwrap();
        assert!(
            state
                .action_observe(&caller, &scoped_request.invocation)
                .is_some()
        );
        assert!(matches!(
            state
                .action_query(&caller, &CatalogueQuery::default())
                .poll_ready(),
            PendingPoll::Ready(Ok(_))
        ));
        let retained = caller.clone();
        caller.retire_execution();
        assert!(matches!(
            state
                .action_query(&retained, &CatalogueQuery::default())
                .poll_ready(),
            PendingPoll::Ready(Err(_))
        ));
        assert!(
            state
                .action_observe(&retained, &scoped_request.invocation)
                .is_none()
        );
        assert!(
            crate::collab_app_broker::answered(
                state.action_cancel(&retained, &scoped_request.invocation)
            )
            .is_err()
        );
        assert!(
            state
                .remote_job(&retained, &scoped_request.invocation)
                .is_none()
        );
        assert!(
            state.pending_invocations.lock().unwrap()[&delivered.id]
                .cancellation_requested_at
                .is_none(),
            "retirement and refused cancellation must not cancel an admitted job"
        );

        let legacy = Caller::local_person("a1".repeat(32)).in_workspace(caller.workspace().unwrap());
        legacy.retire_execution();
        assert!(matches!(
            state
                .action_query(&legacy, &CatalogueQuery::default())
                .poll_ready(),
            PendingPoll::Ready(Ok(_))
        ));
        let legacy_request = request();
        assert!(matches!(
            state
                .action_invoke(&legacy, None, None, &legacy_request)
                .poll_ready(),
            PendingPoll::Ready(Ok(_))
        ));
        app.try_recv().unwrap();
        assert!(
            state
                .action_observe(&legacy, &legacy_request.invocation)
                .is_some()
        );
        assert!(
            crate::collab_app_broker::answered(
                state.action_cancel(&legacy, &legacy_request.invocation)
            )
            .is_ok()
        );
    }
}

/// The broker's api, counting how often a shortcut run observes through it.
struct CountedApi {
    api: action_provider::BrokerApi,
    observes: Arc<std::sync::atomic::AtomicUsize>,
}

impl ActionProvider for CountedApi {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        self.api.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.api.invoke(caller, request)
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.observes.fetch_add(1, Ordering::Relaxed);
        self.api.observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        self.api.next_change(caller, id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionCallError>> {
        self.api.cancel(caller, id)
    }
}

/// Hooks for a run whose host says when a step changes, so nothing may fall
/// back to polling it.
struct NoPolling(crate::automation::ApiHooks);

impl tilo_automation::InvocationHooks for NoPolling {
    fn ask(&self, request: tilo_automation::AskRequest) -> Pending<Option<serde_json::Value>> {
        self.0.ask(request)
    }

    fn settle(&self, invocation: tilo_actions::InvocationId) -> Pending<()> {
        panic!("the broker's API says when {invocation:?} changes; nothing polls it")
    }
}

/// A shortcut of one step that updates the test block.
fn export_shortcut() -> tilo_automation::Shortcut {
    use tilo_action_policy as policy;
    tilo_automation::Shortcut {
        id: "export".into(),
        name: "Export".into(),
        inputs: Vec::new(),
        steps: vec![tilo_automation::Step::new(
            "update",
            tilo_automation::StepKind::Invoke {
                action: tilo_actions::ActionKey::new("so.tilo.test", "test.block", "update"),
                resource: tilo_automation::Selector::Fixed("test:block-a".into()),
                args: std::collections::BTreeMap::new(),
                appearance: None,
            },
        )],
        step_timeout_secs: None,
        output: None,
        grant: vec![policy::Rule::new(
            policy::TreePath::root()
                .any(policy::Level::Workspace)
                .app("so.tilo.test"),
            tilo_action_types::KindSet::ALL,
            policy::Answer::Allow,
        )],
        triggers: Vec::new(),
        saved_by: None,
        saved_proof: None,
        paused: false,
    }
}

/// A shortcut step that runs in the background is observed once per change
/// it makes, not once per poll interval for as long as it runs. The clock is
/// paused: the pauses between changes are the test's, so a poller would be
/// counted on every run, however loaded the host.
#[tokio::test(start_paused = true)]
async fn shortcut_step_observes_once_per_change() {
    const PROGRESS_UPDATES: u64 = 3;
    // The look for an earlier invocation, one wake for the admission itself,
    // then one per progress update and one for the outcome.
    const MOST_OBSERVES: usize = 6;
    let (state, caller, mut app) = fixture();
    let observes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let shortcut = export_shortcut();
    let hooks = NoPolling(crate::automation::ApiHooks::new(
        tokio::runtime::Handle::current(),
        crate::automation::RunningInvocations::default(),
        tilo_automation::AutomationInvocationId::new(),
    ));
    let running = tokio::spawn({
        let state = Arc::clone(&state);
        let observes = Arc::clone(&observes);
        async move {
            let api = CountedApi {
                api: action_provider::BrokerApi::new(state),
                observes,
            };
            tilo_automation::run(
                &api,
                &hooks,
                tilo_automation::InvocationRequest {
                    shortcut: &shortcut,
                    invocation: tilo_automation::AutomationInvocationId::new(),
                    caller,
                    mode: tilo_automation::InvocationMode::Unattended,
                    trigger: tilo_automation::InvocationTrigger::Manual,
                    inputs: serde_json::Map::new(),
                    caused_by: None,
                    parent: None,
                    chain: Vec::new(),
                    approval: None,
                    resume: None,
                    from: None,
                },
            )
            .await
        }
    });

    let delivered = app.recv().await.expect("the step reaches the app");
    // Each pause is many of the old 25 ms poll intervals and well inside the
    // API's safety interval: only a change may wake the run.
    let pause = std::time::Duration::from_millis(150);
    for sequence in 1..=PROGRESS_UPDATES {
        tokio::time::sleep(pause).await;
        state
            .apply_invocation_update(
                7,
                delivered.id,
                tilo_actions::InvocationUpdate::Progress {
                    invocation: delivered.invocation,
                    sequence,
                    phase: "Exporting".into(),
                    completed: Some(sequence),
                    total: Some(PROGRESS_UPDATES),
                    message: None,
                },
                None,
            )
            .await
            .unwrap();
    }
    tokio::time::sleep(pause).await;
    state
        .apply_invocation_update(
            7,
            delivered.id,
            tilo_actions::InvocationUpdate::Succeeded {
                invocation: delivered.invocation,
                output: tilo_actions::ActionOutput::default(),
            },
            None,
        )
        .await
        .unwrap();

    let report = running.await.expect("the run finishes");
    assert_eq!(
        report.outcome,
        tilo_automation::InvocationOutcome::Completed,
        "{report:?}"
    );
    let observes = observes.load(Ordering::Relaxed);
    assert!(
        observes <= MOST_OBSERVES,
        "{observes} observations for {PROGRESS_UPDATES} progress updates"
    );
}

#[tokio::test]
async fn retiring_a_run_clears_only_its_presence_without_cancelling_work() {
    let (state, caller, mut app) = fixture();
    let other = caller.clone().in_new_execution();
    let mut events = state.subscribe_execution_activity();
    let _ = state
        .action_invoke(&caller, None, None, &request())
        .poll_ready();
    let first = app.try_recv().unwrap();
    let buffered = events.try_recv().unwrap();
    assert!(buffered.execution_is_live());
    let _ = state
        .action_invoke(&other, None, None, &request())
        .poll_ready();
    let second = app.try_recv().unwrap();
    events.try_recv().unwrap();
    state.retire_execution(&caller);
    let ended = events.try_recv().unwrap();
    assert_eq!(ended.status, ExecutionStatus::Ended);
    assert_eq!(ended.execution, caller.execution_id().unwrap());
    assert!(!caller.execution_is_live());
    assert!(!buffered.execution_is_live());
    assert!(other.execution_is_live());
    assert_eq!(state.pending_invocations.lock().unwrap().len(), 2);
    for (delivery, visible) in [(first, false), (second, true)] {
        state
            .apply_invocation_update(
                7,
                delivery.id,
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation: delivery.invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(events.try_recv().is_ok(), visible);
    }
    state.retire_execution(&caller);
    assert!(events.try_recv().is_err());
    let _ = state
        .action_invoke(&caller, None, None, &request())
        .poll_ready();
    assert!(
        events.try_recv().is_err(),
        "a retained caller cannot revive presence"
    );
}

/// What the broker answers agent `scribe`'s change through `api` (the
/// caller it is in the workspace) when its person's answer for the app's changes
/// is `answer`, and whether the app heard of it.
fn agent_answered(
    answer: tilo_action_policy::Answer,
    api: impl FnOnce(&str) -> Caller,
) -> (
    PendingPoll<Result<InvocationReceipt, ActionCallError>>,
    bool,
) {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    let owner = "a1".repeat(32);
    let person = tilo_action_policy::Grantee::person(owner.clone());
    let (state, person_caller, mut app) = fixture_granting(Vec::new(), Vec::new(), |workspace| {
        let mut grants = tilo_member_types::Acting::handed_agent("scribe", owner.as_str())
            .handed_preset(workspace);
        grants.push(agent_answers::grant(
            workspace,
            ("scribe", owner.as_str()),
            (
                &AnswerScope::app("so.tilo.test"),
                tilo_action_types::ActionKind::Undoable,
            ),
            answer,
            person.clone(),
        ));
        grants
    });
    let workspace = person_caller
        .workspace()
        .expect("the fixture's workspace")
        .to_owned();
    let answered = state
        .action_invoke(&api(&workspace), None, None, &request())
        .poll_ready();
    (answered, app.try_recv().is_ok())
}

/// The agent a person's answers bind: the one their device admitted, acting for them. An
/// agent that no person stands behind holds nothing.
fn mcp_server(workspace: &str) -> Caller {
    Caller::local_person("a1".repeat(32))
        .in_workspace(workspace.to_owned())
        .in_new_execution()
        .delegated_to("scribe")
}

/// A Store disclosure whose `operation` does not decode is refused with
/// why, not taken for an action nobody offers.
#[test]
fn a_store_disclosure_with_a_malformed_operation_is_refused_with_the_reason() {
    let (state, person, _app) = fixture();
    let workspace = person.workspace().unwrap().to_owned();
    let mut surfaces = state.published_surfaces();
    let surface = surfaces.first_mut().unwrap();
    surface.app_id = "so.tilo.store".into();
    surface.route.app_id = "so.tilo.store".into();
    let action = surface.actions.first_mut().unwrap();
    action.resource = "store:moderation".into();
    action.action = "submit-report".into();
    state.replace_published_surfaces_for_test(surfaces);
    let caller = Caller::authenticated([0xa1; 32], [2; 32])
        .in_workspace(workspace)
        .in_new_execution();
    let mut invoke = request();
    invoke.target.address = tilo_actions::ActionAddress::new("store:moderation", "submit-report");
    invoke.argument = InvokeArgument::Json(
        serde_json::json!({"app": "org.example.app", "operation": "not an operation"})
            .as_object()
            .cloned()
            .unwrap(),
    );
    let refused = state
        .action_invoke(&caller, None, None, &invoke)
        .poll_ready();
    let PendingPoll::Ready(Err(ActionCallError::InvalidRequest { reason })) = &refused else {
        panic!("refused for its operation: {refused:?}");
    };
    assert!(
        reason.contains("operation is malformed"),
        "the reason names the operation: {reason}"
    );
}

/// One app takes another app's action only as a person lets it. With nobody to let it, no
/// workspace, or no such action offered, the call is refused with what to say; with a person and
/// no grant, it waits for them, stating the app that asks. Never is it served unasked.
#[tokio::test]
async fn an_app_taking_another_apps_action_waits_for_its_person_or_is_refused() {
    let (state, person, _app) = fixture();
    let consent = |caller: &Caller, request: &InvokeRequest| {
        state.app_action_consent(caller, "so.tilo.other", request)
    };
    let refusal = |caller: &Caller, request: &InvokeRequest| {
        consent(caller, request)
            .expect_err("never served unasked")
            .to_string()
    };
    let nobody =
        Caller::local_agent("scribe", "a1".repeat(32)).in_workspace(person.workspace().unwrap());
    assert!(refusal(&nobody, &request()).contains("no person"));
    let nowhere = Caller::local_person("a1".repeat(32));
    assert!(refusal(&nowhere, &request()).contains("no workspace"));
    let mut unoffered = request();
    unoffered.target.address = tilo_actions::ActionAddress::new("test:block-a", "erase");
    let unknown = ActionCallError::UnknownAction.to_string();
    assert_eq!(refusal(&person, &unoffered), unknown);
    let waits = consent(&person, &request());
    let Err(ActionCallError::ConfirmationRequired(facts)) = &waits else {
        panic!("no grant lets it, so it waits for the person: {waits:?}");
    };
    assert!(
        matches!(&facts.by, tilo_actions::remote::asked::AskedBy::App { app, .. } if app == "so.tilo.other"),
        "{facts:?}"
    );
}

/// A turn answering Bo through Ada's agent acts as the agent, signed by its own key, and is
/// bounded by both: the agent's own grants and Bo's, the one who asked. Bo may not change the
/// workspace's blocks, so the agent answering him changes none, though Ada's turn of the same
/// agent may; what both may (its notes) the turn for Bo changes.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, end to end, reads as one test"
)]
async fn an_agents_turn_does_only_what_both_it_and_its_asker_may() {
    use tilo_action_policy::agent_answers::AnswerScope;
    use tilo_action_policy::{Answer, Grant, Grantee, Role};
    use tilo_action_types::KindSet;
    use tilo_user_identity::{AgentScope, DeviceCapabilities as Caps};
    let root = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
    let ada = root.verifying_key().to_bytes();
    let owner = hex::encode(ada);
    let bo = [0xb0; 32];
    let workspace = WorkspaceId::from_u128(53);
    let id = workspace.to_string();
    let mut described = Workspace::new(workspace, "Both may");
    described.policy.set_owner(owner.clone());
    described
        .policy
        .members
        .insert(hex::encode(bo), Role::Editor.into());
    described
        .policy
        .grants
        .push(tilo_action_policy::agent_answers::grant(
            &id,
            ("scribe", owner.as_str()),
            (
                &AnswerScope::app("so.tilo.test"),
                tilo_action_types::ActionKind::Undoable,
            ),
            Answer::Allow,
            Grantee::person(owner.clone()),
        ));
    // The agent takes part in the workspace it acts in: its person let it read there, as every
    // agent admitted to a workspace is, before either turn may act.
    described.policy.grants.push(Grant::new(
        "scribe-reads",
        Grantee::agent("scribe", owner.as_str()),
        tilo_action_policy::TreePath::workspace(&id),
        KindSet::READ,
        Answer::Allow,
        Grantee::person(owner.clone()),
    ));
    described.policy.grants.push(Grant::new(
        "bo-no-blocks",
        Grantee::person(hex::encode(bo)),
        AnswerScope::kind("so.tilo.test", "test.block").node(&id),
        KindSet::UNDOABLE,
        Answer::Deny,
        Grantee::person(owner),
    ));
    let mut tree = WorkspaceTree::default();
    tree.desktop.workspaces.push(described);
    let membership = tree.shared();
    let dispatch =
        AppBrokerState::test_broker_with_authority(membership.authority(), membership.services());
    let state = Arc::clone(dispatch.state());
    let action = |resource: &str, kind: &str| PublishedAction {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        resource: resource.into(),
        action: "update".into(),
        resource_kind: kind.into(),
        resource_label: "Item".into(),
        action_label: "Update".into(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        operation_kind: tilo_actions::OperationKind::Effect,
        params: Vec::new(),
        contract: tilo_actions::ActionContract {
            execution: tilo_actions::ExecutionMode::Background { cancellable: true },
            result: tilo_actions::ResultSpec::Job,
            ..Default::default()
        },
        input: PublishedInput::None,
        view: None,
    };
    state.replace_published_surfaces_for_test(vec![PublishedSurface {
        peer_id: 7,
        app_id: "so.tilo.test".into(),
        route: CrdtRoute {
            app_id: "so.tilo.test".into(),
            cluster_id: id.clone(),
            document_id: "record-a".into(),
        },
        actions: vec![
            action("test:block-a", "test.block"),
            action("test:note-a", "test.note"),
        ],
        live: true,
    }]);
    let mut app = state.capture_invocations_for_test(7);
    state
        .stable_invocation_subscribers
        .lock()
        .unwrap()
        .insert(7);

    let now = tilo_clock::unix_now_ns();
    let key = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
    let mut grant = tilo_user_identity::DeviceCert {
        user_public_key: ada,
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Scribe".into(),
        capabilities: Caps::agent_grant(AgentScope::agent_in(
            *uuid::Uuid::parse_str(&id).unwrap().as_bytes(),
            "scribe",
        )),
        issued_at_ns: now - 1_000_000_000,
        expires_at_ns: Some(now + 3_600_000_000_000),
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut grant, &root).unwrap();
    let signed = |resource: &str| {
        let mut asked = request();
        asked.target.address = tilo_actions::ActionAddress::new(resource, "update");
        let signature =
            tilo_server_protocol::invocation::sign_invocation_now(&id, &asked, &grant, &key, now);
        asked.signed = Some(Box::new(signature.unwrap()));
        asked
    };
    let turn_for = |asker: [u8; 32]| {
        Caller::delegated_agent("scribe", ada, grant.device_public_key)
            .in_workspace(id.clone())
            .in_new_execution()
            .on_roster(1, asker)
    };
    let answered =
        |caller: &Caller, asked| state.action_invoke(caller, None, None, &asked).poll_ready();

    let refused = answered(&turn_for(bo), signed("test:block-a"));
    assert!(
        matches!(refused, PendingPoll::Ready(Err(_))),
        "the turn for Bo changes no block, which Bo may not: {refused:?}"
    );
    assert!(app.try_recv().is_err(), "the app never hears of it");
    let allowed = answered(&turn_for(bo), signed("test:note-a"));
    assert!(
        matches!(allowed, PendingPoll::Ready(Ok(_))),
        "the turn for Bo changes a note, which both the agent and Bo may: {allowed:?}"
    );
    assert_eq!(
        app.try_recv().unwrap().resource,
        "test:note-a",
        "the note's change reaches the app"
    );
    let own = answered(&turn_for(ada), signed("test:block-a"));
    assert!(
        matches!(own, PendingPoll::Ready(Ok(_))),
        "Ada's own turn of the agent changes the block: the agent may, so Bo's grants refused it: {own:?}"
    );
}

/// [`fixture`], its action asking its person to type `phrase` before it runs.
fn asking_to_type(
    phrase: &str,
) -> (
    Arc<AppBrokerState>,
    Caller,
    mpsc::UnboundedReceiver<InvocationEnvelope>,
) {
    let (state, person, app) = fixture();
    let mut surface = state.publishers.surfaces.lock().unwrap()[&7].clone();
    surface.actions[0].contract.confirmation = tilo_actions::ConfirmationPolicy::Phrase {
        phrase: phrase.into(),
    };
    state.replace_published_surfaces_for_test(vec![surface]);
    (state, person, app)
}

/// `request` as the dispatch sends it again once its person approved its asked record, having
/// typed `typed`.
fn approved(request: &InvokeRequest, typed: &str) -> InvokeRequest {
    use tilo_actions::remote::asked::{Approval, RecordRef};
    let mut approval = Approval::new(
        RecordRef {
            workspace: "workspace".into(),
            record: format!("invocations/{}", request.invocation.0),
            device: None,
        },
        None,
    );
    approval
        .typed
        .push(zeroize::Zeroizing::new(typed.to_owned()));
    InvokeRequest {
        confirmation: Some(tilo_actions::remote::Confirmation::Asked(approval)),
        ..request.clone()
    }
}

tilo_test_env::scenario! {
    fn a_press_that_asks_waits_for_its_person_and_the_phrase_they_type;

    phase unapproved_it_is_refused_with_what_the_person_is_shown;
    // Nobody approved it yet: it is refused with what its person is shown, the phrase to type
    // among it, and the app never hears of it.
    {
        let (state, person, mut app) = asking_to_type("erase Block");
        let asked = state.action_invoke(&person, None, None, &request()).poll_ready();
        let PendingPoll::Ready(Err(ActionCallError::ConfirmationRequired(facts))) = asked else {
            panic!("the person is asked: {asked:?}");
        };
        assert_eq!(
            facts.by,
            tilo_actions::remote::asked::AskedBy::default(),
            "their own press, answered on their device"
        );
        assert_eq!(facts.verb, "Update");
        assert_eq!(facts.on.as_deref(), Some("Block"));
        assert_eq!(facts.takes.len(), 1, "one thing to type: {facts:?}");
        assert!(facts.takes[0].label.contains("erase Block"), "{facts:?}");
        assert!(!facts.takes[0].secret, "a phrase is no secret");
        assert!(app.try_recv().is_err(), "the app never hears of it");
    }

    phase approved_with_the_wrong_phrase_it_is_refused;
    {
        let (state, person, mut app) = asking_to_type("erase Block");
        let refused = state
            .action_invoke(&person, None, None, &approved(&request(), "erase"))
            .poll_ready();
        assert!(
            matches!(refused, PendingPoll::Ready(Err(ActionCallError::Refused { .. }))),
            "{refused:?}"
        );
        assert!(app.try_recv().is_err(), "the app never hears of it");
    }

    phase approved_with_the_phrase_it_runs;
    {
        let (state, person, mut app) = asking_to_type("erase Block");
        let ran = state
            .action_invoke(&person, None, None, &approved(&request(), "erase Block"))
            .poll_ready();
        assert!(matches!(ran, PendingPoll::Ready(Ok(_))), "{ran:?}");
        assert!(app.try_recv().is_ok(), "the app runs it");
    }
}
