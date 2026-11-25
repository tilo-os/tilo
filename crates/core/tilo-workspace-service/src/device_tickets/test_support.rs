//! A device that prints one ticket, for an embedder's test: the laptop a phone test signs a
//! device in with, without holding the sync engine itself.

use std::sync::Arc;

use ed25519_dalek::{Signer as _, SigningKey};
use iroh::protocol::Router;
use iroh::{Endpoint, RelayMode, SecretKey, endpoint::presets};
use tilo_device_identity::login::{
    Approval, Approved, Approver, Asked, Entry, Reached, TICKET_LIFETIME_NS, Ticket,
};
use tilo_peer_state::actions::{ActionService, NoActions};
use tilo_peer_state::new_device::{Keeping, ServedTicket};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

use crate::workspaces::{Workspaces, person_workspace};

/// The person's laptop, serving one ticket: a device that logs in with it is certified by the
/// person's root and enters the person's workspace on their server.
pub struct Issuing {
    ticket: Ticket,
    endpoint: Endpoint,
    router: Router,
    runtime: tokio::runtime::Handle,
}

impl Issuing {
    /// The laptop of the person whose root is `person`, printing a ticket into `entry`, served
    /// on `runtime`, and listing a device its ticket made in the record of the person's devices,
    /// which `workspaces`, the server keeping their own workspace, holds.
    pub fn start(
        runtime: &tokio::runtime::Handle,
        person: &SigningKey,
        entry: Entry,
        workspaces: Workspaces,
    ) -> Self {
        let laptop = key(8);
        let cert = certified(person, &laptop, "the person's laptop", true);
        let endpoint = runtime.block_on(endpoint(&laptop));
        let ticket = Ticket::new(reached_of(&endpoint.addr()));
        let router = {
            let _entered = runtime.enter();
            serving(
                &endpoint,
                &laptop,
                ticket.clone(),
                person.verifying_key().to_bytes(),
                entry,
                Arc::new(RootSigns {
                    root: person.clone(),
                    device: laptop.clone(),
                    cert,
                }),
                Arc::new(KeptInRecord(workspaces)),
            )
        };
        Self {
            ticket,
            endpoint,
            router,
            runtime: runtime.clone(),
        }
    }

    /// The ticket, as the laptop shows it to the person.
    pub fn ticket(&self) -> String {
        self.ticket.encode()
    }
}

impl Drop for Issuing {
    fn drop(&mut self) {
        let router = self.router.clone();
        let endpoint = self.endpoint.clone();
        drop(self.runtime.spawn(async move {
            let _ = router.shutdown().await;
            endpoint.close().await;
        }));
    }
}

/// The laptop approving the device that brings its ticket: its person's root certifies it.
struct RootSigns {
    root: SigningKey,
    device: SigningKey,
    cert: DeviceCert,
}

impl Approver for RootSigns {
    fn approve(&self, asked: Asked) -> Approval {
        let mut cert = asked.cert.clone();
        sign_device_cert(&mut cert, &self.root).unwrap();
        Box::pin(std::future::ready(Ok(Approved {
            cert,
            approver: self.cert.clone(),
            signature: self.device.sign(&asked.digest).to_bytes(),
        })))
    }
}

/// The record of the person's devices, in their own workspace `workspaces` holds: where the laptop
/// lists the phone its ticket made, as the device that issues a ticket does before it answers it.
struct KeptInRecord(Workspaces);

impl Keeping for KeptInRecord {
    fn keep(&self, cert: DeviceCert) -> tilo_peer_state::new_device::Kept {
        let workspaces = self.0.clone();
        Box::pin(async move {
            let kept = person_workspace::devices::keep(&workspaces, &cert).await;
            if let Err(error) = kept {
                tracing::warn!(%error, "the laptop does not list the phone among the person's devices");
            }
        })
    }
}

/// A key made from `seed`, so a test's devices are the same devices on every run.
fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// `key`'s certificate, signed by `root`: a device of the person whose root it is.
fn certified(root: &SigningKey, key: &SigningKey, label: &str, primary: bool) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: if primary {
            DeviceCapabilities::primary()
        } else {
            DeviceCapabilities::member()
        },
        issued_at_ns: tilo_clock::unix_now_ns().saturating_sub(1_000_000_000),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

/// Where `addr` is reached, as a ticket carries it.
fn reached_of(addr: &iroh::EndpointAddr) -> Reached {
    Reached {
        endpoint: *addr.id.as_bytes(),
        relay: addr.relay_urls().next().map(ToString::to_string),
        direct: addr.ip_addrs().copied().collect(),
    }
}

/// An endpoint of `key`'s on this machine's loopback, reached directly.
async fn endpoint(key: &SigningKey) -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .secret_key(SecretKey::from_bytes(&key.to_bytes()))
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap()
}

/// The laptop's endpoint serving `ticket` as the device that printed it does: a device that logs
/// in with it is certified by `approver` for the person whose root key is `person`, and enters
/// `entry`. What the router serves ends with it.
fn serving(
    endpoint: &Endpoint,
    key: &SigningKey,
    ticket: Ticket,
    person: [u8; 32],
    entry: Entry,
    approver: Arc<dyn Approver>,
    keep: Arc<dyn Keeping>,
) -> Router {
    let service = ActionService::new(key.clone(), Arc::new(NoActions));
    service.admit_uncertified(
        tilo_device_identity::login::SCOPE,
        Arc::new(
            ServedTicket::new(
                ticket,
                person,
                entry,
                DeviceCapabilities::member(),
                approver,
                RevocationList::new(),
                tilo_clock::unix_now_ns().saturating_add(TICKET_LIFETIME_NS),
            )
            .kept_by(keep),
        ),
    );
    Router::builder(endpoint.clone())
        .accept(tilo_peer_state::actions::ACTIONS_ALPN, service)
        .spawn()
}
