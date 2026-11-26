//! What a person's assistant proposes on a server, with no device of theirs on. A piece of work
//! is filed in the workspace's tracker through the server's action service as a task of the
//! assistant's that nobody gave it, so nothing starts; its person's nod gives it to the assistant
//! and the server owes it a turn; what its person does with the task in the tracker counts as
//! their answer; and a kind of work turned down twice is refused before anything is filed. And
//! the turn itself: the model of a turn the server carries out finds `propose` among the tools
//! that turn has, takes it, and is answered the proposal.

use super::*;
use tilo_actions::remote::workspace_initiative::{
    self, LookEnded, LookRound, Proposal, Proposals, State,
};
use tilo_actions::remote::{ActionCallError, Caller, ReceiptStatus};
use tilo_agent_session::bot::host_steps::{
    HostStepError as StepError, HostSteps as Steps, Instruction, Judgement, StepCause,
};
use tilo_agent_session::workspace_actions::WorkspaceActions;
use tilo_server::workspaces::{
    Workspaces,
    person_model::{
        ProviderProtocol,
        scripted::{Linking, Reply, ScriptedProvider},
    },
};
use tilo_workspace_service::agents::{
    Ask, WorkspaceActionProvider, events::Authority, initiative::Initiative, invoke_named,
    workspace_resource,
};

#[path = "hosted_initiative_page.rs"]
mod page;

#[path = "hosted_initiative_scope.rs"]
mod scope;

/// One `action` of `resource` as `caller` takes it, through what the app action server and an
/// agent's turn both answer with.
async fn take_of(
    initiative: &Initiative,
    workspace: &str,
    caller: &Caller,
    (resource, action): (&str, &str),
    arguments: serde_json::Value,
) -> Result<serde_json::Value, ActionCallError> {
    take_with_id(
        initiative,
        workspace,
        caller,
        (resource, action),
        (InvocationId::new(), arguments),
    )
    .await
}

async fn take_with_id(
    initiative: &Initiative,
    workspace: &str,
    caller: &Caller,
    (resource, action): (&str, &str),
    (invocation, arguments): (InvocationId, serde_json::Value),
) -> Result<serde_json::Value, ActionCallError> {
    let request = InvokeRequest {
        invocation,
        target: workspace_initiative::target(resource, action),
        argument: InvokeArgument::Json(arguments.as_object().cloned().unwrap_or_default()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let answering = initiative.invoking(
        WorkspaceId::parse(workspace).unwrap(),
        caller,
        Authority::Turn,
        request,
    );
    let receipt =
        tokio::task::spawn_blocking(move || answering.wait_timeout(Duration::from_secs(60)))
            .await
            .unwrap()
            .ok()
            .flatten()
            .expect("an action of the server's own is answered");
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
        ReceiptStatus::Rejected { error } => Err(error),
        other => panic!("an action of the server's own ends with its answer: {other:?}"),
    }
}

/// One action on what was proposed, as `caller` takes it.
async fn take(
    initiative: &Initiative,
    workspace: &str,
    caller: &Caller,
    action: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, ActionCallError> {
    let proposals = (workspace_initiative::PROPOSALS, action);
    take_of(initiative, workspace, caller, proposals, arguments).await
}

/// The look-round after its `action`, as its person takes it.
async fn look_round(
    initiative: &Initiative,
    workspace: &str,
    person: &Caller,
    action: &str,
    arguments: serde_json::Value,
) -> LookRound {
    let look_round = (workspace_initiative::LOOK_ROUND, action);
    let answer = take_of(initiative, workspace, person, look_round, arguments).await;
    serde_json::from_value(answer.expect("the look-round answers its person"))
        .expect("a look-round is answered")
}

/// The proposal `answer` describes.
fn proposal(answer: Result<serde_json::Value, ActionCallError>) -> Proposal {
    serde_json::from_value(answer.expect("the action is taken")).expect("a proposal is answered")
}

/// A page, a native device and a turn address the same proposal policy. Revoking
/// read permission on that kind leaves the other Assistant resource accessible.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proposal_review_checks_the_proposal_grant_on_every_request() {
    use tilo_action_policy::{Answer, Grant, Grantee, KindSet, TreePath};
    use tilo_actions::remote::{CatalogueQuery, workspace_events};

    let (owner, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&owner, "host", DeviceCapabilities::member());
    let (member, _) = tilo_user_identity::UserIdentity::generate("Bo", 1);
    let device = enrol(&member, "Bo's browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&owner, &host, state.path()).await;
    let workspaces = hosted.workspaces().unwrap();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    super::resource_support::admit_with(
        workspaces,
        workspaces.governor(),
        &workspace,
        &device,
        tilo_peer_state::join::Role::Editor,
        "Bo",
    )
    .await;
    let mut browser =
        super::resource_support::open_admitted_peer(&hosted, &device, &host, &workspace).await;
    let id = browser.workspace;
    crate::support::admitted_to(&browser.peer, id).await;
    let caller =
        tilo_workspace_service::service::local_caller(&device.cert).in_workspace(&workspace);
    let initiative = hosted.roster.initiative.as_ref().unwrap();
    let deny_read = |label, app, kind| {
        Grant::new(
            label,
            Grantee::person(hex::encode(device.cert.user_public_key)),
            TreePath::workspace(workspace.as_str())
                .app(app)
                .kind(kind)
                .rest()
                .action("list"),
            KindSet::READ,
            Answer::Deny,
            Grantee::person(hex::encode(host.cert.user_public_key)),
        )
    };
    let list = || {
        request(
            workspace_initiative::PROVIDER,
            ActionAddress::new(workspace_initiative::PROPOSALS, workspace_initiative::LIST),
            InvokeArgument::None,
        )
    };
    workspaces
        .put_workspace_grants(
            &workspace,
            workspaces.governor(),
            [0xfb; 32],
            vec![deny_read(
                "no-events-list",
                ui::apps::SHORTCUTS,
                workspace_events::KIND,
            )],
        )
        .await
        .unwrap();
    assert!(
        initiative
            .page_proposal(id, &caller, &device.cert, &list())
            .await
            .is_ok()
    );
    assert!(matches!(
        browser.invoke_settled(list()).await.status,
        ReceiptStatus::Completed { .. }
    ));
    assert!(
        take(
            initiative,
            &workspace,
            &caller,
            workspace_initiative::LIST,
            serde_json::json!({})
        )
        .await
        .is_ok()
    );

    let delegated = caller.clone().delegated_to("not-the-person");
    assert!(matches!(
        initiative
            .page_proposal(id, &delegated, &device.cert, &list())
            .await,
        Err(ActionCallError::NotPermitted)
    ));
    assert!(
        initiative
            .page_proposal(id, &caller, &host.cert, &list())
            .await
            .is_err()
    );

    workspaces
        .put_workspace_grants(
            &workspace,
            workspaces.governor(),
            [0xfc; 32],
            vec![deny_read(
                "no-proposals-list",
                ui::apps::ASSISTANT,
                workspace_initiative::PROPOSALS_KIND,
            )],
        )
        .await
        .unwrap();
    assert_eq!(
        initiative
            .page_proposal(id, &caller, &device.cert, &list())
            .await,
        Err(ActionCallError::NotPermitted)
    );
    assert!(matches!(
        browser.invoke_settled(list()).await.status,
        ReceiptStatus::Rejected {
            error: ActionCallError::NotPermitted
        }
    ));
    assert_eq!(
        take(
            initiative,
            &workspace,
            &caller,
            workspace_initiative::LIST,
            serde_json::json!({})
        )
        .await,
        Err(ActionCallError::NotPermitted)
    );
    let query = CatalogueQuery {
        provider: Some(workspace_initiative::PROVIDER.into()),
        ..Default::default()
    };
    for authority in [Authority::of(&device.cert), Authority::Turn] {
        let catalogue = initiative.catalogue(id, &caller, &authority, &query).await;
        assert!(
            catalogue
                .rows
                .iter()
                .any(|row| row.resource.id == workspace_initiative::LOOK_ROUND)
        );
        assert!(
            !catalogue
                .rows
                .iter()
                .any(|row| row.resource.id == workspace_initiative::PROPOSALS)
        );
    }
}

/// What was proposed, as `caller` reads it.
async fn proposed(initiative: &Initiative, workspace: &str, caller: &Caller) -> Proposals {
    let answer = take(
        initiative,
        workspace,
        caller,
        workspace_initiative::LIST,
        serde_json::json!({}),
    )
    .await;
    serde_json::from_value(answer.expect("a person lists what was proposed"))
        .expect("proposals are answered")
}

/// The assistant proposes `title`, a piece of work of the kind `tidy-notes`.
async fn tidy(
    initiative: &Initiative,
    workspace: &str,
    assistant: &Caller,
    title: &str,
) -> Result<serde_json::Value, ActionCallError> {
    take(
        initiative,
        workspace,
        assistant,
        workspace_initiative::PROPOSE,
        serde_json::json!({
            "kind": "tidy-notes",
            "title": title,
            "why": "Six notes from this week have no title.",
        }),
    )
    .await
}

/// The workspace as a person has it: their agent made to work on the server, the chat with them
/// in it, and the tracker. The document the tracker's first issues are kept in.
async fn furnished(
    page: &Browser,
    workspaces: &Workspaces,
    workspace: &str,
    person: &str,
) -> String {
    make_agent(page, workspaces, workspace, person, None).await;
    let catalogue = crate::common::catalogue();
    let governor = workspaces.governor().clone();
    workspaces
        .prepare_application(workspace, &governor, &catalogue, CHAT, "")
        .await
        .unwrap();
    let tracker_document = workspace_resource(&catalogue, TASKS);
    workspaces
        .prepare_application(workspace, &governor, &catalogue, TASKS, &tracker_document)
        .await
        .unwrap();
    tasks::issue_list::chain().part(0).as_str().to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_an_assistant_proposes_stands_among_its_persons_tasks_and_starts_on_their_nod() {
    use serde_json::json;

    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let server = crate::harness::hosted::start(
        &root,
        &server_device,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let actions = server.roster.actions.clone();
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let id = WorkspaceId::parse(&workspace).unwrap();
    let page = Browser::open_at(&server, &browser, &server_device, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let issues = furnished(&page, &workspaces, &workspace, &person).await;

    // What the assistant finds to do on its own, on this server, with the server's action service.
    let (initiative, _events) = Initiative::start(
        workspaces.clone(),
        Some(actions.clone()),
        std::sync::Arc::new(tilo_server::workspace_bots::installed::EventRunner),
    );

    // The person writes to their assistant, and the server owes it a turn. The assistant acts as
    // that turn's caller: the person's device behind it, the session handed to the agent.
    write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "What could you do for me?",
    )
    .await;
    let agents = server.approving(&workspace, &[SCOUT]).await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    let me = Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser.cert.device_public_key,
    )
    .in_workspace(workspace.clone());
    let theirs = me
        .clone()
        .delegated_to(SCOUT)
        .named("Scout")
        .in_agent_session("agent-session:server-proposing")
        .on_turn(
            tilo_actions::policy::turn::Turn::new(
                turns[0].conversation.clone(),
                turns[0].cause.clone(),
            ),
            turns[0].agent.bound_rules().unwrap_or_default(),
        );

    // It proposes a piece of work: a task of its own that nobody gave it, so nothing starts.
    let send = || tidy(&initiative, &workspace, &theirs, "Tidy the meeting notes");
    let (one, two) = tokio::join!(send(), send());
    let first = proposal(one);
    assert_eq!(
        first,
        proposal(two),
        "both devices receive the same proposal and task"
    );
    assert_eq!(first.state, State::Proposed);
    let task = first.task.clone().expect("it stands as a task");
    let part = tracker(&workspaces, &workspace, &issues).await;
    let filed: Vec<_> = part.issues.iter().collect();
    assert_eq!(filed.len(), 1, "one task is filed");
    let (first_issue, issue) = (filed[0].0.clone(), filed[0].1);
    assert_eq!(issue.title, "Tidy the meeting notes");
    assert_eq!(
        issue
            .origin
            .as_ref()
            .and_then(|origin| origin.reference.as_deref()),
        Some(first.id.as_str())
    );
    assert_eq!(task.number, Some(issue.number));
    assert_eq!(
        issue.assignee.as_ref().map(|assignee| assignee.0.as_str()),
        Some("agent:scout")
    );
    assert_eq!(issue.assigned_by, None, "nobody gave it to the assistant");
    let owed = agents.turns(&workspace).await.unwrap();
    assert!(
        owed.iter()
            .all(|turn| !matches!(turn.ask, Ask::Issue { .. })),
        "a proposal starts nothing: {owed:?}"
    );

    // Their person nods: the task is given to the assistant by them, and the server owes it the
    // work.
    let accepted = proposal(
        take(
            &initiative,
            &workspace,
            &me,
            workspace_initiative::ACCEPT,
            serde_json::json!({"proposal": first.id, "always":true}),
        )
        .await,
    );
    assert_eq!(accepted.state, State::Accepted);
    let part = tracker(&workspaces, &workspace, &issues).await;
    let given = part.issues.get(&first_issue).expect("the task is kept");
    assert!(
        given.assigned_by.as_ref().is_some_and(MemberId::is_person),
        "its person gave it: {:?}",
        given.assigned_by
    );
    let assigned_turns = |queue: &[tilo_workspace_service::agents::Turn], id: &str| {
        queue
            .iter()
            .filter(|turn| {
                turn.person == person
                    && matches!(&turn.ask, Ask::Issue { issue, .. } if issue == id)
            })
            .count()
    };
    let owed = agents.turns(&workspace).await.unwrap();
    assert_eq!(assigned_turns(&owed, first_issue.as_str()), 1);

    // Tasks runs an already authorized kind on the person's behalf using the agent's
    // existing delegation. Replaying its proposal queues one issue turn.
    let invocation = InvocationId::new();
    let submit = || {
        take_with_id(
            &initiative,
            &workspace,
            &theirs,
            (
                workspace_initiative::PROPOSALS,
                workspace_initiative::PROPOSE,
            ),
            (
                invocation,
                serde_json::json!({"kind":"tidy-notes","title":"Tidy the next review","why":"Three notes need a title."}),
            ),
        )
    };
    let (one, two) = tokio::join!(submit(), submit());
    let automatic = proposal(one);
    assert_eq!(automatic, proposal(two));
    assert_eq!(automatic.state, State::Started);
    let address = automatic.task.as_ref().unwrap().issue.as_str();
    let part = tracker(&workspaces, &workspace, &issues).await;
    let (queued_id, given) = part
        .issues
        .iter()
        .find(|(id, _)| tasks_model::issue_resource(id) == address)
        .unwrap();
    assert_eq!(
        given.assigned_by.as_ref().and_then(MemberId::person_id),
        Some(person.as_str())
    );
    let queue = agents.turns(&workspace).await.unwrap();
    assert_eq!(assigned_turns(&queue, queued_id.as_str()), 1);
    take(
        &initiative,
        &workspace,
        &me,
        workspace_initiative::ASK_FIRST,
        serde_json::json!({"kind":"tidy-notes"}),
    )
    .await
    .unwrap();

    // A second one their person cancels in the tracker itself. Read from the tracker, it is
    // turned down. The shared status carries no private preference.
    let second = proposal(tidy(&initiative, &workspace, &theirs, "Tidy the drafts").await);
    let second_task = second.task.clone().expect("it stands as a task");
    let part = tracker(&workspaces, &workspace, &issues).await;
    let (second_issue, _) = part
        .issues
        .iter()
        .find(|(_, issue)| Some(issue.number) == second_task.number)
        .expect("the tracker holds the second task");
    invoke_named(
        &WorkspaceActionProvider::new(actions.clone(), id),
        &me,
        &DocumentRoute {
            app_id: TASKS.into(),
            cluster_id: workspace.clone(),
            document: issues.clone(),
        },
        &second_task.issue,
        "place",
        serde_json::json!({"issue": second_issue.as_str(), "status": "cancelled"})
            .as_object()
            .unwrap()
            .clone(),
    )
    .await
    .expect("the person cancels the task in the tracker");
    let listed = proposed(&initiative, &workspace, &me).await;
    let cancelled = listed
        .proposals
        .iter()
        .find(|proposal| proposal.id == second.id)
        .expect("the second proposal is listed");
    assert_eq!(cancelled.state, State::Declined);
    assert!(
        listed.kinds.is_empty(),
        "shared tracker edits cannot mute a kind"
    );

    // A third their person turns down where it was proposed: its task is cancelled in the
    // tracker.
    let third = proposal(tidy(&initiative, &workspace, &theirs, "Tidy the old plans").await);
    let declined = proposal(
        take(
            &initiative,
            &workspace,
            &me,
            workspace_initiative::DECLINE,
            serde_json::json!({"proposal": third.id, "why": "Leave my notes as they are."}),
        )
        .await,
    );
    assert_eq!(declined.state, State::Declined);
    let part = tracker(&workspaces, &workspace, &issues).await;
    let dropped = part
        .issues
        .iter()
        .find(|(_, issue)| issue.title == "Tidy the old plans")
        .expect("the third task is kept");
    assert!(dropped.1.status.closed(), "{:?}", dropped.1.status);
    // Each independent proposal receives the issue it made, even with the same title and assignee.
    let proposal_file = workspaces
        .workspace_root(&workspace)
        .join("initiative")
        .join(&person)
        .join("initiative.json");
    let reports = |kind| json!({"kind":kind, "title":"Prepare the report", "why":kind});
    let proposing = (
        workspace_initiative::PROPOSALS,
        workspace_initiative::PROPOSE,
    );
    let submit = |kind, invocation| {
        take_with_id(
            &initiative,
            &workspace,
            &theirs,
            proposing,
            (invocation, reports(kind)),
        )
    };
    let invoices_ticket = InvocationId::new();
    let (invoices, quotes) = tokio::join!(
        submit("invoices", invoices_ticket),
        submit("quotes", InvocationId::new())
    );
    let (invoices, quotes) = (proposal(invoices), proposal(quotes));
    let receipts = proposal(submit("receipts", InvocationId::new()).await);
    let archive = proposal(submit("archive", InvocationId::new()).await);
    assert_ne!(invoices.task, quotes.task);
    let part = tracker(&workspaces, &workspace, &issues).await;
    for proposal in [&invoices, &quotes, &receipts, &archive] {
        let task = proposal.task.as_ref().unwrap();
        let (id, issue) = part
            .issues
            .iter()
            .find(|(_, issue)| Some(issue.number) == task.number)
            .unwrap();
        assert_eq!(tasks_model::issue_resource(id), task.issue);
        assert_eq!(issue.body.text(), proposal.why);
        assert_eq!(issue.assigned_by, None);
    }

    // Stop between the Assistant and Tasks commits. Quotes has the person's standing
    // instruction but no assignment yet. The first task was given before its one-time
    // acceptance could finish. Invoices was filed under standing permission but has no link.
    // The person declined Receipts, but its native task has not been cancelled.
    let count = part.issues.len();
    drop((page, initiative, _events, agents, actions, workspaces));
    server.agents.shutdown();
    drop((server.web, server.roster));
    server.server.shutdown().await;
    let mut saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&proposal_file).unwrap()).unwrap();
    let book = &mut saved["book"];
    // Older accepted rows carry no proof that Tasks acknowledged the person's decision.
    book.as_object_mut().unwrap().remove("givenTasks");
    book["applying"] = json!({(&quotes.id): {"approval": 0}, (&first.id): {}, (&receipts.id): {}});
    book["kinds"]["quotes"] = json!({"revision": 0});
    book["kinds"]["invoices"] = json!({"always": true});
    book["kinds"]["receipts"] = json!({"declined": 1});
    for row in book["proposals"].as_array_mut().unwrap() {
        if row["id"] == quotes.id || row["id"] == archive.id || row["id"] == automatic.id {
            row["state"] = json!(State::Accepted);
            row["decidedAtMs"] = row["proposedAtMs"].clone();
        } else if row["id"] == invoices.id {
            row["state"] = json!(State::Started);
            row["decidedAtMs"] = json!(invoices.proposed_at_ms);
            row.as_object_mut().unwrap().remove("task");
        } else if row["id"] == receipts.id {
            row["state"] = json!(State::Declined);
            row["decidedAtMs"] = json!(receipts.proposed_at_ms);
        }
    }
    std::fs::write(proposal_file, serde_json::to_vec(&saved).unwrap()).unwrap();
    let server = crate::harness::hosted::start(
        &root,
        &server_device,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let restarted = server.roster.initiative.as_ref().unwrap();
    let standing = |list: &Proposals, kind: &str| {
        list.kinds
            .iter()
            .any(|pref| pref.kind == kind && pref.always)
    };
    assert!(!standing(
        &proposed(restarted, &workspace, &me).await,
        "quotes"
    ));
    let accept = |id: &str, always: bool| {
        take(
            restarted,
            &workspace,
            &me,
            workspace_initiative::ACCEPT,
            json!({"proposal": id, "always": always}),
        )
    };
    // Old approvals are verified against Tasks. Only the issue already given by the owner qualifies.
    assert_eq!(
        proposal(accept(&automatic.id, false).await).state,
        State::Accepted
    );
    assert!(matches!(
        accept(&archive.id, false).await,
        Err(ActionCallError::Unavailable { .. })
    ));
    let accepted = proposal(accept(&quotes.id, true).await);
    assert_eq!(accepted, proposal(accept(&quotes.id, true).await));
    assert_eq!(accepted.state, State::Accepted);
    assert_eq!(accepted.task, quotes.task);
    // Changing the retry's payload cannot expand the original one-time approval.
    assert!(matches!(
        accept(&first.id, true).await,
        Err(ActionCallError::Refused { .. })
    ));
    assert_eq!(
        proposal(accept(&first.id, false).await).state,
        State::Accepted
    );
    let choices = proposed(restarted, &workspace, &me).await;
    assert!(standing(&choices, "quotes"));
    assert!(!standing(&choices, "tidy-notes"));
    let decline = || {
        take(
            restarted,
            &workspace,
            &me,
            workspace_initiative::DECLINE,
            json!({"proposal": receipts.id}),
        )
    };
    let declined = proposal(decline().await);
    assert_eq!(declined.state, State::Declined);
    assert_eq!(declined, proposal(decline().await));
    let choices = proposed(restarted, &workspace, &me).await;
    assert_eq!(
        choices
            .kinds
            .iter()
            .find(|pref| pref.kind == "receipts")
            .unwrap()
            .declined,
        1
    );
    let recovered = proposal(
        take_with_id(
            restarted,
            &workspace,
            &theirs,
            proposing,
            (invoices_ticket, reports("invoices")),
        )
        .await,
    );
    assert_eq!(recovered.state, State::Started);
    assert_eq!(recovered.task, invoices.task);
    let part = tracker(server.workspaces().unwrap(), &workspace, &issues).await;
    assert_eq!(part.issues.len(), count);
    let filed = |proposal: &Proposal| {
        let task = proposal.task.as_ref().unwrap();
        part.issues
            .iter()
            .find(|(id, _)| tasks_model::issue_resource(id) == task.issue)
            .unwrap()
    };
    assert_eq!(filed(&receipts).1.status, tasks_model::Status::Cancelled);
    assert_eq!(filed(&archive).1.assigned_by, None);
    let queue = server
        .approving(&workspace, &[SCOUT])
        .await
        .turns(&workspace)
        .await
        .unwrap();
    for proposal in [&invoices, &quotes, &first] {
        let (id, issue) = filed(proposal);
        assert_eq!(
            issue.assigned_by.as_ref().and_then(MemberId::person_id),
            Some(person.as_str())
        );
        assert_eq!(assigned_turns(&queue, id.as_str()), 1);
    }
    // Once confirmed, a later retry preserves the receipt even after the person deletes the task.
    let (quote_id, _) = filed(&quotes);
    let mut page = Browser::open_at(&server, &browser, &server_device, &workspace).await;
    let route = DocumentRoute {
        app_id: TASKS.into(),
        cluster_id: workspace.clone(),
        document: issues.clone(),
    };
    let deleted = page
        .invoke_settled(InvokeRequest {
            confirmation: Some(Confirmation::Confirmed),
            ..request(
                &tilo_agent_session::resource_provider(&route),
                ActionAddress::new(&quotes.task.as_ref().unwrap().issue, "delete"),
                InvokeArgument::Json(
                    json!({"issue":quote_id.as_str()})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
        })
        .await;
    assert!(
        matches!(deleted.status, ReceiptStatus::Completed { .. }),
        "{deleted:?}"
    );
    assert_eq!(accepted, proposal(accept(&quotes.id, true).await));
    assert!(
        !tracker(server.workspaces().unwrap(), &workspace, &issues)
            .await
            .issues
            .contains(quote_id)
    );

    // The person still reads their proposals when their own agent loses that permission.
    let agent_deny = tilo_action_policy::Grant::new(
        "no-proposals-for-scout",
        tilo_action_policy::Grantee::agent(SCOUT),
        tilo_action_policy::TreePath::workspace(workspace.as_str())
            .app(ui::apps::ASSISTANT)
            .kind(workspace_initiative::PROPOSALS_KIND)
            .rest()
            .action(workspace_initiative::LIST),
        tilo_action_policy::KindSet::READ,
        tilo_action_policy::Answer::Deny,
        tilo_action_policy::Grantee::person(person),
    );
    let workspaces = server.workspaces().unwrap();
    workspaces
        .put_workspace_grants(
            &workspace,
            workspaces.governor(),
            [0xfd; 32],
            vec![agent_deny],
        )
        .await
        .unwrap();
    assert_eq!(
        take(
            restarted,
            &workspace,
            &theirs,
            workspace_initiative::LIST,
            serde_json::json!({})
        )
        .await,
        Err(ActionCallError::NotPermitted)
    );
    assert!(
        !proposed(restarted, &workspace, &me)
            .await
            .proposals
            .is_empty()
    );
}

/// The whole way, as it goes with no device of the person on: the server answers a turn of the
/// assistant, the turn's model calls `propose` by the path its tools list it under, the server
/// files the task, and the model is answered the proposal.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_model_of_a_hosted_turn_proposes_through_the_tools_its_turn_has() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let server = crate::harness::hosted::start(
        &root,
        &server_device,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let actions = server.roster.actions.clone();
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&server, &browser, &server_device, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let issues = furnished(&page, &workspaces, &workspace, &person).await;
    let (initiative, events) = Initiative::start(
        workspaces.clone(),
        Some(actions.clone()),
        std::sync::Arc::new(tilo_server::workspace_bots::installed::EventRunner),
    );

    // The person asks, and the server answers the turn with what its agents are given: the
    // workspace's resources, the person's events and what their assistant does on its own.
    write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "What could you do for me?",
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = server
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(does_then_answers(
            vec![serde_json::json!({
                "path": "tilo.initiative/proposals#propose",
                "args": {
                    "kind": "tidy-notes",
                    "title": "Tidy the meeting notes",
                    "why": "Six notes from this week have no title.",
                },
            })],
            "I put one piece of work in front of you: tidying the meeting notes.",
            Arc::clone(&asked),
        ))
        .with_events(Some(events))
        .with_initiative(Some(initiative.clone()));
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    tokio::time::timeout(
        Duration::from_secs(60),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "the turn never ended; the model was asked {:?} times",
            asked.lock().unwrap().len()
        )
    })
    .unwrap();

    // The model was answered its tool call with the proposal, waiting.
    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 2, "one tool call, then its answer");
    let (answered, failed) = answered_to(&asked[1], "call-1");
    assert!(
        !failed && answered.starts_with("ok") && answered.contains("proposed"),
        "what the tool answered the model: {answered}"
    );

    // And it is there: one proposal of the person's, there as a task nobody gave.
    let me = Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser.cert.device_public_key,
    )
    .in_workspace(workspace.clone());
    let listed = proposed(&initiative, &workspace, &me).await;
    assert_eq!(listed.proposals.len(), 1, "{:?}", listed.proposals);
    let made = &listed.proposals[0];
    assert_eq!(
        (made.kind.as_str(), made.state),
        ("tidy-notes", State::Proposed)
    );
    assert!(made.task.is_some(), "it stands as a task: {made:?}");
    let part = tracker(&workspaces, &workspace, &issues).await;
    let filed: Vec<_> = part.issues.iter().collect();
    assert_eq!(filed.len(), 1, "one task is filed");
    assert_eq!(filed[0].1.title, "Tidy the meeting notes");
    assert_eq!(filed[0].1.assigned_by, None, "nobody gave it");
}

/// What the server's own services answer reaches the model of a turn. They answer an action with
/// its receipt alone, while the tools a turn has read how an invocation ended by following it,
/// so the server keeps the one for the other: the assistant turns its person's look-round on and
/// then reads their events, both through `tilo_do`, and is told what each answered, not that it
/// is still pending.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_the_servers_own_services_answer_reaches_the_model_of_a_turn() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let server = crate::harness::hosted::start(
        &root,
        &server_device,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let actions = server.roster.actions.clone();
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&server, &browser, &server_device, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    furnished(&page, &workspaces, &workspace, &person).await;
    let (initiative, events) = Initiative::start(
        workspaces.clone(),
        Some(actions.clone()),
        std::sync::Arc::new(tilo_server::workspace_bots::installed::EventRunner),
    );

    write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "Look round for me every hour.",
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = server
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(does_then_answers(
            vec![
                serde_json::json!({
                    "path": "tilo.initiative/look-round#turn-on",
                    "args": {"when": "every 3600 seconds"},
                }),
                serde_json::json!({"path": "tilo.events/events#list"}),
            ],
            "I look round every hour now.",
            Arc::clone(&asked),
        ))
        .with_events(Some(events))
        .with_initiative(Some(initiative));
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    tokio::time::timeout(
        Duration::from_secs(60),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "the turn never ended; the model was asked {:?} times",
            asked.lock().unwrap().len()
        )
    })
    .unwrap();

    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 3, "two tool calls, then what each answered");
    // The look-round's own action: it is on, at the rhythm asked for.
    let (turned_on, failed) = answered_to(&asked[1], "call-1");
    assert!(
        !failed && turned_on.starts_with("ok") && turned_on.contains("every 3600 seconds"),
        "what turning it on answered the model: {turned_on}"
    );
    // The events' own action, taken after: the look-round is one of its person's events now.
    let (listed, failed) = answered_to(&asked[2], "call-2");
    assert!(
        !failed && listed.starts_with("ok") && listed.contains("Look-round"),
        "what the person's events answered the model: {listed}"
    );
}

/// What takes a look's instruction, as the server's events hand one over: one ask of Chat's
/// `ask-agent` in the conversation of the person and their assistant, made as the person's
/// event, under the step's own invocation as the cause's key.
struct HandsOver {
    actions: WorkspaceActions,
    /// The person, at their page's device.
    person: Caller,
    conversation: String,
    /// Every instruction handed over.
    handed: Mutex<Vec<String>>,
}

impl Steps for HandsOver {
    fn instruct(&self, instruction: Instruction) -> Result<String, StepError> {
        let workspace = WorkspaceId::parse(&instruction.workspace).unwrap();
        let event = self
            .person
            .clone()
            .delegated_to(instruction.cause.installation.clone())
            .named("Look-round");
        let route = DocumentRoute {
            app_id: CHAT.into(),
            cluster_id: instruction.workspace.clone(),
            document: String::new(),
        };
        let mut arguments = serde_json::json!({
            "channel": self.conversation,
            "text": instruction.instruction,
            "cause": {
                "kind": "event",
                "label": "Look-round",
                "key": instruction.cause.invocation.0.to_string(),
            },
        });
        if let Some(grant) = instruction.look_grant {
            arguments["cause"]["lookGrant"] = serde_json::Value::String(grant);
        }
        tokio::runtime::Handle::current()
            .block_on(invoke_named(
                &WorkspaceActionProvider::new(self.actions.clone(), workspace),
                &event,
                &route,
                &ui::resource::chain_resource_id(&chat::parts::chain()),
                "ask-agent",
                arguments.as_object().unwrap().clone(),
            ))
            .map_err(StepError::Unavailable)?;
        self.handed.lock().unwrap().push(instruction.instruction);
        Ok(String::new())
    }

    fn judge(&self, _: Judgement) -> Result<serde_json::Value, StepError> {
        Err(StepError::Unavailable("nothing is judged here".into()))
    }
}

/// The look-round `event` of `person` fires in `workspace`: what the events' engine hands the
/// server's steps at its time, on a thread that drives nothing else.
async fn fires(
    steps: &Arc<dyn Steps>,
    person: &str,
    workspace: &str,
    event: &str,
) -> Result<String, StepError> {
    let instruction = Instruction {
        look_grant: None,
        person: person.to_owned(),
        workspace: WorkspaceId::parse(workspace).unwrap().to_string(),
        instruction: "Look round.".into(),
        allow: Vec::new(),
        permits: Vec::new(),
        note: serde_json::json!({}),
        cause: StepCause {
            installation: event.to_owned(),
            invocation: InvocationId::new(),
            event: None,
        },
    };
    let steps = Arc::clone(steps);
    tokio::task::spawn_blocking(move || steps.instruct(instruction))
        .await
        .unwrap()
}

/// The person files `title` in the workspace's tracker, a task of nobody's yet.
async fn files(actions: &WorkspaceActions, workspace: &str, person: &Caller, title: &str) {
    let catalogue = crate::common::catalogue();
    invoke_named(
        &WorkspaceActionProvider::new(actions.clone(), WorkspaceId::parse(workspace).unwrap()),
        person,
        &DocumentRoute {
            app_id: TASKS.into(),
            cluster_id: workspace.to_owned(),
            document: workspace_resource(&catalogue, TASKS),
        },
        &ui::resource::chain_resource_id(&tasks::issue_list::chain()),
        "create",
        serde_json::json!({"title": title})
            .as_object()
            .unwrap()
            .clone(),
    )
    .await
    .expect("the person files a task");
}

/// Previous completed assistant updates give a private conversation enough history to summarise.
async fn previous_updates(hands: &HandsOver, workspace: &str) {
    let route = DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: workspace.to_owned(),
        document: String::new(),
    };
    let provider = WorkspaceActionProvider::new(
        hands.actions.clone(),
        WorkspaceId::parse(workspace).unwrap(),
    );
    let scout = hands.person.clone().delegated_to(SCOUT);
    let channel = Id::from_raw(&hands.conversation);
    let record = ui::resource::chain_resource_id(&chat::parts::chain());
    for index in 0..9 {
        let note = format!(
            "Update {index}: {}",
            "The crossing went as planned and nothing needs doing. ".repeat(22)
        );
        invoke_named(
            &provider,
            &scout,
            &route,
            &record,
            "post",
            saying(&channel, &note).as_object().unwrap().clone(),
        )
        .await
        .unwrap();
    }
    // The caused ask follows the earlier updates on Chat's own message clock.
    tokio::time::sleep(Duration::from_millis(1100)).await;
}

/// The look-round's own day in a turn the server really carries out. A look hands over what it
/// found; the turn that starts is answered by the workspace's own model, and every call of it counts
/// on the look-round's day beside the assistant's. At the limit the turn is asked nothing more,
/// the look-round says what it spent, and the next look, with news again, starts no turn and
/// says what the one before it cost.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_a_look_started_spends_the_look_rounds_own_day_and_stops_at_its_limit() {
    spends_the_look_rounds_day(LookTurn::Immediate).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_queued_look_still_spends_its_own_day_after_history_rolls_over_and_the_host_restarts() {
    spends_the_look_rounds_day(LookTurn::Restarted).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_look_never_recalls_or_summarises_a_long_private_chat() {
    spends_the_look_rounds_day(LookTurn::LongChat).await;
}

#[derive(Eq, PartialEq)]
enum LookTurn {
    Immediate,
    Restarted,
    LongChat,
}

async fn spends_the_look_rounds_day(turn: LookTurn) {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let server = crate::harness::hosted::start(
        &root,
        &server_device,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let actions = server.roster.actions.clone();
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&server, &browser, &server_device, &workspace).await;
    let keeper = root.verifying_key().to_bytes();
    let person = hex::encode(keeper);
    furnished(&page, &workspaces, &workspace, &person).await;

    // One look answers directly. The others try a second call on the same budget.
    let first = if turn == LookTurn::LongChat {
        Reply::text("One task is new: call the bank.")
    } else {
        Reply::tool_call("call-1", "tilo_ls", serde_json::json!({}))
    };
    let agents = server
        .approving_for(&workspace, &[SCOUT], &scope::WORK_APPS)
        .await;
    let theirs = ScriptedProvider::answering([
        first.costing(2_000),
        Reply::text("One task is new: call the bank.").costing(300),
    ]);
    workspaces.answer_models_with(theirs.clone());
    workspaces
        .link_model_for(
            &workspace,
            &Linking {
                name: "Google",
                protocol: ProviderProtocol::OpenAiCompatible,
                endpoint: "https://generativelanguage.googleapis.com/v1beta/openai",
                key: Some("sk-the-keepers-own"),
                models: &["gemini-main"],
            },
            true,
        )
        .await
        .unwrap();

    // What the server's events hand a look's instruction to, and the look in front of it.
    let me = Caller::authenticated(keeper, browser.cert.device_public_key)
        .in_workspace(workspace.clone());
    let hands = Arc::new(HandsOver {
        actions: actions.clone(),
        person: me.clone(),
        conversation: chat::Chat::direct_id([
            MemberId::person(person.clone()),
            MemberId::agent(SCOUT),
        ])
        .as_str()
        .to_owned(),
        handed: Mutex::default(),
    });
    if turn == LookTurn::LongChat {
        previous_updates(&hands, &workspace).await;
    }
    let (initiative, events) = Initiative::start_with(
        workspaces.clone(),
        hands.clone(),
        std::sync::Arc::new(tilo_server::workspace_bots::installed::EventRunner),
    );
    let steps = (initiative.before())(hands.clone());

    // Its person turns the look-round on, with a day of its own.
    let on = look_round(
        &initiative,
        &workspace,
        &me,
        workspace_initiative::TURN_ON,
        serde_json::json!({"when": "every 3600 seconds", "dayLimitMicros": 2_000}),
    )
    .await;
    assert_eq!((on.day_limit_micros, on.spent_today_micros), (2_000, 0));
    let event = on.event.clone().expect("it is an event now");

    // The first look notes where the workspace stands and hands nothing over.
    assert_eq!(
        fires(&steps, &person, &workspace, &event).await,
        Ok(String::new())
    );
    assert!(hands.handed.lock().unwrap().is_empty());

    // Its person files a task: news at the next look, which hands it over.
    files(&actions, &workspace, &me, "Call the bank").await;
    assert_eq!(
        fires(&steps, &person, &workspace, &event).await,
        Ok(String::new())
    );
    assert_eq!(
        hands.handed.lock().unwrap().len(),
        1,
        "what the look found is handed over"
    );

    let (initiative, events, steps) = if turn == LookTurn::Restarted {
        // While the ask waits in Chat, quiet looks evict it from the display. Reopening
        // the host drops its in-memory handoffs. Only the durable Chat ask identifies it.
        for _ in 0..workspace_initiative::MAX_LOOKS {
            fires(&steps, &person, &workspace, &event).await.unwrap();
        }
        let recent = look_round(
            &initiative,
            &workspace,
            &me,
            workspace_initiative::READ,
            serde_json::json!({}),
        )
        .await;
        assert_eq!(recent.looks.len(), workspace_initiative::MAX_LOOKS);
        assert!(
            recent
                .looks
                .iter()
                .all(|look| look.ended == LookEnded::Quiet)
        );
        drop(steps);
        drop(events);
        drop(initiative);
        let (reopened, events) = Initiative::start_with(
            workspaces.clone(),
            hands.clone(),
            std::sync::Arc::new(tilo_server::workspace_bots::installed::EventRunner),
        );
        let steps = (reopened.before())(hands.clone());
        (reopened, events, steps)
    } else {
        // A quiet check lands before the queued answer. Its receipt still belongs to the
        // earlier check, even though no provider spending had happened when this one ran.
        fires(&steps, &person, &workspace, &event).await.unwrap();
        (initiative, events, steps)
    };

    // The server owes the assistant the turn the look started and carries it out with the
    // workspace's own model, whose first call spends the look-round's day: it is asked nothing more.
    let unasked = Arc::new(Mutex::new(Vec::new()));
    let agents = agents
        .scripted(never_asked(Arc::clone(&unasked)))
        .with_events(Some(events))
        .with_initiative(Some(initiative.clone()));
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    assert_eq!(turns[0].person, person, "it works for the look's person");
    assert_eq!(turns[0].program.as_deref(), Some(event.as_str()));
    if turn == LookTurn::LongChat {
        assert!(turns[0].history.is_empty());
        assert!(turns[0].fold.is_none());
        assert!(!turns[0].request.contains("The crossing went as planned"));
    }
    tokio::time::timeout(
        Duration::from_secs(60),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .expect("the turn ends")
    .unwrap();
    assert_eq!(
        theirs.asked().len(),
        1,
        "no call past the look-round's limit"
    );
    assert!(unasked.lock().unwrap().is_empty());
    let spent = look_round(
        &initiative,
        &workspace,
        &me,
        workspace_initiative::READ,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(
        (spent.day_limit_micros, spent.spent_today_micros),
        (2_000, 2_000)
    );

    // News again. The look finds it, starts no turn on a spent day, and the look before it
    // now says what its turn cost.
    files(&actions, &workspace, &me, "Renew the lease").await;
    assert_eq!(
        fires(&steps, &person, &workspace, &event).await,
        Ok(String::new())
    );
    assert_eq!(
        hands.handed.lock().unwrap().len(),
        1,
        "a spent day starts no turn"
    );
    assert_eq!(theirs.asked().len(), 1);
    let after = look_round(
        &initiative,
        &workspace,
        &me,
        workspace_initiative::READ,
        serde_json::json!({}),
    )
    .await;
    let looks: Vec<_> = after
        .looks
        .iter()
        .map(|look| (look.ended, look.cost_micros))
        .collect();
    if turn == LookTurn::Restarted {
        assert_eq!(looks.len(), workspace_initiative::MAX_LOOKS);
        assert_eq!(looks[0], (LookEnded::OverLimit, 0));
        assert!(looks[1..].iter().all(|look| *look == (LookEnded::Quiet, 0)));
        return;
    }
    assert_eq!(
        looks,
        [
            (LookEnded::OverLimit, 0),
            (LookEnded::Quiet, 0),
            (LookEnded::Told, 2_000),
            (LookEnded::Quiet, 0),
        ],
        "its looks, newest first"
    );
}
