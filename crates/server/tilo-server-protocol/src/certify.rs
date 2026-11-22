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
//! An agent the host supervises asks its person before an action their
//! grants leave to them. The host queues the question for this one
//! invocation as a [`PendingConfirmation`]; an approver lists them
//! (`ApproverRequest::Confirmations`), shows each as the host's
//! confirmation sheet ([`PendingConfirmation::question`], Allow once or
//! Deny) and answers with `ApproverRequest::Answer`. An unanswered question
//! expires as a refusal.
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
pub const CERTIFY_ALPN: &[u8] = b"tilo-device-certify/1";
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
/// approve), [`ApproverRequest::Confirmations`]
/// (what the host asks that member about actions run for them) and
/// [`ApproverRequest::Answer`] (to one of those) are answered.
///
/// Their root signs it, or one of their own devices does ([`Self::device`]):
/// a question an agent puts to the person reaches every device of theirs,
/// whichever holds the root, so any of them answers it. A device signs only
/// [`ApproverRequest::Confirmations`], [`ApproverRequest::Answer`],
/// [`ApproverRequest::PushKey`] and [`ApproverRequest::Notify`]; pairing
/// stays the root's.
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
                    ApproverRequest::Confirmations
                        | ApproverRequest::Answer { .. }
                        | ApproverRequest::PushKey
                        | ApproverRequest::Notify(_)
                ) {
                    return Err("only the person's root pairs devices");
                }
                if cert.user_public_key != self.user_public_key {
                    return Err("the device is another person's");
                }
                if cert.capabilities.agent.is_some() {
                    return Err("an agent's key answers no question for its person");
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
    /// Read every question an agent is waiting on.
    Confirmations,
    /// Answer one question.
    Answer {
        /// [`PendingConfirmation::id`] of the question.
        id: [u8; 32],
        /// The person's answer, for that one invocation.
        answer: ConfirmationAnswer,
    },
    /// Read every pending request once they differ from `known`, the digests
    /// the approver holds: answered at once when they already do, and
    /// otherwise as a request arrives or is answered, or after
    /// [`AWAIT_PENDING_NS`]. A device showing a pairing link hears of the new
    /// device as it asks, rather than asking again and again.
    AwaitPending { known: Vec<[u8; 32]> },
    /// The key this host signs its pushes with ([`ApproverReply::PushKey`]):
    /// what a browser subscribes with before [`Self::Notify`].
    PushKey,
    /// Push the person a word in this browser when an agent waits for them
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

/// A page departure the person is asked to allow: what a page they are not
/// on is about to send, where it goes, and the filled page it leaves
/// from. A submission in a hosted Browser is exactly this, and the person's
/// allow is what lets that one request leave.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageDeparture {
    /// The page it leaves from, by its origin (`https://visa.example`); empty
    /// when the window cannot say.
    #[serde(default)]
    pub page: String,
    /// What the page does, as `tilo_webview::HeldKind` names it (`submit`,
    /// `navigate`, `request`, `download`, `payment`).
    #[serde(default)]
    pub kind: String,
    /// The method it leaves with (`POST`); empty when the engine named none.
    #[serde(default)]
    pub method: String,
    /// Where it goes, by its origin (`https://visa.example`); empty when it
    /// names none.
    #[serde(default)]
    pub destination: String,
    /// The filled page as the person allows it: a content address of a PNG in
    /// the workspace's store, which their device reads to see what leaves.
    #[serde(default)]
    pub picture: Option<String>,
}

impl PageDeparture {
    /// What the page is about to do, in the person's words: `send the form to
    /// https://visa.example`, or `open https://visa.example` for a page it
    /// fetches. A kind this build does not know, or a departure that names no
    /// destination, reads as what it is. The HTTP method itself stays out of
    /// the sentence: a person thinks in sending and opening, not in verbs.
    #[must_use]
    pub fn doing(&self) -> String {
        let opening = self.method.eq_ignore_ascii_case("GET");
        let mut text = if opening {
            "open".to_owned()
        } else {
            match self.kind.as_str() {
                "submit" => "send the form".to_owned(),
                "navigate" => "leave for".to_owned(),
                "download" => "download from".to_owned(),
                "payment" => "take a payment to".to_owned(),
                // A request, and whatever a newer window names that this build
                // does not know: it is a request either way.
                _ => "send a request".to_owned(),
            }
        };
        if !self.destination.is_empty() {
            text.push_str(if opening { " " } else { " to " });
            text.push_str(&self.destination);
        }
        text
    }
}

/// A question an agent on the host puts to its person: one invocation of
/// an action the person's grants leave to them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "ConfirmationWire", into = "ConfirmationWire")]
pub struct PendingConfirmation {
    /// Names this question, to answer it.
    pub id: [u8; 32],
    /// [`crate::delegation::Delegate::agent_id`] of the asking agent.
    pub agent_id: String,
    /// The agent's display name: the name its key goes by, as History names
    /// its changes.
    pub agent_label: String,
    /// The group it asks in, by the title the agent gave it; empty when it
    /// named none.
    pub title: String,
    /// The action, by the title its app declares.
    pub action_label: String,
    /// What it acts on: the record by its name.
    pub resource_label: String,
    /// The arguments it would run with, each by its label and value as a
    /// reviewer reads it, long values cut and secrets withheld.
    pub arguments: Vec<(String, String)>,
    /// What the action risks, as `tilo_actions::RiskLevel` names it
    /// (`none`, `low`, `destructive`, `privileged`, `sensitive`), for the
    /// sheet to state. An approver reads an unknown name as the gravest.
    pub risk: String,
    /// What the action is able to do, as `tilo_actions::ActionKind` names it
    /// (`read`, `undoable`, `outside`); empty when the host cannot say.
    pub kind: String,
    /// Whether Undo takes the action back.
    pub undoable: bool,
    /// The workspace the agent acts in.
    pub workspace: String,
    /// Unanswered by then, the question is a refusal; ns since the epoch.
    pub expires_at_ns: u64,
    /// The page departure it asks about, when it asks about one: what leaves a
    /// page nobody stands at, and the filled page it leaves from. Written even
    /// when there is none: a question is encoded by position (postcard), so a
    /// field left out is one its reader still expects.
    pub page: Option<PageDeparture>,
    /// What the agent asks in its own words, where it asks the person
    /// something rather than to allow an action: the action fields are
    /// empty then, and the person answers with one of its choices
    /// ([`ConfirmationAnswer::Reply`]).
    /// Written even when there is none, as [`Self::page`] is.
    pub ask: Option<AgentAsk>,
    /// A lasting workspace grant is offered for the action shown.
    pub allow_always: bool,
}

// Fixed fields of the certification protocol. New approval choices are encoded
// after the reply frame's original payload, outside these positional fields.
#[derive(Serialize, Deserialize)]
struct ConfirmationWire {
    id: [u8; 32],
    agent_id: String,
    agent_label: String,
    title: String,
    action_label: String,
    resource_label: String,
    arguments: Vec<(String, String)>,
    risk: String,
    kind: String,
    undoable: bool,
    workspace: String,
    expires_at_ns: u64,
    #[serde(default)]
    page: Option<PageDeparture>,
    #[serde(default)]
    ask: Option<AgentAsk>,
}

impl From<PendingConfirmation> for ConfirmationWire {
    fn from(value: PendingConfirmation) -> Self {
        Self {
            id: value.id,
            agent_id: value.agent_id,
            agent_label: value.agent_label,
            title: value.title,
            action_label: value.action_label,
            resource_label: value.resource_label,
            arguments: value.arguments,
            risk: value.risk,
            kind: value.kind,
            undoable: value.undoable,
            workspace: value.workspace,
            expires_at_ns: value.expires_at_ns,
            page: value.page,
            ask: value.ask,
        }
    }
}

impl From<ConfirmationWire> for PendingConfirmation {
    fn from(value: ConfirmationWire) -> Self {
        Self {
            id: value.id,
            agent_id: value.agent_id,
            agent_label: value.agent_label,
            title: value.title,
            action_label: value.action_label,
            resource_label: value.resource_label,
            arguments: value.arguments,
            risk: value.risk,
            kind: value.kind,
            undoable: value.undoable,
            workspace: value.workspace,
            expires_at_ns: value.expires_at_ns,
            page: value.page,
            ask: value.ask,
            allow_always: false,
        }
    }
}

/// A question an agent puts to its person in its own words, answered with one of its choices;
/// or the host's own ask for a secret, typed into a protected field.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAsk {
    /// The question, as the agent words it.
    pub question: String,
    /// The answers the person picks from, in order; none for a secret.
    pub choices: Vec<String>,
    /// Whether it asks for a secret, such as a password the host keeps in their vault: the
    /// person types it into a field that hides it, and the answer is
    /// [`ConfirmationAnswer::Secret`], never a [`ConfirmationAnswer::Reply`]. The question is
    /// then the host's own words, not the agent's.
    #[serde(default)]
    pub secret: bool,
}

/// A secret a person typed for an [`AgentAsk`] that asks for one. It is wiped when dropped,
/// and its debug output says nothing of it, so a logged answer never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct TypedSecret(zeroize::Zeroizing<String>);

impl Serialize for TypedSecret {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for TypedSecret {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::new)
    }
}

impl TypedSecret {
    /// `typed`, kept as a secret.
    #[must_use]
    pub fn new(typed: String) -> Self {
        Self(zeroize::Zeroizing::new(typed))
    }

    /// What was typed.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for TypedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TypedSecret(..)")
    }
}

/// The longest secret a person types for an [`AgentAsk`], in bytes.
pub const MAX_SECRET_BYTES: usize = 4_096;

impl PendingConfirmation {
    /// The question as the confirmation sheet puts it.
    #[must_use]
    pub fn question(&self) -> String {
        format!(
            "{} wants to {}: {}, for you",
            self.agent_label, self.action_label, self.resource_label
        )
    }

    /// The page the question is about, by its origin, when it asks about one:
    /// where what leaves goes from, and where to when the window named no
    /// page of its own.
    #[must_use]
    pub fn page_origin(&self) -> Option<String> {
        let page = self.page.as_ref()?;
        let origin = if page.page.is_empty() {
            &page.destination
        } else {
            &page.page
        };
        (!origin.is_empty()).then(|| origin.clone())
    }
}

/// The person's answer to a [`PendingConfirmation`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfirmationAnswer {
    /// Run this one invocation.
    AllowOnce,
    /// Do not carry it out; to an [`AgentAsk`], the person chose not to answer.
    Deny,
    /// The person's answer to an [`AgentAsk`]: one of its choices.
    Reply(String),
    /// Carry out this invocation and keep a workspace grant for its scope.
    AllowAlways,
    /// What the person typed for an [`AgentAsk`] that asks for a secret.
    Secret(TypedSecret),
}

impl ConfirmationAnswer {
    /// Whether it answers `question`: allowing or not for an action, a reply
    /// one of its choices is for an [`AgentAsk`]; declining answers either. A
    /// secret answers only a question that asks for one, and such a question
    /// takes no reply.
    #[must_use]
    pub fn answers(&self, question: &PendingConfirmation) -> bool {
        match (self, &question.ask) {
            (Self::Deny, _) => true,
            (Self::AllowOnce, ask) => ask.is_none(),
            (Self::AllowAlways, ask) => {
                ask.is_none() && question.page.is_none() && question.allow_always
            }
            (Self::Reply(_) | Self::Secret(_), None) => false,
            (Self::Secret(typed), Some(ask)) => {
                ask.secret && !typed.expose().is_empty() && typed.expose().len() <= MAX_SECRET_BYTES
            }
            (Self::Reply(_), Some(ask)) if ask.secret => false,
            (Self::Reply(reply), Some(ask)) => {
                let reply = reply.trim();
                ask.choices.iter().any(|choice| choice == reply)
            }
        }
    }
}

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
    /// Every question an agent is waiting on.
    Confirmations(Vec<PendingConfirmation>),
    /// The key this host signs its pushes with, the uncompressed P-256
    /// point a browser subscribes with; empty where it pushes nothing.
    PushKey(Vec<u8>),
}

// Kept outside the legacy reply so an older client still reads every question.
const GRANT_OFFERS_V1: &[u8] = b"tilo grant offers/1\0";

impl ApproverReply {
    /// Encode the original reply followed by one byte per question when any
    /// offer a lasting workspace grant. Old clients read the original reply.
    ///
    /// # Errors
    /// The reply could not be encoded.
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        let mut bytes = postcard::to_allocvec(self)?;
        if let Self::Confirmations(questions) = self
            && questions.iter().any(|question| question.allow_always)
        {
            bytes.extend_from_slice(GRANT_OFFERS_V1);
            bytes.extend(
                questions
                    .iter()
                    .map(|question| u8::from(question.allow_always)),
            );
        }
        Ok(bytes)
    }

    /// Read a reply from either version of the approver.
    ///
    /// # Errors
    /// The reply or its lasting grant offers are malformed.
    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        let (mut reply, rest): (Self, _) = postcard::take_from_bytes(bytes)?;
        if let Self::Confirmations(questions) = &mut reply
            && let Some(offers) = rest.strip_prefix(GRANT_OFFERS_V1)
        {
            if offers.len() != questions.len() || offers.iter().any(|&offer| offer > 1) {
                return Err(postcard::Error::DeserializeBadEncoding);
            }
            for (question, &offer) in questions.iter_mut().zip(offers) {
                question.allow_always = offer == 1;
            }
        }
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::StatementSigner as _;

    #[test]
    fn confirmation_replies_keep_their_original_payload() {
        let question = |id, allow_always| PendingConfirmation {
            id: [id; 32],
            allow_always,
            ..PendingConfirmation::default()
        };
        let old = ApproverReply::Confirmations(vec![question(1, false), question(2, false)]);
        let new = ApproverReply::Confirmations(vec![question(1, true), question(2, false)]);
        // Confirmations (variant 4), two entries, each the id and thirteen
        // empty fields in their original positions.
        let mut old_wire = vec![4, 2];
        for id in [1, 2] {
            old_wire.extend_from_slice(&[id; 32]);
            old_wire.extend_from_slice(&[0; 13]);
        }
        let new_wire = new.encode().unwrap();

        assert_eq!(ApproverReply::decode(&old_wire).unwrap(), old);
        assert!(new_wire.starts_with(&old_wire));
        assert_eq!(
            postcard::from_bytes::<ApproverReply>(&new_wire).unwrap(),
            old
        );
        assert_eq!(ApproverReply::decode(&new_wire).unwrap(), new);
        assert!(ApproverReply::decode(new_wire.split_last().unwrap().1).is_err());
    }

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

    /// An agent's own question is answered with one of its choices, or
    /// declined; never with other words, and never allowed as an action is.
    /// An action's question takes no reply.
    #[test]
    fn a_reply_answers_only_the_question_it_fits() {
        let ask = || PendingConfirmation {
            ask: Some(AgentAsk {
                question: "Which consulate?".into(),
                choices: vec!["Berlin".into(), "Munich".into()],
                secret: false,
            }),
            ..PendingConfirmation::default()
        };
        let reply = |text: &str| ConfirmationAnswer::Reply(text.into());
        assert!(reply("Munich").answers(&ask()));
        assert!(!reply("Hamburg").answers(&ask()), "no such choice");
        assert!(!reply("  ").answers(&ask()), "nothing answers nothing");
        assert!(ConfirmationAnswer::Deny.answers(&ask()));
        assert!(!ConfirmationAnswer::AllowOnce.answers(&ask()));
        let action = PendingConfirmation::default();
        assert!(ConfirmationAnswer::AllowOnce.answers(&action));
        assert!(!ConfirmationAnswer::AllowAlways.answers(&action));
        assert!(!ConfirmationAnswer::AllowAlways.answers(&ask()));
        assert!(
            ConfirmationAnswer::AllowAlways.answers(&PendingConfirmation {
                allow_always: true,
                ..PendingConfirmation::default()
            })
        );
        assert!(!reply("Munich").answers(&action));
    }

    /// A question for a secret takes the secret typed for it, or a decline: never a reply,
    /// which would carry the password as an ordinary answer. A secret answers nothing else.
    #[test]
    fn a_secret_answers_only_the_question_that_asks_for_one() {
        let password = PendingConfirmation {
            ask: Some(AgentAsk {
                question: "The password of ada@example.com".into(),
                secret: true,
                ..AgentAsk::default()
            }),
            ..PendingConfirmation::default()
        };
        let typed = |text: &str| ConfirmationAnswer::Secret(TypedSecret::new(text.into()));
        assert!(typed(" hunter2 ").answers(&password));
        assert!(
            !typed("").answers(&password),
            "nothing typed answers nothing"
        );
        assert!(!typed(&"x".repeat(MAX_SECRET_BYTES + 1)).answers(&password));
        assert!(!ConfirmationAnswer::Reply("hunter2".into()).answers(&password));
        assert!(ConfirmationAnswer::Deny.answers(&password));
        assert!(!ConfirmationAnswer::AllowOnce.answers(&password));
        let words = PendingConfirmation {
            ask: Some(AgentAsk {
                choices: vec!["Later".into()],
                ..AgentAsk::default()
            }),
            ..PendingConfirmation::default()
        };
        assert!(!typed("hunter2").answers(&words));
        assert!(!typed("hunter2").answers(&PendingConfirmation::default()));
        assert_eq!(format!("{:?}", typed("hunter2")), "Secret(TypedSecret(..))");
        let wire = postcard::to_allocvec(&typed("hunter2")).unwrap();
        assert_eq!(
            postcard::from_bytes::<ConfirmationAnswer>(&wire).unwrap(),
            typed("hunter2")
        );
    }

    #[test]
    fn a_device_of_the_member_answers_their_questions_and_pairs_nothing() {
        let (root, phone) = (
            SigningKey::from_bytes(&[7; 32]),
            SigningKey::from_bytes(&[8; 32]),
        );
        let now = 1_000 * MEMBER_APPROVAL_SKEW_NS;
        let member = tilo_user_identity::DeviceCapabilities::member();
        let asked = from_device(
            &root,
            &phone,
            member.clone(),
            ApproverRequest::Confirmations,
        );
        assert_eq!(asked.verify(now), Ok(()));
        let answered = ApproverRequest::Answer {
            id: [1; 32],
            answer: ConfirmationAnswer::AllowOnce,
        };
        assert_eq!(
            from_device(&root, &phone, member.clone(), answered).verify(now),
            Ok(())
        );
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
            from_device(&root, &phone, agent, ApproverRequest::Confirmations)
                .verify(now)
                .is_err(),
            "an agent's key answers nothing for its person"
        );
        let mut forged = from_device(&root, &phone, member, ApproverRequest::Confirmations);
        forged.user_public_key = [9; 32];
        assert!(
            forged.verify(now).is_err(),
            "a device answers only for its own person"
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

    tilo_test_env::scenario! {
        fn a_question_about_a_page_says_where_it_leaves_from;

        phase a_departure_says_what_leaves_where_and_how;
        // A departure reads as what the page is about to do, and a question that
        // names one says where it leaves from.
        {
            let departure = PageDeparture {
                page: "https://consulate.example".into(),
                kind: "submit".into(),
                method: "POST".into(),
                destination: "https://consulate.example".into(),
                picture: Some("tilo.content:5f2b".into()),
            };
            assert_eq!(
                departure.doing(),
                "send the form to https://consulate.example",
                "what leaves, and where to"
            );
            let opening = PageDeparture {
                kind: "navigate".into(),
                method: "GET".into(),
                ..departure.clone()
            };
            assert_eq!(
                opening.doing(),
                "open https://consulate.example",
                "a page it fetches is opened, not sent"
            );
            let question = PendingConfirmation {
                action_label: departure.doing(),
                page: Some(departure.clone()),
                ..PendingConfirmation::default()
            };
            assert_eq!(
                question.page_origin().as_deref(),
                Some("https://consulate.example")
            );
            // A window that named no page of its own leaves from where it goes.
            let elsewhere = PageDeparture {
                page: String::new(),
                ..departure
            };
            let question = PendingConfirmation {
                page: Some(elsewhere),
                ..question
            };
            assert_eq!(
                question.page_origin().as_deref(),
                Some("https://consulate.example")
            );
            assert_eq!(
                PageDeparture::default().doing(),
                "send a request",
                "a departure this build knows nothing else of is still a request"
            );
        }

        phase a_question_reads_the_page_it_names_and_one_that_names_none;
        // A question that names no page reads as one, and one that names a page
        // is read back as it was written.
        {
            let plain: PendingConfirmation = serde_json::from_value(serde_json::json!({
                "id": serde_json::json!(vec![0u8; 32]),
                "agent_id": "agent",
                "agent_label": "Jane Doe's agent",
                "title": "",
                "action_label": "Send the form",
                "resource_label": "browser.page",
                "arguments": [],
                "risk": "outside",
                "kind": "",
                "undoable": false,
                "workspace": "ws",
                "expires_at_ns": 7,
            }))
            .expect("a question without a page is one");
            assert_eq!(plain.page, None);
            assert_eq!(plain.page_origin(), None);

            let departure = PageDeparture {
                page: "https://visa.example".into(),
                kind: "submit".into(),
                method: "POST".into(),
                destination: "https://visa.example".into(),
                picture: None,
            };
            let written = serde_json::to_value(PendingConfirmation {
                page: Some(departure.clone()),
                ..plain
            })
            .expect("the question encodes");
            assert_eq!(written["page"]["method"], "POST");
            let read: PendingConfirmation = serde_json::from_value(written).expect("and reads back");
            assert_eq!(read.page, Some(departure));
        }
    }
}
