// Test helpers throughout this module pair `_sk`/`_pk` (secret/public
// key) and `owner`/`editor` role names — the clearest naming for this
// domain, and renaming would obscure the keypair relationship rather
// than clarify it.
#![allow(
    clippy::similar_names,
    reason = "sk/pk and owner/editor pairings are the conventional, intentional names for a keypair and a two-role fixture"
)]
// Only test builds and `test-support` fixture builds compile this module;
// clippy cannot see `cfg(any(test, ..))` as a test context by itself.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable,
    clippy::panic_in_result_fn,
    clippy::arithmetic_side_effects,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::wildcard_imports,
    clippy::used_underscore_binding,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::significant_drop_tightening,
    clippy::future_not_send,
    clippy::large_futures,
    clippy::await_holding_lock,
    clippy::await_holding_refcell_ref,
    reason = "test scenarios and fixtures: a failed unwrap is the assertion, and each scenario keeps its setup, guards and awaits in one readable body"
)]
use super::*;
include!("../certified_person_fixture.rs");
#[cfg(test)]
include!("../eager_bridge_tests.rs");
#[cfg(test)]
use crate::apps::test_support::WorkspaceTree;
use crate::apps::test_support::WorkspaceTreeHandle;
use std::cell::RefCell;
#[cfg(test)]
use std::os::unix::fs::PermissionsExt;
use std::rc::Rc;
#[cfg(test)]
use std::time::SystemTime;

#[cfg(test)]
#[cfg(test)]
use crate::apps::collab_outbound::recording::{RecordingSink, Sent};
#[cfg(test)]
use crate::apps::test_support::Grants as _;
#[cfg(test)]
use collab_types::{Ed25519PublicKey, Role, sign_op};
#[cfg(test)]
use ed25519_dalek::Signer as _;
#[cfg(test)]
use ed25519_dalek::SigningKey;

/// Prepare an owned test directory for the production broker's socket check.
pub fn secure_socket_parent(parent: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
}

#[cfg(test)]
fn lifecycle_socket(directory: &tempfile::TempDir, name: &str) -> PathBuf {
    secure_socket_parent(directory.path()).expect("secure temporary broker directory");
    directory.path().join(format!("{name}.sock"))
}

#[cfg(test)]
struct LifecycleAuthority;

#[cfg(test)]
impl crate::apps::certificate_authority::CertificateAuthority for LifecycleAuthority {
    fn authorize(&self, _: uuid::Uuid, _: &tilo_user_identity::DeviceCert) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
struct LifecycleSink;

#[cfg(test)]
impl crate::apps::collab_outbound::PeerPublishSink for LifecycleSink {
    fn app_document(&self, _: String, _: String, _: String, _: collab_types::SignedOp) {}

    fn app_presence(&self, _: String, _: String, _: collab_types::PeerPresence) {}

    fn scrollback_request(&self, _: &str, _: ArtifactId, _: u64, _: u16) {}
}

#[cfg(test)]
struct HealthyOwner;

#[cfg(test)]
impl tilo_broker_core::workspace_commits::WorkspaceCommits for HealthyOwner {
    fn projects_workspace(&self, _: tilo_desktop_model::WorkspaceId) -> bool {
        false
    }

    fn health(&self) -> Result<(), String> {
        Ok(())
    }

    fn document(
        &self,
        _: String,
        _: String,
        _: String,
        _: Option<String>,
        _: collab_types::SignedOp,
        _: Option<String>,
    ) -> tilo_broker_core::workspace_commits::DocumentFuture {
        Box::pin(async { Err("unused lifecycle write".into()) })
    }

    fn effect(&self, _: String, _: Vec<u8>) -> tilo_broker_core::workspace_commits::EffectFuture {
        Box::pin(async { Err("unused lifecycle receipt".into()) })
    }
}

/// What tilo-launcher writes first on its collaboration connection: the
/// marker, then the framed handoff naming the app and workspace.
#[cfg(test)]
fn launcher_handoff_bytes(app_id: &str, workspace: &str) -> (Vec<u8>, Vec<u8>) {
    let handoff = serde_json::to_vec(&serde_json::json!({
        "protocol": launcher::services::COLLAB_LAUNCH_PROTOCOL_VERSION,
        "app_id": app_id,
        "workspace_id": workspace,
    }))
    .expect("handoff json");
    (
        launcher::services::COLLAB_LAUNCH_MAGIC.to_vec(),
        launcher::services::COLLAB_LAUNCH_FRAMING
            .encode(&handoff)
            .expect("framed handoff"),
    )
}

/// Records launches as a host does, before it releases the launcher.
#[cfg(test)]
struct RecordedLaunches(Mutex<Vec<(u32, String, String)>>);
#[cfg(test)]
impl crate::apps::services::LauncherServices for RecordedLaunches {
    fn applications(&self) -> Vec<crate::apps::services::LaunchableApplication> {
        Vec::new()
    }
    fn launch(
        &self,
        _: &str,
        _: &tilo_desktop_model::Workspace,
        _: Option<&str>,
        _: &crate::apps::services::LaunchPlace,
    ) -> Result<(), String> {
        Err("this broker launches nothing".into())
    }
    fn attest_process(&self, pid: u32, app: &str, workspace: &str) -> bool {
        self.0
            .lock()
            .expect("records")
            .iter()
            .any(|(recorded, a, r)| *recorded == pid && a == app && r == workspace)
    }
    /// Every recorded launch was started by the `open` holding ticket 31.
    fn authenticated_launch(
        &self,
        pid: u32,
        app: &str,
        workspace: &str,
        _window_instance: Option<u64>,
    ) -> Result<Option<crate::apps::services::AuthenticatedApplicationLaunch>, String> {
        Ok(self.attest_process(pid, app, workspace).then_some(
            crate::apps::services::AuthenticatedApplicationLaunch {
                continuation_instance: None,
                launch_ticket: Some(31),
                windowless: false,
                opened_for: None,
            },
        ))
    }
}

#[cfg(test)]
struct DropBrokerOnHealth {
    broker: Arc<Mutex<Option<crate::apps::collab_broker::BrokerHandle>>>,
}

#[cfg(test)]
impl tilo_broker_core::workspace_commits::WorkspaceCommits for DropBrokerOnHealth {
    fn projects_workspace(&self, _: tilo_desktop_model::WorkspaceId) -> bool {
        false
    }

    fn document(
        &self,
        _: String,
        _: String,
        _: String,
        _: Option<String>,
        _: collab_types::SignedOp,
        _: Option<String>,
    ) -> tilo_broker_core::workspace_commits::DocumentFuture {
        Box::pin(async { Err("unused lifecycle write".into()) })
    }

    fn effect(&self, _: String, _: Vec<u8>) -> tilo_broker_core::workspace_commits::EffectFuture {
        Box::pin(async { Err("unused lifecycle receipt".into()) })
    }

    fn health(&self) -> Result<(), String> {
        drop(
            self.broker
                .lock()
                .map_err(|_| "broker holder unavailable")?
                .take(),
        );
        Ok(())
    }
}

#[cfg(test)]
struct DropOtherBrokerOnHealth {
    broker: Arc<Mutex<Option<crate::apps::collab_broker::BrokerHandle>>>,
    socket: PathBuf,
    journal_root: PathBuf,
    proof: Arc<Mutex<Option<Result<(), String>>>>,
}

#[cfg(test)]
impl tilo_broker_core::workspace_commits::WorkspaceCommits for DropOtherBrokerOnHealth {
    fn projects_workspace(&self, _: tilo_desktop_model::WorkspaceId) -> bool {
        false
    }

    fn document(
        &self,
        _: String,
        _: String,
        _: String,
        _: Option<String>,
        _: collab_types::SignedOp,
        _: Option<String>,
    ) -> tilo_broker_core::workspace_commits::DocumentFuture {
        Box::pin(async { Err("unused lifecycle write".into()) })
    }

    fn effect(&self, _: String, _: Vec<u8>) -> tilo_broker_core::workspace_commits::EffectFuture {
        Box::pin(async { Err("unused lifecycle receipt".into()) })
    }

    fn health(&self) -> Result<(), String> {
        drop(
            self.broker
                .lock()
                .map_err(|_| "other broker holder unavailable")?
                .take(),
        );
        let result = if self.socket.exists() {
            Err("unrelated broker drop returned before socket cleanup".into())
        } else {
            InvocationJournal::open(&self.journal_root)
                .map(drop)
                .map_err(|error| format!("unrelated broker journal remained locked: {error}"))
        };
        *self
            .proof
            .lock()
            .map_err(|_| "other broker proof unavailable")? = Some(result);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectoryResolveMode {
    Found,
    Stale,
    NotFound,
}

/// A directory endpoint retained by the `LocalSet` tests below. It
/// answers only bounded inert rows, through the broker's real directory
/// registry rather than a mock of the shell bridge.
pub struct TestActionDirectory {
    revision: String,
    pub resolve_mode: DirectoryResolveMode,
    result: tilo_actions::ResultSpec,
    /// What the action is: opening Atlas, a view, unless a test makes it another.
    pub operation: tilo_actions::OperationKind,
    confirmation_required: bool,
    params: Vec<tilo_actions::ActionParam>,
    query_count: usize,
    resolve_count: usize,
}

impl TestActionDirectory {
    fn contract(&self) -> tilo_actions::ActionContract {
        let mut contract = tilo_actions::ActionContract::for_operation(self.operation);
        // Opening Atlas changes the invoking presence's view, not the
        // publishing presence's private UI.
        if self.operation == tilo_actions::OperationKind::View {
            contract.tags.push(tilo_actions::PRESENCE_VIEW_TAG.into());
        }
        contract.result = self.result.clone();
        if self.confirmation_required {
            contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
        }
        contract
    }

    fn row(&self) -> runtime_collab::ActionDirectoryRow {
        let contract = self.contract();
        let descriptor = runtime_collab::ExportedAction {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            resource: "workspace:atlas".into(),
            action: "open".into(),
            resource_kind: "workspace".into(),
            resource_label: "Atlas".into(),
            action_label: "Open Atlas".into(),
            keywords: Vec::new(),
            icon: None,
            description: Some("Open the Atlas workspace".into()),
            operation_kind: self.operation,
            params: self.params.clone(),
            contract: contract.clone(),
            input: runtime_collab::ExportedInput::None,
            view: None,
        };
        runtime_collab::ActionDirectoryRow {
            parent_resource: String::new(),
            resource: descriptor.resource.clone(),
            resource_kind: descriptor.resource_kind.clone(),
            resource_label: descriptor.resource_label.clone(),
            presentation: None,
            action: Some(descriptor.action.clone()),
            descriptor: Some(descriptor),
            has_children: false,
            visibility: Some(runtime_collab::RowVisibility {
                governing_kind: Some(self.operation),
                action: "open".into(),
                contract,
            }),
        }
    }
}

struct TestActionDirectoryServer {
    state: Rc<RefCell<TestActionDirectory>>,
}

impl wire::DirectorySource for TestActionDirectoryServer {
    fn query(
        &self,
        _query: runtime_collab::ActionDirectoryQuery,
        _max_bytes: u32,
    ) -> wire::Asked<runtime_collab::ActionDirectoryPage> {
        let mut state = self.state.borrow_mut();
        state.query_count = state.query_count.saturating_add(1);
        let page = runtime_collab::ActionDirectoryPage {
            revision: state.revision.clone(),
            rows: vec![state.row()],
            next_cursor: Vec::new(),
            exhausted: true,
            stale_cursor: false,
            budget_exhausted: false,
        };
        Box::pin(std::future::ready(Ok(page)))
    }

    fn resolve(
        &self,
        _target: (&str, &str, &str),
        local_document: bool,
        _max_bytes: u32,
    ) -> wire::Asked<runtime_collab::ActionDirectoryResolution> {
        if local_document {
            return Box::pin(std::future::ready(Err(
                "no local document snapshots here".into()
            )));
        }
        let mut state = self.state.borrow_mut();
        state.resolve_count = state.resolve_count.saturating_add(1);
        let resolution = match state.resolve_mode {
            // As a current owner does: the revision it resolved at.
            DirectoryResolveMode::Found => runtime_collab::ActionDirectoryResolution::Found {
                row: Box::new(state.row()),
                revision: Some(state.revision.clone()),
            },
            DirectoryResolveMode::Stale => runtime_collab::ActionDirectoryResolution::StaleRevision,
            DirectoryResolveMode::NotFound => runtime_collab::ActionDirectoryResolution::NotFound,
        };
        Box::pin(std::future::ready(Ok(resolution)))
    }
}

#[cfg(test)]
mod signed_worker {
    include!("../signed_worker_test_support.rs");
}
/// Start `action` on `resource` as `actor` on peer `peer`'s live surface,
/// through the broker's own dispatch, under `token`'s confirmation.
#[cfg(test)]
fn start_confirmed(
    state: &AppBrokerState,
    peer: u64,
    resource: &str,
    action: &str,
    argument: Option<&str>,
    actor: &str,
    token: Option<&str>,
) -> Result<InvocationJob, String> {
    state
        .start_invocation_confirmed_for_origin(
            peer,
            resource,
            action,
            InvocationInputs {
                original_request: None,
                requested: None,
                caller: None,
                execution: None,
                argument,
                actor: &BrokerActor::person(actor),
                confirmation_token: token,
                context: None,
            },
            None,
        )
        .map_err(|refused| refused.to_string())?
        .start_blocking(state)
}
#[cfg(test)]
fn start(
    state: &AppBrokerState,
    peer: u64,
    resource: &str,
    action: &str,
    argument: Option<&str>,
    actor: &str,
) -> Result<InvocationJob, String> {
    start_confirmed(state, peer, resource, action, argument, actor, None)
}
#[cfg(test)]
fn invoke(
    state: &AppBrokerState,
    peer: u64,
    resource: &str,
    action: &str,
    argument: Option<&str>,
    actor: &str,
) -> Result<std::sync::mpsc::Receiver<InvocationCompletion>, String> {
    start(state, peer, resource, action, argument, actor).map(|job| job.completion)
}
/// The one-use grant admission mints once `actor` confirmed `action`.
#[cfg(test)]
fn prepare(
    state: &AppBrokerState,
    peer: u64,
    resource: &str,
    action: &str,
    argument: Option<&str>,
    actor: &str,
) -> Result<String, String> {
    let offered = state.publishers.surfaces.lock().unwrap()[&peer]
        .actions
        .iter()
        .find(|offered| offered.resource == resource && offered.action == action)
        .cloned()
        .ok_or("the application no longer publishes that action")?;
    state.mint_confirmation_grant(peer, &offered, argument, &BrokerActor::person(actor))
}

#[cfg(test)]
#[cfg(test)]
include!("../resolved_invocation_tests.rs");
#[cfg(test)]
include!("../external_moderation_rpc_tests.rs");
#[cfg(test)]
include!("../approved_check_tests.rs");
#[cfg(test)]
include!("../canonical_route_tests.rs");
#[cfg(all(test, target_os = "linux"))]
include!("../native_checkout_tests.rs");
#[cfg(all(test, target_os = "linux"))]
include!("../workspace_document_ownership_tests.rs");
#[cfg(test)]
include!("../durable_local_invocation_matrix_tests.rs");
#[cfg(test)]
include!("../document_content_tests.rs");
#[cfg(test)]
include!("../offload_api_tests.rs");
#[cfg(test)]
include!("../hosted_workspace_tests.rs");

pub struct LocalDirectoryFixture {
    _fanout: Arc<FanoutState>,
    state: Arc<AppBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    directory: Rc<dyn wire::DirectorySource>,
    directory_state: Rc<RefCell<TestActionDirectory>>,
    pub broker: crate::apps::collab_broker::AppActionBroker,
    pub membership: Option<WorkspaceTreeHandle>,
    pub cluster_id: String,
    pub route: CrdtRoute,
    pub peer_id: u64,
    invocation_rx: tokio::sync::mpsc::UnboundedReceiver<InvocationEnvelope>,
    source_identity: Option<AdmittedJoinIdentity>,
}

impl LocalDirectoryFixture {
    pub fn query_count(&self) -> usize {
        self.directory_state.borrow().query_count
    }
    pub fn reconnect_directory(&self) {
        self.directories.directories.borrow_mut().insert(
            self.peer_id,
            RegisteredActionDirectory {
                registration: Rc::new(uuid::Uuid::new_v4()),
                pagination: directory_pagination::DirectoryPagination::default(),
                publishing_person: self.source_identity.as_ref().and_then(|source| {
                    source
                        .certificate
                        .as_ref()
                        .map(|certificate| person_id_for_root(certificate.user_public_key))
                }),
                app_id: self.route.app_id.clone(),
                route: self.route.clone(),
                directory: Rc::clone(&self.directory),
            },
        );
    }

    pub fn set_confirmation_required(&self, required: bool) {
        self.directory_state.borrow_mut().confirmation_required = required;
    }

    fn invocation_channel(&self) -> runtime_collab::ChannelHandle {
        self.invocation_channel_for(self.peer_id, &self.route)
    }

    fn invocation_channel_for(
        &self,
        peer_id: u64,
        route: &CrdtRoute,
    ) -> runtime_collab::ChannelHandle {
        wire_channel(JoinedChannel {
            committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
            state: Arc::clone(&self.state),
            directories: Rc::clone(&self.directories),
            channel_id: route.channel_key(),
            peer_id,
            connection_id: peer_id,
            verified_identity: self.source_identity.clone().unwrap_or_else(|| {
                AdmittedJoinIdentity {
                    worker_registration: None,
                    certificate: None,
                    verified: VerifiedJoinIdentity {
                        user_public_key: [0; 32],
                        device_public_key: [0; 32],
                        capped: false,
                    },
                    actor_key: "fixture-application".into(),
                    display_handle: None,
                    role: CapRole::Editor,
                    agent_id: None,
                    session_delegated: false,
                }
            }),
            resource_type: None,
            route_access: RouteAccess::Own,
            lease: None,
            _hold: None,
        })
    }

    /// Exercise the production peer-bound update RPC, including result-contract validation.
    pub async fn complete_invocation(
        &self,
        id: u64,
        update: &tilo_actions::InvocationUpdate,
    ) -> Result<(), runtime_portal::NotAvailable> {
        self.invocation_channel()
            .update_invocation(
                id,
                &serde_json::to_string(update).expect("canonical update"),
            )
            .await
    }

    pub fn take_invocation_identity(&mut self) -> Option<(u64, tilo_actions::InvocationId)> {
        self.invocation_rx
            .try_recv()
            .ok()
            .map(|invocation| (invocation.id, invocation.invocation))
    }

    /// The invocation the broker delivered to this app's connection, whole.
    pub fn take_invocation(&mut self) -> Option<InvocationEnvelope> {
        self.invocation_rx.try_recv().ok()
    }

    /// Drive `pending` until the broker answers it, taking the broker's
    /// turn each pass: a journal write resolves on its own writer thread
    /// and hands its answer to the turn, which nothing else takes here.
    pub async fn drive<T>(&self, mut pending: tilo_actions::remote::Pending<T>) -> T {
        tilo_test_env::within("the broker answers", async {
            loop {
                drain_directory_bridge(self).await;
                if let tilo_actions::remote::PendingPoll::Ready(value) = pending.poll_ready() {
                    break value;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
    }

    pub fn resolve_count(&self) -> usize {
        self.directory_state.borrow().resolve_count
    }
}

#[cfg(test)]
async fn approval_publish(
    fixture: &LocalDirectoryFixture,
    channel: &runtime_collab::ChannelHandle,
    directory: bool,
) {
    if directory {
        channel
            .publish_action_directory(wire_answerer(Rc::clone(&fixture.directory)))
            .await
            .unwrap();
    } else {
        channel.publish_actions(&[]).await.unwrap();
    }
}

#[cfg(test)]
fn local_directory_fixture() -> LocalDirectoryFixture {
    let mut tree = crate::apps::test_support::WorkspaceTree::default();
    let mut workspace = crate::apps::test_support::Workspace::new(
        crate::apps::test_support::WorkspaceId::from_u128(71),
        "Directory test workspace",
    );
    workspace.policy.set_owner("fixture-owner".to_string());
    tree.workspaces.push(workspace);
    tree.set_active(crate::apps::test_support::WorkspaceId::from_u128(71));
    local_directory_fixture_with_membership(tree.shared())
}

pub fn local_directory_fixture_with_membership(
    membership: crate::apps::test_support::WorkspaceTreeHandle,
) -> LocalDirectoryFixture {
    local_directory_fixture_with_peer(membership, Some(71))
}

fn local_directory_fixture_with_peer(
    membership: crate::apps::test_support::WorkspaceTreeHandle,
    peer_id: Option<u64>,
) -> LocalDirectoryFixture {
    local_directory_fixture_with_devices(membership, peer_id, None)
}

fn local_directory_fixture_with_devices(
    membership: WorkspaceTreeHandle,
    peer_id: Option<u64>,
    trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
) -> LocalDirectoryFixture {
    let mut fixture = local_directory_fixture_with_authority(
        membership.authority(),
        membership.services(),
        peer_id,
        trusted_devices,
    );
    fixture.membership = Some(membership);
    fixture
}

pub fn local_directory_fixture_with_authority(
    membership: crate::apps::services::WorkspaceAuthorityHandle,
    services: ProcessServices,
    peer_id: Option<u64>,
    trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
) -> LocalDirectoryFixture {
    let cluster_id = membership
        .snapshot()
        .unwrap()
        .active_workspace()
        .expect("fixture active workspace")
        .id()
        .to_string();
    let route = CrdtRoute::record("so.tilo.atlas", cluster_id.clone(), "atlas");
    let fanout = Arc::new(FanoutState::new());
    let (state, bridge_rx) = AppBrokerState::new_configured(
        &fanout,
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(membership.clone()),
        trusted_devices,
        None,
        services,
    );
    let peer_id = peer_id.unwrap_or_else(|| state.next_peer_id());
    let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge_rx));
    let directory_state = Rc::new(RefCell::new(TestActionDirectory {
        revision: "directory-r1".into(),
        resolve_mode: DirectoryResolveMode::Found,
        result: tilo_actions::ResultSpec::None,
        operation: tilo_actions::OperationKind::View,
        confirmation_required: false,
        params: Vec::new(),
        query_count: 0,
        resolve_count: 0,
    }));
    let directory: Rc<dyn wire::DirectorySource> = Rc::new(TestActionDirectoryServer {
        state: Rc::clone(&directory_state),
    });
    directories.directories.borrow_mut().insert(
        peer_id,
        RegisteredActionDirectory {
            registration: Rc::new(uuid::Uuid::new_v4()),
            pagination: directory_pagination::DirectoryPagination::default(),
            publishing_person: None,
            app_id: route.app_id.clone(),
            route: route.clone(),
            directory: directory.clone(),
        },
    );
    state
        .action_directory_roots
        .lock()
        .expect("directory roots")
        .insert(
            peer_id,
            ActionDirectoryRoot {
                peer_id,
                app_id: route.app_id.clone(),
                cluster_id: cluster_id.clone(),
                document_id: route.document_id.clone(),
            },
        );
    state.refresh_action_directory_root_snapshot();
    let invocation_rx = state.capture_invocations_for_test(peer_id);
    fanout.attach_app_broker(Arc::clone(&state));
    LocalDirectoryFixture {
        _fanout: fanout,
        broker: crate::apps::collab_broker::AppActionBroker::from_state_for_test(Arc::clone(
            &state,
        )),
        membership: None,
        state,
        directories,
        directory,
        directory_state,
        cluster_id,
        route,
        peer_id,
        invocation_rx,
        source_identity: None,
    }
}

#[allow(
    clippy::future_not_send,
    reason = "the directory bridge is deliberately driven on the LocalSet because its state contains Rc values"
)]
pub async fn drain_directory_bridge(fixture: &LocalDirectoryFixture) {
    fixture.state.take_turn(&fixture.directories);
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
}

/// A directory whose container row carries a visibility contract the
/// broker cannot read, beside a well-formed action row.
#[cfg(test)]
struct UndecodableVisibilityDirectory {
    row: runtime_collab::ActionDirectoryRow,
}

#[cfg(test)]
impl wire::DirectorySource for UndecodableVisibilityDirectory {
    fn query(
        &self,
        _query: runtime_collab::ActionDirectoryQuery,
        max_bytes: u32,
    ) -> wire::Asked<runtime_collab::ActionDirectoryPage> {
        let container = runtime_collab::ActionDirectoryRow {
            parent_resource: String::new(),
            resource: "workspace:folder".into(),
            resource_kind: "workspace".into(),
            resource_label: "Folder".into(),
            presentation: None,
            action: None,
            descriptor: None,
            has_children: true,
            visibility: Some(runtime_collab::RowVisibility {
                governing_kind: Some(tilo_actions::OperationKind::View),
                action: "open".into(),
                contract: tilo_actions::ActionContract::view(),
            }),
        };
        let page = runtime_collab::ActionDirectoryPage {
            revision: "directory-r1".into(),
            rows: vec![self.row.clone(), container],
            next_cursor: Vec::new(),
            exhausted: true,
            stale_cursor: false,
            budget_exhausted: false,
        };
        // A permission the broker does not know: the check is unreadable.
        let mut json = serde_json::to_value(&page).expect("test directory page encodes");
        json["rows"][1]["visibility"]["contract"] = serde_json::json!({"kind": "everyone"});
        Box::pin(std::future::ready(runtime_collab::read_directory_answer(
            &json.to_string(),
            u64::from(max_bytes),
            |page: &runtime_collab::ActionDirectoryPage| page.rows.iter().collect(),
        )))
    }

    fn resolve(
        &self,
        _target: (&str, &str, &str),
        _local_document: bool,
        _max_bytes: u32,
    ) -> wire::Asked<runtime_collab::ActionDirectoryResolution> {
        Box::pin(std::future::ready(Err("not resolved here".into())))
    }
}

#[cfg(test)]
pub(super) fn certified_join(
    workspace: &str,
    root: &SigningKey,
    device: &SigningKey,
) -> runtime_collab::CertifiedJoinIdentity {
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Test device".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).expect("root signs device cert");
    runtime_collab::CertifiedJoinIdentity {
        workspace_id: workspace.into(),
        device_certificate: serde_json::to_vec(&cert).expect("certificate JSON"),
        proof: device
            .sign(&tilo_identity_client::collaboration_join_digest(workspace).unwrap())
            .to_bytes()
            .to_vec(),
    }
}

#[cfg(test)]
mod member_join;

#[cfg(test)]
fn verified_device(root: &SigningKey) -> VerifiedJoinIdentity {
    VerifiedJoinIdentity {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: [42; 32],
        capped: false,
    }
}

#[cfg(test)]
fn downgrade_member_to_viewer(membership: &WorkspaceTreeHandle, actor: &str, _workspace: &str) {
    let mut tree = membership.write();
    let workspace = &mut tree.workspaces[0];
    workspace.policy.owner = None;
    workspace.policy.add_member(actor, Role::Viewer);
    drop(tree);
}

/// Governs every workspace for one device; admits every certificate.
#[cfg(test)]
struct GovernedBy([u8; 32]);

#[cfg(test)]
impl crate::apps::certificate_authority::CertificateAuthority for GovernedBy {
    fn authorize(
        &self,
        _workspace: uuid::Uuid,
        _certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        Ok(())
    }
    fn governs(&self, _workspace: uuid::Uuid, device: Option<&[u8; 32]>) -> bool {
        device == Some(&self.0)
    }
}

#[cfg(test)]
fn assert_replay_and_forgery_are_audited(
    state: &Arc<AppBrokerState>,
    broker_channels: &Arc<Mutex<BrokerChannels>>,
    route: &CrdtRoute,
    admitted: &AdmittedJoinIdentity,
    op: &SignedOp,
) {
    let artifact = op.artifact;
    let accepted_before = broker_channels
        .lock()
        .expect("broker channels")
        .state_for(artifact)
        .scrollback_for(artifact, u64::MAX, 10)
        .0;
    handle_signed_op_send(state, 1, route, admitted, op.clone());
    let mut forged = op.clone();
    forged.seq = 2;
    forged.payload = DocumentMutation::Incremental(b"tampered".to_vec())
        .encode()
        .expect("typed incremental");
    handle_signed_op_send(state, 1, route, admitted, forged);

    let audit = state
        .broker_channels
        .lock()
        .expect("broker channels")
        .audit
        .for_artifact(artifact);
    assert_eq!(audit.len(), 2, "replay and forged op are both audited");
    assert!(
        audit
            .iter()
            .any(|entry| matches!(entry.rejection, Some(ValidationError::ReplaySeq { .. })))
    );
    assert!(
        audit
            .iter()
            .any(|entry| matches!(entry.rejection, Some(ValidationError::BadOpSignature(_))))
    );
    assert_eq!(
        broker_channels
            .lock()
            .expect("broker channels")
            .state_for(artifact)
            .scrollback_for(artifact, u64::MAX, 10)
            .0,
        accepted_before,
        "replay and forgery must not change the accepted scrollback",
    );
}

#[cfg(test)]
fn assert_published_snapshot(sink: &RecordingSink, route: &CrdtRoute) {
    let published = sink.snapshot();
    let [
        Sent::Signed {
            app_id,
            cluster_id,
            document_id,
            payload,
        },
    ] = published.as_slice()
    else {
        panic!("the certified document was not published to the peer network");
    };
    assert_eq!(app_id, "so.tilo.notes");
    assert_eq!(cluster_id, &route.cluster_id);
    assert_eq!(document_id, "meeting-notes");
    assert_eq!(
        DocumentMutation::decode(payload).expect("typed network payload"),
        DocumentMutation::Snapshot(b"automerge snapshot".to_vec()),
    );
}

#[cfg(test)]
#[derive(Clone)]
struct CountingCatalogueAuthority {
    tree: Arc<Mutex<tilo_desktop_model::Layout>>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
    failed: Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(test)]
impl crate::apps::services::WorkspaceAuthority for CountingCatalogueAuthority {
    /// Every sample (`with_tree`, and `with_workspace`/`snapshot` through it)
    /// counts once.
    fn with_tree(&self, read: &mut dyn FnMut(&tilo_desktop_model::Layout)) -> Result<(), String> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if self.failed.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("catalogue authority unavailable".into());
        }
        let tree = self
            .tree
            .lock()
            .map_err(|_| "catalogue authority unavailable".to_string())?;
        read(&tree);
        Ok(())
    }
}

#[cfg(test)]
pub(super) fn catalogue_surface(peer_id: u64, cluster: &str, resource: &str) -> PublishedSurface {
    PublishedSurface {
        peer_id,
        app_id: "so.tilo.files".into(),
        route: CrdtRoute::record("so.tilo.files", cluster, format!("record-{peer_id}")),
        actions: vec![PublishedAction {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            resource: resource.into(),
            action: "open".into(),
            resource_kind: "files.entry".into(),
            resource_label: resource.into(),
            action_label: "Open".into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            operation_kind: tilo_actions::OperationKind::View,
            params: Vec::new(),
            contract: tilo_actions::ActionContract {
                kind: tilo_actions::ActionKind::Undoable,
                ..Default::default()
            },
            view: None,
            input: PublishedInput::None,
        }],
        live: true,
    }
}

#[cfg(test)]
struct CountingCatalogueFixture {
    state: Arc<AppBrokerState>,
    tree: Arc<Mutex<tilo_desktop_model::Layout>>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
    failed: Arc<std::sync::atomic::AtomicBool>,
    cluster: String,
}

#[cfg(test)]
fn counting_catalogue_state(owner: &str) -> CountingCatalogueFixture {
    let mut workspace = crate::apps::test_support::Workspace::new(
        crate::apps::test_support::WorkspaceId::from_u128(29),
        "Catalogue workspace",
    );
    workspace.policy.set_owner(owner);
    let cluster = workspace.id().to_string();
    let mut tree = tilo_desktop_model::Layout::default();
    tree.workspaces.push(workspace);
    let tree = Arc::new(Mutex::new(tree));
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fanout = Arc::new(FanoutState::new());
    let (state, _bridge_rx) = AppBrokerState::new_configured(
        &fanout,
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(Arc::new(CountingCatalogueAuthority {
            tree: tree.clone(),
            calls: calls.clone(),
            failed: failed.clone(),
        })),
        None,
        None,
        ProcessServices::default(),
    );
    CountingCatalogueFixture {
        state,
        tree,
        calls,
        failed,
        cluster,
    }
}

#[cfg(test)]
fn catalogue_test_follower() -> Arc<CatalogueFollower> {
    CatalogueFollower::new(&Vec::new())
}

#[cfg(test)]
/// Apply the change `follower` is sent next, if one is queued.
fn read_catalogue(
    follower: &CatalogueFollower,
    held: &mut runtime_collab::ActionCatalogue,
) -> bool {
    let Some(change) = follower.following.lock().unwrap().pending.take() else {
        return false;
    };
    held.apply(&change).unwrap();
    true
}

#[cfg(test)]
fn offers(held: &runtime_collab::ActionCatalogue, resource: &str) -> bool {
    held.surfaces()
        .iter()
        .flat_map(|surface| &surface.actions)
        .any(|action| action.resource == resource)
}

#[cfg(test)]
mod durable_admission_tests;

#[cfg(test)]
mod authority;
#[cfg(test)]
mod consent;
#[cfg(test)]
mod directory;
#[cfg(test)]
mod lifecycle;
#[cfg(test)]
mod made_from;
#[cfg(test)]
mod routing;
