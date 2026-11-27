//! The broker's policy turn: the workspaces' policy and native context
//! subscribers are re-checked when an input announces a change and at the
//! first expiry the last check read, not once a second.
use super::*;
// The idle-broker test reads its thread's wakeups from /proc, so it and the
// fixtures only it uses (`RECHECK_INTERVAL`, `CountedAuthority`,
// `HeldWorkspace`) exist on Linux.
#[cfg(target_os = "linux")]
use crate::policy_schedule::RECHECK_INTERVAL;
use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};
use tilo_action_policy::{Answer, Grant, Grantee, KindSet, Lifetime, TreePath};
#[cfg(target_os = "linux")]
use tilo_identity::policy_change;

const NS: u64 = 1_000_000_000;

/// A committed tree that announces its writes, as the shell's does.
struct Announcing(crate::test_support::WorkspaceTreeHandle);

impl crate::services::WorkspaceAuthority for Announcing {
    fn with_tree(&self, read: &mut dyn FnMut(&tilo_desktop_model::Layout)) -> Result<(), String> {
        read(&self.0.read().desktop);
        Ok(())
    }

    fn announces_policy_changes(&self) -> bool {
        true
    }
}

/// A committed tree that announces its writes, as the shell's does, and keeps the wall time of
/// every read the broker makes of it: each re-check of the workspaces' policy reads the tree
/// once for the next change it waits for, so the reads are the re-checks.
#[cfg(target_os = "linux")]
struct CountedAuthority {
    tree: crate::test_support::WorkspaceTreeHandle,
    reads: Mutex<Vec<u64>>,
}

#[cfg(target_os = "linux")]
impl crate::services::WorkspaceAuthority for CountedAuthority {
    fn with_tree(&self, read: &mut dyn FnMut(&tilo_desktop_model::Layout)) -> Result<(), String> {
        self.reads.lock().unwrap().push(tilo_clock::unix_now_ns());
        read(&self.tree.read().desktop);
        Ok(())
    }

    fn announces_policy_changes(&self) -> bool {
        true
    }
}

#[cfg(target_os = "linux")]
impl CountedAuthority {
    fn reads(&self) -> Vec<u64> {
        self.reads.lock().unwrap().clone()
    }

    /// Change the tree and announce it, as the shell's write guard does.
    fn write(&self, change: impl FnOnce(&mut WorkspaceTree)) {
        change(&mut self.tree.write());
        policy_change::announce();
    }
}

/// The native membership owner of one workspace: what makes the broker re-check policy at all.
#[cfg(target_os = "linux")]
struct HeldWorkspace(String);

#[cfg(target_os = "linux")]
impl crate::services::WorkspaceMemberships for HeldWorkspace {
    fn requires_workspace(&self, workspace: &str) -> bool {
        workspace == self.0
    }
}

/// The owner of the workspaces these tests admit to.
fn owner_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[71; 32])
}

/// A workspace of [`owner_key`] whose guest's membership ends at `guest_until`
/// and whose agent's at `agent_until` (Unix seconds).
fn expiring_workspace(guest_until: u64, agent_until: u64) -> (WorkspaceTree, String) {
    let mut workspace = Workspace::new(WorkspaceId::from_u128(71), "Expiring");
    let workspace_id = workspace.id().to_string();
    let owner = person_id_for_root(owner_key().verifying_key().to_bytes());
    workspace.policy.set_owner(owner.clone());
    let reads = |id: &str, subject: Grantee, until: u64| {
        Grant::new(
            id,
            subject,
            TreePath::workspace(&workspace_id),
            KindSet::READ,
            Answer::Allow,
            Grantee::person(owner.clone()),
        )
        .with_lifetime(Lifetime::Until(until.saturating_mul(1000)))
    };
    workspace
        .policy
        .grant(reads("guest-reads", Grantee::person("guest"), guest_until));
    workspace.policy.grant(reads(
        "scribe-reads",
        Grantee::agent("scribe", "guest"),
        agent_until,
    ));
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    (tree, workspace_id)
}

tilo_test_env::scenario! {
    fn the_policy_turn_wakes_for_the_earliest_policy_change;

    phase the_turn_parks_without_a_deadline_and_wakes_for_the_earliest;
    {
        let now = std::time::Instant::now();
        let soon = now.checked_add(Duration::from_millis(30)).unwrap();
        let later = now.checked_add(Duration::from_secs(1)).unwrap();
        assert_eq!(broker_turn_deadline(None, None), None, "nothing due: park");
        assert_eq!(broker_turn_deadline(Some(later), Some(soon)), Some(soon));
        assert_eq!(broker_turn_deadline(Some(soon), Some(later)), Some(soon));
        assert_eq!(broker_turn_deadline(None, Some(later)), Some(later));
    }

    phase the_next_policy_change_is_the_first_grant_or_certificate_expiry;
    {
        let now = tilo_clock::unix_now();
        let (tree, workspace_id) = expiring_workspace(now.saturating_add(30), now.saturating_add(10));
        let owner = owner_key();
        let route = CrdtRoute::record("so.tilo.chat", &workspace_id, "main");
        let mut admitted = admit_verified_join(
            &tree,
            &route,
            VerifiedJoinIdentity {
                user_public_key: owner.verifying_key().to_bytes(),
                device_public_key: [72; 32],
                capped: false,
            },
        )
        .expect("the owner's workspace admits a verified person");
        let authority = Arc::new(Announcing(tree.shared()));
        let (state, _bridge) = AppBrokerState::test_state(Some(authority), ProcessServices::default());
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
            issuer: None,
        });
        let _context = state.subscribe_context(
            &route.channel_key(),
            1,
            Some(NativeContext { admitted }),
        );
        assert_eq!(
            state.next_policy_change(after).unwrap(),
            (Some(certificate_ends), false),
            "no certificate authority is installed, so no fleet is read"
        );
    }
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
        || {
            let process = tilo_clock::testing::OwnProcess::of_a_test_in_its_own_process();
            let start = tilo_clock::unix_now();
            let _held = tilo_clock::testing::hold_wall_everywhere(
                process,
                std::time::UNIX_EPOCH
                    .checked_add(Duration::from_secs(start))
                    .unwrap(),
            );
            an_idle_broker_rechecks_at_an_expiry_and_on_a_revocation(process, start);
        },
    );
}

#[cfg(target_os = "linux")]
fn an_idle_broker_rechecks_at_an_expiry_and_on_a_revocation(
    process: tilo_clock::testing::OwnProcess,
    start: u64,
) {
    let directory = tempfile::tempdir().expect("temporary socket directory");
    super::tests::secure_socket_parent(directory.path()).expect("private socket directory");
    let socket = directory.path().join("policy.sock");
    // Far enough ahead that the policy clock's own timer, which follows the
    // real wall clock, never fires during the test.
    let expiry = start.saturating_add(3600);
    let agent_expiry = start.saturating_add(7200);
    let (tree, workspace) = expiring_workspace(expiry, agent_expiry);
    let authority = Arc::new(CountedAuthority {
        tree: tree.shared(),
        reads: Mutex::new(Vec::new()),
    });
    let services = ProcessServices {
        workspace_memberships: Some(Arc::new(HeldWorkspace(workspace)) as Arc<_>),
        ..ProcessServices::default()
    };
    let membership: crate::services::WorkspaceAuthorityHandle = authority.clone();
    // The kernel's 15-byte name of `tilo-collab-broker`.
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
        policy_change::timers_reliable(),
        "Linux times expiries with a timerfd"
    );
    let registered =
        |at: u64| crate::policy_schedule::announced_at().contains(&at.saturating_mul(NS));
    tilo_test_env::wait_until("the broker registered the grant's expiry", || {
        registered(expiry)
    });

    // Idle with nothing due: the thread sleeps through what a one-second
    // clock would have woken it for twice. The expiry is registered in the
    // middle of the broker's turn, which then still runs to its end and
    // parks; on a loaded machine that rest of the turn can be preempted any
    // number of times. So the window opens once the thread has parked and
    // closes once it is parked again, and counts only what woke it between.
    let epoch = policy_change::epoch();
    let settled = tilo_test_env::idle::asleep_switches(&tid);
    std::thread::sleep(RECHECK_INTERVAL.saturating_mul(2));
    let woke = tilo_test_env::idle::asleep_switches(&tid).saturating_sub(settled);
    // Each announcement costs a turn of about two switches.
    let announced = policy_change::epoch().wrapping_sub(epoch);
    assert!(
        woke <= announced.saturating_mul(2),
        "the idle broker thread woke {woke} times with {announced} announcements"
    );

    // The grant ends, and the policy clock announces it: one re-check, at
    // the expiry, which registers the next one.
    let before_expiry = authority.reads().len();
    let _held = tilo_clock::testing::hold_wall_everywhere(
        process,
        std::time::UNIX_EPOCH
            .checked_add(Duration::from_secs(expiry))
            .unwrap(),
    );
    policy_change::announce();
    tilo_test_env::wait_until("the re-check at the expiry", || {
        authority.reads().len() > before_expiry
    });
    assert_eq!(
        authority.reads()[before_expiry],
        expiry.saturating_mul(NS),
        "the broker re-checked when the grant expired"
    );
    tilo_test_env::wait_until("the broker registered the next expiry", || {
        registered(agent_expiry)
    });

    // A revocation, announced: re-checked on the announcement, not on a
    // clock (the wall clock stands still).
    let checks = authority.reads().len();
    authority.write(|tree| tree.workspaces[0].policy.owner = None);
    tilo_test_env::wait_until("the re-check of the announced revocation", || {
        authority.reads().len() > checks
    });
}

/// The broker a daemon runs over `workspaces`, its socket in `directory`, and its thread's id.
#[cfg(target_os = "linux")]
fn serving_daemon(
    directory: &std::path::Path,
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    authority: &crate::services::WorkspaceAuthorityHandle,
) -> (crate::collab_broker::BrokerHandle, String) {
    super::tests::secure_socket_parent(directory).expect("private socket directory");
    let services = ProcessServices {
        workspace_memberships: Some(tilo_workspace_service::workspaces::ProfileMemberships::new(
            workspaces.clone(),
            Arc::clone(authority),
        ) as Arc<_>),
        ..ProcessServices::default()
    };
    // The kernel's 15-byte name of `tilo-collab-broker`.
    tilo_test_env::idle::spawned_thread("tilo-collab-bro", || {
        crate::collab_broker::spawn_with_test_services(
            directory.join("policy.sock"),
            Some(Arc::clone(authority)),
            PeerPublisher::disabled(),
            None,
            services,
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        )
        .expect("test broker")
    })
}

/// The daemon's broker over the workspaces its device hosts, wired as the daemon wires it: a
/// person a grant lets read a workspace keeps the window it opened there only while the grant
/// stands. Idle, the broker sleeps through what a one-second clock would wake it for; when the
/// owner takes the grant back, the change is announced and the window is dropped on that, sooner
/// than any one-second re-check could have.
#[cfg(target_os = "linux")]
#[test]
fn a_revoked_grant_drops_the_window_it_let_in_on_the_announcement() {
    use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;
    use tilo_workspace_service::workspaces::fixtures;
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let state_root = tempfile::tempdir().unwrap();
    let (key, cert) = fixtures::hosting_device();
    let host = runtime.block_on(fixtures::test_host(state_root.path(), &key, &cert));
    let workspaces = host.workspaces().clone();
    let workspace = runtime
        .block_on(workspaces.create("Studio"))
        .expect("the device founds a workspace")
        .workspace;
    let (_, guest) = fixtures::guest_device();
    let grant = Grant::new(
        "guest-reads",
        Grantee::person(person_id_for_root(guest.user_public_key)),
        TreePath::workspace(&workspace),
        KindSet::READ,
        Answer::Allow,
        Grantee::person(person_id_for_root(cert.user_public_key)),
    );
    let change = |change: tilo_action_policy::Change| {
        runtime
            .block_on(workspaces.commit_own_change(
                &workspace,
                WorkspaceMembershipCommand::Member { change },
                None,
            ))
            .expect("the owner changes their workspace's grants");
    };
    change(tilo_action_policy::Change::Grant(grant.clone()));

    let directory = tempfile::tempdir().expect("temporary socket directory");
    let authority: crate::services::WorkspaceAuthorityHandle = Arc::new(workspaces.clone());
    let (broker, tid) = serving_daemon(directory.path(), &workspaces, &authority);
    let state = Arc::clone(broker.app_action_broker().unwrap().state());

    // The guest's window on a record of the workspace, admitted for the grant.
    let route = CrdtRoute::record("so.tilo.chat", &workspace, "main");
    let now = tilo_clock::unix_now();
    let mut admitted = authority
        .decide_in_workspace(&workspace, |held| {
            admit_verified_join_at(held, &route, VerifiedJoinIdentity::of(&guest, false), now)
        })
        .unwrap()
        .expect("the grant lets the guest in");
    admitted.certificate = Some(guest);
    let native = state
        .native_roster_for_subscriber(&route, &admitted)
        .unwrap()
        .expect("the device hosts the workspace");
    let mut window = state.subscribe_context(&route.channel_key(), 1, Some(native));
    tilo_test_env::wait_until("the broker's turn for the new window", || {
        state.native_context_targets().unwrap().len() == 1
            && matches!(window.try_recv(), Ok(ContextUpdate::RecordsMoved(_)))
    });

    // Idle: no clock wakes the broker, only announcements do, each a turn of about two switches.
    let epoch = policy_change::epoch();
    let settled = tilo_test_env::idle::asleep_switches(&tid);
    std::thread::sleep(RECHECK_INTERVAL.saturating_mul(2));
    let woke = tilo_test_env::idle::asleep_switches(&tid).saturating_sub(settled);
    let announced = policy_change::epoch().wrapping_sub(epoch);
    assert!(
        woke <= announced.saturating_mul(2),
        "the idle broker thread woke {woke} times with {announced} announcements"
    );

    // The owner takes the grant back: the window is dropped on the announcement.
    let revoked = std::time::Instant::now();
    change(tilo_action_policy::Change::Revoke(grant.id));
    tilo_test_env::wait_until("the window dropped", || {
        loop {
            match window.try_recv() {
                Ok(_) => {}
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break false,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break true,
            }
        }
    });
    let took = revoked.elapsed();
    assert!(
        took < RECHECK_INTERVAL,
        "the revocation took {took:?} to drop the window: a clock's re-check, not the announcement"
    );
    assert!(state.native_context_targets().unwrap().is_empty());
    drop(state);
    drop(broker);
    runtime.block_on(host.shutdown());
}
