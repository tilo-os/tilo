//! Transport-neutral, signed workspace invitations and proof-of-possession admission.
//!
//! The invitation, join and membership contract a workspace speaks before any
//! workspace state is exchanged: signed invitations and their text form, join
//! and membership proofs, owner transport hints, and the rows a join bootstrap
//! and the governed member list carry. Pure types and
//! signature checks over [`tilo_user_identity`] certificates: no Automerge, no
//! admission, no transport. `tilo-peer-state` re-exports this crate as
//! `tilo_peer_state::join`.
//!
//! [`delegation`] holds the root-signed agent and bot delegations, and
//! [`room_workspace_id`] the stable relay shard a workspace UUID maps to; both
//! are re-exported by `tilo-relay-protocol`.
//!
//! The wire types' `room` fields keep their names: hosts, desktops and the
//! desktop shell read them by name, and postcard encodes a struct by field
//! order, so the names never reach a byte but do reach every caller.

pub mod delegation;
pub mod invitations;
/// The invitation and join layouts a build wrote before the grants stopped
/// naming the workspace's authority: read, never written. A stored record
/// keeps the bytes its device signed, so a build that reads a record written
/// then needs the layout that record is in.
pub mod previous;
/// A link that carries an invitation's place and key, not its kilobytes.
pub mod sealed;
pub mod task_delegation;

use base64::Engine as _;
use ed25519_dalek::Verifier as _;
use serde::{Deserialize, Serialize};
use tilo_user_identity::{DeviceCert, RevocationEntry, RevocationList};

/// What a member may do in a workspace.
pub use collab_types::Role;

/// The invitation whose grant no longer names the workspace's authority: an
/// invitation is issued by a device of a member, and nothing above it.
const INVITATION_VERSION: u32 = 3;
/// An invitation that also names who sent it.
const INVITATION_WITH_INVITER_VERSION: u32 = 4;
/// Longest inviter name an invitation carries, in characters.
pub const MAX_INVITER_NAME_CHARS: usize = 80;
/// Challenges carry no authority device: the device that answers is the one
/// the invitation names.
const JOIN_VERSION: u32 = 2;
const INVITATION_DOMAIN: &[u8] = b"tilo/workspace-invitation/v1\0";
const JOIN_PROOF_DOMAIN: &[u8] = b"tilo/workspace-join-proof/v1\0";
const MEMBERSHIP_PROOF_DOMAIN: &[u8] = b"tilo/workspace-membership-proof/v1\0";
const TRANSPORT_PIN_DOMAIN: &[u8] = b"tilo/workspace-transport-pin/v1\0";

pub const TRANSPORT_IROH_OWNER_ENDPOINT_ADDR_V1: &str = "iroh-owner-endpoint-addr-v1";
pub const TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1: &str =
    "iroh-workspace-host-endpoint-addr-v1";
pub const TRANSPORT_TILO_SESSION_JOIN_HTTP_V1: &str = "tilo-session-join-http-v1";

/// The `code` of a [`JoinTransportResponse::Error`] answering a Complete
/// whose proof the host never admitted and no longer can: it holds neither
/// a live challenge the proof answers nor a completion of its request. The
/// proof is dead; the invitation is redeemed, if at all, through a new
/// challenge. Every other refusal of a Complete may follow an admission the
/// answer did not reach, so a retained proof is resent exactly.
pub const JOIN_PROOF_LAPSED: u16 = 410;

/// The `code` of a [`JoinTransportResponse::Error`] answering a Complete
/// the host admitted before, when the workspace has moved on since: the host
/// no longer replays that answer, and the device, a member now, takes the
/// workspace's current state through the membership exchange
/// ([`JoinTransportRequest::MembershipChallenge`]) instead.
pub const JOIN_COMPLETED_REFRESH: u16 = 409;

/// An application record, named at its committed heads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentRow {
    pub app_id: String,
    pub record_id: String,
    pub heads: Vec<String>,
}

/// One person in a governed workspace, as the governance document lists them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernedMember {
    pub user_public_key: [u8; 32],
    pub role: Role,
    pub display_name: Option<String>,
    pub active_device_keys: Vec<[u8; 32]>,
    pub removed: bool,
    /// Made an administrator by the workspace's members: `role` is then
    /// [`Role::Editor`] holding every governance action of the
    /// workspace, bounded by that role
    /// (`tilo_action_policy::roles::administrator_grant`).
    #[serde(default)]
    pub administrator: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportHint {
    pub kind: String,
    pub opaque_json: String,
    /// Root-certified host identity whose exact device key must terminate the
    /// hinted transport. This pin is covered by the invitation signature.
    pub host: DeviceCert,
    /// Domain-separated service key actually terminating this endpoint.
    pub service_public_key: [u8; 32],
    /// Host-device signature binding the exact hint and service key.
    #[serde(with = "serde_bytes")]
    pub host_signature: [u8; 64],
}

#[derive(Serialize)]
struct TransportPinView<'a> {
    kind: &'a str,
    opaque_json: &'a str,
    service_public_key: &'a [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteGrant {
    pub workspace_id: u64,
    /// The workspace's id, a canonical lowercase UUID.
    pub room: String,
    /// Device that signs this invitation: a device of a member the workspace's
    /// policy lets invite, which every participant checks on admission.
    pub issuer: DeviceCert,
    pub role: Role,
    pub nonce: [u8; 16],
    /// `Some` is a recipient-bound invitation and must match exactly. `None`
    /// is an explicit single-use bearer invitation for an unknown person.
    pub recipient_user_public_key: Option<[u8; 32]>,
    pub invitee_name: Option<String>,
    pub expires_at_ns: u64,
    pub transport: Vec<TransportHint>,
}

/// The person who sent an invitation, as the issuer recorded them under its
/// signature: the user root of the device that asked for it and the name
/// they gave.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inviter {
    pub user_public_key: [u8; 32],
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SignedInvitation {
    version: u32,
    grant: InviteGrant,
    #[serde(with = "serde_bytes")]
    signature: [u8; 64],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SignedInvitationWithInviter {
    version: u32,
    grant: InviteGrant,
    inviter: Inviter,
    #[serde(with = "serde_bytes")]
    signature: [u8; 64],
}

#[derive(Serialize)]
struct InvitationView<'a> {
    version: u32,
    grant: &'a InviteGrant,
}

#[derive(Serialize)]
struct InvitationWithInviterView<'a> {
    version: u32,
    grant: &'a InviteGrant,
    inviter: &'a Inviter,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedJoin {
    invite_id: [u8; 32],
    grant: InviteGrant,
    inviter: Option<Inviter>,
    signed_invitation: Vec<u8>,
    /// The authority an invitation issued before grants stopped naming one
    /// was delegated from, where it was not its issuer.
    authority: Option<DeviceCert>,
}

impl VerifiedJoin {
    pub fn invite_id(&self) -> [u8; 32] {
        self.invite_id
    }

    pub fn grant(&self) -> &InviteGrant {
        &self.grant
    }

    /// The device the workspace's host is: the invitation's issuer, or, for
    /// an invitation issued before grants named no authority, the authority its
    /// issuer was delegated from.
    pub fn workspace_authority(&self) -> &DeviceCert {
        self.authority.as_ref().unwrap_or(&self.grant.issuer)
    }

    /// Who sent the invitation, when its issuer recorded them.
    pub fn inviter(&self) -> Option<&Inviter> {
        self.inviter.as_ref()
    }

    pub fn signed_invitation(&self) -> &[u8] {
        &self.signed_invitation
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinChallenge {
    pub version: u32,
    pub signed_invitation: Vec<u8>,
    /// The workspace's id, a canonical lowercase UUID.
    pub room: String,
    pub workspace_id: u64,
    pub user_public_key: [u8; 32],
    pub device_public_key: [u8; 32],
    pub request_id: [u8; 32],
    pub nonce: [u8; 32],
    pub expires_at_ns: u64,
}

impl JoinChallenge {
    /// Issue a join challenge for a device redeeming `join`.
    pub fn new(
        join: &VerifiedJoin,
        device_cert: &DeviceCert,
        request_id: [u8; 32],
        nonce: [u8; 32],
        expires_at_ns: u64,
        now_ns: u64,
    ) -> Result<Self, String> {
        validate_certificate(device_cert, &RevocationList::new(), now_ns)?;
        if join
            .grant
            .recipient_user_public_key
            .is_some_and(|recipient| recipient != device_cert.user_public_key)
        {
            return Err("invitation recipient does not match device certificate".into());
        }
        if expires_at_ns <= now_ns || expires_at_ns > join.grant.expires_at_ns {
            return Err("join challenge has invalid expiry".into());
        }
        Ok(Self {
            version: JOIN_VERSION,
            signed_invitation: join.signed_invitation.clone(),
            room: join.grant.room.clone(),
            workspace_id: join.grant.workspace_id,
            user_public_key: device_cert.user_public_key,
            device_public_key: device_cert.device_public_key,
            request_id,
            nonce,
            expires_at_ns,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinProof {
    pub challenge: JoinChallenge,
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedRedemption {
    join: VerifiedJoin,
    certificate: DeviceCert,
    request_id: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipChallenge {
    pub version: u32,
    /// The workspace's id, a canonical lowercase UUID.
    pub room: String,
    pub workspace_id: u64,
    /// The device answering: a device of the workspace's owner.
    #[serde(alias = "authority_device_public_key")]
    pub host_device_public_key: [u8; 32],
    pub user_public_key: [u8; 32],
    pub device_public_key: [u8; 32],
    pub request_id: [u8; 32],
    pub nonce: [u8; 32],
    pub expires_at_ns: u64,
}

impl MembershipChallenge {
    /// Issue a membership refresh challenge for a device of `workspace`.
    pub fn new(
        workspace: &str,
        workspace_id: u64,
        host: &DeviceCert,
        device_cert: &DeviceCert,
        request_id: [u8; 32],
        nonce: [u8; 32],
        expires_at_ns: u64,
        now_ns: u64,
    ) -> Result<Self, String> {
        let canonical = uuid::Uuid::parse_str(workspace)
            .map_err(|_| "membership workspace is not a UUID".to_string())?
            .to_string();
        validate_certificate(host, &RevocationList::new(), now_ns)?;
        validate_certificate(device_cert, &RevocationList::new(), now_ns)?;
        if expires_at_ns <= now_ns {
            return Err("membership challenge has invalid expiry".into());
        }
        Ok(Self {
            version: JOIN_VERSION,
            room: canonical,
            workspace_id,
            host_device_public_key: host.device_public_key,
            user_public_key: device_cert.user_public_key,
            device_public_key: device_cert.device_public_key,
            request_id,
            nonce,
            expires_at_ns,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipProof {
    pub challenge: MembershipChallenge,
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedMembership {
    challenge: MembershipChallenge,
    device_cert: DeviceCert,
}

impl VerifiedMembership {
    pub fn workspace(&self) -> &str {
        &self.challenge.room
    }
    pub fn workspace_id(&self) -> u64 {
        self.challenge.workspace_id
    }
    pub fn host_device_public_key(&self) -> [u8; 32] {
        self.challenge.host_device_public_key
    }
    pub fn request_id(&self) -> [u8; 32] {
        self.challenge.request_id
    }
    pub fn device_cert(&self) -> &DeviceCert {
        &self.device_cert
    }
}

impl VerifiedRedemption {
    pub fn join(&self) -> &VerifiedJoin {
        &self.join
    }

    pub fn certificate(&self) -> &DeviceCert {
        &self.certificate
    }

    pub fn request_id(&self) -> [u8; 32] {
        self.request_id
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinTransportRequest {
    Inspect { signed_invitation: Vec<u8> },
    Challenge(JoinChallengeRequest),
    Complete(JoinCompleteRequest),
    MembershipChallenge(MembershipChallengeRequest),
    MembershipComplete(MembershipCompleteRequest),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinChallengeRequest {
    pub signed_invitation: Vec<u8>,
    pub device_cert: DeviceCert,
    pub request_id: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinChallengeResponse {
    pub challenge: JoinChallenge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinCompleteRequest {
    pub proof: JoinProof,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipChallengeRequest {
    /// The workspace's id, a canonical lowercase UUID.
    pub room: String,
    pub workspace_id: u64,
    pub device_cert: DeviceCert,
    pub request_id: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipChallengeResponse {
    pub challenge: MembershipChallenge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipCompleteRequest {
    pub proof: MembershipProof,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinInspection {
    pub invite_id: [u8; 32],
    pub grant: InviteGrant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinBootstrap {
    pub version: u32,
    pub invite_id: [u8; 32],
    pub device_cert: DeviceCert,
    pub role: Role,
    /// The workspace's id, a canonical lowercase UUID.
    pub room: String,
    pub workspace_id: u64,
    /// The device that answered: a device of the workspace's owner.
    #[serde(alias = "authority")]
    pub host: DeviceCert,
    pub certified_roster: Vec<DeviceCert>,
    pub revocations: Vec<RevocationEntry>,
    pub records: Vec<DocumentRow>,
    pub governance_messages: Vec<Vec<u8>>,
    pub generation: u64,
}

/// Owner-specific bootstrap data. Authority is established only by admitting
/// the signed governance and deriving the structural Owner from that state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerBootstrapV1 {
    pub version: u32,
    pub device_cert: DeviceCert,
    /// The workspace's id, a canonical lowercase UUID.
    pub room: String,
    pub workspace_id: u64,
    /// The device that answered: a device of the workspace's owner.
    #[serde(alias = "authority")]
    pub host: DeviceCert,
    pub certified_roster: Vec<DeviceCert>,
    pub revocations: Vec<RevocationEntry>,
    pub records: Vec<DocumentRow>,
    pub governance_messages: Vec<Vec<u8>>,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinTransportResponse {
    Inspection(JoinInspection),
    Challenge(JoinChallengeResponse),
    Complete(JoinBootstrap),
    MembershipChallenge(MembershipChallengeResponse),
    MembershipComplete(JoinBootstrap),
    Error { code: u16, message: String },
    OwnerMembershipComplete(OwnerBootstrapV1),
}

/// Sign an invitation grant with the issuer device's `key`, which answers at once
/// ([`collab_types::signer::sign_now`]), returning its wire bytes.
pub fn issue_invitation(
    grant: InviteGrant,
    key: &dyn collab_types::signer::DeviceSigner,
) -> Result<Vec<u8>, String> {
    validate_grant(&grant, 0, false)?;
    if key.device_public_key() != grant.issuer.device_public_key {
        return Err("invitation signer is not the declared issuer device".into());
    }
    let view = InvitationView {
        version: INVITATION_VERSION,
        grant: &grant,
    };
    let digest = tagged_digest(INVITATION_DOMAIN, &view)?;
    let signed = SignedInvitation {
        version: INVITATION_VERSION,
        grant,
        signature: collab_types::signer::sign_now(key, digest.to_vec())
            .map_err(|error| error.to_string())?,
    };
    postcard::to_stdvec(&signed).map_err(|error| error.to_string())
}

/// [`issue_invitation`] that also names `inviter`, under the same signature.
pub fn issue_invitation_from(
    grant: InviteGrant,
    inviter: Inviter,
    key: &dyn collab_types::signer::DeviceSigner,
) -> Result<Vec<u8>, String> {
    validate_grant(&grant, 0, false)?;
    validate_inviter(&inviter)?;
    if key.device_public_key() != grant.issuer.device_public_key {
        return Err("invitation signer is not the declared issuer device".into());
    }
    let digest = tagged_digest(
        INVITATION_DOMAIN,
        &InvitationWithInviterView {
            version: INVITATION_WITH_INVITER_VERSION,
            grant: &grant,
            inviter: &inviter,
        },
    )?;
    let signed = SignedInvitationWithInviter {
        version: INVITATION_WITH_INVITER_VERSION,
        grant,
        inviter,
        signature: collab_types::signer::sign_now(key, digest.to_vec())
            .map_err(|error| error.to_string())?,
    };
    postcard::to_stdvec(&signed).map_err(|error| error.to_string())
}

fn validate_inviter(inviter: &Inviter) -> Result<(), String> {
    let name = inviter.name.trim();
    if name.is_empty()
        || name != inviter.name
        || name.chars().count() > MAX_INVITER_NAME_CHARS
        || name.chars().any(char::is_control)
    {
        return Err("invalid inviter name".into());
    }
    Ok(())
}

pub fn sign_transport_hint(
    kind: impl Into<String>,
    opaque_json: impl Into<String>,
    host: DeviceCert,
    service_public_key: [u8; 32],
    host_key: &dyn collab_types::signer::DeviceSigner,
) -> Result<TransportHint, String> {
    if host_key.device_public_key() != host.device_public_key {
        return Err("transport hint signer is not the certified host device".into());
    }
    let kind = kind.into();
    let opaque_json = opaque_json.into();
    let digest = tagged_digest(
        TRANSPORT_PIN_DOMAIN,
        &TransportPinView {
            kind: &kind,
            opaque_json: &opaque_json,
            service_public_key: &service_public_key,
        },
    )?;
    Ok(TransportHint {
        kind,
        opaque_json,
        host,
        service_public_key,
        host_signature: collab_types::signer::sign_now(host_key, digest.to_vec())
            .map_err(|error| error.to_string())?,
    })
}

/// Verify a transport hint's signature and host certificate.
pub fn verify_transport_hint(hint: &TransportHint) -> Result<(), String> {
    let key = ed25519_dalek::VerifyingKey::from_bytes(&hint.host.device_public_key)
        .map_err(|_| "invalid transport host device key".to_string())?;
    let digest = tagged_digest(
        TRANSPORT_PIN_DOMAIN,
        &TransportPinView {
            kind: &hint.kind,
            opaque_json: &hint.opaque_json,
            service_public_key: &hint.service_public_key,
        },
    )?;
    key.verify(
        &digest,
        &ed25519_dalek::Signature::from_bytes(&hint.host_signature),
    )
    .map_err(|_| "transport host did not sign the endpoint service key".to_string())
}

/// Verify a transport hint at `now_ns` against `revocations`.
pub fn verify_transport_hint_at(
    hint: &TransportHint,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<(), String> {
    validate_certificate(&hint.host, revocations, now_ns)?;
    verify_transport_hint(hint)
}

/// The runs of `signed_invitation`'s bytes that invitations from one issuer
/// over one host share: the issuer's certificate and the signed transport
/// details, each as it is encoded there. Read, not verified: a keeper of
/// many invitations names these runs once, and checks each invitation whole
/// when it puts it back together. An invitation of an earlier layout, or
/// one that does not decode, shares nothing.
#[must_use]
pub fn invitation_shared_runs(signed_invitation: &[u8]) -> Vec<Vec<u8>> {
    let grant = match postcard::take_from_bytes::<u32>(signed_invitation) {
        Ok((INVITATION_VERSION, _)) => {
            postcard::from_bytes::<SignedInvitation>(signed_invitation).map(|signed| signed.grant)
        }
        Ok((INVITATION_WITH_INVITER_VERSION, _)) => {
            postcard::from_bytes::<SignedInvitationWithInviter>(signed_invitation)
                .map(|signed| signed.grant)
        }
        _ => return Vec::new(),
    };
    let Ok(grant) = grant else {
        return Vec::new();
    };
    [
        postcard::to_stdvec(&grant.issuer),
        postcard::to_stdvec(&grant.transport),
    ]
    .into_iter()
    .filter_map(Result::ok)
    .collect()
}

/// The text an invitation is handed over in: its signed bytes as base64.
pub fn encode_invitation_text(signed_invitation: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signed_invitation)
}

/// The signed invitation behind its text ([`encode_invitation_text`]): what
/// the text spells out without base64, for a code that has to be small
/// enough to scan.
pub fn invitation_bytes(text: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| "invalid invitation text encoding".to_string())
}

pub fn decode_and_verify_invitation_text(
    text: &str,
    recipient_user_public_key: Option<[u8; 32]>,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<VerifiedJoin, String> {
    let bytes = invitation_bytes(text)?;
    decode_and_verify_invitation(&bytes, recipient_user_public_key, revocations, now_ns)
}

pub fn decode_and_verify_invitation(
    signed_invitation: &[u8],
    recipient_user_public_key: Option<[u8; 32]>,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<VerifiedJoin, String> {
    let malformed = |_| "invalid signed invitation".to_string();
    let (version, _) = postcard::take_from_bytes::<u32>(signed_invitation).map_err(malformed)?;
    if previous::is_invitation_version(version) {
        let join = VerifiedJoin::from_decoded(
            previous::decode_invitation(signed_invitation, version)?,
            signed_invitation.to_vec(),
        );
        validate_decoded(&join.grant, recipient_user_public_key, revocations, now_ns)?;
        return Ok(join);
    }
    let (grant, inviter, signature) = match version {
        INVITATION_VERSION => {
            let signed: SignedInvitation =
                postcard::from_bytes(signed_invitation).map_err(malformed)?;
            (signed.grant, None, signed.signature)
        }
        INVITATION_WITH_INVITER_VERSION => {
            let signed: SignedInvitationWithInviter =
                postcard::from_bytes(signed_invitation).map_err(malformed)?;
            validate_inviter(&signed.inviter)?;
            (signed.grant, Some(signed.inviter), signed.signature)
        }
        _ => return Err("unsupported invitation version".into()),
    };
    validate_decoded(&grant, recipient_user_public_key, revocations, now_ns)?;
    let digest = match &inviter {
        None => tagged_digest(
            INVITATION_DOMAIN,
            &InvitationView {
                version,
                grant: &grant,
            },
        )?,
        Some(inviter) => tagged_digest(
            INVITATION_DOMAIN,
            &InvitationWithInviterView {
                version,
                grant: &grant,
                inviter,
            },
        )?,
    };
    // A workspace re-reads every invitation its members redeemed whenever its
    // governance changes: one that verified before is remembered.
    tilo_user_identity::verify_remembered(
        &grant.issuer.device_public_key,
        &digest,
        &signature,
        false,
    )
    .map_err(|refused| match refused {
        tilo_user_identity::RememberedRefusal::Key => {
            "invalid invitation issuer device key".to_string()
        }
        tilo_user_identity::RememberedRefusal::Signature => {
            "invitation signature did not verify".to_string()
        }
    })?;
    Ok(VerifiedJoin {
        invite_id: *blake3::hash(signed_invitation).as_bytes(),
        grant,
        inviter,
        signed_invitation: signed_invitation.to_vec(),
        authority: None,
    })
}

/// What every invitation is checked for however it was laid out: a canonical
/// grant into a live workspace, a live issuer certificate and transport pins, and
/// the recipient where the redeemer names one.
fn validate_decoded(
    grant: &InviteGrant,
    recipient_user_public_key: Option<[u8; 32]>,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<(), String> {
    validate_grant(grant, now_ns, true)?;
    validate_certificate(&grant.issuer, revocations, now_ns)?;
    for hint in &grant.transport {
        verify_transport_hint_at(hint, revocations, now_ns)?;
    }
    if let Some(expected) = recipient_user_public_key
        && grant
            .recipient_user_public_key
            .is_some_and(|recipient| recipient != expected)
    {
        return Err("invitation recipient mismatch".into());
    }
    Ok(())
}

/// Sign the answer to a join challenge.
///
/// # Errors
///
/// Returns an error if the challenge cannot be encoded for signing.
pub fn sign_join_proof(
    challenge: JoinChallenge,
    key: &dyn collab_types::signer::DeviceSigner,
) -> Result<JoinProof, String> {
    if key.device_public_key() != challenge.device_public_key {
        return Err("join proof signer is not the challenged device".into());
    }
    let digest = tagged_digest(JOIN_PROOF_DOMAIN, &challenge)?;
    Ok(JoinProof {
        challenge,
        signature: collab_types::signer::sign_now(key, digest.to_vec())
            .map_err(|error| error.to_string())?,
    })
}

/// Verify a join proof against the challenge that was issued.
pub fn verify_join_proof(
    proof: &JoinProof,
    issued_challenge: &JoinChallenge,
    device_cert: &DeviceCert,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<VerifiedRedemption, String> {
    if proof.challenge != *issued_challenge {
        return Err("join proof does not match the issued challenge".into());
    }
    if proof.challenge.version != JOIN_VERSION || now_ns >= proof.challenge.expires_at_ns {
        return Err("join challenge is expired or unsupported".into());
    }
    validate_certificate(device_cert, revocations, now_ns)?;
    if device_cert.user_public_key != proof.challenge.user_public_key
        || device_cert.device_public_key != proof.challenge.device_public_key
    {
        return Err("join proof identity does not match device certificate".into());
    }
    let join = decode_and_verify_invitation(
        &proof.challenge.signed_invitation,
        Some(device_cert.user_public_key),
        revocations,
        now_ns,
    )?;
    if join.grant.room != proof.challenge.room
        || join.grant.workspace_id != proof.challenge.workspace_id
    {
        return Err("join challenge does not match signed invitation".into());
    }
    let key = ed25519_dalek::VerifyingKey::from_bytes(&device_cert.device_public_key)
        .map_err(|_| "invalid join device public key".to_string())?;
    let digest = tagged_digest(JOIN_PROOF_DOMAIN, &proof.challenge)?;
    key.verify(
        &digest,
        &ed25519_dalek::Signature::from_bytes(&proof.signature),
    )
    .map_err(|_| "join proof signature did not verify".to_string())?;
    Ok(VerifiedRedemption {
        join,
        certificate: device_cert.clone(),
        request_id: proof.challenge.request_id,
    })
}

/// Verify the redemption of `signed_invitation` by the device `device_cert`
/// certifies, which already proved it holds its key on the connection that
/// carries the redemption: a signature over keying material exported from
/// that connection's TLS session, which its host checked. That proof is fresh
/// and unique to the connection as a host challenge's answer is to the
/// challenge, so no challenge is issued: the invitation and the certificate
/// are checked as [`verify_join_proof`] checks them.
///
/// # Errors
///
/// The certificate does not verify or is revoked, or the invitation does not
/// verify for its person.
pub fn verify_connection_redemption(
    signed_invitation: &[u8],
    device_cert: &DeviceCert,
    request_id: [u8; 32],
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<VerifiedRedemption, String> {
    validate_certificate(device_cert, revocations, now_ns)?;
    let join = decode_and_verify_invitation(
        signed_invitation,
        Some(device_cert.user_public_key),
        revocations,
        now_ns,
    )?;
    Ok(VerifiedRedemption {
        join,
        certificate: device_cert.clone(),
        request_id,
    })
}

/// Verify a stored `JoinTransportRequest::Complete`, in whichever challenge
/// layout its bytes hold.
///
/// A record written before the join version bump keeps its Complete bytes:
/// they are what the device signed, and nothing here could sign for it
/// again. Its challenge is therefore checked against the layout its version
/// names, and everything else - the scope the invitation pins, the device
/// the challenge names - the same way. `now_ns` is when the proof was made:
/// a stored proof is not fresh, and the caller says when it was.
///
/// # Errors
/// The bytes are not a Complete of either layout, or its proof does not
/// check.
pub fn verify_stored_complete(
    bytes: &[u8],
    device_cert: &DeviceCert,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<VerifiedRedemption, String> {
    match postcard::from_bytes::<JoinTransportRequest>(bytes) {
        Ok(request) if postcard::to_stdvec(&request).is_ok_and(|encoded| encoded == bytes) => {
            let JoinTransportRequest::Complete(complete) = request else {
                return Err("stored join proof is not Complete".into());
            };
            let proof = complete.proof;
            let redemption =
                verify_join_proof(&proof, &proof.challenge, device_cert, revocations, now_ns)?;
            let expected = JoinChallenge::new(
                &redemption.join,
                device_cert,
                proof.challenge.request_id,
                proof.challenge.nonce,
                proof.challenge.expires_at_ns,
                now_ns,
            )?;
            if expected != proof.challenge {
                return Err("join challenge scope mismatch".into());
            }
            Ok(redemption)
        }
        _ => previous::verify_complete(bytes, device_cert, revocations, now_ns),
    }
}

/// Sign the answer to a membership challenge.
///
/// # Errors
///
/// Returns an error if the challenge cannot be encoded for signing.
pub fn sign_membership_proof(
    challenge: MembershipChallenge,
    key: &dyn collab_types::signer::DeviceSigner,
) -> Result<MembershipProof, String> {
    if key.device_public_key() != challenge.device_public_key {
        return Err("membership proof signer is not the challenged device".into());
    }
    let digest = tagged_digest(MEMBERSHIP_PROOF_DOMAIN, &challenge)?;
    Ok(MembershipProof {
        challenge,
        signature: collab_types::signer::sign_now(key, digest.to_vec())
            .map_err(|error| error.to_string())?,
    })
}

/// Verify a membership proof against the challenge that was issued.
pub fn verify_membership_proof(
    proof: &MembershipProof,
    issued_challenge: &MembershipChallenge,
    device_cert: &DeviceCert,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<VerifiedMembership, String> {
    if proof.challenge != *issued_challenge {
        return Err("membership proof does not match the issued challenge".into());
    }
    if proof.challenge.version != JOIN_VERSION || now_ns >= proof.challenge.expires_at_ns {
        return Err("membership challenge is expired or unsupported".into());
    }
    validate_certificate(device_cert, revocations, now_ns)?;
    if device_cert.user_public_key != proof.challenge.user_public_key
        || device_cert.device_public_key != proof.challenge.device_public_key
    {
        return Err("membership proof identity does not match device certificate".into());
    }
    let key = ed25519_dalek::VerifyingKey::from_bytes(&device_cert.device_public_key)
        .map_err(|_| "invalid membership device public key".to_string())?;
    let digest = tagged_digest(MEMBERSHIP_PROOF_DOMAIN, &proof.challenge)?;
    key.verify(
        &digest,
        &ed25519_dalek::Signature::from_bytes(&proof.signature),
    )
    .map_err(|_| "membership proof signature did not verify".to_string())?;
    Ok(VerifiedMembership {
        challenge: proof.challenge.clone(),
        device_cert: device_cert.clone(),
    })
}

fn validate_grant(grant: &InviteGrant, now_ns: u64, enforce_expiry: bool) -> Result<(), String> {
    let canonical = uuid::Uuid::parse_str(&grant.room)
        .map_err(|_| "invitation workspace is not a UUID".to_string())?
        .to_string();
    if canonical != grant.room {
        return Err("invitation workspace is not canonical".into());
    }
    if enforce_expiry && now_ns >= grant.expires_at_ns {
        return Err("invitation has expired".into());
    }
    if grant.transport.len() > 32
        || grant.transport.iter().any(|hint| {
            hint.kind.is_empty()
                || hint.kind.len() > 128
                || hint.opaque_json.len() > 64 * 1024
                || serde_json::from_str::<serde_json::Value>(&hint.opaque_json).is_err()
        })
    {
        return Err("invalid invitation transport hints".into());
    }
    if grant
        .invitee_name
        .as_ref()
        .is_some_and(|name| tilo_user_identity::display_name(name).is_err())
    {
        return Err("invitation display name is invalid".into());
    }
    Ok(())
}

fn validate_certificate(
    certificate: &DeviceCert,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<(), String> {
    tilo_user_identity::verify_device_cert_at(certificate, now_ns)
        .map_err(|error| error.to_string())?;
    if revocations.refuses(certificate) {
        return Err("device certificate is revoked".into());
    }
    Ok(())
}

fn tagged_digest(value: &[u8], body: &impl Serialize) -> Result<[u8; 32], String> {
    let encoded = postcard::to_stdvec(body).map_err(|error| error.to_string())?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(value);
    hasher.update(&encoded);
    Ok(*hasher.finalize().as_bytes())
}

/// Derive the stable numeric workspace shard for a workspace UUID. This is
/// shared by the shell and admin CLI so local workspace handles never leak
/// onto the wire and operators cannot accidentally bind a different shard.
///
/// The name and the derivation key stay as they are: every peer derives the
/// shard it dials from them, so either changing reshards every workspace.
pub fn room_workspace_id(workspace: &str) -> Result<u64, uuid::Error> {
    let workspace = uuid::Uuid::parse_str(workspace)?;
    // Encode on the stack: bounded delivery paths derive this id under a
    // native allowance that funds no heap allocation.
    let mut canonical = [0u8; 36];
    let hash = blake3::derive_key(
        "tilo-room-transport-v1",
        workspace
            .hyphenated()
            .encode_lower(&mut canonical)
            .as_bytes(),
    );
    let mut first8 = [0u8; 8];
    first8.copy_from_slice(&hash[..8]);
    let id = u64::from_le_bytes(first8);
    Ok(if id == 0 { 1 } else { id })
}

/// The topic a workspace's plane numbers itself by, from its id: number 0 for a desk that shows no
/// workspace.
#[must_use]
pub fn workspace_topic(workspace: &str) -> u64 {
    let Ok(id) = uuid::Uuid::parse_str(workspace) else {
        return 0;
    };
    if id.is_nil() {
        return 0;
    }
    room_workspace_id(workspace).unwrap_or(0)
}

#[cfg(test)]
mod tests;

/// Sign `grant` in the invitation layout issued before grants stopped naming
/// the workspace's authority: the bytes a record written then holds, so a decoder
/// for them is tested against what a build wrote, not against itself.
///
/// # Errors
/// Returns an error if the grant cannot be encoded for signing.
#[cfg(any(test, feature = "test-support"))]
pub fn issue_previous_invitation(
    grant: &InviteGrant,
    inviter: Option<&Inviter>,
    authority: &DeviceCert,
    key: &ed25519_dalek::SigningKey,
) -> Result<Vec<u8>, String> {
    previous::encode_invitation(grant, inviter, authority, key)
}

/// A `JoinTransportRequest::Complete` whose challenge is in the layout issued
/// before the join version bump, signed by `key`: the stored proof a record
/// written then holds.
///
/// # Errors
/// Returns an error if the proof cannot be encoded.
#[cfg(any(test, feature = "test-support"))]
pub fn complete_previous_layout(
    challenge: &JoinChallenge,
    authority: &DeviceCert,
    key: &ed25519_dalek::SigningKey,
) -> Result<Vec<u8>, String> {
    previous::encode_complete(challenge, authority, key)
}
