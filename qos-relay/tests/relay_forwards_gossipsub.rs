//! Integration test: two libp2p peers exchange a gossipsub message
//! through an in-process relay, without ever dialling each other
//! directly.
//!
//! Layout:
//!
//! - `relay` listens on an ephemeral TCP port. Its config sets
//!   `topic_allowlist = ["qos.*"]` so the test topic `qos.test` is
//!   forwarded.
//! - Peers A and B build their own swarms with the same gossipsub
//!   config the shell uses (1 s heartbeat, mesh 4/8/12, strict
//!   signed). They dial *only* the relay. Both subscribe to
//!   `qos.test`.
//! - A publishes a payload; B observes it via gossipsub. The
//!   forwarding hops are A → relay → B.
//!
//! This is the marquee test for the whole Phase 9 deliverable: if
//! it passes, the relay actually does its job.

use std::time::Duration;

use libp2p::futures::StreamExt;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{Multiaddr, PeerId, Swarm, gossipsub, noise, tcp, yamux};
use qos_relay::{Relay, RelayConfig};
use tokio::time::timeout;

#[derive(NetworkBehaviour)]
struct PeerBehaviour {
    gossipsub: gossipsub::Behaviour,
}

fn build_peer_swarm() -> Swarm<PeerBehaviour> {
    libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .expect("tcp")
        .with_behaviour(|key| {
            let cfg = gossipsub::ConfigBuilder::default()
                .heartbeat_interval(Duration::from_secs(1))
                .mesh_n(8)
                .mesh_n_low(4)
                .mesh_n_high(12)
                .validation_mode(gossipsub::ValidationMode::Strict)
                .build()
                .expect("gossipsub config");
            let gossipsub = gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(key.clone()),
                cfg,
            )
            .expect("gossipsub behaviour");
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(PeerBehaviour { gossipsub })
        })
        .expect("with_behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build()
}

/// Drive the swarm until at least one TCP listen address shows up,
/// then return it. Bounded by 5 s.
async fn first_listen_addr<B: NetworkBehaviour>(swarm: &mut Swarm<B>) -> Multiaddr {
    timeout(Duration::from_secs(5), async {
        loop {
            if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
                // Filter to TCP — QUIC adds another addr we don't need.
                if address.iter().any(|p| matches!(p, libp2p::multiaddr::Protocol::Tcp(_))) {
                    return address;
                }
            }
        }
    })
    .await
    .expect("listen addr within 5s")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn message_forwarded_via_relay_without_direct_peer_dial() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .try_init();

    // ---- 1. Spin up the relay on a random port -----------------------
    let dir = tempfile::tempdir().unwrap();
    let cfg = RelayConfig {
        listen: vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()],
        identity_path: dir.path().join("relay.key"),
        bootstrap: vec![],
        topic_allowlist: vec!["qos.*".to_string()],
        ..RelayConfig::default()
    };
    let mut relay = Relay::build(cfg).await.expect("relay builds");
    let relay_peer_id = relay.local_peer_id();

    // Drive the relay until it logs a listen addr.
    let relay_addr = relay
        .run_until(|ev| match ev {
            SwarmEvent::NewListenAddr { address, .. }
                if address
                    .iter()
                    .any(|p| matches!(p, libp2p::multiaddr::Protocol::Tcp(_))) =>
            {
                Some(address.clone())
            }
            _ => None,
        })
        .await
        .expect("relay listen addr");

    // Append `/p2p/<peerid>` so peers can authenticate the dial.
    let relay_full: Multiaddr = format!("{}/p2p/{}", relay_addr, relay_peer_id)
        .parse()
        .expect("multiaddr");

    // Spawn the relay's event loop in the background. Hold the
    // join handle so we can abort at the end of the test.
    let relay_task = tokio::spawn(async move {
        loop {
            relay.step().await;
        }
    });

    // ---- 2. Build peer A and peer B ----------------------------------
    let mut swarm_a = build_peer_swarm();
    let mut swarm_b = build_peer_swarm();

    // Each listens on its own loopback port (so the test environment
    // doesn't depend on anything else listening). They will *not*
    // dial each other; both will dial only the relay.
    swarm_a
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .unwrap();
    swarm_b
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .unwrap();
    let _addr_a = first_listen_addr(&mut swarm_a).await;
    let _addr_b = first_listen_addr(&mut swarm_b).await;

    let peer_id_a: PeerId = *swarm_a.local_peer_id();
    let peer_id_b: PeerId = *swarm_b.local_peer_id();
    assert_ne!(peer_id_a, peer_id_b);

    // ---- 3. Subscribe both to qos.test before dialling --------------
    let topic = gossipsub::IdentTopic::new("qos.test");
    swarm_a
        .behaviour_mut()
        .gossipsub
        .subscribe(&topic)
        .expect("subscribe A");
    swarm_b
        .behaviour_mut()
        .gossipsub
        .subscribe(&topic)
        .expect("subscribe B");

    // ---- 4. Dial the relay from both peers --------------------------
    swarm_a.dial(relay_full.clone()).expect("A dial relay");
    swarm_b.dial(relay_full.clone()).expect("B dial relay");

    // ---- 5. Wait for the gossipsub mesh to form across the relay ----
    // Each peer must see the other (or at least see *one* mesh peer
    // — the relay itself) before publishing. We track that A has
    // seen the relay subscribe to qos.test, which only happens once
    // the relay learns about the topic from one of the peers.
    let mesh_ready = async {
        let mut a_sees_relay = false;
        let mut b_sees_relay = false;
        loop {
            tokio::select! {
                ev = swarm_a.select_next_some() => {
                    if let SwarmEvent::Behaviour(PeerBehaviourEvent::Gossipsub(
                        gossipsub::Event::Subscribed { peer_id, .. },
                    )) = ev
                        && peer_id == relay_peer_id
                    {
                        a_sees_relay = true;
                    }
                }
                ev = swarm_b.select_next_some() => {
                    if let SwarmEvent::Behaviour(PeerBehaviourEvent::Gossipsub(
                        gossipsub::Event::Subscribed { peer_id, .. },
                    )) = ev
                        && peer_id == relay_peer_id
                    {
                        b_sees_relay = true;
                    }
                }
            }
            if a_sees_relay && b_sees_relay {
                return;
            }
        }
    };
    timeout(Duration::from_secs(15), mesh_ready)
        .await
        .expect("mesh formed within 15s");

    // Tiny settle so the relay's mesh slot graduates from fanout to
    // mesh — gossipsub publishes via fanout if no mesh peers are
    // known yet, which still works but is racier.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // ---- 6. Publish from A; B must receive via the relay ------------
    let payload = b"hello-via-relay";
    swarm_a
        .behaviour_mut()
        .gossipsub
        .publish(topic.clone(), payload.to_vec())
        .expect("publish A");

    // Drain both swarms until B observes the message. A may emit
    // its own published event; we only assert on B receiving.
    let observed = timeout(Duration::from_secs(20), async {
        loop {
            tokio::select! {
                _ev = swarm_a.select_next_some() => {}
                ev = swarm_b.select_next_some() => {
                    if let SwarmEvent::Behaviour(PeerBehaviourEvent::Gossipsub(
                        gossipsub::Event::Message { message, propagation_source, .. },
                    )) = ev
                        && message.data == payload
                    {
                        return propagation_source;
                    }
                }
            }
        }
    })
    .await
    .expect("B received the message within 20s");

    // The propagation source should be the *relay*, not peer A —
    // we never dialled A↔B directly.
    assert_eq!(
        observed, relay_peer_id,
        "message must be forwarded via the relay (got source {observed}, \
         relay {relay_peer_id})",
    );

    relay_task.abort();
}
