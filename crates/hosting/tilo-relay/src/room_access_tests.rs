//! `decide` over every admission mode, principal and plane.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use ed25519_dalek::SigningKey;
use tilo_relay_protocol::agents::{AgentDelegation, AgentScope};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason};

use super::*;
use crate::workspace::Membership;

const ROOM: &str = "6a1f8f2e-4c55-4b0e-9f43-0d6c1b7a9e21";
const OTHER_ROOM: &str = "0b9e8f6c-2a1d-4e3b-8c7f-5d4a3b2c1e0f";
const NOW: u64 = 1_000_000_000_000;

const MODES: [AdmissionMode; 3] = [
    AdmissionMode::Explicit,
    AdmissionMode::SameOwner,
    AdmissionMode::Governed,
];
const OPS: [RoomOperation; 8] = [
    RoomOperation::Subscribe,
    RoomOperation::State,
    RoomOperation::Replay,
    RoomOperation::Live,
    RoomOperation::ContentDiscovery,
    RoomOperation::Compute,
    RoomOperation::Environment,
    RoomOperation::RemoteAction,
];

/// One decision as a letter, so a row reads as the eight planes in order.
fn letter(decision: Decision) -> char {
    match decision {
        Decision::Admit => 'A',
        Decision::AdmitAndEnrol => 'E',
        Decision::Deny(DenyReason::NotAMember) => 'N',
        Decision::Deny(DenyReason::CrossUserFederation) => 'F',
        Decision::Deny(DenyReason::CertExpired) => 'x',
        Decision::Deny(DenyReason::Revoked) => 'r',
        Decision::Deny(DenyReason::NotTrusted) => 't',
        Decision::Deny(DenyReason::AgentWithdrawn) => 'w',
        Decision::Deny(DenyReason::AgentExpired) => 'e',
        Decision::Deny(DenyReason::AgentOutOfScope) => 'o',
    }
}

struct Relay {
    owner_root: SigningKey,
    owner: [u8; 32],
    bob: [u8; 32],
    revocations: RwLock<RevocationList>,
    policy: RwLock<TrustPolicy>,
    agents: AgentRegistry,
}

impl Relay {
    fn new() -> Self {
        let owner_root = SigningKey::from_bytes(&[1; 32]);
        let owner = owner_root.verifying_key().to_bytes();
        let bob = SigningKey::from_bytes(&[2; 32]).verifying_key().to_bytes();
        // The live handshake policy: the owner and Bob, whose root a governed
        // room on this relay admits. Carol is trusted by nothing.
        let policy = TrustPolicy::AllowList {
            users: vec![hex::encode(owner), hex::encode(bob)],
        };
        Self {
            owner_root,
            owner,
            bob,
            revocations: RwLock::new(RevocationList::new()),
            policy: RwLock::new(policy),
            agents: Arc::default(),
        }
    }

    fn trust(&self) -> RelayTrust<'_> {
        RelayTrust {
            revocations: &self.revocations,
            policy: &self.policy,
            own_user: &self.owner,
            agents: &self.agents,
        }
    }

    fn revoke(&self, device: [u8; 32]) {
        let mut entry = RevocationEntry {
            user_public_key: self.owner,
            revoked_device_public_key: device,
            revoked_at_ns: 1,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &self.owner_root).unwrap();
        self.revocations.write().unwrap().insert(entry).unwrap();
    }

    fn delegate(&self, device: [u8; 32], delegation: Option<(u64, &str)>) {
        let delegation = delegation.map(|(expires_at_ns, room)| AgentDelegation {
            user_public_key: self.owner,
            agent_device_public_key: device,
            agent_id: "agent".into(),
            label: "Agent".into(),
            color: [0; 3],
            scope: AgentScope {
                rooms: vec![room.into()],
                apps: Vec::new(),
                may_confirm: false,
            },
            issued_at_ns: 1,
            expires_at_ns,
            signature: [0; 64],
        });
        self.agents.write().unwrap().insert(device, delegation);
    }
}

fn peer(
    user: [u8; 32],
    device: [u8; 32],
    host: bool,
    expires_at_ns: Option<u64>,
) -> AuthenticatedPeer {
    let capabilities = if host {
        DeviceCapabilities::host()
    } else {
        DeviceCapabilities::primary()
    };
    AuthenticatedPeer {
        user_public_key: user,
        device_public_key: device,
        role: if host {
            PeerRole::PeerRelay
        } else {
            PeerRole::Device
        },
        cert: DeviceCert {
            user_public_key: user,
            device_public_key: device,
            device_label: "device".into(),
            capabilities,
            issued_at_ns: 1,
            expires_at_ns,
            signature: [0; 64],
        },
    }
}

fn room(root: &std::path::Path) -> WorkspaceRuntime {
    let id = crate::room_workspace_id(ROOM).unwrap();
    WorkspaceRuntime::open(root, id, false, Membership::default()).unwrap()
}

/// How a case puts its principal into the room and the relay.
type Setup = Box<dyn Fn(&mut WorkspaceRuntime, &Relay)>;

/// One principal: how the room and the relay know it, and the expected
/// letters per mode (`Explicit`, `SameOwner`, `Governed`) over [`OPS`].
struct Case {
    name: &'static str,
    peer: AuthenticatedPeer,
    setup: Setup,
    expected: [&'static str; 3],
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the matrix is one table: each row is a principal, each letter a plane"
)]
fn decide_matrix() {
    let relay = Relay::new();
    let (owner, bob, carol) = (relay.owner, relay.bob, [3; 32]);
    let listed = |device: [u8; 32]| -> Setup {
        Box::new(move |runtime, _| runtime.membership.devices.push(device))
    };
    let none = || -> Setup { Box::new(|_, _| {}) };
    //                 Subscribe, State, Replay, Live, ContentDiscovery, Compute, Environment, RemoteAction
    let cases = vec![
        Case {
            name: "owner's member device",
            peer: peer(owner, [10; 32], false, None),
            setup: listed([10; 32]),
            expected: ["AAAAAAAA", "AAAAAAAA", "AAAAAAAA"],
        },
        Case {
            name: "owner's unlisted device",
            peer: peer(owner, [11; 32], false, None),
            setup: none(),
            expected: ["NNNNNNNN", "ENNAAEAN", "NNNNNNNN"],
        },
        Case {
            name: "owner's device a pending replacement removed",
            peer: peer(owner, [12; 32], false, None),
            setup: Box::new(|runtime, _| {
                runtime.membership.devices.push([12; 32]);
                runtime.membership_ceiling = Some(HashSet::new());
            }),
            expected: ["NNNNNNNN", "NNNNNNNN", "NNNNNNNN"],
        },
        Case {
            name: "cross-user device under an admitted root",
            peer: peer(bob, [20; 32], false, None),
            setup: Box::new(move |runtime, _| {
                runtime.admitted_users.insert(bob);
            }),
            expected: ["AAAAANAA", "AAAAANAA", "AAAAANAA"],
        },
        Case {
            // Failure scenario 1: trusted relay-wide because another room's
            // governance admits the root, never enrolled here.
            name: "cross-user device whose root only another room governs",
            peer: peer(bob, [21; 32], false, None),
            setup: Box::new(move |runtime, _| {
                runtime.governed_user_roots.insert(bob);
            }),
            expected: ["NNNNNNNN", "NNNNNNNN", "NNNNNNNN"],
        },
        Case {
            name: "cross-user exact member device",
            peer: peer(bob, [22; 32], false, None),
            setup: listed([22; 32]),
            expected: ["AAAAAAAA", "AAAAAAAA", "AAAAAAAA"],
        },
        Case {
            name: "same-user peer relay this room federates with",
            peer: peer(owner, [30; 32], true, None),
            setup: Box::new(|runtime, _| {
                runtime.known_peer_relays.insert([30; 32]);
            }),
            expected: ["AAANNNNN", "AAAAAEAN", "AAANNNNN"],
        },
        Case {
            name: "same-user peer relay this room does not know",
            peer: peer(owner, [31; 32], true, None),
            setup: none(),
            expected: ["NNNNNNNN", "ENNAAEAN", "NNNNNNNN"],
        },
        Case {
            name: "cross-user host certificate, even listed and known",
            peer: peer(bob, [32; 32], true, None),
            setup: Box::new(|runtime, _| {
                runtime.membership.devices.push([32; 32]);
                runtime.known_peer_relays.insert([32; 32]);
            }),
            expected: ["FFFFFFFF", "FFFFFFFF", "FFFFFFFF"],
        },
        Case {
            name: "agent delegated this room",
            peer: peer(owner, [40; 32], false, None),
            setup: Box::new(|runtime, relay| {
                runtime.membership.devices.push([40; 32]);
                relay.delegate([40; 32], Some((NOW + 1, ROOM)));
            }),
            expected: ["AAAAAAAA", "AAAAAAAA", "AAAAAAAA"],
        },
        Case {
            name: "agent whose delegation expired",
            peer: peer(owner, [41; 32], false, None),
            setup: Box::new(|runtime, relay| {
                runtime.membership.devices.push([41; 32]);
                relay.delegate([41; 32], Some((NOW, ROOM)));
            }),
            expected: ["eeeeeeee", "eeeeeeee", "eeeeeeee"],
        },
        Case {
            name: "withdrawn agent",
            peer: peer(owner, [42; 32], false, None),
            setup: Box::new(|runtime, relay| {
                runtime.membership.devices.push([42; 32]);
                relay.delegate([42; 32], None);
            }),
            expected: ["wwwwwwww", "wwwwwwww", "wwwwwwww"],
        },
        Case {
            name: "agent delegated another room",
            peer: peer(owner, [43; 32], false, None),
            setup: Box::new(|runtime, relay| {
                runtime.membership.devices.push([43; 32]);
                relay.delegate([43; 32], Some((NOW + 1, OTHER_ROOM)));
            }),
            expected: ["oooooooo", "oooooooo", "oooooooo"],
        },
        Case {
            name: "member device whose certificate expired",
            peer: peer(owner, [50; 32], false, Some(NOW)),
            setup: listed([50; 32]),
            expected: ["xxxxxxxx", "xxxxxxxx", "xxxxxxxx"],
        },
        Case {
            name: "revoked member device",
            peer: peer(owner, [51; 32], false, None),
            setup: Box::new(|runtime, relay| {
                runtime.membership.devices.push([51; 32]);
                relay.revoke([51; 32]);
            }),
            expected: ["rrrrrrrr", "rrrrrrrr", "rrrrrrrr"],
        },
        Case {
            name: "member device of a user the policy no longer trusts",
            peer: peer(carol, [52; 32], false, None),
            setup: listed([52; 32]),
            expected: ["tttttttt", "tttttttt", "tttttttt"],
        },
    ];
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = room(tmp.path());
    let mut failures = Vec::new();
    for case in &cases {
        for (mode, expected) in MODES.iter().zip(case.expected) {
            runtime.membership = Membership::default();
            runtime.membership_ceiling = None;
            runtime.admitted_users.clear();
            runtime.governed_user_roots.clear();
            runtime.known_peer_relays.clear();
            runtime.admission = *mode;
            (case.setup)(&mut runtime, &relay);
            let actual: String = OPS
                .iter()
                .map(|op| {
                    letter(decide_in(
                        Room::current(&runtime),
                        &relay.trust(),
                        &case.peer,
                        *op,
                        NOW,
                    ))
                })
                .collect();
            if actual != expected {
                failures.push(format!(
                    "{} in {mode:?}: expected {expected}, decided {actual}",
                    case.name
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn replay_reads_listed_devices_within_the_ceiling() {
    let relay = Relay::new();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = room(tmp.path());
    let device = peer(relay.owner, [60; 32], false, None);
    let decide = |runtime: &WorkspaceRuntime, listed: &[[u8; 32]]| {
        decide_in(
            Room::with_listed(runtime, listed),
            &relay.trust(),
            &device,
            RoomOperation::Replay,
            NOW,
        )
    };
    // The file lists the device before the runtime has reloaded it.
    assert_eq!(decide(&runtime, &[[60; 32]]), Decision::Admit);
    // The file no longer lists it, though the runtime still does.
    runtime.membership.devices.push([60; 32]);
    assert_eq!(
        decide(&runtime, &[]),
        Decision::Deny(DenyReason::NotAMember)
    );
    // A pending replacement removed it: the old file cannot grant it.
    runtime.membership_ceiling = Some(HashSet::new());
    assert_eq!(
        decide(&runtime, &[[60; 32]]),
        Decision::Deny(DenyReason::NotAMember)
    );
}

#[test]
fn governance_is_never_relaxed_by_a_later_binding() {
    for requested in MODES {
        assert_eq!(
            AdmissionMode::Governed.rebind(requested),
            AdmissionMode::Governed
        );
        assert_eq!(AdmissionMode::Explicit.rebind(requested), requested);
        assert_eq!(AdmissionMode::SameOwner.rebind(requested), requested);
    }
}

#[test]
fn only_owner_and_editor_roles_write() {
    use collab_types::Role;
    let writers: Vec<Role> = [
        Role::Owner,
        Role::Editor,
        Role::Commenter,
        Role::Viewer,
        Role::Denied,
    ]
    .into_iter()
    .filter(|role| role_grants(*role, Verb::Write))
    .collect();
    assert_eq!(writers, vec![Role::Owner, Role::Editor]);
}

/// A device with an unsigned certificate for planes that read admission
/// only; signatures are the handshake's business, not [`decide`]'s.
pub(crate) fn unsigned_peer(user: [u8; 32], device: [u8; 32]) -> AuthenticatedPeer {
    peer(user, device, false, None)
}
