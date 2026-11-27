#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::wildcard_imports,
    reason = "test scenarios and fixtures: a failed unwrap is the assertion"
)]

//! Who may cancel an invocation by the actor it presents: only whoever started it. Nobody else,
//! in the same workspace too: not another agent of the same person, not an app, not another
//! member, and not a run that only acts as the person, as a shortcut's unattended run or an
//! automation does. A person stops what their agents run from their own certified session,
//! which the served-invocation tests drive.

use super::*;

const APP: &str = "so.tilo.files";
const RESOURCE: &str = "files.entry:/tmp/report";
const ACTION: &str = "refresh";

/// A broker whose workspace `owner` owns and `editor` edits, with an app publishing one cancellable job on peer 7,
/// and the app's end of its invocation stream, which keeps them pending while it is held.
fn broker() -> (
    Arc<AppBrokerState>,
    tokio::sync::mpsc::UnboundedReceiver<InvocationEnvelope>,
) {
    let mut workspace = crate::test_support::Workspace::new(
        crate::test_support::WorkspaceId::from_u128(47),
        "Cancel rule",
    );
    workspace.policy.set_owner("owner");
    workspace
        .policy
        .add_member("editor", tilo_action_policy::Role::Editor);
    // The person's agents may run the app's job, so a refused cancel is never a refused run.
    for agent in ["scout", "helper"] {
        let grant = tilo_action_policy::Grant::new(
            format!("{agent}-runs-jobs"),
            tilo_action_policy::Grantee::agent(agent, "owner"),
            workspace.policy.node().app(APP),
            tilo_action_policy::KindSet::ALL,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person("owner"),
        );
        workspace.policy.grant(grant);
    }
    let route = CrdtRoute::app(APP, workspace.id().to_string());
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let fanout = Arc::new(FanoutState::new());
    let (state, _bridge_rx) = AppBrokerState::new(
        &fanout,
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(tree.shared()),
        None,
    );
    state.publishers.surfaces.lock().unwrap().insert(
        7,
        PublishedSurface {
            peer_id: 7,
            app_id: APP.into(),
            route,
            actions: vec![PublishedAction {
                did: None,
                state: tilo_actions::ActionState::default(),
                semantic_identity: None,
                resource: RESOURCE.into(),
                action: ACTION.into(),
                resource_kind: "files.entry".into(),
                resource_label: "report".into(),
                action_label: "Refresh".into(),
                keywords: Vec::new(),
                icon: None,
                description: None,
                operation_kind: tilo_actions::OperationKind::Effect,
                params: Vec::new(),
                contract: tilo_actions::ActionContract {
                    execution: tilo_actions::ExecutionMode::Background { cancellable: true },
                    result: tilo_actions::ResultSpec::Job,
                    ..tilo_actions::ActionContract::default()
                },
                view: None,
                input: PublishedInput::None,
            }],
            live: true,
        },
    );
    let (tx, invocations) = tokio::sync::mpsc::unbounded_channel();
    state.publishers.streams.lock().unwrap().insert(7, tx);
    (state, invocations)
}

/// The job `actor` starts through the broker's own dispatch.
fn started(state: &AppBrokerState, actor: &BrokerActor) -> u64 {
    state
        .start_invocation_confirmed_for_origin(
            7,
            RESOURCE,
            ACTION,
            InvocationInputs {
                original_request: None,
                requested: None,
                caller: None,
                execution: None,
                argument: None,
                actor,
                confirmation_token: None,
                context: None,
            },
            None,
        )
        .expect("the job is admitted")
        .start_blocking(state)
        .expect("the job starts")
        .id
}

fn member(member: tilo_action_policy::MemberId) -> tilo_action_policy::Actor {
    tilo_action_policy::Actor::from(member)
}

#[test]
fn an_actor_cancels_only_what_it_started() {
    use tilo_action_policy::MemberId;
    let (state, _app) = broker();
    let scout = member(MemberId::agent("scout", "owner"));
    let person = member(MemberId::person("owner"));
    let refused = [
        // Another agent of the same person, which did not start the run.
        (
            "another agent of the person",
            member(MemberId::agent("helper", "owner")),
        ),
        // An agent of the same name acting for someone else.
        (
            "the same agent's name for another person",
            member(MemberId::agent("scout", "editor")),
        ),
        (
            "an app",
            tilo_action_policy::Actor::App("so.tilo.files".into()),
        ),
        ("another member", member(MemberId::person("editor"))),
    ];

    // An agent cancels its own run.
    let own = started(
        &state,
        &BrokerActor::agent("scout", "owner").for_person("owner"),
    );
    for (who, actor) in &refused {
        assert!(!state.can_cancel(own, actor), "{who} is offered no cancel");
        let refusal = state
            .request_cancellation(own, actor)
            .err()
            .unwrap_or_else(|| panic!("{who} is refused"));
        assert_eq!(refusal, NOT_RUNNING, "{who}");
    }
    assert!(
        !state.cancellation_requests.lock().unwrap().contains(&own),
        "no refused cancel reaches the app"
    );
    assert!(state.can_cancel(own, &scout));
    state
        .request_cancellation(own, &scout)
        .expect("the agent cancels its own run")
        .wait_blocking()
        .expect("the request is durable");
    assert!(state.cancellation_requests.lock().unwrap().contains(&own));

    // A run that acts as the person, as a shortcut's unattended run does, or an automation of
    // theirs, does not stop what their agent runs: it is not the person's own session.
    let theirs = started(
        &state,
        &BrokerActor::agent("scout", "owner").for_person("owner"),
    );
    for (who, actor) in [
        ("a run acting as the person", &person),
        (
            "an automation",
            &tilo_action_policy::Actor::automation("nightly"),
        ),
    ] {
        assert!(
            !state.can_cancel(theirs, actor),
            "{who} is offered no cancel"
        );
        assert_eq!(
            state.request_cancellation(theirs, actor).err().as_deref(),
            Some(NOT_RUNNING),
            "{who}"
        );
    }
    assert!(
        !state
            .cancellation_requests
            .lock()
            .unwrap()
            .contains(&theirs)
    );
    // What such a run started is its own to stop.
    let ran = started(&state, &BrokerActor::person("owner"));
    state
        .request_cancellation(ran, &person)
        .expect("a run stops what it started")
        .wait_blocking()
        .expect("the request is durable");

    // An agent does not stop what its person started.
    let persons = started(&state, &BrokerActor::person("owner"));
    assert!(!state.can_cancel(persons, &scout));
    assert_eq!(
        state.request_cancellation(persons, &scout).err().as_deref(),
        Some(NOT_RUNNING)
    );
}
