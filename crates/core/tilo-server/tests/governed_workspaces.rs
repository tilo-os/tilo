//! A workspace this server governs: another person is invited, joins, edits with
//! the workspace's owner, sees their presence, and is removed again.

#![allow(
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "one end-to-end scenario whose relays and links live until it ends"
)]

use std::future::Future;
use std::time::SystemTime;

use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use collab_types::CommitNamed as _;
use neighbour_wire::NeighbourMsg;
use tilo_peer_state::join::{Role, decode_and_verify_invitation};
use tilo_peer_state::{StatePeer, TransportOrigin};
use tilo_server::workspace_join::WorkspaceJoinClient;
use tilo_server::workspaces::{
    CREATE_WORKSPACE, CreatedWorkspace, ENTER_WORKSPACE, EnteredWorkspace, IssueInvitationRequest,
    IssuedInvitation, RemoveMemberRequest, WorkspaceListing,
};
use tilo_server::{Server, ServerConfig};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{
    KEYS, Linked, TestDevice, TestUser, device_endpoint, opened, sealed_changes, sealed_checkpoint,
    sealed_write,
};
use tilo_server::WorkspaceId;

const APP: &str = "so.tilo.chat";

async fn start(state: &std::path::Path, alice: &TestUser, host: &TestDevice) -> Server {
    start_at(state, alice, host, "127.0.0.1:0".parse().unwrap()).await
}

/// [`start`] with its iroh endpoint at `iroh_listen`: a restarted relay that
/// keeps its address. A host names its address in each workspace's
/// governance when it opens it, so one restarted elsewhere changes that
/// governance to say where it is now.
async fn start_at(
    state: &std::path::Path,
    alice: &TestUser,
    host: &TestDevice,
    iroh_listen: std::net::SocketAddr,
) -> Server {
    if iroh_listen.port() != 0 {
        // The stopped server's connections let go of its socket shortly after
        // its shutdown returns.
        tilo_test_env::wait_for("the stopped server's address to be free", || {
            std::net::UdpSocket::bind(iroh_listen).ok().map(drop)
        });
    }
    crate::common::start_server(
        ServerConfig {
            state_root: state.to_path_buf(),
            iroh_listen,
            ..ServerConfig::default()
        },
        tilo_server::daemon::ServerIdentity {
            user_public_key: alice.user_pk,
            device_signing_key: host.sk.clone(),
            device_cert: host.cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap()
}

/// Alice's `device` creates a workspace named `name` on her relay and enters it
/// as its owner, through the server's own host actions asked of it: before it holds any
/// workspace, the host answers them for a device of its own person.
async fn create_and_enter(server: &Server, device: &TestDevice, name: &str) -> WorkspaceListing {
    use governance_actions::{asking, host_action};
    let home = asking(server, device, "").await;
    let created: CreatedWorkspace = host_action(
        &home,
        device,
        CREATE_WORKSPACE,
        serde_json::json!({ "name": name }),
    )
    .await;
    assert_eq!(created.workspace.name, name);
    let entered: EnteredWorkspace = host_action(
        &home,
        device,
        ENTER_WORKSPACE,
        serde_json::json!({ "workspace": created.workspace.workspace }),
    )
    .await;
    assert!(
        entered.invitation.decode().is_ok(),
        "the device is handed an invitation of its own"
    );
    // The device lists itself as its owner's, signing its own row: the host
    // signs nothing for it.
    assert!(
        server
            .workspaces()
            .unwrap()
            .enter(&created.workspace.workspace, (&device.cert, &device.sk))
            .await
            .unwrap(),
        "the device lists itself as the workspace's owner's"
    );
    created.workspace
}

/// An invitation to `workspace` carrying `role`, for `person` only, that the
/// workspace's owner `issuer` issues.
async fn invite(
    server: &Server,
    issuer: &TestDevice,
    workspace: &str,
    role: Role,
    person: [u8; 32],
) -> IssuedInvitation {
    server
        .workspaces()
        .unwrap()
        .issue(
            workspace,
            &issuer.cert,
            IssueInvitationRequest {
                request_id: *blake3::hash(&person).as_bytes(),
                role,
                recipient_user_public_key: Some(person),
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns()
                    .checked_add(600_000_000_000)
                    .unwrap(),
                inviter_name: None,
            },
        )
        .await
        .unwrap()
}

/// Set `key` to `value` in `peer`'s copy of the record and sign the change, sealed under the
/// members' keys, made by the person whose root key is `person`: the record whole when `peer`
/// holds none of it yet.
fn write(
    peer: &mut StatePeer,
    key: &str,
    value: &str,
    key_pair: &ed25519_dalek::SigningKey,
    person: &[u8; 32],
) -> NeighbourMsg {
    sealed_write(peer, &KEYS, key_pair, (APP, ""), |doc| {
        doc.put(ROOT, key, value).unwrap();
        doc.commit_edit("fixture", key, person);
    })
    .unwrap()
}

fn text(peer: &StatePeer, key: &str) -> Option<String> {
    let doc = AutoCommit::load(&opened(peer, &KEYS, (APP, ""))?).ok()?;
    doc.get(ROOT, key)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
}

fn role(peer: &StatePeer, device: [u8; 32]) -> Option<collab_types::Role> {
    peer.role(&device, SystemTime::now()).ok()
}

/// What every scenario here starts from: Alice's server governing a workspace her
/// laptop owns, Bob's phone admitted through an invitation as `role`, and a link open
/// for each of them, Alice's from what the host holds and Bob's from his join alone.
struct Governed {
    alice: TestUser,
    host: TestDevice,
    laptop: TestDevice,
    bob: TestUser,
    phone: TestDevice,
    state: tempfile::TempDir,
    server: Server,
    workspace: WorkspaceListing,
    alice_link: Linked,
    bob_link: Linked,
}

/// A governed workspace's whole setup. Boxed: the fixture's own future is far larger
/// than the tests that only destructure its result.
fn governed_workspace(granted: Role) -> std::pin::Pin<Box<dyn Future<Output = Governed> + Send>> {
    Box::pin(async move {
        let alice = TestUser::fresh("Alice");
        let bob = TestUser::fresh("Bob");
        let host = alice.enrol("alice-home", DeviceCapabilities::member());
        let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
        let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
        let state = tempfile::tempdir().unwrap();
        let server = start(state.path(), &alice, &host).await;

        // Alice's laptop creates a workspace on her relay and enters it as its owner.
        // (That Bob's device cannot is `governance_actions`'s to show.)
        let workspace = create_and_enter(&server, &laptop, "Design review").await;

        // Alice invites Bob's person; his device redeems through the signed
        // address the invitation carries and is admitted by the workspace's host.
        let invitation = invite(&server, &laptop, &workspace.workspace, granted, bob.user_pk).await;
        let verified = decode_and_verify_invitation(
            &invitation.signed_invitation,
            None,
            &RevocationList::new(),
            tilo_clock::unix_now_ns(),
        )
        .unwrap();
        assert_eq!(
            verified.grant().issuer,
            host.cert,
            "the server answers for the workspace"
        );
        let admission = WorkspaceJoinClient::new(device_endpoint(&phone).await)
            .redeem_hint(
                &verified.grant().transport[0],
                invitation.signed_invitation.clone(),
                phone.cert.clone(),
                &phone.sk,
                [7; 32],
            )
            .await
            .unwrap();
        let bootstrap = admission.bootstrap;
        assert_eq!(bootstrap.workspace_id, workspace.workspace_id);
        assert_eq!(bootstrap.role, granted);

        // Bob's own copy, from the bootstrap alone, makes him an editor.
        let mut bob_copy = StatePeer::new(
            &bootstrap.workspace_id.to_string(),
            bootstrap.host.clone(),
            bootstrap.certified_roster.clone(),
            RevocationList::new(),
            SystemTime::now(),
        )
        .unwrap();
        bob_copy.select_exact_admissions().unwrap();
        for bytes in &bootstrap.governance_messages {
            bob_copy
                .receive_from(
                    TransportOrigin::RetainedServerDelta(bootstrap.host.device_public_key),
                    postcard::from_bytes(bytes).unwrap(),
                    SystemTime::now(),
                )
                .unwrap();
        }
        assert_eq!(
            role(&bob_copy, phone.device_pubkey()),
            Some(granted),
            "the bootstrap alone makes Bob's phone an editor"
        );

        // Bob's link starts from that copy and brings the rest; Alice's holds what the
        // host holds now.
        let bob_link = Linked::open_seeded(&server, &phone, workspace.workspace_id, bob_copy)
            .await
            .expect("the workspace admits Bob's phone");
        let alice_link = Linked::open(&server, &laptop, workspace.workspace_id)
            .await
            .expect("the workspace admits Alice's laptop");
        crate::common::receives(&bob_link, |peer| {
            role(peer, phone.device_pubkey()) == Some(granted)
        })
        .await;
        crate::common::receives(&alice_link, |peer| {
            role(peer, laptop.device_pubkey()) == Some(collab_types::Role::Owner)
                && role(peer, phone.device_pubkey()) == Some(granted)
        })
        .await;
        crate::common::wait_until_subscribed(
            &server,
            workspace.workspace_id,
            &phone.device_pubkey(),
        )
        .await;
        Governed {
            alice,
            host,
            laptop,
            bob,
            phone,
            state,
            server,
            workspace,
            alice_link,
            bob_link,
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_invited_person_joins_a_governed_workspace_edits_sees_presence_and_is_removed() {
    let Governed {
        alice,
        host,
        laptop,
        bob,
        phone,
        state,
        server,
        workspace,
        mut alice_link,
        mut bob_link,
    } = governed_workspace(Role::Editor).await;

    // Alice writes; Bob's copy verifies and applies it.
    alice_link
        .publish(|peer| {
            Ok(write(
                peer,
                "topic",
                "from Alice",
                &laptop.sk,
                &laptop.cert.user_public_key,
            ))
        })
        .expect("Alice's write goes out on her link");
    crate::common::receives(&bob_link, |peer| {
        text(peer, "topic").as_deref() == Some("from Alice")
    })
    .await;

    // Bob writes as an editor; Alice's copy verifies and applies it.
    bob_link
        .publish(|peer| {
            Ok(write(
                peer,
                "reply",
                "from Bob",
                &phone.sk,
                &phone.cert.user_public_key,
            ))
        })
        .expect("Bob's write goes out on his link");
    crate::common::receives(&alice_link, |peer| {
        text(peer, "reply").as_deref() == Some("from Bob")
    })
    .await;

    // A conversation: each write depends on the other person's last one.
    for turn in 0..3 {
        let said = format!("Alice {turn}");
        alice_link
            .publish(|peer| {
                Ok(write(
                    peer,
                    "turn",
                    &said,
                    &laptop.sk,
                    &laptop.cert.user_public_key,
                ))
            })
            .expect("Alice's turn goes out on her link");
        crate::common::receives(&bob_link, |peer| {
            text(peer, "turn").as_deref() == Some(said.as_str())
        })
        .await;
        let said = format!("Bob {turn}");
        bob_link
            .publish(|peer| {
                Ok(write(
                    peer,
                    "turn",
                    &said,
                    &phone.sk,
                    &phone.cert.user_public_key,
                ))
            })
            .expect("Bob's turn goes out on his link");
        crate::common::receives(&alice_link, |peer| {
            text(peer, "turn").as_deref() == Some(said.as_str())
        })
        .await;
    }

    // Each sees the other's presence: the frame the other device sent, under its own
    // name, on the link the workspace forwards.
    alice_link
        .send(NeighbourMsg::LivePresence {
            peer: tilo_peer_state::peer_handle(&laptop.device_pubkey()),
            frame: b"alice is here".to_vec(),
        })
        .expect("Alice's presence goes out on her link");
    bob_link
        .send(NeighbourMsg::LivePresence {
            peer: tilo_peer_state::peer_handle(&phone.device_pubkey()),
            frame: b"bob is here".to_vec(),
        })
        .expect("Bob's presence goes out on his link");
    let seen = crate::common::next(&mut bob_link, |heard| {
        matches!(
            &heard.message,
            NeighbourMsg::LivePresence { frame, .. }
                if frame.as_slice() == b"alice is here".as_slice()
        )
    })
    .await;
    assert_eq!(
        seen.author,
        laptop.device_pubkey(),
        "Bob hears Alice's own presence"
    );
    let seen = crate::common::next(&mut alice_link, |heard| {
        matches!(
            &heard.message,
            NeighbourMsg::LivePresence { frame, .. }
                if frame.as_slice() == b"bob is here".as_slice()
        )
    })
    .await;
    assert_eq!(
        seen.author,
        phone.device_pubkey(),
        "Alice hears Bob's own presence"
    );

    // Alice removes Bob: the server stops admitting his device.
    let generation = server
        .workspaces()
        .unwrap()
        .remove_member(
            &workspace.workspace,
            &laptop.cert,
            RemoveMemberRequest {
                request_id: [9; 32],
                user_public_key: bob.user_pk,
            },
        )
        .await
        .unwrap()
        .generation;
    assert!(generation > workspace.generation);
    assert!(
        bob_link
            .reopen(&server, workspace.workspace_id)
            .await
            .is_err(),
        "a removed member is not admitted"
    );

    // The workspace survives a restart of its server, governance and all: every change its people
    // made is still there.
    let iroh_listen = *server.node_addr().ip_addrs().next().unwrap();
    drop(alice_link);
    drop(bob_link);
    server.shutdown().await;
    let server = start_at(state.path(), &alice, &host, iroh_listen).await;
    let workspaces = server.workspaces().unwrap().list().await;
    let [restored] = workspaces.as_slice() else {
        panic!("one governed workspace: {workspaces:?}")
    };
    assert_eq!(restored.workspace, workspace.workspace);
    // A host names its own address in a workspace's governance when it opens it
    // (`Workspaces::hosts`), out of the address its endpoint reports — which names the relay
    // it is reached through once it has reached it ([`iroh::Endpoint::addr`]). The host waits
    // for that relay before it opens anything, so the restart names the address it had, and
    // no change its people made goes.
    assert!(
        restored.generation >= generation,
        "a restart lost governance: {} is below {generation}",
        restored.generation
    );
    assert!(
        restored
            .members
            .iter()
            .any(|member| member.root() == Some(bob.user_pk) && member.removed),
        "{:?}",
        restored.members
    );
    server.shutdown().await;
}

/// A host restarted with a workspace already on disk names the relay it is reached through
/// before it answers anything, so the first invitation it mints after start carries an
/// address a stranger can dial; a restart at the same address changes no governance.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restarted_host_names_its_server_before_its_first_invitation() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let server = start(state.path(), &alice, &host).await;
    let workspace = create_and_enter(&server, &laptop, "Design review").await;
    let generation = server.workspaces().unwrap().list().await[0].generation;
    assert!(
        server.node_addr().relay_urls().next().is_some(),
        "a started host names the relay it is reached through"
    );
    let iroh_listen = *server.node_addr().ip_addrs().next().unwrap();
    server.shutdown().await;

    let server = start_at(state.path(), &alice, &host, iroh_listen).await;
    // What a person does as soon as their host is up again: hand out a link.
    let invited = invite(
        &server,
        &laptop,
        &workspace.workspace,
        Role::Editor,
        bob.user_pk,
    )
    .await;
    let hint = tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1::from_transport_hint(
        &decode_and_verify_invitation(
            &invited.signed_invitation,
            None,
            &RevocationList::new(),
            tilo_clock::unix_now_ns(),
        )
        .unwrap()
        .grant()
        .transport[0],
    )
    .unwrap();
    let relay_url = server
        .node_addr()
        .relay_urls()
        .next()
        .cloned()
        .expect("the relay is reached through its packet relay");
    assert!(
        hint.endpoint_addr
            .relay_urls()
            .any(|named| *named == relay_url),
        "an invitation minted right after start names the relay: {:?}",
        hint.endpoint_addr
    );
    let restored = server.workspaces().unwrap().list().await;
    assert_eq!(
        restored[0].generation, generation,
        "a restart at the same address names the address it had, changing nothing"
    );
    server.shutdown().await;
}

/// The server keeps and forwards only changes that name an action their app
/// publishes, decided against the app's surface the op names; a change
/// claiming anything else never reaches the workspace.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_server_forwards_only_changes_their_app_surface_publishes() {
    const SURFACE: &str = r#"
[[resources]]
kind = "fixture"
label = "Fixture"
version = 1

[[resources.fields]]
owner = "fixture"
path = "topic"
stored = "value"
title = "Topic"
type = "String"
"#;
    // The surfaces it installs are the process's: no other test runs here.
    if !crate::common::in_own_process(
        "governed_workspaces::the_server_forwards_only_changes_their_app_surface_publishes",
    ) {
        return;
    }
    let kept = tilo_surface_compile::compile(SURFACE).expect("the fixture is a surface");
    let hash = *blake3::hash(kept.as_bytes()).as_bytes();
    let hex = hex::encode(hash);
    crate::common::install_process_surfaces(
        collab_types::Surfaces::from_source(std::sync::Arc::new(
            collab_types::surface::ContentSurfaces::new(move |_, wanted| {
                (wanted == hex).then(|| kept.clone().into_bytes())
            }),
        )),
        move |_| Some(hash),
    );
    let Governed {
        // The server's storage: kept alive for the whole test.
        state: _state,
        laptop,
        server,
        alice_link,
        bob_link,
        ..
    } = governed_workspace(Role::Editor).await;

    // A change claiming a verb the app does not publish: refused by the
    // server, so it never reaches Bob (whose copy would refuse it loudly).
    // It goes first on the one link, so Bob would see it first.
    let context = tilo_actions::ActionContext {
        what: Some(tilo_actions::Did::Verb(tilo_actions::VerbCall::new(
            tilo_actions::ActionKey::new(APP, "fixture", "explode"),
            [],
        ))),
        ..tilo_actions::ActionContext::for_resource("fixture", "fixture", "Fixture")
    };
    // Signed by a device that does not check its own changes.
    let smuggled = alice_link
        .read(|peer| {
            let mut unchecked = peer.clone();
            unchecked.set_surfaces(collab_types::Surfaces::default());
            sealed_write(&mut unchecked, &KEYS, &laptop.sk, (APP, ""), |doc| {
                doc.put(ROOT, "smuggled", "from Alice").unwrap();
                doc.commit_with(
                    automerge::transaction::CommitOptions::default()
                        .with_message(context.change_message().unwrap()),
                );
            })
        })
        .unwrap();
    alice_link
        .send(smuggled)
        .expect("Alice's unchecked change goes out on her link");
    // An edit of a field the app publishes reaches Bob.
    alice_link
        .publish(|peer| {
            Ok(write(
                peer,
                "topic",
                "from Alice",
                &laptop.sk,
                &laptop.cert.user_public_key,
            ))
        })
        .unwrap();
    crate::common::receives(&bob_link, |peer| {
        text(peer, "topic").is_some() || text(peer, "smuggled").is_some()
    })
    .await;
    assert_eq!(
        bob_link.read(|peer| text(peer, "smuggled")),
        None,
        "the server forwarded a refused change"
    );
    server.shutdown().await;
}
/// The server decides each change against its author's grants, as the workspace's
/// host projects them: a commenter's comment reaches the workspace, their
/// edit of the record does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_server_refuses_an_action_the_author_is_not_granted() {
    const SURFACE: &str = r#"
[[resources]]
kind = "fixture"
label = "Fixture"
version = 1

[[resources.fields]]
owner = "fixture"
path = "topic"
stored = "value"
title = "Topic"
type = "String"

[[resources.verbs]]
id = "comment"
kind = "undoable"
label = "Comment"
field = "comments"
params = []
"#;
    // The surfaces it installs are the process's: no other test runs here.
    if !crate::common::in_own_process(
        "governed_workspaces::the_server_refuses_an_action_the_author_is_not_granted",
    ) {
        return;
    }
    let kept = tilo_surface_compile::compile(SURFACE).expect("the fixture is a surface");
    let hash = *blake3::hash(kept.as_bytes()).as_bytes();
    let hex = hex::encode(hash);
    crate::common::install_process_surfaces(
        collab_types::Surfaces::from_source(std::sync::Arc::new(
            collab_types::surface::ContentSurfaces::new(move |_, wanted| {
                (wanted == hex).then(|| kept.clone().into_bytes())
            }),
        )),
        move |_| Some(hash),
    );
    let Governed {
        // The server's storage: kept alive for the whole test.
        state: _state,
        laptop,
        phone,
        server,
        workspace,
        alice_link,
        bob_link,
        ..
    } = governed_workspace(Role::Commenter).await;

    // Alice's chat: the record exists before Bob writes to it.
    alice_link
        .publish(|peer| {
            Ok(write(
                peer,
                "topic",
                "from Alice",
                &laptop.sk,
                &laptop.cert.user_public_key,
            ))
        })
        .unwrap();
    crate::common::receives(&bob_link, |peer| text(peer, "topic").is_some()).await;
    // Signed as Bob's device signs, without his own copy checking it.
    let sign = |doc: &mut AutoCommit, seq: u64| {
        let mutation = collab_types::DocumentMutation::Incremental(doc.save_after(&[]));
        let route = (workspace.workspace.as_str(), APP, "");
        crate::common::signed_by_hand(&phone.sk, route, &mutation, seq, Some(hash))
    };
    // An edit of the topic: published by the app, not granted to a commenter.
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "topic", "from Bob").unwrap();
    doc.commit_edit("fixture", "topic", &phone.cert.user_public_key);
    bob_link
        .send(sign(&mut doc, 1))
        .expect("Bob's edit goes out on his link");
    // A comment: theirs to make. The refused edit took no number, so the
    // comment is Bob's first write too.
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "comment", "looks good").unwrap();
    let context = tilo_actions::ActionContext {
        what: Some(tilo_actions::Did::Verb(tilo_actions::VerbCall::new(
            tilo_actions::ActionKey::new(APP, "fixture", "comment"),
            [],
        ))),
        ..tilo_actions::ActionContext::for_resource("fixture", "fixture", "Fixture")
    };
    doc.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(context.change_message().unwrap()),
    );
    bob_link
        .send(sign(&mut doc, 1))
        .expect("Bob's comment goes out on his link");
    crate::common::receives(&alice_link, |peer| text(peer, "comment").is_some()).await;
    assert_eq!(
        alice_link.read(|peer| text(peer, "topic")).as_deref(),
        Some("from Alice"),
        "the server forwarded an ungranted edit"
    );
    server.shutdown().await;
}

/// A public workspace prunes the guests who left in one governance change, and
/// what it kept about how they joined goes with them: their invitations
/// admit nobody again, and the guest who stayed is still a member.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pruning_removes_idle_guests_in_one_change_and_their_invitations_admit_nobody() {
    let alice = TestUser::fresh("Alice");
    let host = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let server = start(state.path(), &alice, &host).await;
    let workspace = create_and_enter(&server, &laptop, "Demo").await;
    let mut guests = Vec::new();
    for (seed, name) in [(17, "Ada"), (18, "Bea"), (19, "Cy")] {
        let guest = TestUser::fresh(name);
        let phone = guest.enrol("phone", DeviceCapabilities::member());
        let invitation = invite(
            &server,
            &laptop,
            &workspace.workspace,
            Role::Editor,
            guest.user_pk,
        )
        .await;
        let verified = decode_and_verify_invitation(
            &invitation.signed_invitation,
            None,
            &RevocationList::new(),
            tilo_clock::unix_now_ns(),
        )
        .unwrap();
        WorkspaceJoinClient::new(device_endpoint(&phone).await)
            .redeem_hint(
                &verified.grant().transport[0],
                invitation.signed_invitation.clone(),
                phone.cert.clone(),
                &phone.sk,
                [seed; 32],
            )
            .await
            .unwrap();
        guests.push((guest, phone, verified, invitation));
    }
    let workspaces = server.workspaces().unwrap();
    let before = workspaces
        .summary(&workspace.workspace)
        .await
        .unwrap()
        .generation;
    let idle = std::collections::BTreeSet::from([guests[0].0.user_pk, guests[1].0.user_pk]);
    let pruned = workspaces
        .prune_members(&workspace.workspace, &host.cert, [9; 32], &idle)
        .await
        .unwrap();
    let step = pruned.generation - before;

    let members = workspaces
        .summary(&workspace.workspace)
        .await
        .unwrap()
        .members;
    let is_member = |person: [u8; 32]| {
        members
            .iter()
            .any(|member| member.root() == Some(person) && !member.removed)
    };
    assert!(!is_member(guests[0].0.user_pk) && !is_member(guests[1].0.user_pk));
    assert!(
        is_member(guests[2].0.user_pk),
        "the guest who stayed is still a member"
    );

    let (_, phone, verified, invitation) = &guests[0];
    assert!(
        WorkspaceJoinClient::new(device_endpoint(phone).await)
            .redeem_hint(
                &verified.grant().transport[0],
                invitation.signed_invitation.clone(),
                phone.cert.clone(),
                &phone.sk,
                [8; 32],
            )
            .await
            .is_err(),
        "a pruned guest's invitation admits nobody again"
    );
    // Pruning two guests is one change of the member list, as pruning one is.
    let last = std::collections::BTreeSet::from([guests[2].0.user_pk]);
    let after = workspaces
        .summary(&workspace.workspace)
        .await
        .unwrap()
        .generation;
    let pruned = workspaces
        .prune_members(&workspace.workspace, &host.cert, [10; 32], &last)
        .await
        .unwrap();
    assert_eq!(
        pruned.generation - after,
        step,
        "one change for every guest"
    );
    server.shutdown().await;
}

#[path = "governed_workspaces/members.rs"]
mod members;

#[path = "governed_workspaces/governance_actions.rs"]
mod governance_actions;

#[path = "governed_workspaces/app_records.rs"]
mod app_records;

#[path = "governed_workspaces/sealed_documents.rs"]
mod sealed_documents;

#[path = "governed_workspaces/shared_desktop.rs"]
mod shared_desktop;

#[path = "governed_workspaces/revocations.rs"]
mod revocations;

#[path = "governed_workspaces/copies_read.rs"]
mod copies_read;

#[path = "governed_workspaces/hard_stops.rs"]
mod hard_stops;

#[path = "governed_workspaces/app_push.rs"]
mod app_push;

#[path = "governed_workspaces/record_content.rs"]
mod record_content;
