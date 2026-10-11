//! Room-admin and workspace-join wire bytes, captured by encoding the same
//! values with the types as they were in `tilo-relay` before this crate was
//! split out (TIL-240). These are postcard formats between separately updated
//! hosts and desktops: a changed byte here is a wire break, not a vector to
//! recapture.

use super::{
    CancelInvitationRequest, IssueInvitationRequest, IssuedInvitation, PendingInvitation,
    RemoveMemberRequest, RoomAdminRequest, RoomCommand, RoomReply, RoomSummary,
    SetMemberRoleRequest,
};
use tilo_peer_state::GovernedMember;
use tilo_peer_state::join::{
    JoinChallenge, JoinChallengeRequest, JoinChallengeResponse, JoinCompleteRequest, JoinProof,
    JoinTransportRequest, JoinTransportResponse, MemberRole, MembershipChallenge,
    MembershipChallengeRequest, MembershipChallengeResponse, MembershipCompleteRequest,
    MembershipProof,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

const COMMANDS: &[(&str, &str)] = &[
    ("Create", "0004526f6f6d"),
    ("List", "01"),
    (
        "Enter",
        "022430303030303030302d303030302d303030302d303030302d303030303030303030303031",
    ),
    (
        "Invite",
        "032430303030303030302d303030302d303030302d303030302d3030303030303030303030310101010101010101010101010101010101010101010101010101010101010101020102020202020202020202020202020202020202020202020202020202020202020102426fe8070103416461",
    ),
    (
        "Cancel",
        "042430303030303030302d303030302d303030302d303030302d30303030303030303030303103030303030303030303030303030303030303030303030303030303030303030404040404040404040404040404040404040404040404040404040404040404",
    ),
    (
        "Pending",
        "052430303030303030302d303030302d303030302d303030302d303030303030303030303031",
    ),
    (
        "SetRole",
        "062430303030303030302d303030302d303030302d303030302d3030303030303030303030310505050505050505050505050505050505050505050505050505050505050505060606060606060606060606060606060606060606060606060606060606060600",
    ),
    (
        "Remove",
        "072430303030303030302d303030302d303030302d303030302d30303030303030303030303107070707070707070707070707070707070707070707070707070707070707070808080808080808080808080808080808080808080808080808080808080808",
    ),
    (
        "RecordEffectReceipt",
        "082430303030303030302d303030302d303030302d303030302d3030303030303030303030311000112233445566778899aabbccddeeff03090807",
    ),
    (
        "ClaimTopologyNamespace",
        "092430303030303030302d303030302d303030302d303030302d3030303030303030303030312a",
    ),
];

const REPLIES: &[(&str, &str)] = &[
    (
        "Room",
        "002430303030303030302d303030302d303030302d303030302d303030303030303030303031888e98a8c0e080810104526f6f6dac020105050505050505050505050505050505050505050505050505050505050505050101034164610106060606060606060606060606060606060606060606060606060606060606060001",
    ),
    (
        "Rooms",
        "01022430303030303030302d303030302d303030302d303030302d303030303030303030303031888e98a8c0e080810104526f6f6dac0201050505050505050505050505050505050505050505050505050505050505050501010341646101060606060606060606060606060606060606060606060606060606060606060600012430303030303030302d303030302d303030302d303030302d303030303030303030303031888e98a8c0e080810104526f6f6dac020105050505050505050505050505050505050505050505050505050505050505050101034164610106060606060606060606060606060606060606060606060606060606060606060001",
    ),
    ("Entered", "0201"),
    (
        "Invitation",
        "030a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a03010203",
    ),
    (
        "Pending",
        "04010b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b010001024379d00f",
    ),
    ("Governance", "054d"),
    ("Done", "06"),
    ("Refused", "07026e6f"),
];

const REQUESTS: &[(&str, &str)] = &[(
    "List",
    "fa4834147f6e690c3693eff61336046403cd8ae2a14f31b3c407358569239565197f6b23e16c8532c6abc838facd5ea789be0c76b2920334039bfa8b3d368d6104776972650000010107010940003b8467f0ecc7b3fe0f9dd333b350bb4860f9dd08ff19295dd94a3014614b7409e733873b7d78f0ca3b0472ae715517e391bdd444851e0441b493c61effe90001",
)];

const JOIN_REQUESTS: &[(&str, &str)] = &[
    ("Inspect", "0003030104"),
    (
        "Challenge",
        "01020509fa4834147f6e690c3693eff61336046403cd8ae2a14f31b3c407358569239565197f6b23e16c8532c6abc838facd5ea789be0c76b2920334039bfa8b3d368d6104776972650000010107010940003b8467f0ecc7b3fe0f9dd333b350bb4860f9dd08ff19295dd94a3014614b7409e733873b7d78f0ca3b0472ae715517e391bdd444851e0441b493c61effe9001111111111111111111111111111111111111111111111111111111111111111",
    ),
    (
        "Complete",
        "02010201022430303030303030302d303030302d303030302d303030302d303030303030303030303031030c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f1010101010101010101010101010101010101010101010101010101010101010044012121212121212121212121212121212121212121212121212121212121212121212121212121212121212121212121212121212121212121212121212121212",
    ),
    (
        "MembershipChallenge",
        "032430303030303030302d303030302d303030302d303030302d30303030303030303030303103fa4834147f6e690c3693eff61336046403cd8ae2a14f31b3c407358569239565197f6b23e16c8532c6abc838facd5ea789be0c76b2920334039bfa8b3d368d6104776972650000010107010940003b8467f0ecc7b3fe0f9dd333b350bb4860f9dd08ff19295dd94a3014614b7409e733873b7d78f0ca3b0472ae715517e391bdd444851e0441b493c61effe9001313131313131313131313131313131313131313131313131313131313131313",
    ),
    (
        "MembershipComplete",
        "04012430303030303030302d303030302d303030302d303030302d303030303030303030303031030c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f1010101010101010101010101010101010101010101010101010101010101010044014141414141414141414141414141414141414141414141414141414141414141414141414141414141414141414141414141414141414141414141414141414",
    ),
];

const JOIN_RESPONSES: &[(&str, &str)] = &[
    (
        "Challenge",
        "01010201022430303030303030302d303030302d303030302d303030302d303030303030303030303031030c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f101010101010101010101010101010101010101010101010101010101010101004",
    ),
    (
        "MembershipChallenge",
        "03012430303030303030302d303030302d303030302d303030302d303030303030303030303031030c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f101010101010101010101010101010101010101010101010101010101010101004",
    ),
    ("Error", "059903086361707475726564"),
];

fn cert() -> DeviceCert {
    let root = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "wire".into(),
        capabilities: DeviceCapabilities::host(),
        issued_at_ns: 7,
        expires_at_ns: Some(9),
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    cert
}

fn summary() -> RoomSummary {
    RoomSummary {
        room: "00000000-0000-0000-0000-000000000001".into(),
        workspace_id: 0x0102_0304_0506_0708,
        name: "Room".into(),
        generation: 300,
        members: vec![GovernedMember {
            user_public_key: [5; 32],
            role: MemberRole::Commenter,
            display_name: Some("Ada".into()),
            active_device_keys: vec![[6; 32]],
            removed: false,
            administrator: true,
        }],
    }
}

fn commands() -> Vec<(&'static str, RoomCommand)> {
    let room = || "00000000-0000-0000-0000-000000000001".to_string();
    vec![
        (
            "Create",
            RoomCommand::Create {
                name: "Room".into(),
            },
        ),
        ("List", RoomCommand::List),
        ("Enter", RoomCommand::Enter { room: room() }),
        (
            "Invite",
            RoomCommand::Invite {
                room: room(),
                request: IssueInvitationRequest {
                    request_id: [1; 32],
                    role: MemberRole::Editor,
                    recipient_user_public_key: Some([2; 32]),
                    invitee_name: Some("Bo".into()),
                    expires_at_ns: 1_000,
                    inviter_name: Some("Ada".into()),
                },
            },
        ),
        (
            "Cancel",
            RoomCommand::Cancel {
                room: room(),
                request: CancelInvitationRequest {
                    request_id: [3; 32],
                    invite_id: [4; 32],
                },
            },
        ),
        ("Pending", RoomCommand::Pending { room: room() }),
        (
            "SetRole",
            RoomCommand::SetRole {
                room: room(),
                request: SetMemberRoleRequest {
                    request_id: [5; 32],
                    user_public_key: [6; 32],
                    role: MemberRole::Viewer,
                },
            },
        ),
        (
            "Remove",
            RoomCommand::Remove {
                room: room(),
                request: RemoveMemberRequest {
                    request_id: [7; 32],
                    user_public_key: [8; 32],
                },
            },
        ),
        (
            "RecordEffectReceipt",
            RoomCommand::RecordEffectReceipt {
                room: room(),
                invocation: uuid::Uuid::from_u128(0x0011_2233_4455_6677_8899_aabb_ccdd_eeff),
                redacted: vec![9, 8, 7],
            },
        ),
        (
            "ClaimTopologyNamespace",
            RoomCommand::ClaimTopologyNamespace {
                room: room(),
                salt: 42,
            },
        ),
    ]
}

fn replies() -> Vec<(&'static str, RoomReply)> {
    vec![
        ("Room", RoomReply::Room(summary())),
        ("Rooms", RoomReply::Rooms(vec![summary(), summary()])),
        ("Entered", RoomReply::Entered { changed: true }),
        (
            "Invitation",
            RoomReply::Invitation(IssuedInvitation {
                invite_id: [10; 32],
                signed_invitation: vec![1, 2, 3],
            }),
        ),
        (
            "Pending",
            RoomReply::Pending(vec![PendingInvitation {
                invite_id: [11; 32],
                role: MemberRole::Commenter,
                recipient_user_public_key: None,
                invitee_name: Some("Cy".into()),
                expires_at_ns: 2_000,
            }]),
        ),
        ("Governance", RoomReply::Governance { generation: 77 }),
        ("Done", RoomReply::Done),
        ("Refused", RoomReply::Refused("no".into())),
    ]
}

fn join_challenge() -> JoinChallenge {
    JoinChallenge {
        version: 1,
        signed_invitation: vec![1, 2],
        room: "00000000-0000-0000-0000-000000000001".into(),
        workspace_id: 3,
        authority_device_public_key: [12; 32],
        user_public_key: [13; 32],
        device_public_key: [14; 32],
        request_id: [15; 32],
        nonce: [16; 32],
        expires_at_ns: 4,
    }
}

fn membership_challenge() -> MembershipChallenge {
    MembershipChallenge {
        version: 1,
        room: "00000000-0000-0000-0000-000000000001".into(),
        workspace_id: 3,
        authority_device_public_key: [12; 32],
        user_public_key: [13; 32],
        device_public_key: [14; 32],
        request_id: [15; 32],
        nonce: [16; 32],
        expires_at_ns: 4,
    }
}

fn join_requests() -> Vec<(&'static str, JoinTransportRequest)> {
    vec![
        (
            "Inspect",
            JoinTransportRequest::Inspect {
                signed_invitation: vec![3, 1, 4],
            },
        ),
        (
            "Challenge",
            JoinTransportRequest::Challenge(JoinChallengeRequest {
                signed_invitation: vec![5, 9],
                device_cert: cert(),
                request_id: [17; 32],
            }),
        ),
        (
            "Complete",
            JoinTransportRequest::Complete(JoinCompleteRequest {
                proof: JoinProof {
                    challenge: join_challenge(),
                    signature: [18; 64],
                },
            }),
        ),
        (
            "MembershipChallenge",
            JoinTransportRequest::MembershipChallenge(MembershipChallengeRequest {
                room: "00000000-0000-0000-0000-000000000001".into(),
                workspace_id: 3,
                device_cert: cert(),
                request_id: [19; 32],
            }),
        ),
        (
            "MembershipComplete",
            JoinTransportRequest::MembershipComplete(MembershipCompleteRequest {
                proof: MembershipProof {
                    challenge: membership_challenge(),
                    signature: [20; 64],
                },
            }),
        ),
    ]
}

fn join_responses() -> Vec<(&'static str, JoinTransportResponse)> {
    vec![
        (
            "Challenge",
            JoinTransportResponse::Challenge(JoinChallengeResponse {
                challenge: join_challenge(),
            }),
        ),
        (
            "MembershipChallenge",
            JoinTransportResponse::MembershipChallenge(MembershipChallengeResponse {
                challenge: membership_challenge(),
            }),
        ),
        (
            "Error",
            JoinTransportResponse::Error {
                code: 409,
                message: "captured".into(),
            },
        ),
    ]
}

fn assert_stable<T>(what: &str, values: Vec<(&'static str, T)>, expected: &[(&str, &str)])
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let names: Vec<&str> = values.iter().map(|(name, _)| *name).collect();
    let expected_names: Vec<&str> = expected.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        names, expected_names,
        "{what}: every captured case is encoded"
    );
    for ((name, value), (_, bytes)) in values.into_iter().zip(expected) {
        let encoded = postcard::to_stdvec(&value).unwrap();
        assert_eq!(
            hex::encode(&encoded),
            *bytes,
            "{what} {name} changed on the wire"
        );
        let decoded: T = postcard::from_bytes(&encoded).unwrap();
        assert_eq!(decoded, value, "{what} {name} round-trips");
    }
}

#[test]
fn room_admin_wire_bytes_are_stable() {
    assert_stable("RoomCommand", commands(), COMMANDS);
    assert_stable("RoomReply", replies(), REPLIES);
    assert_stable(
        "RoomAdminRequest",
        vec![(
            "List",
            RoomAdminRequest {
                device_cert: cert(),
                command: RoomCommand::List,
            },
        )],
        REQUESTS,
    );
}

#[test]
fn workspace_join_wire_bytes_are_stable() {
    assert_stable("JoinTransportRequest", join_requests(), JOIN_REQUESTS);
    assert_stable("JoinTransportResponse", join_responses(), JOIN_RESPONSES);
}

/// Every room variant has a captured case: a new variant stops this match
/// compiling, and the counts fail until the variant has a case and a vector.
#[test]
fn room_admin_wire_cases_cover_every_variant() {
    for (_, command) in commands() {
        match command {
            RoomCommand::Create { .. }
            | RoomCommand::List
            | RoomCommand::Enter { .. }
            | RoomCommand::Invite { .. }
            | RoomCommand::Cancel { .. }
            | RoomCommand::Pending { .. }
            | RoomCommand::SetRole { .. }
            | RoomCommand::Remove { .. }
            | RoomCommand::RecordEffectReceipt { .. }
            | RoomCommand::ClaimTopologyNamespace { .. } => {}
        }
    }
    for (_, reply) in replies() {
        match reply {
            RoomReply::Room(_)
            | RoomReply::Rooms(_)
            | RoomReply::Entered { .. }
            | RoomReply::Invitation(_)
            | RoomReply::Pending(_)
            | RoomReply::Governance { .. }
            | RoomReply::Done
            | RoomReply::Refused(_) => {}
        }
    }
    assert_eq!(commands().len(), 10);
    assert_eq!(replies().len(), 8);
}
