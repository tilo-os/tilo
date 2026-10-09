//! How often one admission or one directory page reads the committed
//! workspace authority, and that neither copies it: no owned snapshot is
//! requested, and the heap the path asks for does not grow with the
//! workspace's effect-receipt history.
use super::*;
use crate::services::WorkspaceAuthority;
use crate::test_support::{Workspace, WorkspaceId};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::AtomicUsize;
use tilo_actions::remote::{
    Caller, InvokeArgument, InvokeRequest, PendingPoll, RemoteActionTarget,
};
use tilo_desktop_model::DesktopTree;

type Revision = Box<dyn FnOnce(&mut DesktopTree) + Send>;

/// A committed authority that counts every sample and every owned snapshot,
/// and can change policy just before a chosen sample reads it.
struct SampledAuthority {
    tree: Mutex<DesktopTree>,
    samples: AtomicUsize,
    owned_snapshots: AtomicUsize,
    /// Applied to the tree immediately before the numbered (1-based) sample.
    before: Mutex<Option<(usize, Revision)>>,
}

impl SampledAuthority {
    fn new(tree: DesktopTree) -> Arc<Self> {
        Arc::new(Self {
            tree: Mutex::new(tree),
            samples: AtomicUsize::new(0),
            owned_snapshots: AtomicUsize::new(0),
            before: Mutex::new(None),
        })
    }

    fn samples(&self) -> usize {
        self.samples.load(Ordering::SeqCst)
    }

    fn owned_snapshots(&self) -> usize {
        self.owned_snapshots.load(Ordering::SeqCst)
    }

    fn reset(&self) {
        self.samples.store(0, Ordering::SeqCst);
        self.owned_snapshots.store(0, Ordering::SeqCst);
    }

    fn revise_before_sample(&self, sample: usize, revise: Revision) {
        *self.before.lock().unwrap() = Some((sample, revise));
    }
}

impl WorkspaceAuthority for SampledAuthority {
    fn with_tree(&self, read: &mut dyn FnMut(&DesktopTree)) -> Result<(), String> {
        let sample = self
            .samples
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
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
        self.owned_snapshots.fetch_add(1, Ordering::SeqCst);
        let mut copy = None;
        self.with_tree(&mut |tree| copy = Some(tree.clone()))?;
        copy.ok_or_else(|| "authority did not answer".into())
    }
}

// Bytes this thread requests from the heap while metering is on. A copy
// of the tree, wherever it is written (a `snapshot()`, or a `clone()` inside
// a decide closure), allocates at least its receipt history.
std::thread_local! {
    static METERED: Cell<Option<usize>> = const { Cell::new(None) };
}

struct Meter;

#[global_allocator]
static ALLOCATOR: Meter = Meter;

fn metered(bytes: usize) {
    let _ = METERED.try_with(|total| {
        if let Some(sum) = total.get() {
            total.set(Some(sum.saturating_add(bytes)));
        }
    });
}

// SAFETY: forwards the exact System allocation contracts; the bookkeeping
// uses a fixed thread-local Cell and never changes pointers or sizes.
unsafe impl GlobalAlloc for Meter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        metered(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        metered(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        metered(size);
        unsafe { System.realloc(ptr, layout, size) }
    }
}

/// Start counting this thread's heap requests.
fn meter_start() {
    METERED.with(|total| total.set(Some(0)));
}

/// Stop counting and return the bytes requested since [`meter_start`].
fn meter_stop() -> usize {
    METERED.with(Cell::take).unwrap_or(0)
}

/// Effect receipts in the heavy fixture: a long agent history.
const RECEIPTS: usize = 20_000;

/// The heap a copy of the fixture's receipt history needs at least.
fn receipt_history_bytes() -> usize {
    RECEIPTS.saturating_mul(
        std::mem::size_of::<tilo_desktop_model::WorkspaceEffectReceipt>().saturating_add(64),
    )
}

/// The most a path may request on the heavy fixture: a quarter of one
/// receipt-history copy. A path that copied the tree once would exceed it.
fn copy_free_budget() -> usize {
    receipt_history_bytes() / 4
}

fn owner() -> String {
    "a1".repeat(32)
}

fn owned_room() -> (DesktopTree, String) {
    let mut workspace = Workspace::new(WorkspaceId(91), "Sampled authority");
    workspace.set_owner(owner());
    workspace.effect_receipts = (0..RECEIPTS)
        .map(|index| tilo_desktop_model::WorkspaceEffectReceipt {
            invocation: tilo_actions::InvocationId::new(),
            actor: format!("{index:064}"),
            context: tilo_actions::ActionContext::default(),
            status: tilo_actions::EffectReceiptStatus::Succeeded,
            completed_at: 0,
        })
        .collect();
    let room = workspace.room.to_string();
    let tree = DesktopTree {
        active: WorkspaceId(91),
        workspaces: vec![workspace],
        ..DesktopTree::default()
    };
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
        fresh_binding: None,
        wait_ms: None,
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
fn the_heap_meter_sees_one_copy_of_the_receipt_history() {
    let (tree, _) = owned_room();
    meter_start();
    let copy = tree.clone();
    let copied = meter_stop();
    assert_eq!(
        copy.workspaces[0].effect_receipts.len(),
        tree.workspaces[0].effect_receipts.len()
    );
    assert!(
        copied >= receipt_history_bytes(),
        "a tree copy requested {copied} bytes; the meter cannot observe a copy"
    );
    assert!(copy_free_budget() < copied, "the budget admits a copy");
}

#[test]
fn action_host_invocation_admission_samples_authority_once_without_an_owned_snapshot() {
    let (tree, room) = owned_room();
    let authority = SampledAuthority::new(tree);
    let (state, caller, mut app) = invocation_fixture(&authority, &room);

    meter_start();
    let admitted = state
        .action_invoke(&caller, None, None, &invoke_request())
        .poll_ready();
    let requested = meter_stop();

    assert!(
        matches!(admitted, PendingPoll::Ready(Ok(_))),
        "the owner is admitted"
    );
    assert!(app.try_recv().is_ok(), "the invocation reaches the app");
    // One sample decides whether the action is offered to the caller (before
    // any confirmation can reveal it); one decides every check of the
    // admission itself. Neither asks for an owned snapshot of the tree.
    assert_eq!(authority.samples(), 2);
    assert_eq!(authority.owned_snapshots(), 0);
    // Nor does anything on the path copy the tree another way.
    assert!(
        requested < copy_free_budget(),
        "admission requested {requested} bytes with {RECEIPTS} receipts"
    );
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
    assert_eq!(authority.owned_snapshots(), 0);
}

const PAGE_ROWS: usize = 200;

/// An application directory answering every query with `PAGE_ROWS` Read
/// actions.
struct WideDirectory;

impl WideDirectory {
    fn row(index: usize) -> runtime_collab::ActionDirectoryRow {
        let contract = tilo_actions::ActionContract::view();
        let resource = format!("workspace:atlas-{index}");
        let descriptor = runtime_collab::ExportedAction {
            semantic_identity: None,
            resource: resource.clone(),
            action: "open".into(),
            resource_kind: "workspace".into(),
            resource_label: "Atlas".into(),
            action_label: "Open Atlas".into(),
            description: None,
            operation_kind: tilo_actions::OperationKind::View,
            params: Vec::new(),
            contract: contract.clone(),
            input: runtime_collab::ExportedInput::None,
            view: None,
        };
        runtime_collab::ActionDirectoryRow {
            parent_resource: String::new(),
            resource,
            resource_kind: descriptor.resource_kind.clone(),
            resource_label: descriptor.resource_label.clone(),
            presentation: None,
            action: Some(descriptor.action.clone()),
            descriptor: Some(descriptor),
            fresh_binding: None,
            has_children: false,
            visibility: Some(runtime_collab::RowVisibility {
                action: "open".into(),
                contract,
            }),
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
        if let Err(error) =
            runtime_collab::write_action_directory_page(results.get().init_page(), &page)
        {
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
#[allow(
    clippy::future_not_send,
    reason = "the directory registry holds Rc Cap'n Proto clients and is driven on the test's LocalSet, like production"
)]
async fn query_wide_page(
    authority: &Arc<SampledAuthority>,
    room: &str,
    actor: &BrokerActor,
) -> (runtime_collab::ActionDirectoryPage, usize) {
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
    meter_start();
    let page = state
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
        .await;
    let requested = meter_stop();
    (page.expect("the directory page is answered"), requested)
}

#[tokio::test(flavor = "current_thread")]
async fn directory_page_samples_authority_once_regardless_of_rows() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (tree, room) = owned_room();
            let authority = SampledAuthority::new(tree);

            let (page, requested) =
                query_wide_page(&authority, &room, &BrokerActor::person(owner())).await;
            assert_eq!(page.rows.len(), PAGE_ROWS, "the owner sees every row");
            assert_eq!(authority.samples(), 1);
            assert_eq!(authority.owned_snapshots(), 0);
            assert!(
                requested < copy_free_budget(),
                "a {PAGE_ROWS}-row page requested {requested} bytes with {RECEIPTS} receipts"
            );

            let (page, requested) =
                query_wide_page(&authority, &room, &BrokerActor::person("stranger")).await;
            assert!(page.rows.is_empty(), "a stranger sees no row");
            assert_eq!(authority.samples(), 1);
            assert_eq!(authority.owned_snapshots(), 0);
            assert!(requested < copy_free_budget(), "{requested} bytes");
        })
        .await;
}
