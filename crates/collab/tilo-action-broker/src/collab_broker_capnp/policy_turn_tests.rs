//! The broker's policy turn: membership surfaces and native
//! context subscribers are re-checked when an input announces a change and
//! at the first expiry the last check read, not once a second.
use super::*;
// The idle-broker test reads its thread's wakeups from /proc, so it and the
// fixtures only it uses (`RECHECK_INTERVAL`, `Announcing::write`,
// `CountedRoom`) exist on Linux.
#[cfg(target_os = "linux")]
use crate::policy_schedule::RECHECK_INTERVAL;
use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};
use tilo_action_policy::{Answer, Grant, KindSet, Lifetime, Subject, TreePath};

const NS: u64 = 1_000_000_000;

/// A committed tree that announces its writes, as the shell's does.
struct Announcing(crate::test_support::WorkspaceTreeHandle);

impl crate::services::WorkspaceAuthority for Announcing {
    fn with_tree(
        &self,
        read: &mut dyn FnMut(&tilo_desktop_model::DesktopTree),
    ) -> Result<(), String> {
        read(&self.0.read().desktop);
        Ok(())
    }

    fn announces_policy_changes(&self) -> bool {
        true
    }
}

#[cfg(target_os = "linux")]
impl Announcing {
    /// Change the tree and announce it, as the shell's write guard does.
    fn write(&self, change: impl FnOnce(&mut WorkspaceTree)) {
        change(&mut self.0.write());
        tilo_identity_host::policy_change::announce();
    }
}

/// The native membership owner of one room. Every re-check of the room's
/// membership surface asks whether it keeps the room, so the questions are
/// the re-checks.
#[cfg(target_os = "linux")]
struct CountedRoom {
    room: String,
    checks: Mutex<Vec<u64>>,
}

#[cfg(target_os = "linux")]
impl CountedRoom {
    fn checks(&self) -> Vec<u64> {
        self.checks.lock().unwrap().clone()
    }
}

#[cfg(target_os = "linux")]
impl crate::services::WorkspaceMemberships for CountedRoom {
    fn requires_room(&self, room: &str) -> bool {
        self.checks.lock().unwrap().push(tilo_clock::unix_now_ns());
        room == self.room
    }

    fn execute(&self, _: tilo_owner_commit::OwnerCommitRequest) -> tilo_owner_commit::CommitFuture {
        Box::pin(async { Err("the policy turn fixture never commits".into()) })
    }
}

/// The owner of the rooms these tests admit to.
fn owner_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[71; 32])
}

/// A room of [`owner_key`] whose guest's membership ends at `guest_until`
/// and whose agent's at `agent_until` (Unix seconds).
fn expiring_room(guest_until: u64, agent_until: u64) -> (WorkspaceTree, String) {
    let mut workspace = Workspace::new(WorkspaceId(71), "Expiring");
    let room = workspace.room().to_string();
    let owner = principal_id_for_root(owner_key().verifying_key().to_bytes());
    workspace.set_owner(owner.clone());
    let reads = |id: &str, subject: Subject, until: u64| {
        Grant::new(
            id,
            subject,
            TreePath::room(&room),
            KindSet::READ,
            Answer::Allow,
            Subject::person(owner.clone()),
        )
        .with_lifetime(Lifetime::Until(until.saturating_mul(1000)))
    };
    workspace.grant(reads("guest-reads", Subject::person("guest"), guest_until));
    workspace.grant(reads("scribe-reads", Subject::agent("scribe"), agent_until));
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    (tree, room)
}

#[test]
fn the_turn_parks_without_a_deadline_and_wakes_for_the_earliest() {
    let now = std::time::Instant::now();
    let soon = now.checked_add(Duration::from_millis(30)).unwrap();
    let later = now.checked_add(Duration::from_secs(1)).unwrap();
    assert_eq!(broker_turn_deadline(None, None), None, "nothing due: park");
    assert_eq!(broker_turn_deadline(Some(later), Some(soon)), Some(soon));
    assert_eq!(broker_turn_deadline(Some(soon), Some(later)), Some(soon));
    assert_eq!(broker_turn_deadline(None, Some(later)), Some(later));
}

#[test]
fn the_next_policy_change_is_the_first_grant_or_certificate_expiry() {
    let now = tilo_clock::unix_now();
    let (tree, room) = expiring_room(now.saturating_add(30), now.saturating_add(10));
    let owner = owner_key();
    let route = CrdtRoute::record("so.tilo.chat", &room, "main");
    let mut admitted = admit_verified_join(
        &tree,
        &route,
        VerifiedJoinIdentity {
            user_public_key: owner.verifying_key().to_bytes(),
            device_public_key: [72; 32],
            capped: false,
        },
    )
    .expect("the owner's room admits a verified person");
    let authority = Arc::new(Announcing(tree.shared()));
    let (state, _bridge) = CapnpBrokerState::new_configured(
        &Arc::new(FanoutState::new()),
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(authority),
        None,
        None,
        HostServices::default(),
    );
    let after = now.saturating_mul(NS);
    assert_eq!(
        state.next_policy_change(after).unwrap(),
        (Some(now.saturating_add(10).saturating_mul(NS)), false),
        "the agent's membership ends first"
    );
    assert_eq!(
        state
            .next_policy_change(now.saturating_add(10).saturating_mul(NS))
            .unwrap()
            .0,
        Some(now.saturating_add(30).saturating_mul(NS)),
        "then the guest's"
    );

    // An admitted native session whose certificate ends sooner still.
    let certificate_ends = after.saturating_add(NS.saturating_mul(5));
    admitted.certificate = Some(tilo_user_identity::DeviceCert {
        user_public_key: owner.verifying_key().to_bytes(),
        device_public_key: [72; 32],
        device_label: "laptop".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: after.saturating_sub(NS),
        expires_at_ns: Some(certificate_ends),
        signature: [0; 64],
    });
    let _context = state.subscribe_context(
        &route.channel_key(),
        1,
        Some(NativeContext {
            admitted,
            roster: Vec::new(),
        }),
    );
    assert_eq!(
        state.next_policy_change(after).unwrap(),
        (Some(certificate_ends), false),
        "no certificate authority is installed, so no fleet is read"
    );
}

/// An idle broker whose host announces its policy changes registers its
/// grant's expiry with the policy clock, re-checks when that announces the
/// expiry, and re-checks when a revocation is announced; in between its
/// thread is not woken. The wall clock is held, so the expiry comes when the
/// test moves it there and announces it as the policy clock would; the test
/// runs in a process of its own, so no other test reads the held time.
#[cfg(target_os = "linux")] // the thread's wakeups are read from /proc
#[test]
fn an_idle_broker_rechecks_at_an_expiry_and_on_a_revocation_and_sleeps_in_between() {
    tilo_test_env::in_own_process(
        module_path!(),
        "an_idle_broker_rechecks_at_an_expiry_and_on_a_revocation_and_sleeps_in_between",
        an_idle_broker_rechecks_at_an_expiry_and_on_a_revocation,
    );
}

#[cfg(target_os = "linux")]
fn an_idle_broker_rechecks_at_an_expiry_and_on_a_revocation() {
    let start = tilo_clock::unix_now();
    tilo_clock::freeze_wall(
        std::time::UNIX_EPOCH
            .checked_add(Duration::from_secs(start))
            .unwrap(),
    );
    let directory = tempfile::tempdir().expect("temporary socket directory");
    super::tests::secure_socket_parent(directory.path()).expect("private socket directory");
    let socket = directory.path().join("policy.sock");
    // Far enough ahead that the policy clock's own timer, which follows the
    // real wall clock, never fires during the test.
    let expiry = start.saturating_add(3600);
    let agent_expiry = start.saturating_add(7200);
    let (tree, room) = expiring_room(expiry, agent_expiry);
    let authority = Arc::new(Announcing(tree.shared()));
    let owner = Arc::new(CountedRoom {
        room,
        checks: Mutex::new(Vec::new()),
    });
    let services = HostServices {
        workspace_memberships: Some(Arc::clone(&owner) as Arc<_>),
        ..HostServices::default()
    };
    let membership: crate::services::WorkspaceAuthorityHandle = authority.clone();
    // The kernel's 15-byte name of `tilo-collab-broker-capnp`.
    let (_broker, tid) = tilo_test_env::idle::spawned_thread("tilo-collab-bro", || {
        crate::collab_broker::spawn_with_test_services(
            socket.clone(),
            Some(membership),
            PeerPublisher::disabled(),
            None,
            services,
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        )
        .expect("test broker")
    });
    assert!(
        tilo_identity_host::policy_change::timers_reliable(),
        "Linux times expiries with a timerfd"
    );
    let registered =
        |at: u64| crate::policy_schedule::announced_at().contains(&at.saturating_mul(NS));
    tilo_test_env::wait_until("the broker registered the grant's expiry", || {
        registered(expiry)
    });

    // Idle with nothing due: the thread sleeps through what a one-second
    // clock would have woken it for twice.
    let epoch = tilo_identity_host::policy_change::epoch();
    let settled = tilo_test_env::idle::context_switches(&tid);
    std::thread::sleep(RECHECK_INTERVAL.saturating_mul(2));
    let woke = tilo_test_env::idle::context_switches(&tid).saturating_sub(settled);
    // Another test of this process may announce; each costs a turn of about
    // two switches. Parking after the start's check costs at most two more.
    let announced = tilo_identity_host::policy_change::epoch().wrapping_sub(epoch);
    assert!(
        woke <= announced.saturating_mul(2).saturating_add(2),
        "the idle broker thread woke {woke} times with {announced} announcements"
    );

    // The grant ends, and the policy clock announces it: one re-check, at
    // the expiry, which registers the next one.
    let before_expiry = owner.checks().len();
    tilo_clock::freeze_wall(
        std::time::UNIX_EPOCH
            .checked_add(Duration::from_secs(expiry))
            .unwrap(),
    );
    tilo_identity_host::policy_change::announce();
    tilo_test_env::wait_until("the re-check at the expiry", || {
        owner.checks().len() > before_expiry
    });
    assert_eq!(
        owner.checks()[before_expiry],
        expiry.saturating_mul(NS),
        "the broker re-checked when the grant expired"
    );
    tilo_test_env::wait_until("the broker registered the next expiry", || {
        registered(agent_expiry)
    });

    // A revocation, announced: re-checked on the announcement, not on a
    // clock (the wall clock stands still).
    let checks = owner.checks().len();
    authority.write(|tree| tree.workspaces[0].policy.owner = None);
    tilo_test_env::wait_until("the re-check of the announced revocation", || {
        owner.checks().len() > checks
    });
}
