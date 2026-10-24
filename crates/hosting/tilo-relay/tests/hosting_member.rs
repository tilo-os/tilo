//! A certified host of a person serves a room one of the person's devices
//! governs: the governor attaches it, the room's hosts name it, it admits
//! whom the room's governance admits as that governance changes, carries
//! members' changes to the governor, and stops when detached.

use iroh::protocol::Router;
use tilo_relay::host::{HostConfig, HostIdentity, HostMode, RoomHost};
use tilo_relay::{Envelope, RelayClient};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, TestUser};

/// How long the hosts are watched doing nothing: longer than the host's
/// first two waits before dialing again, so a session that kept ending
/// would show.
const IDLE_WINDOW: std::time::Duration = std::time::Duration::from_secs(4);

/// An endpoint whose key is the device's own, as a device dials with.
async fn device_endpoint(device: &TestDevice) -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&device.sk.to_bytes()))
        .bind()
        .await
        .unwrap()
}

/// A room host in `mode` on `device`'s own endpoint, routed as its embedder
/// routes it.
struct Hosted {
    host: RoomHost,
    router: Router,
    _state: tempfile::TempDir,
}

async fn host_on(user: &TestUser, device: &TestDevice, mode: HostMode) -> Hosted {
    let state = tempfile::tempdir().unwrap();
    let endpoint = device_endpoint(device).await;
    let host = RoomHost::open(
        HostConfig::new(state.path().to_path_buf(), mode),
        HostIdentity {
            user_public_key: user.user_pk,
            device_signing_key: device.sk.clone(),
            device_cert: device.cert.clone(),
            revocations: RevocationList::new(),
        },
        endpoint.clone(),
    )
    .await
    .unwrap();
    let mut router = Router::builder(endpoint);
    for (alpn, handler) in host.protocols() {
        router = router.accept(alpn, handler);
    }
    Hosted {
        host,
        router: router.spawn(),
        _state: state,
    }
}

impl Hosted {
    async fn stop(self) {
        self.router.shutdown().await.unwrap();
        self.host.shutdown().await;
    }
}

/// A session of `device` to the certified host `host` of its own person.
async fn session_to(user: &TestUser, device: &TestDevice, host: &Hosted) -> RelayClient {
    RelayClient::connect_v2_to_enrolled_home_peer(
        device_endpoint(device).await,
        host.host.node_addr(),
        user.user_pk,
        host.host.device_cert().device_public_key,
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap()
}

/// Whether `client`'s host admits it to `workspace_id`.
async fn admitted(client: &RelayClient, workspace_id: u64) -> bool {
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    loop {
        match client.recv().await.expect("the session stays open") {
            Envelope::Ready {
                workspace_id: ready,
                ..
            } if ready == workspace_id => return true,
            Envelope::NotAuthorised {
                workspace_id: refused,
                ..
            } if refused == workspace_id => return false,
            _ => {}
        }
    }
}

/// The next delta `client` receives.
async fn next_delta(client: &RelayClient) -> Vec<u8> {
    tilo_test_env::within("the delta arrives", async {
        loop {
            if let Envelope::Delta { delta, .. } =
                client.recv().await.expect("the session stays open")
            {
                break delta;
            }
        }
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "Hosted::stop consumes each host after the assertions; the suggested earlier drop would use a moved value"
)]
async fn a_certified_host_of_the_person_serves_a_room_its_device_governs() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let nas = alice.enrol("nas", DeviceCapabilities::host());
    let governor = host_on(&alice, &laptop, HostMode::Device).await;
    let member = host_on(&alice, &nas, HostMode::CertifiedHost).await;
    let rooms = governor.host.rooms();
    let room = rooms.create("Notes").await.unwrap();

    // Which hosts serve the room is its owner's decision: another person's
    // device asking is refused.
    let bob = TestUser::fresh("Bob");
    let bobs = bob.enrol("bob-phone", DeviceCapabilities::member());
    let refused = rooms
        .attach_host(
            &room.room,
            &bobs.cert,
            nas.device_pubkey(),
            member.host.node_addr(),
        )
        .await;
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(rooms.hosts(&room.room).await.unwrap().len(), 1);

    let hint = rooms
        .attach_host(
            &room.room,
            &laptop.cert,
            nas.device_pubkey(),
            member.host.node_addr(),
        )
        .await
        .unwrap();
    let hosts = rooms.hosts(&room.room).await.unwrap();
    assert_eq!(hosts.len(), 2, "the governor and the host serving it");
    assert_eq!(hosts[0].host, laptop.cert, "the governor comes first");
    assert_eq!(hosts[1], hint);

    // The phone enters the room on the laptop; the host follows that
    // governance and admits it.
    rooms.enter(&room.room, &phone.cert).await.unwrap();
    let on_member = session_to(&alice, &phone, &member).await;
    tilo_test_env::within("the host admits the member its governor admitted", async {
        while !admitted(&on_member, room.workspace_id).await {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;

    // A change the phone sends the host reaches the governor's own session.
    let local = governor
        .host
        .connect_local(
            laptop.cert.clone(),
            laptop.sk.clone(),
            RevocationList::new(),
        )
        .await
        .unwrap();
    assert!(admitted(&local, room.workspace_id).await);
    on_member
        .send(Envelope::Delta {
            workspace_id: room.workspace_id,
            author_device_public_key: phone.device_pubkey(),
            authored_at_ns: 1,
            delta: b"from the phone, through the host".to_vec(),
            signature: [0; 64],
        })
        .unwrap();
    assert_eq!(
        next_delta(&local).await,
        b"from the phone, through the host"
    );

    // With nothing changing, neither host does anything: no session is
    // opened again, nothing is subscribed, sent or received.
    let quiet = (governor.host.metrics(), member.host.metrics());
    tokio::time::sleep(IDLE_WINDOW).await;
    assert_eq!((governor.host.metrics(), member.host.metrics()), quiet);

    // Detached, the host is no longer named and admits nobody to the room.
    assert!(
        rooms
            .detach_host(&room.room, &laptop.cert, nas.device_pubkey(), None)
            .await
            .unwrap(),
        "the host confirms it stopped"
    );
    assert_eq!(rooms.hosts(&room.room).await.unwrap().len(), 1);
    let again = session_to(&alice, &phone, &member).await;
    assert!(!admitted(&again, room.workspace_id).await);

    drop((on_member, local, again));
    member.stop().await;
    governor.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "Hosted::stop consumes each host after the assertions; the suggested earlier drop would use a moved value"
)]
async fn a_device_serves_no_room_another_device_governs() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let desktop = alice.enrol("desktop", DeviceCapabilities::primary());
    let governor = host_on(&alice, &laptop, HostMode::Device).await;
    let other = host_on(&alice, &desktop, HostMode::Device).await;
    let rooms = governor.host.rooms();
    let room = rooms.create("Notes").await.unwrap();

    let refused = rooms
        .attach_host(
            &room.room,
            &laptop.cert,
            desktop.device_pubkey(),
            other.host.node_addr(),
        )
        .await;
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(rooms.hosts(&room.room).await.unwrap().len(), 1);
    other.stop().await;
    governor.stop().await;
}

/// A host the room's owner detaches lets the room go: it no longer holds it
/// open, though its files stay. Attached again, it serves the room from
/// those files and catches up on what the room published meanwhile.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "Hosted::stop consumes each host after the assertions; the suggested earlier drop would use a moved value"
)]
async fn a_detached_host_lets_the_room_go_and_serves_it_again_from_its_files() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let tablet = alice.enrol("tablet", DeviceCapabilities::primary());
    let nas = alice.enrol("nas", DeviceCapabilities::host());
    let governor = host_on(&alice, &laptop, HostMode::Device).await;
    let member = host_on(&alice, &nas, HostMode::CertifiedHost).await;
    let rooms = governor.host.rooms();
    let room = rooms.create("Notes").await.unwrap();
    let records = member.host.room_records();
    let retained = || async { records.retained(room.workspace_id).await.ok() };

    rooms
        .attach_host(
            &room.room,
            &laptop.cert,
            nas.device_pubkey(),
            member.host.node_addr(),
        )
        .await
        .unwrap();
    rooms.enter(&room.room, &phone.cert).await.unwrap();
    let kept = tilo_test_env::within("the host to keep the room's changes", async {
        loop {
            if let Some(bodies) = retained().await.filter(|bodies| !bodies.is_empty()) {
                break bodies.len();
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;

    assert!(
        rooms
            .detach_host(&room.room, &laptop.cert, nas.device_pubkey(), None)
            .await
            .unwrap()
    );
    assert!(retained().await.is_none(), "the host let the room go");

    // The room moves on while the host is away.
    rooms.enter(&room.room, &tablet.cert).await.unwrap();

    rooms
        .attach_host(
            &room.room,
            &laptop.cert,
            nas.device_pubkey(),
            member.host.node_addr(),
        )
        .await
        .unwrap();
    assert!(
        retained().await.is_some_and(|bodies| bodies.len() >= kept),
        "served again from what it kept"
    );
    tilo_test_env::within("the host to catch up", async {
        while retained().await.is_none_or(|bodies| bodies.len() <= kept) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    let on_member = session_to(&alice, &tablet, &member).await;
    tilo_test_env::within("the host to admit the device entered meanwhile", async {
        while !admitted(&on_member, room.workspace_id).await {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;

    drop(on_member);
    member.stop().await;
    governor.stop().await;
}
