//! The Lounge a released relay kept, opened by this checkout's relay: the
//! upgrade a release of tilo.sh makes. `tools/relay-upgrade/run.sh` builds the
//! release the live relay runs, has it write a Lounge with that release's own
//! harness (`tools/relay-upgrade/<release>/seed.rs`) and runs this on the same
//! state, with `TILO_UPGRADE_DIR` naming it. Nothing here writes that state
//! itself: what it reads is what the release wrote.

use super::*;
use tilo_relay_protocol::workspaces::{Role, WorkspacesReply, WorkspacesRequest};
use tilo_session_client::Event;

/// What the release's seed wrote beside its state, `seed.json`.
#[derive(serde::Deserialize)]
struct Seed {
    release: String,
    workspace: String,
    workspace_id: u64,
    declared_owner: Person,
    declared_moderator: Person,
    members: Vec<Person>,
    stranger: Person,
    listed_members: u32,
    invitation: String,
    chat_text: String,
    own_workspace: Own,
    wait_list: Waiting,
}

/// The address someone left on the Lounge's wait list, when the release kept
/// one: a listed Lounge of v0.1.5 keeps none.
#[derive(serde::Deserialize)]
struct Waiting {
    contact: Option<String>,
}

/// A workspace of their own a member made beside the Lounge, and listed.
#[derive(serde::Deserialize)]
struct Own {
    workspace: String,
    name: String,
    description: String,
    owner: Person,
}

#[derive(serde::Deserialize)]
struct Person {
    seed: u8,
    name: String,
}

impl Person {
    fn device(&self) -> (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert) {
        own_device(self.seed, &self.name)
    }

    fn root(&self) -> [u8; 32] {
        self.device().1.user_public_key
    }
}

/// The Lounge as this checkout's image starts it (its entrypoint's flags,
/// with the door taking people by invitation and keeping a wait list), on
/// the state in `dir`.
fn start_upgraded(dir: &std::path::Path) -> Workspace {
    let state = dir.join("lounge");
    let avatars = state.join("avatars");
    start_with(
        &state,
        &dir.join("root"),
        &state.join("secrets"),
        &[
            "--workspace-name",
            "Lounge",
            "--public-avatars",
            avatars.to_str().unwrap(),
            "--public-avatars-url",
            "https://lounge.example/avatars",
            "--listing-description",
            "The Tilo community: chat, notes and a shared desktop",
            "--default-role",
            "editor",
            "--chat-channel",
            "general",
            "--chat-channel",
            "feedback",
            "--feedback-channel",
            "feedback",
            "--invitation-only",
            "--wait-list-max",
            "500",
            "--web-plain",
            "127.0.0.1:0",
        ],
    )
    .unwrap()
}

/// The room's first answer, or why there was none: a start the state refused
/// ends the process, and says so on its standard error.
fn started_or_why(room: &mut Workspace) -> Started {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Ok(OperatorEvent::Started {
            workspace: Some(workspace),
            plain_origin: Some(plain_origin),
            ..
        }) = room.events.recv_timeout(Duration::from_millis(200))
        {
            return Started {
                id: tilo_relay::room_workspace_id(&workspace).unwrap(),
                plain: bound_address(&plain_origin),
                workspace,
            };
        }
        if let Some(status) = room.child.try_wait().unwrap() {
            panic!("the relay refused the state the release kept and stopped: {status}");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the relay did not start on the state the release kept"
        );
    }
}

/// A page of `person`'s device, which came in before the upgrade, back in
/// the Lounge.
async fn back_in(
    plain: std::net::SocketAddr,
    seed: &Seed,
    person: &Person,
) -> tilo_session_client::Link {
    let (host, host_cert, _) = chat_route(plain, &seed.workspace).await;
    let (key, cert) = person.device();
    let link = page(host, key, cert, &host_cert);
    link.send(Cmd::Join(seed.workspace_id)).unwrap();
    let workspace_id = seed.workspace_id;
    wait_for(
        &format!("{} to be back in the Lounge", person.name),
        || {
            link.drain()
                .iter()
                .any(|event| matches!(event, Event::HostReady { ws, .. } if *ws == workspace_id))
                .then_some(())
        },
        || format!("{:?}", link.status()),
    )
    .await;
    link
}

/// What the host's administration answers `request` from `link`.
async fn administer(
    link: &tilo_session_client::Link,
    request: WorkspacesRequest,
) -> WorkspacesReply {
    let receipt = link.ask_workspaces(request.clone()).unwrap();
    tokio::time::timeout(tilo_test_env::hang_bound(), receipt)
        .await
        .unwrap_or_else(|_| panic!("no answer to {request:?}"))
        .unwrap_or_else(|_| panic!("the session dropped {request:?}"))
        .unwrap_or_else(|refused| panic!("{request:?} refused: {refused}"))
}

/// A person leaves an address at the invitation-only door's wait list.
async fn leave_address(plain: std::net::SocketAddr, workspace: &str, email: &str) {
    use tilo_relay_client::HttpClient as _;
    let (status, body) = Plain(plain)
        .post(
            &format!("/peer/v1/directory/{workspace}/wait-list"),
            serde_json::to_vec(&serde_json::json!({"email": email, "language": "en"})).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        status,
        200,
        "the door kept no address: {}",
        String::from_utf8_lossy(&body)
    );
}

/// The Lounge a release kept starts on this checkout's relay with everything
/// in it: the same workspace, listed as it was and taking people by
/// invitation; its people at their roles; the owner the operator declared
/// lists it, sees the invitation they left open and invites again; a member
/// reads what they wrote; and the door's wait list keeps an address and lists
/// it to the owner.
#[test]
#[ignore = "run by tools/relay-upgrade/run.sh on the state a release kept"]
fn the_lounge_a_release_kept_starts_with_everything_in_it() {
    let dir = std::path::PathBuf::from(
        std::env::var_os("TILO_UPGRADE_DIR").expect("TILO_UPGRADE_DIR names the release's state"),
    );
    let seed: Seed =
        serde_json::from_slice(&std::fs::read(dir.join("seed.json")).unwrap()).unwrap();
    let mut room = start_upgraded(&dir);
    let started = started_or_why(&mut room);
    assert_eq!(
        started.workspace, seed.workspace,
        "the Lounge {} kept is the one that opens",
        seed.release
    );
    assert_eq!(started.id, seed.workspace_id);
    let plain = started.plain;
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        listed_as_it_was(plain, &seed).await;
        // The door takes people by invitation: a stranger is turned away.
        let stranger = seed.stranger.device();
        let refused = enter_through_door(
            plain,
            &seed.workspace,
            (&stranger.0, &stranger.1),
            &seed.stranger.name,
        )
        .await;
        assert!(
            refused.is_err(),
            "a stranger walked into the invitation-only Lounge"
        );
        the_owner_governs_it(plain, &seed).await;
        the_member_keeps_their_own(plain, &seed).await;
        the_member_reads_what_they_wrote(plain, &seed).await;
    });
    room.stop();
}

/// The directory lists the Lounge as it was, with everyone in it, and the
/// workspace a member made of their own as they listed it.
async fn listed_as_it_was(plain: std::net::SocketAddr, seed: &Seed) {
    let listed = directory_page(plain).await;
    let lounge = listed
        .workspaces
        .iter()
        .find(|listed| listed.workspace == seed.workspace)
        .expect("the Lounge is listed");
    assert_eq!(
        lounge.description,
        "The Tilo community: chat, notes and a shared desktop"
    );
    assert_eq!(lounge.default_role, Role::Editor);
    // Everyone the release had: its owner, the owner and moderator the
    // operator declared, and the people who came in through the door. (The
    // release's own directory counted `listed_members` of them.)
    let people = u32::try_from(seed.members.len()).unwrap().saturating_add(3);
    assert!(
        lounge.members >= seed.listed_members,
        "{} counted {}, this relay {}: someone was lost",
        seed.release,
        seed.listed_members,
        lounge.members
    );
    assert_eq!(lounge.members, people, "the Lounge's people");
    let own = listed
        .workspaces
        .iter()
        .find(|listed| listed.workspace == seed.own_workspace.workspace)
        .unwrap_or_else(|| {
            panic!(
                "{}'s own workspace is no longer listed: {:?}",
                seed.own_workspace.owner.name, listed.workspaces
            )
        });
    assert_eq!(own.description, seed.own_workspace.description);
    assert_eq!(own.default_role, Role::Viewer);
}

/// The owner the operator declared lists the Lounge and its people at their
/// roles, sees the invitation they left open, invites again, and reads the
/// wait list: an address left before the upgrade, and one left now.
async fn the_owner_governs_it(plain: std::net::SocketAddr, seed: &Seed) {
    let owner = back_in(plain, seed, &seed.declared_owner).await;
    let WorkspacesReply::Workspaces(workspaces) = administer(&owner, WorkspacesRequest::List).await
    else {
        panic!("no workspaces listed");
    };
    let governed = workspaces
        .iter()
        .find(|workspace| workspace.workspace == seed.workspace)
        .expect("the owner lists the Lounge");
    for member in &governed.members {
        eprintln!(
            "release upgrade: {} {:?}{}{}",
            hex::encode(member.user_public_key),
            member.role,
            if member.owner { ", owner" } else { "" },
            if member.removed { ", removed" } else { "" }
        );
    }
    let role_of = |person: &Person| {
        governed
            .members
            .iter()
            .find(|member| member.user_public_key == person.root() && !member.removed)
            .unwrap_or_else(|| panic!("{} is no longer a member", person.name))
            .role
    };
    role_of(&seed.declared_owner);
    assert_eq!(role_of(&seed.declared_moderator), Role::Moderator);
    for member in &seed.members {
        assert_eq!(role_of(member), Role::Editor, "{}", member.name);
    }

    let WorkspacesReply::Pending(pending) = administer(
        &owner,
        WorkspacesRequest::Pending {
            workspace: seed.workspace.clone(),
        },
    )
    .await
    else {
        panic!("no invitations listed");
    };
    assert!(
        pending
            .iter()
            .any(|invitation| hex::encode(invitation.invite_id) == seed.invitation),
        "the invitation {} left open is gone: {pending:?}",
        seed.release
    );
    let invited = administer(
        &owner,
        WorkspacesRequest::Invite {
            workspace: seed.workspace.clone(),
            request_id: rand::random(),
            role: Role::Editor,
            expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
            inviter_name: Some(seed.declared_owner.name.clone()),
            wait_list_entry: None,
            recipient: None,
        },
    )
    .await;
    assert!(
        matches!(invited, WorkspacesReply::Invitation { .. }),
        "{invited:?}"
    );

    leave_address(plain, &seed.workspace, "grace@example.org").await;
    let WorkspacesReply::WaitList(waiting) = administer(
        &owner,
        WorkspacesRequest::WaitList {
            workspace: seed.workspace.clone(),
        },
    )
    .await
    else {
        panic!("no wait list");
    };
    for contact in seed
        .wait_list
        .contact
        .iter()
        .map(String::as_str)
        .chain(["grace@example.org"])
    {
        assert!(
            waiting.iter().any(|entry| entry.contact == contact),
            "{contact} is not on the wait list: {waiting:?}"
        );
    }
    owner.close();
}

/// The member who made a workspace of their own still has it.
async fn the_member_keeps_their_own(plain: std::net::SocketAddr, seed: &Seed) {
    let maker = back_in(plain, seed, &seed.own_workspace.owner).await;
    let WorkspacesReply::Workspaces(theirs) = administer(&maker, WorkspacesRequest::List).await
    else {
        panic!("no workspaces listed");
    };
    assert!(
        theirs.iter().any(
            |workspace| workspace.workspace == seed.own_workspace.workspace
                && workspace.name == seed.own_workspace.name
        ),
        "{} lost their own workspace: {theirs:?}",
        seed.own_workspace.owner.name
    );
    maker.close();
}

/// A member reads the chat as they wrote it, its channels as it had them.
async fn the_member_reads_what_they_wrote(plain: std::net::SocketAddr, seed: &Seed) {
    let reader = &seed.members[0];
    let (host, host_cert, route) = chat_route(plain, &seed.workspace).await;
    let (key, cert) = reader.device();
    let link = page(host, key, cert, &host_cert);
    link.send(Cmd::Join(seed.workspace_id)).unwrap();
    link.send(Cmd::OpenRoute(route.clone())).unwrap();
    let chat = reads_the_chat(&link, seed.workspace_id, &route).await;
    let channels = {
        use tilo_resource::Resource as _;
        chat::Chat::read(&chat).map(|chat| {
            chat.channels
                .values()
                .map(|channel| channel.name.clone())
                .collect::<Vec<_>>()
        })
    };
    let channels = channels.unwrap_or_else(|error| {
        panic!(
            "the chat {} kept does not read: {error:?}; it holds {:?}",
            seed.release,
            automerge::ReadDoc::keys(&chat, automerge::ROOT).collect::<Vec<_>>()
        )
    });
    for wanted in ["general", "feedback"] {
        assert!(
            channels.iter().any(|name| name == wanted),
            "the chat lost its {wanted} channel: {channels:?}"
        );
    }
    let edited: Option<String> = automerge::ReadDoc::get(&chat, automerge::ROOT, "edited")
        .unwrap()
        .and_then(|(value, _)| value.into_string().ok());
    assert_eq!(
        edited.as_deref(),
        Some(seed.chat_text.as_str()),
        "what {} wrote",
        reader.name
    );
    link.close();
}

/// The chat as `link`'s page holds it once the host let it in and the record
/// landed: what the host sent, read whether or not this build's Chat reads it.
async fn reads_the_chat(
    link: &tilo_session_client::Link,
    workspace_id: u64,
    route: &tilo_peer_state::document_frame::DocumentRoute,
) -> automerge::AutoCommit {
    let mut ready = false;
    let mut doc = None;
    wait_for(
        "the chat to land",
        || {
            for event in link.drain() {
                match &event {
                    Event::HostReady { ws, .. } if *ws == workspace_id => ready = true,
                    Event::Landed { route: landed, .. } if landed == route => {
                        if let Ok(Some(bytes)) =
                            link.engine()
                                .record(&route.cluster_id, &route.app_id, &route.record)
                        {
                            doc = automerge::AutoCommit::load(&bytes).ok();
                        }
                    }
                    _ => {}
                }
            }
            (ready && doc.is_some()).then_some(())
        },
        || format!("{:?}", link.status()),
    )
    .await;
    doc.expect("the chat landed")
}
