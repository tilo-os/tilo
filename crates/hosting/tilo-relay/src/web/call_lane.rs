//! Calls from pages: the turns a page posts to the host that serves it, and
//! the device that carries them onto the call's lane.
//!
//! A page holds its channel's content keys and seals its own media
//! ([`tilo_call_lane::Page`]), so what it posts is already sealed. Its host is
//! a member device of the workspaces it serves, and takes part in the call as
//! itself ([`tilo_call_lane::Bridge`]): it opens the call's lanes to the
//! workspace's other devices and copies frames between them and the page,
//! opening none of them. A call a page is in is therefore as end-to-end
//! encrypted as one between two devices.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use iroh::EndpointAddr;
use tilo_call_lane::page::{Down, Up};
use tilo_call_lane::{Bridge, Roster};
use tilo_lock::MutexRecover as _;
use tokio::sync::{Mutex as AsyncMutex, OnceCell};

/// A call a page is bridged into, and the devices its lanes were opened to.
struct Carried {
    bridge: Arc<Bridge>,
    /// The devices whose lane is up: a turn opens each of them no more.
    dialled: BTreeSet<[u8; 32]>,
    /// The devices a lane is being opened to right now: a dial that fails
    /// leaves the device out again, so a later turn tries it once more.
    dialling: BTreeSet<[u8; 32]>,
}

/// The calls this host carries, by workspace and call name: one bridge and
/// its lanes per call, shared by the pages turning it.
type CallTable = Mutex<BTreeMap<(String, String), Arc<AsyncMutex<Carried>>>>;

/// Why a turn did not go through.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Failed {
    /// The host has no endpoint to carry the call's lanes on.
    #[error("this host cannot carry a call's lanes: {0}")]
    NoLane(String),
    /// The page's bridge refused the turn: it carries another call, or the
    /// call is over.
    #[error("{0}")]
    Refused(String),
}

/// The pages' calls on one host: one lane member per call, taking part as
/// this host's device and answering the turns its pages post.
pub(crate) struct CallLane {
    /// This host's device: the key a page's frames name, and the id the call's
    /// lanes open from.
    device: [u8; 32],
    /// The device's signing key, which the lanes' endpoint binds with: the id
    /// the call's other devices admit as the workspace's member.
    key: Option<iroh::SecretKey>,
    /// The endpoint the lanes run on, bound the first time a page asks for a
    /// call: a host that serves no call never pays for one.
    endpoint: OnceCell<iroh::Endpoint>,
    calls: CallTable,
}

impl std::fmt::Debug for CallLane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallLane")
            .field("device", &self.device)
            .field("calls", &self.calls.lock_recover().len())
            .finish_non_exhaustive()
    }
}

impl CallLane {
    /// The pages' calls carried as `device`, the host's own key, which the
    /// workspaces it serves admit as a member. `key` signs the lanes'
    /// connections: the device's own, or none for a host that serves no lane
    /// until one is handed over with [`Self::at`].
    pub(crate) fn new(device: [u8; 32], key: Option<iroh::SecretKey>) -> Self {
        Self {
            device,
            key,
            endpoint: OnceCell::new(),
            calls: Mutex::new(BTreeMap::new()),
        }
    }

    /// The same, on an endpoint already bound: the lanes' connections come
    /// from its id, which must be the host's device.
    pub(crate) fn at(endpoint: iroh::Endpoint) -> Self {
        let carried = Self::new(*endpoint.id().as_bytes(), None);
        let _ = carried.endpoint.set(endpoint);
        carried
    }

    /// One turn of a page's call: what the page sealed goes onto the call's
    /// lanes, and what the call's devices sent for it comes back.
    ///
    /// `devices` is who the workspace admits: the call's lanes open to them,
    /// and only they would be answered here.
    ///
    /// # Errors
    ///
    /// [`Failed`] when no lane can be opened from this host, or when the
    /// page's bridge refuses the turn.
    pub(crate) async fn turn(&self, devices: Vec<[u8; 32]>, up: Up) -> Result<Down, Failed> {
        let endpoint = self.endpoint().await?;
        let key = (up.workspace.clone(), up.call.clone());
        let carried = {
            let mut calls = self.calls.lock_recover();
            Arc::clone(calls.entry(key.clone()).or_insert_with(|| {
                Arc::new(AsyncMutex::new(Carried {
                    bridge: Bridge::new(
                        up.workspace.clone(),
                        up.channel,
                        up.call.clone(),
                        self.device,
                        Roster::new(devices.clone()),
                    ),
                    dialled: BTreeSet::new(),
                    dialling: BTreeSet::new(),
                }))
            }))
        };
        let handle = Arc::clone(&carried);
        let mut carried = carried.lock().await;
        carried
            .bridge
            .set_roster(Roster::new(devices.iter().copied()));
        for device in devices {
            if device == self.device
                || carried.dialled.contains(&device)
                || !carried.dialling.insert(device)
            {
                continue;
            }
            let bridge = Arc::clone(&carried.bridge);
            let endpoint = endpoint.clone();
            let opening = Arc::clone(&handle);
            // A device that is not in the call, or cannot be reached, refuses
            // or never answers: the page's turn does not wait on it, and the
            // lane is tried again on a later turn.
            tokio::spawn(async move {
                let Ok(remote) = iroh::EndpointId::from_bytes(&device) else {
                    return;
                };
                let dialled = bridge
                    .dial(&endpoint, EndpointAddr::new(remote))
                    .await
                    .is_ok();
                let mut carried = opening.lock().await;
                carried.dialling.remove(&device);
                if dialled {
                    carried.dialled.insert(device);
                }
            });
        }
        let outcome = carried
            .bridge
            .exchange(up)
            .await
            .map_err(|error| Failed::Refused(error.to_string()));
        drop(carried);
        if outcome.is_err() {
            // The call is over, or this bridge will not carry it: the page
            // starts it afresh if it ever turns again.
            let mut calls = self.calls.lock_recover();
            if calls
                .get(&key)
                .is_some_and(|held| Arc::ptr_eq(held, &handle))
            {
                calls.remove(&key);
            }
        }
        outcome
    }

    /// The lanes' endpoint, bound on first use with this host's device key: the
    /// id the workspaces it serves admit as a member.
    async fn endpoint(&self) -> Result<iroh::Endpoint, Failed> {
        let endpoint =
            self.endpoint
                .get_or_try_init(|| async {
                    let key = self.key.clone().ok_or_else(|| {
                        Failed::NoLane("the lanes' endpoint was handed over".into())
                    })?;
                    // What `presets::N0` builds, with its publisher behind number
                    // 0's directory gate ([`tilo_endpoint_reachability`]); the host
                    // keeps the default relays, since its members reach it there.
                    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                        .secret_key(key)
                        .address_lookup(tilo_endpoint_reachability::collaborating())
                        .address_lookup(iroh::address_lookup::DnsAddressLookup::n0_dns())
                        .relay_mode(iroh::endpoint::default_relay_mode())
                        .bind()
                        .await
                        .map_err(|error| Failed::NoLane(error.to_string()))
                })
                .await?;
        Ok(endpoint.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::time::Duration;
    use tilo_call_lane::{Calls, Hello, Lane};

    const WORKSPACE: &str = "11111111-1111-4111-8111-111111111111";
    const CALL: &str = "call";

    /// A loopback endpoint that finds its peers through `lookup`.
    async fn bound(
        seed: u8,
        lookup: &iroh::address_lookup::memory::MemoryLookup,
    ) -> iroh::Endpoint {
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(iroh::SecretKey::from_bytes(&[seed; 32]))
            .address_lookup(lookup.clone())
            .relay_mode(iroh::RelayMode::Disabled)
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .expect("a loopback address")
            .bind()
            .await
            .expect("a loopback endpoint binds");
        let port = endpoint.bound_sockets()[0].port();
        lookup.add_endpoint_info(
            iroh::EndpointAddr::new(endpoint.id())
                .with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port))),
        );
        endpoint
    }

    /// A page's turn reaches the call's lane, and what the call's device sends
    /// comes back on the next one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_turn_reaches_the_calls_lanes_and_brings_back_what_they_carry() {
        let lookup = iroh::address_lookup::memory::MemoryLookup::new();
        let host = bound(0x71, &lookup).await;
        let phone = bound(0x72, &lookup).await;
        let host_device = *host.id().as_bytes();
        let phone_device = *phone.id().as_bytes();
        let channel = collab_types::ChannelId::new();
        let roster = Roster::new([host_device, phone_device]);

        // The call's other device, as a native one takes part: it answers the
        // lanes the host opens under the call's protocol.
        let (calls, accepted) = Calls::new(
            Hello {
                workspace: WORKSPACE.to_owned(),
                channel,
                call: CALL.to_owned(),
                lane: Lane::Audio,
            },
            roster.clone(),
        );
        let answering = iroh::protocol::Router::builder(phone.clone())
            .accept(tilo_call_lane::ALPN, calls)
            .spawn();
        let (heard, mut carried) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let sent = vec![9_u8; 4];
        let wired = sent.clone();
        tokio::spawn(async move {
            let mut accepted = accepted;
            while let Some(incoming) = accepted.recv().await {
                if incoming.lane() == Lane::Audio {
                    let _ = incoming.send(wired.clone());
                }
                let heard = heard.clone();
                tokio::spawn(async move {
                    while let Ok(datagram) = incoming.received().await {
                        let _ = heard.send(datagram.to_vec());
                    }
                });
            }
        });

        let lane = CallLane::at(host.clone());
        let page_turn = || Up {
            workspace: WORKSPACE.to_owned(),
            call: CALL.to_owned(),
            channel,
            audio: vec![vec![1, 2, 3]],
            ..Up::default()
        };

        // Lanes open on the first turn: the page's frames reach the call,
        // and what the call's device sent comes back for the page, named by
        // the device that sent it. A turn takes what arrived since the last
        // one, so both directions are read off the turns as they come.
        let (heard_page, heard_phone) = tokio::time::timeout(Duration::from_secs(20), async {
            let (mut heard_page, mut heard_phone) = (false, false);
            loop {
                let down = lane
                    .turn(vec![host_device, phone_device], page_turn())
                    .await
                    .expect("the host carries the page's call");
                if !heard_page && let Ok(played) = carried.try_recv() {
                    assert_eq!(played, vec![1, 2, 3]);
                    heard_page = true;
                }
                heard_phone |= down
                    .audio
                    .iter()
                    .any(|carried| carried.from == phone_device && carried.datagram == sent);
                if heard_page && heard_phone {
                    break (heard_page, heard_phone);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the page and the call hear each other");
        assert!(heard_page && heard_phone);
        let _ = answering.shutdown().await;
    }
}
