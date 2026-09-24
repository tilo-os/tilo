//! Qualification through the actual owner, native publication cursor, Coedit,
//! authenticated local relay, selected store, and fresh/offline StatePeer admission.
use ed25519_dalek::SigningKey;
use qos_peer_state::{StatePeer, TransportOrigin};
use qos_session_peer::{
    SessionOwner,
    host::{LocalHost, NativeSubscription, publish},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    time::{Duration, Instant},
};
use tilo_relay::{Inbound, RelayClient, RelayCoeditSession};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, RevocationList,
    persist::{self, IdentityPaths},
};
use ui::automerge::{AutoCommit, ROOT, transaction::Transactable};
use web_time::SystemTime;

fn exact(actual: &StatePeer, expected: &StatePeer) -> bool {
    actual.rows() == expected.rows()
        && actual.rows().iter().all(|row| {
            actual
                .durable_coverage(&row.app_id, &row.record_id)
                .unwrap()
                == expected
                    .durable_coverage(&row.app_id, &row.record_id)
                    .unwrap()
        })
        && actual.admitted_room().map(|r| r.clone().get_heads())
            == expected.admitted_room().map(|r| r.clone().get_heads())
}
fn fresh(owner: &SessionOwner) -> StatePeer {
    StatePeer::new(
        owner.peer().room(),
        owner.peer().authority().clone(),
        owner.peer().certificates(),
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap()
}
fn apply(peer: &mut StatePeer, author: [u8; 32], data: &[u8]) {
    let admitted = peer
        .receive_from(
            TransportOrigin::RelayDelta(author),
            postcard::from_bytes(data).unwrap(),
            SystemTime::now(),
        )
        .unwrap();
    assert_eq!(
        admitted.rejected, 0,
        "deferred replay rejected a governed frame"
    );
}
async fn native_ready(owner: &mut SessionOwner, session: &mut NativeSubscription) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !session.is_ready() {
            match session.recv().await.expect("native Ready") {
                Inbound::Delta { author, data } => {
                    owner
                        .receive(
                            TransportOrigin::RelayDelta(author),
                            postcard::from_bytes(&data).unwrap(),
                        )
                        .unwrap();
                }
                Inbound::Ready { .. } => {}
                Inbound::Presence { .. } => panic!("unexpected presence"),
            }
        }
    })
    .await
    .unwrap();
}
async fn observer(
    host: &LocalHost,
    owner: &SessionOwner,
    cert: &DeviceCert,
    key: &SigningKey,
) -> RelayCoeditSession {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::from(
            host.packet_url.clone(),
        )))
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    endpoint.online().await;
    let client = RelayClient::connect_v2_to_enrolled_home_peer(
        endpoint,
        host.address.clone(),
        cert.user_public_key,
        owner.host_certificate().device_public_key,
        cert.clone(),
        key.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    RelayCoeditSession::join(
        client,
        tilo_relay::room_workspace_id(owner.peer().room()).unwrap(),
        cert.device_public_key,
    )
    .unwrap()
}
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Route {
    Room(String),
    Document(String, String, String),
}
#[derive(Default)]
struct Output {
    frames: usize,
    bytes: usize,
    routes: BTreeMap<Route, usize>,
}
impl Output {
    fn observe(&mut self, data: &[u8]) {
        let route = match postcard::from_bytes::<qos_peer_state::NeighbourMsg>(data).unwrap() {
            qos_peer_state::NeighbourMsg::SyncResp { room, .. } => Route::Room(room),
            qos_peer_state::NeighbourMsg::AppDocument {
                cluster_id: room,
                app_id,
                record_id,
                ..
            } => Route::Document(room, app_id, record_id),
            message => panic!("unexpected owner publication: {message:?}"),
        };
        self.frames += 1;
        self.bytes += data.len();
        *self.routes.entry(route).or_default() += 1;
    }
    fn assert_full_basis(&self, owner: &SessionOwner) {
        // Independent canonical state, not outgoing publication metadata.
        let mut expected = BTreeMap::from([(Route::Room(owner.peer().room().into()), 1)]);
        for row in owner.peer().rows() {
            assert!(
                expected
                    .insert(
                        Route::Document(owner.peer().room().into(), row.app_id, row.record_id),
                        1
                    )
                    .is_none()
            );
        }
        assert_eq!(
            self.routes, expected,
            "every canonical route appears exactly once"
        );
    }
}
async fn replay(session: &RelayCoeditSession, peers: &mut [&mut StatePeer]) -> Output {
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut output = Output::default();
        loop {
            match session.recv().await.expect("observer Ready") {
                Inbound::Delta { author, data } => {
                    output.observe(&data);
                    for peer in peers.iter_mut() {
                        apply(peer, author, &data);
                    }
                }
                Inbound::Ready { .. } => return output,
                Inbound::Presence { .. } => panic!("unexpected presence"),
            }
        }
    })
    .await
    .unwrap()
}
async fn converge(
    session: &RelayCoeditSession,
    peer: &mut StatePeer,
    owner: &SessionOwner,
) -> Output {
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut output = Output::default();
        while !exact(peer, owner.peer()) {
            match session.recv().await.expect("live selected activation") {
                Inbound::Delta { author, data } => {
                    output.observe(&data);
                    apply(peer, author, &data);
                }
                _ => panic!("unexpected event while waiting for exact receipts"),
            }
        }
        output
    })
    .await
    .unwrap()
}
fn census(path: &Path) -> (usize, u64, usize, u64) {
    use std::os::unix::fs::MetadataExt;
    fn walk(
        path: &Path,
        count: &mut usize,
        bytes: &mut u64,
        unique: &mut BTreeMap<(u64, u64), u64>,
    ) {
        if !path.exists() {
            return;
        }
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                walk(&entry.path(), count, bytes, unique);
            } else if entry.path().extension().is_some_and(|ext| ext == "delta") {
                *count += 1;
                *bytes += meta.len();
                unique.insert((meta.dev(), meta.ino()), meta.len());
            }
        }
    }
    let (mut count, mut bytes, mut unique) = (0, 0, BTreeMap::new());
    for name in ["retained", "shell-selected-v1"] {
        walk(&path.join(name), &mut count, &mut bytes, &mut unique);
    }
    (count, bytes, unique.len(), unique.values().sum())
}
fn tree_totals(path: &Path) -> (usize, u64) {
    if !path.exists() {
        return (0, 0);
    }
    fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                tree_totals(&entry.path())
            } else {
                (1, entry.metadata().unwrap().len())
            }
        })
        .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
}
fn peak_rss_bytes() -> u64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    let rss = unsafe { usage.assume_init() }.ru_maxrss as u64;
    if cfg!(target_os = "macos") {
        rss
    } else {
        rss * 1024
    }
}
fn report(
    owner: &SessionOwner,
    offline: usize,
    cut: &str,
    output: &Output,
) -> (usize, u64, usize, u64) {
    let workspace = owner.root().join("relay/workspaces").join(
        tilo_relay::room_workspace_id(owner.peer().room())
            .unwrap()
            .to_string(),
    );
    let files = census(&workspace);
    let retained = tree_totals(&workspace.join("retained"));
    let selected = tree_totals(&workspace.join("shell-selected-v1"));
    let workspace_totals = tree_totals(&workspace);
    let pending = fs::read_dir(workspace.join("pending"))
        .unwrap()
        .map(|entry| fs::read_dir(entry.unwrap().path()).unwrap().count())
        .sum::<usize>();
    println!(
        "owner_growth {}",
        serde_json::json!({"offline":offline,"cut":cut,"owner_generation":owner.status().generation,"owner_outgoing_frames":owner.outgoing().len(),"owner_outgoing_body_bytes":owner.outgoing().iter().map(|p| p.bytes.len()).sum::<usize>(),"retained_all_files":retained.0+selected.0,"retained_all_logical_bytes":retained.1+selected.1,"workspace_all_files":workspace_totals.0,"workspace_all_logical_bytes":workspace_totals.1,"retained_delta_paths":files.0,"retained_logical_bytes":files.1,"retained_unique_inodes":files.2,"retained_unique_inode_bytes":files.3,"pending_files":pending,"observed_body_frames":output.frames,"observed_body_bytes":output.bytes,"process_peak_rss_bytes":peak_rss_bytes()})
    );
    assert_eq!(
        pending, 0,
        "shell retained publication must not grow per-offline queues"
    );
    files
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_owner_growth_is_independent_of_offline_population_and_replays_exact_receipts() {
    let mut population_counts = Vec::new();
    for offline in [0, 32] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("owner");
        let mut owner = SessionOwner::initialize(&path).unwrap();
        let paths = IdentityPaths::under(owner.root().canonicalize().unwrap().join("identity"));
        let key = persist::read_device_key(&paths.device_privkey).unwrap();
        let root = persist::read_signing_key(&paths.user_privkey).unwrap();
        let observer_key = SigningKey::from_bytes(&[90; 32]);
        let mut observer_cert = None;
        let mut candidate = owner.peer().clone();
        for number in 0..=offline {
            let member = if number == 0 {
                observer_key.clone()
            } else {
                SigningKey::from_bytes(&[100 + number as u8; 32])
            };
            let mut cert = DeviceCert {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: member.verifying_key().to_bytes(),
                device_label: format!("qualification member {number}"),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: tilo_relay::clock::now_ns(),
                expires_at_ns: None,
                signature: [0; 64],
            };
            tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
            candidate
                .admit_certificate(cert.clone(), &key, SystemTime::now())
                .unwrap();
            if number == 0 {
                observer_cert = Some(cert);
            }
        }
        owner
            .commit_admission(candidate, owner.directory().to_vec())
            .unwrap();
        let cert = observer_cert.unwrap();
        let host = LocalHost::start(&owner).await.unwrap();
        let mut native = host.connect(&owner).await.unwrap();
        native_ready(&mut owner, &mut native).await;
        let mut session = observer(&host, &owner, &cert, &observer_key).await;
        let mut receiver = fresh(&owner);
        replay(&session, &mut [&mut receiver]).await;
        publish(&owner, &native).unwrap();
        let output = converge(&session, &mut receiver, &owner).await;
        output.assert_full_basis(&owner);
        report(&owner, offline, "baseline", &output);
        let mut offline_peer = receiver.clone();
        let mut full_counts = BTreeSet::new();
        for round in 0..6 {
            let before_rows = owner.peer().rows();
            let before_receipt = owner.peer().durable_coverage("dev.qos.chat", "").unwrap();
            owner.refresh_checkpoint().unwrap();
            assert_eq!(
                owner.peer().rows(),
                before_rows,
                "same-head re-signing changed documents"
            );
            assert_ne!(
                owner.peer().durable_coverage("dev.qos.chat", "").unwrap(),
                before_receipt,
                "same-head receipt must advance exactly"
            );
            publish(&owner, &native).unwrap();
            let output = converge(&session, &mut receiver, &owner).await;
            output.assert_full_basis(&owner);
            let files = report(&owner, offline, &format!("refresh-{round}"), &output);
            full_counts.insert(files.2);
            assert_eq!(
                files.2,
                owner.peer().rows().len() + 1,
                "retain exactly one full owner checkpoint basis"
            );
            let untouched = owner.peer().durable_coverage("dev.qos.chat", "").unwrap();
            let mut candidate = owner.peer().clone();
            let app = "dev.qos.word-processor";
            let record = "tab:0000000000000001";
            let mut doc = AutoCommit::load(&candidate.document(app, record).unwrap()).unwrap();
            doc.put(ROOT, "qualification_commit", round as i64).unwrap();
            doc.commit();
            candidate
                .publish_document(app, record, doc.save(), &key, SystemTime::now())
                .unwrap();
            owner
                .commit_mutation_candidate(candidate, owner.directory().to_vec())
                .unwrap();
            assert_eq!(
                owner.peer().durable_coverage("dev.qos.chat", "").unwrap(),
                untouched
            );
            publish(&owner, &native).unwrap();
            let output = converge(&session, &mut receiver, &owner).await;
            assert_eq!(
                output.frames, 2,
                "small commit output must be room plus touched app"
            );
            report(&owner, offline, &format!("commit-{round}"), &output);
            if round == 2 {
                native.into_client().close().await;
                native = host.connect(&owner).await.unwrap();
                native_ready(&mut owner, &mut native).await;
                publish(&owner, &native).unwrap();
                // Exact duplicate reconnect publication is already covered. The
                // observer reconnect below supplies an ordered replay barrier.
                session.into_client().close().await;
                session = observer(&host, &owner, &cert, &observer_key).await;
                let mut new_peer = fresh(&owner);
                let output = replay(&session, &mut [&mut receiver, &mut new_peer]).await;
                assert!(exact(&receiver, owner.peer()) && exact(&new_peer, owner.peer()));
                report(&owner, offline, "reconnect", &output);
            }
        }
        population_counts.push(full_counts);
        owner.refresh_checkpoint().unwrap();
        publish(&owner, &native).unwrap();
        converge(&session, &mut receiver, &owner).await;
        session.into_client().close().await;
        native.into_client().close().await;
        host.shutdown().await.unwrap();
        let expected = owner.peer().clone();
        let committed = owner.status();
        drop(owner);
        let owner = SessionOwner::open(&path).unwrap();
        assert_eq!(owner.status(), committed);
        let host = LocalHost::start(&owner).await.unwrap();
        let session = observer(&host, &owner, &cert, &observer_key).await;
        let mut new_peer = fresh(&owner);
        let start = Instant::now();
        let output = replay(
            &session,
            &mut [&mut receiver, &mut new_peer, &mut offline_peer],
        )
        .await;
        output.assert_full_basis(&owner);
        for peer in [&receiver, &new_peer, &offline_peer] {
            assert!(
                exact(peer, &expected),
                "final heads, room heads or exact receipts differ"
            );
        }
        report(&owner, offline, "restart-final", &output);
        println!(
            "owner_growth_final {}",
            serde_json::json!({"offline":offline,"replay_ms":start.elapsed().as_millis(),"heads":owner.peer().rows(),"exact_receipts":owner.peer().rows().iter().map(|row| owner.peer().durable_coverage(&row.app_id,&row.record_id).unwrap()).collect::<Vec<_>>()})
        );
        session.into_client().close().await;
        host.shutdown().await.unwrap();
    }
    assert_eq!(
        population_counts[0], population_counts[1],
        "offline population changed retained full-basis counts"
    );
}
