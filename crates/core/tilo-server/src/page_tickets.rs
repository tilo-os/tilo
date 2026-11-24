//! What a host tells its device's tickets about the pages it serves
//! ([`tilo_peer_state::new_device::PageHost`]): a tab has no iroh endpoint of its own, so the host
//! a page is on serves the tickets the page shows, through the device's one set of tickets
//! ([`tilo_peer_state::new_device::Tickets`]). The host names where it is dialled and the
//! certificate a device entering through it pins, refuses what its revocations refuse, and keeps
//! each device a page's ticket added in its person's record.

use tilo_device_identity::login::Reached;
use tilo_lock::RwLockRecover as _;
use tilo_user_identity::{DeviceCert, RevocationList};
use tilo_workspace_service::handshake::LiveIdentity;
use tilo_workspace_service::workspaces::Workspaces;

/// This host, as the pages it serves reach it.
pub(crate) struct PageTickets {
    /// The host's identity: its certificate and the revocations it knows.
    pub(crate) identity: LiveIdentity,
    /// Where the host is dialled.
    pub(crate) endpoint: iroh::Endpoint,
    /// The workspaces this host governs, whose records keep each person's devices.
    pub(crate) records: Option<Workspaces>,
}

impl tilo_peer_state::new_device::PageHost for PageTickets {
    fn reached(&self) -> Option<Reached> {
        let addr = self.endpoint.addr();
        Some(Reached {
            endpoint: *addr.id.as_bytes(),
            relay: addr.relay_urls().next().map(ToString::to_string),
            direct: addr.ip_addrs().copied().collect(),
        })
    }

    fn server_cert(&self) -> DeviceCert {
        self.identity.cert.clone()
    }

    fn revocations(&self) -> RevocationList {
        self.identity.revocations.read_recover().clone()
    }

    fn keep(&self, cert: DeviceCert) {
        let (Some(records), Ok(runtime)) =
            (self.records.clone(), tokio::runtime::Handle::try_current())
        else {
            return;
        };
        runtime.spawn(async move {
            let label = cert.device_label.clone();
            records.keep_issued_device(&cert, &label).await;
        });
    }
}
