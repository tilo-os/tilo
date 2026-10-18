//! What a browser device keeps, and how it gets certified.
//!
//! The device key itself never passes through here: it lives behind a
//! [`DeviceSigner`] (a non-extractable `WebCrypto` key in the page, a
//! `SigningKey` natively). A [`PeerStore`] keeps only public material — the
//! device's certificate, the pinned host certificate — and the commit outbox
//! that must survive a reload. There is no enrollment state machine:
//! [`certify`] signs one request and waits for a root holder to answer it
//! through the host. A page that holds the user root itself (behind another
//! [`DeviceSigner`]) certifies its own device with [`certify_with_root`] and
//! other devices, such as the node that hosts its workspaces, with
//! [`issue_device_cert`].

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tilo_relay_client::DeviceSigner;
use tilo_relay_protocol::certify::{
    ApproverDecision, ApproverReply, ApproverRequest, CertifyReply, MemberApproval,
    PendingCertification,
};
use tilo_user_identity::{
    CERTIFICATION_SCHEMA_VERSION, CertificationPolicy, CertificationRequest, DeviceCapabilities,
    DeviceCert, RevocationList, verify_device_cert_at, verify_issued_cert,
};

use crate::session::Host;
use tilo_lock::MutexRecover as _;
use tilo_relay_client::https::LinkError;

/// Longest a certification request waits for an approver.
pub const CERTIFICATION_WAIT_NS: u64 = 5 * 60 * 1_000_000_000;

/// One commit the host has not acknowledged yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry {
    /// Commit id, reused when the commit is resent.
    pub op: [u8; 16],
    /// Relay workspace the commit targets.
    pub workspace_id: u64,
    /// Document route the commit belongs to.
    pub route: crate::document::DocumentRoute,
    /// Retention channel ([`neighbour_wire::retention`]) of the body.
    pub channel: [u8; 32],
    /// Whether the body is a retained checkpoint rather than a delta.
    pub checkpoint: bool,
    /// Encoded `NeighbourMsg` body.
    pub body: Vec<u8>,
}

/// The durable part of a browser peer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerRecord {
    /// This device's root-signed certificate.
    pub cert: Option<DeviceCert>,
    /// The host certificate pinned at certification.
    pub host_cert: Option<DeviceCert>,
    /// Commits not yet acknowledged, in submission order.
    pub outbox: Vec<OutboxEntry>,
}

/// Persistence for a [`PeerRecord`]. The page implements it over `IndexedDB`
/// (strict durability); a save counts only once it is durable.
pub trait PeerStore: tilo_relay_client::MaybeSendSync {
    /// The stored record, if any.
    fn load(&self) -> Option<PeerRecord>;
    /// Replace the stored record.
    ///
    /// # Errors
    ///
    /// Returns a description when the write did not become durable.
    fn save(&self, record: &PeerRecord) -> Result<(), String>;
    /// The stored channel state (encrypted-channel membership and the
    /// edits held for it), if any. A store without it keeps channel state
    /// for the session only.
    fn load_channels(&self) -> Option<Vec<u8>> {
        None
    }
    /// Replace the stored channel state.
    ///
    /// # Errors
    ///
    /// Returns a description when the write did not become durable.
    fn save_channels(&self, _bytes: &[u8]) -> Result<(), String> {
        Ok(())
    }
    /// The key channel state is sealed under at rest, kept apart from that
    /// state: in the page, wrapped by a non-extractable `WebCrypto` key. A
    /// store without one seals channel state under a key of the session's
    /// own, so stored state does not open in the next session.
    fn channel_vault(&self) -> Option<[u8; 32]> {
        None
    }
}

/// An in-memory [`PeerStore`] for native peers and tests.
pub struct MemoryPeerStore(Mutex<Option<PeerRecord>>, Mutex<Option<Vec<u8>>>, [u8; 32]);

impl Default for MemoryPeerStore {
    fn default() -> Self {
        Self(Mutex::default(), Mutex::default(), rand::random())
    }
}

impl PeerStore for MemoryPeerStore {
    fn load(&self) -> Option<PeerRecord> {
        self.0.lock_recover().clone()
    }

    fn save(&self, record: &PeerRecord) -> Result<(), String> {
        *self.0.lock_recover() = Some(record.clone());
        Ok(())
    }

    fn load_channels(&self) -> Option<Vec<u8>> {
        self.1.lock_recover().clone()
    }

    fn save_channels(&self, bytes: &[u8]) -> Result<(), String> {
        *self.1.lock_recover() = Some(bytes.to_vec());
        Ok(())
    }

    fn channel_vault(&self) -> Option<[u8; 32]> {
        Some(self.2)
    }
}

/// Why certification did not produce a certificate.
#[derive(Debug, thiserror::Error)]
pub enum CertifyError {
    /// The request could not be built or signed.
    #[error("certification request: {0}")]
    Request(String),
    /// The host could not be reached or answered badly.
    #[error("certification transport: {0}")]
    Transport(#[from] LinkError),
    /// An approver declined, or the host refused.
    #[error("certification denied: {0}")]
    Denied(String),
    /// No approver answered in time.
    #[error("no approver answered the certification request")]
    Expired,
}

/// Ask the user's host to have the root holder certify the device behind
/// `signer`. `on_pending` receives the pairing code to display while
/// waiting. Returns the device certificate and the host certificate to pin,
/// both checked here: the host cannot hand back a certificate for another
/// key, another user or wider capabilities.
///
/// # Errors
///
/// Returns [`CertifyError`] when signing, the transport, or the answer fails.
pub async fn certify(
    host: &Host,
    user_public_key: [u8; 32],
    label: &str,
    invite: [u8; 16],
    signer: &dyn DeviceSigner,
    on_pending: impl FnOnce(&str),
) -> Result<(DeviceCert, DeviceCert), CertifyError> {
    let request = signed_request(user_public_key, label, invite, signer).await?;
    let reply = match host {
        Host::Https(client) => {
            let digest = request
                .digest()
                .map_err(|error| CertifyError::Request(error.to_string()))?;
            let mut reply =
                tilo_relay_client::https::certify_request(client.as_ref(), &request).await?;
            let mut on_pending = Some(on_pending);
            while let CertifyReply::Pending { pairing_code } = &reply {
                if let Some(show) = on_pending.take() {
                    show(pairing_code);
                }
                reply = tilo_relay_client::https::certify_wait(client.as_ref(), digest).await?;
            }
            reply
        }
        #[cfg(feature = "iroh")]
        Host::Iroh { addr, relay_url } => {
            let mut builder = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal);
            let mut addr = addr.clone();
            if let Some(url) = relay_url {
                builder = builder.relay_mode(iroh::RelayMode::Custom(url.clone().into()));
                addr = addr.with_relay_url(url.clone());
            }
            let endpoint = builder
                .bind()
                .await
                .map_err(|error| LinkError::Unavailable(error.to_string()))?;
            let outcome = tilo_relay_client::request_certification(
                &endpoint,
                addr,
                request.clone(),
                on_pending,
            )
            .await
            .map_err(|error| LinkError::Refused(error.to_string()))?;
            endpoint.close().await;
            match outcome {
                tilo_relay_client::CertificationOutcome::Issued { cert, host_cert } => {
                    CertifyReply::Issued { cert, host_cert }
                }
                tilo_relay_client::CertificationOutcome::Denied(reason) => {
                    CertifyReply::Denied { reason }
                }
                tilo_relay_client::CertificationOutcome::Expired => CertifyReply::Expired,
            }
        }
    };
    match reply {
        CertifyReply::Issued { cert, host_cert } => {
            check_issued(&request, &cert, &host_cert)?;
            Ok((cert, host_cert))
        }
        CertifyReply::Denied { reason } => Err(CertifyError::Denied(reason)),
        CertifyReply::Expired | CertifyReply::Pending { .. } => Err(CertifyError::Expired),
    }
}

/// What a certificate a page's root issues lets its device do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssuedRole {
    /// A browser: [`CertificationPolicy::browser`] (a member, 30 days).
    Browser,
    /// A native node that hosts and governs the person's workspaces: it
    /// relays, pins and reads them and owns their rooms, but admits no
    /// devices. One year.
    Node,
    /// The session beside a node that runs the person's apps and agents:
    /// a member device of theirs with its own key, never a host and never
    /// one that admits devices. One year, like its node.
    Session,
}

/// How long a node's certificate lasts.
pub const NODE_CERT_LIFETIME_NS: u64 = 365 * 24 * 60 * 60 * 1_000_000_000;

impl IssuedRole {
    /// The capabilities granted.
    pub fn capabilities(self) -> DeviceCapabilities {
        match self {
            Self::Browser => CertificationPolicy::browser().ceiling,
            Self::Node => tilo_user_identity::cloud_enrollment::host_capabilities(),
            Self::Session => DeviceCapabilities::member(),
        }
    }

    fn lifetime_ns(self) -> u64 {
        match self {
            Self::Browser => CertificationPolicy::browser().lifetime_ns,
            Self::Node | Self::Session => NODE_CERT_LIFETIME_NS,
        }
    }
}

/// Have `root` (the user root the page holds) certify `device_public_key`
/// as `role`, issued now. Never certifies the root itself.
///
/// # Errors
///
/// Returns [`CertifyError::Request`] for the root's own key or when signing
/// fails or does not verify.
pub async fn issue_device_cert(
    root: &dyn DeviceSigner,
    device_public_key: [u8; 32],
    label: &str,
    role: IssuedRole,
) -> Result<DeviceCert, CertifyError> {
    let user_public_key = root.device_public_key();
    if device_public_key == user_public_key {
        return Err(CertifyError::Request(
            "the root certifies devices, never itself".into(),
        ));
    }
    let now = tilo_clock::unix_now_ns();
    let mut cert = DeviceCert {
        user_public_key,
        device_public_key,
        device_label: label.to_owned(),
        capabilities: role.capabilities(),
        issued_at_ns: now,
        expires_at_ns: Some(now.saturating_add(role.lifetime_ns())),
        signature: [0; 64],
    };
    let request = |error: &dyn std::fmt::Display| CertifyError::Request(error.to_string());
    let digest = tilo_user_identity::device_cert_signing_digest(&cert).map_err(|e| request(&e))?;
    cert.signature = root
        .sign(digest.to_vec())
        .await
        .map_err(|error| CertifyError::Request(error.0))?;
    verify_device_cert_at(&cert, now).map_err(|e| request(&e))?;
    Ok(cert)
}

/// `approval` of `request`, signed by the member's `root`.
async fn member_approval(
    root: &dyn DeviceSigner,
    request: ApproverRequest,
) -> Result<MemberApproval, CertifyError> {
    let user_public_key = root.device_public_key();
    let issued_at_ns = tilo_clock::unix_now_ns();
    let message = MemberApproval::signed_message(&user_public_key, issued_at_ns, &request)
        .map_err(|error| CertifyError::Request(error.to_string()))?;
    let signature = root
        .sign(message)
        .await
        .map_err(|error| CertifyError::Request(error.0))?;
    Ok(MemberApproval {
        user_public_key,
        issued_at_ns,
        request,
        signature,
    })
}

async fn approve(
    client: &dyn crate::HttpClient,
    root: &dyn DeviceSigner,
    request: ApproverRequest,
) -> Result<ApproverReply, CertifyError> {
    let approval = member_approval(root, request).await?;
    match tilo_relay_client::https::certify_approve(client, &approval).await? {
        ApproverReply::Refused { reason } => Err(CertifyError::Denied(reason)),
        reply => Ok(reply),
    }
}

/// The requests of new devices waiting at the host at `client` for `root`
/// (a member's root the page holds) to answer: a new device of theirs asked
/// to be certified there, showing its pairing code.
///
/// # Errors
///
/// Returns [`CertifyError::Transport`] when the host is not reached, or
/// [`CertifyError::Denied`] when it refuses.
pub async fn pending_pairings(
    client: &dyn crate::HttpClient,
    root: &dyn DeviceSigner,
) -> Result<Vec<PendingCertification>, CertifyError> {
    match approve(client, root, ApproverRequest::ListPending).await? {
        ApproverReply::Pending(pending) => Ok(pending),
        _ => Err(CertifyError::Denied(
            "the host answered something else".into(),
        )),
    }
}

/// Answer the pending request `request_digest` at the host at `client`:
/// approved, `root` certifies the device it names as a browser of that root,
/// and the certificate it issued is answered, for the root's own record of
/// the person's devices.
///
/// # Errors
///
/// Returns [`CertifyError::Denied`] when the request is gone or the host
/// refuses the answer, [`CertifyError::Transport`] when it is not reached.
pub async fn answer_pairing(
    client: &dyn crate::HttpClient,
    root: &dyn DeviceSigner,
    request_digest: [u8; 32],
    approved: bool,
) -> Result<Option<DeviceCert>, CertifyError> {
    let decision = if approved {
        let pending = pending_pairings(client, root).await?;
        let Some(found) = pending
            .iter()
            .find(|entry| entry.request_digest == request_digest)
        else {
            return Err(CertifyError::Denied("that device no longer waits".into()));
        };
        let request = &found.request;
        ApproverDecision::Issue(
            issue_device_cert(
                root,
                request.device_public_key,
                &request.device_label,
                IssuedRole::Browser,
            )
            .await?,
        )
    } else {
        ApproverDecision::Deny
    };
    let issued = match &decision {
        ApproverDecision::Issue(cert) => Some(cert.clone()),
        _ => None,
    };
    decide_pairing(client, root, request_digest, issued.clone())
        .await
        .map(|()| issued)
}

/// Answer the pending request `request_digest` at the host at `client`,
/// as `approver` (a member's root, or what signs its approval commands):
/// with `cert`, a certificate the person's root already issued for exactly
/// that request, or turned away without one.
///
/// # Errors
///
/// As [`answer_pairing`].
pub async fn decide_pairing(
    client: &dyn crate::HttpClient,
    approver: &dyn DeviceSigner,
    request_digest: [u8; 32],
    cert: Option<DeviceCert>,
) -> Result<(), CertifyError> {
    let decision = match cert {
        Some(cert) => ApproverDecision::Issue(cert),
        None => ApproverDecision::Deny,
    };
    approve(
        client,
        approver,
        ApproverRequest::Decide {
            request_digest,
            decision,
        },
    )
    .await
    .map(drop)
}

/// Certify the device behind `signer` with the root the page holds, for the
/// host at `client`: the host's certificate must be a host of the same
/// root. Returns the device certificate and the host certificate to pin, as
/// [`certify`] does.
///
/// # Errors
///
/// Returns [`CertifyError::Transport`] when the host is not reached,
/// [`CertifyError::Denied`] when it is not a host of this root, or the
/// errors of [`issue_device_cert`].
pub async fn certify_with_root(
    client: &dyn crate::HttpClient,
    root: &dyn DeviceSigner,
    signer: &dyn DeviceSigner,
    label: &str,
) -> Result<(DeviceCert, DeviceCert), CertifyError> {
    let host_cert = tilo_relay_client::https::host_info(client).await?.host_cert;
    check_host(&host_cert, root.device_public_key())?;
    let cert =
        issue_device_cert(root, signer.device_public_key(), label, IssuedRole::Browser).await?;
    Ok((cert, host_cert))
}

fn check_host(host_cert: &DeviceCert, user_public_key: [u8; 32]) -> Result<(), CertifyError> {
    verify_device_cert_at(host_cert, tilo_clock::unix_now_ns())
        .map_err(|error| CertifyError::Denied(format!("host certificate refused: {error}")))?;
    if host_cert.user_public_key != user_public_key || !host_cert.capabilities.can_act_as_host {
        return Err(CertifyError::Denied(
            "host certificate is not a host of this user".into(),
        ));
    }
    Ok(())
}

async fn signed_request(
    user_public_key: [u8; 32],
    label: &str,
    invite: [u8; 16],
    signer: &dyn DeviceSigner,
) -> Result<CertificationRequest, CertifyError> {
    let now = tilo_clock::unix_now_ns();
    let unsigned = CertificationRequest {
        version: CERTIFICATION_SCHEMA_VERSION,
        user_public_key,
        device_public_key: signer.device_public_key(),
        device_label: label.to_owned(),
        requested: DeviceCapabilities::member(),
        nonce: rand::random(),
        invite,
        issued_at_ns: now,
        expires_at_ns: now.saturating_add(CERTIFICATION_WAIT_NS),
        signature: [0; 64],
    };
    let digest = unsigned
        .signing_digest()
        .map_err(|error| CertifyError::Request(error.to_string()))?;
    let signature = signer
        .sign(digest.to_vec())
        .await
        .map_err(|error| CertifyError::Request(error.0))?;
    unsigned
        .with_signature(signature)
        .map_err(|error| CertifyError::Request(error.to_string()))
}

/// How far a certificate's issue time may run ahead of this page's clock:
/// the host stamps its own nanosecond clock, and a page's clock is coarser
/// (a browser's is whole milliseconds), so a certificate checked the moment
/// it arrives can look issued in the future.
const ISSUE_SKEW_NS: u64 = 5 * 1_000_000_000;

/// When to check a certificate issued at `issued_at_ns`, reading `now_ns`.
fn checked_at(now_ns: u64, issued_at_ns: u64) -> u64 {
    if issued_at_ns > now_ns && issued_at_ns.saturating_sub(now_ns) <= ISSUE_SKEW_NS {
        issued_at_ns
    } else {
        now_ns
    }
}

fn check_issued(
    request: &CertificationRequest,
    cert: &DeviceCert,
    host_cert: &DeviceCert,
) -> Result<(), CertifyError> {
    let now = tilo_clock::unix_now_ns();
    let refused = |reason: String| CertifyError::Denied(reason);
    verify_issued_cert(
        request,
        cert,
        &CertificationPolicy::browser(),
        &RevocationList::new(),
        checked_at(now, cert.issued_at_ns),
    )
    .map_err(|error| refused(format!("issued certificate refused: {error}")))?;
    // A member pairs through any host they share, so the host may be
    // another person's: it must be a host, not necessarily this root's.
    verify_device_cert_at(host_cert, now)
        .map_err(|error| refused(format!("host certificate refused: {error}")))?;
    if !host_cert.capabilities.can_act_as_host {
        return Err(refused("host certificate is not a host's".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::verify_device_cert;

    #[tokio::test]
    async fn a_page_root_issues_browser_node_and_session_certificates_but_never_its_own() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let device = SigningKey::from_bytes(&[2; 32]).verifying_key().to_bytes();

        let browser = issue_device_cert(&root, device, "Browser", IssuedRole::Browser)
            .await
            .unwrap();
        verify_device_cert(&browser).unwrap();
        assert_eq!(browser.user_public_key, root.verifying_key().to_bytes());
        assert_eq!(browser.capabilities, DeviceCapabilities::member());
        let lifetime = browser.expires_at_ns.unwrap() - browser.issued_at_ns;
        assert_eq!(lifetime, CertificationPolicy::browser().lifetime_ns);

        let node = issue_device_cert(&root, device, "Company node", IssuedRole::Node)
            .await
            .unwrap();
        verify_device_cert(&node).unwrap();
        let caps = node.capabilities;
        assert!(caps.can_act_as_host && caps.can_own_workspaces && caps.can_read_all_workspaces);
        assert!(!caps.can_admit_devices && !caps.foreign);
        assert_eq!(
            node.expires_at_ns.unwrap() - node.issued_at_ns,
            NODE_CERT_LIFETIME_NS
        );

        let session = issue_device_cert(&root, device, "Apps on the node", IssuedRole::Session)
            .await
            .unwrap();
        verify_device_cert(&session).unwrap();
        assert_eq!(session.capabilities, DeviceCapabilities::member());
        assert!(!session.capabilities.can_act_as_host);
        assert_eq!(
            session.expires_at_ns.unwrap() - session.issued_at_ns,
            NODE_CERT_LIFETIME_NS
        );

        let own = root.verifying_key().to_bytes();
        assert!(matches!(
            issue_device_cert(&root, own, "Root", IssuedRole::Node).await,
            Err(CertifyError::Request(_))
        ));
    }

    #[test]
    fn a_certificate_issued_just_ahead_of_the_page_clock_is_checked_at_its_issue() {
        let now = 1_000_000_000_000;
        assert_eq!(checked_at(now, now + 400_000), now + 400_000);
        assert_eq!(checked_at(now, now - 1), now);
        assert_eq!(checked_at(now, now + ISSUE_SKEW_NS + 1), now);
    }
}
