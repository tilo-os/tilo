use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use ed25519_dalek::SigningKey;
use tilo_relay_protocol::delegation::{Delegate, DelegateScope, Delegation};
use tilo_user_identity::{
    AgentScope, DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason,
};

use super::*;
use crate::workspace::Membership;
use tilo_relay_protocol::WorkspaceId;

const ROOM: &str = "6a1f8f2e-4c55-4b0e-9f43-0d6c1b7a9e21";
const OTHER_ROOM: &str = "0b9e8f6c-2a1d-4e3b-8c7f-5d4a3b2c1e0f";
const NOW: u64 = 1_000_000_000_000;

const MODES: [AdmissionMode; 3] = [
    AdmissionMode::Explicit,
    AdmissionMode::SameOwner,
    AdmissionMode::Governed,
];
const OPS: [WorkspaceOperation; 11] = [
    WorkspaceOperation::Subscribe,
    WorkspaceOperation::State,
    WorkspaceOperation::Replay,
    WorkspaceOperation::Live,
    WorkspaceOperation::ContentDiscovery,
    WorkspaceOperation::Compute,
    WorkspaceOperation::Environment,
    WorkspaceOperation::RemoteAction,
    WorkspaceOperation::DelegateMembership,
    WorkspaceOperation::Publish,
    WorkspaceOperation::ChannelAccess,
];

/// One decision as a letter, so a row reads as the planes in order.
fn letter(decision: Decision) -> char {
    match decision {
        Decision::Admit => 'A',
        Decision::AdmitAndEnrol => 'E',
        Decision::Deny(DenyReason::NotAMember) => 'N',
        Decision::Deny(DenyReason::CertExpired) => 'x',
        Decision::Deny(DenyReason::Revoked) => 'r',
        Decision::Deny(DenyReason::NotTrusted) => 't',
        Decision::Deny(DenyReason::AgentWithdrawn) => 'w',
        Decision::Deny(DenyReason::AgentExpired) => 'e',
        Decision::Deny(DenyReason::AgentOutOfScope) => 'o',
        Decision::Deny(DenyReason::OutOfEnvironmentScope) => 'b',
        Decision::Deny(DenyReason::EnvironmentStopped) => 's',
        Decision::Deny(DenyReason::NotADelegatedAgent) => 'd',
    }
}

struct Relay {
    owner_root: SigningKey,
    owner: [u8; 32],
    bob: [u8; 32],
    revocations: RwLock<RevocationList>,
    policy: RwLock<TrustPolicy>,
    agents: AgentRegistry,
    environments: crate::environment::EnvironmentRegistry,
}

impl Relay {
    fn new() -> Self {
        let owner_root = SigningKey::from_bytes(&[1; 32]);
        let owner = owner_root.verifying_key().to_bytes();
        let bob = SigningKey::from_bytes(&[2; 32]).verifying_key().to_bytes();
        // The live handshake policy: the owner and Bob, whose roots a governed
        // workspace on this relay admits. Carol is trusted by nothing.
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
            environments: crate::environment::EnvironmentRegistry::default(),
        }
    }

    fn trust(&self) -> RelayTrust<'_> {
        RelayTrust {
            revocations: &self.revocations,
            policy: &self.policy,
            own_user: &self.owner,
            agents: &self.agents,
            environments: &self.environments,
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
        let delegation = delegation.map(|(expires_at_ns, workspace)| Delegation {
            user_public_key: self.owner,
            device_public_key: device,
            delegate: Delegate::Agent {
                agent_id: "agent".into(),
            },
            label: "Agent".into(),
            color: [0; 3],
            scope: DelegateScope {
                workspaces: vec![WorkspaceId::parse(workspace).unwrap()],
                apps: Vec::new(),
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
        cert: DeviceCert {
            user_public_key: user,
            device_public_key: device,
            device_label: "device".into(),
            capabilities,
            issued_at_ns: 1,
            expires_at_ns,
            signature: [0; 64],
            issuer: None,
        },
    }
}

/// An agent's grant of `user`'s, reaching `scope`.
fn grant(
    user: [u8; 32],
    device: [u8; 32],
    scope: AgentScope,
    expires_at_ns: Option<u64>,
) -> AuthenticatedPeer {
    let mut grant = peer(user, device, false, expires_at_ns);
    grant.cert.capabilities = DeviceCapabilities::agent_grant(scope);
    grant
}

/// The scope of one workspace, by its id.
fn one(workspace: &str) -> AgentScope {
    AgentScope::parse(workspace).unwrap()
}

/// A grant `device` that redeemed an invitation of its person `user`, a
/// member of the workspace.
fn member_grant(device: [u8; 32], user: [u8; 32]) -> Setup {
    Box::new(move |runtime, _| {
        runtime.membership.devices.push(device);
        runtime.governed_user_roots.insert(user);
    })
}

fn workspace(root: &std::path::Path) -> WorkspaceRuntime {
    let id = WorkspaceId::parse(ROOM).unwrap();
    WorkspaceRuntime::open(root, id, false, Membership::default()).unwrap()
}

/// How a case puts its principal into the workspace and the relay.
type Setup = Box<dyn Fn(&mut WorkspaceRuntime, &Relay)>;

/// One principal: how the workspace and the relay know it, and the expected
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
    // Subscribe, State, Replay, Live, ContentDiscovery, Compute, Environment, RemoteAction,
    // DelegateMembership, Publish, ChannelAccess
    let cases = vec![
        Case {
            name: "owner's member device",
            peer: peer(owner, [10; 32], false, None),
            setup: listed([10; 32]),
            expected: ["AAAAAAAAdAA", "AAAAAAAAdAA", "AAAAAAAAdAA"],
        },
        Case {
            name: "owner's unlisted device",
            peer: peer(owner, [11; 32], false, None),
            setup: none(),
            expected: ["NNNNNNNNdNN", "ENNAAEANdNN", "NNNNNNNNdNN"],
        },
        // An agent's grant: its person's agent where its scope reaches, it
        // redeemed its person's invitation and its person is a member, on an
        // agent's planes only.
        Case {
            name: "a grant for all its person's workspaces, a member device of a member",
            peer: grant(owner, [13; 32], AgentScope::AllWorkspaces, Some(NOW + 1)),
            setup: member_grant([13; 32], owner),
            expected: ["AAAAANNANAA", "AAAAANNANAA", "AAAAANNANAA"],
        },
        Case {
            name: "a grant listed in a governed room its person is no member of",
            peer: grant(owner, [14; 32], AgentScope::AllWorkspaces, Some(NOW + 1)),
            setup: listed([14; 32]),
            expected: ["AAAAANNANAA", "AAAAANNANAA", "NNNNNNNNNNN"],
        },
        Case {
            name: "a grant for exactly this room",
            peer: grant(owner, [15; 32], one(ROOM), Some(NOW + 1)),
            setup: member_grant([15; 32], owner),
            expected: ["AAAAANNANAA", "AAAAANNANAA", "AAAAANNANAA"],
        },
        Case {
            name: "a grant for another room, even listed here",
            peer: grant(owner, [16; 32], one(OTHER_ROOM), Some(NOW + 1)),
            setup: member_grant([16; 32], owner),
            expected: ["NNNNNNNNNNN", "NNNNNNNNNNN", "NNNNNNNNNNN"],
        },
        Case {
            name: "a grant that never redeemed an invitation here, on its person's own host",
            peer: grant(owner, [17; 32], AgentScope::AllWorkspaces, Some(NOW + 1)),
            setup: Box::new(move |runtime, _| {
                runtime.governed_user_roots.insert(owner);
            }),
            expected: ["NNNNNNNNNNN", "NNNNNNNNNNN", "NNNNNNNNNNN"],
        },
        Case {
            name: "a grant past its certificate's expiry",
            peer: grant(owner, [18; 32], AgentScope::AllWorkspaces, Some(NOW)),
            setup: member_grant([18; 32], owner),
            expected: ["xxxxxxxxxxx", "xxxxxxxxxxx", "xxxxxxxxxxx"],
        },
        Case {
            name: "a grant its person revoked",
            peer: grant(owner, [19; 32], AgentScope::AllWorkspaces, Some(NOW + 1)),
            setup: Box::new(move |runtime, relay| {
                member_grant([19; 32], owner)(runtime, relay);
                relay.revoke([19; 32]);
            }),
            expected: ["rrrrrrrrrrr", "rrrrrrrrrrr", "rrrrrrrrrrr"],
        },
        Case {
            name: "a grant minted for this room by a grant its person revoked",
            peer: {
                let parent = grant(owner, [62; 32], AgentScope::AllWorkspaces, Some(NOW + 1));
                let mut minted = grant(owner, [63; 32], one(ROOM), Some(NOW + 1));
                minted.cert.issuer = Some(Box::new(parent.cert));
                minted
            },
            setup: Box::new(move |runtime, relay| {
                member_grant([63; 32], owner)(runtime, relay);
                relay.revoke([62; 32]);
            }),
            expected: ["rrrrrrrrrrr", "rrrrrrrrrrr", "rrrrrrrrrrr"],
        },
        Case {
            name: "owner's device a pending replacement removed",
            peer: peer(owner, [12; 32], false, None),
            setup: Box::new(|runtime, _| {
                runtime.membership.devices.push([12; 32]);
                runtime.membership_ceiling = Some(HashSet::new());
            }),
            expected: ["NNNNNNNNdNN", "NNNNNNNNdNN", "NNNNNNNNdNN"],
        },
        Case {
            // Failure scenario 1: trusted relay-wide because another workspace's
            // governance admits the root, never enrolled here.
            name: "cross-user device whose root only another room governs",
            peer: peer(bob, [21; 32], false, None),
            setup: Box::new(move |runtime, _| {
                runtime.governed_user_roots.insert(bob);
            }),
            expected: ["NNNNNNNNdNN", "NNNNNNNNdNN", "NNNNNNNNdNN"],
        },
        Case {
            name: "cross-user exact member device",
            peer: peer(bob, [22; 32], false, None),
            setup: listed([22; 32]),
            expected: ["AAAAAAAAdAA", "AAAAAAAAdAA", "AAAAAAAAdAA"],
        },
        Case {
            name: "same-user host this room is attached to",
            peer: peer(owner, [30; 32], true, None),
            setup: Box::new(|runtime, _| {
                runtime.attached_hosts.insert([30; 32]);
            }),
            expected: ["AAANNNNNdAA", "AAAAAEANdAA", "AAANNNNNdAA"],
        },
        Case {
            name: "same-user host this room is not attached to",
            peer: peer(owner, [31; 32], true, None),
            setup: none(),
            expected: ["NNNNNNNNdNN", "ENNAAEANdNN", "NNNNNNNNdNN"],
        },
        Case {
            name: "cross-user host certificate, listed as a member",
            peer: peer(bob, [32; 32], true, None),
            setup: listed([32; 32]),
            expected: ["AAAAAAAAdAA", "AAAAAAAAdAA", "AAAAAAAAdAA"],
        },
        Case {
            name: "agent delegated this room",
            peer: peer(owner, [40; 32], false, None),
            setup: Box::new(|runtime, relay| {
                runtime.membership.devices.push([40; 32]);
                relay.delegate([40; 32], Some((NOW + 1, ROOM)));
            }),
            expected: ["AAAAAAAAdAA", "AAAAAAAAdAA", "AAAAAAAAdAA"],
        },
        Case {
            name: "agent whose delegation expired",
            peer: peer(owner, [41; 32], false, None),
            setup: Box::new(|runtime, relay| {
                runtime.membership.devices.push([41; 32]);
                relay.delegate([41; 32], Some((NOW, ROOM)));
            }),
            expected: ["eeeeeeeeeee", "eeeeeeeeeee", "eeeeeeeeeee"],
        },
        Case {
            name: "withdrawn agent",
            peer: peer(owner, [42; 32], false, None),
            setup: Box::new(|runtime, relay| {
                runtime.membership.devices.push([42; 32]);
                relay.delegate([42; 32], None);
            }),
            expected: ["wwwwwwwwwww", "wwwwwwwwwww", "wwwwwwwwwww"],
        },
        Case {
            name: "agent delegated another room",
            peer: peer(owner, [43; 32], false, None),
            setup: Box::new(|runtime, relay| {
                runtime.membership.devices.push([43; 32]);
                relay.delegate([43; 32], Some((NOW + 1, OTHER_ROOM)));
            }),
            expected: ["ooooooooooo", "ooooooooooo", "ooooooooooo"],
        },
        Case {
            name: "the device governing this room on this relay",
            peer: peer(owner, [70; 32], true, None),
            setup: Box::new(|runtime, _| {
                runtime.workspace_authority =
                    Some(crate::workspaces::WorkspaceAuthority::governed_by(ROOM));
            }),
            expected: ["NNNNNNNNdNN", "ENNAAEANdNN", "NNNNNNNNdNN"],
        },
        Case {
            name: "another listed device of the owner, while this relay governs",
            peer: peer(owner, [71; 32], false, None),
            setup: Box::new(|runtime, _| {
                runtime.membership.devices.push([71; 32]);
                runtime.workspace_authority =
                    Some(crate::workspaces::WorkspaceAuthority::governed_by(ROOM));
            }),
            expected: ["AAAAAAAAdAA", "AAAAAAAAdAA", "AAAAAAAAdAA"],
        },
        Case {
            name: "agent delegated this room, in a room this relay governs",
            peer: peer(owner, [44; 32], false, None),
            setup: Box::new(|runtime, relay| {
                runtime.workspace_authority =
                    Some(crate::workspaces::WorkspaceAuthority::governed_by(ROOM));
                relay.delegate([44; 32], Some((NOW + 1, ROOM)));
            }),
            expected: ["NNNNNNNNdNN", "ENNAAEANdNN", "NNNNNNNNANN"],
        },
        Case {
            name: "another user's agent, in a room this relay governs",
            peer: peer(bob, [45; 32], false, None),
            setup: Box::new(|runtime, relay| {
                runtime.workspace_authority =
                    Some(crate::workspaces::WorkspaceAuthority::governed_by(ROOM));
                relay.delegate([45; 32], Some((NOW + 1, ROOM)));
            }),
            expected: ["NNNNNNNNdNN", "NNNNNNNNdNN", "NNNNNNNNdNN"],
        },
        Case {
            name: "member device whose certificate expired",
            peer: peer(owner, [50; 32], false, Some(NOW)),
            setup: listed([50; 32]),
            expected: ["xxxxxxxxxxx", "xxxxxxxxxxx", "xxxxxxxxxxx"],
        },
        Case {
            name: "revoked member device",
            peer: peer(owner, [51; 32], false, None),
            setup: Box::new(|runtime, relay| {
                runtime.membership.devices.push([51; 32]);
                relay.revoke([51; 32]);
            }),
            expected: ["rrrrrrrrrrr", "rrrrrrrrrrr", "rrrrrrrrrrr"],
        },
        Case {
            name: "member device of a user the policy no longer trusts",
            peer: peer(carol, [52; 32], false, None),
            setup: listed([52; 32]),
            expected: ["ttttttttttt", "ttttttttttt", "ttttttttttt"],
        },
    ];
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = workspace(tmp.path());
    let mut failures = Vec::new();
    for case in &cases {
        for (mode, expected) in MODES.iter().zip(case.expected) {
            runtime.membership = Membership::default();
            runtime.membership_ceiling = None;
            runtime.governed_user_roots.clear();
            runtime.attached_hosts.clear();
            runtime.workspace_authority = None;
            runtime.state_sessions.clear();
            runtime.admission = *mode;
            (case.setup)(&mut runtime, &relay);
            let actual: String = OPS
                .iter()
                .map(|op| {
                    letter(decide_in(
                        WorkspaceFacts::current(&runtime),
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

/// A browser environment's device reaches the planes a window's session
/// works on in its one workspace, and reaches nothing at all once its
/// environment stopped — however the workspace lists it.
#[test]
fn an_environments_device_is_confined_and_a_stopped_one_is_refused() {
    let relay = Relay::new();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = workspace(tmp.path());
    let now = tilo_clock::unix_now_ns();
    let until = now.saturating_add(60 * 1_000_000_000);
    let device = [80; 32];
    let environment = peer(relay.owner, device, false, Some(until));
    // The workspace lists it as a member device of the person, as it does
    // while the session runs: what it reaches is the register's to say.
    runtime.membership.devices.push(device);
    let scope = crate::environment::EnvironmentScope::for_record(
        WorkspaceId::parse(ROOM).unwrap(),
        "so.tilo.browser",
        "record-1",
    );
    let letters = |runtime: &WorkspaceRuntime| -> String {
        OPS.iter()
            .map(|op| {
                letter(decide_in(
                    WorkspaceFacts::current(runtime),
                    &relay.trust(),
                    &environment,
                    *op,
                    now,
                ))
            })
            .collect()
    };
    relay.environments.place(device, scope, until).unwrap();
    assert_eq!(
        letters(&runtime),
        "AAAAbbbAbAA",
        "subscribe, state, replay and live; discovery, compute, environment and \
         governance are another device's; its record's actions, publishing and \
         channel access"
    );
    relay.environments.stop(device, until).unwrap();
    assert_eq!(
        letters(&runtime),
        "sssssssssss",
        "a stopped environment's device is refused on every plane, listed or not"
    );
    // Past its certificate's expiry the certificate refuses it, and the
    // register need not keep the entry.
    assert_eq!(
        decide_in(
            WorkspaceFacts::current(&runtime),
            &relay.trust(),
            &environment,
            WorkspaceOperation::State,
            until,
        ),
        Decision::Deny(DenyReason::CertExpired)
    );
}

#[test]
fn replay_reads_listed_devices_within_the_ceiling() {
    let relay = Relay::new();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = workspace(tmp.path());
    let device = peer(relay.owner, [60; 32], false, None);
    let decide = |runtime: &WorkspaceRuntime, listed: &[[u8; 32]]| {
        decide_in(
            WorkspaceFacts::with_listed(runtime, listed),
            &relay.trust(),
            &device,
            WorkspaceOperation::Replay,
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
fn an_agent_joins_governance_only_inside_its_live_delegation() {
    let relay = Relay::new();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = workspace(tmp.path());
    runtime.admission = AdmissionMode::Governed;
    runtime.workspace_authority = Some(crate::workspaces::WorkspaceAuthority::governed_by(ROOM));
    let agent = peer(relay.owner, [90; 32], false, None);
    let decide = |runtime: &WorkspaceRuntime, now: u64| {
        decide_in(
            WorkspaceFacts::current(runtime),
            &relay.trust(),
            &agent,
            WorkspaceOperation::DelegateMembership,
            now,
        )
    };
    // Without a grant the device is no agent: refused.
    assert_eq!(
        decide(&runtime, NOW),
        Decision::Deny(DenyReason::NotADelegatedAgent)
    );
    relay.delegate([90; 32], Some((NOW + 1, ROOM)));
    assert_eq!(decide(&runtime, NOW), Decision::Admit);
    // At its expiry, and once withdrawn, it is refused.
    assert_eq!(
        decide(&runtime, NOW + 1),
        Decision::Deny(DenyReason::AgentExpired)
    );
    relay.delegate([90; 32], None);
    assert_eq!(
        decide(&runtime, NOW),
        Decision::Deny(DenyReason::AgentWithdrawn)
    );
    // A workspace no authority of this relay governs takes no agent in.
    relay.delegate([90; 32], Some((NOW + 1, ROOM)));
    runtime.workspace_authority = None;
    assert_eq!(
        decide(&runtime, NOW),
        Decision::Deny(DenyReason::NotADelegatedAgent)
    );
    // Its certificate must still be current.
    runtime.workspace_authority = Some(crate::workspaces::WorkspaceAuthority::governed_by(ROOM));
    relay.revoke([90; 32]);
    assert_eq!(decide(&runtime, NOW), Decision::Deny(DenyReason::Revoked));
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
fn owners_moderators_and_editors_write() {
    use collab_types::Role;
    let writers: Vec<Role> = [
        Role::Owner,
        Role::Moderator,
        Role::Editor,
        Role::Commenter,
        Role::Viewer,
    ]
    .into_iter()
    .filter(|role| role_writes(*role))
    .collect();
    assert_eq!(writers, vec![Role::Owner, Role::Moderator, Role::Editor]);
}

/// A device with an unsigned certificate for planes that read admission
/// only; signatures are the handshake's business, not [`decide`]'s.
pub(crate) fn unsigned_peer(user: [u8; 32], device: [u8; 32]) -> AuthenticatedPeer {
    peer(user, device, false, None)
}

/// A signed certificate of `root` for `device`, issued at `issued_at_ns`,
/// granting the member set and expiring a day later.
fn signed_member(root: &SigningKey, device: &SigningKey, issued_at_ns: u64) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "tilo-yoga".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns,
        expires_at_ns: Some(issued_at_ns.saturating_add(24 * 60 * 60 * 1_000_000_000)),
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
    cert
}

/// A joined device's certificate that ran out is refused at a host, and the
/// renewal of it — the same device and root, issued later, granting no more —
/// is admitted. This is the shape of a laptop whose certificate lapsed and
/// whose person's devices renewed it.
#[test]
fn a_host_refuses_a_lapsed_certificate_and_admits_its_renewal() {
    let relay = Relay::new();
    let root = &relay.owner_root;
    let device = SigningKey::from_bytes(&[0x5a; 32]);
    // The matrix's NOW is an arbitrary instant; a certificate is judged
    // against the clock a device actually has.
    let now = 1_800_000_000_000_000_000;
    let lapsed = signed_member(root, &device, now - 48 * 60 * 60 * 1_000_000_000);
    let renewed = signed_member(root, &device, now - 60 * 1_000_000_000);
    assert!(
        lapsed.is_expired_at(now),
        "the certificate the host knew ran out"
    );
    assert!(!renewed.is_expired_at(now), "the renewal is valid now");
    assert!(renewed.renews(&lapsed));

    let trust = relay.trust();
    let lapsed_peer = AuthenticatedPeer {
        user_public_key: lapsed.user_public_key,
        device_public_key: lapsed.device_public_key,
        cert: lapsed,
    };
    assert_eq!(
        trust.cert(&lapsed_peer, now),
        Err(DenyReason::CertExpired),
        "a host refuses the certificate that ran out"
    );

    let renewed_peer = AuthenticatedPeer {
        user_public_key: renewed.user_public_key,
        device_public_key: renewed.device_public_key,
        cert: renewed,
    };
    assert!(
        trust.cert(&renewed_peer, now).is_ok(),
        "and admits the renewal, as the same device"
    );
}
