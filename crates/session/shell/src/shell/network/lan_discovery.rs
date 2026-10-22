//! Private LAN discovery of this person's own devices
//! ([`tilo_lan_discovery`]) wired into the endpoint: it looks for the devices
//! this session counts as the person's, and a device that asks for this one
//! is counted as theirs and dialed.

use std::path::Path;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use super::own_devices::own_device_sources;
use super::{HostIdentity, NetCmd, now_unix_nanos, personal_device_peers};

/// The person's devices as LAN discovery sees them: this session's own
/// device set ([`personal_device_peers`]), shared with the network.
struct DiscoveryTrust {
    person: [u8; 32],
    cert: tilo_user_identity::DeviceCert,
    fleet: Option<crate::device_fleet::DeviceFleetStore>,
    peers: Arc<Mutex<personal_device_peers::PersonalDevicePeers>>,
}

impl DiscoveryTrust {
    fn devices(&self) -> Vec<[u8; 32]> {
        let local = self.cert.device_public_key;
        self.peers.lock().map_or_else(
            |_| Vec::new(),
            |mut peers| {
                peers
                    .devices_at(|| own_device_sources(Some(self.person), self.fleet.as_ref()))
                    .iter()
                    .copied()
                    .filter(|device| *device != local)
                    .collect()
            },
        )
    }
}

impl tilo_lan_discovery::OwnDevices for DiscoveryTrust {
    fn certificate(&self) -> Option<tilo_user_identity::DeviceCert> {
        Some(self.cert.clone())
    }

    fn devices(&self) -> Vec<[u8; 32]> {
        DiscoveryTrust::devices(self)
    }

    fn admits(&self, cert: &tilo_user_identity::DeviceCert) -> bool {
        cert.user_public_key == self.person
            && cert.device_public_key != self.cert.device_public_key
            && cert.capabilities.is_personal_device()
            && tilo_user_identity::verify_device_cert_at(cert, now_unix_nanos()).is_ok()
            && self
                .peers
                .lock()
                .is_ok_and(|peers| !peers.is_revoked(&cert.device_public_key))
    }
}

/// Start private LAN discovery of the person's own devices
/// ([`tilo_lan_discovery`]), registered with `endpoint`'s address lookup,
/// and act on what it finds: dial a device that answered, and count a device
/// that asked for this one as the person's, then dial it.
pub(super) fn start(
    runtime: &tokio::runtime::Runtime,
    endpoint: &iroh::Endpoint,
    identity: &HostIdentity,
    fleet: Option<crate::device_fleet::DeviceFleetStore>,
    peers: &Arc<Mutex<personal_device_peers::PersonalDevicePeers>>,
    state_dir: &Path,
    cmd_tx: &mpsc::Sender<NetCmd>,
) -> Option<tilo_lan_discovery::LanDiscovery> {
    let trust = Arc::new(DiscoveryTrust {
        person: identity.device_cert.user_public_key,
        cert: identity.device_cert.clone(),
        fleet,
        peers: Arc::clone(peers),
    });
    let state = state_dir.join("lan-discovery.json");
    let (discovery, mut found) = match tilo_lan_discovery::LanDiscovery::spawn(
        runtime.handle(),
        identity.device_signing_key.clone(),
        trust,
        tilo_lan_discovery::Config::lan(Some(state)),
    ) {
        Ok(started) => started,
        Err(error) => {
            tracing::warn!(%error, "LAN discovery of this person's devices could not start");
            return None;
        }
    };
    let Ok(address_lookup) = endpoint.address_lookup() else {
        tracing::warn!("LAN discovery: the endpoint has no address lookup registry");
        return None;
    };
    address_lookup.add(discovery.clone());
    discovery.probe(None);
    let peers = Arc::clone(peers);
    let tx = cmd_tx.clone();
    runtime.spawn(async move {
        while let Some(found) = found.recv().await {
            let addr = match found {
                tilo_lan_discovery::Found::Answered(addr) => addr,
                tilo_lan_discovery::Found::Asked(cert) => {
                    if let Ok(mut peers) = peers.lock() {
                        peers.remember(cert.device_public_key);
                    }
                    let Ok(id) = iroh::EndpointId::from_bytes(&cert.device_public_key) else {
                        continue;
                    };
                    iroh::EndpointAddr::new(id)
                }
            };
            if tx.send(NetCmd::Connect { addr }).await.is_err() {
                return;
            }
        }
    });
    tracing::info!("peer-sync: private LAN discovery of this person's devices started");
    Some(discovery)
}
