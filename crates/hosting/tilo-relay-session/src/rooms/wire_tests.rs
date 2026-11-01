//! Workspace-join wire bytes, captured by encoding the same
//! values with the types as they were in `tilo-relay` before this crate was
//! split out. These are postcard formats between separately updated
//! hosts and desktops: a changed byte here is a wire break, not a vector to
//! recapture.
//!
//! The join challenge no longer carries the room's governing device beside the
//! signer: a host answers a join as a device of the room's owner, which the
//! membership challenge already names. The two vectors holding a join challenge
//! (`Complete` and the `Challenge` answer) are that much shorter, and a host
//! and a desktop of different versions refuse each other's bytes.

use super::IssuedInvitation;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
use tilo_workspace_join::{
    JoinChallenge, JoinChallengeRequest, JoinChallengeResponse, JoinCompleteRequest, JoinProof,
    JoinTransportRequest, JoinTransportResponse, MembershipChallenge, MembershipChallengeRequest,
    MembershipChallengeResponse, MembershipCompleteRequest, MembershipProof,
};

const JOIN_REQUESTS: &[(&str, &str)] = &[
    ("Inspect", "0003030104"),
    (
        "Challenge",
        "01020509fa4834147f6e690c3693eff61336046403cd8ae2a14f31b3c407358569239565197f6b23e16c8532c6abc838facd5ea789be0c76b2920334039bfa8b3d368d6104776972650000010107010940003b8467f0ecc7b3fe0f9dd333b350bb4860f9dd08ff19295dd94a3014614b7409e733873b7d78f0ca3b0472ae715517e391bdd444851e0441b493c61effe9001111111111111111111111111111111111111111111111111111111111111111",
    ),
    (
        "Complete",
        "02010201022430303030303030302d303030302d303030302d303030302d303030303030303030303031030d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f1010101010101010101010101010101010101010101010101010101010101010044012121212121212121212121212121212121212121212121212121212121212121212121212121212121212121212121212121212121212121212121212121212",
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
        "01010201022430303030303030302d303030302d303030302d303030302d303030303030303030303031030d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f101010101010101010101010101010101010101010101010101010101010101004",
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

fn join_challenge() -> JoinChallenge {
    JoinChallenge {
        version: 1,
        signed_invitation: vec![1, 2],
        room: "00000000-0000-0000-0000-000000000001".into(),
        workspace_id: 3,
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
        host_device_public_key: [12; 32],
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
fn workspace_join_wire_bytes_are_stable() {
    assert_stable("JoinTransportRequest", join_requests(), JOIN_REQUESTS);
    assert_stable("JoinTransportResponse", join_responses(), JOIN_RESPONSES);
}

#[test]
fn an_actions_invitation_decodes_to_what_was_issued() {
    let issued = IssuedInvitation {
        invite_id: [4; 32],
        signed_invitation: vec![1, 2, 3],
    };
    let answered = super::ActionInvitation::of(&issued);
    assert_eq!(answered.invite, hex::encode([4; 32]));
    assert_eq!(answered.decode(), Ok(issued));
    let short = super::ActionInvitation {
        invite: "00".into(),
        invitation: String::new(),
    };
    assert!(short.decode().is_err());
}
