//! A device that would not run a request itself asks the workspace's other
//! devices whether they would, over the same connection it would ask them
//! for an action: each answers from its own state, the requester ranks the
//! answers, and a round nobody answers leaves the request where it was
//! asked.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
tilo_automerge_isolate::test_worker!();

mod common;

use common::{addr, device, localhost_endpoint, workspace};

use ed25519_dalek::SigningKey;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr};
use neighbour_wire::offer::{Answer, Decline, Power, Query, Route, State, Work};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tilo_actions::InvocationId;
use tilo_actions::remote::TestHost;
use tilo_peer_state::actions::{ACTIONS_ALPN, ActionPlane, OFFER_DEADLINE, Offering};
use tilo_peer_state::direct::Replica;
use tilo_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};
use web_time::SystemTime;

const WORKSPACE: &str = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
const ELSEWHERE: &str = "ffffffff-ffff-4fff-8fff-ffffffffffff";
const APP: &str = "so.tilo.notes";

/// How long a test waits for a round before it counts as lost.
const PATIENCE: Duration = Duration::from_secs(20);

/// The choice a round of `targets` came to, waited for.
async fn asked(
    plane: &ActionPlane,
    asker: &Endpoint,
    targets: Vec<EndpointAddr>,
    within: Duration,
) -> tilo_peer_state::placement::Choice {
    asked_about(plane, asker, targets, &work(), within).await
}

/// The choice a round about `work` came to, waited for.
async fn asked_about(
    plane: &ActionPlane,
    asker: &Endpoint,
    targets: Vec<EndpointAddr>,
    work: &Work,
    within: Duration,
) -> tilo_peer_state::placement::Choice {
    tokio::time::timeout(
        PATIENCE,
        plane.offers(
            asker,
            targets,
            WORKSPACE,
            &route(),
            work,
            InvocationId::new(),
            within,
        ),
    )
    .await
    .expect("the round finishes")
    .expect("the round completes")
}

/// A device that answers offers with what the test tells it to say of
/// itself.
struct Reported(Answer);

impl Offering for Reported {
    fn report(&self, _from: [u8; 32], _offer: &Query) -> Answer {
        self.0.clone()
    }
}

/// A device that answers offers: its workspace replica, its plane, and the
/// router that serves the plane's connection.
struct Engine {
    key: SigningKey,
    endpoint: Endpoint,
    router: Router,
}

impl Engine {
    async fn serving(replica: Replica, key: SigningKey, reported: Option<Answer>) -> Self {
        let endpoint = localhost_endpoint(&key).await;
        let plane = ActionPlane::new(key.clone(), Arc::new(TestHost::new()));
        plane.hold(replica);
        if let Some(state) = reported {
            plane.offering(Reported(state));
        }
        let router = Router::builder(endpoint.clone())
            .accept(ACTIONS_ALPN, plane)
            .spawn();
        Self {
            key,
            endpoint,
            router,
        }
    }

    fn addr(&self) -> EndpointAddr {
        addr(&self.endpoint)
    }

    fn device(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    async fn close(self) {
        self.router.shutdown().await.unwrap();
        self.endpoint.close().await;
    }
}

/// What a device says of itself when it would run the route: `load` and
/// `latency` are the test's to vary.
fn willing(load_permille: u32) -> State {
    State {
        requester: false,
        online: true,
        holds_workspace: true,
        app_installed: true,
        store_role: false,
        data_saving: false,
        critical_battery: false,
        priority: 0,
        power: Power::External,
        load_permille,
        free_slots: Some(1),
        latency_ms: 0,
    }
}

fn route() -> Route {
    Route::new(APP, "notes.root", "insert")
}

/// `state` as the answer a device sends, holding none of the job's inputs.
fn answer(state: State) -> Answer {
    Answer {
        state,
        holds: Vec::new(),
    }
}

/// `state` as the answer of a device that already holds `idle`'s inputs.
fn holding(state: State, inputs: &[([u8; 32], u64)]) -> Answer {
    Answer {
        state,
        holds: inputs.iter().map(|(digest, _)| *digest).collect(),
    }
}

/// The work an offer is about: one small input, and a run here it could
/// beat.
fn work() -> Work {
    Work {
        inputs: vec![([7; 32], 1_000)],
        input_bytes: 1_000,
        local_wall_ms: Some(10_000),
        offload: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_device_that_reports_the_lowest_cost_wins_and_the_rest_say_why() {
    let root = SigningKey::from_bytes(&[0x51; 32]);
    let here = device(&root, 0x52);
    let busy = device(&root, 0x53);
    let idle = device(&root, 0x54);
    let roster = vec![here.1.clone(), busy.1.clone(), idle.1.clone()];
    let peer = workspace(WORKSPACE, &here, &roster);
    let replica = Replica::new(peer);

    // The busy device holds the workspace but reports a full pipeline; the
    // idle one holds nothing to do.
    let busy = Engine::serving(replica.clone(), busy.0.clone(), Some(answer(willing(750)))).await;
    let idle = Engine::serving(replica.clone(), idle.0.clone(), Some(answer(willing(0)))).await;
    let asker = localhost_endpoint(&here.0).await;
    let plane = ActionPlane::new(here.0.clone(), Arc::new(TestHost::new()));
    plane.hold(replica.clone());
    let targets = vec![busy.addr(), idle.addr()];

    let started = Instant::now();
    let choice = asked(&plane, &asker, targets, OFFER_DEADLINE).await;

    assert_eq!(
        choice.worker,
        Some(idle.device()),
        "the device with nothing in hand wins: {choice}"
    );
    assert_eq!(choice.considered.len(), 2, "both devices were asked");
    assert_eq!(
        choice
            .considered
            .iter()
            .map(|candidate| candidate.device)
            .collect::<Vec<_>>(),
        vec![idle.device(), busy.device()],
        "willing devices best first: {choice}"
    );
    let rank = choice.rank.expect("the winner's rank is kept");
    assert_eq!(rank.load_permille, 0, "it won on its own load");
    assert!(
        started.elapsed() < OFFER_DEADLINE,
        "a round of reachable devices answers inside the deadline"
    );

    busy.close().await;
    idle.close().await;
    asker.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nobody_offering_leaves_the_run_where_it_was_asked() {
    let root = SigningKey::from_bytes(&[0x61; 32]);
    let here = device(&root, 0x62);
    let store = device(&root, 0x63);
    let roster = vec![here.1.clone(), store.1.clone()];
    let peer = workspace(WORKSPACE, &here, &roster);
    let replica = Replica::new(peer);

    // The only other device is a store of the workspace: it keeps ciphertext
    // and runs nothing.
    let mut store_only = willing(0);
    store_only.store_role = true;
    let store = Engine::serving(replica.clone(), store.0.clone(), Some(answer(store_only))).await;
    let asker = localhost_endpoint(&here.0).await;
    let plane = ActionPlane::new(here.0.clone(), Arc::new(TestHost::new()));

    let choice = asked(&plane, &asker, vec![store.addr()], OFFER_DEADLINE).await;
    assert!(choice.is_local(), "a store never takes work: {choice}");
    assert_eq!(
        choice.considered.first().map(|candidate| candidate.verdict),
        Some(Err(Decline::StoreOnly))
    );
    assert_eq!(choice.to_string(), {
        let short = hex::encode(&store.device()[..4]);
        format!("no device offered to run it; {short}: store_only")
    });

    store.close().await;
    asker.close().await;
}

/// A device that does not answer — it was never reachable — costs the round
/// no more than the deadline, and is ranked as away.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_that_is_away_is_ranked_as_away_inside_the_deadline() {
    let root = SigningKey::from_bytes(&[0x71; 32]);
    let here = device(&root, 0x72);
    let away = device(&root, 0x73);
    let roster = vec![here.1.clone(), away.1.clone()];
    let peer = workspace(WORKSPACE, &here, &roster);
    let replica = Replica::new(peer);

    // A device address nothing listens on: the port was bound and closed.
    let gone = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = gone.local_addr().unwrap().port();
    drop(gone);
    let unreachable =
        EndpointAddr::new(iroh::EndpointId::from_bytes(&away.1.device_public_key).unwrap())
            .with_ip_addr(std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                port,
            )));

    let asker = localhost_endpoint(&here.0).await;
    let plane = ActionPlane::new(here.0, Arc::new(TestHost::new()));
    plane.hold(replica);

    let started = Instant::now();
    let choice = asked(
        &plane,
        &asker,
        vec![unreachable],
        Duration::from_millis(500),
    )
    .await;
    assert!(choice.is_local());
    assert_eq!(
        choice.considered.first().map(|candidate| candidate.verdict),
        Some(Err(Decline::Offline))
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "a device that is not there fails fast: {:?}",
        started.elapsed()
    );

    asker.close().await;
}

/// A device the asking device may not be told about — it is not in the
/// workspace — answers nothing, so nothing about it reaches the round.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_asked_from_outside_the_workspace_answers_nothing() {
    let root = SigningKey::from_bytes(&[0x81; 32]);
    let member = device(&root, 0x82);
    let stranger = device(&root, 0x83);
    let roster = vec![member.1.clone()];
    let peer = workspace(WORKSPACE, &member, &roster);
    let replica = Replica::new(peer);

    let answering = Engine::serving(replica, member.0.clone(), Some(answer(willing(0)))).await;
    let asker = localhost_endpoint(&stranger.0).await;
    let plane = ActionPlane::new(stranger.0.clone(), Arc::new(TestHost::new()));

    let choice = asked(&plane, &asker, vec![answering.addr()], OFFER_DEADLINE).await;
    assert!(choice.is_local(), "no answer came: {choice}");
    assert_eq!(
        choice.considered.first().map(|candidate| candidate.verdict),
        Some(Err(Decline::Offline))
    );

    answering.close().await;
    asker.close().await;
}

/// The devices a round may ask are the workspace's own roster: certified,
/// unrevoked and not this device.
#[tokio::test]
async fn the_devices_a_round_asks_are_the_workspaces_own() {
    let root = SigningKey::from_bytes(&[0x91; 32]);
    let here = device(&root, 0x92);
    let other = device(&root, 0x93);
    let revoked = device(&root, 0x94);
    let roster = vec![here.1.clone(), other.1.clone(), revoked.1.clone()];
    let peer = workspace(WORKSPACE, &here, &roster);
    let mut entry = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: revoked.1.device_public_key,
        revoked_at_ns: 1,
        reason: RevocationReason::Retired,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &root).unwrap();
    let replica = Replica::new(peer);
    assert!(
        replica
            .write(|peer| peer.governance_mut().publish_governed_revocations(
                &[entry],
                &here.0,
                SystemTime::now()
            ))
            .unwrap()
    );
    let plane = ActionPlane::new(here.0, Arc::new(TestHost::new()));
    plane.hold(replica);

    assert_eq!(
        plane.participants(WORKSPACE).await,
        vec![other.1.device_public_key],
        "the device that asked and the revoked one are never asked"
    );
    assert!(
        plane.participants(ELSEWHERE).await.is_empty(),
        "a workspace this device does not hold has nobody to ask"
    );
}

/// A device with no offering says its app is not installed: it never claims
/// work it cannot run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_with_nothing_to_say_offers_nothing() {
    let root = SigningKey::from_bytes(&[0xa1; 32]);
    let here = device(&root, 0xa2);
    let quiet = device(&root, 0xa3);
    let roster = vec![here.1.clone(), quiet.1.clone()];
    let peer = workspace(WORKSPACE, &here, &roster);
    let replica = Replica::new(peer);

    let quiet = Engine::serving(replica, quiet.0.clone(), None).await;
    let asker = localhost_endpoint(&here.0).await;
    let plane = ActionPlane::new(here.0.clone(), Arc::new(TestHost::new()));

    let choice = asked(&plane, &asker, vec![quiet.addr()], OFFER_DEADLINE).await;
    assert!(choice.is_local());
    assert_eq!(
        choice.considered.first().map(|candidate| candidate.verdict),
        Some(Err(Decline::AppNotInstalled))
    );

    quiet.close().await;
    asker.close().await;
}

/// A job whose inputs a device already holds goes there although another
/// device answers cheaper, and a job nobody holds stays where it was asked:
/// the bytes that would move decide before rank does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_jobs_inputs_decide_before_rank_does() {
    let root = SigningKey::from_bytes(&[0x61; 32]);
    let here = device(&root, 0x62);
    let near = device(&root, 0x63);
    let far = device(&root, 0x64);
    let roster = vec![here.1.clone(), near.1.clone(), far.1.clone()];
    let replica = Replica::new(workspace(WORKSPACE, &here, &roster));

    // One big input, and a run here it would have to beat.
    let mut heavy = work();
    heavy.inputs = vec![([7; 32], 32_000_000)];
    heavy.input_bytes = 32_000_000;
    heavy.local_wall_ms = Some(1_000);

    let near = Engine::serving(replica.clone(), near.0.clone(), Some(answer(willing(0)))).await;
    let far = Engine::serving(
        replica.clone(),
        far.0.clone(),
        Some(holding(willing(500), &heavy.inputs)),
    )
    .await;
    let asker = localhost_endpoint(&here.0).await;
    let plane = ActionPlane::new(here.0.clone(), Arc::new(TestHost::new()));
    plane.hold(replica);

    let choice = asked_about(
        &plane,
        &asker,
        vec![near.addr(), far.addr()],
        &heavy,
        OFFER_DEADLINE,
    )
    .await;
    assert_eq!(
        choice.worker,
        Some(far.device()),
        "the device holding the input needs nothing moved: {choice}"
    );
    assert_eq!(
        choice.transfer.map(|transfer| transfer.moving_bytes),
        Some(0),
        "an input the winner holds does not move: {choice}"
    );
    assert!(
        !choice
            .considered
            .iter()
            .find(|candidate| candidate.device == near.device())
            .unwrap()
            .worthwhile,
        "the nearer device would have to receive all of it: {choice}"
    );

    near.close().await;
    far.close().await;
}

/// A job heavy enough that moving it beats nothing stays where it was
/// asked, even though a device offered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_job_whose_inputs_move_slower_than_it_runs_stays_here() {
    let root = SigningKey::from_bytes(&[0x71; 32]);
    let here = device(&root, 0x72);
    let other = device(&root, 0x73);
    let roster = vec![here.1.clone(), other.1.clone()];
    let replica = Replica::new(workspace(WORKSPACE, &here, &roster));

    let mut heavy = work();
    heavy.inputs = vec![([7; 32], 64_000_000)];
    heavy.input_bytes = 64_000_000;
    heavy.local_wall_ms = Some(1_000);

    let other = Engine::serving(replica, other.0.clone(), Some(answer(willing(0)))).await;
    let asker = localhost_endpoint(&here.0).await;
    let plane = ActionPlane::new(here.0.clone(), Arc::new(TestHost::new()));

    let choice = asked_about(&plane, &asker, vec![other.addr()], &heavy, OFFER_DEADLINE).await;
    assert!(
        choice.is_local(),
        "moving the inputs would be slower: {choice}"
    );
    assert!(
        choice.to_string().contains("no faster than the run here"),
        "{choice}"
    );

    other.close().await;
}
