//! Read a workspace record a build of this host wrote, whatever version it
//! is in.
//!
//! A record is one postcard struct whose field order is the format, and its
//! join ledger holds the grant of every invitation the workspace issued,
//! beside the invitation it signed. The grant's layout changed once, when it
//! stopped naming the workspace's authority and the delegations that let
//! another device issue for it. The record's version was not bumped with it,
//! so a version 2 record holds either layout, and the invitation's own
//! version — which is in the bytes the ledger keeps — is what says which.
//!
//! Version 3 is the record this build writes, its grants in this build's
//! layout. [`v2_to_v3`] reads a version 2 record field by field and writes it
//! again as version 3, each grant read in the layout its invitation names.
//! Nothing else changed: the peer parts, the publications, the completions
//! and the rest of the ledger read as they are. `Format::bring_up` keeps the
//! file as it was beside it as `<record>.v2`, and the peer's large parts,
//! which the record names by digest, are untouched.

use tilo_files::native::migration::Format;
use tilo_peer_state::join::invitations::IssuedInvitation;
use tilo_peer_state::join::previous;
use tilo_peer_state::join::{InviteGrant, JoinBootstrap, JoinChallenge, MembershipChallenge};
use tilo_user_identity::DeviceCert;

use tilo_peer_state::join_ledger::{
    AdministrationRecord, CancellationRecord, InvitationRecord, JoinChallengeRecord,
    JoinCompletionRecord, JoinLedger, MembershipChallengeRecord,
};

use super::{Publication, RECORD_VERSION, WorkspaceRecord};

/// A record's versions: 2 (either grant layout) and 3 (this build's).
pub(super) const FORMAT: Format = Format {
    name: "governed workspace record",
    version: RECORD_VERSION,
    oldest: 2,
    steps: &[v2_to_v3],
};

/// Bring a version 2 record up to version 3, its grants in this build's
/// layout.
///
/// # Errors
///
/// The bytes are not a version 2 record, or a grant does not read in the
/// layout its invitation names.
#[expect(
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    reason = "a migration step takes its bytes by value, and the record it reads is one long struct"
)]
fn v2_to_v3(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    let mut rest: &[u8] = &bytes;

    macro_rules! field {
        ($ty:ty, $name:literal) => {{
            let (value, tail) = postcard::take_from_bytes::<$ty>(rest)
                .map_err(|error| format!("version 2 record, {}: {error}", $name))?;
            rest = tail;
            value
        }};
    }

    let _version: u32 = field!(u32, "version");
    let workspace: String = field!(String, "workspace");
    let workspace_id: u64 = field!(u64, "workspace_id");
    let name: String = field!(String, "name");
    let authority: DeviceCert = field!(DeviceCert, "authority");
    let peer = field!(Vec<super::peer_parts::PeerPart>, "peer");
    let generation: u64 = field!(u64, "generation");
    let outgoing: Vec<Publication> = field!(Vec<Publication>, "outgoing");
    let published: usize = field!(usize, "published");

    // The ledger, its invitations' grants read in the layout their own
    // invitation names.
    let invitations = {
        let count: usize = field!(usize, "ledger invitations");
        let mut invitations = Vec::with_capacity(count);
        for _ in 0..count {
            let request_id: [u8; 32] = field!([u8; 32], "invitation request_id");
            let request_digest: [u8; 32] = field!([u8; 32], "invitation request_digest");
            let issued: IssuedInvitation = field!(IssuedInvitation, "invitation issued");
            // The grant's layout is the invitation's own version's.
            let (grant, consumed) = take_grant(rest, &issued)?;
            rest = &rest[consumed..];
            let cancellation: Option<CancellationRecord> =
                field!(Option<CancellationRecord>, "invitation cancellation");
            invitations.push(InvitationRecord {
                request_id,
                request_digest,
                issued,
                grant,
                cancellation,
            });
        }
        invitations
    };
    let join_challenges = {
        let count: usize = field!(usize, "ledger join challenges");
        let mut challenges = Vec::with_capacity(count);
        for _ in 0..count {
            let remote_endpoint: [u8; 32] = field!([u8; 32], "challenge remote_endpoint");
            let request_digest: [u8; 32] = field!([u8; 32], "challenge request_digest");
            let device_cert: DeviceCert = field!(DeviceCert, "challenge device_cert");
            let challenge: JoinChallenge = field!(JoinChallenge, "challenge");
            challenges.push(JoinChallengeRecord {
                remote_endpoint,
                request_digest,
                device_cert,
                challenge,
            });
        }
        challenges
    };
    let join_completions = {
        let count: usize = field!(usize, "ledger join completions");
        let mut completions = Vec::with_capacity(count);
        for _ in 0..count {
            let remote_endpoint: [u8; 32] = field!([u8; 32], "completion remote_endpoint");
            let request_id: [u8; 32] = field!([u8; 32], "completion request_id");
            let proof_digest: [u8; 32] = field!([u8; 32], "completion proof_digest");
            let bootstrap: JoinBootstrap = field!(JoinBootstrap, "completion bootstrap");
            completions.push(JoinCompletionRecord {
                remote_endpoint,
                request_id,
                proof_digest,
                bootstrap,
            });
        }
        completions
    };
    let membership_challenges = {
        let count: usize = field!(usize, "ledger membership challenges");
        let mut challenges = Vec::with_capacity(count);
        for _ in 0..count {
            let remote_endpoint: [u8; 32] = field!([u8; 32], "membership remote_endpoint");
            let request_digest: [u8; 32] = field!([u8; 32], "membership request_digest");
            let device_cert: DeviceCert = field!(DeviceCert, "membership device_cert");
            let challenge: MembershipChallenge =
                field!(MembershipChallenge, "membership challenge");
            challenges.push(MembershipChallengeRecord {
                remote_endpoint,
                request_digest,
                device_cert,
                challenge,
            });
        }
        challenges
    };
    let administration = {
        let count: usize = field!(usize, "ledger administration");
        let mut records = Vec::with_capacity(count);
        for _ in 0..count {
            let request_id: [u8; 32] = field!([u8; 32], "administration request_id");
            let request_digest: [u8; 32] = field!([u8; 32], "administration request_digest");
            let generation: u64 = field!(u64, "administration generation");
            records.push(AdministrationRecord {
                request_id,
                request_digest,
                generation,
            });
        }
        records
    };

    postcard::to_stdvec(&WorkspaceRecord {
        version: RECORD_VERSION,
        workspace,
        workspace_id,
        name,
        authority,
        peer,
        generation,
        outgoing,
        published,
        collaboration: JoinLedger {
            invitations,
            join_challenges,
            join_completions,
            membership_challenges,
            administration,
        },
    })
    .map_err(|error| error.to_string())
}

/// The grant version 2 kept beside `issued`, in this build's layout, and how
/// many bytes it took.
fn take_grant(bytes: &[u8], issued: &IssuedInvitation) -> Result<(InviteGrant, usize), String> {
    let (version, _) = postcard::take_from_bytes::<u32>(&issued.signed_invitation)
        .map_err(|error| format!("an issued invitation has no version: {error}"))?;
    if previous::is_invitation_version(version) {
        previous::take_stored_grant(bytes)
    } else {
        let (grant, tail): (InviteGrant, &[u8]) =
            postcard::take_from_bytes(bytes).map_err(|error| error.to_string())?;
        Ok((grant, bytes.len() - tail.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer as _;
    use tilo_peer_state::join::{MemberRole, TransportHint};
    use tilo_user_identity::DeviceCapabilities;

    /// A device certificate, signed by its own key: a governor's, for the
    /// layouts a record names.
    fn cert(seed: u8) -> (ed25519_dalek::SigningKey, DeviceCert) {
        let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let mut cert = DeviceCert {
            user_public_key: key.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: format!("test {seed}"),
            capabilities: DeviceCapabilities::host(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, &key).unwrap();
        (key, cert)
    }

    /// A grant as the layout before the invitation version bump wrote it: it
    /// named the workspace's authority, and the grant it has today does not.
    #[derive(serde::Serialize)]
    struct GrantV1 {
        workspace_id: u64,
        room: String,
        authority: DeviceCert,
        issuer: DeviceCert,
        delegation: Option<()>,
        role: MemberRole,
        nonce: [u8; 16],
        recipient_user_public_key: Option<[u8; 32]>,
        invitee_name: Option<String>,
        expires_at_ns: u64,
        transport: Vec<TransportHint>,
    }

    /// A signed invitation of that layout, whose version names it.
    #[derive(serde::Serialize)]
    struct SignedInvitationV1 {
        version: u32,
        grant: GrantV1,
        signature: Vec<u8>,
    }

    #[derive(serde::Serialize)]
    struct InvitationViewV1<'a> {
        version: u32,
        grant: &'a GrantV1,
    }

    /// The signed invitation a build of the earlier layout wrote, and the
    /// grant it carries as that build's ledger kept it beside it.
    fn earlier_invitation() -> (Vec<u8>, Vec<u8>, InviteGrant) {
        let (key, authority) = cert(1);
        let grant = InviteGrant {
            workspace_id: 7,
            room: "302d59e0-f586-42e1-a59e-3aa613f0f866".into(),
            issuer: authority.clone(),
            role: MemberRole::Editor,
            nonce: [4; 16],
            recipient_user_public_key: Some([5; 32]),
            invitee_name: None,
            expires_at_ns: u64::MAX,
            transport: Vec::<TransportHint>::new(),
        };
        let earlier = GrantV1 {
            workspace_id: grant.workspace_id,
            room: grant.room.clone(),
            authority: authority.clone(),
            issuer: grant.issuer.clone(),
            delegation: None,
            role: grant.role,
            nonce: grant.nonce,
            recipient_user_public_key: grant.recipient_user_public_key,
            invitee_name: grant.invitee_name.clone(),
            expires_at_ns: grant.expires_at_ns,
            transport: grant.transport.clone(),
        };
        let digest = {
            let encoded = postcard::to_stdvec(&InvitationViewV1 {
                version: 1,
                grant: &earlier,
            })
            .unwrap();
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"tilo/workspace-invitation/v1\0");
            hasher.update(&encoded);
            *hasher.finalize().as_bytes()
        };
        let signature = key.sign(&digest).to_bytes();
        let signed = postcard::to_stdvec(&SignedInvitationV1 {
            version: 1,
            grant: earlier,
            signature: signature.to_vec(),
        })
        .unwrap();
        let ledger_copy = postcard::to_stdvec(&GrantV1 {
            workspace_id: grant.workspace_id,
            room: grant.room.clone(),
            authority,
            issuer: grant.issuer.clone(),
            delegation: None,
            role: grant.role,
            nonce: grant.nonce,
            recipient_user_public_key: grant.recipient_user_public_key,
            invitee_name: grant.invitee_name.clone(),
            expires_at_ns: grant.expires_at_ns,
            transport: grant.transport.clone(),
        })
        .unwrap();
        (signed, ledger_copy, grant)
    }

    /// A grant stored in the earlier layout reads back as this build's, and
    /// takes exactly its own bytes, so the record after it still reads.
    #[test]
    fn a_grant_stored_in_the_earlier_layout_reads_back() {
        let (signed, ledger_copy, grant) = earlier_invitation();
        let issued = IssuedInvitation {
            invite_id: *blake3::hash(&signed).as_bytes(),
            signed_invitation: signed,
        };
        let mut record = ledger_copy.clone();
        record.extend_from_slice(b"after");
        let (read, taken) = take_grant(&record, &issued).unwrap();
        assert_eq!(read, grant);
        assert_eq!(&record[taken..], b"after");
    }

    /// A whole version 2 record, its ledger holding one invitation of the
    /// earlier layout, comes up as version 3 with that invitation's grant
    /// read in the layout its own version names.
    #[test]
    fn a_version_two_record_comes_up_with_its_invitations_grant() {
        let (signed, ledger_copy, grant) = earlier_invitation();
        let issued = IssuedInvitation {
            invite_id: *blake3::hash(&signed).as_bytes(),
            signed_invitation: signed,
        };
        // Version 2's record, byte for byte: the record's own struct, its
        // invitation's grant in the earlier layout.
        let mut invitation = postcard::to_stdvec(&[9_u8; 32]).unwrap();
        invitation.extend(postcard::to_stdvec(&[8_u8; 32]).unwrap());
        invitation.extend(postcard::to_stdvec(&issued).unwrap());
        invitation.extend(ledger_copy);
        invitation.extend(postcard::to_stdvec(&None::<CancellationRecord>).unwrap());
        let record = version_two(&grant, &invitation);
        let migrated = FORMAT.migrate(record, 2).unwrap();
        let record: WorkspaceRecord = postcard::from_bytes(&migrated).unwrap();
        assert_eq!(record.version, RECORD_VERSION);
        assert_eq!(record.collaboration.invitations.len(), 1);
        assert_eq!(record.collaboration.invitations[0].grant, grant);
    }

    /// A version 2 record whose ledger holds `invitation`, built field by
    /// field as version 2 wrote it.
    fn version_two(grant: &InviteGrant, invitation: &[u8]) -> Vec<u8> {
        let mut bytes = postcard::to_stdvec(&2_u32).unwrap();
        bytes.extend(postcard::to_stdvec(&grant.room).unwrap());
        bytes.extend(postcard::to_stdvec(&grant.workspace_id).unwrap());
        bytes.extend(postcard::to_stdvec("Lounge").unwrap());
        let (_, authority) = cert(1);
        bytes.extend(postcard::to_stdvec(&authority).unwrap());
        bytes.extend(
            postcard::to_stdvec(&Vec::<super::super::peer_parts::PeerPart>::new()).unwrap(),
        );
        bytes.extend(postcard::to_stdvec(&1_u64).unwrap());
        bytes.extend(postcard::to_stdvec(&Vec::<Publication>::new()).unwrap());
        bytes.extend(postcard::to_stdvec(&0_usize).unwrap());
        // The ledger: one invitation, nothing else.
        bytes.extend(postcard::to_stdvec(&1_usize).unwrap());
        bytes.extend(invitation);
        for _ in 0..4 {
            bytes.extend(postcard::to_stdvec(&0_usize).unwrap());
        }
        bytes
    }

    /// A grant in this build's layout reads as it was written.
    #[test]
    fn a_grant_stored_in_this_layout_reads_back() {
        let (_, _, grant) = earlier_invitation();
        let (key, _) = cert(1);
        let signed = tilo_peer_state::join::issue_invitation(grant.clone(), &key).unwrap();
        let issued = IssuedInvitation {
            invite_id: *blake3::hash(&signed).as_bytes(),
            signed_invitation: signed,
        };
        let mut record = postcard::to_stdvec(&grant).unwrap();
        record.extend_from_slice(b"after");
        let (read, taken) = take_grant(&record, &issued).unwrap();
        assert_eq!(read, grant);
        assert_eq!(&record[taken..], b"after");
    }
}
