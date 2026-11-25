//! A workspace moves from one server to another the way its owner moves it: the owner has the
//! new server keep it as well (`tilo workspace servers add <its line> -w W`), then the old one
//! keep it no more (`tilo workspace servers remove <old> -w W`), and logs in at the new one from
//! the computer that holds their root (`tilo login --server <its line>`).
//!
//! Both changes are signed on the computer that holds the owner's root, where the commands run:
//! no server governs the workspace, the one it was made on included.
//!
//! The owner's computer came into the workspace on the old server through the invitation that
//! server issued it when it logged in there, and that invitation expired long before the move:
//! the new server takes in the workspace's devices from its governance alone, and lets the
//! computer in on the certificate its owner's root signed, however old the invitation it first
//! came in by.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tilo_actions::remote::ActionCallError;
use tilo_peer_state::members::MembershipOpCommand;
use tilo_peer_state::workspace::GovernanceRow;
use tilo_server::daemon::{Server, ServerIdentity};
use tilo_server::workspaces::Workspaces;
use tilo_server::{ServerConfig, WorkspaceId};
use tilo_server_protocol::WorkSetting;
use tilo_server_protocol::web::{PageHost, PageHostAddress};
use tilo_user_identity::{DeviceCapabilities, RevocationList};
use tilo_workspace_service::workspaces::OwnWorkspaceOffer;

use tilo_peer_state::join::Role;

use crate::common::{
    Linked, TestDevice, TestUser, copy_says, invitation, redeem, servers_action, start_server,
    write,
};

/// How long the invitation a server issues a device logging in there stays redeemable
/// (`OWN_INVITATION_SECONDS`), and some: the clock is moved past it once the computer is in.
const PAST_THE_INVITATION: Duration = Duration::from_secs(11 * 60);

/// The process's wall clock, running as the real one does but `ahead` of it: every thread and
/// every server in the process reads it, so moving it ahead lets an invitation expire without
/// waiting for it.
struct RunningClock {
    ahead: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    ticking: Option<std::thread::JoinHandle<()>>,
}

impl RunningClock {
    fn start(process: tilo_clock::testing::OwnProcess) -> Self {
        let started_at = tilo_clock::wall();
        let started = tilo_clock::now();
        let ahead = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let held = tilo_clock::testing::hold_wall_everywhere(process, started_at);
        let ticking = {
            let (ahead, stop) = (Arc::clone(&ahead), Arc::clone(&stop));
            std::thread::spawn(move || {
                let mut held = held;
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(5));
                    let at = started_at
                        + started.elapsed()
                        + Duration::from_millis(ahead.load(Ordering::Relaxed));
                    // The new hold takes over before the one it replaces lets go, so no read
                    // between them answers the real clock.
                    let next = tilo_clock::testing::hold_wall_everywhere(process, at);
                    drop(std::mem::replace(&mut held, next));
                }
            })
        };
        Self {
            ahead,
            stop,
            ticking: Some(ticking),
        }
    }

    /// Move the clock `by` ahead, and wait until it reads so.
    fn advance(&self, by: Duration) {
        let before = tilo_clock::unix_now_ns();
        self.ahead
            .fetch_add(u64::try_from(by.as_millis()).unwrap(), Ordering::Relaxed);
        let by_ns = u64::try_from(by.as_nanos()).unwrap();
        tilo_test_env::wait_for("the clock to move ahead", || {
            (tilo_clock::unix_now_ns() >= before + by_ns).then_some(())
        });
    }
}

impl Drop for RunningClock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(ticking) = self.ticking.take() {
            let _ = ticking.join();
        }
    }
}

/// A server of a site that keeps people's own workspaces (`host`, as people know it), on a key
/// its site's root certified: a server like tilo.sh, nobody in the workspaces it keeps itself.
async fn site_server(site: &str) -> (Site, TestDevice) {
    let root = TestUser::fresh(site);
    let device = root.enrol(site, DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start_server(
        ServerConfig {
            work: WorkSetting::own_person(),
            ..ServerConfig::loopback(state.path().to_path_buf())
        },
        ServerIdentity {
            user_public_key: root.user_pk,
            device_signing_key: device.sk.clone(),
            device_cert: device.cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap_or_else(|error| panic!("{site}'s server starts: {error}"));
    let kept = Site {
        server,
        _state: state,
    };
    assert!(
        kept.workspaces()
            .offer_own_workspaces(OwnWorkspaceOffer::free(site))
            .unwrap(),
        "{site} keeps people's own workspaces"
    );
    (kept, device)
}

/// A site's server, run as its daemon runs it (it keeps the files its workspaces' records name),
/// over the state it keeps.
struct Site {
    server: Server,
    _state: tempfile::TempDir,
}

impl Site {
    fn workspaces(&self) -> &Workspaces {
        self.server
            .workspaces()
            .expect("a site's server keeps workspaces")
    }
}

/// `hosted`'s own line, as its `web-host.json` keeps it and its operator hands it over.
fn line(hosted: &Site, device: &TestDevice) -> String {
    serde_json::to_string(&PageHost {
        server_cert: device.cert.clone(),
        address: Some(PageHostAddress {
            endpoint_addr: hosted.server.node_addr(),
            relay_url: None,
            web: None,
        }),
        origin: None,
    })
    .unwrap()
}

/// The devices that signed a change to which servers keep `workspace`, as `hosted` holds it.
async fn keeping_signed_by(hosted: &Site, workspace: &str) -> Vec<[u8; 32]> {
    let state = hosted
        .workspaces()
        .state(workspace)
        .await
        .expect("the server holds the workspace");
    let doc = state
        .admitted_workspace()
        .expect("the workspace's governance is admitted");
    tilo_peer_state::members::ops(doc)
        .values()
        .filter(|op| {
            matches!(
                &op.command,
                MembershipOpCommand::Governance { rows } if rows.contains(&GovernanceRow::Hosts)
            )
        })
        .map(|op| op.author.device_public_key)
        .collect()
}

/// Whether `hosted`'s own copy of `record` carries `body`: the sealed writes it carries, opened
/// with the keys the person's devices share. A server keeps a record's writes whether or not it
/// reads them.
async fn host_says(hosted: &Site, workspace: WorkspaceId, record: &str, body: &str) -> bool {
    hosted
        .workspaces()
        .state(&workspace.to_string())
        .await
        .is_some_and(|state| copy_says(&state, record, body))
}

/// Wait until `hosted`'s own copy of `record` carries `body`.
async fn reaches(hosted: &Site, workspace: WorkspaceId, record: &str, body: &str, what: &str) {
    tilo_test_env::within(what, async {
        while !host_says(hosted, workspace, record, body).await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// The owner's computer logs in at `hosted` by its line alone, as `tilo login --server` does, and
/// the server lets it in to the workspace the login names there: the login.
async fn log_in(
    laptop: &TestDevice,
    hosted: &Site,
    device: &TestDevice,
    what: &str,
) -> tilo_device_identity::login::Device {
    let login = tilo_test_env::within(
        format!("the computer logs in at {what}"),
        tilo_api::daemon::enter::log_in_at(
            &laptop.sk,
            &laptop.cert,
            serde_json::from_str(&line(hosted, device)).unwrap(),
            None,
        ),
    )
    .await
    .unwrap_or_else(|error| panic!("the root's own computer logs in at {what}: {error:#}"));
    assert!(login.reaches_a_server(), "the login is live: {login:?}");
    login
}

#[test]
fn a_workspace_moved_to_another_server_lets_its_owner_log_in_there() {
    // The invitation the computer came in by expires while the test runs: the test takes the
    // process to itself and moves the clock past it instead of waiting.
    tilo_test_env::in_own_process(
        module_path!(),
        "a_workspace_moved_to_another_server_lets_its_owner_log_in_there",
        || {
            let process = tilo_clock::testing::OwnProcess::of_a_test_in_its_own_process();
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
                .expect("a runtime for this test alone")
                .block_on(moved(process));
        },
    );
}

#[expect(
    clippy::too_many_lines,
    reason = "one owner's move, step by step, as they make it"
)]
async fn moved(process: tilo_clock::testing::OwnProcess) {
    let clock = RunningClock::start(process);
    let alice = TestUser::fresh("Alice");
    // Alice's computer, which holds her root.
    let laptop = alice.enrol("Alice's laptop", DeviceCapabilities::primary());

    // Server A keeps the company workspace, which is Alice's.
    let (a, a_device) = site_server("a.test").await;
    let company = a
        .workspaces()
        .create_for("Company", None, alice.user_pk)
        .await
        .unwrap();

    // Her computer logs in there: the server lets it in to the workspace through its entry, and
    // its member copy (what its daemon's service keeps) links to the server from then on.
    let login = log_in(&laptop, &a, &a_device, "server A").await;
    assert_eq!(login.workspace, company.workspace);

    // Long after, that invitation has expired.
    clock.advance(PAST_THE_INVITATION);

    // The computer writes records and a file's content.
    let mut on_a = Linked::open(&a.server, &laptop, company.workspace_id)
        .await
        .expect("server A admits the computer");
    // The link brings the copy the workspace's governance, which the computer writes under: the
    // row its login signed among it, so what the copy writes as the computer follows that row.
    crate::common::receives(&mut on_a, |peer| {
        peer.permissions(laptop.device_pubkey(), tilo_clock::wall())
            .is_ok()
            && peer
                .governance()
                .governed_access(tilo_clock::wall())
                .is_ok_and(|access| access.member_devices.contains(&laptop.device_pubkey()))
    })
    .await;
    for n in 1..=3 {
        write(&on_a, &laptop, &format!("note:{n}"), &format!("note {n}"))
            .expect("the computer writes its record, sealed");
    }
    reaches(
        &a,
        company.workspace_id,
        "note:3",
        "note 3",
        "the computer's records reach server A",
    )
    .await;
    // Not covered yet: a file's content moving with the workspace. A server keeps the content a
    // record names by reading the record, so such a record is written in the clear; a server
    // that joins is caught up with the sealed writes the others keep, never a record in the
    // clear, and the server it replaces does not let go of a record it alone holds. The content
    // moves once members hand it to the servers keeping their workspace.

    // The computer keeps its copy of the workspace; its daemon's own service holds the workspace
    // from here on, the one copy its key writes.
    drop(on_a);
    let laptop_state = tempfile::tempdir().unwrap();
    let (laptop_service, laptop_keys) =
        crate::common::own_service(laptop_state.path(), &laptop).await;
    let own = laptop_service.workspaces().clone();
    // Alice's own workspace is founded on her computer, which marks itself as the device that
    // keeps her root there.
    tilo_server::servers::found(&own)
        .await
        .expect("Alice's own workspace is founded on her computer");
    tilo_workspace_service::workspaces::person_workspace::devices::mark_root_holder(
        &own,
        &alice.user_sk,
        &laptop.cert,
    )
    .await
    .expect("her computer keeps her root");
    let a_hint = a
        .workspaces()
        .hosts(&company.workspace)
        .await
        .unwrap()
        .into_iter()
        .find(|hint| hint.host == a_device.cert)
        .expect("server A names its own address");
    tilo_test_env::within(
        "the computer holds the company workspace",
        own.hold_as_member(company.workspace_id, &a_hint),
    )
    .await
    .expect("server A admits Alice's computer");

    // A server signs no change to which servers keep the workspace: asked by the owner's own
    // computer, server A refuses to, as it keeps none of her root.
    let refused = servers_action(
        a.workspaces(),
        &laptop,
        &company.workspace,
        "add",
        serde_json::json!({ "host": line(&a, &a_device) }),
    )
    .await
    .expect_err("a server changes which servers keep a workspace under no key of its own");
    assert!(
        matches!(&refused, ActionCallError::Refused { .. }),
        "{refused:?}"
    );
    // The only server keeping the workspace is not taken out before another keeps it.
    let refused = servers_action(
        &own,
        &laptop,
        &company.workspace,
        "remove",
        serde_json::json!({ "server": hex::encode(a_device.device_pubkey()) }),
    )
    .await
    .expect_err("the only server keeping the workspace is not removed");
    assert!(
        matches!(&refused, ActionCallError::Refused { .. }),
        "{refused:?}"
    );

    let founded = keeping_signed_by(&a, &company.workspace).await;

    // Server B, a site that keeps workspaces for the people its home workspace let in: Alice
    // came in by an invitation into it.
    let (b, b_device) = site_server("b.test").await;
    let home = b.workspaces().create("Lounge").await.unwrap();
    assert!(b.workspaces().admit_own_through(&home.workspace));
    let invited = invitation(
        b.workspaces(),
        &b_device,
        &home.workspace,
        Role::Editor,
        &laptop,
    )
    .await;
    redeem(&laptop, &invited)
        .await
        .expect("Alice comes into server B's home by its invitation");

    // `tilo -w Company do workspace/servers#add host=<B's line>`: B keeps it as well, and takes in
    // every record.
    servers_action(
        &own,
        &laptop,
        &company.workspace,
        "add",
        serde_json::json!({ "host": line(&b, &b_device) }),
    )
    .await
    .unwrap_or_else(|error| panic!("server B keeps the workspace as well: {error:?}"));
    for n in 1..=3 {
        reaches(
            &b,
            company.workspace_id,
            &format!("note:{n}"),
            &format!("note {n}"),
            "server B takes in the computer's records",
        )
        .await;
    }

    // `tilo -w Company do workspace/servers#remove server=<A>`: A keeps it no more.
    servers_action(
        &own,
        &laptop,
        &company.workspace,
        "remove",
        serde_json::json!({ "server": hex::encode(a_device.device_pubkey()) }),
    )
    .await
    .unwrap_or_else(|error| panic!("server A keeps the workspace no more: {error:?}"));
    tilo_test_env::within("server B to name itself alone", async {
        while b
            .workspaces()
            .hosts(&company.workspace)
            .await
            .map_or(true, |hosts| hosts.len() != 1)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;

    // The server the workspace moved to takes no part in governing it, and Alice's computer
    // signed both changes to which servers keep it.
    let at_b = b
        .workspaces()
        .state(&company.workspace)
        .await
        .expect("server B holds the workspace");
    let policy = tilo_peer_state::workspace::policy_of(
        at_b.admitted_workspace()
            .expect("the workspace's governance is admitted"),
        &company.workspace,
    );
    // Server A, which made the workspace for Alice, is still one of its administrators: the
    // computer first came in through the invitation A issued it under that grant, and a
    // computer entering its owner's workspace by its root-signed certificate alone is what
    // lets A hold none.
    let b_person = hex::encode(b_device.cert.user_public_key);
    assert!(
        !policy.is_administrator(&b_person) && policy.active_role(&b_person).is_none(),
        "the server it moved to neither administers nor takes part in the workspace"
    );
    // Every change since the one that founded the workspace on A, which A signed naming itself
    // as it made the workspace for Alice, is her computer's.
    let mut changed = keeping_signed_by(&b, &company.workspace).await;
    for founding in &founded {
        if let Some(at) = changed.iter().position(|device| device == founding) {
            changed.remove(at);
        }
    }
    assert!(
        changed.len() >= 2
            && changed
                .iter()
                .all(|device| *device == laptop.device_pubkey()),
        "the computer signed every change to the servers keeping it: {changed:?}"
    );

    // The old server is gone.
    a.server.shutdown().await;

    // `tilo login --server <B's line>`: the computer that holds Alice's root logs in at B and
    // enters the company workspace there.
    let login = log_in(&laptop, &b, &b_device, "server B").await;
    assert_eq!(
        login.workspace, company.workspace,
        "the login at B starts in the company workspace"
    );
    // B holds what the computer wrote at A, from B's own copy.
    assert!(
        host_says(&b, company.workspace_id, "note:1", "note 1").await,
        "B reads what the computer wrote at A"
    );
    // Not covered yet: the computer's daemon holding the workspace at B and writing there. B
    // administers nothing, and a further device of a member that enters by the certificate its
    // root signed, with no administrator to admit it, is refused there still ("workspace write
    // forbidden").
    drop(own);
    laptop_service.shutdown().await;
    drop(laptop_keys);
    b.server.shutdown().await;
    drop(clock);
}
