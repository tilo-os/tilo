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
//! Version 3 holds the grants in this build's layout. [`v2_to_v3`] reads a
//! version 2 record field by field and writes it again as version 3, each
//! grant read in the layout its invitation names. Nothing else changed: the
//! peer parts, the publications, the completions and the rest of the ledger
//! read as they are. `Format::bring_up` keeps the file as it was beside it as
//! `<record>.v2`, and the peer's large parts, which the record names by
//! digest, are untouched.
//!
//! Version 4 is the record this build writes ([`StoredRecord`]). A version 3
//! record held every publication since the workspace's base, the base its
//! whole governance, and every join receipt the workspace ever made, and was
//! written whole with every change, so each admission cost a write of the
//! whole workspace. Version 4 holds only the publications and receipts not in
//! the files beside it yet ([`super::appended`]), and how many each file
//! holds. [`v3_to_v4`] keeps every one in the record and none in the files;
//! the first write moves them out.

use serde::{Deserialize, Serialize};
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

/// A record's versions: 2 (either grant layout), 3 (this build's grants, every
/// publication and receipt inline) and 4 (this build's).
pub(super) const FORMAT: Format = Format {
    name: "governed workspace record",
    version: RECORD_VERSION,
    oldest: 2,
    steps: &[v2_to_v3, v3_to_v4],
};

/// The version 3 layout: [`WorkspaceRecord`] as it is held.
const V3: u32 = 3;

/// A workspace record as this build writes it (version 4): the record held
/// ([`WorkspaceRecord`], the version 3 layout) with only the publications and
/// join receipts not in the files beside it yet, and how many those hold.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct StoredRecord {
    pub(super) version: u32,
    pub(super) workspace: String,
    pub(super) workspace_id: u64,
    pub(super) name: String,
    pub(super) authority: DeviceCert,
    pub(super) peer: Vec<super::peer_parts::PeerPart>,
    pub(super) generation: u64,
    /// The publications after the ones the publications file holds.
    pub(super) outgoing: Vec<Publication>,
    pub(super) published: usize,
    /// The ledger, its join receipts those after the receipts file's.
    pub(super) collaboration: JoinLedger,
    /// How many publications the publications file holds, the first of the
    /// workspace's.
    pub(super) filed_publications: u64,
    /// How many join receipts the receipts file holds, the first of the
    /// ledger's.
    pub(super) filed_receipts: u64,
}

/// [`StoredRecord`] borrowed from the record held, as it is written.
#[derive(Serialize)]
pub(super) struct StoredRecordRef<'a> {
    pub(super) version: u32,
    pub(super) workspace: &'a str,
    pub(super) workspace_id: u64,
    pub(super) name: &'a str,
    pub(super) authority: &'a DeviceCert,
    pub(super) peer: &'a [super::peer_parts::PeerPart],
    pub(super) generation: u64,
    pub(super) outgoing: &'a [Publication],
    pub(super) published: usize,
    pub(super) collaboration: LedgerRef<'a>,
    pub(super) filed_publications: u64,
    pub(super) filed_receipts: u64,
}

/// The join ledger as a written record holds it, borrowed: its receipts
/// those the receipts file does not hold. The ledger's own layout.
#[derive(Serialize)]
pub(super) struct LedgerRef<'a> {
    pub(super) invitations: &'a [InvitationRecord],
    pub(super) join_challenges: &'a [JoinChallengeRecord],
    pub(super) join_completions: &'a [JoinCompletionRecord],
    pub(super) membership_challenges: &'a [MembershipChallengeRecord],
    pub(super) administration: &'a [AdministrationRecord],
}

/// Bring a version 3 record up to version 4: every publication and receipt
/// in the record, none in the files beside it.
///
/// # Errors
///
/// The bytes are not a version 3 record.
#[expect(
    clippy::needless_pass_by_value,
    reason = "a migration step takes its bytes by value"
)]
fn v3_to_v4(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    let record: WorkspaceRecord =
        postcard::from_bytes(&bytes).map_err(|error| format!("version 3 record: {error}"))?;
    postcard::to_stdvec(&StoredRecord {
        version: RECORD_VERSION,
        workspace: record.workspace,
        workspace_id: record.workspace_id,
        name: record.name,
        authority: record.authority,
        peer: record.peer,
        generation: record.generation,
        outgoing: record.outgoing,
        published: record.published,
        collaboration: record.collaboration,
        filed_publications: 0,
        filed_receipts: 0,
    })
    .map_err(|error| error.to_string())
}

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
            let (grant, tail) = take_grant(rest, &issued)?;
            rest = tail;
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
        version: V3,
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

/// The grant version 2 kept beside `issued`, in this build's layout, and the
/// bytes after it.
fn take_grant<'a>(
    bytes: &'a [u8],
    issued: &IssuedInvitation,
) -> Result<(InviteGrant, &'a [u8]), String> {
    let (version, _) = postcard::take_from_bytes::<u32>(&issued.signed_invitation)
        .map_err(|error| format!("an issued invitation has no version: {error}"))?;
    if previous::is_invitation_version(version) {
        let (grant, taken) = previous::take_stored_grant(bytes)?;
        let tail = bytes
            .get(taken..)
            .ok_or_else(|| "a stored grant took more than its record holds".to_string())?;
        Ok((grant, tail))
    } else {
        postcard::take_from_bytes(bytes).map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer as _;
    use tilo_files::native::migration::assert_golden_bytes;
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
            issuer: None,
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
        let grant = kept_grant();
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
        let (signed, mut record, grant) = earlier_invitation();
        let issued = IssuedInvitation {
            invite_id: *blake3::hash(&signed).as_bytes(),
            signed_invitation: signed,
        };
        record.extend_from_slice(b"after");
        let (read, tail) = take_grant(&record, &issued).unwrap();
        assert_eq!(read, grant);
        assert_eq!(tail, b"after");
    }

    /// A governed workspace record as version 4 writes it, hex: the kept
    /// version 3 record brought up, its publication and ledger in the record
    /// and nothing in the files beside it.
    const KEPT_V4: &str = "042433303264353965302d663538362d343265312d613539652d336161363133663066383636d4c5c5d4dc84898a67064c6f756e67658a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c0674657374203100000101010040d457bd4c013a0b2ed06ec03e15c09b0d857c8ce6fe04c9e0ebeddde7610ed1b83921aea55f5912c14ac15794a8e3d20522d3a159c9f89004139f65647efddb04010002856f01010301020301000109090909090909090909090909090909090909090909090909090909090909090808080808080808080808080808080808080808080808080808080808080808d39cd19405d7a20469cc26de0e5bf849101780a79edefb936648c5d6066a972db40203072433303264353965302d663538362d343265312d613539652d3361613631336630663836368a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c0674657374203100000101010040d457bd4c013a0b2ed06ec03e15c09b0d857c8ce6fe04c9e0ebeddde7610ed1b83921aea55f5912c14ac15794a8e3d20522d3a159c9f89004139f65647efddb04020404040404040404040404040404040401050505050505050505050505050505050505050505050505050505050505050500ffffffffffffffffff0100404039d46ce13e59087832db34e2e3bc347cc8125f421cdc2d33f106ad80076547764ba067929fba9aa1b3da9e8d0b765a753ff85d6008b4e0e89eef786c8ffa00072433303264353965302d663538362d343265312d613539652d3361613631336630663836368a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c0674657374203100000101010040d457bd4c013a0b2ed06ec03e15c09b0d857c8ce6fe04c9e0ebeddde7610ed1b83921aea55f5912c14ac15794a8e3d20522d3a159c9f89004139f65647efddb04020404040404040404040404040404040401050505050505050505050505050505050505050505050505050505050505050500ffffffffffffffffff010000000000000000";

    /// The workspace the kept records are of.
    const KEPT_WORKSPACE: &str = "302d59e0-f586-42e1-a59e-3aa613f0f866";

    /// A governed workspace record as version 3 encoded it, hex, kept so the
    /// layout cannot change under the version devices hold. It is
    /// [`kept_version_three`] as `postcard` wrote it.
    const KEPT_V3: &str = "032433303264353965302d663538362d343265312d613539652d336161363133663066383636d4c5c5d4dc84898a67064c6f756e67658a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c0674657374203100000101010040d457bd4c013a0b2ed06ec03e15c09b0d857c8ce6fe04c9e0ebeddde7610ed1b83921aea55f5912c14ac15794a8e3d20522d3a159c9f89004139f65647efddb04010002856f01010301020301000109090909090909090909090909090909090909090909090909090909090909090808080808080808080808080808080808080808080808080808080808080808d39cd19405d7a20469cc26de0e5bf849101780a79edefb936648c5d6066a972db40203072433303264353965302d663538362d343265312d613539652d3361613631336630663836368a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c0674657374203100000101010040d457bd4c013a0b2ed06ec03e15c09b0d857c8ce6fe04c9e0ebeddde7610ed1b83921aea55f5912c14ac15794a8e3d20522d3a159c9f89004139f65647efddb04020404040404040404040404040404040401050505050505050505050505050505050505050505050505050505050505050500ffffffffffffffffff0100404039d46ce13e59087832db34e2e3bc347cc8125f421cdc2d33f106ad80076547764ba067929fba9aa1b3da9e8d0b765a753ff85d6008b4e0e89eef786c8ffa00072433303264353965302d663538362d343265312d613539652d3361613631336630663836368a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c0674657374203100000101010040d457bd4c013a0b2ed06ec03e15c09b0d857c8ce6fe04c9e0ebeddde7610ed1b83921aea55f5912c14ac15794a8e3d20522d3a159c9f89004139f65647efddb04020404040404040404040404040404040401050505050505050505050505050505050505050505050505050505050505050500ffffffffffffffffff01000000000000";

    /// A governed workspace record as version 2 encoded it, hex: the same
    /// workspace, its invitation's grant in the layout of that day.
    const KEPT_V2: &str = "022433303264353965302d663538362d343265312d613539652d33616136313366306638363607064c6f756e67658a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c0674657374203100000101010040d457bd4c013a0b2ed06ec03e15c09b0d857c8ce6fe04c9e0ebeddde7610ed1b83921aea55f5912c14ac15794a8e3d20522d3a159c9f89004139f65647efddb04000100000109090909090909090909090909090909090909090909090909090909090909090808080808080808080808080808080808080808080808080808080808080808b6a4f814a2b32c586c506fc11bc2407464587041aa365c02924e9eedd1bde599c30301072433303264353965302d663538362d343265312d613539652d3361613631336630663836368a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c0674657374203100000101010040d457bd4c013a0b2ed06ec03e15c09b0d857c8ce6fe04c9e0ebeddde7610ed1b83921aea55f5912c14ac15794a8e3d20522d3a159c9f89004139f65647efddb048a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c0674657374203100000101010040d457bd4c013a0b2ed06ec03e15c09b0d857c8ce6fe04c9e0ebeddde7610ed1b83921aea55f5912c14ac15794a8e3d20522d3a159c9f89004139f65647efddb0400020404040404040404040404040404040401050505050505050505050505050505050505050505050505050505050505050500ffffffffffffffffff0100405f9dbd4344624e9ff036f80477b9fa2786a5ded38dbd5241023634da2bf3e1179e6f814de9e25e1b3eeb534997dc16dd8c3ba4eeb188c4086335ceda85128905072433303264353965302d663538362d343265312d613539652d3361613631336630663836368a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c0674657374203100000101010040d457bd4c013a0b2ed06ec03e15c09b0d857c8ce6fe04c9e0ebeddde7610ed1b83921aea55f5912c14ac15794a8e3d20522d3a159c9f89004139f65647efddb048a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c0674657374203100000101010040d457bd4c013a0b2ed06ec03e15c09b0d857c8ce6fe04c9e0ebeddde7610ed1b83921aea55f5912c14ac15794a8e3d20522d3a159c9f89004139f65647efddb0400020404040404040404040404040404040401050505050505050505050505050505050505050505050505050505050505050500ffffffffffffffffff01000000000000";

    /// Bytes kept as hex, as a test reads them.
    fn kept(hex: &str) -> Vec<u8> {
        hex::decode(hex).unwrap()
    }

    /// The grant the kept records hold, in this build's layout.
    fn kept_grant() -> InviteGrant {
        InviteGrant {
            workspace_id: 7,
            room: KEPT_WORKSPACE.into(),
            issuer: cert(1).1,
            role: MemberRole::Editor,
            nonce: [4; 16],
            recipient_user_public_key: Some([5; 32]),
            invitee_name: None,
            expires_at_ns: u64::MAX,
            transport: Vec::<TransportHint>::new(),
        }
    }

    /// The record version 3 wrote for the kept workspace: one invitation in
    /// its ledger, one publication out, its peer state inline.
    fn kept_version_three() -> WorkspaceRecord {
        let (key, authority) = cert(1);
        let grant = kept_grant();
        let signed = tilo_peer_state::join::issue_invitation(grant.clone(), &key).unwrap();
        WorkspaceRecord {
            version: V3,
            workspace: KEPT_WORKSPACE.into(),
            workspace_id: crate::room_workspace_id(KEPT_WORKSPACE).unwrap(),
            name: "Lounge".into(),
            authority,
            peer: vec![super::super::peer_parts::PeerPart::Inline(vec![0x85, 0x6f])],
            generation: 1,
            outgoing: vec![Publication {
                bytes: vec![1, 2, 3].into(),
                checkpoint: true,
            }],
            published: 0,
            collaboration: JoinLedger {
                invitations: vec![InvitationRecord {
                    request_id: [9; 32],
                    request_digest: [8; 32],
                    issued: IssuedInvitation {
                        invite_id: *blake3::hash(&signed).as_bytes(),
                        signed_invitation: signed,
                    },
                    grant,
                    cancellation: None,
                }],
                join_challenges: Vec::new(),
                join_completions: Vec::new(),
                membership_challenges: Vec::new(),
                administration: Vec::new(),
            },
        }
    }

    /// The record version 2 wrote for the kept workspace, byte for byte, its
    /// invitation's grant in the layout of that day.
    fn kept_version_two() -> Vec<u8> {
        let (signed, ledger_copy, grant) = earlier_invitation();
        let issued = IssuedInvitation {
            invite_id: *blake3::hash(&signed).as_bytes(),
            signed_invitation: signed,
        };
        let mut invitation = postcard::to_stdvec(&[9_u8; 32]).unwrap();
        invitation.extend(postcard::to_stdvec(&[8_u8; 32]).unwrap());
        invitation.extend(postcard::to_stdvec(&issued).unwrap());
        invitation.extend(ledger_copy);
        invitation.extend(postcard::to_stdvec(&None::<CancellationRecord>).unwrap());
        version_two(&grant, &invitation)
    }

    /// The record a build of version 2 wrote comes up with that invitation's
    /// grant read in the layout its own version names, and the bytes kept
    /// here are that build's.
    #[test]
    fn the_version_two_record_kept_here_comes_up_with_its_invitations_grant() {
        assert_golden_bytes(
            "a governed workspace record, version 2",
            &kept(KEPT_V2),
            |bytes| {
                let migrated = FORMAT.migrate(bytes.to_vec(), 2).unwrap();
                let record: StoredRecord = postcard::from_bytes(&migrated).unwrap();
                assert_eq!(record.version, RECORD_VERSION);
                assert_eq!(record.collaboration.invitations.len(), 1);
                assert_eq!(record.collaboration.invitations[0].grant, kept_grant());
            },
            kept_version_two,
        );
    }

    /// The version 3 layout, which the step to version 4 reads, reads back
    /// the way the bytes kept beside it do.
    #[test]
    fn the_version_three_record_kept_here_reads_and_writes_as_it_is() {
        assert_golden_bytes(
            "a governed workspace record, version 3",
            &kept(KEPT_V3),
            |bytes| {
                let record: WorkspaceRecord = postcard::from_bytes(bytes).unwrap();
                assert_eq!(record.version, V3);
                assert_eq!(record.workspace, KEPT_WORKSPACE);
                assert_eq!(record.name, "Lounge");
                assert_eq!(record.collaboration.invitations[0].grant, kept_grant());
            },
            || postcard::to_stdvec(&kept_version_three()).unwrap(),
        );
    }

    /// A version 3 record comes up as version 4 with its publications and
    /// receipts in the record, the files beside it holding none.
    #[test]
    fn the_version_three_record_kept_here_comes_up_as_version_four() {
        tilo_files::native::migration::assert_step_reads_previous(
            &FORMAT,
            &kept(KEPT_V3),
            |migrated| assert_eq!(migrated, kept(KEPT_V4)),
        );
    }

    /// The record this build writes reads back the way the bytes kept beside
    /// it do: a field added, removed or reordered anywhere in the record,
    /// its ledger or the peer state they hold changes these bytes, and a
    /// workspace a device already holds would no longer open.
    #[test]
    fn the_version_four_record_kept_here_reads_and_writes_as_it_is() {
        assert_golden_bytes(
            "a governed workspace record, version 4",
            &kept(KEPT_V4),
            |bytes| {
                let record: StoredRecord = postcard::from_bytes(bytes).unwrap();
                assert_eq!(record.version, RECORD_VERSION);
                assert_eq!(record.workspace, KEPT_WORKSPACE);
                assert_eq!(record.collaboration.invitations[0].grant, kept_grant());
                assert_eq!((record.filed_publications, record.filed_receipts), (0, 0));
                assert_eq!(&*record.outgoing[0].bytes, &[1, 2, 3]);
            },
            || {
                let kept = kept_version_three();
                let ledger = &kept.collaboration;
                postcard::to_stdvec(&StoredRecordRef {
                    version: RECORD_VERSION,
                    workspace: &kept.workspace,
                    workspace_id: kept.workspace_id,
                    name: &kept.name,
                    authority: &kept.authority,
                    peer: &kept.peer,
                    generation: kept.generation,
                    outgoing: &kept.outgoing,
                    published: kept.published,
                    collaboration: LedgerRef {
                        invitations: &ledger.invitations,
                        join_challenges: &ledger.join_challenges,
                        join_completions: &ledger.join_completions,
                        membership_challenges: &ledger.membership_challenges,
                        administration: &ledger.administration,
                    },
                    filed_publications: 0,
                    filed_receipts: 0,
                })
                .unwrap()
            },
        );
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
        let (read, tail) = take_grant(&record, &issued).unwrap();
        assert_eq!(read, grant);
        assert_eq!(tail, b"after");
    }
}
