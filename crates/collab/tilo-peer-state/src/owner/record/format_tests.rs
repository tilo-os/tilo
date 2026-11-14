//! A workspace's entry as the 0.1 releases wrote it comes up to this build's
//! layout, and this build's layout is pinned by the bytes it writes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]
use super::{FORMAT, read_whole, stored_version, vouched_answer};
use crate::join::{DocumentRow, JoinTransportResponse, Role};
use crate::owner::record::{StoredWorkspace, WORKSPACE_VERSION};
use crate::owner::testing;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_desktop_model::WorkspaceId;
use tilo_files::native::migration::{assert_golden_bytes, assert_step_reads_previous};
use tilo_user_identity::RevocationEntry;

/// A record v0.1.5 wrote as one file: two workspaces founded here, one
/// enrollment in flight.
const RELEASED: &str =
    include_str!("../../../tests/fixtures/owner-record/single-file.owner.json.hex");
/// One workspace's file as this build writes it: [`RELEASED`]'s `Founders`
/// as it comes up.
const CURRENT: &str = include_str!("../../../golden/owner-workspace-record/v4.hex");
const FOUNDERS: &str = "40267648-f762-4c8d-89ad-c5ab27aa13b8";

fn bytes(hex: &str) -> Vec<u8> {
    hex.split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).unwrap())
        .collect()
}

/// The released record's entry for `Founders`, as its file held it.
fn released_entry() -> Value {
    let whole: Value = serde_json::from_slice(&bytes(RELEASED)).unwrap();
    whole["rooms"][FOUNDERS].clone()
}

fn founders() -> WorkspaceId {
    WorkspaceId::parse(FOUNDERS).unwrap()
}

#[test]
fn the_format_brings_every_version_up() {
    assert!(FORMAT.is_whole());
    assert_eq!(FORMAT.version, WORKSPACE_VERSION);
}

#[test]
fn a_released_entry_reads_in_this_builds_layout() {
    let mut previous = released_entry();
    previous["version"] = 3.into();
    assert_step_reads_previous(&FORMAT, &serde_json::to_vec(&previous).unwrap(), |bytes| {
        let stored: StoredWorkspace = serde_json::from_slice(bytes).unwrap();
        assert_eq!(stored.version, WORKSPACE_VERSION);
        assert_eq!(stored.workspace, founders());
    });
}

#[test]
fn a_record_a_release_wrote_as_one_file_reads() {
    let record = read_whole(&bytes(RELEASED)).unwrap();
    assert_eq!(record.revision(), 3);
    assert_eq!(record.held().count(), 2);
    assert!(
        record
            .workspaces
            .values()
            .all(|stored| stored.version == WORKSPACE_VERSION)
    );
    assert!(
        record
            .workspaces
            .iter()
            .all(|(workspace, stored)| stored.workspace == *workspace)
    );
}

#[test]
fn an_entry_of_a_newer_build_or_before_the_releases_is_refused() {
    for version in [1, WORKSPACE_VERSION + 1] {
        let mut whole: Value = serde_json::from_slice(&bytes(RELEASED)).unwrap();
        whole["rooms"][FOUNDERS]["version"] = version.into();
        assert!(read_whole(&serde_json::to_vec(&whole).unwrap()).is_err());
    }
}

/// The ledger of a workspace that issued an invitation and admitted a join
/// through it, with a join and a membership challenge still waiting, as the
/// releases wrote it.
#[test]
fn a_released_join_ledger_names_its_workspace_by_id() {
    let mut entry = released_entry();
    let host = entry["room_host"].clone();
    let hint = Vec::<Value>::new();
    let grant = json!({
        "workspace_id": 11_844_705_113_337_559_481_u64,
        "room": FOUNDERS,
        "issuer": host,
        "role": "Editor",
        "nonce": vec![1_u8; 16],
        "recipient_user_public_key": null,
        "invitee_name": "Bo",
        "expires_at_ns": 9,
        "transport": hint,
    });
    let challenge = json!({
        "remote_endpoint": vec![2_u8; 32],
        "request_digest": vec![3_u8; 32],
        "device_cert": host,
        "challenge": {
            "version": 2,
            "signed_invitation": [4, 5],
            "room": FOUNDERS,
            "workspace_id": 11_844_705_113_337_559_481_u64,
            "user_public_key": vec![6_u8; 32],
            "device_public_key": vec![7_u8; 32],
            "request_id": vec![8_u8; 32],
            "nonce": vec![9_u8; 32],
            "expires_at_ns": 10,
        },
    });
    entry["join_ledger"] = json!({
        "invitations": [{
            "request_id": vec![11_u8; 32],
            "request_digest": vec![12_u8; 32],
            "issued": { "invite_id": vec![13_u8; 32], "signed_invitation": [3, 1, 2] },
            "grant": grant,
            "cancellation": null,
        }],
        "join_challenges": [challenge],
        "join_completions": [{
            "remote_endpoint": vec![14_u8; 32],
            "request_id": vec![15_u8; 32],
            "proof_digest": vec![16_u8; 32],
            "bootstrap": {
                "version": 1,
                "invite_id": vec![13_u8; 32],
                "device_cert": host,
                "role": "Viewer",
                "room": FOUNDERS,
                "workspace_id": 11_844_705_113_337_559_481_u64,
                "host": host,
                "certified_roster": [],
                "revocations": [],
                "records": [],
                "governance_messages": [[17, 18]],
                "generation": 4,
            },
        }],
        "membership_challenges": [],
        "administration": [{ "request_id": vec![19_u8; 32], "request_digest": vec![20_u8; 32], "generation": 2 }],
    });
    let brought = FORMAT
        .migrate(serde_json::to_vec(&entry).unwrap(), 2)
        .unwrap();
    let stored: StoredWorkspace = serde_json::from_slice(&brought).unwrap();
    let ledger = &stored.join_ledger;
    let [invitation] = ledger.invitations.as_slice() else {
        panic!("one invitation");
    };
    assert_eq!(invitation.grant.workspace_id, founders());
    assert_eq!(invitation.grant.role, Role::Editor);
    assert_eq!(invitation.grant.invitee_name.as_deref(), Some("Bo"));
    assert_eq!(
        invitation.issued.signed_invitation,
        [3, 1, 2],
        "signed bytes are kept as they are"
    );
    assert!(ledger.join_challenges.is_empty());
    assert!(ledger.membership_challenges.is_empty());
    let [completion] = ledger.join_completions.as_slice() else {
        panic!("one completion");
    };
    assert_eq!(completion.bootstrap.workspace_id, founders());
    assert_eq!(completion.bootstrap.role, Role::Viewer);
    assert_eq!(completion.bootstrap.generation, 4);
    assert!(completion.bootstrap.governance_messages.is_empty());
    assert_eq!(ledger.administration.len(), 1);
}

/// A membership answer a host vouched for, in the releases' layout: the
/// response's variant, then its bootstrap naming the workspace by text and
/// number.
#[test]
fn a_released_vouched_answer_names_its_workspace_by_id() {
    let key = SigningKey::from_bytes(&[12; 32]);
    let device = testing::certificate(&SigningKey::from_bytes(&[11; 32]), &key, "A's desktop");
    let released = postcard::to_stdvec(&(
        4_u32,
        (
            1_u32,
            [7_u8; 32],
            &device,
            Role::Moderator,
            FOUNDERS,
            11_844_705_113_337_559_481_u64,
            &device,
            vec![&device],
            Vec::<RevocationEntry>::new(),
            Vec::<DocumentRow>::new(),
            vec![vec![1_u8, 2]],
            5_u64,
        ),
    ))
    .unwrap();
    let answer: JoinTransportResponse =
        postcard::from_bytes(&vouched_answer(&released).unwrap()).unwrap();
    let JoinTransportResponse::MembershipComplete(bootstrap) = answer else {
        panic!("a membership answer");
    };
    assert_eq!(bootstrap.workspace_id, founders());
    assert_eq!(bootstrap.role, Role::Moderator);
    assert_eq!(bootstrap.device_cert, device);
    assert_eq!(bootstrap.governance_messages, vec![vec![1, 2]]);
    assert_eq!(bootstrap.generation, 5);
}

/// This build's layout: a field added, removed, renamed or reordered breaks
/// it, which is when the entry needs a new version and a step from this one.
#[test]
fn this_builds_entry_reads_and_writes_as_it_is() {
    let golden = bytes(CURRENT);
    assert_eq!(stored_version(&golden).unwrap(), WORKSPACE_VERSION);
    let read = || serde_json::from_slice::<StoredWorkspace>(&golden).unwrap();
    assert_golden_bytes(
        "owner workspace record",
        &golden,
        |_| {
            let stored = read();
            assert_eq!(stored.workspace, founders());
            assert!(stored.hosted_here());
        },
        || crate::owner::file_store::part_bytes(&read()).unwrap(),
    );
}
