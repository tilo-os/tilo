//! The same actions, decided by both routes an action takes: the host
//! running a hosted workspace's records in confined workers (the route MCP,
//! the SDKs and every device of a hosted workspace take), and the desktop's
//! broker admitting a caller's invocation of what an app publishes.
//!
//! One person, an editor of a workspace another person's host governs,
//! acts as themselves, through an agent their own device handed its session
//! to, through two keys they certified (one for all their workspaces, one
//! for this workspace alone), and through an agent device they certified,
//! which the workspace lets make notes. For every action the workspace's Notes,
//! PDF Viewer, Writer and Terminal records publish, each caller is offered
//! on the host exactly what the broker offers them; and a set of actions
//! (a note, the form's fields, a Word export, a paragraph, a command) runs,
//! asks the person first, or is refused alike on both. Putting a file into
//! the workspace is decided as an edit of the workspace on both. A command
//! the host runs for any of them runs in the workspace's shell.
//!
//! Each caller's actions are also the same caller's to both: the broker's
//! actor and the host's replica name the same actor, for the same person.
//! What an agent or a key does is one run the host names, which History
//! keeps and which the host itself shows live, present and narrated, though
//! no client here says anything of itself.
use super::*;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use tilo_action_policy::{Answer, Role};
use tilo_actions::remote::{
    Caller, CallerPrincipal, ConfirmationQuestion, ConfirmationReply, ContentPutAnswer,
    ContentPutStep, Pending,
};
use tilo_agent_participant::workspace_actions::{
    ViewReadPolicy, WorkspaceMember, WorkspaceRecordsLink,
};
use tilo_broker_core::{BrokerActor, CrdtRoute, PublishedAction, PublishedInput, PublishedSurface};
use tilo_session_client::{Cmd, Outbox};

const NOTES: &str = "so.tilo.notes";
const PDF: &str = "so.tilo.pdf-viewer";
const WRITER: &str = "so.tilo.word-processor";
const TERMINAL: &str = "so.tilo.terminal";

/// The workspace's word on its one member: an editor, on their own device
/// and on the keys they certified, who allows what they are asked.
struct Member {
    host: WorkspaceMember,
    person: [u8; 32],
    devices: Vec<[u8; 32]>,
    asked: AtomicUsize,
    content: tilo_content::ContentStore,
    /// The workspace's shell, where the host runs its Terminal's commands.
    shell: Arc<dyn tilo_agent_participant::workspace_actions::WorkspaceShell>,
}

impl Member {
    fn standing(&self, caller: &Caller) -> Option<ui::permissions::Standing> {
        (caller.user_public_key() == Some(&self.person)
            && caller
                .device_public_key()
                .is_some_and(|device| self.devices.contains(device))
            && matches!(caller.principal(), CallerPrincipal::Person(_)))
        .then(|| {
            let person = hex::encode(self.person);
            tilo_action_policy::Standing::person(workspace_policy(&person), person)
        })
    }
}

/// The workspace's policy: the member an editor of it, whose agent device
/// (`assistant`) may make notes.
fn workspace_policy(member: &str) -> tilo_action_policy::WorkspacePolicy {
    use tilo_action_policy::agent_answers::{AnswerScope, grant};
    let mut policy =
        ui::permissions::PermissionContext::member_of(WORKSPACE, member.to_owned(), Role::Editor)
            .standing()
            .policy
            .as_ref()
            .clone();
    for kind in [
        tilo_action_policy::ActionKind::Read,
        tilo_action_policy::ActionKind::Undoable,
    ] {
        policy.grants.push(grant(
            WORKSPACE,
            "assistant",
            (&AnswerScope::app(NOTES), kind),
            Answer::Allow,
            tilo_action_policy::Grantee::person(member),
        ));
    }
    policy
}

impl WorkspaceRecordsLink for Member {
    fn user_public_key(&self) -> [u8; 32] {
        self.host.endpoint.host_cert.user_public_key
    }
    fn member(&self) -> WorkspaceMember {
        self.host.clone()
    }
    fn admits(&self, _workspace: u64, caller: &Caller) -> bool {
        self.standing(caller).is_some()
    }
    fn view_read_policy(
        &self,
        _workspace: u64,
        caller: &Caller,
        _app: &str,
        _record: &str,
    ) -> Option<ViewReadPolicy> {
        Some(ViewReadPolicy {
            workspace: WORKSPACE.into(),
            standing: self.standing(caller)?,
            sandbox: Vec::new(),
            unix_time: tilo_clock::unix_now_ns() / 1_000_000_000,
        })
    }
    fn workspace_policy(&self, workspace: u64, caller: &Caller) -> Option<ViewReadPolicy> {
        self.view_read_policy(workspace, caller, "", "")
    }
    fn workspace_content(&self, _workspace: u64) -> Option<tilo_content::ContentStore> {
        Some(self.content.clone())
    }
    fn shell(&self) -> Option<Arc<dyn tilo_agent_participant::workspace_actions::WorkspaceShell>> {
        Some(self.shell.clone())
    }
    fn ask_person(
        &self,
        _workspace: u64,
        caller: &Caller,
        _question: ConfirmationQuestion,
    ) -> Option<Pending<Result<ConfirmationReply, ActionHostError>>> {
        assert_eq!(
            caller.user_public_key(),
            Some(&self.person),
            "the member is asked"
        );
        self.asked.fetch_add(1, Ordering::SeqCst);
        Some(Pending::ready(Ok(ConfirmationReply::AllowOnce)))
    }
}

/// How one route admitted an invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Admitted {
    Runs,
    AsksFirst,
    Refused,
}

fn served(
    actions: &tilo_agent_participant::WorkspaceActions,
    workspace: u64,
    caller: &Caller,
    message: RemoteActionMessage,
) -> RemoteActionMessage {
    actions
        .serve(workspace, caller, message)
        .unwrap()
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .unwrap()
}

/// What the host lists `caller` on the record `provider` names.
fn listed(
    actions: &tilo_agent_participant::WorkspaceActions,
    workspace: u64,
    caller: &Caller,
    provider: &str,
) -> Vec<tilo_actions::ActionDescriptor> {
    let query = CatalogueQuery {
        provider: Some(provider.to_owned()),
        max_results: 500,
        ..CatalogueQuery::default()
    };
    match served(
        actions,
        workspace,
        caller,
        RemoteActionMessage::CatalogueRequest {
            correlation: 1,
            query,
        },
    ) {
        RemoteActionMessage::CatalogueResponse { result, .. } => result
            .unwrap()
            .rows
            .into_iter()
            .filter_map(|row| row.action)
            .collect(),
        other => panic!("expected a catalogue, got {other:?}"),
    }
}

/// The receipt `caller`'s `request` settles with: a host still settling the
/// workspace is asked again, and an invocation accepted while the person is
/// asked is observed until it ends.
fn settled(
    actions: &tilo_agent_participant::WorkspaceActions,
    workspace: u64,
    caller: &Caller,
    request: &InvokeRequest,
) -> ReceiptStatus {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let mut attempt = request.clone();
        attempt.invocation = InvocationId::new();
        let mut status = match served(
            actions,
            workspace,
            caller,
            RemoteActionMessage::Invoke(attempt.clone()),
        ) {
            RemoteActionMessage::Receipt(receipt) => receipt.status,
            other => panic!("expected a receipt, got {other:?}"),
        };
        while matches!(status, ReceiptStatus::Accepted) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
            status = match served(
                actions,
                workspace,
                caller,
                RemoteActionMessage::observe(attempt.invocation),
            ) {
                RemoteActionMessage::Update {
                    state: Some(state), ..
                } => state.receipt().status,
                other => panic!("expected an observation, got {other:?}"),
            };
        }
        let again = matches!(
            &status,
            ReceiptStatus::Rejected {
                error: ActionHostError::Unavailable { .. }
            }
        ) || matches!(&status, ReceiptStatus::Failed { error } if error.retryable);
        if !again || Instant::now() >= deadline {
            return status;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `descriptor`, as an app publishes it to the broker.
fn published(descriptor: &tilo_actions::ActionDescriptor) -> PublishedAction {
    PublishedAction {
        resource: descriptor.address.resource.clone(),
        semantic_identity: descriptor.semantic_identity.clone(),
        action: descriptor.address.action.clone(),
        resource_kind: descriptor.resource_kind.clone(),
        resource_label: descriptor.resource_label.clone(),
        action_label: descriptor.action_label.clone(),
        keywords: descriptor.keywords.clone(),
        icon: descriptor.icon.clone(),
        description: descriptor.description.clone(),
        state: descriptor.state.clone(),
        operation_kind: descriptor.kind,
        params: descriptor.params.clone(),
        contract: descriptor.contract.clone(),
        view: None,
        input: PublishedInput::None,
    }
}

/// The broker's workspace, governed by the same policy.
fn broker_workspace(member: &str) -> tilo_desktop_model::Workspace {
    let mut workspace = tilo_desktop_model::Workspace::new(
        tilo_desktop_model::WorkspaceSlot(1),
        "Hosted workspace",
    );
    workspace.policy = workspace_policy(member);
    workspace
}

/// Whether the broker offers `caller` the action `descriptor` of a record
/// of `app`: it may take it or would be asked first, and an agent is
/// offered only what reaches agents.
fn broker_offers(
    workspace: &tilo_desktop_model::Workspace,
    caller: &Caller,
    app: &str,
    descriptor: &tilo_actions::ActionDescriptor,
) -> bool {
    let actor = BrokerActor::for_caller(caller);
    (!actor.is_agent() || descriptor.contract.reaches_agents())
        && actor.action_answer_in(
            workspace,
            app,
            (
                &descriptor.resource_kind,
                &descriptor.address.resource,
                &descriptor.address.action,
            ),
            &descriptor.contract.need(),
            now_secs(),
        ) != Answer::Deny
}

/// How the broker admits `caller`'s invocation of `descriptor` on the record
/// at `route`, where an app publishes it.
fn broker_admits(
    workspace: &tilo_desktop_model::Workspace,
    caller: &Caller,
    route: &DocumentRoute,
    descriptor: &tilo_actions::ActionDescriptor,
) -> Admitted {
    let action = published(descriptor);
    let surface = PublishedSurface {
        peer_id: 1,
        app_id: route.app_id.clone(),
        route: CrdtRoute::record(&route.app_id, WORKSPACE, &route.record),
        actions: vec![action.clone()],
        live: true,
    };
    match BrokerActor::for_caller(caller).admit_published_in(
        workspace,
        now_secs(),
        None,
        (&surface, &action),
    ) {
        Ok((false, _)) => Admitted::Runs,
        Ok((true, _)) => Admitted::AsksFirst,
        Err(_) => Admitted::Refused,
    }
}

/// An action's address, as the routes' offers are compared by.
fn address(descriptor: &tilo_actions::ActionDescriptor) -> String {
    format!(
        "{}#{}",
        descriptor.address.resource, descriptor.address.action
    )
}

fn now_secs() -> u64 {
    tilo_clock::unix_now_ns() / 1_000_000_000
}

fn json(value: serde_json::Value) -> InvokeArgument {
    let serde_json::Value::Object(map) = value else {
        panic!("arguments are an object")
    };
    InvokeArgument::Json(map)
}

/// A key the person certified for a service acting for an agent they
/// approved, as the host admits it: the agent the key is, for the person.
/// Each connection is an execution the host minted for it, as the room
/// host's session gives an agent's or a key's requests.
fn key(person: [u8; 32], device: [u8; 32], label: &str) -> Caller {
    Caller::authenticated(person, device)
        .delegated_to(format!("grant-{}", hex::encode(device)))
        .named(label)
        .in_workspace(WORKSPACE)
        .in_new_execution()
}

/// The run a client claims for itself, which no host takes.
fn claimed_run() -> tilo_actions::ActionContext {
    tilo_actions::ActionContext {
        group: Some(tilo_actions::WorkGroup {
            id: "agent-run:claimed".into(),
            label: "Not me".into(),
            kind: tilo_actions::context::AGENT_RUN_GROUP.into(),
            title: None,
        }),
        ..tilo_actions::ActionContext::default()
    }
}

/// The runs the changes of `route` the page received are grouped under.
fn runs_of(page: &mut Browser, route: &DocumentRoute) -> Vec<tilo_actions::WorkGroup> {
    page.pump();
    let mut doc = automerge::AutoCommit::new();
    for (_, bytes) in page.documents.get(route).into_iter().flatten() {
        let _ = tilo_automerge::ApplyIncoming::load_incoming(&mut doc, bytes);
    }
    doc.get_changes(&[])
        .iter()
        .filter_map(|change| change.message())
        .filter_map(tilo_actions::ActionContext::from_change_message)
        .filter_map(|context| context.group)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn both_routes_decide_every_callers_actions_alike() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (guest, _) = tilo_user_identity::UserIdentity::generate("Grace", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let guest_device = enrol(&guest, "guest-browser", DeviceCapabilities::member());
    let all_key = enrol(&guest, "visa-backend", DeviceCapabilities::member());
    let one_key = enrol(&guest, "applicant-7", DeviceCapabilities::member());
    let agent_device = enrol(&guest, "assistant-box", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    // A workspace's shell works in a directory an environment may be
    // granted: not one below /dev, where the scratch state is.
    let shell_state = tempfile::tempdir().unwrap();
    let shells = shell_state.path().join("shells");
    let hosted = start(&root, &host, state.path()).await;
    let _participant = crate::participant::Participant::start(&hosted.relay, &root).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let records: Vec<DocumentRoute> = [NOTES, PDF, WRITER, TERMINAL]
        .into_iter()
        .map(|app| browser.create(app))
        .collect();
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();

    let spoken_by = hex::encode(host.cert.device_public_key);
    let mut differences = Vec::new();
    let person = guest_device.cert.user_public_key;
    let member = Arc::new(Member {
        host: WorkspaceMember {
            endpoint: tilo_agent_participant::HostEndpoint::iroh(
                hosted.relay.node_addr(),
                host.cert.clone(),
            ),
            key: host.key.clone(),
            runtime: tokio::runtime::Handle::current(),
        },
        person,
        devices: vec![
            guest_device.cert.device_public_key,
            all_key.cert.device_public_key,
            one_key.cert.device_public_key,
            agent_device.cert.device_public_key,
        ],
        asked: AtomicUsize::new(0),
        content: tilo_content::ContentStore::in_dir(state.path().join("member-content")),
        shell: tilo_relay::agents::host_shells(shells.clone()).unwrap(),
    });
    let actions = tilo_agent_participant::WorkspaceActions::new(Arc::new(crate::common::catalogue));
    actions.attach(member.clone()).unwrap();
    let workspace = browser.workspace;
    let themselves =
        Caller::authenticated(person, guest_device.cert.device_public_key).in_workspace(WORKSPACE);
    let callers = [
        (
            "their agent",
            themselves
                .clone()
                .delegated_to("Claude Code")
                .in_new_execution(),
        ),
        ("the editor", themselves),
        (
            "their key",
            key(person, all_key.cert.device_public_key, "Visa backend"),
        ),
        (
            "their workspace key",
            key(person, one_key.cert.device_public_key, "Applicant 7"),
        ),
        (
            "their agent device",
            Caller::delegated_agent("assistant", person, agent_device.cert.device_public_key)
                .named("Assistant")
                .in_workspace(WORKSPACE)
                .in_new_execution(),
        ),
    ];
    let owner = Caller::authenticated(
        browser_device.cert.user_public_key,
        browser_device.cert.device_public_key,
    )
    .in_workspace(WORKSPACE);
    let broker = broker_workspace(&hex::encode(person));
    let broker_workspace = &broker;

    // Offered: what each record publishes to anyone, as the host's own
    // person is listed it, offered to each caller alike by both routes.
    let mut every = BTreeMap::new();
    for route in &records {
        let provider = record_provider(route);
        // Everything any of them is listed, beside what the host's own
        // person is: an agent is offered the record's views too.
        let mut all = listed(&actions, workspace, &owner, &provider);
        assert!(!all.is_empty(), "{} publishes actions", route.app_id);
        for (_, caller) in &callers {
            for descriptor in listed(&actions, workspace, caller, &provider) {
                if !all.iter().any(|known| known.address == descriptor.address) {
                    all.push(descriptor);
                }
            }
        }
        // What runs only on a device of the person's, at a window they
        // attend, stays with the desktop: no host runs it for anyone.
        let device_local = |descriptor: &tilo_actions::ActionDescriptor| {
            descriptor.contract.tags.iter().any(|tag| {
                tag == tilo_actions::OWN_DEVICE_SCOPE || tag == tilo_actions::ATTENDED_TAG
            })
        };
        for (who, caller) in &callers {
            let host: BTreeSet<_> = listed(&actions, workspace, caller, &provider)
                .iter()
                .map(address)
                .collect();
            let broker: BTreeSet<_> = all
                .iter()
                .filter(|descriptor| !device_local(descriptor))
                .filter(|descriptor| broker_offers(&broker, caller, &route.app_id, descriptor))
                .map(address)
                .collect();
            for address in host.symmetric_difference(&broker) {
                let descriptor = all
                    .iter()
                    .find(|descriptor| self::address(descriptor) == *address);
                let detail = descriptor.map(|descriptor| {
                    let need = descriptor.contract.need();
                    format!(
                        "{:?} {:?}{}, audience {}, broker answers {:?}",
                        descriptor.kind,
                        need.kind,
                        need.field
                            .as_deref()
                            .map(|field| format!(" at {field}"))
                            .unwrap_or_default(),
                        if descriptor.contract.reaches_agents() {
                            "everyone"
                        } else {
                            "people"
                        },
                        BrokerActor::for_caller(caller).action_answer_in(
                            broker_workspace,
                            &route.app_id,
                            (
                                &descriptor.resource_kind,
                                &descriptor.address.resource,
                                &descriptor.address.action,
                            ),
                            &need,
                            now_secs(),
                        ),
                    )
                });
                differences.push(format!(
                    "{who} on {}: {address} offered by the {} only ({})",
                    route.app_id,
                    if host.contains(address) {
                        "host"
                    } else {
                        "broker"
                    },
                    detail.unwrap_or_default(),
                ));
            }
        }
        every.insert(route.app_id.clone(), (route.clone(), all));
    }

    // Admitted: each of these runs, asks first or is refused alike.
    let invocations = [
        (NOTES, "new-note", serde_json::json!({"title": "Parity"})),
        (PDF, "read-form-fields", serde_json::json!({})),
        // Reaching outside: the editor's grants ask first, and an agent's.
        (PDF, "copy-selection", serde_json::json!({})),
        (
            WRITER,
            "append-paragraph",
            serde_json::json!({"text": "Parity"}),
        ),
        (
            WRITER,
            "export-content",
            serde_json::json!({"format": "docx"}),
        ),
        (
            TERMINAL,
            "run",
            serde_json::json!({"command": "echo ran here"}),
        ),
    ];
    let mut commands_ran = 0;
    // The agents and keys whose note the host made: each made a run.
    let mut agents = Vec::new();
    for (app, action, arguments) in invocations {
        let (route, all) = &every[app];
        let descriptor = all
            .iter()
            .find(|descriptor| descriptor.address.action == action)
            .unwrap_or_else(|| panic!("{app} publishes {action}"));
        for (who, caller) in &callers {
            let by_broker = broker_admits(&broker, caller, route, descriptor);
            let asked = member.asked.load(Ordering::SeqCst);
            let status = settled(
                &actions,
                workspace,
                caller,
                &InvokeRequest {
                    context: Some(claimed_run()),
                    ..request(
                        &record_provider(route),
                        descriptor.address.clone(),
                        json(arguments.clone()),
                    )
                },
            );
            let was_asked = member.asked.load(Ordering::SeqCst) > asked;
            let by_host = match &status {
                ReceiptStatus::Rejected {
                    error:
                        ActionHostError::NotPermitted
                        | ActionHostError::UnknownAction
                        | ActionHostError::ConfirmationRequired { .. },
                } => Admitted::Refused,
                _ if was_asked => Admitted::AsksFirst,
                // It ran: what the action answered is its own.
                ReceiptStatus::Completed { .. } | ReceiptStatus::Failed { .. } => Admitted::Runs,
                other => panic!("{who} {app}#{action}: {other:?}"),
            };
            if by_host != by_broker {
                differences.push(format!(
                    "{who} {app}#{action}: the host {by_host:?} ({status:?}), the broker {by_broker:?}"
                ));
            }
            if app == NOTES
                && by_host == Admitted::Runs
                && let Some(run) = caller.agent_run()
            {
                agents.push(run.label);
            }
            // A command the host runs for anyone runs in the workspace's
            // shell, and answers with how it ran: never left waiting.
            if app == TERMINAL && by_host != Admitted::Refused {
                let ran = match &status {
                    ReceiptStatus::Completed { output } => format!("{output:?}"),
                    other => format!("{other:?}"),
                };
                if ran.contains("ran here") {
                    commands_ran += 1;
                } else {
                    differences.push(format!("{who}'s command did not run: {ran}"));
                }
            }
        }
    }

    // Recorded: each agent's and key's changes are one run the host names,
    // by the name it knows for them, whatever the client claimed; a person's
    // own changes are no run. The page sees what History, Activity and undo
    // read.
    let notes = &every[NOTES].0;
    assert!(
        ["Claude Code", "Visa backend", "Applicant 7", "Assistant"]
            .iter()
            .all(|name| agents.iter().any(|agent| agent == name)),
        "every agent and key made a note: {agents:?}"
    );
    let deadline = tilo_test_env::hang_deadline();
    let runs = loop {
        let runs = runs_of(&mut browser, notes);
        if agents
            .iter()
            .all(|name| runs.iter().any(|run| &run.label == name))
            || Instant::now() >= deadline
        {
            break runs;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    for name in &agents {
        let named: BTreeSet<_> = runs
            .iter()
            .filter(|run| run.label == *name)
            .map(|run| (run.id.clone(), run.kind.clone()))
            .collect();
        if named.len() != 1
            || named.iter().any(|(id, kind)| {
                kind != tilo_actions::context::AGENT_RUN_GROUP || id == "agent-run:claimed"
            })
        {
            differences.push(format!("{name}'s changes on the host: runs {named:?}"));
        }
    }
    if runs.iter().any(|run| run.label == "Not me") {
        differences.push("the host took a run a client claimed".into());
    }

    // Seen live: no client here says anything of itself on the live plane,
    // as an SDK client never does. The host speaks for each run it executes:
    // the run is present as an agent named as the host knows it, said by the
    // host for it, and its actions are narrated as that run's.
    let deadline = tilo_test_env::hang_deadline();
    let (seen, narrated) = loop {
        browser.pump();
        let seen: BTreeMap<String, collab_types::presence::Shown> = browser
            .here()
            .into_iter()
            .filter(|member| member.spoken_by.as_deref() == Some(spoken_by.as_str()))
            .map(|member| (member.label.clone(), member))
            .collect();
        let narrated: BTreeSet<String> = browser
            .narrated
            .iter()
            .map(|(_, work)| work.actor.id.clone())
            .collect();
        let all = agents.iter().all(|name| {
            seen.get(name)
                .is_some_and(|member| narrated.contains(&member.publisher))
        });
        if all || Instant::now() >= deadline {
            break (seen, narrated);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    for name in &agents {
        match seen.get(name) {
            Some(member) if member.agent && narrated.contains(&member.publisher) => {}
            other => differences.push(format!(
                "{name}'s run is not seen live: present as {other:?}, narrated as {narrated:?}"
            )),
        }
    }
    if seen.keys().any(|label| !agents.contains(label)) {
        differences.push(format!(
            "the host spoke for someone who ran nothing: {:?}",
            seen.keys().collect::<Vec<_>>()
        ));
    }

    // A file put into the workspace is an edit of the workspace on both.
    for (who, caller) in &callers {
        let begun = served(
            &actions,
            workspace,
            caller,
            RemoteActionMessage::ContentPutRequest {
                correlation: 1,
                step: ContentPutStep::Begin { size: 4 },
            },
        );
        let host = matches!(
            begun,
            RemoteActionMessage::ContentPutResponse {
                result: Ok(ContentPutAnswer::Begun { .. }),
                ..
            }
        );
        let broker_may = BrokerActor::for_caller(caller).answer_in(
            &broker,
            &broker.node(),
            tilo_action_policy::ActionKind::Undoable,
            now_secs(),
        ) == Answer::Allow;
        if host != broker_may {
            differences.push(format!(
                "{who} putting a file: the host {host}, the broker {broker_may}"
            ));
        }
    }

    // Who acts: the broker's actor and the actor the host names on the
    // changes it makes are one, for the same person.
    for (who, caller) in &callers {
        let actor = BrokerActor::for_caller(caller);
        let named = caller.actor();
        if actor.id() != named.id || actor.acting_for() != named.for_person.as_deref() {
            differences.push(format!(
                "{who}: the broker acts as {} for {:?}, the host names {} for {:?}",
                actor.id(),
                actor.acting_for(),
                named.id,
                named.for_person
            ));
        }
    }

    assert!(commands_ran > 0, "a member's command ran on the host");
    assert!(
        member.asked.load(Ordering::SeqCst) > 0,
        "an action asked the person first"
    );
    assert!(
        differences.is_empty(),
        "the routes decide differently:\n{}",
        differences.join("\n")
    );
}

/// Seen once: an agent through `tilo mcp`'s own path (its session the
/// agent's run, its records opened as windows) is one entry in the people a
/// page shows, the run the host speaks for by the name the host knows. Its
/// computer says nothing of itself, so no second entry is the client's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_through_tilo_mcp_is_seen_once_as_the_hosts_run() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(&root, "Codex on build-box", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let _participant = crate::participant::Participant::start(&hosted.relay, &root).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    browser.create(NOTES);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();

    let codex = Browser::open(&hosted, &computer, &host).await;
    let _ = tilo_session_client::device::run_as_agent(&codex.peer, "Codex").await;
    let collab_types::Principal::Person(person) = collab_types::Principal::person_from_key(
        collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) else {
        panic!("a key names a person")
    };
    let actions = tilo_session_core::remote::SessionActions::new(
        &codex.peer,
        codex.workspace,
        host.cert.device_public_key,
    );
    let windows = tilo_runtime::mcp_windows::AgentWindows::new(
        actions.clone(),
        person.clone(),
        WORKSPACE.into(),
    );
    let server = tilo_runtime::mcp::McpServer::new(actions, WORKSPACE.into(), person)
        .with_surface(tilo_runtime::mcp::Surface::Agent)
        .with_agent_run("run-codex", "Codex");
    let listed = super::hosted_documents::tool(&server, "tilo_ls", serde_json::json!({})).await;
    assert!(listed.contains("notes"), "{listed}");
    let made = super::hosted_documents::tool(
        &server,
        "tilo_do",
        serde_json::json!({"path": "notes#new-note", "args": {"title": "Seen once"}}),
    )
    .await;
    windows.changed(server.take_changed(), None).await;

    let spoken_by = hex::encode(host.cert.device_public_key);
    let computer_publisher = hex::encode(computer.cert.device_public_key);
    let deadline = tilo_test_env::hang_deadline();
    let codexes = loop {
        browser.pump();
        let codexes: Vec<collab_types::presence::Shown> = browser
            .here()
            .into_iter()
            .filter(|member| member.label == "Codex" || member.publisher == computer_publisher)
            .collect();
        let seen = codexes
            .iter()
            .any(|member| member.spoken_by.as_deref() == Some(spoken_by.as_str()));
        if seen || Instant::now() >= deadline {
            break codexes;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    // Its word restated a few times over: a second entry would have arrived.
    tokio::time::sleep(Duration::from_secs(4)).await;
    browser.pump();
    let again: Vec<collab_types::presence::Shown> = browser
        .here()
        .into_iter()
        .filter(|member| member.label == "Codex" || member.publisher == computer_publisher)
        .collect();
    for seen in [&codexes, &again] {
        assert!(
            seen.len() == 1
                && seen[0].agent
                && seen[0].spoken_by.as_deref() == Some(spoken_by.as_str()),
            "an agent through `tilo mcp` ({made}) is not seen once, as the host's run: {seen:?}"
        );
    }
}

/// Seen on another person's host: a workspace a node someone else runs
/// governs (a Lounge's), a member's agent through `tilo mcp`'s path is seen
/// once, named and narrated, as the run that host speaks for, by the member's
/// own page, though the host is no member of the workspace's live plane and
/// never holds its keys. A member device that says a run on the host's track
/// is not heard, and one it says on the plane is its own, never the host's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_is_seen_once_on_a_host_another_person_runs() {
    use tilo_peer_state::join::Role;
    let (operator, _) = tilo_user_identity::UserIdentity::generate("Operator", 1);
    let (guest, _) = tilo_user_identity::UserIdentity::generate("Grace", 1);
    let host = enrol(
        &operator,
        "lounge",
        tilo_user_identity::DeviceCapabilities::host(),
    );
    let page_device = enrol(&guest, "browser", DeviceCapabilities::member());
    let computer = enrol(
        &guest,
        "Claude Code on laptop",
        DeviceCapabilities::member(),
    );
    let state = crate::support::scratch_dir();
    let hosted = start(&operator, &host, state.path()).await;
    let workspaces = hosted.relay.workspaces().unwrap().clone();
    let workspace = workspaces.create("Grace's workspace").await.unwrap();
    let cluster = workspace.workspace.clone();
    for device in [&page_device, &computer] {
        super::resource_support::admit_person(&hosted, &host, &cluster, device, Role::Editor).await;
    }
    let mut browser =
        super::resource_support::open_admitted_peer(&hosted, &page_device, &host, &cluster).await;
    let agent =
        super::resource_support::open_admitted_peer(&hosted, &computer, &host, &cluster).await;
    let _ = tilo_session_client::device::run_as_agent(&agent.peer, "Claude Code").await;
    let collab_types::Principal::Person(person) = collab_types::Principal::person_from_key(
        collab_types::Ed25519PublicKey::from_bytes(guest.verifying_key().to_bytes()),
    ) else {
        panic!("a key names a person")
    };
    let actions = tilo_session_core::remote::SessionActions::new(
        &agent.peer,
        agent.workspace,
        host.cert.device_public_key,
    );
    let server = tilo_runtime::mcp::McpServer::new(actions, cluster.clone(), person)
        .with_surface(tilo_runtime::mcp::Surface::Agent)
        .with_agent_run("run-guest", "Claude Code");
    let listed = super::hosted_documents::tool(&server, "tilo_ls", serde_json::json!({})).await;

    // The host is no member device of the workspace: its governance never
    // makes it a reader of the workspace's scope, whose keys seal the live
    // plane and open every member-readable record.
    let host_key = hex::encode(host.cert.device_public_key);
    let members = loop {
        browser.pump();
        let status = browser.peer.status();
        if let Some(devices) = status.room_devices.get(&cluster)
            && devices.contains_key(&hex::encode(page_device.cert.device_public_key))
        {
            break devices.clone();
        }
        assert!(
            Instant::now() < tilo_test_env::hang_deadline(),
            "the page never read the workspace's members"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        !members.contains_key(&host_key),
        "the operator's host is a member device of the workspace: {members:?}"
    );

    // A member device that says it speaks for a run, as the host would: on
    // the host's own track (signed by itself, not the host) and on the plane.
    let forged = collab_types::presence::Presence::desktop(
        cluster.clone(),
        collab_types::presence::Payload::Shell(Box::new(tilo_session_client::presence::view(
            1,
            tilo_clock::unix_now_ms(),
            "Impostor",
            [1, 2, 3],
            Some("Looking at the apps".into()),
            None,
            None,
        ))),
    )
    .spoken_for("agent-run:forged");
    agent
        .peer
        .send(Cmd::SayForRun(
            agent.workspace,
            tilo_relay_protocol::host_voice::HostWords::Presence(forged.clone()),
        ))
        .unwrap();
    agent
        .peer
        .send(Cmd::Presence(agent.workspace, vec![forged]))
        .unwrap();

    // The agent writes in a document: an action in a record, which the host
    // narrates.
    super::hosted_documents::act(&server, "word-processor#open", serde_json::json!({})).await;
    let (opened, record) = super::hosted_documents::act(
        &server,
        "word-processor#append-paragraph",
        serde_json::json!({"text": "Hello"}),
    )
    .await;
    let spoken_by = host_key;
    let deadline = tilo_test_env::hang_deadline();
    loop {
        browser.pump();
        let here = browser.here();
        assert!(
            here.iter()
                .filter(|member| member.label == "Impostor")
                .all(|member| member.spoken_by.as_deref() != Some(spoken_by.as_str())),
            "a member device speaks in the host's place: {here:?}"
        );
        let agents: Vec<_> = here
            .iter()
            .filter(|member| member.agent && member.label != "Impostor")
            .collect();
        let narrated = agents.first().is_some_and(|agent| {
            browser
                .narrated
                .iter()
                .any(|(_, work)| work.actor.id == agent.publisher)
        });
        if agents.len() == 1
            && agents[0].label == "Claude Code"
            && agents[0].spoken_by.as_deref() == Some(spoken_by.as_str())
            && agents[0].activity.is_some()
            && narrated
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the member's agent is not seen once, named and narrated, on the operator's host \
             (listed {listed}, opened {opened} in {record:?}): {here:?}, narrated {:?}",
            browser.narrated
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // The forged word was never taken, however long the page listens.
    tokio::time::sleep(Duration::from_secs(1)).await;
    browser.pump();
    assert!(
        !browser
            .here()
            .iter()
            .any(|member| member.label == "Impostor"),
        "a member device spoke in the host's place: {:?}",
        browser.here()
    );
}
