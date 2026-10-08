//! How often one admission or one directory page reads the committed
//! workspace authority, and that it borrows rather than copies it.
use super::*;
use crate::services::WorkspaceAuthority;
use crate::test_support::{Workspace, WorkspaceId};
use std::sync::atomic::AtomicUsize;
use tilo_actions::remote::{
    Caller, InvokeArgument, InvokeRequest, PendingPoll, RemoteActionTarget,
};
use tilo_desktop_model::DesktopTree;

type Revision = Box<dyn FnOnce(&mut DesktopTree) + Send>;

/// A committed authority that counts every sample and every owned copy,
/// and can change policy just before a chosen sample reads it.
struct SampledAuthority {
    tree: Mutex<DesktopTree>,
    samples: AtomicUsize,
    copies: AtomicUsize,
    /// Applied to the tree immediately before the numbered (1-based) sample.
    before: Mutex<Option<(usize, Revision)>>,
}

impl SampledAuthority {
    fn new(tree: DesktopTree) -> Arc<Self> {
        Arc::new(Self {
            tree: Mutex::new(tree),
            samples: AtomicUsize::new(0),
            copies: AtomicUsize::new(0),
            before: Mutex::new(None),
        })
    }

    fn samples(&self) -> usize {
        self.samples.load(Ordering::SeqCst)
    }

    fn copies(&self) -> usize {
        self.copies.load(Ordering::SeqCst)
    }

    fn reset(&self) {
        self.samples.store(0, Ordering::SeqCst);
        self.copies.store(0, Ordering::SeqCst);
    }

    fn revise_before_sample(&self, sample: usize, revise: Revision) {
        *self.before.lock().unwrap() = Some((sample, revise));
    }
}

impl WorkspaceAuthority for SampledAuthority {
    fn with_tree(&self, read: &mut dyn FnMut(&DesktopTree)) -> Result<(), String> {
        let sample = self.samples.fetch_add(1, Ordering::SeqCst) + 1;
        let mut tree = self.tree.lock().map_err(|_| "authority poisoned")?;
        let mut before = self.before.lock().map_err(|_| "authority poisoned")?;
        if before.as_ref().is_some_and(|(at, _)| *at == sample)
            && let Some((_, revise)) = before.take()
        {
            revise(&mut tree);
        }
        drop(before);
        read(&tree);
        Ok(())
    }

    fn snapshot(&self) -> Result<DesktopTree, String> {
        self.copies.fetch_add(1, Ordering::SeqCst);
        let mut copy = None;
        self.with_tree(&mut |tree| copy = Some(tree.clone()))?;
        copy.ok_or_else(|| "authority did not answer".into())
    }
}

fn owner() -> String {
    "a1".repeat(32)
}

fn owned_room() -> (DesktopTree, String) {
    let mut workspace = Workspace::new(WorkspaceId(91), "Sampled authority");
    workspace.set_owner(owner());
    let room = workspace.room.to_string();
    let mut tree = DesktopTree::default();
    tree.active = WorkspaceId(91);
    tree.workspaces.push(workspace);
    (tree, room)
}

fn write_surface(room: &str) -> PublishedSurface {
    PublishedSurface {
        peer_id: 7,
        app_id: "so.tilo.test".into(),
        route: CrdtRoute::record("so.tilo.test", room, "record-a"),
        actions: vec![PublishedAction {
            semantic_identity: None,
            resource: "test:block-a".into(),
            action: "update".into(),
            resource_kind: "test.block".into(),
            resource_label: "Block".into(),
            action_label: "Update".into(),
            description: None,
            operation_kind: tilo_actions::OperationKind::Effect,
            params: Vec::new(),
            contract: tilo_actions::ActionContract {
                permission: tilo_actions::PermissionRequirement::Write,
                ..Default::default()
            },
            input: PublishedInput::None,
            view: None,
        }],
        live: true,
    }
}

fn invoke_request() -> InvokeRequest {
    InvokeRequest {
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
    }
}

/// A broker over `authority` with one live application publishing a
/// Write action in `room`, and the caller it is offered to.
fn invocation_fixture(
    authority: &Arc<SampledAuthority>,
    room: &str,
) -> (
    Arc<CapnpBrokerState>,
    Caller,
    mpsc::UnboundedReceiver<InvocationEnvelope>,
) {
    let broker = CapnpBrokerState::test_broker_with_authority(
        Arc::clone(authority) as WorkspaceTreeHandle,
        HostServices::default(),
    );
    let state = Arc::clone(broker.state());
    state.replace_published_surfaces_for_test(vec![write_surface(room)]);
    let receiver = state.capture_invocations_for_test(7);
    let caller = Caller::local_person(owner())
        .in_room(room.to_owned())
        .in_new_execution();
    authority.reset();
    (state, caller, receiver)
}

#[test]
fn action_host_invocation_admission_samples_authority_once_and_copies_nothing() {
    let (tree, room) = owned_room();
    let authority = SampledAuthority::new(tree);
    let (state, caller, mut app) = invocation_fixture(&authority, &room);

    let admitted = state
        .action_invoke(&caller, None, None, &invoke_request())
        .poll_ready();

    assert!(
        matches!(admitted, PendingPoll::Ready(Ok(_))),
        "the owner is admitted"
    );
    assert!(app.try_recv().is_ok(), "the invocation reaches the app");
    // One sample decides whether the action is offered to the caller (before
    // any confirmation can reveal it); one decides every check of the
    // admission itself. Neither copies the tree.
    assert_eq!(authority.samples(), 2);
    assert_eq!(authority.copies(), 0);
}

#[test]
fn admission_decides_on_one_consistent_sample() {
    let (tree, room) = owned_room();
    let authority = SampledAuthority::new(tree);
    let (state, caller, mut app) = invocation_fixture(&authority, &room);
    // The caller is offered the action on the first sample; ownership moves
    // away just before the admission reads authority again.
    authority.revise_before_sample(
        2,
        Box::new(|tree| tree.workspaces[0].set_owner("replacement")),
    );

    let admitted = state
        .action_invoke(&caller, None, None, &invoke_request())
        .poll_ready();

    assert!(
        matches!(admitted, PendingPoll::Ready(Err(_))),
        "the admission decides on the revised policy"
    );
    assert!(app.try_recv().is_err(), "nothing reaches the app");
    // The action check and the record check share the admission's one
    // sample: there is no later read that could decide on other state.
    assert_eq!(authority.samples(), 2);
    assert_eq!(authority.copies(), 0);
}

const PAGE_ROWS: usize = 200;

/// An application directory answering every query with `PAGE_ROWS` Read
/// actions.
struct WideDirectory;

impl WideDirectory {
    fn row(index: usize) -> runtime_collab::ActionDirectoryRow {
        let contract_json =
            serde_json::to_string(&tilo_actions::ActionContract::view()).expect("contract");
        let resource = format!("workspace:atlas-{index}");
        let descriptor = runtime_collab::ExportedAction {
            semantic_identity: None,
            resource: resource.clone(),
            action: "open".into(),
            resource_kind: "workspace".into(),
            resource_label: "Atlas".into(),
            action_label: "Open Atlas".into(),
            description: None,
            operation_kind: "view".into(),
            params_json: "[]".into(),
            contract_json: contract_json.clone(),
            input: runtime_collab::ExportedInput::None,
            view: None,
        };
        runtime_collab::ActionDirectoryRow {
            parent_resource: String::new(),
            resource,
            resource_kind: descriptor.resource_kind.clone(),
            resource_label: descriptor.resource_label.clone(),
            resource_presentation_json: String::new(),
            action: Some(descriptor.action.clone()),
            descriptor: Some(descriptor),
            has_children: false,
            visibility_action: Some("open".into()),
            visibility_contract_json: contract_json,
        }
    }
}

#[allow(
    refining_impl_trait,
    reason = "the generated Cap'n Proto test endpoint has the same concrete Promise shape as production endpoints"
)]
impl action_directory::Server for WideDirectory {
    fn query(
        self: Rc<Self>,
        _params: action_directory::QueryParams,
        mut results: action_directory::QueryResults,
    ) -> Promise<(), capnp::Error> {
        let page = runtime_collab::ActionDirectoryPage {
            revision: "wide-r1".into(),
            rows: (0..PAGE_ROWS).map(Self::row).collect(),
            next_cursor: Vec::new(),
            exhausted: true,
            stale_cursor: false,
            budget_exhausted: false,
        };
        if let Err(error) = write_directory_page(results.get().init_page(), &page) {
            return Promise::err(error);
        }
        Promise::ok(())
    }

    fn resolve(
        self: Rc<Self>,
        _params: action_directory::ResolveParams,
        mut results: action_directory::ResolveResults,
    ) -> Promise<(), capnp::Error> {
        results.get().init_resolution().set_not_found(());
        Promise::ok(())
    }
}

/// Query one page of the wide directory in `room` as `actor`.
async fn query_wide_page(
    authority: &Arc<SampledAuthority>,
    room: &str,
    actor: &BrokerActor,
) -> runtime_collab::ActionDirectoryPage {
    let fanout = Arc::new(FanoutState::new());
    let (state, bridge_rx) = CapnpBrokerState::new_configured(
        &fanout,
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(Arc::clone(authority) as WorkspaceTreeHandle),
        None,
        None,
        HostServices::default(),
    );
    let route = CrdtRoute::record("so.tilo.atlas", room, "atlas");
    let directories = LocalActionDirectoryRegistry::new(bridge_rx);
    directories.directories.borrow_mut().insert(
        41,
        RegisteredActionDirectory {
            registration: Rc::new(uuid::Uuid::new_v4()),
            app_id: route.app_id.clone(),
            route,
            directory: capnp_rpc::new_client(WideDirectory),
        },
    );
    authority.reset();
    state
        .query_action_directory_for_actor(
            &directories,
            actor,
            room,
            &runtime_collab::ActionDirectoryQuery {
                peer_id: 41,
                revision: String::new(),
                cursor: Vec::new(),
                needle: String::new(),
                parent_resource: String::new(),
                max_results: u32::try_from(PAGE_ROWS).expect("page rows"),
                max_bytes: CapnpBrokerState::DIRECTORY_MAX_BYTES,
                max_work: CapnpBrokerState::DIRECTORY_MAX_WORK,
                max_depth: 1,
            },
        )
        .await
        .expect("the directory page is answered")
}

#[tokio::test(flavor = "current_thread")]
async fn directory_page_samples_authority_once_regardless_of_rows() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (tree, room) = owned_room();
            let authority = SampledAuthority::new(tree);

            let page = query_wide_page(&authority, &room, &BrokerActor::person(owner())).await;
            assert_eq!(page.rows.len(), PAGE_ROWS, "the owner sees every row");
            assert_eq!(authority.samples(), 1);
            assert_eq!(authority.copies(), 0);

            let page = query_wide_page(&authority, &room, &BrokerActor::person("stranger")).await;
            assert!(page.rows.is_empty(), "a stranger sees no row");
            assert_eq!(authority.samples(), 1);
            assert_eq!(authority.copies(), 0);
        })
        .await;
}
