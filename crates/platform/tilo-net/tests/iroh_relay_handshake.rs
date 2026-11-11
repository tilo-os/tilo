//! A relay client sends right behind its answer to the relay's challenge.
//!
//! A page reaches its host only through the host's packet relay, and its
//! WebSocket offers no TLS key material, so the relay authenticates it with a
//! challenge. Upstream, the client then waited a round trip for the relay to
//! confirm its answer before it sent anything, and a fresh visitor's session
//! began that much later. The vendored client (`vendor/iroh-relay`,
//! `ClientBuilder::send_before_confirmed`) goes on at once: the relay reads
//! what follows the answer only once it confirmed it, and a denial is the
//! connection's first error instead.

#![cfg(not(target_family = "wasm"))]
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use futures_util::{SinkExt as _, StreamExt as _};
use iroh::SecretKey;
use iroh_relay::client::{Client, ClientBuilder};
use iroh_relay::protos::relay::{ClientToRelayMsg, Datagrams, RelayToClientMsg};
use iroh_relay::server::{Access, AccessControl, ClientRequest, RelayConfig, Server, ServerConfig};
use iroh_relay::tls::{CaTlsConfig, default_provider};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Each way's delay on the network between the client and the relay.
const ONE_WAY: Duration = Duration::from_millis(150);

/// A relay admitting everyone but the keys it was given.
#[derive(Debug)]
struct Refusing(Vec<iroh::PublicKey>);

impl AccessControl for Refusing {
    async fn on_connect(&self, request: &ClientRequest) -> Access {
        if self.0.contains(&request.endpoint_id()) {
            Access::Deny { reason: None }
        } else {
            Access::Allow
        }
    }
}

async fn relay(refused: Vec<iroh::PublicKey>) -> Server {
    let mut relay = RelayConfig::new((Ipv4Addr::LOCALHOST, 0));
    relay.access = std::sync::Arc::new(Refusing(refused));
    let mut config = ServerConfig::default();
    config.relay = Some(relay);
    Server::spawn(config).await.expect("a local relay")
}

/// A TCP path to `upstream` whose bytes each take [`ONE_WAY`] to arrive.
async fn slow_path(upstream: SocketAddr) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("address");
    tokio::spawn(async move {
        while let Ok((near, _)) = listener.accept().await {
            let far = tokio::net::TcpStream::connect(upstream)
                .await
                .expect("the relay");
            let (near_read, near_write) = near.into_split();
            let (far_read, far_write) = far.into_split();
            tokio::spawn(carry(near_read, far_write));
            tokio::spawn(carry(far_read, near_write));
        }
    });
    addr
}

async fn carry(mut from: tokio::net::tcp::OwnedReadHalf, mut to: tokio::net::tcp::OwnedWriteHalf) {
    let (queue, mut due) = tokio::sync::mpsc::unbounded_channel::<(Instant, Vec<u8>)>();
    tokio::spawn(async move {
        while let Some((at, bytes)) = due.recv().await {
            tokio::time::sleep_until(at.into()).await;
            if to.write_all(&bytes).await.is_err() {
                return;
            }
        }
    });
    let mut buffer = vec![0; 64 * 1024];
    while let Ok(read) = from.read(&mut buffer).await {
        if read == 0
            || queue
                .send((Instant::now() + ONE_WAY, buffer[..read].to_vec()))
                .is_err()
        {
            return;
        }
    }
}

fn client(url: &str, key: SecretKey, send_before_confirmed: bool) -> ClientBuilder {
    ClientBuilder::new(
        url.parse::<iroh::RelayUrl>().expect("relay URL"),
        key,
        iroh::dns::DnsResolver::new(),
    )
    .tls_client_config(
        CaTlsConfig::default()
            .client_config(default_provider())
            .expect("TLS config"),
    )
    .send_before_confirmed(send_before_confirmed)
}

/// The next datagrams `client` receives.
async fn datagrams(client: &mut Client) -> Datagrams {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(10), client.next())
            .await
            .expect("a message in time")
            .expect("the connection is open")
            .expect("a message, not an error");
        if let RelayToClientMsg::Datagrams { datagrams, .. } = message {
            return datagrams;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_sends_without_waiting_for_the_relay_to_confirm_it() {
    let server = relay(Vec::new()).await;
    let http = server.http_addr().expect("the relay's HTTP address");
    let direct = format!("http://{http}");
    let slow = format!("http://{}", slow_path(http).await);
    let round_trip = ONE_WAY * 2;

    let receiver_key = SecretKey::generate();
    let receiver_id = receiver_key.public();
    let mut receiver = client(&direct, receiver_key, false)
        .connect()
        .await
        .expect("the receiver connects");

    // Waiting for the confirmation: the upgrade, then the challenge's answer.
    let started = Instant::now();
    let waiting = client(&slow, SecretKey::generate(), false)
        .connect()
        .await
        .expect("a client that waits connects");
    let waited = started.elapsed();
    assert!(
        waited >= round_trip * 2,
        "a client that waits for the confirmation took {waited:?}"
    );
    drop(waiting);

    // Without it: the upgrade alone, and what it sends right away arrives.
    let sender_key = SecretKey::generate();
    let sender_id = sender_key.public();
    let started = Instant::now();
    let mut sender = client(&slow, sender_key, true)
        .connect()
        .await
        .expect("a client that goes on connects");
    let went_on = started.elapsed();
    assert!(
        went_on < round_trip + round_trip / 2,
        "a client that goes on took {went_on:?}, one round trip is {round_trip:?}"
    );
    let hello = Datagrams::from("right behind the answer");
    sender
        .send(ClientToRelayMsg::Datagrams {
            dst_endpoint_id: receiver_id,
            datagrams: hello.clone(),
        })
        .await
        .expect("sent");
    assert_eq!(datagrams(&mut receiver).await, hello);

    // The confirmation it did not wait for is passed over: what comes back to
    // it reads as it always did.
    let back = Datagrams::from("and back");
    receiver
        .send(ClientToRelayMsg::Datagrams {
            dst_endpoint_id: sender_id,
            datagrams: back.clone(),
        })
        .await
        .expect("sent back");
    assert_eq!(datagrams(&mut sender).await, back);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relay_that_refuses_a_client_that_went_on_ends_its_connection() {
    let refused = SecretKey::generate();
    let server = relay(vec![refused.public()]).await;
    let url = format!("http://{}", server.http_addr().expect("address"));
    let mut client = client(&url, refused, true)
        .connect()
        .await
        .expect("a client that goes on is not told at once");
    let first = tokio::time::timeout(Duration::from_secs(10), client.next())
        .await
        .expect("an answer in time");
    match first {
        Some(Err(error)) => assert!(
            error.to_string().contains("denied") || format!("{error:?}").contains("denied"),
            "the refusal reads as one: {error:?}"
        ),
        other => panic!("a refused client read {other:?}"),
    }
}
