//! Reaching a room's host: this device's own host in process, a host's
//! HTTPS routes, or its iroh endpoint. Every holder of a room's session
//! reaches its host, and renews this device's membership with it, through
//! [`Reach`].
use super::{Connect, DialError};
use crate::join::TransportHint;
use ed25519_dalek::SigningKey;
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostTrust};
use tilo_identity_host::custody::HostIdentity;
use tilo_relay_session::{
    ClientError, RelayClient, RelayCoeditSession,
    workspace_join::{LocalJoinService, WorkspaceJoinClient},
};
use tilo_user_identity::{DeviceCert, RevocationList};

/// This device's own room host, running in this process: a room placed on
/// this device is joined through it, as an endpoint cannot dial itself.
pub trait OwnHost: Send + Sync {
    /// The host's workspace-join exchange, for enrolling in and renewing
    /// membership of its rooms.
    fn join_service(&self) -> Arc<dyn LocalJoinService>;
    /// An in-process member session as `cert`, through the handshake a
    /// remote member's dial takes.
    fn connect_local(
        &self,
        cert: DeviceCert,
        key: SigningKey,
        revocations: RevocationList,
    ) -> Pin<Box<dyn Future<Output = Result<RelayClient, ClientError>> + Send + '_>>;
}

/// How this device reaches a room's host.
#[derive(Clone)]
pub struct Reach {
    /// This device's endpoint, keyed by its device key.
    pub endpoint: iroh::Endpoint,
    /// The host's HTTPS routes, for a host that publishes no iroh address.
    pub https: Option<Arc<dyn tilo_relay_client::HttpClient>>,
    /// This device's own host, once it runs.
    pub own_host: Option<Arc<dyn OwnHost>>,
}

impl Reach {
    /// Join workspace `ws`'s co-edit session at `via` as `cert`: in process
    /// when `via` is this device's own host, over HTTPS when the host is
    /// reached so, and dialled otherwise. With `probe`, a host that closes
    /// the stream without answering is asked which peer protocol it speaks.
    ///
    /// # Errors
    /// The hint does not authenticate for `ws`, the host authenticates as
    /// another certificate or speaks another peer protocol version
    /// ([`DialError::PeerVersion`]), the connection fails or takes over 10
    /// seconds, only a certified host is reached over HTTPS, and a room
    /// hosted here fails while this device's host does not run.
    pub async fn session(
        &self,
        ws: u64,
        via: &TransportHint,
        cert: DeviceCert,
        key: SigningKey,
        revocations: RevocationList,
        probe: bool,
    ) -> Result<RelayCoeditSession, DialError> {
        let (payload, trust) =
            IrohWorkspaceHostHintV1::authenticate_transport_hint(via).map_err(|e| e.to_string())?;
        if payload.workspace_id != ws {
            return Err("native relay workspace pin mismatch".into());
        }
        let own = cert.device_public_key;
        let client = if super::hosted_by(via, &own) {
            let host = self
                .own_host
                .as_ref()
                .ok_or("this device's room host is not running")?;
            host.connect_local(cert, key, revocations).await?
        } else {
            let probing = (probe && self.https.is_none()).then(|| cert.clone());
            let addr = payload.endpoint_addr.clone();
            let host = via.host.clone();
            let connect = async {
                match (&self.https, trust) {
                    (Some(http), WorkspaceHostTrust::CertifiedHost) => {
                        RelayClient::connect_https(
                            Arc::clone(http),
                            host.device_public_key,
                            cert,
                            &key,
                            revocations,
                        )
                        .await
                    }
                    (Some(_), WorkspaceHostTrust::InteractiveOwner(_)) => {
                        return Err("only a certified host is reached over HTTPS".into());
                    }
                    (None, WorkspaceHostTrust::CertifiedHost) => {
                        RelayClient::connect_v2_to_enrolled_home_peer(
                            self.endpoint.clone(),
                            addr,
                            host.user_public_key,
                            host.device_public_key,
                            cert,
                            key,
                            revocations,
                        )
                        .await
                    }
                    (None, WorkspaceHostTrust::InteractiveOwner(trust)) => {
                        RelayClient::connect_v2_to_interactive_workspace_owner(
                            self.endpoint.clone(),
                            addr,
                            trust,
                            cert,
                            key,
                            revocations,
                        )
                        .await
                    }
                }
                .map_err(DialError::from)
            };
            match tokio::time::timeout(Duration::from_secs(10), connect).await {
                Err(_) => return Err("native relay connection timed out".into()),
                Ok(Err(DialError::NoAnswer(reason))) => {
                    let theirs = match &probing {
                        Some(cert) => probe_version(&self.endpoint, &payload, cert).await,
                        None => None,
                    };
                    return Err(theirs.map_or(DialError::NoAnswer(reason), |theirs| {
                        DialError::PeerVersion { theirs }
                    }));
                }
                Ok(client) => client?,
            }
        };
        if client.relay_peer.cert != via.host {
            return Err("native relay authenticated a different host certificate".into());
        }
        RelayCoeditSession::join(client, ws, own)
            .map_err(|error| DialError::Failed(error.to_string()))
    }

    /// The workspace-join exchange with a room's host as this device,
    /// `own`: with this device's own host in process when it is the host
    /// (`hosted_here`), else over HTTPS or iroh. `None` for a room hosted
    /// here while this device's host does not run.
    #[must_use]
    pub fn join_client(&self, hosted_here: bool, own: [u8; 32]) -> Option<WorkspaceJoinClient> {
        if hosted_here {
            let host = self.own_host.as_ref()?;
            return Some(WorkspaceJoinClient::local(host.join_service(), own));
        }
        Some(match &self.https {
            Some(http) => WorkspaceJoinClient::https(Arc::clone(http), None),
            None => WorkspaceJoinClient::new(self.endpoint.clone()),
        })
    }

    /// Prove this device's membership of `connect`'s room with the renewal
    /// of the certificate its record holds, when custody holds one
    /// ([`crate::owner::enroll::renew_membership`]). Answers whether it
    /// renewed.
    ///
    /// # Errors
    /// The room is not held here, its host cannot be reached or refuses.
    pub async fn renew(&self, connect: &Connect, identity: &HostIdentity) -> Result<bool, String> {
        let room = connect.room();
        let own = identity.device_cert.device_public_key;
        let hosted_here = connect.owner().with_current(room, |view| {
            Ok(super::hosted_by(view.transport_hint()?, &own))
        })?;
        let client = self
            .join_client(hosted_here, own)
            .ok_or("this device's room host is not running")?;
        crate::owner::enroll::renew_membership(
            client,
            identity.clone(),
            connect.owner().clone(),
            room,
        )
        .await
    }
}

/// The peer protocol version of the host at `host`, when a probe shows it
/// is not this device's: a host that predates versioned handshakes reads as
/// [`tilo_relay_protocol::auth::UNVERSIONED_PEER_PROTOCOL`]. `None` when the
/// host speaks this device's version (it refused this device instead), or
/// the probe could not tell.
async fn probe_version(
    endpoint: &iroh::Endpoint,
    host: &IrohWorkspaceHostHintV1,
    cert: &DeviceCert,
) -> Option<u32> {
    use tilo_relay_session::client::HandshakeProbe;
    let probe = tokio::time::timeout(
        Duration::from_secs(10),
        RelayClient::probe_handshake_version(endpoint, host.endpoint_addr.clone(), cert),
    )
    .await
    .ok()?
    .ok()?;
    match probe {
        HandshakeProbe::Unversioned => Some(tilo_relay_protocol::auth::UNVERSIONED_PEER_PROTOCOL),
        HandshakeProbe::Versioned(theirs) if theirs != neighbour_wire::PEER_PROTOCOL_VERSION => {
            Some(theirs)
        }
        HandshakeProbe::Versioned(_) | HandshakeProbe::Unknown => None,
    }
}
