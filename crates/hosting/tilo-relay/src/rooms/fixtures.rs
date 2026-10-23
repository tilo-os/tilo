//! A governing host and the devices around it, for the tests of rooms and of
//! the planes embedders build on them.
#![allow(clippy::expect_used, reason = "a fixture failing is its test failing")]

use std::path::Path;
use std::time::SystemTime;

use ed25519_dalek::SigningKey;
use iroh::protocol::Router;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

use super::Rooms;
use crate::host::{HostConfig, HostIdentity, HostMode, RoomHost};

/// A host certified only to host rooms: it governs them without stewarding
/// their channels.
#[must_use]
pub fn governor() -> (SigningKey, DeviceCert) {
    governor_with(DeviceCapabilities::host())
}

/// A host device of the person whose root is `[1; 32]`, certified with
/// `capabilities`.
///
/// # Panics
///
/// The certificate cannot be signed.
#[must_use]
pub fn governor_with(capabilities: DeviceCapabilities) -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[1; 32]);
    let key = SigningKey::from_bytes(&[2; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "home".into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).expect("a fixture certificate signs");
    (key, cert)
}

/// A certified room host on its own endpoint, routed as an embedder routes
/// it.
pub struct TestHost {
    host: RoomHost,
    router: Router,
}

impl TestHost {
    /// The rooms it governs.
    #[must_use]
    pub const fn rooms(&self) -> &Rooms {
        self.host.rooms()
    }

    /// The host itself.
    #[must_use]
    pub const fn host(&self) -> &RoomHost {
        &self.host
    }

    /// The durable records of its rooms.
    #[must_use]
    pub fn room_records(&self) -> crate::room_records::RoomRecords {
        self.host.room_records()
    }

    /// What a member dials.
    #[must_use]
    pub fn node_addr(&self) -> iroh_base::EndpointAddr {
        self.host.node_addr()
    }

    /// The compactor of its workspaces.
    #[cfg(test)]
    pub(crate) fn compactor(&self) -> &crate::compaction::Compactor {
        self.host.compactor()
    }

    /// Stop the host, then its router and endpoint.
    pub async fn shutdown(self) {
        self.host.shutdown().await;
        let _closed = self.router.shutdown().await;
    }
}

/// A certified host of `cert`'s device keeping its state under `state_root`.
///
/// # Panics
///
/// The endpoint cannot bind or the host cannot open.
pub async fn test_host(state_root: &Path, key: &SigningKey, cert: &DeviceCert) -> TestHost {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .bind()
        .await
        .expect("a loopback endpoint binds");
    let host = RoomHost::open(
        HostConfig::new(state_root.to_owned(), HostMode::CertifiedHost),
        HostIdentity {
            user_public_key: cert.user_public_key,
            device_signing_key: key.clone(),
            device_cert: cert.clone(),
            revocations: RevocationList::new(),
        },
        endpoint.clone(),
    )
    .await
    .expect("the host opens");
    let mut router = Router::builder(endpoint);
    for (alpn, handler) in host.protocols() {
        router = router.accept(alpn, handler);
    }
    TestHost {
        host,
        router: router.spawn(),
    }
}

/// Admit `member` to `room` as a reader, as the governor `key` decides it.
///
/// # Panics
///
/// The room is not governed here, or refuses the certificate.
pub async fn admit_view_reader(rooms: &Rooms, room: &str, key: &SigningKey, member: &DeviceCert) {
    let state = rooms.room(room).await.expect("a governed room");
    let mut room = state.lock().await;
    let now = SystemTime::now();
    let mut candidate = room.peer().clone();
    candidate
        .admit_certificate(member.clone(), key, now)
        .expect("the room admits the certificate");
    let collaboration = room.collaboration().clone();
    room.commit(candidate, key, now, |_, _| Ok(collaboration))
        .expect("the admission commits");
}

/// Until when `room` restricts `person`, as `host` reads its policy.
///
/// # Panics
///
/// The room is not governed here.
pub async fn restricted_until(
    rooms: &Rooms,
    room: &str,
    host: &DeviceCert,
    person: [u8; 32],
) -> Option<u64> {
    let state = rooms.room(room).await.expect("a governed room");
    let now = SystemTime::now();
    let permissions = state
        .lock()
        .await
        .peer()
        .permissions(host.device_public_key, now)
        .expect("the host's permissions");
    permissions.policy.restricted_until(
        &hex::encode(person),
        &tilo_action_policy::Context::at_time(now),
    )
}
