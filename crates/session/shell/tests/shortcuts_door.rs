//! Shortcuts saved, published and run through the action door.
//!
//! A fixture application connects to the production collaboration broker
//! and publishes two actions. A shortcut chaining them is saved through the
//! door's `shortcuts#create`, found in the catalogue as
//! `shortcuts.shortcut:<id>#run` and run from there. The fixture sees both
//! steps in order with the first step's output wired into the second, every
//! change it makes carries the run's attribution, the finished run is
//! recorded as one history run, and undoing that run reverts both changes.
//! A shortcut whose steps reach outside its grant is refused at save time.

use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{AutoCommit, ReadDoc};
use collab_types::{PermissionProfileRule, Scope, Verb};
use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::{Map, Value, json};
use shell::shell::state::WorkspaceAuthority as _;
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CatalogueQuery, InvokeArgument, InvokeRequest,
    MAX_CATALOGUE_PAGE, Pending, ReceiptStatus, RemoteActionTarget, workspace_provider,
};
use tilo_actions::{
    ActionAddress, ActionContext, ActionContract, ActionOutput, ActionParam, InvocationId,
    InvocationUpdate, OperationKind, ParamType, ResultSpec,
};
use tilo_automation::{
    Binding, SHORTCUTS_APP_ID, Selector, Shortcut, Step, StepKind, shortcut_resource_id,
};
use ui::collaboration::{
    CertifiedEndpointIdentity, CollabEvent, CollabRequest, CollaborationManager, ExportedAction,
    ExportedInput,
};

const ROOM: uuid::Uuid = uuid::Uuid::from_u128(0x5c07_c075_0000_4000_8000_0000_0000_0001);
const ROOT_KEY: [u8; 32] = [0x5C; 32];
const FIXTURE: &str = "so.tilo.fixture-drafts";
const FIXTURE_HANDLE: &str = "drafts";
const RECORD: &str = "file:drafts.qdoc";
const INBOX: &str = "fixture.drafts:inbox";
const DEADLINE: Duration = Duration::from_secs(20);

fn device_key(handle: &str) -> SigningKey {
    let mut seed = [0_u8; 32];
    for (index, byte) in handle.bytes().enumerate() {
        seed[index % 32] ^= byte;
    }
    seed[31] ^= 0x5C;
    SigningKey::from_bytes(&seed)
}

fn endpoint_identity(handle: &str) -> CertifiedEndpointIdentity {
    let root = SigningKey::from_bytes(&ROOT_KEY);
    let device = device_key(handle);
    let mut certificate = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: handle.to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut certificate, &root).expect("root signs the device");
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id: ROOM.to_string(),
        device_certificate: serde_json::to_vec(&certificate).expect("certificate JSON"),
        proof: device
            .sign(&runtime_identity::collaboration_join_signing_bytes(
                &ROOM.to_string(),
            ))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .expect("certified join encodes");
    CertifiedEndpointIdentity::new(presentation, device)
}

fn owner_principal() -> String {
    let root = SigningKey::from_bytes(&ROOT_KEY);
    match collab_types::Principal::person_from_key(collab_types::Ed25519PublicKey::from_bytes(
        root.verifying_key().to_bytes(),
    )) {
        collab_types::Principal::Person(principal) => principal,
        other => panic!("a person key names a person, not {other:?}"),
    }
}

/// The production broker hosting one room the owner holds, with the
/// fixture application admitted and allowed its own surface.
fn spawn_broker(
    path: &Path,
) -> (
    shell::shell::collab_broker::BrokerHandle,
    shell::shell::state::WorkspaceTreeHandle,
) {
    let owner = owner_principal();
    let mut workspace =
        shell::shell::state::Workspace::new(shell::shell::state::WorkspaceId(1), "Drafts room");
    workspace.room = shell::shell::state::RoomId::parse(&ROOM.to_string()).expect("room id");
    workspace.set_owner(owner.clone());
    workspace
        .grant_as(
            &owner,
            ui::permissions::CapOverride::membership(
                ui::permissions::Subject::agent(FIXTURE_HANDLE),
                workspace.room.to_string(),
                ui::permissions::Verb::Write,
            ),
        )
        .expect("the owner admits the fixture");
    workspace
        .grant_as(
            &owner,
            ui::permissions::CapOverride::grant(
                ui::permissions::Subject::agent(FIXTURE_HANDLE),
                ui::permissions::Scope::App(FIXTURE.to_string()),
                ui::permissions::Verb::Write,
            ),
        )
        .expect("the owner grants the fixture its own surface");
    let mut tree = shell::shell::state::WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    let membership = tree.shared();
    let broker = shell::shell::collab_broker::spawn_native_harness(
        path.to_path_buf(),
        std::sync::Arc::clone(&membership),
        vec![device_key(FIXTURE_HANDLE).verifying_key().to_bytes()],
    )
    .expect("bind the collaboration service");
    (broker, membership)
}

fn exported(
    action: &str,
    label: &str,
    params: &[ActionParam],
    result: ResultSpec,
) -> ExportedAction {
    ExportedAction {
        resource: INBOX.into(),
        action: action.into(),
        resource_kind: "fixture.drafts".into(),
        resource_label: "Inbox".into(),
        action_label: label.into(),
        description: None,
        operation_kind: OperationKind::Document.as_str().to_string(),
        params_json: serde_json::to_string(&params).expect("params JSON"),
        contract_json: serde_json::to_string(&ActionContract {
            result,
            ..ActionContract::default()
        })
        .expect("contract JSON"),
        view: None,
        input: ExportedInput::None,
    }
}

/// An application with two document actions: `draft` writes a draft and
/// returns its id, `publish` publishes a draft by id.
struct FixtureApp {
    manager: CollaborationManager,
    doc: AutoCommit,
    /// Every invocation served, in order: the action and its arguments.
    served: Vec<(String, Map<String, Value>)>,
}

impl FixtureApp {
    fn connect(path: &Path) -> Self {
        let stream = UnixStream::connect(path).expect("connect to the collaboration service");
        let mut manager = CollaborationManager::for_route_fd_with_identity(
            stream.into_raw_fd(),
            FIXTURE,
            ROOM,
            Some(RECORD.to_string()),
            Some(FIXTURE_HANDLE.to_string()),
            endpoint_identity(FIXTURE_HANDLE),
        )
        .expect("start the collaboration client");
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "created", true).expect("seed");
        doc.commit();
        let draft_result = ResultSpec::Json {
            schema: json!({
                "type": "object",
                "properties": {"draft": {"type": "string"}},
                "required": ["draft"]
            }),
        };
        manager.send(CollabRequest::PublishActions {
            app_id: FIXTURE.into(),
            actions: vec![
                exported(
                    "draft",
                    "Write draft",
                    &[ActionParam::required(
                        "title",
                        ParamType::Text,
                        "Title of the draft",
                    )],
                    draft_result,
                ),
                exported(
                    "publish",
                    "Publish draft",
                    &[ActionParam::required(
                        "draft",
                        ParamType::Text,
                        "Id of the draft to publish",
                    )],
                    ResultSpec::None,
                ),
            ],
        });
        manager.send(CollabRequest::document_snapshot(doc.save()));
        Self {
            manager,
            doc,
            served: Vec::new(),
        }
    }

    /// Serve whatever the broker sent since the last call.
    fn pump(&mut self) {
        for event in self.manager.poll() {
            match event {
                CollabEvent::DocumentSnapshotRequested { .. } => {
                    let snapshot = CollabRequest::document_snapshot(self.doc.save());
                    self.manager.send(snapshot);
                }
                CollabEvent::Invocation {
                    id,
                    action,
                    argument,
                    context,
                    ..
                } => {
                    let update = self.serve(id, &action, argument.as_deref(), context.as_deref());
                    self.manager
                        .send(CollabRequest::UpdateInvocation { id, update });
                }
                _ => {}
            }
        }
    }

    fn serve(
        &mut self,
        id: u64,
        action: &str,
        argument: Option<&str>,
        context: Option<&ActionContext>,
    ) -> InvocationUpdate {
        let invocation = InvocationId(uuid::Uuid::from_u128(u128::from(id)));
        let arguments: Map<String, Value> = argument
            .and_then(|slot| slot.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX))
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default();
        self.served.push((action.to_string(), arguments.clone()));
        let text = |name: &str| {
            arguments
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let output = match action {
            "draft" => {
                self.doc
                    .put(automerge::ROOT, "draft", text("title"))
                    .expect("write the draft");
                ActionOutput {
                    value: Some(json!({"draft": "draft:1"})),
                    resources: Vec::new(),
                }
            }
            "publish" => {
                self.doc
                    .put(automerge::ROOT, "published", text("draft"))
                    .expect("publish the draft");
                ActionOutput::default()
            }
            other => panic!("the fixture publishes no `{other}`"),
        };
        let message = context.and_then(ActionContext::change_message);
        let options = match message {
            Some(message) => CommitOptions::default().with_message(message),
            None => CommitOptions::default(),
        };
        self.doc.commit_with(options);
        InvocationUpdate::Succeeded { invocation, output }
    }
}

fn answer<T>(pending: Pending<T>) -> T {
    match pending.wait_timeout(Duration::from_secs(5)) {
        Ok(Some(value)) => value,
        _ => panic!("the broker did not answer"),
    }
}

/// A running broker, the door as the room's owner reaches it, and the
/// fixture application once its actions are offered.
struct Door {
    broker: shell::shell::collab_broker::BrokerHandle,
    membership: shell::shell::state::WorkspaceTreeHandle,
    host: shell::shell::remote_actions::BrokerActionHost,
    owner: Caller,
    app: FixtureApp,
    deadline: Instant,
    _dir: tempfile::TempDir,
}

impl Door {
    fn open() -> Self {
        let dir = tempfile::tempdir().expect("socket directory");
        let socket = dir.path().join("collab.sock");
        let (broker, membership) = spawn_broker(&socket);
        let host = shell::shell::remote_actions::BrokerActionHost::new(
            broker.app_action_broker().expect("the action broker"),
        );
        let mut door = Self {
            broker,
            membership,
            host,
            owner: Caller::local_person(owner_principal()),
            app: FixtureApp::connect(&socket),
            deadline: Instant::now()
                .checked_add(DEADLINE)
                .expect("the deadline fits in an Instant"),
            _dir: dir,
        };
        while !(door.offers(FIXTURE, INBOX, "draft") && door.offers(FIXTURE, INBOX, "publish")) {
            assert!(
                Instant::now() < door.deadline,
                "the fixture's actions were never offered"
            );
            door.app.pump();
            std::thread::sleep(Duration::from_millis(10));
        }
        door
    }

    fn offers(&self, app: &str, resource: &str, action: &str) -> bool {
        answer(self.host.query(
            &self.owner,
            CatalogueQuery {
                max_results: MAX_CATALOGUE_PAGE,
                ..CatalogueQuery::default()
            },
        ))
        .expect("catalogue")
        .rows
        .iter()
        .any(|row| {
            row.app_id == app
                && row.action.as_ref().is_some_and(|descriptor| {
                    descriptor.address.resource == resource && descriptor.address.action == action
                })
        })
    }

    /// Invoke a published action of the room's own provider as the owner.
    fn invoke(
        &self,
        resource: &str,
        action: &str,
        arguments: &Value,
    ) -> Result<(InvocationId, ReceiptStatus), ActionHostError> {
        let invocation = InvocationId::new();
        let arguments = arguments.as_object().cloned().unwrap_or_default();
        let receipt = answer(self.host.invoke(
            &self.owner,
            InvokeRequest {
                invocation,
                target: RemoteActionTarget {
                    provider: workspace_provider(&ROOM.to_string()),
                    address: ActionAddress::new(resource, action),
                },
                argument: if arguments.is_empty() {
                    InvokeArgument::None
                } else {
                    InvokeArgument::Json(arguments)
                },
                context: None,
                confirmation: None,
                revision: None,
            },
        ))?;
        Ok((invocation, receipt.status))
    }

    fn save(&self, shortcut: &Shortcut) -> Result<ReceiptStatus, ActionHostError> {
        let arguments = json!({ "shortcut": serde_json::to_value(shortcut).unwrap() });
        self.invoke("shortcuts", "create", &arguments)
            .map(|(_, status)| status)
    }

    /// Serve the fixture until the invocation `run` ends.
    fn run_to_end(&mut self, run: InvocationId) -> ReceiptStatus {
        loop {
            assert!(Instant::now() < self.deadline, "the run never finished");
            self.app.pump();
            match self
                .host
                .observe(&self.owner, &run)
                .map(|state| state.receipt().status)
            {
                Some(ReceiptStatus::Accepted) | None => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Some(done) => return done,
            }
        }
    }

    fn finished_runs(&self) -> Vec<shell::shell::automation_host::FinishedShortcutRun> {
        let broker = self.broker.app_action_broker().expect("the action broker");
        loop {
            let finished = broker.take_finished_shortcut_runs();
            if !finished.is_empty() {
                return finished;
            }
            assert!(
                Instant::now() < self.deadline,
                "the finished run was never handed over"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn invoke_step(id: &str, action: &str, args: &[(&str, Binding)]) -> Step {
    Step::new(
        id,
        StepKind::Invoke {
            app_id: FIXTURE.into(),
            resource: Selector::Fixed(INBOX.into()),
            action: action.into(),
            args: args
                .iter()
                .map(|(name, binding)| ((*name).to_string(), binding.clone()))
                .collect(),
        },
    )
}

fn publish_draft(id: &str, granted: &[&str]) -> Shortcut {
    Shortcut {
        id: id.into(),
        name: "Publish a draft".into(),
        inputs: vec![ActionParam::required(
            "title",
            ParamType::Text,
            "Title of the draft",
        )],
        steps: vec![
            invoke_step(
                "draft",
                "draft",
                &[("title", Binding::Input("title".into()))],
            ),
            invoke_step(
                "publish",
                "publish",
                &[("draft", Binding::step("draft", "/draft"))],
            ),
        ],
        output: Some(Binding::step("draft", "/draft")),
        grant: granted
            .iter()
            .map(|action| {
                PermissionProfileRule::grant(
                    Scope::Action {
                        app: FIXTURE.into(),
                        action: (*action).to_string(),
                    },
                    Verb::Write,
                )
            })
            .collect(),
        triggers: Vec::new(),
    }
}

/// Every change an action made in `doc` is grouped under run `run` of the
/// shortcut "Publish a draft" in the fixture.
fn assert_attributed_to(doc: &mut AutoCommit, run: InvocationId) {
    let contexts: Vec<ActionContext> = qdoc::history::versions(doc)
        .into_iter()
        .filter_map(|version| ActionContext::from_change_message(version.message.as_deref()?))
        .collect();
    assert_eq!(contexts.len(), 2, "one attributed change per step");
    for context in &contexts {
        let group = context.group.as_ref().expect("grouped under the run");
        assert_eq!(group.id, run.0.to_string());
        assert_eq!(group.kind, "shortcut");
        assert_eq!(context.purpose.as_deref(), Some("Publish a draft"));
        assert_eq!(
            context.app.as_ref().map(|app| app.id.as_str()),
            Some(FIXTURE)
        );
    }
}

#[test]
fn a_saved_shortcut_runs_its_steps_through_the_door_and_undoes_as_one() {
    let mut door = Door::open();

    // Save through the door; the shortcut is then offered as its own action.
    let shortcut = publish_draft("publish-draft", &["draft", "publish"]);
    let resource = shortcut_resource_id("publish-draft");
    match door.save(&shortcut).expect("the shortcut is saved") {
        ReceiptStatus::Completed { output } => assert_eq!(
            output.resources,
            std::slice::from_ref(&resource),
            "saving answers with the shortcut's resource"
        ),
        other => panic!("saving completes at once: {other:?}"),
    }
    assert_eq!(
        door.membership.read().desktop.workspaces[0].shortcuts,
        std::slice::from_ref(&shortcut),
        "the workspace holds the saved shortcut"
    );
    assert!(
        door.offers(SHORTCUTS_APP_ID, &resource, "run"),
        "the saved shortcut is offered as `{resource}#run`"
    );

    // Run it through the door and serve its steps.
    let (run, _) = door
        .invoke(&resource, "run", &json!({ "title": "Launch notes" }))
        .expect("the run is admitted");
    match door.run_to_end(run) {
        ReceiptStatus::Completed { output } => assert_eq!(
            output.value,
            Some(json!("draft:1")),
            "the run answers with the shortcut's output"
        ),
        other => panic!("the run completes: {other:?}"),
    }
    let served: Vec<_> = door
        .app
        .served
        .iter()
        .map(|(action, arguments)| (action.as_str(), Value::Object(arguments.clone())))
        .collect();
    assert_eq!(
        served,
        [
            ("draft", json!({"title": "Launch notes"})),
            ("publish", json!({"draft": "draft:1"})),
        ],
        "both steps ran in order, the draft's id wired into the publish"
    );
    assert_attributed_to(&mut door.app.doc, run);

    // The finished run is recorded as one history run, attributed to the
    // person who ran it.
    let finished = door.finished_runs();
    assert_eq!(finished.len(), 1, "one run finished");
    let mut workspace_doc = AutoCommit::new();
    shell::shell::automation_host::record_shortcut_run(
        &mut workspace_doc,
        &finished[0],
        &mut [(RECORD.to_string(), &mut door.app.doc)],
    )
    .expect("the run is recorded");
    workspace_doc.commit();
    let runs = ui::read_runs(&workspace_doc);
    assert_eq!(runs.len(), 1, "exactly one run recorded");
    let recorded = &runs[0];
    assert_eq!(recorded.label, "Publish a draft");
    assert_eq!(
        recorded.issued_by.as_deref(),
        Some(owner_principal().as_str())
    );
    assert_eq!(recorded.apps(), [FIXTURE]);
    assert_eq!(recorded.steps[0].record, RECORD);
    assert_eq!(recorded.steps[0].summary, "Write draft, Publish draft");

    // Undoing the run reverts both steps.
    let doc = &mut door.app.doc;
    let report = ui::undo_run(
        recorded,
        None,
        &ui::AuthorDirectory::default(),
        &mut [(FIXTURE.to_string(), RECORD.to_string(), &mut *doc)],
    );
    assert!(report.is_complete(), "the whole run is undone: {report:?}");
    doc.commit();
    assert!(doc.get(automerge::ROOT, "draft").unwrap().is_none());
    assert!(doc.get(automerge::ROOT, "published").unwrap().is_none());
    assert!(
        doc.get(automerge::ROOT, "created").unwrap().is_some(),
        "what the run did not do stays"
    );
}

#[test]
fn a_shortcut_reaching_outside_its_grant_is_refused() {
    let door = Door::open();
    match door.save(&publish_draft("sneaky", &["draft"])) {
        Err(ActionHostError::Refused { reason }) => assert!(
            reason.contains("grant does not cover `publish`"),
            "refused for the step outside its grant: {reason}"
        ),
        other => panic!("a step outside the grant must be refused: {other:?}"),
    }
    assert!(
        door.membership.read().desktop.workspaces[0]
            .shortcuts
            .is_empty()
    );
    assert!(!door.offers(SHORTCUTS_APP_ID, &shortcut_resource_id("sneaky"), "run"));
    assert!(door.app.served.is_empty(), "nothing ran");
}
