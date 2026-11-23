//! Devices of one workspace find each other on the local network with no
//! relay and no directory, while someone else on that network learns
//! nothing: no key, no address, and no answer.
//!
//! Every endpoint here speaks on this machine's loopback, so a test hears
//! only its own endpoints and needs no multicast on the network it runs on.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointId, RelayMode, SecretKey};
use tilo_endpoint_reachability::Nearby;
use tilo_endpoint_reachability::lan::{DATAGRAM_LEN, Fleet, GROUP, PORT};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
use tokio::time::{Instant, timeout};

const ALPN: &[u8] = b"tilo/lan-discovery-test";

/// The devices of one person as one device knows them: their root, and the
/// devices the person revoked.
struct Person {
    root: [u8; 32],
    cert: DeviceCert,
    revoked: Mutex<BTreeSet<[u8; 32]>>,
}

impl Fleet for Person {
    fn certificate(&self) -> Option<DeviceCert> {
        Some(self.cert.clone())
    }

    fn admits(&self, cert: &DeviceCert) -> bool {
        cert.user_public_key == self.root
            && tilo_user_identity::verify_device_cert(cert).is_ok()
            && !self
                .revoked
                .lock()
                .unwrap()
                .contains(&cert.device_public_key)
    }
}

fn certified(root: &SigningKey, device: &SigningKey) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Device".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

struct Device {
    endpoint: Endpoint,
    nearby: Nearby,
    person: Arc<Person>,
}

/// A device of `root`'s on its own key, found and answering only on this
/// machine's loopback, with no relay.
async fn device(root: &SigningKey, seed: u8) -> Device {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let person = Arc::new(Person {
        root: root.verifying_key().to_bytes(),
        cert: certified(root, &key),
        revoked: Mutex::default(),
    });
    let nearby = Nearby::on(Arc::new(|| {}), [Ipv4Addr::LOCALHOST]);
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&key.to_bytes()))
        .relay_mode(RelayMode::Disabled)
        .alpns(vec![ALPN.to_vec()])
        .address_lookup(nearby.lookup(person.clone()))
        .bind()
        .await
        .expect("the endpoint binds");
    Device {
        endpoint,
        nearby,
        person,
    }
}

/// Answer every connection `device` hears, reading what its first stream
/// says.
fn answering(device: &Device) -> tokio::task::JoinHandle<()> {
    let endpoint = device.endpoint.clone();
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            tokio::spawn(async move {
                let Ok(connection) = incoming.await else {
                    return;
                };
                if let Ok((_, mut receive)) = connection.accept_bi().await {
                    let said = receive.read_to_end(1024).await.expect("the stream reads");
                    assert_eq!(said, b"hello");
                }
                connection.closed().await;
            });
        }
    })
}

/// `from` dials `to` by key alone until it reaches it, for up to `within`.
async fn reaches(from: &Device, to: EndpointId, within: Duration) -> bool {
    let until = Instant::now() + within;
    while Instant::now() < until {
        if let Ok(Ok(connection)) =
            timeout(Duration::from_secs(10), from.endpoint.connect(to, ALPN)).await
        {
            let (mut send, _) = connection.open_bi().await.expect("a stream");
            send.write_all(b"hello").await.expect("sent");
            send.finish().expect("finished");
            send.stopped().await.ok();
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

/// What someone listening on the network heard, datagram by datagram.
type Datagrams = Arc<Mutex<Vec<Vec<u8>>>>;

/// Someone on the network who joins the probes' group and keeps every
/// datagram that reaches it.
fn listening() -> (Datagrams, tokio::task::JoinHandle<()>) {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
    socket.set_reuse_address(true).unwrap();
    socket.set_nonblocking(true).unwrap();
    socket
        .bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, PORT).into())
        .unwrap();
    let socket = tokio::net::UdpSocket::from_std(socket.into()).unwrap();
    socket
        .join_multicast_v4(GROUP, Ipv4Addr::LOCALHOST)
        .expect("the group is joined on loopback");
    let heard = Arc::new(Mutex::new(Vec::new()));
    let keeping = Arc::clone(&heard);
    let task = tokio::spawn(async move {
        let mut buffer = vec![0_u8; 65_536];
        while let Ok((len, _)) = socket.recv_from(&mut buffer).await {
            keeping.lock().unwrap().push(buffer[..len].to_vec());
        }
    });
    (heard, task)
}

/// Every way a datagram could name `device`'s key or the addresses it
/// listens on.
fn tells(device: &Device) -> Vec<Vec<u8>> {
    let key = *device.endpoint.id().as_bytes();
    let mut told = vec![
        key.to_vec(),
        ed25519_dalek::VerifyingKey::from_bytes(&key)
            .unwrap()
            .to_montgomery()
            .to_bytes()
            .to_vec(),
        device.endpoint.id().to_string().into_bytes(),
    ];
    let addrs: Vec<SocketAddr> = device.endpoint.addr().ip_addrs().copied().collect();
    assert!(!addrs.is_empty(), "the device listens somewhere");
    for addr in addrs {
        told.push(addr.to_string().into_bytes());
        if let std::net::IpAddr::V4(ip) = addr.ip() {
            let mut big = ip.octets().to_vec();
            big.extend_from_slice(&addr.port().to_be_bytes());
            told.push(big);
            let mut little = ip.octets().to_vec();
            little.extend_from_slice(&addr.port().to_le_bytes());
            told.push(little);
        }
    }
    told
}

/// Two devices of the person find each other by key alone, with no relay;
/// another person's device and a revoked one asking for them get no answer,
/// and someone listening on the network hears only same-sized boxes of
/// random bytes that name none of their keys or addresses.
#[tokio::test(flavor = "multi_thread")]
async fn workspace_devices_find_each_other_and_nobody_else_learns_anything() {
    let (heard, listener) = listening();
    let root = SigningKey::from_bytes(&[1; 32]);
    let host = device(&root, 2).await;
    let guest = device(&root, 3).await;
    let stranger = device(&SigningKey::from_bytes(&[9; 32]), 4).await;
    let revoked = device(&root, 5).await;
    host.person
        .revoked
        .lock()
        .unwrap()
        .insert(*revoked.endpoint.id().as_bytes());
    let answers = answering(&host);

    assert!(
        reaches(&guest, host.endpoint.id(), Duration::from_secs(30)).await,
        "the person's device is found on the local network"
    );
    assert!(guest.nearby.heard().contains(&host.endpoint.id()));
    assert!(
        host.nearby.heard().contains(&guest.endpoint.id()),
        "the device that asked is heard as well"
    );

    for outsider in [&stranger, &revoked] {
        assert!(
            !reaches(outsider, host.endpoint.id(), Duration::from_secs(6)).await,
            "a device the host does not admit is not answered"
        );
        assert!(!outsider.nearby.heard().contains(&host.endpoint.id()));
        assert!(!host.nearby.heard().contains(&outsider.endpoint.id()));
    }

    let datagrams = heard.lock().unwrap().clone();
    assert!(
        !datagrams.is_empty(),
        "the listener heard the probes on the network"
    );
    let told: Vec<Vec<u8>> = [&host, &guest, &stranger, &revoked]
        .into_iter()
        .flat_map(tells)
        .collect();
    for datagram in &datagrams {
        assert_eq!(datagram.len(), DATAGRAM_LEN, "every probe is the same size");
        for named in &told {
            assert!(
                !datagram.windows(named.len()).any(|window| window == named),
                "a probe names no key or address"
            );
        }
    }
    answers.abort();
    listener.abort();
    for device in [host, guest, stranger, revoked] {
        device.endpoint.close().await;
    }
}

/// A device enrolled after another knows that one and finds it; the older
/// device, which never heard of the newer one and holds no list of the
/// person's devices at all, learns it from its probe alone and dials it
/// back by key over the local network.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_enrolled_later_is_learned_over_the_local_network_alone() {
    let root = SigningKey::from_bytes(&[11; 32]);
    let older = device(&root, 12).await;
    let newer = device(&root, 13).await;
    let older_answers = answering(&older);
    let newer_answers = answering(&newer);
    assert!(older.nearby.heard().is_empty());

    assert!(
        reaches(&newer, older.endpoint.id(), Duration::from_secs(30)).await,
        "the newer device finds the older one"
    );
    assert!(
        older.nearby.heard().contains(&newer.endpoint.id()),
        "the older device learned the newer one from the network"
    );
    assert!(
        reaches(&older, newer.endpoint.id(), Duration::from_secs(30)).await,
        "and reaches it by key"
    );
    older_answers.abort();
    newer_answers.abort();
    older.endpoint.close().await;
    newer.endpoint.close().await;
}
