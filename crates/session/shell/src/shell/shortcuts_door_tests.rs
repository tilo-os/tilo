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
//!
//! Triggers start saved shortcuts the same way: a chord pressed in a
//! compositor session runs the shortcut saved with it, a schedule fires
//! once per fire time across a restart and catches up (or not) after a
//! sleep, a step asking for confirmation runs unattended only when its
//! saver consented in advance, and revoking the saver's membership or
//! deleting the shortcut stops its triggers.
//!
//! Applications reach the door under their own identity: the shortcuts app
//! reads the catalogue it declared and uses its own library, but cannot
//! invoke another app's action itself, and an app that declared nothing is
//! refused the catalogue. An app that may invoke but holds no grant asks
//! the person on the compositor's sheet: allowing once admits that call
//! alone, always allowing records a grant that asks no more (until revoked),
//! denying refuses, and with nobody present nothing is asked and the call is
//! refused. An application opening starts a shortcut with the application's
//! id bound as its input.
//!
//! A run started by a person at the screen asks them instead: its
//! confirmations and questions appear in the shell's sheet, where an answer
//! continues the run and Cancel stops it before anything else is applied.

use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::shell::app_action_consent::{AppActionConsent, SheetPrompter};
use crate::shell::automation_triggers::{Firing, ManualClock, TriggerEvent, TriggerHost, armed};
use crate::shell::portal_dialog::{DialogView, PortalDialogController, PortalDialogQueue};
use automerge::transaction::{CommitOptions, Transactable};
use automerge::{AutoCommit, ReadDoc};
use ed25519_dalek::Signer as _;
use serde_json::{Map, Value, json};
use tilo_action_policy::{Answer, KindSet, Level, Rule, TreePath};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvokeArgument,
    InvokeRequest, MAX_CATALOGUE_PAGE, Pending, ReceiptStatus, RemoteActionMessage,
    RemoteActionTarget, workspace_provider,
};
use tilo_actions::{
    ActionAddress, ActionContext, ActionContract, ActionOutput, ActionParam, InvocationId,
    InvocationUpdate, OperationKind, ParamType, ResultSpec,
};
use tilo_automation::{
    Binding, CatchUp, RunId, SHORTCUTS_APP_ID, Schedule, Selector, Shortcut, Step, StepKind,
    TriggerSpec, action_rule, shortcut_resource_id,
};
use ui::collaboration::{
    CertifiedEndpointIdentity, CollabEvent, CollabRequest, CollaborationManager, ExportedAction,
    ExportedInput,
};

#[path = "shortcuts_door_tests/unhosted.rs"]
mod unhosted;

#[path = "shortcuts_door_tests/hosted_writes.rs"]
mod hosted_writes;

use crate::shell::network::one_host_tests::{Device, Person, Wakes, hosted_workspace, until};

const FIXTURE: &str = "so.tilo.fixture-drafts";
const FIXTURE_HANDLE: &str = "drafts";
const RECORD: &str = "file:drafts.qdoc";
const INBOX: &str = "fixture.drafts:inbox";
const INBOX_KIND: &str = "fixture.drafts";
/// A second resource offering `draft`, so a grant on the inbox's `draft`
/// can be shown not to reach it.
const OUTBOX: &str = "fixture.outbox:outbox";
/// Records of the applications the owner runs on this device, each
/// connecting as the owner.
const SHORTCUTS_HANDLE: &str = "shortcuts-app";
const SILENT_HANDLE: &str = "silent-app";
const INVOKER_HANDLE: &str = "invoker-app";
const SILENT_APP: &str = "so.tilo.fixture-silent";
const INVOKER_APP: &str = "so.tilo.fixture-invoker";
/// The seed of the root of Bob, a member of the room who is not its owner.
const BOB_SEED: u8 = 0xB0;

fn person_of(root: [u8; 32]) -> String {
    match collab_types::Principal::person_from_key(collab_types::Ed25519PublicKey::from_bytes(root))
    {
        collab_types::Principal::Person(principal) => principal,
        other => panic!("a person key names a person, not {other:?}"),
    }
}

/// The room's owner: the person this host was set up as, whose device
/// hosts the room.
fn owner_principal() -> String {
    let paths = crate::firstboot::collaboration_identity_paths();
    person_of(
        tilo_user_identity::persist::read_public_key(&paths.user_pubkey)
            .expect("this host's person"),
    )
}

/// Bob, a member of the room who may edit it.
fn bob() -> String {
    person_of(Person::new(BOB_SEED).user())
}

/// The room's workspace in `tree`.
fn room_workspace(
    tree: &crate::shell::state::WorkspaceTree,
    room: crate::shell::state::RoomId,
) -> &crate::shell::state::Workspace {
    tree.desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .expect("the room's workspace")
}

/// An application's presentation of this device to the broker in `room`:
/// the device the room's host admitted, whose key the network holds.
fn endpoint_identity(room: crate::shell::state::RoomId) -> CertifiedEndpointIdentity {
    let (device, certificate) =
        tilo_identity_host::broker::trusted_current_device(&room.to_string())
            .expect("this device's identity in the room");
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id: room.to_string(),
        device_certificate: serde_json::to_vec(&certificate).expect("certificate JSON"),
        proof: device
            .sign(&runtime_identity::collaboration_join_signing_bytes(
                &room.to_string(),
            ))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .expect("certified join encodes");
    CertifiedEndpointIdentity::new(presentation, device)
}

/// This device, hosting a new workspace's room on its own host as every
/// device does, with the fixture application allowed its own surface.
struct HostedRoom {
    device: Device,
    wakes: std::sync::Arc<Wakes>,
    local: crate::shell::state::WorkspaceId,
    room: crate::shell::state::RoomId,
}

impl HostedRoom {
    /// Start this device as the identity set up under `identity_root`, its
    /// session's state under `dir`, and make the workspace.
    fn start(identity_root: &Path, dir: &Path) -> Self {
        let wakes = std::sync::Arc::new(Wakes::default());
        let device = Device::start_as_this_host(identity_root, "Desktop", dir, 1, &wakes);
        let (local, room) = hosted_workspace(&device, &[], "Drafts room");
        let hosted = Self {
            device,
            wakes,
            local,
            room,
        };
        let fixture = hosted.workspace_node().app(FIXTURE);
        hosted.grant(&tilo_action_policy::Grant::new(
            "fixture-surface",
            tilo_action_policy::Subject::agent(FIXTURE_HANDLE),
            fixture,
            KindSet::ALL,
            Answer::Allow,
            tilo_action_policy::Subject::person(owner_principal()),
        ));
        hosted
    }

    /// Bob joins the room from his own device, its state under `dir`,
    /// through the invitation the room's host issues him as an editor.
    fn bob_joins(&self, dir: &Path) -> Device {
        let person = Person::new(BOB_SEED);
        let device = Device::start(
            &person,
            "Bob's laptop",
            dir,
            2,
            tilo_user_identity::DeviceCapabilities::primary(),
            &self.wakes,
        );
        let invitation = self.device.invite(self.local, "Bob", Some(person.user()));
        device
            .join(&invitation)
            .expect("Bob's join field takes the invitation");
        let joined = until(&[&self.device, &device], "Bob to join the room", || {
            device.joins.lock().expect("joins").pop()
        });
        assert_eq!(joined, Ok(self.room), "Bob joins the room");
        until(
            &[&self.device, &device],
            "Bob to be a member of the room here",
            || {
                room_workspace(&self.device.tree.read(), self.room)
                    .role_of(&bob())
                    .is_some()
                    .then_some(())
            },
        );
        device
    }

    fn network(&self) -> &std::sync::Arc<crate::shell::network::ShellNetwork> {
        self.device.net()
    }

    fn workspace_node(&self) -> TreePath {
        room_workspace(&self.device.tree.read(), self.room).node()
    }

    /// Ask the room's host, as its owner, for the governance change
    /// `action` with `arguments`, and wait until this device's room shows
    /// what `shown` looks for.
    fn govern(
        &self,
        action: &str,
        arguments: Value,
        what: &str,
        shown: impl Fn(&crate::shell::state::Workspace) -> bool,
    ) {
        let Value::Object(arguments) = arguments else {
            panic!("governance arguments are an object");
        };
        self.device
            .govern(self.room, action, arguments)
            .unwrap_or_else(|error| panic!("the room's host takes the change: {error}"));
        until(&[&self.device], what, || {
            shown(room_workspace(&self.device.tree.read(), self.room)).then_some(())
        });
    }

    /// Grant `grant` in the room, as its owner gives it.
    fn grant(&self, grant: &tilo_action_policy::Grant) {
        let id = grant.id.clone();
        self.govern(
            tilo_action_policy::roles::governance::GRANT,
            json!({ "grant": grant }),
            "the grant to reach the room",
            |workspace| workspace.policy.grants.iter().any(|held| held.id == id),
        );
    }

    /// Let `app` keep records in the room, as opening the app there first
    /// does: the room's host lets it author, and its records are sealed
    /// channels the app opens itself, never documents made here.
    fn open_first(&self, app: &str) {
        until(&[&self.device], "the room to seal its records", || {
            self.device
                .tree
                .read()
                .seals_records(self.room)
                .then_some(())
        });
        self.device
            .let_app_author(self.room, app)
            .unwrap_or_else(|error| panic!("the room's host lets {app} author: {error}"));
    }
}

fn exported(
    action: &str,
    label: &str,
    params: &[ActionParam],
    result: ResultSpec,
) -> ExportedAction {
    ExportedAction {
        semantic_identity: None,
        resource: INBOX.into(),
        action: action.into(),
        resource_kind: "fixture.drafts".into(),
        resource_label: "Inbox".into(),
        action_label: label.into(),
        description: None,
        operation_kind: OperationKind::Document,
        params: params.to_vec(),
        contract: ActionContract {
            result,
            ..ActionContract::default()
        },
        view: None,
        input: ExportedInput::None,
    }
}

/// A connection to the collaboration service at `path` from `app_id`,
/// launched into the room as the launcher launches every app: the room's
/// records are its committed documents, which only a launched app reads.
fn launched(path: &Path, app_id: &str, room: &str) -> UnixStream {
    crate::shell::launcher::register_in_process_launch(app_id, room);
    let mut stream = UnixStream::connect(path).expect("connect to the collaboration service");
    launcher::services::write_collab_handoff(&mut stream, app_id, room)
        .expect("hand the connection over as the launcher does");
    stream
}

/// An application with two document actions: `draft` writes a draft and
/// returns its id, `publish` publishes a draft by id.
struct FixtureApp {
    manager: CollaborationManager,
    /// The device's network, whose turn runs each time the app is served,
    /// as the compositor's loop runs beside the app.
    net: std::sync::Arc<crate::shell::network::ShellNetwork>,
    doc: AutoCommit,
    /// Every invocation served, in order: the action and its arguments.
    served: Vec<(String, Map<String, Value>)>,
}

impl FixtureApp {
    fn connect(
        path: &Path,
        room: crate::shell::state::RoomId,
        net: &std::sync::Arc<crate::shell::network::ShellNetwork>,
    ) -> Self {
        let stream = launched(path, FIXTURE, &room.to_string());
        let mut manager = CollaborationManager::for_route_fd_with_identity(
            stream.into_raw_fd(),
            FIXTURE,
            room.to_string().parse().expect("a room is a UUID"),
            Some(RECORD.to_string()),
            Some(FIXTURE_HANDLE.to_string()),
            endpoint_identity(room),
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
                ExportedAction {
                    contract: ActionContract {
                        confirmation: tilo_actions::ConfirmationPolicy::Confirm,
                        ..ActionContract::default()
                    },
                    ..exported("archive", "Archive inbox", &[], ResultSpec::None)
                },
                ExportedAction {
                    resource: OUTBOX.into(),
                    resource_kind: "fixture.outbox".into(),
                    resource_label: "Outbox".into(),
                    ..exported(
                        "draft",
                        "Write draft",
                        &[ActionParam::required(
                            "title",
                            ParamType::Text,
                            "Title of the draft",
                        )],
                        ResultSpec::None,
                    )
                },
            ],
        });
        manager.send(CollabRequest::document_snapshot(doc.save()));
        Self {
            manager,
            net: std::sync::Arc::clone(net),
            doc,
            served: Vec::new(),
        }
    }

    /// Serve whatever the broker sent since the last call.
    fn pump(&mut self) {
        self.net.tick();
        for event in self.manager.poll() {
            match event {
                CollabEvent::DocumentSnapshotRequested { .. } => {
                    let snapshot = CollabRequest::document_snapshot(self.doc.save());
                    self.manager.send(snapshot);
                }
                CollabEvent::Invocation {
                    id,
                    invocation,
                    action,
                    argument,
                    context,
                    ..
                } => {
                    let update =
                        self.serve(invocation, &action, argument.as_deref(), context.as_deref());
                    self.manager
                        .send(CollabRequest::UpdateInvocation { id, update });
                }
                _ => {}
            }
        }
    }

    fn serve(
        &mut self,
        invocation: InvocationId,
        action: &str,
        argument: Option<&str>,
        context: Option<&ActionContext>,
    ) -> InvocationUpdate {
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
            "archive" => {
                self.doc
                    .put(automerge::ROOT, "archived", true)
                    .expect("archive the inbox");
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
    match pending.wait_timeout(tilo_test_env::hang_bound()) {
        Ok(Some(value)) => value,
        _ => panic!("the broker did not answer"),
    }
}

/// A running broker, the door as the room's owner reaches it, and the
/// fixture application once its actions are offered.
struct Door {
    broker: crate::shell::collab_broker::BrokerHandle,
    membership: crate::shell::state::WorkspaceTreeHandle,
    host: crate::shell::remote_actions::BrokerActionHost,
    owner: Caller,
    app: FixtureApp,
    deadline: Instant,
    /// Bob's device, once he joined the room.
    bob: Option<Device>,
    /// This device and the room it hosts, kept up as long as the broker.
    hosted: HostedRoom,
    room: crate::shell::state::RoomId,
    dir: tempfile::TempDir,
    /// This host's own identity, which signs the evidence of every
    /// admission it attests, as a desktop's does.
    _host_identity: tilo_test_env::ScopedEnv,
}

impl Door {
    fn open() -> Self {
        let dir = tempfile::tempdir().expect("socket directory");
        // The broker binds only inside an owner-only directory.
        std::fs::set_permissions(
            dir.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .expect("owner-only socket directory");
        let mut host_identity = tilo_test_env::ScopedEnv::new();
        host_identity
            .set("XDG_CONFIG_HOME", dir.path().join("config"))
            .set("TILO_IDENTITY_ROOT", dir.path().join("system"))
            .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1")
            .set(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent-tilo-test-bus",
            )
            .remove("TILO_USER_IDENTITY")
            .remove("TILO_IDENTITY_BROKER_FD");
        let identity = runtime_identity::identity_dir_from_env().expect("the identity directory");
        crate::firstboot::ensure_throwaway_test_identity(
            &identity,
            &crate::firstboot::collaboration_identity_paths(),
        )
        .expect("the host's own identity");
        let hosted = HostedRoom::start(&dir.path().join("system"), &dir.path().join("session"));
        let room = hosted.room;
        // The person this device acts as in the room is the identity it
        // was set up with.
        let registry = tilo_identity_host::registry::IdentityRegistryStore::at(&identity);
        let selected = registry
            .load_reconciled()
            .expect("the identity registry")
            .default_identity
            .expect("the set-up identity");
        registry
            .bind_workspace(&room.to_string(), &selected)
            .expect("the room acts as the set-up identity");
        assert_eq!(
            crate::shell::state::local_user_id_for_room(&room),
            owner_principal(),
            "the session acts for the room's owner"
        );
        let (_, certificate) =
            tilo_identity_host::broker::trusted_current_device(&room.to_string())
                .expect("this device's identity in the room");
        let held = hosted.device.cert();
        assert_eq!(
            (certificate.user_public_key, certificate.device_public_key),
            (held.user_public_key, held.device_public_key),
            "the broker's device is the one the network holds"
        );
        hosted.open_first(FIXTURE);
        let socket = dir.path().join("collab.sock");
        let broker = crate::shell::collab_broker::spawn_native_owner_harness_with_reads(
            socket.clone(),
            hosted.device.tree.clone(),
            vec![certificate.device_public_key],
            hosted.network(),
            None,
        )
        .expect("bind the collaboration service");
        let host = crate::shell::remote_actions::BrokerActionHost::new(
            broker.app_action_broker().expect("the action broker"),
        );
        let app = FixtureApp::connect(&socket, room, hosted.network());
        let mut door = Self {
            broker,
            membership: hosted.device.tree.clone(),
            host,
            owner: Caller::local_person(owner_principal()),
            app,
            deadline: tilo_test_env::hang_deadline(),
            bob: None,
            hosted,
            room,
            dir,
            _host_identity: host_identity,
        };
        while !["draft", "publish", "archive"]
            .into_iter()
            .all(|action| door.offers(FIXTURE, INBOX, action))
        {
            assert!(
                Instant::now() < door.deadline,
                "the fixture's actions were never offered"
            );
            door.app.pump();
            std::thread::sleep(Duration::from_millis(10));
        }
        door
    }

    /// The person a compositor session on this device acts for in the
    /// room: the room's owner.
    fn session_person(&self) -> String {
        crate::shell::state::local_user_id_for_room(&self.room)
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
        self.invoke_as(&self.owner, resource, action, arguments)
    }

    /// Invoke a published action of the room's own provider as `caller`.
    fn invoke_as(
        &self,
        caller: &Caller,
        resource: &str,
        action: &str,
        arguments: &Value,
    ) -> Result<(InvocationId, ReceiptStatus), ActionHostError> {
        let invocation = InvocationId::new();
        let arguments = arguments.as_object().cloned().unwrap_or_default();
        let receipt = answer(self.host.invoke(
            caller,
            InvokeRequest {
                on_behalf_of: None,
                invocation,
                target: RemoteActionTarget {
                    provider: workspace_provider(&self.room.to_string()),
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
                wait_ms: None,
                appearance: None,
            },
        ))?;
        Ok((invocation, receipt.status))
    }

    fn save(&self, shortcut: &Shortcut) -> Result<ReceiptStatus, ActionHostError> {
        self.save_as(&self.owner, shortcut)
    }

    fn save_as(
        &self,
        caller: &Caller,
        shortcut: &Shortcut,
    ) -> Result<ReceiptStatus, ActionHostError> {
        let arguments = json!({ "shortcut": serde_json::to_value(shortcut).unwrap() });
        self.invoke_as(caller, "shortcuts", "create", &arguments)
            .map(|(_, status)| status)
    }

    /// Save `shortcut` from Bob's own device, as the room's member who
    /// may edit it; he joins the room on it first.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "Bob's device turns its loop until his shortcut reaches the room"
    )]
    fn save_from_bobs_device(&mut self, shortcut: &Shortcut) {
        if self.bob.is_none() {
            self.bob = Some(self.hosted.bob_joins(&self.dir.path().join("bob")));
        }
        let device = self.bob.as_ref().expect("Bob's device");
        let saved = Shortcut {
            saved_by: Some(bob()),
            ..shortcut.clone()
        };
        let saving = crate::shell::broker_services::NativeShortcutLibrary::commit(
            device.tree.clone(),
            Some(std::sync::Arc::clone(device.net())),
            self.room,
            bob(),
            tilo_automation::ShortcutChange::Save(Box::new(saved)),
        );
        device
            .answer("Bob's shortcut save", saving)
            .expect("a member saves a scheduled shortcut from his device");
        let id = shortcut.id.clone();
        until(
            &[&self.hosted.device, device],
            "Bob's shortcut to reach the room",
            || {
                room_workspace(&self.membership.read(), self.room)
                    .shortcuts
                    .iter()
                    .any(|saved| saved.id == id)
                    .then_some(())
            },
        );
    }

    /// The room's owner removes `person` from the room at its host, and
    /// the room here shows them gone. Whether they were a member.
    fn remove_member(&self, person: &str) -> bool {
        if room_workspace(&self.membership.read(), self.room)
            .role_of(person)
            .is_none()
        {
            return false;
        }
        self.hosted.govern(
            tilo_action_policy::roles::governance::REMOVE_MEMBER,
            json!({ "person": person }),
            "the removed member to leave the room",
            |workspace| workspace.role_of(person).is_none(),
        );
        true
    }

    /// Start `trigger` of `shortcut` as `person` in the room, the way the
    /// session does when the trigger fires.
    fn fire(
        &self,
        person: &str,
        shortcut: &str,
        trigger: &TriggerSpec,
        run: RunId,
    ) -> Result<u64, ActionHostError> {
        self.fire_with(person, shortcut, trigger, run, Map::new())
    }

    /// Start the run `firing` asks for, as `person`, with its payload.
    fn fire_firing(&self, person: &str, firing: &Firing) -> Result<u64, ActionHostError> {
        self.fire_with(
            person,
            &firing.shortcut,
            &firing.trigger,
            firing.run,
            firing.inputs.clone(),
        )
    }

    fn fire_with(
        &self,
        person: &str,
        shortcut: &str,
        trigger: &TriggerSpec,
        run: RunId,
        inputs: Map<String, Value>,
    ) -> Result<u64, ActionHostError> {
        self.broker
            .app_action_broker()
            .expect("the action broker")
            .start_triggered_shortcut_run(
                &Caller::local_person(person).in_room(self.room.to_string()),
                &self.room.to_string(),
                shortcut,
                trigger,
                run,
                inputs,
            )
    }

    /// Serve the fixture until broker job `job` ends.
    fn job_to_end(&mut self, job: u64) -> InvocationUpdate {
        let broker = self.broker.app_action_broker().expect("the action broker");
        loop {
            assert!(Instant::now() < self.deadline, "the run never finished");
            self.app.pump();
            match broker.status(job) {
                Some(
                    done @ (InvocationUpdate::Succeeded { .. }
                    | InvocationUpdate::Failed { .. }
                    | InvocationUpdate::Cancelled { .. }),
                ) => return done,
                _ => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    }

    /// Serve the fixture for a moment, for asserting that nothing arrives.
    fn serve_briefly(&mut self) {
        let until = Instant::now()
            .checked_add(Duration::from_millis(300))
            .expect("a moment fits in an Instant");
        while Instant::now() < until {
            self.app.pump();
            std::thread::sleep(Duration::from_millis(5));
        }
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

    fn finished_runs(&self) -> Vec<crate::shell::automation_host::FinishedShortcutRun> {
        loop {
            let finished = self
                .broker
                .app_action_broker()
                .expect("the action broker")
                .take_finished_shortcut_runs();
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
            action: tilo_actions::ActionKey::new(FIXTURE, "fixture.drafts", action),
            resource: Selector::Fixed(INBOX.into()),
            args: args
                .iter()
                .map(|(name, binding)| ((*name).to_string(), binding.clone()))
                .collect(),
            appearance: None,
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
                action_rule(
                    FIXTURE,
                    INBOX_KIND,
                    Some(INBOX),
                    action,
                    KindSet::READ_UNDOABLE,
                    Answer::Allow,
                )
            })
            .collect(),
        triggers: Vec::new(),
        saved_by: None,
        saved_proof: None,
    }
}

/// Every change an action made in `doc` is grouped under run `run` of the
/// shortcut "Publish a draft" in the fixture.
fn assert_attributed_to(doc: &mut AutoCommit, run: InvocationId) {
    let contexts: Vec<ActionContext> = tilo_resource::history::versions(doc)
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
    let stored = Shortcut {
        saved_by: Some(owner_principal()),
        saved_proof: None,
        ..shortcut
    };
    assert_eq!(
        room_workspace(&door.membership.read(), door.room).shortcuts,
        std::slice::from_ref(&stored),
        "the workspace holds the saved shortcut, as saved by the owner"
    );
    assert!(
        door.offers(SHORTCUTS_APP_ID, &resource, "run"),
        "the saved shortcut is offered as `{resource}#run`"
    );

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
    crate::shell::automation_host::record_shortcut_run(
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
    assert_eq!(recorded.steps[0].sentence(), "Draft, Publish");

    // Undoing the run reverts both steps.
    let mut doc = std::mem::take(&mut door.app.doc);
    drop(door);
    let report = ui::undo_run(
        recorded,
        None,
        &ui::AuthorDirectory::default(),
        &mut [(FIXTURE.to_string(), RECORD.to_string(), &mut doc)],
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
        room_workspace(&door.membership.read(), door.room)
            .shortcuts
            .is_empty()
    );
    assert!(!door.offers(SHORTCUTS_APP_ID, &shortcut_resource_id("sneaky"), "run"));
    let served_nothing = door.app.served.is_empty();
    drop(door);
    assert!(served_nothing, "nothing ran");
}

/// "Publish a draft" with a fixed title, so nobody need give it inputs,
/// started by `triggers`.
fn triggered_draft(id: &str, triggers: Vec<TriggerSpec>) -> Shortcut {
    let mut shortcut = publish_draft(id, &["draft", "publish"]);
    shortcut.inputs.clear();
    shortcut.steps[0] = invoke_step(
        "draft",
        "draft",
        &[("title", Binding::Literal(json!("Weekly notes")))],
    );
    shortcut.triggers = triggers;
    shortcut
}

fn hourly(catch_up: CatchUp) -> TriggerSpec {
    TriggerSpec::Schedule {
        schedule: Schedule::Interval { seconds: 3600 },
        catch_up,
    }
}

fn at(text: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(text)
        .expect("an RFC 3339 instant")
        .with_timezone(&chrono::Utc)
}

fn utc_clock(now: &str) -> std::sync::Arc<ManualClock> {
    std::sync::Arc::new(ManualClock::new(
        at(now),
        chrono::FixedOffset::east_opt(0).expect("UTC"),
    ))
}

/// What `triggers` fire now for the shortcuts `person` saved in the room:
/// show them the passing time, then wait for the schedule ledger the
/// tick handed to the state writer and take the firings it held, the way
/// the loop does once the write wakes it.
fn tick(triggers: &mut TriggerHost, door: &Door, person: &str) -> Vec<Firing> {
    let tree = door.membership.read();
    let armed = armed(
        &tree.desktop.workspaces,
        |_| person.to_string(),
        |ws| tree.room_revocations(ws.room()),
        tilo_clock::unix_now_ns(),
    );
    let mut fired = triggers.observe(TriggerEvent::Tick, &armed);
    drop(tree);
    crate::shell::durable_writer::global().flush();
    fired.extend(triggers.take_settled_firings());
    assert!(!triggers.holds_firings(), "a written ledger holds nothing");
    fired
}

fn served_actions(door: &Door) -> Vec<String> {
    door.app
        .served
        .iter()
        .map(|(action, _)| action.clone())
        .collect()
}

/// Press `key` with `modifiers` in `host`, after one layout pass.
fn press(
    host: &mut crate::shell::overlay_host::ShellOverlayHost,
    key: winit::keyboard::Key,
    modifiers: winit::keyboard::ModifiersState,
) {
    use std::collections::HashMap;
    use taffy::{AvailableSpace, NodeId, Size, Style, TaffyTree};
    use ui::widget::Widget as _;

    let theme = ui::theme::Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn ui::widget::Widget> = HashMap::new();
    let child = host.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size::from_lengths(1280.0, 800.0),
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut text = ui::text_render::TextRenderer::new();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(1280.0),
                height: AvailableSpace::Definite(800.0),
            },
            |known, available, node_id, _, _| match node_map.get(&node_id) {
                Some(&widget) => unsafe { (*widget).measure(known, available, &mut text) },
                None => Size::ZERO,
            },
        )
        .unwrap();
    host.apply_layout(&taffy, child, vello::kurbo::Point::ORIGIN);

    let mut event = ui::synthetic_input::key_event(key, None, winit::event::ElementState::Pressed);
    // A plain character key also carries the text it types.
    if modifiers.is_empty()
        && let winit::keyboard::Key::Character(text) = &event.logical_key
    {
        event.text = Some(text.clone());
    }
    let event = ui::synthetic_input::keyboard_input(event);
    let handled = ui::testing::Dispatch::new()
        .modifiers(modifiers)
        .layout(&taffy, child)
        .send(host, &event)
        .handled;
    assert!(handled, "the shell took the chord");
}

#[test]
fn a_chord_pressed_in_the_shell_runs_the_shortcut_saved_with_it() {
    let mut door = Door::open();
    // The person is read while the door's host identity is installed:
    // dropping the door restores the environment it replaced.
    let person = door.session_person();
    let session = Caller::local_person(person.clone());
    let chord = TriggerSpec::Keyboard {
        chord: "Super+Shift+Y".into(),
    };
    door.save_as(&session, &triggered_draft("from-keys", vec![chord]))
        .expect("the shortcut is saved with its keyboard trigger");

    let (mut host, _chrome) =
        crate::shell::build_tilo_overlay_host_with_handle(door.membership.clone(), false);
    host.set_shortcut_config(settings_store::hardware::ShortcutConfig::default())
        .expect("the built-in shortcut table is valid");
    host.attach_app_action_broker(door.broker.app_action_broker().expect("the action broker"));
    host.tick_shortcut_triggers_now();
    // Building a compositor host is slow on a cold process; the run gets
    // its own full hang bound from here.
    door.deadline = tilo_test_env::hang_deadline();
    let listed: Vec<(String, String)> = host
        .action_chords()
        .iter()
        .map(|bound| (bound.label.clone(), bound.chord.display()))
        .collect();
    assert_eq!(
        listed,
        [(
            "Run Publish a draft".to_string(),
            "Super+Shift+y".to_string()
        )],
        "the guide and Settings list the chord with what it runs"
    );

    press(
        &mut host,
        winit::keyboard::Key::Character("Y".into()),
        winit::keyboard::ModifiersState::SUPER | winit::keyboard::ModifiersState::SHIFT,
    );
    while door.app.served.len() < 2 {
        assert!(
            Instant::now() < door.deadline,
            "the chord never ran the shortcut"
        );
        door.app.pump();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(served_actions(&door), ["draft", "publish"]);
    let finished = door.finished_runs();
    drop(door);
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].report.trigger, "keyboard");
    assert_eq!(finished[0].invoker, person);
}

#[test]
fn a_chord_the_desktop_owns_cannot_start_a_shortcut() {
    let door = Door::open();
    let fixed = triggered_draft(
        "steals-alt-tab",
        vec![TriggerSpec::Keyboard {
            chord: "Alt+Tab".into(),
        }],
    );
    match door.save(&fixed) {
        Err(ActionHostError::Refused { reason }) => {
            assert!(reason.contains("Switch windows"), "{reason}");
        }
        other => panic!("a fixed desktop chord must be refused: {other:?}"),
    }
    let watching = triggered_draft(
        "on-network",
        vec![TriggerSpec::NetworkChanged {
            network: None,
            connected: Some(true),
        }],
    );
    let watching_refused = matches!(door.save(&watching), Err(ActionHostError::Refused { .. }));
    drop(door);
    assert!(watching_refused, "network triggers cannot be saved yet");
}

#[test]
fn an_app_opening_runs_the_shortcut_with_the_app_bound_as_its_input() {
    let mut door = Door::open();
    let owner = owner_principal();
    let mut on_open = publish_draft("on-open", &["draft", "publish"]);
    on_open.inputs = vec![ActionParam::required(
        "app_id",
        ParamType::Text,
        "The application that opened",
    )];
    on_open.steps[0] = invoke_step(
        "draft",
        "draft",
        &[("title", Binding::Input("app_id".into()))],
    );
    on_open.triggers = vec![TriggerSpec::AppOpened {
        app_id: FIXTURE.into(),
    }];
    door.save(&on_open)
        .expect("a required input every trigger gives may be required");

    let mut mistyped = on_open.clone();
    mistyped.id = "mistyped".into();
    mistyped.inputs[0].ty = ParamType::Integer;
    match door.save(&mistyped) {
        Err(ActionHostError::Refused { reason }) => {
            assert!(reason.contains("`app_id` is integer"), "{reason}");
        }
        other => panic!("an input typed against its trigger is refused: {other:?}"),
    }

    let mut triggers = TriggerHost::new(utc_clock("2026-09-25T08:00:00Z"), None);
    let observe = |triggers: &mut TriggerHost, running: &[&str]| {
        let running: std::collections::BTreeSet<String> =
            running.iter().map(ToString::to_string).collect();
        let tree = door.membership.read();
        let armed = armed(
            &tree.desktop.workspaces,
            |_| owner.clone(),
            |ws| tree.room_revocations(ws.room()),
            tilo_clock::unix_now_ns(),
        );
        let fired = triggers.observe(TriggerEvent::Apps(&running), &armed);
        drop(tree);
        fired
    };
    assert!(
        observe(&mut triggers, &["so.tilo.notes"]).is_empty(),
        "what is already running when the session starts did not just open"
    );
    let fired = observe(&mut triggers, &["so.tilo.notes", FIXTURE]);
    assert_eq!(fired.len(), 1, "{fired:?}");
    assert_eq!(fired[0].inputs.get("app_id"), Some(&json!(FIXTURE)));
    assert!(observe(&mut triggers, &["so.tilo.notes", FIXTURE]).is_empty());

    let job = door
        .fire_firing(&owner, &fired[0])
        .expect("the trigger starts the shortcut");
    assert!(matches!(
        door.job_to_end(job),
        InvocationUpdate::Succeeded { .. }
    ));
    let served = served_actions(&door);
    let title = door.app.served[0].1.get("title").cloned();
    drop(door);
    assert_eq!(served, ["draft", "publish"]);
    assert_eq!(
        title,
        Some(json!(FIXTURE)),
        "the opened app's id reached the step through the shortcut's input"
    );
}

#[test]
fn a_schedule_fires_once_per_fire_time_across_a_restart() {
    let mut door = Door::open();
    let owner = owner_principal();
    let ledger = door.dir.path().join("schedules.json");
    let clock = utc_clock("2026-09-25T08:00:00Z");
    door.save(&triggered_draft("hourly", vec![hourly(CatchUp::Skip)]))
        .expect("the shortcut is saved with its schedule");

    let mut triggers = TriggerHost::new(clock.clone(), Some(ledger.clone()));
    assert!(
        tick(&mut triggers, &door, &owner).is_empty(),
        "arming a schedule fires nothing"
    );
    clock.advance(chrono::Duration::minutes(59));
    assert!(tick(&mut triggers, &door, &owner).is_empty());

    // Restarted before it is due: the persisted fire time still holds.
    drop(triggers);
    let mut triggers = TriggerHost::new(clock.clone(), Some(ledger.clone()));
    clock.advance(chrono::Duration::minutes(1));
    let fired = tick(&mut triggers, &door, &owner);
    assert_eq!(fired.len(), 1, "the fire time came: {fired:?}");
    let job = door
        .fire(&owner, &fired[0].shortcut, &fired[0].trigger, fired[0].run)
        .expect("the trigger starts the shortcut");
    assert!(
        matches!(door.job_to_end(job), InvocationUpdate::Succeeded { .. }),
        "the scheduled run completes"
    );

    // Restarted right after it fired: that fire time is not fired again.
    drop(triggers);
    let mut triggers = TriggerHost::new(clock.clone(), Some(ledger));
    assert!(tick(&mut triggers, &door, &owner).is_empty());
    clock.advance(chrono::Duration::minutes(30));
    assert!(tick(&mut triggers, &door, &owner).is_empty());
    door.serve_briefly();
    assert_eq!(
        served_actions(&door),
        ["draft", "publish"],
        "the schedule ran exactly once"
    );
    let finished = door.finished_runs();
    drop(door);
    assert_eq!(finished[0].report.trigger, "schedule");
}

#[test]
fn fire_times_missed_while_asleep_are_skipped_or_caught_up_once() {
    let mut door = Door::open();
    let owner = owner_principal();
    let clock = utc_clock("2026-09-25T08:00:00Z");
    door.save(&triggered_draft("skips", vec![hourly(CatchUp::Skip)]))
        .expect("saved");
    door.save(&triggered_draft(
        "catches-up",
        vec![hourly(CatchUp::RunOnce)],
    ))
    .expect("saved");
    let mut triggers = TriggerHost::new(clock.clone(), None);
    assert!(tick(&mut triggers, &door, &owner).is_empty());

    // Asleep for five fire times.
    clock.advance(chrono::Duration::hours(5) + chrono::Duration::minutes(10));
    let fired = tick(&mut triggers, &door, &owner);
    let names: Vec<&str> = fired
        .iter()
        .map(|firing| firing.shortcut.as_str())
        .collect();
    assert_eq!(
        names,
        ["catches-up"],
        "one catch-up run for however many were missed, none for the skipper"
    );
    let job = door
        .fire(&owner, &fired[0].shortcut, &fired[0].trigger, fired[0].run)
        .expect("the catch-up run starts");
    assert!(matches!(
        door.job_to_end(job),
        InvocationUpdate::Succeeded { .. }
    ));
    assert!(tick(&mut triggers, &door, &owner).is_empty());

    // Both are on time again at the next fire time after waking.
    clock.advance(chrono::Duration::minutes(50));
    let mut names: Vec<String> = tick(&mut triggers, &door, &owner)
        .into_iter()
        .map(|firing| firing.shortcut)
        .collect();
    drop(door);
    names.sort();
    assert_eq!(names, ["catches-up", "skips"]);
}

#[test]
fn an_unattended_run_takes_a_confirmed_step_only_with_advance_consent() {
    let mut door = Door::open();
    let owner = owner_principal();
    let schedule = hourly(CatchUp::Skip);
    let mut tidy = triggered_draft("tidy", vec![schedule.clone()]);
    tidy.steps = vec![invoke_step("archive", "archive", &[])];
    tidy.output = None;
    // A grant on the whole app lets the run take the step, but consents to
    // no confirmation in advance.
    tidy.grant.push(Rule::new(
        TreePath::root().any(Level::Room).app(FIXTURE),
        KindSet::READ_UNDOABLE,
        Answer::Allow,
    ));
    door.save(&tidy).expect("saved without consent");
    let job = door
        .fire(&owner, "tidy", &schedule, RunId::new())
        .expect("the trigger starts the shortcut");
    match door.job_to_end(job) {
        InvocationUpdate::Failed { error, .. } => assert!(
            error.message.contains("needs confirmation"),
            "{}",
            error.message
        ),
        other => panic!("nobody may confirm an unattended run: {other:?}"),
    }
    assert!(door.app.served.is_empty(), "nothing was archived");

    tidy.grant.push(archive_grant());
    door.invoke(
        "shortcuts",
        "update",
        &json!({ "shortcut": serde_json::to_value(&tidy).unwrap() }),
    )
    .expect("saved with its saver's advance consent");
    let run = answer(door.host.query(
        &door.owner,
        CatalogueQuery {
            target: Some(ActionAddress::new(shortcut_resource_id("tidy"), "run")),
            max_results: MAX_CATALOGUE_PAGE,
            ..CatalogueQuery::default()
        },
    ))
    .expect("catalogue")
    .rows
    .into_iter()
    .find_map(|row| row.action)
    .expect("the shortcut is offered");
    assert!(
        run.description.as_deref().is_some_and(|text| text
            .split_once("Confirmed in advance: ")
            .is_some_and(|(_, consents)| consents.contains("`archive` on fixture.drafts:inbox"))),
        "the consent is shown with the shortcut: {:?}",
        run.description
    );
    let job = door
        .fire(&owner, "tidy", &schedule, RunId::new())
        .expect("the trigger starts the shortcut");
    assert!(matches!(
        door.job_to_end(job),
        InvocationUpdate::Succeeded { .. }
    ));
    let served = served_actions(&door);
    drop(door);
    assert_eq!(served, ["archive"]);
}

#[test]
fn revoking_the_savers_membership_or_deleting_the_shortcut_stops_its_triggers() {
    let mut door = Door::open();
    let clock = utc_clock("2026-09-25T08:00:00Z");
    let schedule = hourly(CatchUp::Skip);
    door.save_from_bobs_device(&triggered_draft("bobs", vec![schedule.clone()]));
    let mut triggers = TriggerHost::new(clock.clone(), None);
    assert!(
        tick(&mut triggers, &door, &owner_principal()).is_empty()
            && tick(&mut triggers, &door, &owner_principal()).is_empty(),
        "the owner's session does not arm a member's shortcut"
    );
    assert!(
        matches!(
            door.fire(&owner_principal(), "bobs", &schedule, RunId::new()),
            Err(ActionHostError::NotPermitted)
        ),
        "nor may it start one"
    );

    assert!(tick(&mut triggers, &door, &bob()).is_empty());
    clock.advance(chrono::Duration::hours(1));
    let fired = tick(&mut triggers, &door, &bob());
    assert_eq!(fired.len(), 1);
    let job = door
        .fire(&bob(), "bobs", &fired[0].trigger, fired[0].run)
        .expect("the saver's trigger starts it");
    assert!(matches!(
        door.job_to_end(job),
        InvocationUpdate::Succeeded { .. }
    ));

    // The owner revokes Bob's membership: the next fire starts nothing.
    assert!(door.remove_member(&bob()), "Bob was a member to remove");
    clock.advance(chrono::Duration::hours(1));
    let fired = tick(&mut triggers, &door, &bob());
    assert_eq!(fired.len(), 1, "the schedule itself still comes round");
    assert!(
        door.fire(&bob(), "bobs", &fired[0].trigger, fired[0].run)
            .is_err(),
        "a revoked member's shortcut does not start"
    );
    door.serve_briefly();
    assert_eq!(
        served_actions(&door),
        ["draft", "publish"],
        "only the first run ran"
    );

    // Deleting the shortcut disarms it altogether.
    door.invoke("shortcuts", "delete", &json!({ "id": "bobs" }))
        .expect("the owner deletes it");
    clock.advance(chrono::Duration::hours(1));
    assert!(tick(&mut triggers, &door, &bob()).is_empty());
    let fired_after_delete = door.fire(&bob(), "bobs", &schedule, RunId::new());
    drop(door);
    assert!(matches!(
        fired_after_delete,
        Err(ActionHostError::UnknownAction)
    ));
}

/// An application the owner is using, connected as itself.
struct AppConnection {
    manager: CollaborationManager,
    next_correlation: u64,
}

impl AppConnection {
    /// `app_id` opening its record of `device` in the door's room, as the
    /// launcher opens it there.
    fn connect(door: &Door, app_id: &str, device: &str) -> Self {
        let record = format!("file:{device}.qdoc");
        door.hosted.open_first(app_id);
        let stream = launched(
            &door.dir.path().join("collab.sock"),
            app_id,
            &door.room.to_string(),
        );
        let manager = CollaborationManager::for_route_fd_with_identity(
            stream.into_raw_fd(),
            app_id,
            door.room.to_string().parse().expect("a room is a UUID"),
            Some(record),
            None,
            endpoint_identity(door.room),
        )
        .expect("start the collaboration client");
        Self {
            manager,
            next_correlation: 1,
        }
    }

    /// Send `message` and wait for the answer `answered` picks out, serving
    /// the fixture meanwhile.
    fn ask(
        &mut self,
        door: &mut Door,
        message: RemoteActionMessage,
        answered: impl Fn(&RemoteActionMessage) -> bool,
    ) -> RemoteActionMessage {
        self.ask_while(door, message, answered, || {})
    }

    /// [`Self::ask`], doing `meanwhile` each time round, as a person at
    /// the screen would.
    fn ask_while(
        &mut self,
        door: &mut Door,
        message: RemoteActionMessage,
        answered: impl Fn(&RemoteActionMessage) -> bool,
        mut meanwhile: impl FnMut(),
    ) -> RemoteActionMessage {
        self.manager.send(CollabRequest::RemoteAction { message });
        loop {
            assert!(Instant::now() < door.deadline, "the door never answered");
            door.app.pump();
            meanwhile();
            for event in self.manager.poll() {
                if let CollabEvent::RemoteAction { message } = event
                    && answered(&message)
                {
                    return message;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "the result is the door's answer; a malformed answer fails the test"
    )]
    fn catalogue(&mut self, door: &mut Door) -> Result<CataloguePage, ActionHostError> {
        let correlation = self.next_correlation;
        self.next_correlation = self.next_correlation.wrapping_add(1);
        let answer = self.ask(
            door,
            RemoteActionMessage::CatalogueRequest {
                correlation,
                query: CatalogueQuery {
                    max_results: MAX_CATALOGUE_PAGE,
                    ..CatalogueQuery::default()
                },
            },
            |message| {
                matches!(
                    message,
                    RemoteActionMessage::CatalogueResponse { correlation: answered, .. }
                        if *answered == correlation
                )
            },
        );
        match answer {
            RemoteActionMessage::CatalogueResponse { result, .. } => result,
            other => panic!("not a catalogue answer: {other:?}"),
        }
    }

    fn invoke(
        &mut self,
        door: &mut Door,
        target: RemoteActionTarget,
        arguments: &Value,
    ) -> ReceiptStatus {
        self.invoke_while(door, target, arguments, || {})
    }

    fn invoke_while(
        &mut self,
        door: &mut Door,
        target: RemoteActionTarget,
        arguments: &Value,
        meanwhile: impl FnMut(),
    ) -> ReceiptStatus {
        let invocation = InvocationId::new();
        let arguments = arguments.as_object().cloned().unwrap_or_default();
        let answer = self.ask_while(
            door,
            RemoteActionMessage::Invoke(InvokeRequest {
                on_behalf_of: None,
                invocation,
                target,
                argument: if arguments.is_empty() {
                    InvokeArgument::None
                } else {
                    InvokeArgument::Json(arguments)
                },
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
            }),
            |message| {
                matches!(
                    message,
                    RemoteActionMessage::Receipt(receipt) if receipt.invocation == invocation
                )
            },
            meanwhile,
        );
        match answer {
            RemoteActionMessage::Receipt(receipt) => receipt.status,
            other => panic!("not a receipt: {other:?}"),
        }
    }

    /// Invoke `target`, answering the grant sheet the way `answer` says —
    /// `None` for a person who expects no sheet. Returns the receipt and
    /// the sheet as the person saw it, if one came up.
    fn invoke_answering(
        &mut self,
        door: &mut Door,
        sheet: &mut PortalDialogController,
        target: RemoteActionTarget,
        arguments: &Value,
        answer: Option<AppActionConsent>,
    ) -> (ReceiptStatus, Option<DialogView>) {
        let mut seen = None;
        let status = self.invoke_while(door, target, arguments, || {
            sheet.pump();
            let Some(view) = sheet.view() else {
                return;
            };
            let answer = answer.unwrap_or_else(|| panic!("no sheet was expected: {view:?}"));
            seen = Some(view);
            answer_sheet(sheet, answer);
        });
        (status, seen)
    }
}

/// Answer the sheet on screen by clicking it, as the person would.
fn answer_sheet(sheet: &mut PortalDialogController, answer: AppActionConsent) {
    let layout = sheet
        .layout(vello::kurbo::Rect::new(0.0, 0.0, 1600.0, 900.0))
        .expect("the sheet is laid out");
    if answer == AppActionConsent::Deny {
        assert!(sheet.click(layout.cancel.center(), &layout), "Deny answers");
        return;
    }
    let row = AppActionConsent::ALLOWING
        .iter()
        .position(|choice| *choice == answer)
        .expect("an allowing answer");
    assert!(sheet.click(layout.choices[row].center(), &layout));
    assert!(
        sheet.click(layout.confirm.center(), &layout),
        "Allow answers"
    );
}

/// The door with declared access: the shortcuts app may read the
/// catalogue, the invoker may also invoke, the silent app declared nothing.
fn door_with_declared_access() -> Door {
    let door = Door::open();
    door.broker
        .app_action_broker()
        .expect("the action broker")
        .set_app_action_access(std::sync::Arc::new(|app: &str| {
            crate::shell::app_action_access::ActionAccess {
                query: app == SHORTCUTS_APP_ID || app == INVOKER_APP,
                invoke: app == INVOKER_APP,
            }
        }));
    door
}

fn fixture_draft(door: &Door) -> RemoteActionTarget {
    fixture_target(door, INBOX, "draft")
}

fn fixture_target(door: &Door, resource: &str, action: &str) -> RemoteActionTarget {
    answer(door.host.query(
        &door.owner,
        CatalogueQuery {
            max_results: MAX_CATALOGUE_PAGE,
            ..CatalogueQuery::default()
        },
    ))
    .expect("catalogue")
    .rows
    .iter()
    .find(|row| {
        row.app_id == FIXTURE
            && row.action.as_ref().is_some_and(|descriptor| {
                descriptor.address.resource == resource && descriptor.address.action == action
            })
    })
    .and_then(tilo_actions::remote::CatalogueRow::target)
    .expect("the fixture offers the action")
}

fn library(door: &Door, action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: workspace_provider(&door.room.to_string()),
        address: ActionAddress::new("shortcuts", action),
    }
}

#[test]
fn an_app_that_declared_nothing_is_refused_the_catalogue() {
    let mut door = door_with_declared_access();
    let mut silent = AppConnection::connect(&door, SILENT_APP, SILENT_HANDLE);
    assert_eq!(
        silent.catalogue(&mut door).map(|page| page.rows.len()),
        Err(ActionHostError::NotPermitted),
        "reading the catalogue needs `[actions] query`"
    );
    let draft = fixture_draft(&door);
    assert!(matches!(
        silent.invoke(&mut door, draft, &json!({"title": "x"})),
        ReceiptStatus::Rejected {
            error: ActionHostError::NotPermitted
        }
    ));
    let served_nothing = door.app.served.is_empty();
    drop(door);
    assert!(served_nothing, "nothing ran");
}

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the door serves every call to the end of the test"
)]
fn the_shortcuts_app_uses_its_library_but_cannot_invoke_another_app_itself() {
    let mut door = door_with_declared_access();
    let mut shortcuts = AppConnection::connect(&door, SHORTCUTS_APP_ID, SHORTCUTS_HANDLE);

    let page = shortcuts
        .catalogue(&mut door)
        .expect("the shortcuts app declared `[actions] query`");
    assert!(
        page.rows.iter().any(|row| row.app_id == FIXTURE),
        "it reads what the person using it is offered"
    );

    let draft = fixture_draft(&door);
    match shortcuts.invoke(&mut door, draft, &json!({"title": "x"})) {
        ReceiptStatus::Rejected {
            error: ActionHostError::NotPermitted,
        } => {}
        other => panic!("another app's action is not the shortcuts app's to invoke: {other:?}"),
    }
    assert!(door.app.served.is_empty(), "the fixture was never reached");

    // Its own library is its own, used as the person.
    let saved = publish_draft("from-the-app", &["draft", "publish"]);
    let arguments = json!({ "shortcut": serde_json::to_value(&saved).unwrap() });
    let create = library(&door, "create");
    match shortcuts.invoke(&mut door, create, &arguments) {
        ReceiptStatus::Completed { .. } => {}
        other => panic!("the shortcuts app saves through its own library: {other:?}"),
    }
    let list = library(&door, "list");
    match shortcuts.invoke(&mut door, list, &json!({})) {
        ReceiptStatus::Completed { output } => {
            let listed: Vec<Shortcut> =
                serde_json::from_value(output.value.expect("the library")).unwrap();
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].steps, saved.steps);
            assert_eq!(
                listed[0].saved_by.as_deref(),
                Some(owner_principal().as_str()),
                "saved as the person using the app, not as the app"
            );
        }
        other => panic!("the library lists: {other:?}"),
    }
}

#[test]
fn an_app_that_may_invoke_still_needs_the_workspace_to_grant_it() {
    let mut door = door_with_declared_access();
    let mut invoker = AppConnection::connect(&door, INVOKER_APP, INVOKER_HANDLE);
    let draft = fixture_draft(&door);

    match invoker.invoke(&mut door, draft.clone(), &json!({"title": "x"})) {
        ReceiptStatus::Rejected { .. } => {}
        other => panic!("an ungranted app is refused: {other:?}"),
    }
    assert!(door.app.served.is_empty(), "nothing ran");

    let node = door.hosted.workspace_node().app(FIXTURE);
    door.hosted.grant(&tilo_action_policy::Grant::new(
        "invoker-fixture",
        tilo_action_policy::Subject::app(INVOKER_APP),
        node,
        KindSet::READ_UNDOABLE,
        Answer::Allow,
        tilo_action_policy::Subject::person(owner_principal()),
    ));
    if let ReceiptStatus::Rejected { error } =
        invoker.invoke(&mut door, draft, &json!({"title": "Granted"}))
    {
        panic!("a granted app is admitted: {error:?}");
    }
    while door.app.served.is_empty() {
        assert!(
            Instant::now() < door.deadline,
            "the fixture was never reached"
        );
        door.app.pump();
        std::thread::sleep(Duration::from_millis(5));
    }
    let served = door.app.served[0].0.clone();
    drop(door);
    assert_eq!(served, "draft");
}

/// The door with declared access and a person who answers grant questions
/// on the returned sheet when `present`.
fn door_asking(present: bool) -> (Door, PortalDialogController) {
    let door = door_with_declared_access();
    let queue = PortalDialogQueue::default();
    door.broker
        .app_action_broker()
        .expect("the action broker")
        .set_app_action_consent(std::sync::Arc::new(SheetPrompter::new(
            queue.clone(),
            std::sync::Arc::new(move |_| present),
        )));
    (door, PortalDialogController::new(queue))
}

/// Grants that let the invoker use the fixture's `draft`.
fn invoker_draft_grants(door: &Door) -> Vec<tilo_action_policy::Grant> {
    let tree = door.membership.read();
    let room = room_workspace(&tree, door.room);
    let draft = tilo_actions::action_node(&room.node(), FIXTURE, INBOX_KIND, INBOX, None, "draft");
    let grants = room
        .policy
        .grants
        .iter()
        .filter(|grant| {
            grant.subject == tilo_action_policy::Subject::app(INVOKER_APP) && grant.node == draft
        })
        .cloned()
        .collect();
    drop(tree);
    grants
}

fn history_mentions(door: &Door, text: &str) -> bool {
    room_workspace(&door.membership.read(), door.room)
        .effect_receipts
        .iter()
        .any(|receipt| {
            ui::history_source::sentence::step_summary(&receipt.context)
                .is_some_and(|sentence| sentence.contains(text))
        })
}

fn served_until(door: &mut Door, count: usize) {
    while door.app.served.len() < count {
        assert!(
            Instant::now() < door.deadline,
            "the fixture was never reached"
        );
        door.app.pump();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn not_permitted(status: &ReceiptStatus) -> bool {
    matches!(
        status,
        ReceiptStatus::Rejected {
            error: ActionHostError::NotPermitted
        }
    )
}

#[test]
fn allowing_once_admits_that_call_and_the_next_asks_again() {
    let (mut door, mut sheet) = door_asking(true);
    let mut invoker = AppConnection::connect(&door, INVOKER_APP, INVOKER_HANDLE);
    let draft = fixture_draft(&door);

    let (status, seen) = invoker.invoke_answering(
        &mut door,
        &mut sheet,
        draft.clone(),
        &json!({"title": "Once"}),
        Some(AppActionConsent::Once),
    );
    if let ReceiptStatus::Rejected { error } = status {
        panic!("allowed once, the call goes ahead: {error:?}");
    }
    let seen = seen.expect("the person was asked");
    assert!(
        seen.title().contains("wants to use"),
        "the sheet names who wants what: {seen:?}"
    );
    assert_eq!(
        seen.app_name(),
        "Inbox · Write draft",
        "the action as published, on the resource it is on"
    );
    assert!(seen.detail().contains("Low risk"), "{seen:?}");
    assert_eq!(
        seen.choices(),
        [
            "Allow once",
            "Allow for this session",
            "Always allow in this workspace"
        ]
    );
    assert_eq!(seen.cancel_label(), "Deny");
    served_until(&mut door, 1);
    assert_eq!(door.app.served[0].0, "draft");
    assert!(
        invoker_draft_grants(&door).is_empty(),
        "a grant given once is gone once used"
    );
    assert!(
        history_mentions(&door, "Allowed"),
        "the grant is in the workspace's history"
    );

    let (status, seen) = invoker.invoke_answering(
        &mut door,
        &mut sheet,
        draft,
        &json!({"title": "Again"}),
        Some(AppActionConsent::Deny),
    );
    assert!(seen.is_some(), "the next call asks again");
    assert!(not_permitted(&status), "{status:?}");
    door.serve_briefly();
    assert_eq!(door.app.served.len(), 1, "the denied call never ran");
}

/// An app whose grant asks first does not confirm the action by its own
/// word: the person is asked on their sheet each time, and the call runs
/// only on their answer.
#[test]
fn a_grant_that_asks_first_runs_each_call_on_the_persons_confirmation() {
    let (mut door, mut sheet) = door_asking(true);
    let mut invoker = AppConnection::connect(&door, INVOKER_APP, INVOKER_HANDLE);
    let draft = fixture_draft(&door);
    let node = tilo_actions::action_node(
        &door.hosted.workspace_node(),
        FIXTURE,
        INBOX_KIND,
        INBOX,
        None,
        "draft",
    );
    door.hosted.grant(&tilo_action_policy::Grant::new(
        "invoker-drafts-when-asked",
        tilo_action_policy::Subject::app(INVOKER_APP),
        node,
        KindSet::READ_UNDOABLE,
        Answer::Ask,
        tilo_action_policy::Subject::person(owner_principal()),
    ));

    // The app claims a confirmation of its own; it counts for nothing.
    let mut confirm = |door: &mut Door, answer: AppActionConsent, title: &str| {
        let invocation = InvocationId::new();
        let mut seen = None;
        let reply = invoker.ask_while(
            door,
            RemoteActionMessage::Invoke(InvokeRequest {
                on_behalf_of: None,
                invocation,
                target: draft.clone(),
                argument: InvokeArgument::Json(
                    json!({ "title": title })
                        .as_object()
                        .cloned()
                        .expect("an object"),
                ),
                context: None,
                confirmation: Some(tilo_actions::remote::Confirmation::Confirmed),
                revision: None,
                wait_ms: None,
                appearance: None,
            }),
            |message| {
                matches!(
                    message,
                    RemoteActionMessage::Receipt(receipt) if receipt.invocation == invocation
                )
            },
            || {
                sheet.pump();
                if let Some(view) = sheet.view() {
                    seen = Some(view);
                    answer_sheet(&mut sheet, answer);
                }
            },
        );
        let RemoteActionMessage::Receipt(receipt) = reply else {
            panic!("not a receipt: {reply:?}");
        };
        (receipt.status, seen)
    };

    let (status, seen) = confirm(&mut door, AppActionConsent::Once, "Confirmed");
    if let ReceiptStatus::Rejected { error } = status {
        panic!("confirmed by the person, the call goes ahead: {error:?}");
    }
    let seen = seen.expect("the person was asked, not the app's word taken");
    assert!(
        seen.title().contains("wants to act for you"),
        "the sheet asks to confirm one call: {seen:?}"
    );
    assert_eq!(seen.app_name(), "Inbox · Write draft");
    assert_eq!(
        seen.choices(),
        ["Allow once"],
        "confirming grants nothing beyond this call"
    );
    served_until(&mut door, 1);
    assert_eq!(
        invoker_draft_grants(&door).len(),
        1,
        "nothing was granted beyond the grant that asks"
    );

    let (status, seen) = confirm(&mut door, AppActionConsent::Deny, "Declined");
    assert!(seen.is_some(), "every call asks again");
    assert!(not_permitted(&status), "{status:?}");
    door.serve_briefly();
    assert_eq!(door.app.served.len(), 1, "the declined call never ran");
}

/// A grant names one resource's action: the same action on another
/// resource is another action, asked about afresh and refused when denied.
#[test]
fn an_app_granted_one_resources_action_is_asked_again_for_another_resource() {
    let (mut door, mut sheet) = door_asking(true);
    let mut invoker = AppConnection::connect(&door, INVOKER_APP, INVOKER_HANDLE);
    let inbox = fixture_draft(&door);
    let outbox = fixture_target(&door, OUTBOX, "draft");

    let (status, seen) = invoker.invoke_answering(
        &mut door,
        &mut sheet,
        inbox,
        &json!({"title": "First"}),
        Some(AppActionConsent::Always),
    );
    assert!(seen.is_some());
    if let ReceiptStatus::Rejected { error } = status {
        panic!("always allowed, the call goes ahead: {error:?}");
    }
    served_until(&mut door, 1);
    assert_eq!(invoker_draft_grants(&door).len(), 1, "a grant on the inbox");

    let (status, seen) = invoker.invoke_answering(
        &mut door,
        &mut sheet,
        outbox,
        &json!({"title": "Elsewhere"}),
        Some(AppActionConsent::Deny),
    );
    let seen = seen.expect("the same action on another resource asks again");
    assert_eq!(seen.app_name(), "Outbox · Write draft");
    assert!(not_permitted(&status), "{status:?}");
    door.serve_briefly();
    assert_eq!(door.app.served.len(), 1, "the outbox's draft never ran");
}

#[test]
fn always_allowing_records_a_grant_that_asks_no_more_until_revoked() {
    let (mut door, mut sheet) = door_asking(true);
    let mut invoker = AppConnection::connect(&door, INVOKER_APP, INVOKER_HANDLE);
    let draft = fixture_draft(&door);

    let (status, seen) = invoker.invoke_answering(
        &mut door,
        &mut sheet,
        draft.clone(),
        &json!({"title": "First"}),
        Some(AppActionConsent::Always),
    );
    assert!(seen.is_some());
    if let ReceiptStatus::Rejected { error } = status {
        panic!("always allowed, the call goes ahead: {error:?}");
    }
    let grants = invoker_draft_grants(&door);
    assert_eq!(grants.len(), 1, "one grant is recorded");
    assert_eq!(
        grants[0].lifetime,
        tilo_action_policy::Lifetime::UntilRevoked
    );
    assert_eq!(grants[0].answer, Answer::Allow);
    assert_eq!(
        grants[0].granted_by,
        tilo_action_policy::Subject::person(owner_principal()),
        "granted by the person who answered"
    );

    let (status, seen) = invoker.invoke_answering(
        &mut door,
        &mut sheet,
        draft.clone(),
        &json!({"title": "Second"}),
        None,
    );
    assert!(seen.is_none());
    if let ReceiptStatus::Rejected { error } = status {
        panic!("a granted app is not asked again: {error:?}");
    }
    served_until(&mut door, 2);

    // Agents and scripts take it back through the workspace's own action,
    // with the confirmation any permission change needs.
    let revoke = |confirmation| {
        answer(
            door.host.invoke(
                &door.owner,
                InvokeRequest {
                    on_behalf_of: None,
                    invocation: InvocationId::new(),
                    target: RemoteActionTarget {
                        provider: workspace_provider(&door.room.to_string()),
                        address: ActionAddress::new(
                            format!("workspace.permissions:{}", door.room),
                            tilo_desktop::workspace_settings::APP_ACTION_REVOKE_ACTION_ID,
                        ),
                    },
                    argument: InvokeArgument::Json(
                        json!({
                            "app": INVOKER_APP,
                            "target_app": FIXTURE,
                            "resource": INBOX,
                            "action": "draft",
                        })
                        .as_object()
                        .cloned()
                        .expect("an object"),
                    ),
                    context: None,
                    confirmation,
                    revision: None,
                    wait_ms: None,
                    appearance: None,
                },
            ),
        )
        .map(|receipt| receipt.status)
    };
    // Governance checks the confirmation itself: an unconfirmed revoke is
    // refused there, or before admission, and changes nothing.
    match revoke(None) {
        Ok(ReceiptStatus::Rejected { .. } | ReceiptStatus::Failed { .. }) | Err(_) => {}
        other => panic!("revoking needs confirmation: {other:?}"),
    }
    assert_eq!(invoker_draft_grants(&door).len(), 1);
    match revoke(Some(tilo_actions::remote::Confirmation::Confirmed)) {
        Ok(ReceiptStatus::Rejected { error }) | Err(error) => {
            panic!("the owner revokes: {error:?}")
        }
        Ok(_) => {}
    }
    assert!(invoker_draft_grants(&door).is_empty(), "the grant is gone");

    let (status, seen) = invoker.invoke_answering(
        &mut door,
        &mut sheet,
        draft,
        &json!({"title": "Third"}),
        Some(AppActionConsent::Deny),
    );
    assert!(seen.is_some(), "revoked, the app is asked again");
    assert!(not_permitted(&status), "{status:?}");
}

#[test]
fn denying_refuses_and_records_nothing() {
    let (mut door, mut sheet) = door_asking(true);
    let mut invoker = AppConnection::connect(&door, INVOKER_APP, INVOKER_HANDLE);
    let draft = fixture_draft(&door);
    let (status, seen) = invoker.invoke_answering(
        &mut door,
        &mut sheet,
        draft,
        &json!({"title": "No"}),
        Some(AppActionConsent::Deny),
    );
    assert!(seen.is_some());
    assert!(not_permitted(&status), "{status:?}");
    door.serve_briefly();
    assert!(door.app.served.is_empty(), "nothing ran");
    assert!(invoker_draft_grants(&door).is_empty(), "nothing is granted");
}

#[test]
fn with_nobody_present_the_call_is_refused_without_asking() {
    let (mut door, mut sheet) = door_asking(false);
    let mut invoker = AppConnection::connect(&door, INVOKER_APP, INVOKER_HANDLE);
    let draft = fixture_draft(&door);
    let (status, seen) = invoker.invoke_answering(
        &mut door,
        &mut sheet,
        draft,
        &json!({"title": "Unattended"}),
        None,
    );
    assert!(seen.is_none(), "no sheet is shown");
    assert!(not_permitted(&status), "{status:?}");
    door.serve_briefly();
    assert!(door.app.served.is_empty(), "nothing ran");
    assert!(invoker_draft_grants(&door).is_empty());
}

/// A compositor session sharing the door's workspaces and broker, and the
/// person it acts for.
fn session_host(door: &Door) -> crate::shell::overlay_host::ShellOverlayHost {
    let (mut host, _chrome) =
        crate::shell::build_tilo_overlay_host_with_handle(door.membership.clone(), false);
    host.set_shortcut_config(settings_store::hardware::ShortcutConfig::default())
        .expect("the built-in shortcut table is valid");
    host.attach_app_action_broker(door.broker.app_action_broker().expect("the action broker"));
    host
}

/// The inbox's `archive`, named exactly: an advance consent too.
fn archive_grant() -> Rule {
    action_rule(
        FIXTURE,
        INBOX_KIND,
        Some(INBOX),
        "archive",
        KindSet::READ_UNDOABLE,
        Answer::Allow,
    )
}

/// Start `shortcut` from the palette's path, as the session's person.
fn run_interactively(door: &Door, shortcut: &str) -> u64 {
    door.broker
        .app_action_broker()
        .expect("the action broker")
        .start_interactive_shortcut_run(
            &Caller::local_person(door.session_person()).in_room(door.room.to_string()),
            &door.room.to_string(),
            shortcut,
            Map::new(),
        )
        .expect("the run starts")
}

/// Serve the fixture until the shell shows a sheet; what it shows.
fn await_sheet(
    door: &mut Door,
    host: &mut crate::shell::overlay_host::ShellOverlayHost,
) -> crate::shell::portal_dialog::DialogView {
    loop {
        assert!(Instant::now() < door.deadline, "no sheet was shown");
        door.app.pump();
        host.pump_shortcut_step_prompts();
        if let Some(view) = host.sheet_view() {
            return view;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn type_text(host: &mut crate::shell::overlay_host::ShellOverlayHost, text: &str) {
    for character in text.chars() {
        press(
            host,
            winit::keyboard::Key::Character(character.to_string().into()),
            winit::keyboard::ModifiersState::empty(),
        );
    }
}

fn press_named(
    host: &mut crate::shell::overlay_host::ShellOverlayHost,
    key: winit::keyboard::NamedKey,
) {
    press(
        host,
        winit::keyboard::Key::Named(key),
        winit::keyboard::ModifiersState::empty(),
    );
}

#[test]
fn a_confirmation_step_is_approved_in_the_sheet_and_the_run_continues() {
    let mut door = Door::open();
    let mut tidy = triggered_draft("tidy", Vec::new());
    tidy.steps = vec![invoke_step("archive", "archive", &[])];
    tidy.output = None;
    tidy.grant.push(archive_grant());
    door.save_as(&Caller::local_person(door.session_person()), &tidy)
        .expect("saved");
    let mut host = session_host(&door);

    let job = run_interactively(&door, "tidy");
    let sheet = await_sheet(&mut door, &mut host);
    assert_eq!(sheet.title(), "Archive inbox?");
    assert!(
        sheet.detail().contains("Inbox"),
        "the sheet names what is acted on: {}",
        sheet.detail()
    );
    assert!(door.app.served.is_empty(), "nothing runs before the answer");
    press_named(&mut host, winit::keyboard::NamedKey::Enter);
    assert!(matches!(
        door.job_to_end(job),
        InvocationUpdate::Succeeded { .. }
    ));
    let served = served_actions(&door);
    drop(door);
    let sheet_gone = host.sheet_view().is_none();
    drop(host);
    assert_eq!(served, ["archive"]);
    assert!(sheet_gone, "the sheet is gone");
}

#[test]
fn a_question_answered_in_the_sheet_is_bound_and_a_secret_stays_hidden() {
    let mut door = Door::open();
    let mut ask = triggered_draft("ask", Vec::new());
    ask.steps = vec![
        Step::new(
            "title",
            StepKind::Ask {
                param: ActionParam::required("title", ParamType::Text, "Title of the draft"),
            },
        ),
        Step::new(
            "token",
            StepKind::Ask {
                param: ActionParam::required("token", ParamType::Secret, "Publishing token"),
            },
        ),
        invoke_step("draft", "draft", &[("title", Binding::step("title", ""))]),
    ];
    ask.output = None;
    door.save_as(&Caller::local_person(door.session_person()), &ask)
        .expect("saved");
    let mut host = session_host(&door);

    let job = run_interactively(&door, "ask");
    assert_eq!(
        await_sheet(&mut door, &mut host).title(),
        "Title of the draft"
    );
    type_text(&mut host, "notes");
    press_named(&mut host, winit::keyboard::NamedKey::Enter);

    assert_eq!(
        await_sheet(&mut door, &mut host).title(),
        "Publishing token"
    );
    type_text(&mut host, "s3");
    let field = host.sheet_field().expect("the answer field");
    assert!(
        field.is_secret() && !field.is_revealed(),
        "a secret answer is hidden on screen"
    );
    assert_eq!(field.value(), "s3");
    press_named(&mut host, winit::keyboard::NamedKey::Enter);
    drop(host);

    assert!(matches!(
        door.job_to_end(job),
        InvocationUpdate::Succeeded { .. }
    ));
    assert_eq!(
        door.app.served,
        [(
            "draft".to_string(),
            json!({"title": "notes"}).as_object().unwrap().clone()
        )],
        "the answer is bound into the next step"
    );
    let finished = door.finished_runs();
    drop(door);
    let report = serde_json::to_value(&finished[0].report).unwrap();
    let recorded = report.to_string();
    assert!(
        !recorded.contains("s3") && recorded.contains("[redacted]"),
        "the secret is redacted from the report: {recorded}"
    );
}

#[test]
fn cancelling_the_sheet_stops_the_run_with_nothing_applied() {
    let mut door = Door::open();
    let mut tidy = triggered_draft("tidy-then-draft", Vec::new());
    tidy.steps.insert(0, invoke_step("archive", "archive", &[]));
    tidy.grant.push(archive_grant());
    door.save_as(&Caller::local_person(door.session_person()), &tidy)
        .expect("saved");
    let mut host = session_host(&door);

    let job = run_interactively(&door, "tidy-then-draft");
    await_sheet(&mut door, &mut host);
    press_named(&mut host, winit::keyboard::NamedKey::Escape);
    match door.job_to_end(job) {
        InvocationUpdate::Failed { error, .. } => {
            assert!(error.message.contains("declined"), "{}", error.message);
        }
        other => panic!("a cancelled run stops: {other:?}"),
    }
    door.serve_briefly();
    let nothing_applied = door.app.served.is_empty();
    drop(door);
    let sheet_gone = host.sheet_view().is_none();
    drop(host);
    assert!(nothing_applied, "nothing was applied");
    assert!(sheet_gone);
}
