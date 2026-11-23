//! Server-mediated device certification on its own ALPN.
//!
//! A device without a certificate cannot pass the [`auth`](crate::auth)
//! handshake, so it asks for one here. Two kinds of connection share the
//! ALPN, told apart by the first frame:
//!
//! ```text
//! requester → host:  CertifyOpen::Request(CertificationRequest)
//! host → requester:  CertifyReply::Pending { pairing_code }
//!                    … later, on the same stream …
//! host → requester:  CertifyReply::Issued { cert, server_cert, label }
//!                  | CertifyReply::Denied { reason } | CertifyReply::Expired
//!
//! approver → host:   CertifyOpen::Approver(Hello)   then the usual
//!                    HelloAck / HelloAck2 mutual authentication
//! approver → host:   ApproverRequest::ListPending
//! host → approver:   ApproverReply::Pending(Vec<PendingCertification>)
//! approver → host:   ApproverRequest::Decide { request_digest, decision }
//! host → approver:   ApproverReply::Decided | ApproverReply::Refused { reason }
//!
//! member → host:     CertifyOpen::Member(MemberApproval)
//! host → member:     ApproverReply
//! ```
//!
//! A request the host itself queued for a delegate device (an agent it
//! supervises, or a bot) carries a [`DelegateProposal`]: the label, colour, workspaces, apps and expiry the person
//! asked for. `ApproverRequest::DelegateProposals` lists them, and the one
//! consent step is `ApproverDecision::IssueDelegate`, a capability-less
//! certificate together with the root-signed delegation of exactly that
//! proposal (or a narrower one). A bare `Issue` is refused for such a
//! request, so a delegate never exists without its delegation.
//!
//! A member of a workspace the host serves pairs a new device the same way,
//! through any host they share: the new device asks on the certification
//! ALPN and shows its pairing code, and a page holding the member's root
//! reads and answers that member's pending requests with
//! [`MemberApproval`]s, each signed by the root and each on a connection of
//! its own ([`CertifyOpen::Member`]): a page reaches the host on the same
//! carrier for this as for everything else. The
//! host lists a member only their own requests and accepts a certificate
//! only for the exact request it answers.
//!
//! The host never signs a certificate. An approver is a device of the host's
//! user with `can_admit_devices`, i.e. one that holds root ownership. The host
//! checks every issued certificate against the exact request and its
//! [`CertificationPolicy`](tilo_user_identity::CertificationPolicy) before
//! handing it on, and refuses revoked devices at request and at issuance.

use serde::{Deserialize, Serialize};
use tilo_user_identity::canonical::{CanonicalError, signing_digest};
use tilo_user_identity::{CertificationRequest, DeviceCert};

use crate::auth::Hello;
use crate::delegation::{DelegateProposal, Delegation};

/// ALPN of the certification exchange. Frozen wire ABI.
pub const CERTIFY_ALPN: &[u8] = b"tilo-device-certify/2";
/// Largest certification frame either side accepts.
pub const MAX_CERTIFY_FRAME_BYTES: usize = 16 * 1024;
/// Requests one host holds at once; further requests are refused.
pub const MAX_PENDING_CERTIFICATIONS: usize = 32;
/// The number the host adds to a device name another of the person's devices
/// already reads by ([`numbered_label`]), so two devices set up the same way
/// still read apart.
pub const MAX_NUMBERED_LABELS: u32 = 999;

/// `base` numbered `n`: the name itself for the first, then `Laptop 2`,
/// `Laptop 3`, …, within what a certificate's label carries.
#[must_use]
pub fn numbered_label(base: &str, n: u32) -> String {
    let room = tilo_user_identity::certification::MAX_CERTIFICATION_LABEL_BYTES;
    if n < 2 {
        return fitted(base, room);
    }
    let suffix = format!(" {n}");
    format!(
        "{}{suffix}",
        fitted(base, room.saturating_sub(suffix.len()))
    )
}

/// `text` cut to `room` bytes without splitting a character: what a
/// certificate's label carries.
#[must_use]
fn fitted(text: &str, room: usize) -> String {
    let mut end = room.min(text.len());
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default().to_owned()
}

/// First frame on a certification stream. Its variants' order is their tag
/// on the wire: a new opener goes last, and the ones before keep theirs.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[allow(
    clippy::large_enum_variant,
    reason = "a wire message, decoded once per connection and never kept in bulk"
)]
pub enum CertifyOpen {
    /// An uncertified device asks to be certified.
    Request(CertificationRequest),
    /// A certified device opens an approver session.
    Approver(Hello),
    /// A member's root asks one approver command of the host, answered with
    /// one [`ApproverReply`]. A host that predates it refuses the opener.
    Member(MemberApproval),
}

/// What the host tells a waiting requester.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::large_enum_variant,
    reason = "a wire message, decoded once per request and never kept in bulk"
)]
pub enum CertifyReply {
    /// Queued for an approver. The same code is shown to the approver.
    Pending {
        /// Six-digit [`CertificationRequest::pairing_code`].
        pairing_code: String,
    },
    /// Issued and checked by the host.
    Issued {
        /// The root-signed certificate for the requesting device.
        cert: DeviceCert,
        /// The host's own certificate, which the device pins for later
        /// sessions.
        server_cert: DeviceCert,
        /// The name the host gave the device: the one it proposed when no
        /// other device of the person reads by it, else the next numbered
        /// name (`Laptop 2`), so two devices set up the same way read apart.
        label: String,
    },
    /// An approver declined, or the host refused the request.
    Denied {
        /// Human-readable reason.
        reason: String,
    },
    /// No approver answered within the request lifetime.
    Expired,
}

/// One request awaiting an approver's decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCertification {
    /// The exact signed request.
    pub request: CertificationRequest,
    /// Its [`CertificationRequest::digest`], used to answer it.
    pub request_digest: [u8; 32],
    /// The code the requester displays.
    pub pairing_code: String,
    /// When the host received the request, Unix-epoch nanoseconds.
    pub requested_at_ns: u64,
    /// Signing in: the request is a tab's, one a page's sign-in ticket here expects,
    /// and the certificate that answers it signs that tab in.
    #[serde(default)]
    pub sign_in: bool,
}

/// An approver's answer to one request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::large_enum_variant,
    reason = "a wire message, decoded once per request and never kept in bulk"
)]
pub enum ApproverDecision {
    /// Root-signed certificate for the request.
    Issue(DeviceCert),
    /// Decline.
    Deny,
    /// A delegate's request (an agent or a bot): its capability-less
    /// certificate and the root-signed delegation of its proposal, approved
    /// together.
    IssueDelegate {
        /// Root-signed certificate for the delegate's device.
        cert: DeviceCert,
        /// Root-signed delegation; no wider than the proposal.
        delegation: Delegation,
    },
}

/// Domain label of a [`MemberApproval`] signature: the signer signs the BLAKE3
/// digest of this label followed by the canonical encoding of the approval's
/// unsigned fields.
pub const MEMBER_APPROVAL_LABEL: &[u8] = b"tilo member approval v2\0";
/// How far a [`MemberApproval`]'s time may be from the host's clock.
pub const MEMBER_APPROVAL_SKEW_NS: u64 = 5 * 60 * 1_000_000_000;

/// An approver command from a member ([`CertifyOpen::Member`]): only
/// [`ApproverRequest::ListPending`] and [`ApproverRequest::AwaitPending`]
/// (that member's own requests), [`ApproverRequest::Decide`] (one of them),
/// [`ApproverRequest::DelegateProposals`] (the agents they are asked to
/// approve) and the push requests are answered.
///
/// Their root signs it, or one of their own devices does ([`Self::device`]).
/// A device signs only [`ApproverRequest::PushKey`] and
/// [`ApproverRequest::Notify`]; pairing stays the root's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberApproval {
    /// The member's root: the signer, or the root `device` chains to.
    pub user_public_key: [u8; 32],
    /// When it was signed, Unix-epoch nanoseconds.
    pub issued_at_ns: u64,
    /// The command.
    pub request: ApproverRequest,
    /// Ed25519 signature over [`Self::signed_digest`]: the root's, or the
    /// device's when `device` names one.
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
    /// The device of the member that signed instead of their root: its
    /// certificate, under `user_public_key`.
    #[serde(default)]
    pub device: Option<DeviceCert>,
}

impl MemberApproval {
    /// The digest the root, or the device, signs: the approval's root, its
    /// time and the command.
    ///
    /// # Errors
    ///
    /// [`CanonicalError::Serialize`] when the command cannot be encoded.
    pub fn signed_digest(
        user_public_key: &[u8; 32],
        issued_at_ns: u64,
        request: &ApproverRequest,
    ) -> Result<[u8; 32], CanonicalError> {
        #[derive(Serialize)]
        struct View<'a> {
            user_public_key: &'a [u8; 32],
            issued_at_ns: u64,
            request: &'a ApproverRequest,
        }
        signing_digest(
            MEMBER_APPROVAL_LABEL,
            &View {
                user_public_key,
                issued_at_ns,
                request,
            },
        )
    }

    /// Check the signature, and that it was signed within the allowed skew
    /// of `now_ns`. A device's command must be one a device may sign, from a
    /// certificate of the member's that holds at `now_ns` and is no agent's.
    /// Whether that device is revoked is the host's to check.
    pub fn verify(&self, now_ns: u64) -> Result<(), &'static str> {
        if self.issued_at_ns.abs_diff(now_ns) > MEMBER_APPROVAL_SKEW_NS {
            return Err("the approval is too old or from the future");
        }
        let signer = match &self.device {
            None => self.user_public_key,
            Some(cert) => {
                if !matches!(
                    self.request,
                    ApproverRequest::PushKey | ApproverRequest::Notify(_)
                ) {
                    return Err("only the person's root pairs devices");
                }
                if cert.user_public_key != self.user_public_key {
                    return Err("the device is another person's");
                }
                if cert.capabilities.agent.is_some() {
                    return Err("an agent's key signs nothing for its person");
                }
                tilo_user_identity::verify_device_cert_at(cert, now_ns)
                    .map_err(|_| "the device's certificate does not hold")?;
                cert.device_public_key
            }
        };
        let key = ed25519_dalek::VerifyingKey::from_bytes(&signer)
            .map_err(|_| "the approval names no valid root")?;
        let message = Self::signed_digest(&self.user_public_key, self.issued_at_ns, &self.request)
            .map_err(|_| "the approval cannot be encoded")?;
        key.verify_strict(
            &message,
            &ed25519_dalek::Signature::from_bytes(&self.signature),
        )
        .map_err(|_| "the approval is not signed by its root or device")
    }
}

/// A pending request the host queued for a delegate, and what it proposes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingDelegate {
    /// [`PendingCertification::request_digest`] of the delegate's request.
    pub request_digest: [u8; 32],
    /// What the person asked the delegate to be allowed.
    pub proposal: DelegateProposal,
}

/// Approver command after mutual authentication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::large_enum_variant,
    reason = "a wire message, decoded once per request and never kept in bulk"
)]
pub enum ApproverRequest {
    /// Read every pending request.
    ListPending,
    /// Answer one pending request.
    Decide {
        /// [`PendingCertification::request_digest`] of the request.
        request_digest: [u8; 32],
        decision: ApproverDecision,
    },
    /// Read every pending request for an agent the asking person's root
    /// approves.
    DelegateProposals,
    /// Read every pending request once they differ from `known`, the digests
    /// the approver holds: answered at once when they already do, and
    /// otherwise as a request arrives or is answered, or after
    /// [`AWAIT_PENDING_NS`]. A device showing a pairing link hears of the new
    /// device as it asks, rather than asking again and again.
    AwaitPending { known: Vec<[u8; 32]> },
    /// The key this host signs its pushes with ([`ApproverReply::PushKey`]):
    /// what a browser subscribes with before [`Self::Notify`].
    PushKey,
    /// Push the person a word in this browser when something waits for them
    /// and no Tilo of theirs is open: the browser's push subscription.
    Notify(PushSubscription),
}

/// Where a browser takes pushes, as its `PushManager` subscribed
/// (RFC 8030, RFC 8291): its push service's address for it, its key and its
/// auth secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushSubscription {
    /// The push service's `https` address for this browser.
    pub endpoint: String,
    /// The browser's P-256 key, uncompressed (65 bytes).
    #[serde(with = "serde_bytes")]
    pub p256dh: Vec<u8>,
    /// The browser's auth secret (16 bytes).
    #[serde(with = "serde_bytes")]
    pub auth: Vec<u8>,
}

impl PushSubscription {
    /// The longest push service address taken.
    pub const MAX_ENDPOINT: usize = 1024;

    /// Whether this is a subscription a host can push to: an `https`
    /// address of bounded length, a key and an auth secret of their sizes.
    ///
    /// # Errors
    /// What is wrong with it.
    pub fn check(&self) -> Result<(), &'static str> {
        if !self.endpoint.starts_with("https://") || self.endpoint.len() > Self::MAX_ENDPOINT {
            return Err("a push subscription names an https address");
        }
        if self.p256dh.len() != 65 || self.p256dh.first() != Some(&4) {
            return Err("a push subscription carries the browser's P-256 key");
        }
        if self.auth.len() != 16 {
            return Err("a push subscription carries a 16-byte auth secret");
        }
        Ok(())
    }
}

/// The longest a host holds [`ApproverRequest::AwaitPending`] before it
/// answers with what waits, changed or not.
pub const AWAIT_PENDING_NS: u64 = 20 * 1_000_000_000;

/// Host answer to an [`ApproverRequest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApproverReply {
    /// Every pending request.
    Pending(Vec<PendingCertification>),
    /// The decision was delivered to the requester.
    Decided,
    /// The decision was not accepted.
    Refused {
        /// Human-readable reason.
        reason: String,
    },
    DelegateProposals(Vec<PendingDelegate>),
    /// The key this host signs its pushes with, the uncompressed P-256
    /// point a browser subscribes with; empty where it pushes nothing.
    PushKey(Vec<u8>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::StatementSigner as _;

    fn approval(root: &SigningKey, issued_at_ns: u64, request: ApproverRequest) -> MemberApproval {
        let user_public_key = root.verifying_key().to_bytes();
        let message =
            MemberApproval::signed_digest(&user_public_key, issued_at_ns, &request).unwrap();
        MemberApproval {
            user_public_key,
            issued_at_ns,
            request,
            signature: root.sign_digest(&message).unwrap(),
            device: None,
        }
    }

    /// A command one of the member's devices signs, carrying its certificate.
    fn from_device(
        root: &SigningKey,
        device: &SigningKey,
        capabilities: tilo_user_identity::DeviceCapabilities,
        request: ApproverRequest,
    ) -> MemberApproval {
        let now = 1_000 * MEMBER_APPROVAL_SKEW_NS;
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Phone".into(),
            capabilities,
            issued_at_ns: 1,
            expires_at_ns: Some(now + MEMBER_APPROVAL_SKEW_NS),
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
        let user_public_key = root.verifying_key().to_bytes();
        let message = MemberApproval::signed_digest(&user_public_key, now, &request).unwrap();
        MemberApproval {
            user_public_key,
            issued_at_ns: now,
            request,
            signature: device.sign_digest(&message).unwrap(),
            device: Some(cert),
        }
    }

    #[test]
    fn a_device_of_the_member_subscribes_pushes_and_pairs_nothing() {
        let (root, phone) = (
            SigningKey::from_bytes(&[7; 32]),
            SigningKey::from_bytes(&[8; 32]),
        );
        let now = 1_000 * MEMBER_APPROVAL_SKEW_NS;
        let member = tilo_user_identity::DeviceCapabilities::member();
        let asked = from_device(&root, &phone, member.clone(), ApproverRequest::PushKey);
        assert_eq!(asked.verify(now), Ok(()));
        assert!(
            from_device(&root, &phone, member.clone(), ApproverRequest::ListPending)
                .verify(now)
                .is_err(),
            "pairing stays the root's"
        );
        let agent = tilo_user_identity::DeviceCapabilities::agent_grant(
            tilo_user_identity::AgentScope::AllWorkspaces,
        );
        assert!(
            from_device(&root, &phone, agent, ApproverRequest::PushKey)
                .verify(now)
                .is_err(),
            "an agent's key signs nothing for its person"
        );
        let mut forged = from_device(&root, &phone, member, ApproverRequest::PushKey);
        forged.user_public_key = [9; 32];
        assert!(
            forged.verify(now).is_err(),
            "a device signs only for its own person"
        );
    }

    #[test]
    fn a_member_approval_holds_only_for_its_root_command_and_time() {
        let root = SigningKey::from_bytes(&[7; 32]);
        let now = 1_000 * MEMBER_APPROVAL_SKEW_NS;
        let signed = approval(&root, now, ApproverRequest::ListPending);
        assert_eq!(signed.verify(now + 1), Ok(()));
        assert!(
            signed.verify(now + 2 * MEMBER_APPROVAL_SKEW_NS).is_err(),
            "too old"
        );
        let mut other = signed.clone();
        other.request = ApproverRequest::Decide {
            request_digest: [1; 32],
            decision: ApproverDecision::Deny,
        };
        assert!(
            other.verify(now).is_err(),
            "the signature covers the command"
        );
        let mut stranger = signed;
        stranger.user_public_key = SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes();
        assert!(stranger.verify(now).is_err(), "and the root");
    }
}
