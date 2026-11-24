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
//! [`delegation`] holds the root-signed agent delegations; it is
//! re-exported by `tilo-server-protocol`.
//!
//! A workspace is named by its [`WorkspaceId`] everywhere here.

pub mod delegation;
pub mod invitations;
pub mod native;
mod workspace_text;

use std::collections::BTreeMap;

use base64::Engine as _;
pub use collab_types::WorkspaceId;
use ed25519_dalek::Verifier as _;
use serde::{Deserialize, Serialize};
use tilo_user_identity::{DeviceCert, EndpointDelegation, RevocationEntry, RevocationList};

/// What a member may do in a workspace.
pub use collab_types::Role;
pub use collab_types::{Member, MemberId, Person};

/// The invitation whose grant names its workspace by its [`WorkspaceId`]
/// alone and no authority: an invitation is issued by a device of a member,
/// and nothing above it.
const INVITATION_VERSION: u32 = 5;
/// An invitation that also names who sent it.
const INVITATION_WITH_INVITER_VERSION: u32 = 6;
/// An invitation that makes its holder an administrator besides giving them
/// its role, naming who sent it when its issuer knows: written only for an
/// administrator's invitation, so every other invitation stays in a layout
/// every build reads.
const ADMINISTRATOR_INVITATION_VERSION: u32 = 7;
/// Longest inviter name an invitation carries, in characters.
pub const MAX_INVITER_NAME_CHARS: usize = 80;
/// Challenges carry no authority device, and name the workspace by its
/// [`WorkspaceId`] alone.
const JOIN_VERSION: u32 = 4;
/// The variant index of [`JoinTransportRequest::Complete`] in every join
/// layout: what a stored Complete's bytes start with.
const COMPLETE_VARIANT: u32 = 2;
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
    pub document_id: String,
    pub heads: Vec<String>,
}

/// A workspace's member list, and the devices each member holds now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberList {
    pub members: Vec<Member>,
    /// The device keys of each member, by the member whose root certified them.
    #[serde(with = "devices_by_member")]
    pub devices: BTreeMap<MemberId, Vec<[u8; 32]>>,
}

/// A member is no JSON object key, and the daemon publishes a reading that holds this list as
/// JSON: the devices travel as pairs of member and keys, which postcard writes as it writes the
/// map.
mod devices_by_member {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Deserializer, Serializer};

    use super::MemberId;

    pub(super) fn serialize<S: Serializer>(
        devices: &BTreeMap<MemberId, Vec<[u8; 32]>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(devices)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<MemberId, Vec<[u8; 32]>>, D::Error> {
        Ok(Vec::<(MemberId, Vec<[u8; 32]>)>::deserialize(deserializer)?
            .into_iter()
            .collect())
    }
}

impl MemberList {
    /// The member whose device `device` is.
    #[must_use]
    pub fn member_of(&self, device: &[u8; 32]) -> Option<&Member> {
        self.devices
            .iter()
            .find(|(_, devices)| devices.contains(device))
            .and_then(|(id, _)| self.members.iter().find(|member| &member.id == id))
    }
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
    pub workspace_id: WorkspaceId,
    /// Device that signs this invitation: a device of a member the workspace's
    /// policy lets invite, which every key holder checks on admission.
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
/// they go by there, empty when the workspace knows none.
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SignedAdministratorInvitation {
    version: u32,
    grant: InviteGrant,
    inviter: Option<Inviter>,
    administrator: bool,
    #[serde(with = "serde_bytes")]
    signature: [u8; 64],
}

#[derive(Serialize)]
struct AdministratorInvitationView<'a> {
    version: u32,
    grant: &'a InviteGrant,
    inviter: &'a Option<Inviter>,
    administrator: bool,
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
    administrator: bool,
    signed_invitation: Vec<u8>,
}

/// A signed invitation of any layout, decoded and its signature checked.
struct Decoded {
    grant: InviteGrant,
    inviter: Option<Inviter>,
    administrator: bool,
}

impl VerifiedJoin {
    pub fn invite_id(&self) -> [u8; 32] {
        self.invite_id
    }

    pub fn grant(&self) -> &InviteGrant {
        &self.grant
    }

    /// The device that issued the invitation: the device of a member the
    /// workspace's policy let invite.
    pub fn workspace_authority(&self) -> &DeviceCert {
        &self.grant.issuer
    }

    /// Who sent the invitation, when its issuer recorded them.
    pub fn inviter(&self) -> Option<&Inviter> {
        self.inviter.as_ref()
    }

    /// Whether the invitation makes its holder an administrator besides
    /// giving them its role. Whoever admits them checks that the people it
    /// names ([`Self::granted_by`]) may make administrators.
    pub const fn administrator(&self) -> bool {
        self.administrator
    }

    /// The people whose authority the invitation carries, by user root: its
    /// issuer's person, and the inviter it names when that is someone else
    /// (a host signs for the person who asked it). Each must be allowed
    /// what the invitation gives.
    pub fn granted_by(&self) -> Vec<[u8; 32]> {
        let mut people = vec![self.grant.issuer.user_public_key];
        if let Some(inviter) = &self.inviter
            && inviter.user_public_key != self.grant.issuer.user_public_key
        {
            people.push(inviter.user_public_key);
        }
        people
    }

    pub fn signed_invitation(&self) -> &[u8] {
        &self.signed_invitation
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinChallenge {
    pub version: u32,
    pub signed_invitation: Vec<u8>,
    pub workspace_id: WorkspaceId,
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
    pub workspace_id: WorkspaceId,
    /// The device answering: a device of the workspace's owner.
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
        workspace: WorkspaceId,
        host: &DeviceCert,
        device_cert: &DeviceCert,
        request_id: [u8; 32],
        nonce: [u8; 32],
        expires_at_ns: u64,
        now_ns: u64,
    ) -> Result<Self, String> {
        validate_certificate(host, &RevocationList::new(), now_ns)?;
        validate_certificate(device_cert, &RevocationList::new(), now_ns)?;
        if expires_at_ns <= now_ns {
            return Err("membership challenge has invalid expiry".into());
        }
        Ok(Self {
            version: JOIN_VERSION,
            workspace_id: workspace,
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
    pub fn workspace_id(&self) -> WorkspaceId {
        self.challenge.workspace_id
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
    Inspect {
        signed_invitation: Vec<u8>,
    },
    Challenge(JoinChallengeRequest),
    Complete(JoinCompleteRequest),
    MembershipChallenge(MembershipChallengeRequest),
    MembershipComplete(MembershipCompleteRequest),
    ResolveInvitation {
        reference: native::InvitationRef,
        device_cert: DeviceCert,
    },
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
    /// The workspace's governance, as encoded neighbour messages, given once
    /// the invitation verified for the asking device: what the device writes
    /// its own admission on top of.
    pub governance: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinCompleteRequest {
    pub proof: JoinProof,
    /// The device's own admission, as workspace changes it signed: its
    /// redemption of the invitation and its device row. The answering device
    /// takes them in as the joining device's and passes them on unchanged.
    pub admission: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipChallengeRequest {
    pub workspace_id: WorkspaceId,
    pub device_cert: DeviceCert,
    pub request_id: [u8; 32],
    /// This device's statement that the transport key it dials from speaks
    /// for it, when that key is not its own: a page keeps its device key
    /// unextractable and dials from a key of its own, so the host has to be
    /// shown who that key speaks for before it challenges the certificate.
    pub delegation: Option<EndpointDelegation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipChallengeResponse {
    pub challenge: MembershipChallenge,
    /// The workspace's governance, as encoded neighbour messages: what a
    /// further device of a member writes its own row on top of.
    pub governance: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipCompleteRequest {
    pub proof: MembershipProof,
    /// A further device's own row, as workspace changes it signed; empty for
    /// a device the workspace lists already.
    pub admission: Vec<Vec<u8>>,
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
    pub workspace_id: WorkspaceId,
    /// The device that answered: a device of the workspace's owner.
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
    pub workspace_id: WorkspaceId,
    /// The device that answered: a device of the workspace's owner.
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
    ResolvedInvitation { signed_invitation: Vec<u8> },
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

/// [`issue_invitation`] of an invitation that also makes its holder an
/// administrator, naming `inviter` when the issuer signs for someone else:
/// the role must be [`Role::Editor`], the role an administrator holds.
pub fn issue_administrator_invitation(
    grant: InviteGrant,
    inviter: Option<Inviter>,
    key: &dyn collab_types::signer::DeviceSigner,
) -> Result<Vec<u8>, String> {
    validate_grant(&grant, 0, false)?;
    if grant.role != Role::Editor {
        return Err("an administrator joins as an Editor".into());
    }
    if let Some(inviter) = &inviter {
        validate_inviter(inviter)?;
    }
    if key.device_public_key() != grant.issuer.device_public_key {
        return Err("invitation signer is not the declared issuer device".into());
    }
    let digest = tagged_digest(
        INVITATION_DOMAIN,
        &AdministratorInvitationView {
            version: ADMINISTRATOR_INVITATION_VERSION,
            grant: &grant,
            inviter: &inviter,
            administrator: true,
        },
    )?;
    let signed = SignedAdministratorInvitation {
        version: ADMINISTRATOR_INVITATION_VERSION,
        grant,
        inviter,
        administrator: true,
        signature: collab_types::signer::sign_now(key, digest.to_vec())
            .map_err(|error| error.to_string())?,
    };
    postcard::to_stdvec(&signed).map_err(|error| error.to_string())
}

/// Whether `inviter` names who sent an invitation well: a name of at most
/// [`MAX_INVITER_NAME_CHARS`] printable characters, or none at all, for a
/// person whose name its workspace does not know. The name is what a surface
/// shows beside the invitation; who sent it is its
/// [`user_public_key`](Inviter::user_public_key), which every admitting member
/// checks.
fn validate_inviter(inviter: &Inviter) -> Result<(), String> {
    let name = inviter.name.trim();
    if name.is_empty() {
        return if inviter.name.is_empty() {
            Ok(())
        } else {
            Err("invalid inviter name".into())
        };
    }
    if name != inviter.name
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
/// when it puts it back together. An invitation that named an authority, or
/// one that does not decode, shares nothing.
#[must_use]
pub fn invitation_shared_runs(signed_invitation: &[u8]) -> Vec<Vec<u8>> {
    let grant = match postcard::take_from_bytes::<u32>(signed_invitation) {
        Ok((INVITATION_VERSION, _)) => postcard::from_bytes::<SignedInvitation>(signed_invitation)
            .map(|signed| signed.grant)
            .ok(),
        Ok((INVITATION_WITH_INVITER_VERSION, _)) => {
            postcard::from_bytes::<SignedInvitationWithInviter>(signed_invitation)
                .map(|signed| signed.grant)
                .ok()
        }
        Ok((ADMINISTRATOR_INVITATION_VERSION, _)) => {
            postcard::from_bytes::<SignedAdministratorInvitation>(signed_invitation)
                .map(|signed| signed.grant)
                .ok()
        }
        _ => None,
    };
    let Some(grant) = grant else {
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

/// Whether `signed_invitation` is laid out as an administrator's invitation,
/// read without verifying it: for listing what an invitation a host keeps
/// gives. Whoever admits its holder verifies it whole.
#[must_use]
pub fn makes_administrator(signed_invitation: &[u8]) -> bool {
    matches!(
        postcard::take_from_bytes::<u32>(signed_invitation),
        Ok((ADMINISTRATOR_INVITATION_VERSION, _))
    )
}

/// The text an invitation is handed over in: its signed bytes as base64.
pub fn encode_invitation_text(signed_invitation: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signed_invitation)
}

const NATIVE_INVITATION: &str = "tilo://invite#";

/// Carry the signed invitation in a link a Tilo device can open without a site.
#[must_use]
pub fn native_invitation_link(signed: &[u8]) -> String {
    format!("{NATIVE_INVITATION}{}", encode_invitation_text(signed))
}

/// The signed text carried by a native invitation link.
#[must_use]
pub fn native_invitation_text(link: &str) -> Option<&str> {
    link.trim().strip_prefix(NATIVE_INVITATION)
}

/// Recover the signed bytes from an invitation code or native link.
///
/// # Errors
/// The invitation is not base64url text.
pub fn invitation_bytes(text: &str) -> Result<Vec<u8>, String> {
    let text = native_invitation_text(text).unwrap_or(text);
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
    decode_and_verify(
        signed_invitation,
        recipient_user_public_key,
        revocations,
        now_ns,
        true,
    )
}

/// [`decode_and_verify_invitation`] for an invitation whose expiry does not bind its redeemer:
/// a device of the workspace's owner comes in on its certificate, and the invitation it
/// redeemed gives it no role. Its signature, workspace, issuer and certificates are checked
/// as of `now_ns` all the same.
///
/// # Errors
/// As [`decode_and_verify_invitation`], but never for having expired.
pub fn decode_and_verify_invitation_past_expiry(
    signed_invitation: &[u8],
    recipient_user_public_key: Option<[u8; 32]>,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<VerifiedJoin, String> {
    decode_and_verify(
        signed_invitation,
        recipient_user_public_key,
        revocations,
        now_ns,
        false,
    )
}

fn decode_and_verify(
    signed_invitation: &[u8],
    recipient_user_public_key: Option<[u8; 32]>,
    revocations: &RevocationList,
    now_ns: u64,
    enforce_expiry: bool,
) -> Result<VerifiedJoin, String> {
    let (version, _) = postcard::take_from_bytes::<u32>(signed_invitation)
        .map_err(|_| "invalid signed invitation".to_string())?;
    // The version names the layout the issuer signed: each is read and its
    // signature checked over that layout alone.
    let decoded = match version {
        INVITATION_VERSION | INVITATION_WITH_INVITER_VERSION => {
            decode_invitation(signed_invitation, version)?
        }
        ADMINISTRATOR_INVITATION_VERSION => decode_administrator_invitation(signed_invitation)?,
        _ => return Err("unsupported invitation version".into()),
    };
    validate_decoded(
        &decoded.grant,
        recipient_user_public_key,
        revocations,
        now_ns,
        enforce_expiry,
    )?;
    Ok(VerifiedJoin {
        invite_id: *blake3::hash(signed_invitation).as_bytes(),
        grant: decoded.grant,
        inviter: decoded.inviter,
        administrator: decoded.administrator,
        signed_invitation: signed_invitation.to_vec(),
    })
}

/// Decode and check the issuer's signature of an invitation in this build's
/// layout.
fn decode_invitation(signed_invitation: &[u8], version: u32) -> Result<Decoded, String> {
    let malformed = |_| "invalid signed invitation".to_string();
    let (grant, inviter, signature) = if version == INVITATION_WITH_INVITER_VERSION {
        let signed: SignedInvitationWithInviter =
            postcard::from_bytes(signed_invitation).map_err(malformed)?;
        validate_inviter(&signed.inviter)?;
        (signed.grant, Some(signed.inviter), signed.signature)
    } else {
        let signed: SignedInvitation =
            postcard::from_bytes(signed_invitation).map_err(malformed)?;
        (signed.grant, None, signed.signature)
    };
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
    verify_issuer_signature(&grant.issuer, &digest, &signature)?;
    Ok(Decoded {
        grant,
        inviter,
        administrator: false,
    })
}

/// Decode and check the issuer's signature of an administrator's invitation
/// ([`ADMINISTRATOR_INVITATION_VERSION`]).
fn decode_administrator_invitation(signed_invitation: &[u8]) -> Result<Decoded, String> {
    let signed: SignedAdministratorInvitation = postcard::from_bytes(signed_invitation)
        .map_err(|_| "invalid signed invitation".to_string())?;
    if let Some(inviter) = &signed.inviter {
        validate_inviter(inviter)?;
    }
    if !signed.administrator || signed.grant.role != Role::Editor {
        return Err("invalid signed invitation".into());
    }
    let digest = tagged_digest(
        INVITATION_DOMAIN,
        &AdministratorInvitationView {
            version: signed.version,
            grant: &signed.grant,
            inviter: &signed.inviter,
            administrator: signed.administrator,
        },
    )?;
    verify_issuer_signature(&signed.grant.issuer, &digest, &signed.signature)?;
    Ok(Decoded {
        grant: signed.grant,
        inviter: signed.inviter,
        administrator: true,
    })
}

/// Check `issuer` signed `digest`.
fn verify_issuer_signature(
    issuer: &DeviceCert,
    digest: &[u8; 32],
    signature: &[u8; 64],
) -> Result<(), String> {
    // A workspace re-reads every invitation its members redeemed whenever its
    // governance changes: one that verified before is remembered.
    tilo_user_identity::verify_remembered(&issuer.device_public_key, digest, signature, false)
        .map_err(|refused| match refused {
            tilo_user_identity::RememberedRefusal::Key => {
                "invalid invitation issuer device key".to_string()
            }
            tilo_user_identity::RememberedRefusal::Signature => {
                "invitation signature did not verify".to_string()
            }
        })
}

/// Check the device `device_cert` certifies signed the join proof `digest`.
fn verify_device_proof(
    device_cert: &DeviceCert,
    digest: &[u8; 32],
    signature: &[u8; 64],
) -> Result<(), String> {
    ed25519_dalek::VerifyingKey::from_bytes(&device_cert.device_public_key)
        .map_err(|_| "invalid join device public key".to_string())?
        .verify(digest, &ed25519_dalek::Signature::from_bytes(signature))
        .map_err(|_| "join proof signature did not verify".to_string())
}

/// What every invitation is checked for however it was laid out: a canonical
/// grant into a live workspace, a live issuer certificate and transport pins, and
/// the recipient where the redeemer names one.
fn validate_decoded(
    grant: &InviteGrant,
    recipient_user_public_key: Option<[u8; 32]>,
    revocations: &RevocationList,
    now_ns: u64,
    enforce_expiry: bool,
) -> Result<(), String> {
    validate_grant(grant, now_ns, enforce_expiry)?;
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
    if join.grant.workspace_id != proof.challenge.workspace_id {
        return Err("join challenge does not match signed invitation".into());
    }
    verify_device_proof(
        device_cert,
        &tagged_digest(JOIN_PROOF_DOMAIN, &proof.challenge)?,
        &proof.signature,
    )?;
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
/// A record written before a join version bump keeps its Complete bytes:
/// they are what the device signed, and nothing here could sign for it
/// again. Its challenge is therefore checked in the layout its version
/// names, and everything else - the scope the invitation pins, the device
/// the challenge names - the same way. `now_ns` is when the proof was made:
/// a stored proof is not fresh, and the caller says when it was.
///
/// # Errors
/// The bytes are not a Complete of a layout this reads, or its proof does
/// not check.
pub fn verify_stored_complete(
    bytes: &[u8],
    device_cert: &DeviceCert,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<VerifiedRedemption, String> {
    let not_complete = || "stored join proof is not Complete".to_string();
    // Every layout starts with the request's variant, then the challenge's
    // version.
    let (variant, challenge) =
        postcard::take_from_bytes::<u32>(bytes).map_err(|_| not_complete())?;
    if variant != COMPLETE_VARIANT {
        return Err(not_complete());
    }
    let (version, _) = postcard::take_from_bytes::<u32>(challenge).map_err(|_| not_complete())?;
    match version {
        JOIN_VERSION => {}
        _ => return Err("join challenge is expired or unsupported".into()),
    }
    let request =
        postcard::from_bytes::<JoinTransportRequest>(bytes).map_err(|_| not_complete())?;
    if postcard::to_stdvec(&request).map_err(|error| error.to_string())? != bytes {
        return Err("noncanonical stored Complete".into());
    }
    let JoinTransportRequest::Complete(complete) = request else {
        return Err(not_complete());
    };
    let proof = complete.proof;
    let redemption = verify_join_proof(&proof, &proof.challenge, device_cert, revocations, now_ns)?;
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

/// Sign the answer to a membership challenge, as the device's signer answers: a page's key signs
/// through a promise, later than the turn that asked.
///
/// # Errors
///
/// Returns an error if the challenge cannot be encoded for signing, or the signer refused.
pub async fn sign_membership_proof(
    challenge: MembershipChallenge,
    key: &dyn collab_types::signer::DeviceSigner,
) -> Result<MembershipProof, String> {
    if key.device_public_key() != challenge.device_public_key {
        return Err("membership proof signer is not the challenged device".into());
    }
    let digest = tagged_digest(MEMBERSHIP_PROOF_DOMAIN, &challenge)?;
    Ok(MembershipProof {
        challenge,
        signature: key
            .sign(digest.to_vec())
            .await
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

#[cfg(test)]
mod tests;
