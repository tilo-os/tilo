use super::*;
use std::sync::Arc;
use tilo_files::native::migration::{assert_golden_bytes, assert_step_reads_previous};
use tilo_peer_state::join::invitations::IssuedInvitation;
use tilo_peer_state::join::{
    InviteGrant, JoinBootstrap, JoinChallenge, MembershipChallenge, Role, TransportHint,
};
use tilo_peer_state::join_ledger::CancellationRecord;
use tilo_user_identity::DeviceCapabilities;

/// The Lounge's record as the live relay's release, v0.1.5, wrote it: its
/// relay and its own test harness kept a Lounge (an owner, a moderator, two
/// people who came in by invitation, one invitation left open), and this is
/// the file it left (`tools/relay-upgrade`).
/// (Kept as hex text: two lowercase digits per byte, 16 bytes per line.)
static RELEASED_V3: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
    include_str!("../../../golden/governed-workspace-record/v3.hex")
        .split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).expect("a hex byte"))
        .collect()
});

/// The Lounge the release kept.
const RELEASED_WORKSPACE: &str = "abee87a1-91ca-46b7-9233-8ab47f9ec3e7";

/// The invitation its owner left open.
const RELEASED_OPEN_INVITATION: &str =
    "c1c38450577ca739ef152e72b7300099e32ef135bf39919bbde5b16a19c2d071";

/// The version 3 layout reads the release's record and writes it back byte
/// for byte: a type the layout of that day shares with this build changing
/// its encoding breaks this, where the step would otherwise read a released
/// record wrong.
#[test]
fn the_record_the_release_kept_reads_and_writes_as_it_is() {
    assert_golden_bytes(
        "a governed workspace record, version 3 (v0.1.5)",
        RELEASED_V3.as_slice(),
        |bytes| {
            let record: v3::Record = postcard::from_bytes(bytes).unwrap();
            assert_eq!(record.version, 3);
            assert_eq!(record.workspace, RELEASED_WORKSPACE);
            assert_eq!(record.name, "Lounge");
        },
        || {
            let record: v3::Record = postcard::from_bytes(RELEASED_V3.as_slice()).unwrap();
            postcard::to_stdvec(&record).unwrap()
        },
    );
}

/// The release's record comes up as version 4 with everything in it: the
/// same workspace named by its UUID throughout, every invitation (the open
/// one too), every admission's receipt and the peer state as it was.
#[test]
fn the_record_the_release_kept_comes_up_as_version_four() {
    let released: v3::Record = postcard::from_bytes(RELEASED_V3.as_slice()).unwrap();
    let ledger = &released.collaboration;
    let counts = (
        ledger.invitations.len(),
        ledger.join_completions.len(),
        ledger.administration.len(),
    );
    let (peer, outgoing, generation) = (
        released.peer.clone(),
        released.outgoing.clone(),
        released.generation,
    );
    assert_step_reads_previous(&FORMAT, RELEASED_V3.as_slice(), |migrated| {
        let record: StoredRecord = postcard::from_bytes(migrated).unwrap();
        let lounge = WorkspaceId::parse(RELEASED_WORKSPACE).unwrap();
        assert_eq!(record.version, RECORD_VERSION);
        assert_eq!(record.workspace, RELEASED_WORKSPACE);
        assert_eq!(record.workspace_id, lounge);
        assert_eq!(record.name, "Lounge");
        assert_eq!(record.peer, peer);
        assert_eq!(record.outgoing, outgoing);
        assert_eq!(record.generation, generation);
        assert_eq!((record.filed_publications, record.filed_receipts), (0, 0));
        let ledger = &record.collaboration;
        assert_eq!(
            (
                ledger.invitations.len(),
                ledger.join_completions.len(),
                ledger.administration.len(),
            ),
            counts
        );
        // Challenges no device can answer any more go; it asks again.
        assert!(ledger.join_challenges.is_empty() && ledger.membership_challenges.is_empty());
        assert!(
            ledger
                .invitations
                .iter()
                .any(|invitation| hex::encode(invitation.issued.invite_id)
                    == RELEASED_OPEN_INVITATION
                    && invitation.cancellation.is_none()),
            "the invitation the release left open"
        );
        assert!(
            ledger
                .invitations
                .iter()
                .all(|invitation| invitation.grant.workspace_id == lounge)
        );
        assert!(
            ledger
                .join_completions
                .iter()
                .all(|completion| completion.bootstrap.workspace_id == lounge
                    && completion.bootstrap.governance_messages.is_empty())
        );
    });
}

/// Every grant the Lounge the release kept held is carried: as a grant, or,
/// for the grants that release gave the apps themselves, named with what
/// gives their use now (an app acts as the person who runs it). Read from
/// the real record, so a grant the release kept that this build neither
/// holds nor explains fails here.
#[test]
fn every_grant_the_release_kept_is_held_or_needs_none() {
    use tilo_peer_state::members::earlier::Why;
    let migrated = FORMAT.migrate(RELEASED_V3.clone(), 3).unwrap();
    let record: StoredRecord = postcard::from_bytes(&migrated).unwrap();
    let bytes: Vec<u8> = record
        .peer
        .iter()
        .flat_map(|part| match part {
            super::super::peer_parts::PeerPart::Inline(bytes) => bytes.clone(),
            stored @ super::super::peer_parts::PeerPart::Stored { .. } => {
                panic!("the kept record holds its peer state inline, not {stored:?}")
            }
        })
        .collect();
    let peer = tilo_peer_state::StatePeer::restore(
        &bytes,
        &record.workspace,
        record.authority.clone(),
        vec![record.authority.clone()],
        tilo_user_identity::RevocationList::new(),
        tilo_clock::wall(),
    )
    .unwrap();
    let earlier = peer
        .earlier_membership()
        .expect("the release kept its membership in the earlier layout");
    for left in &earlier.left_out {
        assert!(
            left.why.carried(),
            "the release's grant {} is lost: {}",
            left.grant,
            left.why
        );
    }
    let named = |wanted: &Why| earlier.left_out.iter().any(|left| &left.why == wanted);
    for wanted in [Why::SessionApp, Why::FilesApp, Why::DevicePanel] {
        assert!(named(&wanted), "{wanted:?} among {:?}", earlier.left_out);
    }
    for app in ["so.tilo.chat", "so.tilo.notes", "so.tilo.file-explorer"] {
        assert!(
            named(&Why::AppAuthor { app: app.into() }),
            "{app}'s authorship among {:?}",
            earlier.left_out
        );
    }
}

/// A device certificate as the kept record holds it: its fields fixed, its
/// signature none, which nothing here checks.
fn kept_cert(seed: u8) -> DeviceCert {
    DeviceCert {
        user_public_key: [seed; 32],
        device_public_key: [seed.wrapping_add(1); 32],
        device_label: format!("device {seed}"),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [seed; 64],
        issuer: None,
    }
}

/// The workspace the kept record is of.
fn kept_workspace() -> WorkspaceId {
    WorkspaceId::parse("302d59e0-f586-42e1-a59e-3aa613f0f866").unwrap()
}

/// A ledger as this build keeps it, one of each thing it holds.
fn kept_ledger() -> JoinLedger {
    let workspace = kept_workspace();
    let issuer = kept_cert(1);
    let joiner = kept_cert(2);
    let invitations = [InvitationRecord {
        request_id: [9; 32],
        request_digest: [8; 32],
        issued: IssuedInvitation {
            invite_id: [7; 32],
            signed_invitation: vec![3, 1, 4],
        },
        grant: InviteGrant {
            workspace_id: workspace,
            issuer: issuer.clone(),
            role: Role::Editor,
            nonce: [4; 16],
            recipient_user_public_key: Some([5; 32]),
            invitee_name: Some("Ana".into()),
            expires_at_ns: u64::MAX,
            transport: vec![TransportHint {
                kind: "relay".into(),
                opaque_json: "{}".into(),
                host: issuer.clone(),
                service_public_key: [6; 32],
                host_signature: [6; 64],
            }],
        },
        cancellation: Some(CancellationRecord {
            request_id: [10; 32],
            request_digest: [11; 32],
        }),
    }];
    let join_challenges = [JoinChallengeRecord {
        remote_endpoint: [12; 32],
        request_digest: [13; 32],
        device_cert: joiner.clone(),
        challenge: JoinChallenge {
            version: 1,
            signed_invitation: vec![3, 1, 4],
            workspace_id: workspace,
            user_public_key: joiner.user_public_key,
            device_public_key: joiner.device_public_key,
            request_id: [14; 32],
            nonce: [15; 32],
            expires_at_ns: 99,
        },
    }];
    let join_completions = [JoinCompletionRecord {
        remote_endpoint: [12; 32],
        request_id: [14; 32],
        proof_digest: [16; 32],
        bootstrap: JoinBootstrap {
            version: 1,
            invite_id: [7; 32],
            device_cert: joiner.clone(),
            role: Role::Editor,
            workspace_id: workspace,
            host: issuer.clone(),
            certified_roster: Vec::new(),
            revocations: Vec::new(),
            records: Vec::new(),
            governance_messages: Vec::new(),
            generation: 3,
        },
    }];
    let membership_challenges = [MembershipChallengeRecord {
        remote_endpoint: [17; 32],
        request_digest: [18; 32],
        device_cert: joiner.clone(),
        challenge: MembershipChallenge {
            version: 1,
            workspace_id: workspace,
            host_device_public_key: issuer.device_public_key,
            user_public_key: joiner.user_public_key,
            device_public_key: joiner.device_public_key,
            request_id: [19; 32],
            nonce: [20; 32],
            expires_at_ns: 99,
        },
    }];
    let administration = [AdministrationRecord {
        request_id: [21; 32],
        request_digest: [22; 32],
        generation: 2,
    }];
    JoinLedger {
        invitations: invitations.into(),
        join_challenges: join_challenges.into(),
        join_completions: join_completions.into(),
        membership_challenges: membership_challenges.into(),
        administration: administration.into(),
    }
}

/// A record as this build writes it, one of each thing its ledger holds.
fn kept_version_four() -> Vec<u8> {
    let workspace = kept_workspace();
    let issuer = kept_cert(1);
    let ledger = kept_ledger();
    let peer = [
        super::super::peer_parts::PeerPart::Inline(vec![0x85, 0x6f]),
        super::super::peer_parts::PeerPart::Stored {
            digest: [23; 32],
            len: 5,
        },
    ];
    let outgoing = [Publication {
        bytes: vec![1, 2, 3].into(),
        checkpoint: true,
    }];
    postcard::to_stdvec(&StoredRecordRef {
        version: RECORD_VERSION,
        workspace: &workspace.to_string(),
        workspace_id: workspace,
        name: "Lounge",
        authority: &issuer,
        peer: &peer,
        generation: 3,
        outgoing: &outgoing,
        published: 1,
        collaboration: LedgerRef {
            invitations: &ledger.invitations,
            join_challenges: &ledger.join_challenges,
            join_completions: &ledger.join_completions,
            membership_challenges: &ledger.membership_challenges,
            administration: &ledger.administration,
        },
        filed_publications: 4,
        filed_receipts: 5,
    })
    .unwrap()
}

/// [`kept_version_four`] as this build wrote it, hex.
const KEPT_V4: &str = "042433303264353965302d663538362d343265312d613539652d33616136313366306638363610302d59e0f58642e1a59e3aa613f0f866064c6f756e6765010101010101010101010101010101010101010101010101010101010101010102020202020202020202020202020202020202020202020202020202020202020864657669636520310100000001004001010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101020002856f011717171717171717171717171717171717171717171717171717171717171717050301030102030101010909090909090909090909090909090909090909090909090909090909090909080808080808080808080808080808080808080808080808080808080808080807070707070707070707070707070707070707070707070707070707070707070303010410302d59e0f58642e1a59e3aa613f0f86601010101010101010101010101010101010101010101010101010101010101010202020202020202020202020202020202020202020202020202020202020202086465766963652031010000000100400101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010102040404040404040404040404040404040105050505050505050505050505050505050505050505050505050505050505050103416e61ffffffffffffffffff01010572656c6179027b7d01010101010101010101010101010101010101010101010101010101010101010202020202020202020202020202020202020202020202020202020202020202086465766963652031010000000100400101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010106060606060606060606060606060606060606060606060606060606060606064006060606060606060606060606060606060606060606060606060606060606060606060606060606060606060606060606060606060606060606060606060606010a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b010c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d020202020202020202020202020202020202020202020202020202020202020203030303030303030303030303030303030303030303030303030303030303030864657669636520320100000001004002020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202010303010410302d59e0f58642e1a59e3aa613f0f866020202020202020202020202020202020202020202020202020202020202020203030303030303030303030303030303030303030303030303030303030303030e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f63010c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e10101010101010101010101010101010101010101010101010101010101010100107070707070707070707070707070707070707070707070707070707070707070202020202020202020202020202020202020202020202020202020202020202030303030303030303030303030303030303030303030303030303030303030308646576696365203201000000010040020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020210302d59e0f58642e1a59e3aa613f0f866010101010101010101010101010101010101010101010101010101010101010102020202020202020202020202020202020202020202020202020202020202020864657669636520310100000001004001010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101000000000301111111111111111111111111111111111111111111111111111111111111111112121212121212121212121212121212121212121212121212121212121212120202020202020202020202020202020202020202020202020202020202020202030303030303030303030303030303030303030303030303030303030303030308646576696365203201000000010040020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020110302d59e0f58642e1a59e3aa613f0f86602020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202030303030303030303030303030303030303030303030303030303030303030313131313131313131313131313131313131313131313131313131313131313131414141414141414141414141414141414141414141414141414141414141414630115151515151515151515151515151515151515151515151515151515151515151616161616161616161616161616161616161616161616161616161616161616020405";

/// The record this build writes reads back the way the bytes kept beside it
/// do: a field added, removed or reordered anywhere in the record, its ledger
/// or the peer state they hold changes these bytes, and a workspace a relay
/// already holds would no longer open.
#[test]
fn the_version_four_record_kept_here_reads_and_writes_as_it_is() {
    assert_golden_bytes(
        "a governed workspace record, version 4",
        &hex::decode(KEPT_V4).unwrap(),
        |bytes| {
            let record: StoredRecord = postcard::from_bytes(bytes).unwrap();
            assert_eq!(record.version, RECORD_VERSION);
            assert_eq!(record.workspace_id, kept_workspace());
            assert_eq!(record.workspace, kept_workspace().to_string());
            assert_eq!((record.filed_publications, record.filed_receipts), (4, 5));
            let ledger = &record.collaboration;
            assert_eq!(ledger.invitations[0].grant.workspace_id, kept_workspace());
            assert_eq!(ledger.join_completions[0].bootstrap.generation, 3);
            assert_eq!(
                ledger.membership_challenges[0].challenge.workspace_id,
                kept_workspace()
            );
            assert_eq!(&*record.outgoing[0].bytes, &[1, 2, 3]);
        },
        kept_version_four,
    );
}

const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

/// The record this build writes: a workspace its host governs, one
/// publication and one administration record not filed beside it yet.
/// Kept so a workspace's file goes on reading the same way; a field
/// added, removed or reordered changes these bytes.
const GOLDEN: &str = include_str!("../../../golden/governed-workspace-record/v4.hex");

fn golden() -> Vec<u8> {
    hex::decode(GOLDEN.split_whitespace().collect::<String>()).unwrap()
}

fn kept() -> StoredRecord {
    StoredRecord {
        version: RECORD_VERSION,
        workspace: WORKSPACE.into(),
        workspace_id: WorkspaceId::parse(WORKSPACE).unwrap(),
        name: "Lounge".into(),
        authority: DeviceCert {
            user_public_key: [1; 32],
            device_public_key: [2; 32],
            device_label: "host".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 3,
            expires_at_ns: None,
            signature: [4; 64],
            issuer: None,
        },
        peer: vec![super::super::peer_parts::PeerPart::Inline(vec![1, 2, 3])],
        generation: 5,
        outgoing: vec![Publication {
            bytes: Arc::from([6, 7, 8]),
            checkpoint: false,
        }],
        published: 0,
        collaboration: JoinLedger {
            administration: vec![AdministrationRecord {
                request_id: [7; 32],
                request_digest: [8; 32],
                generation: 9,
            }],
            ..JoinLedger::default()
        },
        filed_publications: 0,
        filed_receipts: 0,
    }
}

/// `record` as the workspace file's writer encodes it.
fn written(record: &StoredRecord) -> Vec<u8> {
    postcard::to_stdvec(&StoredRecordRef {
        version: record.version,
        workspace: &record.workspace,
        workspace_id: record.workspace_id,
        name: &record.name,
        authority: &record.authority,
        peer: &record.peer,
        generation: record.generation,
        outgoing: &record.outgoing,
        published: record.published,
        collaboration: LedgerRef {
            invitations: &record.collaboration.invitations,
            join_challenges: &record.collaboration.join_challenges,
            join_completions: &record.collaboration.join_completions,
            membership_challenges: &record.collaboration.membership_challenges,
            administration: &record.collaboration.administration,
        },
        filed_publications: record.filed_publications,
        filed_receipts: record.filed_receipts,
    })
    .unwrap()
}

#[test]
fn the_record_kept_here_reads_and_writes_as_it_is() {
    let kept = kept();
    assert_golden_bytes(
        FORMAT.name,
        &golden(),
        |bytes| {
            let read: StoredRecord = postcard::from_bytes(bytes).unwrap();
            assert_eq!(read.version, FORMAT.version);
            assert_eq!(read.workspace_id, kept.workspace_id);
            assert_eq!(read.name, "Lounge");
            assert_eq!(read.authority.device_public_key, [2; 32]);
            assert_eq!(read.peer.len(), 1);
            assert_eq!(read.outgoing[0].bytes.as_ref(), [6, 7, 8]);
            assert_eq!(read.collaboration.administration[0].generation, 9);
        },
        || written(&kept),
    );
}
