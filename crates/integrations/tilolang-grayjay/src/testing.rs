#![allow(
    clippy::expect_used,
    reason = "the peer is a test's device: a port or a thread this machine will not give it \
              fails the test loudly, which is the answer a test wants"
)]

//! Grayjay's own side of a pairing, for a test and for a peer that runs in this process.
//!
//! [`Peer`] listens as a Grayjay device does: it advertises an invite naming its key and
//! address, takes one client's connection at a time, and answers the state exchange with the
//! subscriptions and history it was given. What the client pushes back it keeps, so a test
//! reads what the app sent the other app.

use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::protocol::{
    SerializedChannel, Subscription, SyncDeviceInfo, SyncSubscriptionsPackage, sync_opcode,
};
use crate::transport::{DeviceKey, Incoming, Session};
use crate::{PeerRoute, SharedSubscription};

/// A Grayjay device a client pairs with and syncs from.
pub struct Peer {
    address: SocketAddr,
    key: DeviceKey,
    code: Option<String>,
    subscriptions: Vec<SharedSubscription>,
    history: Vec<String>,
    /// Each connection's pushed subscriptions, in the order the connections came.
    pushed: Arc<Mutex<Vec<Vec<SharedSubscription>>>>,
    stop: Arc<AtomicBool>,
    serving: Option<JoinHandle<()>>,
}

impl Peer {
    /// Listen on a loopback port, holding `subscriptions` and `history` for whoever pairs.
    pub fn start(
        subscriptions: Vec<SharedSubscription>,
        history: Vec<String>,
        code: Option<String>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let address = listener.local_addr().expect("the listener's address");
        let key = DeviceKey::from_private(&[0x42; 32]).expect("a device key");
        let pushed = Arc::new(Mutex::new(Vec::new()));
        let known = Arc::new(Mutex::new(None::<String>));
        let stop = Arc::new(AtomicBool::new(false));
        let mut peer = Self {
            address,
            key,
            code,
            subscriptions,
            history,
            pushed,
            stop,
            serving: None,
        };
        let listener = listener;
        let serving = {
            let pushed = Arc::clone(&peer.pushed);
            let known = Arc::clone(&known);
            let stop = Arc::clone(&peer.stop);
            let key = DeviceKey::from_private(&[0x42; 32]).expect("a device key");
            let code = peer.code.clone();
            let subscriptions = peer.subscriptions.clone();
            let history = peer.history.clone();
            thread::Builder::new()
                .name("grayjay-peer".into())
                .spawn(move || {
                    while !stop.load(Ordering::Acquire) {
                        let Ok((stream, _)) = listener.accept() else {
                            break;
                        };
                        serve_connection(
                            stream,
                            &key,
                            code.as_deref(),
                            &subscriptions,
                            &history,
                            &known,
                            &pushed,
                        );
                    }
                })
                .expect("the peer's thread")
        };
        peer.serving = Some(serving);
        peer
    }

    /// The invite Grayjay's Sync screen would show, as JSON: the key, the address, the port,
    /// and the code the other end must carry while pairing.
    #[must_use]
    pub fn invite(&self) -> String {
        serde_json::to_string(&SyncDeviceInfo {
            public_key: self.key.public_base64(),
            addresses: vec![self.address.ip().to_string()],
            port: i32::from(self.address.port()),
            code: self.code.clone(),
        })
        .expect("an invite")
    }

    /// The route a client's pairing made, as the app would record it.
    #[must_use]
    pub fn route(&self) -> PeerRoute {
        PeerRoute {
            key: self.key.public_base64(),
            name: "Grayjay peer".into(),
            addresses: vec![self.address.ip().to_string()],
            port: i32::from(self.address.port()),
        }
    }

    /// What each connection pushed, in the order they came.
    #[must_use]
    pub fn pushed(&self) -> Vec<Vec<SharedSubscription>> {
        self.pushed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake the accept the serving thread waits on, so it sees the stop.
        let _ = TcpStream::connect(self.address);
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
    }
}

/// One connection: accept, authorize, answer the state exchange, keep what is pushed.
fn serve_connection(
    stream: TcpStream,
    key: &DeviceKey,
    code: Option<&str>,
    subscriptions: &[SharedSubscription],
    history: &[String],
    known: &Mutex<Option<String>>,
    pushed: &Mutex<Vec<Vec<SharedSubscription>>>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(mut session) = Session::accept_responder(stream, key, "Grayjay", |peer_key, carried| {
        let mut known = known
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // The connection that adds the device carries the invite's code; every later one is the
        // same
        // client, which the device knows by its key from then on.
        let allowed = known
            .as_deref()
            .map_or(carried == code, |previous| previous == peer_key);
        if allowed && known.is_none() {
            *known = Some(peer_key.to_owned());
        }
        allowed
    }) else {
        return;
    };
    if session
        .complete_authorization(std::time::Instant::now() + Duration::from_secs(10))
        .is_err()
    {
        return;
    }
    loop {
        match session.recv() {
            Ok(Incoming::Data {
                sub_opcode,
                payload,
            }) => match sub_opcode {
                sync_opcode::SYNC_STATE_EXCHANGE => {
                    let package = SyncSubscriptionsPackage {
                        subscriptions: subscriptions
                            .iter()
                            .map(|subscription| Subscription {
                                channel: SerializedChannel::named(
                                    subscription.url.clone(),
                                    Some(subscription.name.clone()),
                                ),
                                creation_time: 1_700_000_000,
                            })
                            .collect(),
                        subscription_removals: BTreeMap::new(),
                    };
                    let Ok(json) = serde_json::to_string(&package) else {
                        return;
                    };
                    if session
                        .send_data(sync_opcode::SYNC_SUBSCRIPTIONS, &json)
                        .is_err()
                    {
                        return;
                    }
                    let entries: Vec<serde_json::Value> = history
                        .iter()
                        .map(|url| serde_json::json!({ "video": { "url": url } }))
                        .collect();
                    let Ok(json) = serde_json::to_string(&entries) else {
                        return;
                    };
                    if session.send_data(sync_opcode::SYNC_HISTORY, &json).is_err() {
                        return;
                    }
                }
                sync_opcode::SYNC_SUBSCRIPTIONS => {
                    if let Ok(package) =
                        serde_json::from_slice::<SyncSubscriptionsPackage>(&payload)
                    {
                        let sent: Vec<SharedSubscription> = package
                            .subscriptions
                            .into_iter()
                            .map(|subscription| SharedSubscription {
                                url: subscription.channel.url,
                                name: subscription.channel.name,
                            })
                            .collect();
                        pushed
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(sent);
                    }
                    return;
                }
                _ => {}
            },
            _ => return,
        }
    }
}
