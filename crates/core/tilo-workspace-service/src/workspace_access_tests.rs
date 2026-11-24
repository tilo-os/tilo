use std::sync::{Arc, RwLock};

use ed25519_dalek::SigningKey;
use tilo_server_protocol::delegation::{Delegate, DelegateScope, Delegation};
use tilo_user_identity::{
    AgentScope, DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason,
};

use super::*;
use tilo_server_protocol::WorkspaceId;

const WORKSPACE: &str = "6a1f8f2e-4c55-4b0e-9f43-0d6c1b7a9e21";
const OTHER_WORKSPACE: &str = "0b9e8f6c-2a1d-4e3b-8c7f-5d4a3b2c1e0f";
const NOW: u64 = 1_000_000_000_000;

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
    WorkspaceOperation::KeyAccess,
];

/// One decision as a letter, so a row reads as the protocols in order.
fn letter(decision: Decision) -> char {
    match decision {
        Decision::Admit => 'A',
        Decision::Deny(DenyReason::Unlisted) => 'E',
        Decision::Deny(DenyReason::NotAMember) => 'N',
        Decision::Deny(DenyReason::CertExpired) => 'x',
        Decision::Deny(DenyReason::Revoked) => 'r',
        Decision::Deny(DenyReason::Blocked) => 'k',
        Decision::Deny(DenyReason::NotTrusted) => 't',
        Decision::Deny(DenyReason::AgentWithdrawn) => 'w',
        Decision::Deny(DenyReason::AgentExpired) => 'e',
        Decision::Deny(DenyReason::AgentOutOfScope) => 'o',
        Decision::Deny(DenyReason::OutOfEnvironmentScope) => 'b',
        Decision::Deny(DenyReason::EnvironmentStopped) => 's',
        Decision::Deny(DenyReason::NotADelegatedAgent) => 'd',
    }
}

struct Server {
    owner_root: SigningKey,
    owner: [u8; 32],
    bob: [u8; 32],
    revocations: RwLock<RevocationList>,
    policy: RwLock<TrustPolicy>,
    blocked: RwLock<crate::workspaces::blocked::BlockedKeys>,
    agents: AgentRegistry,
    environments: crate::environment::EnvironmentRegistry,
}

impl Server {
    fn new() -> Self {
        let owner_root = SigningKey::from_bytes(&[1; 32]);
        let owner = owner_root.verifying_key().to_bytes();
        let bob = SigningKey::from_bytes(&[2; 32]).verifying_key().to_bytes();
        // The live handshake policy: the owner and Bob, whose roots a
        // workspace on this server has as members. Carol is trusted by nothing.
        let policy = TrustPolicy::AllowList {
            users: vec![hex::encode(owner), hex::encode(bob)],
        };
        Self {
            owner_root,
            owner,
            bob,
            revocations: RwLock::new(RevocationList::new()),
            policy: RwLock::new(policy),
            blocked: RwLock::new(crate::workspaces::blocked::BlockedKeys::default()),
            agents: Arc::default(),
            environments: crate::environment::EnvironmentRegistry::default(),
        }
    }

    fn trust(&self) -> ServerTrust<'_> {
        ServerTrust {
            revocations: &self.revocations,
            policy: &self.policy,
            blocked: &self.blocked,
            own_user: &self.owner,
            agents: &self.agents,
            environments: &self.environments,
        }
    }

    fn block_device(&self, device: [u8; 32]) {
        let mut state = crate::workspaces::blocked::Blocked::default();
        state
            .block_device(hex::encode(device))
            .expect("a device key blocks");
        *self.blocked.write().unwrap() = crate::workspaces::blocked::BlockedKeys::of(&state);
    }

    /// The live policy trusts the person whose root is `user` too.
    fn trust_person(&self, user: [u8; 32]) {
        if let TrustPolicy::AllowList { users } = &mut *self.policy.write().unwrap() {
            users.push(hex::encode(user));
        }
    }

    fn block_user(&self, user: [u8; 32]) {
        let mut state = crate::workspaces::blocked::Blocked::default();
        state
            .block_user(hex::encode(user))
            .expect("a user key blocks");
        *self.blocked.write().unwrap() = crate::workspaces::blocked::BlockedKeys::of(&state);
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

    fn delegate(&self, user: [u8; 32], device: [u8; 32], delegation: Option<(u64, &str)>) {
        let delegation = delegation.map(|(expires_at_ns, workspace)| Delegation {
            user_public_key: user,
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
        DeviceCapabilities::member()
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
        list_device(runtime, device);
        list_person(runtime, user);
    })
}

/// Put `device` on the workspace's member list.
fn list_device(runtime: &mut WorkspaceRuntime, device: [u8; 32]) {
    let members = Arc::make_mut(&mut runtime.members);
    members.member_devices.push(device);
    members.member_devices.sort_unstable();
}

/// Put the person whose root is `user` on the workspace's member list.
fn list_person(runtime: &mut WorkspaceRuntime, user: [u8; 32]) {
    let members = Arc::make_mut(&mut runtime.members);
    members.member_user_roots.push(user);
    members.member_user_roots.sort_unstable();
}

/// [`super::decide_in`] over what the host's runtime for the workspace holds.
fn decide_in(
    runtime: &WorkspaceRuntime,
    trust: &ServerTrust<'_>,
    peer: &AuthenticatedPeer,
    op: WorkspaceOperation,
    now_ns: u64,
) -> Decision {
    super::decide_in(WorkspaceFacts::current(runtime), trust, peer, op, now_ns)
}

fn workspace(root: &std::path::Path) -> WorkspaceRuntime {
    let id = WorkspaceId::parse(WORKSPACE).unwrap();
    WorkspaceRuntime::open(root, id).unwrap()
}

/// How a case puts its actor into the workspace and the server.
type Setup = Box<dyn Fn(&mut WorkspaceRuntime, &Server)>;

/// One actor: how the workspace and the server know it, and the expected
/// letters over [`OPS`].
struct Case {
    name: &'static str,
    peer: AuthenticatedPeer,
    setup: Setup,
    expected: &'static str,
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the matrix is one table: each row is an actor, each letter a protocol"
)]
fn decide_matrix() {
    let server = Server::new();
    let (owner, bob, carol) = (server.owner, server.bob, [3; 32]);
    let listed =
        |device: [u8; 32]| -> Setup { Box::new(move |runtime, _| list_device(runtime, device)) };
    let none = || -> Setup { Box::new(|_, _| {}) };
    // Subscribe, State, Replay, Live, ContentDiscovery, Compute, Environment, RemoteAction,
    // DelegateMembership, Publish, KeyAccess
    let cases = vec![
        Case {
            name: "owner's member device",
            peer: peer(owner, [10; 32], false, None),
            setup: listed([10; 32]),
            expected: "AAAAAAAAdAA",
        },
        Case {
            name: "owner's unlisted device",
            peer: peer(owner, [11; 32], false, None),
            setup: none(),
            expected: "NNNNNNNNdNN",
        },
        // An agent's grant: its person's agent where its scope reaches, it
        // redeemed its person's invitation and its person is a member, on an
        // agent's protocols only.
        Case {
            name: "a grant for all its person's workspaces, a member device of a member",
            peer: grant(owner, [13; 32], AgentScope::AllWorkspaces, Some(NOW + 1)),
            setup: member_grant([13; 32], owner),
            expected: "AAAAANNANAA",
        },
        Case {
            name: "a grant listed in a workspace its person is no member of",
            peer: grant(owner, [14; 32], AgentScope::AllWorkspaces, Some(NOW + 1)),
            setup: listed([14; 32]),
            expected: "NNNNNNNNNNN",
        },
        Case {
            name: "a grant for exactly this workspace",
            peer: grant(owner, [15; 32], one(WORKSPACE), Some(NOW + 1)),
            setup: member_grant([15; 32], owner),
            expected: "AAAAANNANAA",
        },
        Case {
            name: "a grant for another workspace, even listed here",
            peer: grant(owner, [16; 32], one(OTHER_WORKSPACE), Some(NOW + 1)),
            setup: member_grant([16; 32], owner),
            expected: "NNNNNNNNNNN",
        },
        Case {
            name: "a grant that never redeemed an invitation here, on its person's own host",
            peer: grant(owner, [17; 32], AgentScope::AllWorkspaces, Some(NOW + 1)),
            setup: Box::new(move |runtime, _| {
                list_person(runtime, owner);
            }),
            expected: "NNNNNNNNNNN",
        },
        Case {
            name: "a grant past its certificate's expiry",
            peer: grant(owner, [18; 32], AgentScope::AllWorkspaces, Some(NOW)),
            setup: member_grant([18; 32], owner),
            expected: "xxxxxxxxxxx",
        },
        Case {
            name: "a grant its person revoked",
            peer: grant(owner, [19; 32], AgentScope::AllWorkspaces, Some(NOW + 1)),
            setup: Box::new(move |runtime, server| {
                member_grant([19; 32], owner)(runtime, server);
                server.revoke([19; 32]);
            }),
            expected: "rrrrrrrrrrr",
        },
        Case {
            name: "a grant minted for this workspace by a grant its person revoked",
            peer: {
                let parent = grant(owner, [62; 32], AgentScope::AllWorkspaces, Some(NOW + 1));
                let mut minted = grant(owner, [63; 32], one(WORKSPACE), Some(NOW + 1));
                minted.cert.issuer = Some(Box::new(parent.cert));
                minted
            },
            setup: Box::new(move |runtime, server| {
                member_grant([63; 32], owner)(runtime, server);
                server.revoke([62; 32]);
            }),
            expected: "rrrrrrrrrrr",
        },
        Case {
            // The operator's handshake policy does not gate a workspace's own membership: the
            // list holds the device's person, so the link admits it, and its subscribe enrols
            // it without the server's own place in the workspace.
            name: "cross-user device whose root only another workspace governs",
            peer: peer(bob, [21; 32], false, None),
            setup: none(),
            expected: "NNNNNNNNdNN",
        },
        Case {
            // A further device of a person the list has lists itself on its first link, and
            // reaches nothing else until it is on the list.
            name: "cross-user device of a member, not on the list yet",
            peer: peer(bob, [23; 32], false, None),
            setup: Box::new(move |runtime, _| {
                list_person(runtime, bob);
            }),
            expected: "ENNNNNNNdNN",
        },
        Case {
            name: "cross-user exact member device",
            peer: peer(bob, [22; 32], false, None),
            setup: listed([22; 32]),
            expected: "AAAAAAAAdAA",
        },
        Case {
            // Holding the workspace, or being the server it lives on, makes nobody a member.
            name: "same-user host the member list does not have",
            peer: peer(owner, [31; 32], true, None),
            setup: none(),
            expected: "NNNNNNNNdNN",
        },
        Case {
            name: "cross-user host certificate, listed as a member",
            peer: peer(bob, [32; 32], true, None),
            setup: listed([32; 32]),
            expected: "AAAAAAAAdAA",
        },
        Case {
            name: "agent delegated this workspace",
            peer: peer(owner, [40; 32], false, None),
            setup: Box::new(|runtime, server| {
                list_device(runtime, [40; 32]);
                server.delegate(server.owner, [40; 32], Some((NOW + 1, WORKSPACE)));
            }),
            expected: "AAAAAAAAdAA",
        },
        Case {
            name: "agent whose delegation expired",
            peer: peer(owner, [41; 32], false, None),
            setup: Box::new(|runtime, server| {
                list_device(runtime, [41; 32]);
                server.delegate(server.owner, [41; 32], Some((NOW, WORKSPACE)));
            }),
            expected: "eeeeeeeeeee",
        },
        Case {
            name: "withdrawn agent",
            peer: peer(owner, [42; 32], false, None),
            setup: Box::new(|runtime, server| {
                list_device(runtime, [42; 32]);
                server.delegate(server.owner, [42; 32], None);
            }),
            expected: "wwwwwwwwwww",
        },
        Case {
            name: "agent delegated another workspace",
            peer: peer(owner, [43; 32], false, None),
            setup: Box::new(|runtime, server| {
                list_device(runtime, [43; 32]);
                server.delegate(server.owner, [43; 32], Some((NOW + 1, OTHER_WORKSPACE)));
            }),
            expected: "ooooooooooo",
        },
        Case {
            // Its person is a member: an unlisted device of theirs links once it lists itself.
            name: "an unlisted device of this server's person, a member",
            peer: peer(owner, [70; 32], true, None),
            setup: Box::new(move |runtime, _| list_person(runtime, owner)),
            expected: "ENNNNNNNdNN",
        },
        Case {
            name: "another listed device of the owner, a member",
            peer: peer(owner, [71; 32], false, None),
            setup: Box::new(move |runtime, _| {
                list_device(runtime, [71; 32]);
                list_person(runtime, owner);
            }),
            expected: "AAAAAAAAdAA",
        },
        Case {
            name: "agent of this server's person, a member, delegated this workspace",
            peer: peer(owner, [44; 32], false, None),
            setup: Box::new(move |runtime, server| {
                list_person(runtime, owner);
                server.delegate(server.owner, [44; 32], Some((NOW + 1, WORKSPACE)));
            }),
            expected: "ENNNNNNNANN",
        },
        Case {
            // The agent's approver is a device of the person it acts for, Bob
            // here, and the host's own person is no special case: it needs no
            // place in the workspace to carry Bob's agent in.
            name: "a member's agent, where this server's person has no place",
            peer: peer(bob, [45; 32], false, None),
            setup: Box::new(move |runtime, server| {
                list_person(runtime, bob);
                server.delegate(server.bob, [45; 32], Some((NOW + 1, WORKSPACE)));
            }),
            expected: "ENNNNNNNANN",
        },
        Case {
            // An agent of a person the list does not have joins nowhere, whoever the host is.
            name: "agent delegated this workspace by a person the list does not have",
            peer: peer(bob, [47; 32], false, None),
            setup: Box::new(move |runtime, server| {
                list_person(runtime, owner);
                server.delegate(server.bob, [47; 32], Some((NOW + 1, WORKSPACE)));
            }),
            expected: "NNNNNNNNdNN",
        },
        Case {
            name: "a further device of a member",
            peer: peer(bob, [46; 32], false, None),
            setup: Box::new(move |runtime, _| {
                list_person(runtime, bob);
                list_person(runtime, owner);
            }),
            expected: "ENNNNNNNdNN",
        },
        Case {
            // A member's further device is theirs by the certificate their root signed: a server
            // holding the workspace for someone else, with no place in it, carries it in.
            name: "a further device of a member, where this server's person has no place",
            peer: peer(bob, [48; 32], false, None),
            setup: Box::new(move |runtime, _| list_person(runtime, bob)),
            expected: "ENNNNNNNdNN",
        },
        Case {
            // A person the list removed is no member: none of their devices is admitted,
            // wherever the server holding the workspace stands.
            name: "a device of a person the list removed",
            peer: peer(bob, [49; 32], false, None),
            setup: Box::new(move |runtime, _| list_person(runtime, owner)),
            expected: "NNNNNNNNdNN",
        },
        Case {
            name: "member device whose certificate expired",
            peer: peer(owner, [50; 32], false, Some(NOW)),
            setup: listed([50; 32]),
            expected: "xxxxxxxxxxx",
        },
        Case {
            name: "revoked member device",
            peer: peer(owner, [51; 32], false, None),
            setup: Box::new(|runtime, server| {
                list_device(runtime, [51; 32]);
                server.revoke([51; 32]);
            }),
            expected: "rrrrrrrrrrr",
        },
        Case {
            // A device the workspace's own list holds reaches it on that rule: the operator's
            // handshake policy gates who connects to the host, not who its membership admits.
            name: "member device of a user the policy no longer trusts",
            peer: peer(carol, [52; 32], false, None),
            setup: listed([52; 32]),
            expected: "AAAAAAAAdAA",
        },
    ];
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = workspace(tmp.path());
    let mut failures = Vec::new();
    for case in &cases {
        runtime.members = Arc::default();
        (case.setup)(&mut runtime, &server);
        let actual: String = OPS
            .iter()
            .map(|op| letter(decide_in(&runtime, &server.trust(), &case.peer, *op, NOW)))
            .collect();
        if actual != case.expected {
            failures.push(format!(
                "{}: expected {}, decided {actual}",
                case.name, case.expected
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A browser environment's device reaches the protocols a window's session
/// works on in its one workspace, and reaches nothing at all once its
/// environment stopped — however the workspace lists it.
#[test]
fn an_environments_device_is_confined_and_a_stopped_one_is_refused() {
    let server = Server::new();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = workspace(tmp.path());
    let now = tilo_clock::unix_now_ns();
    let until = now.saturating_add(60 * 1_000_000_000);
    let device = [80; 32];
    let environment = peer(server.owner, device, false, Some(until));
    // The workspace lists it as a member device of the person, as it does
    // while the session runs: what it reaches is the register's to say.
    list_device(&mut runtime, device);
    let scope = crate::environment::EnvironmentScope::for_resource(
        WorkspaceId::parse(WORKSPACE).unwrap(),
        "so.tilo.browser",
        "record-1",
    );
    let letters = |runtime: &WorkspaceRuntime| -> String {
        OPS.iter()
            .map(|op| letter(decide_in(runtime, &server.trust(), &environment, *op, now)))
            .collect()
    };
    server.environments.place(device, scope, until).unwrap();
    assert_eq!(
        letters(&runtime),
        "AAAAbbbAbAA",
        "subscribe, state, replay and live; discovery, compute, environment and \
         governance are another device's; its record's actions, publishing and \
         channel access"
    );
    server.environments.stop(device, until).unwrap();
    assert_eq!(
        letters(&runtime),
        "sssssssssss",
        "a stopped environment's device is refused on every protocol, listed or not"
    );
    // Past its certificate's expiry the certificate refuses it, and the
    // register need not keep the entry.
    assert_eq!(
        decide_in(
            &runtime,
            &server.trust(),
            &environment,
            WorkspaceOperation::State,
            until,
        ),
        Decision::Deny(DenyReason::CertExpired)
    );
}

#[test]
fn an_agent_joins_the_member_list_only_inside_its_live_delegation() {
    let server = Server::new();
    let tmp = tempfile::tempdir().unwrap();
    let mut runtime = workspace(tmp.path());
    list_person(&mut runtime, server.owner);
    let agent = peer(server.owner, [90; 32], false, None);
    let decide = |runtime: &WorkspaceRuntime, now: u64| {
        decide_in(
            runtime,
            &server.trust(),
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
    server.delegate(server.owner, [90; 32], Some((NOW + 1, WORKSPACE)));
    assert_eq!(decide(&runtime, NOW), Decision::Admit);
    // At its expiry, and once withdrawn, it is refused.
    assert_eq!(
        decide(&runtime, NOW + 1),
        Decision::Deny(DenyReason::AgentExpired)
    );
    server.delegate(server.owner, [90; 32], None);
    assert_eq!(
        decide(&runtime, NOW),
        Decision::Deny(DenyReason::AgentWithdrawn)
    );
    // A workspace whose list does not have the agent's person takes no agent in.
    server.delegate(server.owner, [90; 32], Some((NOW + 1, WORKSPACE)));
    let members = std::mem::take(&mut runtime.members);
    assert_eq!(
        decide(&runtime, NOW),
        Decision::Deny(DenyReason::NotADelegatedAgent)
    );
    // Its certificate must still be current.
    runtime.members = members;
    server.revoke([90; 32]);
    assert_eq!(decide(&runtime, NOW), Decision::Deny(DenyReason::Revoked));
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
    let server = Server::new();
    let root = &server.owner_root;
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

    let trust = server.trust();
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

/// A browser tab one of the person's devices signed is a device of theirs; a
/// workspace service its device made, an agent's grant, a stand-in and a
/// certificate one of those signed are not.
#[test]
fn a_tab_a_device_signed_is_a_device_of_the_person() {
    let root = SigningKey::from_bytes(&[0x31; 32]);
    let device_key = SigningKey::from_bytes(&[0x32; 32]);
    let device = signed_member(&root, &device_key, 1_000);
    assert!(person_device(&device), "the root's own device");

    let signed_by = |key: &SigningKey,
                     capabilities: DeviceCapabilities,
                     issuer: &DeviceCert,
                     signer: &SigningKey| {
        let mut cert = DeviceCert {
            user_public_key: issuer.user_public_key,
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "Firefox on phone".into(),
            capabilities,
            issued_at_ns: 2_000,
            expires_at_ns: Some(2_000 + 30 * 24 * 60 * 60 * 1_000_000_000),
            signature: [0; 64],
            issuer: Some(Box::new(issuer.clone())),
        };
        tilo_user_identity::sign_device_cert(&mut cert, signer).unwrap();
        cert
    };

    let tab = signed_by(
        &SigningKey::from_bytes(&[0x33; 32]),
        DeviceCapabilities::member(),
        &device,
        &device_key,
    );
    assert!(
        person_device(&tab),
        "a tab one of the person's devices signed is theirs"
    );

    // A device's workspace service is that device's own, not a device of the
    // person's, and nothing it signs is one.
    let service_key = SigningKey::from_bytes(&[0x34; 32]);
    let mut service = tilo_user_identity::prepare_service_cert(
        &device,
        service_key.verifying_key().to_bytes(),
        "Apps on tilo-yoga",
        2_000,
    );
    tilo_user_identity::sign_device_cert(&mut service, &device_key).unwrap();
    assert!(!person_device(&service));
    let mut under_service = DeviceCert {
        user_public_key: service.user_public_key,
        device_public_key: [0x35; 32],
        device_label: "Firefox on phone".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 2_000,
        expires_at_ns: Some(2_000 + 30 * 24 * 60 * 60 * 1_000_000_000),
        signature: [0; 64],
        issuer: Some(Box::new(service)),
    };
    assert!(
        tilo_user_identity::sign_device_cert(&mut under_service, &service_key).is_err(),
        "a service signs no device"
    );
    assert!(!person_device(&under_service));

    // An agent's grant and a stand-in are keys made to act for the person.
    let mut grant = DeviceCert {
        user_public_key: device.user_public_key,
        device_public_key: [0x36; 32],
        device_label: "Jane Doe's agent".into(),
        capabilities: DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
        issued_at_ns: 2_000,
        expires_at_ns: Some(2_000 + 7 * 24 * 60 * 60 * 1_000_000_000),
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut grant, &root).unwrap();
    assert!(!person_device(&grant));
    let mut stand_in = tilo_user_identity::prepare_stand_in_cert(
        device.user_public_key,
        [0x37; 32],
        "Stand-in on tilo.example",
        2_000,
        2_000 + 24 * 60 * 60 * 1_000_000_000,
    );
    tilo_user_identity::sign_device_cert(&mut stand_in, &root).unwrap();
    assert!(!person_device(&stand_in));
}

/// The operator's block stands where a workspace's membership and the live
/// policy both admit a person: a blocked device is refused whatever else lets
/// it in. Blocking a person's root refuses every device of theirs.
#[test]
fn a_blocked_device_or_person_is_refused_though_membership_admits_it() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::new();
    let guest = [0x4a; 32];
    let device = [0x4b; 32];
    // The workspace holds the person and the device, and the policy trusts the
    // person, as it does the members of the workspaces held here.
    server.trust_person(guest);
    let mut runtime = workspace(dir.path());
    list_person(&mut runtime, guest);
    list_device(&mut runtime, device);
    let first_peer = peer(guest, device, true, None);
    let decide = |op| decide_in(&runtime, &server.trust(), &first_peer, op, NOW);
    assert_eq!(
        decide(WorkspaceOperation::Subscribe),
        Decision::Admit,
        "the workspace's membership admits the guest's device"
    );

    server.block_device(device);
    assert_eq!(
        decide(WorkspaceOperation::Subscribe),
        Decision::Deny(DenyReason::Blocked)
    );
    assert_eq!(
        decide(WorkspaceOperation::Publish),
        Decision::Deny(DenyReason::Blocked)
    );

    // The person's key refuses every device of theirs, listed or not.
    let other_dir = tempfile::tempdir().unwrap();
    let mut other = workspace(other_dir.path());
    let second = [0x4c; 32];
    list_person(&mut other, guest);
    list_device(&mut other, second);
    let second_peer = peer(guest, second, true, None);
    let server = Server::new();
    server.trust_person(guest);
    server.block_user(guest);
    assert_eq!(
        decide_in(
            &other,
            &server.trust(),
            &second_peer,
            WorkspaceOperation::Subscribe,
            NOW
        ),
        Decision::Deny(DenyReason::Blocked)
    );

    assert_eq!(
        DenyReason::Blocked.refusal(),
        tilo_server_protocol::refusal::Refusal::NotAllowed("this host refuses this device".into())
    );
}

/// A device the record of its person's devices holds the root's revocation for is refused as
/// revoked, as the entry arrives. The root of that record signs nothing about another person's
/// device, so a record holding an entry it did not sign refuses nothing.
#[test]
fn a_device_the_persons_record_holds_a_revocation_for_is_refused() {
    use crate::workspaces::blocked::BlockedKeys;
    use crate::workspaces::person_workspace::devices::Devices;

    let server = Server::new();
    let root = SigningKey::from_bytes(&[0x51; 32]);
    let laptop = signed_member(&root, &SigningKey::from_bytes(&[0x52; 32]), 1_000);
    let stranger = signed_member(
        &SigningKey::from_bytes(&[0x53; 32]),
        &SigningKey::from_bytes(&[0x54; 32]),
        1_000,
    );
    let person = laptop.user_public_key;
    let revoking = |root: &SigningKey, device: [u8; 32]| {
        let mut entry = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: device,
            revoked_at_ns: 1,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, root).expect("the root signs");
        hex::encode(postcard::to_allocvec(&entry).expect("the entry encodes"))
    };
    let mut record = Devices::default();
    for cert in [&laptop, &stranger] {
        record
            .keep_device(
                hex::encode(cert.device_public_key),
                hex::encode(postcard::to_allocvec(cert).expect("the certificate encodes")),
            )
            .expect("the record keeps the device");
    }
    record
        .revoke_device(
            hex::encode(laptop.device_public_key),
            revoking(&root, laptop.device_public_key),
        )
        .expect("the root's entry takes the laptop back");
    assert!(
        record
            .revoke_device(
                hex::encode(stranger.device_public_key),
                revoking(&root, stranger.device_public_key),
            )
            .is_err(),
        "only the root that certified a device revokes it"
    );
    let on = |cert: &DeviceCert| peer(cert.user_public_key, cert.device_public_key, true, None);
    assert_ne!(
        server.trust().cert(&on(&laptop), NOW),
        Err(DenyReason::Revoked),
        "nothing refuses the laptop before the record's entry arrives"
    );

    *server.blocked.write().unwrap() = BlockedKeys::revoking(&record, &person);
    assert_eq!(
        server.trust().cert(&on(&laptop), NOW),
        Err(DenyReason::Revoked),
        "the record's root-signed entry is what refuses the laptop"
    );
    assert!(
        server.revocations.read().unwrap().is_empty(),
        "the record is the one place the revocation is kept"
    );
    assert_ne!(
        server.trust().cert(&on(&stranger), NOW),
        Err(DenyReason::Revoked),
        "an entry naming a device another root certified revokes nothing"
    );
}
