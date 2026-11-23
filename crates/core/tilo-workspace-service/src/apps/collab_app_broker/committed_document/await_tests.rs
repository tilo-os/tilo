//! A held `Await` makes exactly the Open's checks, and makes them again only
//! when told something changed (or, where nothing can tell it, on the clock
//! readers used to poll at).
use super::{
    AwaitingDocument, Budget, ChainHeadAnswer, HeldRevision, NativeDocumentRead,
    NativeDocumentReadContext, NativeDocumentReads, ResourceRead, UNANNOUNCED_RECHECK,
    hold_until_changed, next_unannounced_recheck, read_unchanged,
};
use automerge::transaction::Transactable as _;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

const WORKSPACE: &str = "7851d1ef-c4fa-4d23-aa65-5ad85f80a6af";

/// What the owner would answer an Open now.
#[derive(Clone, Copy)]
enum Owner {
    At([u8; 32]),
    Pending,
    Absent,
}

struct Service {
    owner: Mutex<Owner>,
    readable: AtomicBool,
    /// Whether `unchanged` can answer from heads; otherwise the broker
    /// captures.
    cheap: bool,
    captures: AtomicUsize,
}
impl Service {
    fn new(owner: Owner, cheap: bool) -> Arc<Self> {
        Arc::new(Self {
            owner: Mutex::new(owner),
            readable: AtomicBool::new(true),
            cheap,
            captures: AtomicUsize::new(0),
        })
    }
    fn owner(&self) -> Owner {
        *self.owner.lock().unwrap()
    }
}
impl NativeDocumentReads for Service {
    /// The document is one this device holds, sealed under its workspace's keys.
    fn seals_documents(&self, _: &str) -> bool {
        true
    }
    fn resource_read(&self, _: &str, _: &str, _: &str) -> super::ResourceRead {
        super::ResourceRead::Sealed
    }
    fn authorize(&self, context: &NativeDocumentReadContext) -> Result<(), String> {
        context.revalidate()?;
        if self.readable.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err("the reader may not read that document".into())
        }
    }
    fn capture(
        &self,
        context: &NativeDocumentReadContext,
        known: Option<[u8; 32]>,
        _: usize,
    ) -> Result<NativeDocumentRead, String> {
        self.authorize(context)?;
        self.captures.fetch_add(1, Ordering::AcqRel);
        Ok(match self.owner() {
            Owner::At(revision) if Some(revision) == known => {
                NativeDocumentRead::Unchanged(revision)
            }
            Owner::At(revision) => NativeDocumentRead::Captured {
                revision,
                heads: Vec::new(),
                body: Box::new(Bytes(vec![1])),
            },
            Owner::Pending => NativeDocumentRead::Pending,
            Owner::Absent => NativeDocumentRead::Absent,
        })
    }
    fn unchanged(
        &self,
        context: &NativeDocumentReadContext,
        known: Option<[u8; 32]>,
    ) -> Result<Option<bool>, String> {
        if !self.cheap {
            return Ok(None);
        }
        self.authorize(context)?;
        Ok(match (self.owner(), known) {
            (Owner::At(revision), Some(known)) => Some(revision == known),
            _ => None,
        })
    }
}

struct Bytes(Vec<u8>);
impl super::DocumentBytes for Bytes {
    fn bytes(&self) -> &[u8] {
        &self.0
    }
}

fn context(ownership: &Arc<AtomicBool>) -> NativeDocumentReadContext {
    let root = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[32; 32]);
    let mut certificate = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Reader".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
    let ownership = Arc::clone(ownership);
    NativeDocumentReadContext {
        resource_type: None,
        route: crate::apps::collab_broker::CrdtRoute::record("so.tilo.notes", WORKSPACE, ""),
        certificate,
        revalidate: Arc::new(move || {
            if ownership.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err("committed read launcher ownership ended".into())
            }
        }),
    }
}

fn unchanged(service: &Service, ownership: &Arc<AtomicBool>, known: Option<[u8; 32]>) -> bool {
    holds(service, ownership, known, false)
}

/// Whether a wait holds for a reader holding `known`, or for `None` the
/// `Absent` (`absent`) or `Unavailable` its last Open answered.
fn holds(
    service: &Service,
    ownership: &Arc<AtomicBool>,
    known: Option<[u8; 32]>,
    absent: bool,
) -> bool {
    read_unchanged(
        service,
        service.resource_read(WORKSPACE, "so.tilo.notes", ""),
        || Ok(context(ownership)),
        &Arc::new(Budget::default()),
        known,
        absent,
    )
    .is_some()
}

tilo_test_env::scenario! {
    fn a_reader_waits_until_its_answer_would_change;

    phase a_reader_told_a_record_is_absent_waits_until_that_changes;
    {
        let ownership = Arc::new(AtomicBool::new(true));
        let service = Service::new(Owner::Pending, true);
        assert!(holds(&service, &ownership, None, false));
        *service.owner.lock().unwrap() = Owner::Absent;
        assert!(
            !holds(&service, &ownership, None, false),
            "the hosts saying they have none ends a wait on a record arriving"
        );
        assert!(
            holds(&service, &ownership, None, true),
            "an absent record holds a reader told so"
        );
        *service.owner.lock().unwrap() = Owner::Pending;
        assert!(
            !holds(&service, &ownership, None, true),
            "a host connecting ends the wait of a reader told it is absent"
        );
        *service.owner.lock().unwrap() = Owner::At([1; 32]);
        assert!(
            !holds(&service, &ownership, None, true),
            "the record arriving ends it too"
        );
    }

    phase a_wait_holds_exactly_while_an_open_would_answer_what_the_reader_holds;
    {
        for cheap in [true, false] {
            let ownership = Arc::new(AtomicBool::new(true));
            let service = Service::new(Owner::At([1; 32]), cheap);
            assert!(unchanged(&service, &ownership, Some([1; 32])));
            assert!(
                !unchanged(&service, &ownership, Some([2; 32])),
                "a reader holding another revision is stale"
            );
            assert!(
                !unchanged(&service, &ownership, None),
                "a readable record is stale to a reader that holds nothing"
            );
            *service.owner.lock().unwrap() = Owner::At([3; 32]);
            assert!(
                !unchanged(&service, &ownership, Some([1; 32])),
                "a newer committed revision ends the wait"
            );
            *service.owner.lock().unwrap() = Owner::Pending;
            assert!(
                unchanged(&service, &ownership, None),
                "a checkpoint still arriving holds a reader that holds nothing"
            );
        }
    }

    phase a_refusal_ends_the_wait_so_the_reader_s_open_meets_it;
    {
        let ownership = Arc::new(AtomicBool::new(true));
        let service = Service::new(Owner::At([1; 32]), true);
        service.readable.store(false, Ordering::Release);
        assert!(
            !unchanged(&service, &ownership, Some([1; 32])),
            "a revoked read permission is never held"
        );
        service.readable.store(true, Ordering::Release);
        ownership.store(false, Ordering::Release);
        assert!(
            !unchanged(&service, &ownership, Some([1; 32])),
            "ended launcher ownership is never held"
        );
        assert!(
            read_unchanged(
                service.as_ref(),
                service.as_ref().resource_read(WORKSPACE, "so.tilo.notes", ""),
                || Err("committed read admission ended".into()),
                &Arc::new(Budget::default()),
                Some([1; 32]),
                false,
            )
            .is_none(),
            "a refused admission is never held"
        );
    }

    phase an_unchanged_revision_is_held_from_heads_without_a_capture;
    {
        let ownership = Arc::new(AtomicBool::new(true));
        let service = Service::new(Owner::At([1; 32]), true);
        for _ in 0..10 {
            assert!(unchanged(&service, &ownership, Some([1; 32])));
        }
        assert_eq!(service.captures.load(Ordering::Acquire), 0);
    }

    phase a_capture_the_owner_cannot_avoid_respects_the_shared_byte_budget;
    {
        let ownership = Arc::new(AtomicBool::new(true));
        let service = Service::new(Owner::At([1; 32]), false);
        let budget = Arc::new(Budget::default());
        let held: Vec<_> = std::iter::from_fn(|| budget.try_reserve()).collect();
        assert!(!held.is_empty());
        assert!(
            read_unchanged(
                service.as_ref(),
                service.as_ref().resource_read(WORKSPACE, "so.tilo.notes", ""),
                || Ok(context(&ownership)),
                &budget,
                Some([1; 32]),
                false,
            )
            .is_none(),
            "with the budget spent the wait ends and the Open decides"
        );
        assert_eq!(service.captures.load(Ordering::Acquire), 0);
        drop(held);
        assert!(
            read_unchanged(
                service.as_ref(),
                service.as_ref().resource_read(WORKSPACE, "so.tilo.notes", ""),
                || Ok(context(&ownership)),
                &budget,
                Some([1; 32]),
                false,
            )
            .is_some()
        );
    }
}

#[tokio::test(start_paused = true)]
async fn an_announced_wait_checks_again_only_when_woken() {
    let wake = Arc::new(tokio::sync::Notify::new());
    let checks = Cell::new(0_u32);
    let stale = Cell::new(false);
    let held = hold_until_changed(
        &wake,
        None,
        || {},
        || {
            checks.set(checks.get() + 1);
            Ok((!stale.get()).then_some(true))
        },
    );
    tokio::pin!(held);
    // A minute of idle time: one check, no clock.
    assert!(
        tokio::time::timeout(Duration::from_secs(60), held.as_mut())
            .await
            .is_err()
    );
    assert_eq!(checks.get(), 1, "an idle wait never re-checks on a clock");
    // Something it reads changed, but not the answer: one more check.
    wake.notify_one();
    assert!(
        tokio::time::timeout(Duration::from_secs(60), held.as_mut())
            .await
            .is_err()
    );
    assert_eq!(checks.get(), 2);
    // The next change ends it at once.
    stale.set(true);
    let started = tokio::time::Instant::now();
    wake.notify_one();
    assert_eq!(held.await, Ok(()));
    assert_eq!(checks.get(), 3);
    assert!(started.elapsed() < UNANNOUNCED_RECHECK);
}

#[tokio::test(start_paused = true)]
async fn an_unannounced_input_keeps_the_old_poll_interval() {
    let wake = Arc::new(tokio::sync::Notify::new());
    let checks = Cell::new(0_u32);
    let started = tokio::time::Instant::now();
    let held = hold_until_changed(
        &wake,
        None,
        || {},
        || {
            checks.set(checks.get() + 1);
            // Revoked on the sixth look, which a 200 ms clock reaches at 1 s.
            Ok((checks.get() < 6).then_some(false))
        },
    );
    assert_eq!(held.await, Ok(()));
    assert_eq!(checks.get(), 6);
    assert!(
        started.elapsed() <= UNANNOUNCED_RECHECK * 5,
        "a revocation nothing announces is still seen within the old interval"
    );
}

#[tokio::test(start_paused = true)]
async fn a_change_while_checking_is_not_missed() {
    let wake = Arc::new(tokio::sync::Notify::new());
    let checks = Cell::new(0_u32);
    let held = hold_until_changed(
        &wake,
        None,
        || {},
        || {
            checks.set(checks.get() + 1);
            if checks.get() == 1 {
                // An announcement lands after the watch was armed and while
                // the check runs: it must wake the next turn.
                wake.notify_one();
                return Ok(Some(true));
            }
            Ok(None)
        },
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), held).await,
        Ok(Ok(()))
    );
    assert_eq!(checks.get(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_replaced_wait_is_refused() {
    let wake = Arc::new(tokio::sync::Notify::new());
    let replaced = Cell::new(false);
    let held = hold_until_changed(
        &wake,
        None,
        || {},
        || {
            if replaced.get() {
                return Err("committed read wait was replaced".into());
            }
            Ok(Some(true))
        },
    );
    tokio::pin!(held);
    assert!(
        tokio::time::timeout(Duration::from_secs(1), held.as_mut())
            .await
            .is_err()
    );
    replaced.set(true);
    wake.notify_one();
    assert!(held.await.is_err());
}

#[tokio::test(start_paused = true)]
async fn clocked_waits_share_their_recheck_ticks() {
    let first = next_unannounced_recheck();
    let now = tokio::time::Instant::now();
    assert!(first > now && first <= now.checked_add(UNANNOUNCED_RECHECK).unwrap());
    // A wait armed later within the same period re-checks on the same tick,
    // so many readers cost the broker one wakeup per tick.
    tokio::time::advance(Duration::from_millis(50)).await;
    if tokio::time::Instant::now() < first {
        assert_eq!(next_unannounced_recheck(), first);
    }
}

/// A held read of a sealed record whose write this device holds no key for
/// yet ends once the key arrives, with nothing written to the record.
#[tokio::test]
async fn a_sealed_read_waiting_for_its_key_ends_when_the_key_arrives() {
    use tilo_workspace_keys::{SharedKeys, testing};
    let workspace = Arc::new(testing::Workspace::default());
    let workspace_id = tilo_desktop_model::WorkspaceId::parse(WORKSPACE).unwrap();
    let policy = testing::Policy::readers([1, 2]);
    let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    let [founder, reader] = [0_u8, 1].map(|id| {
        let seed = id + 1;
        let service = tilo_workspace_keys::WorkspaceKeys::open(
            tilo_workspace_keys::KeysConfig {
                state: Arc::new(tilo_workspace_keys::KeyDir::new(
                    dirs[usize::from(id)].path(),
                )),
                vault: key_group::VaultKey::new([seed; 32]),
                signer: Arc::new(testing::key(seed)),
                role: tilo_workspace_keys::KeyRole::Committer,
            },
            Box::new(testing::Directory {
                policy: policy.clone(),
                founds: Arc::new(std::sync::atomic::AtomicBool::new(id == 0)),
            }),
            Arc::new(tilo_workspace_keys::SealChecks::default()),
        )
        .unwrap();
        let endpoint = testing::Endpoint::join(&workspace, workspace_id, usize::from(id));
        (SharedKeys::new(service), endpoint)
    });
    let keys = tilo_workspace_keys::KeyProvider::default();
    keys.set(Some(reader.0.clone()));
    let keys_here = || {
        keys.get()
            .and_then(|access| access.record_keys(workspace_id))
            .is_some_and(|keys| keys.is_ok())
    };
    let mut now = tilo_clock::now();
    let mut pass = |(access, endpoint): &(SharedKeys, testing::Endpoint)| {
        now = now.checked_add(Duration::from_secs(60)).unwrap();
        access.access(workspace_id);
        access.pump_at(endpoint, now);
    };
    // The founder's group exists; the reader has heard of it and holds none
    // of its keys.
    pass(&founder);
    pass(&reader);
    assert!(!keys_here());

    let wake = Arc::new(tokio::sync::Notify::new());
    let ownership = Arc::new(AtomicBool::new(true));
    let service = Service::new(Owner::Pending, false);
    let held = hold_until_changed(
        &wake,
        Some(&keys),
        || {},
        || {
            if keys_here() {
                *service.owner.lock().unwrap() = Owner::At([2; 32]);
            }
            Ok(holds(&service, &ownership, None, false).then_some(true))
        },
    );
    tokio::pin!(held);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), held.as_mut())
            .await
            .is_err(),
        "the record is still arriving"
    );
    // The founder admits the reader, whose service takes its Welcome in.
    pass(&founder);
    pass(&reader);
    assert!(keys_here());
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), held).await,
        Ok(Ok(())),
        "keys arriving end the wait, with nothing written to the record"
    );
}

/// The session's own document owner, with the checks the broker makes counted: one check is one
/// wake of the broker's thread, so an idle reader's cost is the count. Every answer, the expiries
/// it names included, is the service's own.
struct CountedReads {
    inner: Arc<dyn NativeDocumentReads>,
    checks: Arc<AtomicUsize>,
}

impl CountedReads {
    fn checked<T>(&self, answer: T) -> T {
        self.checks.fetch_add(1, Ordering::AcqRel);
        answer
    }
}

impl NativeDocumentReads for CountedReads {
    fn seals_documents(&self, workspace: &str) -> bool {
        self.inner.seals_documents(workspace)
    }
    fn resource_read(&self, workspace: &str, app: &str, record: &str) -> ResourceRead {
        self.inner.resource_read(workspace, app, record)
    }
    fn holds_resource(&self, workspace: &str, app: &str, record: &str) -> bool {
        self.inner.holds_resource(workspace, app, record)
    }
    fn await_arriving_document(
        &self,
        context: &NativeDocumentReadContext,
    ) -> Option<AwaitingDocument> {
        self.inner.await_arriving_document(context)
    }
    fn chain_head_answer(&self, context: &NativeDocumentReadContext) -> Option<ChainHeadAnswer> {
        self.inner.chain_head_answer(context)
    }
    fn authorize(&self, context: &NativeDocumentReadContext) -> Result<(), String> {
        self.inner.authorize(context)
    }
    fn capture(
        &self,
        context: &NativeDocumentReadContext,
        known_revision: Option<[u8; 32]>,
        maximum: usize,
    ) -> Result<NativeDocumentRead, String> {
        self.checked(self.inner.capture(context, known_revision, maximum))
    }
    fn capture_since(
        &self,
        context: &NativeDocumentReadContext,
        held: &HeldRevision,
        maximum: usize,
    ) -> Result<NativeDocumentRead, String> {
        self.checked(self.inner.capture_since(context, held, maximum))
    }
    fn unchanged(
        &self,
        context: &NativeDocumentReadContext,
        known_revision: Option<[u8; 32]>,
    ) -> Result<Option<bool>, String> {
        self.checked(self.inner.unchanged(context, known_revision))
    }
    fn next_change(
        &self,
        context: &NativeDocumentReadContext,
        after: u64,
    ) -> Result<Option<u64>, String> {
        self.inner.next_change(context, after)
    }
}

/// A workspace authority whose writes announce, as the session's does.
struct Announcing(crate::apps::test_support::WorkspaceTreeHandle);

impl crate::apps::services::WorkspaceAuthority for Announcing {
    fn with_tree(&self, read: &mut dyn FnMut(&tilo_desktop_model::Layout)) -> Result<(), String> {
        read(&self.0.read().desktop);
        Ok(())
    }

    fn announces_policy_changes(&self) -> bool {
        true
    }
}

/// A read of a sealed record the session's own workspace service holds, with nothing written to
/// it and no expiry near, waits on the signals it was given: a write reaching the record, a key
/// arriving, or an announced policy change. It does not ask again on a clock, so the broker
/// thread behind it stays asleep. This is the shared-workspace read an idle desktop holds open,
/// and the check that it costs no wake. Nothing else in this crate runs beside it: another
/// test's announcement is not this read's to hear.
#[test]
fn an_idle_held_read_of_a_sealed_record_asks_nothing_of_a_clock() {
    tilo_test_env::in_own_process(
        module_path!(),
        "an_idle_held_read_of_a_sealed_record_asks_nothing_of_a_clock",
        || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("a runtime for the host and the broker's local set")
                .block_on(an_idle_held_read_body());
        },
    );
}

#[allow(
    clippy::future_not_send,
    reason = "the broker's channel handles are Rc-based and this future runs only on a LocalSet"
)]
async fn an_idle_held_read_body() {
    const APP: &str = "so.tilo.notes";
    const RECORD: &str = "note-1";
    use crate::apps::route::CrdtRoute;
    use crate::apps::test_support::{Workspace, WorkspaceTree};
    use crate::workspaces::fixtures::{governor, test_host};
    use std::time::Instant;

    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(scratch.path(), &key, &cert).await;
    let created = server.workspaces().create("Held").await.unwrap();
    let workspace = created.workspace;
    server
        .workspaces()
        .author_workspace_document(&workspace, &cert, (APP, RECORD), |doc| {
            doc.put(automerge::ROOT, "text", "idle")
                .map_err(|error| error.to_string())
        })
        .await
        .unwrap();

    let checks = Arc::new(AtomicUsize::new(0));
    let reads = Arc::new(CountedReads {
        inner: server.host().document_reads(),
        checks: Arc::clone(&checks),
    });
    let route = CrdtRoute::record(APP, workspace.clone(), RECORD);
    let context = NativeDocumentReadContext::for_reader(route.clone(), cert.clone());
    let NativeDocumentRead::Captured { revision, .. } =
        reads.capture(&context, None, 1 << 20).unwrap()
    else {
        panic!("the service holds the record whole");
    };
    // What the idle wait costs, from here: the capture that learned the revision is not a wake.
    checks.store(0, Ordering::Release);

    // The workspace as the broker decides it: this device is its owner, its reads announced.
    let mut held = Workspace::new(
        crate::apps::test_support::WorkspaceId::parse(&workspace).expect("a workspace id"),
        "Held",
    );
    held.set_owner(tilo_broker_core::person_id_for_root(cert.user_public_key));
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(held);
    let membership = tree.shared();
    let authority: crate::apps::services::WorkspaceAuthorityHandle =
        Arc::new(Announcing(membership.clone()));
    let (state, bridge) = super::super::AppBrokerState::test_state(
        Some(Arc::clone(&authority)),
        crate::apps::services::ProcessServices {
            committed_documents: Some(reads.clone() as Arc<dyn NativeDocumentReads>),
            ..crate::apps::services::ProcessServices::default()
        },
    );
    state.ensure_channel(&route.channel_key());
    let mut admitted = super::super::admit_verified_join(
        &membership.read(),
        &route,
        super::super::VerifiedJoinIdentity::of(&cert, false),
    )
    .expect("the owner's device is admitted to its workspace");
    admitted.certificate = Some(cert.clone());
    let peer_id = state.next_peer_id();
    let channel = Rc::new(super::super::JoinedChannel {
        committed_read: Rc::new(std::cell::RefCell::new(super::Slot::default())),
        state,
        directories: Rc::new(super::super::LocalActionDirectoryRegistry::new(bridge)),
        channel_id: route.channel_key(),
        peer_id,
        connection_id: peer_id,
        verified_identity: admitted,
        resource_type: None,
        route_access: super::super::RouteAccess::Own,
        lease: None,
        _hold: None,
    });

    // What the held wait made of it over two seconds with nothing written: the first check, and
    // nothing after it until a signal.
    let ended = tokio::task::LocalSet::new()
        .run_until(async move {
            let started = Instant::now();
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                channel.await_committed_change(Some(revision)),
            )
            .await;
            started.elapsed()
        })
        .await;
    assert!(
        ended >= Duration::from_millis(1900),
        "the read held, as an unchanged record should: {ended:?}"
    );
    let asked = checks.load(Ordering::Acquire);
    assert!(
        asked <= 2,
        "the idle held read asked its owner {asked} times in two seconds: a read waiting on what \
         announces its change asks once, not on a clock"
    );
}
