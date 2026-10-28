//! "Which of my devices are here?" on the local network: how one person's
//! devices find each other's endpoint addresses without telling anyone else
//! whose devices they are, or how many.
//!
//! Finding a device is not trusting it. A found address is dialed like any
//! other: the transport authenticates the endpoint key, and the device's
//! certificate and the person's revocations are checked before anything
//! syncs. Discovery only saves the round trip through a relay, and lets two
//! of the person's devices meet on a network with no internet at all.
//!
//! # Exchange
//!
//! 1. A device broadcasts a probe to UDP [`PORT`], asking for up to
//!    [`SLOTS`] of the person's devices it knows.
//! 2. A device that finds a slot sealed to it opens the probe, learns which
//!    device asked and checks that the asker is one of the person's current
//!    devices ([`OwnDevices::admits`]). It answers once per probe, sealed to
//!    the asker, naming its endpoint's addresses. It also learns the asker:
//!    a device enrolled after this one is found by it this way.
//! 3. The asker opens the reply, checks that a device it asked for signed
//!    it, and dials that device at the addresses it names.
//!
//! # Probe
//!
//! `"TLOLINK2" || epoch (u32 LE) || salt (16) || slot[0] … slot[7] || body`,
//! [`DATAGRAM_LEN`] bytes. `salt` is fresh for every probe and `epoch`
//! counts ten-minute steps (see *Freshness*). For each device `P` asked
//! for, a slot is the probe's fresh body key `K` sealed to `X25519(P)` with
//! HPKE ([`tilo_seal::Label::LanDiscoveryProbe`], the header as associated
//! data); `X25519(·)` is the device's Ed25519 key in its X25519 form, as
//! the personal device fabric seals to a device. Unused slots are boxes of
//! random bytes to keys nobody holds, and the slots are sorted, so the
//! probe says nothing about how many devices the person has or which slot
//! is whose. The body is a `tilo-seal` envelope under `K`, bound to the
//! header and slots, holding the asking device's certificate (padded to a
//! fixed length) and its device key's signature over the header and slots:
//! a collaborator who has seen the certificate cannot ask in its name.
//!
//! A device recognises a probe made for it with one key agreement per
//! slot. Nothing is shared across the fleet: a device that left it or was
//! revoked learns nothing from probes not sealed to it, and is not
//! answered once the person's devices refuse its certificate. No secret
//! has to be redistributed.
//!
//! # Reply
//!
//! `"TLOLNKR2" || salt || HPKE box`, also [`DATAGRAM_LEN`] bytes, so a
//! reply is no larger than the probe that caused it. The box is sealed to
//! `X25519(asker)` ([`tilo_seal::Label::LanDiscoveryReply`], associated data
//! `"TLOLNKR2" || salt || asker`) and holds, padded, the answering device's
//! key, its endpoint's addresses and its signature over the salt, both keys
//! and the addresses, which proves to the asker that the device it asked
//! answered, not a stranger redirecting its dial.
//!
//! # Freshness
//!
//! A probe recorded and replayed later must not make the person's devices
//! give away that they are there ([`Freshness`]):
//!
//! - A device whose clock is synchronised to a time source sends its
//!   epoch and accepts probes within one epoch of it.
//! - A device without one (a board with no battery-backed clock on a
//!   network without a time source) does not send its clock, which may be
//!   the image's build date and would single it out. It sends the newest
//!   epoch it learned from one of the person's devices, advanced by the
//!   time since, or 0.
//! - Every device remembers, per asking device and across restarts, the
//!   newest epoch it accepted from it, and refuses older ones.
//! - Each probe is answered once, and at most a fixed number of probes
//!   are opened per second.
//!
//! What remains: a probe recorded from a device never heard from at a newer
//! epoch, replayed to a device with no time source, still gets one answer
//! per recording. The answer is sealed to the device that made the probe.

#![cfg_attr(
    test,
    allow(
        clippy::arithmetic_side_effects,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

mod freshness;
mod service;
mod wire;

pub use freshness::{EPOCH, Freshness, epoch_at};
pub use service::{Config, Found, LanDiscovery, OwnDevices, PORT, PROVENANCE};
pub use wire::{DATAGRAM_LEN, MAX_REPLY_ADDRS, SLOTS};

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use ed25519_dalek::SigningKey;
    use iroh::address_lookup::{AddressLookup as _, EndpointData};
    use iroh::{EndpointId, TransportAddr};
    use n0_future::StreamExt as _;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

    use super::*;

    struct Person {
        root: [u8; 32],
        cert: DeviceCert,
        known: Mutex<Vec<[u8; 32]>>,
        revoked: Mutex<Vec<[u8; 32]>>,
    }

    impl OwnDevices for Person {
        fn certificate(&self) -> Option<DeviceCert> {
            Some(self.cert.clone())
        }
        fn devices(&self) -> Vec<[u8; 32]> {
            self.known.lock().unwrap().clone()
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

    fn cert(root: &SigningKey, device: &SigningKey) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Device".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    fn free_port() -> u16 {
        std::net::UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    struct Device {
        key: [u8; 32],
        person: Arc<Person>,
        discovery: LanDiscovery,
        found: tokio::sync::mpsc::Receiver<Found>,
    }

    /// A device listening on its own loopback port and probing `peer_port`:
    /// two devices on one host stand in for a broadcast domain.
    fn device(root: &SigningKey, seed: u8, port: u16, peer_port: u16) -> Device {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let person = Arc::new(Person {
            root: root.verifying_key().to_bytes(),
            cert: cert(root, &key),
            known: Mutex::default(),
            revoked: Mutex::default(),
        });
        let (discovery, found) = LanDiscovery::spawn(
            &tokio::runtime::Handle::current(),
            key.clone(),
            person.clone(),
            Config {
                port,
                broadcast: SocketAddrV4::new(Ipv4Addr::LOCALHOST, peer_port),
                state: None,
            },
        )
        .unwrap();
        let listen: SocketAddr = format!("10.0.0.{seed}:4433").parse().unwrap();
        discovery.publish(&EndpointData::new(vec![TransportAddr::Ip(listen)]));
        Device {
            key: key.verifying_key().to_bytes(),
            person,
            discovery,
            found,
        }
    }

    async fn next(found: &mut tokio::sync::mpsc::Receiver<Found>) -> Option<Found> {
        tokio::time::timeout(Duration::from_secs(5), found.recv())
            .await
            .ok()
            .flatten()
    }

    #[tokio::test]
    async fn a_device_finds_an_older_one_which_learns_it_in_turn() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let (a_port, b_port) = (free_port(), free_port());
        let mut older = device(&root, 2, a_port, b_port);
        let newer = device(&root, 3, b_port, a_port);
        // The newer device was enrolled knowing the older one; the older
        // one has never heard of it.
        newer.person.known.lock().unwrap().push(older.key);

        let id = EndpointId::from_bytes(&older.key).unwrap();
        let mut resolved = newer.discovery.resolve(id).expect("a device of the person");
        let item = tokio::time::timeout(Duration::from_secs(5), resolved.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(item.endpoint_id(), id);
        assert_eq!(
            item.ip_addrs().copied().collect::<Vec<_>>(),
            vec!["10.0.0.2:4433".parse::<SocketAddr>().unwrap()]
        );
        match next(&mut older.found).await {
            Some(Found::Asked(cert)) => assert_eq!(cert.device_public_key, newer.key),
            other => panic!("the older device should learn the newer one, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn strangers_and_revoked_devices_are_not_answered() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let stranger_root = SigningKey::from_bytes(&[9; 32]);
        let (a_port, b_port, c_port) = (free_port(), free_port(), free_port());
        let mut own = device(&root, 2, a_port, b_port);
        let stranger = device(&stranger_root, 3, b_port, a_port);
        stranger.person.known.lock().unwrap().push(own.key);
        let id = EndpointId::from_bytes(&own.key).unwrap();
        let mut resolved = stranger.discovery.resolve(id).unwrap();
        assert!(resolved.next().await.is_none(), "another person's device");
        assert!(own.found.try_recv().is_err());

        let revoked = device(&root, 4, c_port, a_port);
        revoked.person.known.lock().unwrap().push(own.key);
        own.person.revoked.lock().unwrap().push(revoked.key);
        let mut resolved = revoked.discovery.resolve(id).unwrap();
        assert!(resolved.next().await.is_none(), "a revoked device");
        assert!(own.found.try_recv().is_err());
    }

    #[tokio::test]
    async fn only_the_persons_devices_are_resolved() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let one = device(&root, 2, free_port(), free_port());
        let unknown =
            EndpointId::from_bytes(&SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes())
                .unwrap();
        assert!(one.discovery.resolve(unknown).is_none());
    }
}
