//! What the host runs for a member who is not its own person, and for that
//! member's agent, decided by the member's access now: a viewer changes
//! nothing, a commenter comments and nothing else, an editor edits but is
//! asked first where the action asks, and a member removed or a device
//! revoked is refused whatever it still holds. The workspace's policy is
//! the fixture's to set; everything else is the host's own service.
use super::*;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tilo_actions::remote::{Caller, ContentPutAnswer, ContentPutStep};
use tilo_agent_session::workspace_actions::{HostDevice, ViewReadPolicy, WorkspaceDocumentsLink};
use tilo_server_protocol::WorkspaceId;
use tilo_session_client::Outbox;
use ui::permissions::Role;

const TASKS: &str = "so.tilo.tasks";

/// The workspace's word on one member: their role, whether their device is
/// revoked, and what they answer when the host asks them.
pub(super) struct Membership {
    member: HostDevice,
    /// The host's workspaces, which hand the service the keys its device holds.
    workspaces: tilo_server::workspaces::Workspaces,
    person: [u8; 32],
    device: [u8; 32],
    role: Mutex<Option<Role>>,
    revoked: AtomicBool,
    /// Whether the next ask for the keys of the workspace's agents is answered as the device's
    /// key service answers while it takes part and holds no keys yet.
    keys_not_yet: AtomicBool,
    content: tilo_content::ContentStore,
    /// Set by [`Self::hold`]: whether the next read of a member's access waits,
    /// and whether it began. It takes its time once it has, as a request that
    /// keeps the workspace's thread busy does.
    slow: Mutex<(bool, bool)>,
    /// Wakes the test when the read [`Self::hold`] armed begins.
    began: std::sync::Condvar,
}

impl Membership {
    /// The workspace's word on `member`, a viewer until it says otherwise, as the service of
    /// `server`'s device reads it.
    pub(super) fn of(
        hosted: &Hosted,
        server: &Device,
        member: &Device,
        content: std::path::PathBuf,
    ) -> Arc<Self> {
        Arc::new(Self {
            member: HostDevice {
                endpoint: tilo_agent_session::ServerEndpoint::iroh(
                    hosted.server.node_addr(),
                    server.cert.clone(),
                ),
                key: server.key.clone(),
                runtime: tokio::runtime::Handle::current(),
                keys: None,
            },
            workspaces: hosted.server.workspaces().unwrap().clone(),
            person: member.cert.user_public_key,
            device: member.cert.device_public_key,
            role: Mutex::new(Some(Role::Viewer)),
            revoked: AtomicBool::new(false),
            keys_not_yet: AtomicBool::new(false),
            content: tilo_content::ContentStore::in_dir(content),
            slow: Mutex::new((false, false)),
            began: std::sync::Condvar::new(),
        })
    }

    /// The member's role from now on; none once the workspace removed them.
    pub(super) fn set_role(&self, role: Option<Role>) {
        *self.role.lock().unwrap() = role;
    }

    /// Answer the next ask for the keys of the workspace's agents as the device's key service
    /// does while it takes part and holds no keys yet.
    fn keys_not_yet_once(&self) {
        self.keys_not_yet.store(true, Ordering::SeqCst);
    }

    /// The member's role from now on, in the workspace's governance too:
    /// what every device deciding their changes reads, given by the
    /// workspace owner's server device.
    pub(super) async fn becomes(&self, role: Role) {
        *self.role.lock().unwrap() = Some(role);
        self.workspaces
            .set_member_role(
                WORKSPACE,
                &self.member.endpoint.server_cert,
                tilo_server::workspaces::SetMemberRoleRequest {
                    request_id: *blake3::hash(
                        format!("{role:?} {}", tilo_clock::unix_now_ns()).as_bytes(),
                    )
                    .as_bytes(),
                    user_public_key: self.person,
                    role,
                },
            )
            .await
            .unwrap();
    }

    /// Hold the workspace's thread from the next read of a member's access
    /// ([`Self::held`]) for [`HELD`], waking [`Self::began`] when it starts.
    fn hold(&self) {
        *self.slow.lock().unwrap() = (true, false);
    }

    /// The one read of the member's own access [`Self::hold`] armed waits,
    /// and says it began.
    fn held(&self, caller: &Caller) {
        if caller.user_public_key() != Some(&self.person) {
            return;
        }
        let mut slow = self.slow.lock().unwrap();
        if !slow.0 {
            return;
        }
        *slow = (false, true);
        self.began.notify_all();
        drop(slow);
        std::thread::sleep(HELD);
    }

    fn access(&self, caller: &Caller) -> Option<ui::permissions::MemberAccess> {
        let role = (*self.role.lock().unwrap())?;
        (!self.revoked.load(Ordering::SeqCst)
            && caller.user_public_key() == Some(&self.person)
            && caller.device_public_key() == Some(&self.device)
            && caller.acting().is_person())
        .then(|| {
            ui::permissions::PermissionContext::member_of(
                WORKSPACE,
                &ui::permissions::MemberId::person(hex::encode(self.person)),
                role,
            )
            .access()
            .clone()
        })
    }
}

impl WorkspaceDocumentsLink for Membership {
    fn documents(&self) -> Option<Arc<dyn tilo_server::workspace_documents::DeviceDocuments>> {
        Some(Arc::new(
            tilo_server::device_documents::ServiceDocuments::of(self.workspaces.clone()),
        ))
    }

    fn writes(&self) -> Option<Arc<dyn tilo_server::workspace_documents::DeviceWrites>> {
        Some(Arc::new(tilo_server::device_documents::ServiceWrites::of(
            self.workspaces.clone(),
        )))
    }

    fn user_public_key(&self) -> [u8; 32] {
        self.member.endpoint.server_cert.user_public_key
    }
    fn own_device(&self) -> HostDevice {
        self.member.clone()
    }
    fn admits(&self, _workspace: WorkspaceId, caller: &Caller) -> bool {
        self.access(caller).is_some()
    }
    fn view_read_policy(
        &self,
        _workspace: WorkspaceId,
        caller: &Caller,
        _app: &str,
        _record: &str,
    ) -> Option<ViewReadPolicy> {
        Some(ViewReadPolicy {
            workspace: WORKSPACE.into(),
            access: self.access(caller)?,
            sandbox: Vec::new(),
            unix_time: tilo_clock::unix_now_ns() / 1_000_000_000,
            agent_bound: Vec::new(),
        })
    }
    fn workspace_policy(&self, workspace: WorkspaceId, caller: &Caller) -> Option<ViewReadPolicy> {
        self.held(caller);
        self.view_read_policy(workspace, caller, "", "")
    }
    fn workspace_content(&self, _workspace: WorkspaceId) -> Option<tilo_content::ContentStore> {
        Some(self.content.clone())
    }
    fn serving_keys(
        &self,
        route: &DocumentRoute,
        create: bool,
    ) -> Option<Result<key_group::ContentKeys, String>> {
        if route.document == collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND)
            && self.keys_not_yet.swap(false, Ordering::SeqCst)
        {
            return Some(Err("this device holds no keys of the workspace yet".into()));
        }
        self.member
            .runtime
            .block_on(self.workspaces.own_route_keys(route, create))
    }
}

/// What the service answers `caller` for `message`.
fn served(
    actions: &tilo_agent_session::WorkspaceActions,
    workspace: WorkspaceId,
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

/// The status `caller`'s `request` is answered with.
pub(super) fn invoked(
    actions: &tilo_agent_session::WorkspaceActions,
    workspace: WorkspaceId,
    caller: &Caller,
    request: &InvokeRequest,
) -> ReceiptStatus {
    // A host still settling the workspace answers that it is unavailable, or
    // that a save is to be tried again: asked again under a new id.
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
        let again = matches!(
            &status,
            ReceiptStatus::Rejected {
                error: ActionCallError::Unavailable { .. }
            }
        ) || matches!(&status, ReceiptStatus::Failed { error } if error.retryable);
        if again && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        // A write whose save does not land within the host's grace is answered
        // accepted: observed, as a page observes it, until it ends.
        while matches!(status, ReceiptStatus::Accepted) {
            assert!(Instant::now() < deadline, "the invocation never settled");
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
        return status;
    }
}

/// `bytes` put into the workspace's store by `caller`: its reference.
#[expect(
    clippy::panic_in_result_fn,
    reason = "a reply of another kind is the failure the test reports"
)]
fn put(
    actions: &tilo_agent_session::WorkspaceActions,
    workspace: WorkspaceId,
    caller: &Caller,
    bytes: &[u8],
) -> Result<String, ActionCallError> {
    let step = |step| match served(
        actions,
        workspace,
        caller,
        RemoteActionMessage::ContentPutRequest {
            correlation: 1,
            step,
        },
    ) {
        RemoteActionMessage::ContentPutResponse { result, .. } => result,
        other => panic!("expected an upload's answer, got {other:?}"),
    };
    let ContentPutAnswer::Begun { upload } = step(ContentPutStep::Begin {
        size: bytes.len() as u64,
    })?
    else {
        panic!("an upload begins")
    };
    step(ContentPutStep::append(upload, 0, bytes))?;
    match step(ContentPutStep::Finish { upload })? {
        ContentPutAnswer::Finished { content } => Ok(content),
        other => panic!("expected the content, got {other:?}"),
    }
}

/// The bytes of `content` `caller` gets from the workspace's store.
#[expect(
    clippy::panic_in_result_fn,
    reason = "a reply of another kind is the failure the test reports"
)]
fn got(
    actions: &tilo_agent_session::WorkspaceActions,
    workspace: WorkspaceId,
    caller: &Caller,
    content: &str,
) -> Result<Vec<u8>, ActionCallError> {
    match served(
        actions,
        workspace,
        caller,
        RemoteActionMessage::ContentGetRequest {
            correlation: 1,
            content: content.to_owned(),
            offset: 0,
            length: 0,
        },
    ) {
        RemoteActionMessage::ContentGetResponse { result, .. } => {
            Ok(result?.bytes().expect("well-formed bytes"))
        }
        other => panic!("expected a piece, got {other:?}"),
    }
}

/// The actions `caller` is listed on the document `provider` names, at every
/// level of it: a listing holds the actions of the resources directly under
/// the one it names, so the record is walked as an agent walks it.
pub(super) fn listed(
    actions: &tilo_agent_session::WorkspaceActions,
    workspace: WorkspaceId,
    caller: &Caller,
    provider: &str,
) -> Vec<tilo_actions::ActionDescriptor> {
    let mut listed = Vec::new();
    let mut under: Vec<Option<String>> = vec![None];
    let mut visited: std::collections::HashSet<Option<String>> = std::collections::HashSet::new();
    // A record's tree is shallow; a cap keeps a mistake from listing it
    // forever.
    let mut asked = 0_u32;
    while let Some(parent) = under.pop() {
        if !visited.insert(parent.clone()) {
            continue;
        }
        asked = asked.saturating_add(1);
        assert!(
            asked <= 64,
            "the record {provider} lists more levels than a tree has"
        );
        let query = CatalogueQuery {
            provider: Some(provider.to_owned()),
            parent_resource: parent,
            max_results: 500,
            ..CatalogueQuery::default()
        };
        let rows = match served(
            actions,
            workspace,
            caller,
            RemoteActionMessage::CatalogueRequest {
                correlation: 1,
                query,
            },
        ) {
            RemoteActionMessage::CatalogueResponse { result, .. } => result.unwrap().rows,
            other => panic!("expected a catalogue, got {other:?}"),
        };
        for row in rows {
            if row.has_children {
                under.push(Some(row.resource.id.clone()));
            }
            if let Some(action) = row.action {
                listed.push(action);
            }
        }
    }
    listed
}

/// End a test's page and its server.
pub(super) async fn ended(browser: Browser, hosted: Hosted) {
    browser.peer.close();
    hosted.agents.shutdown();
    hosted.server.shutdown().await;
}

pub(super) fn json(value: serde_json::Value) -> InvokeArgument {
    let serde_json::Value::Object(map) = value else {
        panic!("arguments are an object")
    };
    InvokeArgument::Json(map)
}

/// How long the request that holds the workspace's thread takes (see
/// [`Membership::hold`]).
const HELD: Duration = Duration::from_secs(3);
/// How long content steps that do not need that thread may take: far below
/// [`HELD`], far above what a step of them costs.
const SOON: Duration = Duration::from_secs(1);

/// Putting content into a workspace is not the workspace's thread's work:
/// once its store is found, a member's content steps answer beside a request
/// that holds that thread, and many of them finish while it is still held.
///
/// Before they answered beside it, each of them queued behind whatever the
/// thread was serving, so one slow request held a whole folder's upload.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn content_steps_answer_beside_a_request_that_holds_the_workspaces_thread() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (guest, _) = tilo_user_identity::UserIdentity::generate("Grace", 1);
    let machine = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let editor_device = enrol(&guest, "editor-browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let server = start(&root, &machine, state.path()).await;
    let mut browser = Browser::open(&server, &browser_device, &machine).await;
    browser.create(TASKS);
    crate::workspace_actions::saved(&browser.peer.barrier().unwrap()).unwrap();

    let membership = Membership::of(
        &server,
        &machine,
        &editor_device,
        state.path().join("member-content"),
    );
    let actions = tilo_agent_session::WorkspaceActions::new(Arc::new(crate::common::catalogue));
    actions.attach(membership.clone()).unwrap();
    let workspace = browser.workspace;
    let editor = Caller::authenticated(
        editor_device.cert.user_public_key,
        editor_device.cert.device_public_key,
    )
    .in_workspace(WORKSPACE);
    membership.set_role(Some(Role::Editor));
    // The first step finds the workspace's store on the workspace's thread;
    // every step after it is served beside that thread.
    let step = |step: ContentPutStep| match served(
        &actions,
        workspace,
        &editor,
        RemoteActionMessage::ContentPutRequest {
            correlation: 1,
            step,
        },
    ) {
        RemoteActionMessage::ContentPutResponse { result, .. } => result.unwrap(),
        other => panic!("expected an upload's answer, got {other:?}"),
    };
    let ContentPutAnswer::Begun { upload: first } = step(ContentPutStep::Begin { size: 8 }) else {
        panic!("an upload begins")
    };
    let ContentPutAnswer::Begun { upload: second } = step(ContentPutStep::Begin { size: 8 }) else {
        panic!("an upload begins")
    };

    // The workspace settled: a listing of it answers before its thread is held.
    let listing = || RemoteActionMessage::CatalogueRequest {
        correlation: 8,
        query: CatalogueQuery {
            max_results: 1,
            ..CatalogueQuery::default()
        },
    };
    served(&actions, workspace, &editor, listing());

    // A request holds the workspace's thread: the service reads the member's
    // access from this fixture, which here takes its time.
    membership.hold();
    let mut held = actions.serve(workspace, &editor, listing()).unwrap();
    {
        let mut slow = membership.slow.lock().unwrap();
        while !slow.1 {
            let (guard, waited) = membership.began.wait_timeout(slow, HELD * 2).unwrap();
            slow = guard;
            assert!(
                !waited.timed_out(),
                "the request holding the workspace's thread began"
            );
        }
    }

    // Many steps of the same member, while it is held: they answer, and none of
    // them waits for the thread to be free. The two appends of one upload keep
    // the order they were sent in.
    let started = Instant::now();
    let puts = [
        ContentPutStep::Many {
            files: vec![hex::encode(b"first file")],
        },
        ContentPutStep::Many {
            files: vec![hex::encode(b"second file"), hex::encode(b"third file")],
        },
        ContentPutStep::append(first, 0, b"head"),
        ContentPutStep::append(first, 4, b"tail"),
        ContentPutStep::append(second, 0, b"only"),
    ];
    let mut answers = Vec::new();
    for step in puts {
        let answer = actions
            .serve(
                workspace,
                &editor,
                RemoteActionMessage::ContentPutRequest {
                    correlation: 2,
                    step,
                },
            )
            .unwrap()
            .wait_timeout(SOON.saturating_sub(started.elapsed()))
            .unwrap_or_else(|_| panic!("a content step waited for the workspace's thread"))
            .expect("a content step is answered");
        answers.push(answer);
    }
    assert!(
        started.elapsed() < SOON,
        "the steps took {:?}, waiting for the workspace's thread",
        started.elapsed()
    );
    for answer in answers {
        match answer {
            RemoteActionMessage::ContentPutResponse { result, .. } => {
                assert!(result.is_ok(), "{result:?}");
            }
            other => panic!("expected an upload's answer, got {other:?}"),
        }
    }
    assert_eq!(
        held.poll_ready(),
        tilo_actions::remote::PendingPoll::Waiting,
        "the request holding the thread still waits"
    );
    // Another request of the workspace may have taken the wait first, and been
    // served before the one this test submitted.
    assert!(
        held.wait_timeout(HELD * 4).ok().flatten().is_some(),
        "the held request answers once its read is done"
    );
    ended(browser, server).await;
}

/// The tracker keeps its issues in the parts of a chain, and a workspace that holds no issue
/// yet holds no part of it. Its first issue is filed on the chain itself: that write is taken
/// as the chain's before any part of it stands here.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_first_write_into_an_empty_chain_is_taken() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let site = start(&root, &server, state.path()).await;
    let mut browser = Browser::open(&site, &browser_device, &server).await;
    let tasks = browser.create(TASKS);
    crate::workspace_actions::saved(&browser.peer.barrier().unwrap()).unwrap();
    let tasks_provider = resource_provider(&tasks);
    let issues = ui::resource::chain_resource_id(&ui::resource::DocumentId::derived(
        "issues",
        &ui::resource::DocumentId::workspace_default(),
    ));
    let filed = browser
        .invoke(request(
            &tasks_provider,
            ActionAddress::new(issues, "create"),
            json(serde_json::json!({"title": "Ship the launch"})),
        ))
        .await;
    assert!(
        matches!(
            filed.status,
            ReceiptStatus::Completed { .. } | ReceiptStatus::Accepted
        ),
        "the first issue of a tracker that holds none is filed: {filed:?}"
    );
    ended(browser, site).await;
}

/// A member's first write founds the part of a chain the workspace holds none of: the host
/// answers the chain's head, and the part the view wants, that it holds none of either, and the
/// write founds them here rather than waiting for a part that never comes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_members_first_write_founds_a_chain_the_workspace_holds_no_part_of() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (guest, _) = tilo_user_identity::UserIdentity::generate("Grace", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let guest_device = enrol(&guest, "guest-browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let tasks = browser.create(TASKS);
    crate::workspace_actions::saved(&browser.peer.barrier().unwrap()).unwrap();
    let membership = Membership::of(
        &hosted,
        &host,
        &guest_device,
        state.path().join("member-content"),
    );
    let actions = tilo_agent_session::WorkspaceActions::new(Arc::new(crate::common::catalogue));
    actions.attach(membership.clone()).unwrap();
    super::resource_support::admit_person(
        &hosted,
        &host,
        WORKSPACE,
        &guest_device,
        tilo_peer_state::join::Role::Editor,
    )
    .await;
    membership.becomes(Role::Editor).await;
    let workspace = browser.workspace;
    let member = Caller::authenticated(
        guest_device.cert.user_public_key,
        guest_device.cert.device_public_key,
    )
    .in_workspace(WORKSPACE);
    // Nobody filed an issue: the workspace holds no part of the issues' chain, and the member
    // writes the first.
    let issues = ui::resource::chain_resource_id(&ui::resource::DocumentId::derived(
        "issues",
        &ui::resource::DocumentId::workspace_default(),
    ));
    let created = invoked(
        &actions,
        workspace,
        &member,
        &request(
            &format!(
                "{}:{TASKS}:{}",
                tilo_actions::remote::RESOURCE_PROVIDER,
                tasks.document
            ),
            ActionAddress::new(issues, "create"),
            json(serde_json::json!({"title": "The first issue"})),
        ),
    );
    let ReceiptStatus::Completed { output } = created else {
        panic!("the first write founds the chain: {created:?}");
    };
    assert!(
        output
            .value
            .as_ref()
            .and_then(|value| value.get("made"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|made| made.len() == 1),
        "the create answers the issue it made: {output:?}"
    );
}

// What an app keeps for its whole workspace is reached the same way and judged by the same
// membership, so its tests stand below this fixture.
#[path = "workspace_documents.rs"]
mod workspace_documents;

// What a carrier of edits names to its caller is read from the same listings, for every app.
#[path = "carried_names.rs"]
mod carried_names;

const CHAT: &str = "so.tilo.chat";

/// What a test of the workspace's agents asks through: the action service of the server's
/// device over a link the test answers for, as the server's own person and as a member who
/// edits.
struct Asked {
    actions: tilo_agent_session::WorkspaceActions,
    membership: Arc<Membership>,
    workspace: WorkspaceId,
    provider: String,
    owner: Caller,
    member: Caller,
    /// The server, its person's page and its state, for as long as the test asks.
    _kept: Vec<Box<dyn std::any::Any>>,
}

/// A server whose workspace holds Chat, and the action service asked for its agents.
async fn agents_asked() -> Asked {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (guest, _) = tilo_user_identity::UserIdentity::generate("Grace", 1);
    let device = enrol(&root, "home-server", DeviceCapabilities::member());
    let page_device = enrol(&root, "browser", DeviceCapabilities::member());
    let guest_device = enrol(&guest, "guest-browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let server = start(&root, &device, state.path()).await;
    let mut page = Browser::open(&server, &page_device, &device).await;
    let assistant_doc = page.create(ui::tilo_workspace_agent::OWNER);
    crate::workspace_actions::saved(&page.peer.barrier().unwrap()).unwrap();
    let membership = Membership::of(
        &server,
        &device,
        &guest_device,
        state.path().join("member-content"),
    );
    membership.set_role(Some(Role::Editor));
    let actions = tilo_agent_session::WorkspaceActions::new(Arc::new(crate::common::catalogue));
    actions.attach(membership.clone()).unwrap();
    let person = |device: &Device| {
        Caller::authenticated(device.cert.user_public_key, device.cert.device_public_key)
            .in_workspace(WORKSPACE)
    };
    Asked {
        actions,
        membership,
        workspace: page.workspace,
        provider: format!(
            "{}:{}:{}",
            tilo_actions::remote::RESOURCE_PROVIDER,
            ui::tilo_workspace_agent::OWNER,
            assistant_doc.document
        ),
        owner: person(&page_device),
        member: person(&guest_device),
        _kept: vec![Box::new(server), Box::new(page), Box::new(state)],
    }
}

/// One ask of the workspace's agents, which Chat keeps beside its own document.
fn of_agents(provider: &str, action: &str, argument: InvokeArgument) -> InvokeRequest {
    request(
        provider,
        ActionAddress::new(ui::tilo_workspace_agent::KIND, action),
        argument,
    )
}

/// The names of the agents `read`, the answer to a read of them, lists.
fn agent_names(read: &ReceiptStatus) -> Vec<String> {
    let ReceiptStatus::Completed { output } = read else {
        panic!("the agents were not read: {read:?}")
    };
    output
        .value
        .as_ref()
        .and_then(|value| value.get("agents"))
        .and_then(serde_json::Value::as_array)
        .map(|agents| {
            agents
                .iter()
                .filter_map(|agent| agent.get("name")?.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// A change the server's own person made whose hand-over to the session failed is not built
/// on. The key service answers the first ask for the agents' keys as it does while its device
/// holds none: the write fails as one to be made again. The service kept the copy that had
/// made the change, so the write made again was a change to a document the session was never
/// handed, and waited for that document for good, as did every write to it after.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_owners_next_write_is_saved_after_a_hand_over_that_failed() {
    let asked = agents_asked().await;
    let create = of_agents(
        &asked.provider,
        "create-agent",
        json(serde_json::json!({"name": "Assistant"})),
    );
    asked.membership.keys_not_yet_once();
    // `invoked` makes again what is answered as to be made again: the first hand-over fails.
    let made = invoked(&asked.actions, asked.workspace, &asked.owner, &create);
    assert!(
        matches!(made, ReceiptStatus::Completed { .. }),
        "made again after a hand-over that failed, the write is saved: {made:?}"
    );
    let read = invoked(
        &asked.actions,
        asked.workspace,
        &asked.owner,
        &of_agents(&asked.provider, "read", InvokeArgument::None),
    );
    assert_eq!(
        agent_names(&read),
        ["Assistant"],
        "the agent of the write that failed is in no copy: the saved one alone is read"
    );
}

/// A member's change whose hand-over failed is in no copy the server writes through: what
/// its own person writes next is saved, and the member reads the workspace's agents. The
/// member's change was merged into the server's copy before it was handed over, so the next
/// write made there built on a change no device ever sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_members_hand_over_that_failed_leaves_nothing_the_owner_writes_on() {
    let asked = agents_asked().await;
    let create = |name: &str| {
        of_agents(
            &asked.provider,
            "create-agent",
            json(serde_json::json!({"name": name})),
        )
    };
    asked.membership.keys_not_yet_once();
    let first = invoked(
        &asked.actions,
        asked.workspace,
        &asked.member,
        &create("Assistant"),
    );
    assert!(
        matches!(first, ReceiptStatus::Completed { .. }),
        "made again after a hand-over that failed, the member's write is saved: {first:?}"
    );
    let second = invoked(
        &asked.actions,
        asked.workspace,
        &asked.owner,
        &create("Second"),
    );
    assert!(
        matches!(second, ReceiptStatus::Completed { .. }),
        "what the server's own person writes after it is saved: {second:?}"
    );
    let mut names = agent_names(&invoked(
        &asked.actions,
        asked.workspace,
        &asked.member,
        &of_agents(&asked.provider, "read", InvokeArgument::None),
    ));
    names.sort();
    assert_eq!(
        names,
        ["Assistant", "Second"],
        "the member reads the workspace's agents: the two that were saved"
    );
}

// What a member is answered by the role the workspace's own governance holds stands below this
// fixture too.
#[path = "held_roles.rs"]
mod held_roles;
