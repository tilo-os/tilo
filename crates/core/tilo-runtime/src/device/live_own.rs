//! The scopes of the person's own devices, as this daemon carries its desktop into them.
//!
//! Each of the person's devices relays a scope of its own in their own workspace
//! ([`Scope::device`]); what one of their devices sends another of theirs alone — its keyboard and
//! pointer, a camera's picture, a game controller — crosses there. The desktop session holds no
//! key, so it opens a link toward the scope ([`link::Toward::Device`]) and this daemon joins it as
//! the device, sealed under the person's own workspace's keys: publishing at the other device's
//! relay, dialled from this device's endpoint, or reading this device's own scope on the relay it
//! runs. Who this device's own scope hears is the person's devices as the one identity system
//! names them: the record of their devices, each certificate signed by their root, less the ones
//! they took back ([`super::own_devices`]). The other device's relay judges this device the same
//! way ([`super::members`]), so a device of another person is admitted by neither end.

use std::sync::Arc;

use collab_types::scope::Scope;
use iroh::{EndpointAddr, EndpointId};
use tilo_live::link;
use tilo_live::live_session::{Join, LiveError, LiveSession};
use tilo_live::native::Iroh;
use tilo_live::roster::Roster;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::workspaces::person_workspace::devices::Devices;

use super::{Live, SessionKeys, now_ns, own_devices, person_workspace, session_keys};

impl Live {
    /// Carry `link`, a link the desktop opened into the scope of its person's device `device`,
    /// until either ends: the scope is joined as this device and the desktop's objects go in
    /// unsealed, what it hears comes back opened. A scope that cannot be joined closes the link
    /// saying why, which the desktop's open reads as a failure and tries again later.
    pub(super) async fn carry_device_scope(&self, device: [u8; 32], link: link::Session) {
        let session = match self.join_device_scope(device).await {
            Ok(session) => Arc::new(session),
            Err(why) => {
                tracing::debug!(device = %hex::encode(device), why,
                    "the scope of the person's device was not joined for the desktop");
                link.close(0, why);
                return;
            }
        };
        let here = device == self.shared.device;
        let roster = async {
            // This device's own scope hears the person's devices as their record names them now: a
            // device they add is heard from then on, one they take back is not.
            let Some(person) = self.shared.person.get().filter(|_| here) else {
                return std::future::pending().await;
            };
            let mut moved = person.changes();
            while moved.changed().await.is_ok() {
                session.set_roster(self.heard_devices());
            }
            std::future::pending::<()>().await;
        };
        tokio::select! {
            served = tilo_live::link_call::serve(link, Arc::clone(&session)) => {
                if served.is_err() {
                    tracing::debug!(device = %hex::encode(device),
                        "a desktop's link into its person's device's scope ended unserved");
                }
            }
            () = roster => {}
        }
    }

    /// The scope of the person's device `device`, joined as this device: at that device's relay,
    /// publishing, or this device's own on the relay it runs, reading.
    async fn join_device_scope(&self, device: [u8; 32]) -> Result<LiveSession, &'static str> {
        let cert = &self.shared.cert;
        let own = self.shared.device;
        let workspace = person_workspace(&cert.user_public_key);
        let SessionKeys::Ready(keys) = session_keys(&self.workspaces, workspace) else {
            return Err("the person's own workspace's keys are not here");
        };
        let scope = Scope::device(workspace.to_string(), device);
        let epoch = keys.epoch();
        let here = device == own;
        let join = Join {
            scope: scope.clone(),
            device: own,
            keys,
            roster: if here {
                self.heard_devices()
            } else {
                Roster::new([])
            },
            publish: !here,
            presence: false,
        };
        let joined = if here {
            LiveSession::local(&self.relay, &join)
        } else {
            let grant = own_grant(cert, workspace, &scope, epoch)
                .ok_or("this device's grant for its person's devices could not be made")?;
            let id = EndpointId::from_bytes(&device)
                .map_err(|_| "a device key that names no endpoint")?;
            // Where the other device says its endpoint answers, as the record of the person's
            // devices keeps it, beside what this endpoint's discovery finds.
            let addr = self
                .device_addrs(device)
                .into_iter()
                .fold(EndpointAddr::new(id), EndpointAddr::with_ip_addr);
            let dial = Arc::new(Iroh::new(self.endpoint.clone(), addr));
            LiveSession::join_within(dial, &grant, join, link::REACH).await
        };
        joined.map_err(|error| match error {
            LiveError::Denied => "the person's device turned this device away",
            LiveError::Busy => "the person's device was admitting as many as it takes",
            _ => "the person's device was not reached",
        })
    }

    /// Where the person's device `device` says its endpoint answers, as their record keeps it.
    fn device_addrs(&self, device: [u8; 32]) -> Vec<std::net::SocketAddr> {
        let root = self.shared.cert.user_public_key;
        self.shared
            .person
            .get()
            .and_then(|person| person.state::<Devices>().ok())
            .and_then(|record| {
                record
                    .listed(&root)
                    .into_iter()
                    .find(|listed| listed.cert.device_public_key == device)
            })
            .map(|listed| listed.device.endpoint_addrs())
            .unwrap_or_default()
    }

    /// Whom this device's own scope hears: the person's other devices, as their record names
    /// them now; nobody while the record is not here.
    fn heard_devices(&self) -> Roster {
        let own = self.shared.device;
        let root = self.shared.cert.user_public_key;
        let devices = self
            .shared
            .person
            .get()
            .and_then(|person| person.state::<Devices>().ok())
            .map(|record| own_devices(&record, &root, now_ns()))
            .unwrap_or_default();
        Roster::new(
            devices
                .into_iter()
                .map(|device| device.device)
                .filter(|device| *device != own),
        )
    }
}

/// The grant `cert`'s device presents to join `scope` of its person's own workspace `workspace`,
/// sealed under `epoch`.
fn own_grant(
    cert: &DeviceCert,
    workspace: tilo_desktop_model::WorkspaceId,
    scope: &Scope,
    epoch: u64,
) -> Option<Vec<u8>> {
    let certificate = serde_json::to_vec(cert).ok()?;
    tilo_live::grant::Native::new(
        *workspace.as_uuid().as_bytes(),
        &scope.id(),
        epoch,
        certificate,
        Vec::new(),
    )
    .ok()
    .map(|grant| grant.encode())
}
