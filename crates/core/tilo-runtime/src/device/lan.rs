//! The person's other devices heard on this device's local network, as its daemon publishes them
//! for its person's shell: what a device row says is nearby, so reachable here without a server.
//!
//! The endpoint's local discovery does the hearing ([`tilo_endpoint_reachability::lan`]): it asks
//! the network for the keys this device dials and answers the devices whose certificate this
//! device admits, so nothing on the network learns who owns a device or how many there are. What
//! this keeps is the answer: the keys heard, published as the reading their shell follows, and it
//! drops to none as a device lapses.
//!
//! A device is admitted only while the person's record still certifies it: what a device heard
//! before the person took it back leaves the reading at once.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tilo_endpoint_reachability::{Fleet, Nearby};
use tilo_lock::MutexRecover as _;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::workspaces::FollowedRecord;
use tilo_workspace_service::workspaces::person_workspace::devices::{Devices, NEARBY};

use super::reading::{Changes, Read, Served};

/// The local discovery of this device's endpoint, and the devices it heard: the lookup the
/// endpoint binds with, and the reading the person's shell follows.
pub(crate) struct Lan {
    /// The certificate this device presents in its probes.
    cert: DeviceCert,
    /// What it heard from the local network.
    nearby: Nearby,
    /// The record of the person's devices, once this daemon follows it: what admits a device, and
    /// what takes one back.
    record: Mutex<Option<Devices>>,
    /// Moves with every change of the reading: what a follower reads again.
    moved: tokio::sync::watch::Sender<u64>,
}

impl Lan {
    /// The discovery of this device `cert`, speaking on every network its endpoint is on: what its
    /// daemon runs, told no networks but what [`tilo_endpoint_reachability::NETWORKS_ENV`] names,
    /// when it names any.
    #[must_use]
    pub(crate) fn new(cert: DeviceCert) -> Arc<Self> {
        Self::hearing(cert, None)
    }

    /// [`Self::new`], speaking only on `networks`: what a test keeps on this machine's loopback.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn on(cert: DeviceCert, networks: impl IntoIterator<Item = Ipv4Addr>) -> Arc<Self> {
        Self::hearing(cert, Some(networks.into_iter().collect()))
    }

    fn hearing(cert: DeviceCert, networks: Option<Vec<Ipv4Addr>>) -> Arc<Self> {
        let (moved, _) = tokio::sync::watch::channel(0_u64);
        let telling = moved.clone();
        let changed: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            telling.send_modify(|moved| *moved = moved.wrapping_add(1));
        });
        let nearby = match networks {
            Some(networks) => Nearby::on(Arc::clone(&changed), networks),
            None => Nearby::from_env(Arc::clone(&changed)).unwrap_or_else(|| Nearby::new(changed)),
        };
        Arc::new(Self {
            cert,
            nearby,
            record: Mutex::default(),
            moved,
        })
    }

    /// The lookup this device's endpoint binds with: it asks the network for a key it dials and
    /// answers the devices the person's record admits, keeping what it heard here.
    #[must_use]
    pub(crate) fn lookup(self: &Arc<Self>) -> tilo_endpoint_reachability::lan::Lan {
        self.nearby.lookup(Arc::clone(self) as Arc<dyn Fleet>)
    }

    /// Admit the devices `record` certifies, from now on: what this daemon follows in the
    /// person's own workspace.
    pub(crate) fn admitting(self: &Arc<Self>, record: FollowedRecord) {
        let now = record.state::<Devices>().ok();
        self.keeping(now);
        let keeping = Arc::downgrade(self);
        let mut changes = record.changes();
        tilo_rt::app().spawn(async move {
            while changes.changed().await.is_ok() {
                let Some(lan) = keeping.upgrade() else {
                    return;
                };
                lan.keeping(record.state::<Devices>().ok());
            }
        });
    }

    /// Keep `record` as what admits a device, and publish the reading if that changed any.
    fn keeping(&self, record: Option<Devices>) {
        *self.record.lock_recover() = record;
        self.moved
            .send_modify(|moved| *moved = moved.wrapping_add(1));
    }

    /// The person's devices heard on this device's local network now, its own aside, each still
    /// certified by their record.
    fn heard(&self) -> BTreeSet<[u8; 32]> {
        let certified: BTreeSet<[u8; 32]> = self
            .record
            .lock_recover()
            .as_ref()
            .map(|record| {
                record
                    .certified_at(&self.cert.user_public_key, tilo_clock::unix_now_ns())
                    .into_iter()
                    .map(|cert| cert.device_public_key)
                    .collect()
            })
            .unwrap_or_default();
        self.nearby
            .heard()
            .into_iter()
            .map(|device| *device.as_bytes())
            .filter(|device| certified.contains(device))
            .filter(|device| *device != self.cert.device_public_key)
            .collect()
    }

    /// The reading the person's shell follows over the shell link.
    pub(crate) fn published(self: &Arc<Self>) -> Served {
        let reading = Arc::clone(self);
        let changing = Arc::clone(self);
        let read: Arc<Read> =
            Arc::new(move |_caller: &tilo_app_host::services::Caller| reading.reading());
        let changes: Arc<Changes> = Arc::new(move || changing.moved.subscribe());
        Served::new(NEARBY, read, changes)
    }

    fn reading(&self) -> Value {
        json!({
            "devices": self
                .heard()
                .into_iter()
                .map(hex::encode)
                .collect::<Vec<_>>(),
        })
    }
}

impl Fleet for Lan {
    fn certificate(&self) -> Option<DeviceCert> {
        Some(self.cert.clone())
    }

    fn admits(&self, cert: &DeviceCert) -> bool {
        cert.user_public_key == self.cert.user_public_key
            && tilo_user_identity::verify_device_cert(cert).is_ok()
            && self.record.lock_recover().as_ref().is_some_and(|record| {
                record
                    .listed(&self.cert.user_public_key)
                    .into_iter()
                    .any(|listed| {
                        listed.cert.device_public_key == cert.device_public_key
                            && !listed.device.revoked(&listed.cert)
                    })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use iroh::endpoint::presets;
    use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey};
    use tilo_user_identity::{DeviceCapabilities, sign_device_cert};
    use tilo_workspace_service::workspaces::person_workspace::devices::Devices;

    /// The protocol the two endpoints meet on: what a probe's answer makes dialable.
    const ALPN: &[u8] = b"tilo-lan-test/1";

    /// `device`'s certificate, signed by the person's root.
    fn certified(root: &SigningKey, device: [u8; 32], label: &str) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device,
            device_label: label.into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, root).expect("the person's root signs the device");
        cert
    }

    /// The person's record of their devices, certifying each of `certs`.
    fn record(certs: &[DeviceCert]) -> Devices {
        let mut devices = tilo_resource::Replica::<Devices>::new().in_app("so.tilo.model", "Model");
        for cert in certs {
            devices
                .apply(Devices::keeping(cert).expect("the device certificate encodes"))
                .expect("the record keeps the device");
        }
        devices.state().expect("the record reads")
    }

    /// The endpoint `key`'s device runs on, answering `ALPN` and hearing `lan`'s discovery.
    async fn endpoint(key: &SigningKey, lan: &Arc<Lan>) -> Endpoint {
        Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::from_bytes(&key.to_bytes()))
            .relay_mode(RelayMode::Disabled)
            .alpns(vec![ALPN.to_vec()])
            .address_lookup(lan.lookup())
            .bind()
            .await
            .expect("the endpoint binds")
    }

    /// Answer every connection `endpoint` hears, so a dial that found it completes.
    fn answering(endpoint: &Endpoint) {
        let endpoint = endpoint.clone();
        tokio::spawn(async move {
            while let Some(connecting) = endpoint.accept().await {
                tokio::spawn(async move {
                    if let Ok(connection) = connecting.await {
                        let _ = connection.accept_bi().await;
                    }
                });
            }
        });
    }

    /// `from` dials `to` by key alone until the local network answers where it is, for up to
    /// `within`.
    async fn reaches(from: &Endpoint, to: iroh::EndpointId, within: std::time::Duration) -> bool {
        let until = std::time::Instant::now()
            .checked_add(within)
            .expect("a test window fits in the clock");
        while std::time::Instant::now() < until {
            let dialled = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                from.connect(EndpointAddr::new(to), ALPN),
            )
            .await;
            if let Ok(Ok(connection)) = dialled {
                // The answering device opens no stream of its own: reaching it is the whole
                // question, so the connection ends here.
                drop(connection);
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        false
    }

    /// Two devices of one person, each with its own endpoint and its own discovery, find each
    /// other on this machine's loopback, and the reading the person's shell follows names the
    /// other; a device the record does not certify is answered by neither.
    #[test]
    fn two_devices_of_one_person_hear_each_other_on_the_local_network() {
        let root = SigningKey::from_bytes(&[1; 32]);
        // Each device's endpoint runs on its key, and its certificate names that key's public half.
        let (mine_key, other_key, stranger_key) = (
            SigningKey::from_bytes(&[2; 32]),
            SigningKey::from_bytes(&[3; 32]),
            SigningKey::from_bytes(&[4; 32]),
        );
        let mine = mine_key.verifying_key().to_bytes();
        let other = other_key.verifying_key().to_bytes();
        let stranger = stranger_key.verifying_key().to_bytes();
        let my_cert = certified(&root, mine, "Laptop");
        let other_cert = certified(&root, other, "Phone");
        let stranger_cert = certified(&root, stranger, "Stranger");
        let record = record(&[my_cert.clone(), other_cert.clone()]);
        let a = Lan::on(my_cert.clone(), [Ipv4Addr::LOCALHOST]);
        a.keeping(Some(record.clone()));
        let b = Lan::on(other_cert.clone(), [Ipv4Addr::LOCALHOST]);
        b.keeping(Some(record));
        // The stranger's own discovery: it asks, and the person's devices answer it nothing.
        let c = Lan::on(stranger_cert, [Ipv4Addr::LOCALHOST]);
        c.keeping(Some(Devices::default()));
        assert!(
            a.admits(&other_cert),
            "the person's record admits the phone"
        );
        assert!(b.admits(&my_cert), "and the laptop");
        tilo_rt::app()
            .block_on(async {
                let endpoint_a = endpoint(&mine_key, &a).await;
                let endpoint_b = endpoint(&other_key, &b).await;
                let endpoint_c = endpoint(&stranger_key, &c).await;
                assert!(
                    endpoint_a.addr().ip_addrs().next().is_some(),
                    "the endpoint listens somewhere"
                );
                for answered in [&endpoint_a, &endpoint_b, &endpoint_c] {
                    answering(answered);
                }
                assert!(
                    reaches(
                        &endpoint_a,
                        endpoint_b.id(),
                        std::time::Duration::from_secs(30)
                    )
                    .await,
                    "the laptop finds the phone on the local network: heard {:?}",
                    a.heard()
                );
                assert!(
                    reaches(
                        &endpoint_b,
                        endpoint_a.id(),
                        std::time::Duration::from_secs(30)
                    )
                    .await,
                    "and the phone finds the laptop: heard {:?}",
                    b.heard()
                );
                assert!(
                    !reaches(
                        &endpoint_c,
                        endpoint_a.id(),
                        std::time::Duration::from_secs(6)
                    )
                    .await,
                    "a device the record does not certify is answered by nobody"
                );
                assert!(
                    !b.heard().contains(&stranger),
                    "and the phone does not hear it"
                );
                let reading = a.reading();
                let heard: Vec<String> = serde_json::from_value(
                    reading
                        .get("devices")
                        .cloned()
                        .expect("the reading lists the devices heard"),
                )
                .expect("the heard devices are keys");
                assert_eq!(
                    heard,
                    [hex::encode(other)],
                    "the reading the person's shell follows names the other device"
                );
            })
            .expect("the runtime runs");
    }
}
