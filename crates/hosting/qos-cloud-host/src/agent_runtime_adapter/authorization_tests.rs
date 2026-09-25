//! Broker certificate verdicts: an idle workspace revalidates every live
//! worker on each RPC; the owner must answer that from one locked proof per
//! Store change / `STORE_RECHECK`, and never across a Store change or a
//! roster/revocation time boundary.
use super::*;
use qos_action_broker::collab_broker::CrdtRoute;
use qos_action_broker::peer_actions::CertificateAuthority;
use qos_desktop::session_commands::DesktopCommand;
use qos_peer_state::desktop_command::{BoundCommand, CommandSource};
use qos_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use std::time::{Duration, Instant};

thread_local! {
    /// Store opens through `RuntimeOwner::store_for` on this test thread.
    pub(super) static OPENS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}
fn opens() -> u64 {
    OPENS.with(std::cell::Cell::get)
}

fn open_notes(store: &mut Store, caller: &DeviceCert) {
    let heads = store
        .peer()
        .unwrap()
        .admitted_room()
        .unwrap()
        .clone()
        .get_heads()
        .iter()
        .map(|head| head.0)
        .collect();
    let request = OwnerCommitRequest {
        room: store.state.room.clone(),
        operation: CommitOperation::DesktopCommand {
            invocation: Uuid::new_v4(),
            caller: caller.clone(),
            command: BoundCommand {
                source: CommandSource {
                    room: store.state.room.clone(),
                    heads,
                },
                command: DesktopCommand::OpenApp {
                    app: "so.tilo.notes".into(),
                },
            },
        },
    };
    store.commit_desktop_command(&request, None).unwrap();
}

struct Workspace {
    _directory: tempfile::TempDir,
    owner: Arc<RuntimeOwner>,
    person: DeviceCert,
    host: DeviceCert,
    room: Uuid,
    routes: Vec<CrdtRoute>,
}

/// A selected Cloud owner with `records` real Notes records, each the route
/// of one host worker.
fn workspace(records: usize) -> Workspace {
    let (directory, owner) = crate::agent_workers::tests::selected();
    let mut store = owner.store().unwrap();
    let peer = store.peer().unwrap();
    let host = peer.authority().clone();
    let person = peer
        .certificates()
        .into_iter()
        .find(|cert| cert.device_public_key != store.key().verifying_key().to_bytes())
        .unwrap();
    for _ in 0..records {
        open_notes(&mut store, &person);
    }
    let room: Uuid = store.state.room.parse().unwrap();
    let routes = store
        .peer()
        .unwrap()
        .rows()
        .into_iter()
        .filter(|row| row.app_id == "so.tilo.notes")
        .map(|row| CrdtRoute::record(&row.app_id, &store.state.room, &row.record_id))
        .collect::<Vec<_>>();
    assert_eq!(routes.len(), records);
    drop(store);
    Workspace {
        _directory: directory,
        owner,
        person,
        host,
        room,
        routes,
    }
}

/// One idle revalidation pass: every worker session and the room admission.
fn pass(w: &Workspace) -> Result<(), String> {
    for route in &w.routes {
        w.owner.authorize_host_worker(route, &w.host)?;
    }
    CertificateAuthority::authorize(&*w.owner, w.room, &w.person)
}

#[test]
fn agent_authorization_idle_workers_share_one_store_open_per_recheck() {
    const WORKERS: usize = 67;
    let w = workspace(WORKERS);
    // Warm-up: each new key needs its own first proof.
    pass(&w).unwrap();
    let before = opens();
    let began = Instant::now();
    let mut passes = 0;
    // 20 revalidations of every worker per second, across > 2 recheck intervals.
    while began.elapsed() < STORE_RECHECK_SPAN {
        pass(&w).unwrap();
        passes += 1;
        std::thread::sleep(Duration::from_millis(50));
    }
    let seconds = began.elapsed().as_secs_f64();
    let opened = opens() - before;
    let calls = passes * (WORKERS + 1);
    // One locked re-proof per STORE_RECHECK for all keys (plus one of slack).
    let bound = (seconds / store_hint::STORE_RECHECK.as_secs_f64()).ceil() as u64 + 1;
    assert!(
        opened <= bound,
        "{opened} Store opens for {calls} verdicts in {seconds:.1}s (bound {bound})"
    );
    assert!(
        (opened as f64) / seconds < 2.0,
        "{opened} opens in {seconds:.1}s"
    );
}
const STORE_RECHECK_SPAN: Duration = Duration::from_millis(4_500);

#[test]
fn agent_authorization_store_change_reproves_every_retained_verdict() {
    let w = workspace(3);
    pass(&w).unwrap();
    let before = opens();
    pass(&w).unwrap();
    assert_eq!(opens(), before, "unchanged Store: all verdicts retained");
    // Any commit changes the Store stamp: the next verdict is proved again
    // under the lock, once for every retained key.
    open_notes(&mut w.owner.store().unwrap(), &w.person);
    let before = opens();
    pass(&w).unwrap();
    assert_eq!(
        opens(),
        before + 1,
        "one locked re-proof after a Store change"
    );
    // A route whose record does not exist is refused and never retained.
    let absent = CrdtRoute::record("so.tilo.notes", &w.room.to_string(), "tab:ffffffffffffffff");
    let before = opens();
    for _ in 0..2 {
        assert!(w.owner.authorize_host_worker(&absent, &w.host).is_err());
    }
    assert_eq!(opens(), before + 2, "a refusal is proved on every call");
}

fn revoke(w: &Workspace, device: [u8; 32], at_ns: u64) {
    use qos_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};
    let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let mut entry = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: device,
        revoked_at_ns: at_ns,
        reason: RevocationReason::Lost,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &root).unwrap();
    w.owner
        .store()
        .unwrap()
        .receive(
            qos_peer_state::TransportOrigin::RelayDelta(w.person.device_public_key),
            qos_peer_state::NeighbourMsg::IdentityHello {
                cert: Box::new(w.person.clone()),
                revocations: vec![entry],
            },
            qos_relay::clock::now_ns(),
        )
        .unwrap();
}

#[test]
fn agent_authorization_committed_revocation_refuses_every_retained_worker() {
    let w = workspace(3);
    pass(&w).unwrap();
    pass(&w).unwrap();
    revoke(&w, w.host.device_public_key, qos_relay::clock::now_ns());
    for route in &w.routes {
        assert!(w.owner.authorize_host_worker(route, &w.host).is_err());
    }
}

#[test]
fn agent_authorization_verdict_expires_at_a_future_revocation_instant() {
    // The peer applies a received revocation at once, so a scheduled one is
    // exercised on the proof itself: every batch-retained verdict carries the
    // `Verified` taken under the lock, which ends at the next roster/revocation
    // instant of that Store state even when no Store file changes.
    let w = workspace(1);
    pass(&w).unwrap();
    let store = w.owner.store().unwrap();
    let now = store.policy_now_ns().unwrap();
    let at = now + 1_000_000_000;
    let mut state = store.state.clone();
    let mut entry = state
        .bound
        .as_ref()
        .unwrap()
        .revocations
        .first()
        .cloned()
        .unwrap_or(qos_user_identity::RevocationEntry {
            user_public_key: w.host.user_public_key,
            revoked_device_public_key: w.host.device_public_key,
            revoked_at_ns: at,
            reason: qos_user_identity::RevocationReason::Lost,
            signature: [0; 64],
        });
    entry.revoked_at_ns = at;
    state.bound.as_mut().unwrap().revocations.push(entry);
    let verified = store_hint::Verified::under_lock(&w.owner.root, &state, now).unwrap();
    drop(store);
    assert!(verified.current(&w.owner.root, at - 1));
    assert!(
        !verified.current(&w.owner.root, at),
        "no reuse at the revocation instant"
    );
    // And the owner's retained verdicts are dropped with the Store change.
    open_notes(&mut w.owner.store().unwrap(), &w.person);
    let before = opens();
    pass(&w).unwrap();
    assert_eq!(opens(), before + 1);
}
