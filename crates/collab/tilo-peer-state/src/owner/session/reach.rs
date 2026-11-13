//! Reaching a workspace's host: this device's own host in process, or a host's
//! iroh endpoint. Every holder of a workspace's link reaches its host, and
//! renews this device's membership with it, through [`Reach`].
use super::Connect;
use crate::direct::{Dialed, DirectSync};
use crate::join::TransportHint;
use crate::join_transport::{LocalJoinService, WorkspaceJoinClient};
use collab_types::signer::DeviceSigner;
use std::{sync::Arc, time::Duration};
use tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1;
use tilo_identity_host::custody::HostIdentity;
use crate::join::WorkspaceId;

/// This device's own workspace host, running in this process: a workspace placed on
/// this device is linked through it, as an endpoint cannot dial itself.
pub trait OwnHost: Send + Sync {
    /// The host's workspace-join exchange, for enrolling in and renewing
    /// membership of its workspaces.
    fn join_service(&self) -> Arc<dyn LocalJoinService>;
    /// The host's end of `workspace`'s link, syncing the replica the host
    /// decides the workspace on. `None` for a workspace it does not hold.
    fn direct(&self, workspace: WorkspaceId) -> Option<DirectSync>;
}

/// How this device reaches a workspace's host.
#[derive(Clone)]
pub struct Reach {
    /// This device's endpoint, keyed by its device key.
    pub endpoint: iroh::Endpoint,
    /// This device's own host, once it runs.
    pub own_host: Option<Arc<dyn OwnHost>>,
}

/// A workspace's link, ready to run: this device's end, and the host's when it
/// is in this process.
pub struct Linked {
    /// This device's end of the link.
    pub link: DirectSync,
    host: Host,
}

enum Host {
    /// A host dialled over its endpoint.
    Dialled(Box<Dialed>),
    /// This device's own host: the two ends pair in process.
    Paired(DirectSync),
}

impl Linked {
    /// Sync with the workspace's host until the link ends.
    ///
    /// # Errors
    ///
    /// The connection ends badly, or either device is no longer in the
    /// workspace's roster.
    pub async fn run(self) -> Result<(), String> {
        match self.host {
            Host::Dialled(dialed) => dialed.run(&self.link).await,
            Host::Paired(host) => DirectSync::pair(&self.link, &host).await,
        }
    }
}

impl Reach {
    /// Link `connect`'s workspace to its host at `via` as the device `signer`
    /// is the custody of: in process when `via` is this device's own host,
    /// and over [`DIRECT_ALPN`](crate::direct::DIRECT_ALPN) otherwise.
    ///
    /// # Errors
    /// The hint does not authenticate for the workspace, the host refuses this
    /// device or cannot be reached in 10 seconds, or a workspace hosted here
    /// is held by no host of this device.
    pub async fn session(
        &self,
        connect: &Connect,
        via: &TransportHint,
        signer: Arc<dyn DeviceSigner>,
    ) -> Result<Linked, String> {
        let (payload, _) =
            IrohWorkspaceHostHintV1::authenticate_transport_hint(via).map_err(|e| e.to_string())?;
        let workspace = connect.workspace();
        if payload.workspace_id != workspace {
            return Err("native link workspace pin mismatch".into());
        }
        let link = connect.link(signer)?;
        let own = link.device();
        if super::hosted_by(via, &own) {
            let host = self
                .own_host
                .as_ref()
                .ok_or("this device's workspace host is not running")?
                .direct(payload.workspace_id)
                .ok_or("this device's workspace host holds no such workspace")?;
            return Ok(Linked {
                link,
                host: Host::Paired(host),
            });
        }
        // The relay answers on a service key of its own, which the hint's signature binds to the
        // host device: the link's history is that device's.
        let host = payload.host_device_certificate.device_public_key;
        let dialed = tokio::time::timeout(
            Duration::from_secs(10),
            link.dial(&self.endpoint, payload.endpoint_addr, host),
        )
        .await
        .map_err(|_| "native link connection timed out".to_string())??;
        Ok(Linked {
            link,
            host: Host::Dialled(Box::new(dialed)),
        })
    }

    /// The workspace-join exchange with a workspace's host as this device,
    /// `own`: with this device's own host in process when it is the host
    /// (`hosted_here`), else over iroh. `None` for a workspace hosted here
    /// while this device's host does not run.
    #[must_use]
    pub fn join_client(&self, hosted_here: bool, own: [u8; 32]) -> Option<WorkspaceJoinClient> {
        if hosted_here {
            let host = self.own_host.as_ref()?;
            return Some(WorkspaceJoinClient::local(host.join_service(), own));
        }
        Some(WorkspaceJoinClient::new(self.endpoint.clone()))
    }

    /// Prove this device's membership of `connect`'s workspace with the renewal
    /// of the certificate its record holds, when custody holds one
    /// ([`crate::owner::enroll::renew_membership`]). Answers whether it
    /// renewed.
    pub async fn renew(&self, connect: &Connect, identity: &HostIdentity) -> Result<bool, String> {
        let workspace = connect.workspace();
        let own = identity.device_cert.device_public_key;
        let hosted_here = connect.owner().with_current(workspace, |view| {
            Ok(super::hosted_by(view.transport_hint()?, &own))
        })?;
        let client = self
            .join_client(hosted_here, own)
            .ok_or("this device's workspace host is not running")?;
        crate::owner::enroll::renew_membership(
            client,
            identity.clone(),
            connect.owner().clone(),
            workspace,
        )
        .await
    }
}
