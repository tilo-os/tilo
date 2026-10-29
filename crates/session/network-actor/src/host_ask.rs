//! Asking a host of the person for one of its own operations (`tilo.host`):
//! listing and entering the rooms it hosts, creating one, hosting one.
//!
//! The host is asked directly, over a session opened for the one request
//! and closed with its answer: the host's handshake proves it is the device
//! the person's directory names, and the host takes the asker for a device
//! of its own person by its certificate. No workspace is named or
//! subscribed, so the session takes no place of this device's other
//! sessions there; nothing is kept dialling the host, and no timer runs for
//! it.
use iroh::EndpointAddr;
use tilo_actions::remote::{InvokeRequest, ReceiptStatus};
use tilo_relay_session::RelayClient;
use tilo_user_identity::RevocationList;

use super::ActorState;

/// One host operation for a host of the person.
#[derive(Debug)]
pub struct Request {
    /// Where the host is reached.
    pub addr: EndpointAddr,
    /// The host's device, as the person's directory names it: the session
    /// pins it. `None` for a host this session is configured with, pinned
    /// by its endpoint alone.
    pub device: Option<[u8; 32]>,
    /// The invocation; its target names the provider.
    pub invoke: InvokeRequest,
    /// Where the receipt goes.
    pub reply: std::sync::mpsc::SyncSender<Result<ReceiptStatus, String>>,
}

impl ActorState {
    /// Ask `request` of the host it names, on a session opened for it.
    pub(super) fn ask_host(&self, request: Request) {
        let Some(identity) = self.host_identity.clone() else {
            let _ = request
                .reply
                .send(Err("this device has no identity to ask with".into()));
            return;
        };
        let endpoint = self.endpoint.clone();
        tokio::spawn(async move {
            let Request {
                addr,
                device,
                invoke,
                reply,
            } = request;
            let answer = async {
                let (cert, entries) = identity.current_person_material()?;
                let mut revocations = RevocationList::new();
                for entry in entries {
                    revocations
                        .insert(entry)
                        .map_err(|error| error.to_string())?;
                }
                let key = identity.device_signing_key.clone();
                let client = match device {
                    Some(device) => {
                        RelayClient::connect_v2_to_own_device(
                            endpoint,
                            addr,
                            cert.user_public_key,
                            device,
                            cert,
                            key,
                            revocations,
                        )
                        .await
                    }
                    None => {
                        RelayClient::connect_to_pinned_host(endpoint, addr, cert, key, revocations)
                            .await
                    }
                }
                .map_err(|error| error.to_string())?;
                // Let go, never closed: the endpoint is this device's own.
                tilo_relay_session::rooms::ask_host(&client, invoke).await
            };
            let _ = reply.send(answer.await);
        });
    }
}
