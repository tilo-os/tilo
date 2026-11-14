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
    declared_owner: Person,
    declared_moderator: Person,
    members: Vec<Person>,
    stranger: Person,
    listed_members: u32,
    invitation: String,
    /// When that invitation lapses, as the release set it.
    invitation_expires_at_ns: u64,
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

/// The workspace's id, as the release's own `seed.json` names it.
fn workspace_id(seed: &Seed) -> tilo_relay_protocol::WorkspaceId {
    tilo_relay_protocol::WorkspaceId::parse(&seed.workspace).expect("the seed names a workspace")
}

impl Person {
    fn device(&self) -> (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert) {
        own_device(self.seed, &self.name)
    }

    fn root(&self) -> [u8; 32] {
        self.device().1.user_public_key
    }
}

/// What the upgraded relay logged, line by line, as it logged it: passed on
/// to this test's own output, and kept to read.
#[derive(Clone, Default)]
struct RelayLog(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl RelayLog {
    /// Pass `stderr` on, keeping each line.
    fn tee(&self, stderr: std::process::ChildStderr) {
        let kept = self.0.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("{line}");
                kept.lock().unwrap().push(strip_ansi(&line));
            }
        });
    }

    fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

/// `line` without its terminal colours.
fn strip_ansi(line: &str) -> String {
    let mut plain = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            plain.push(c);
        }
    }
    plain
}

/// The Lounge as this checkout's image starts it (its entrypoint's flags,
/// with the door taking people by invitation and keeping a wait list), on
/// the state in `dir`, its log in `log`: at least what an operator's relay
/// logs of a governance brought up from an earlier release's.
fn start_upgraded(dir: &std::path::Path, log: &RelayLog) -> Workspace {
    let state = dir.join("lounge");
    let avatars = state.join("avatars");
    let filter = format!(
        "{},tilo_peer_state::state::rebase=info",
        std::env::var("TILO_UPGRADE_RELAY_LOG").unwrap_or_else(|_| "warn".to_owned())
    );
    let mut room = spawn_room(
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
        &filter,
        std::process::Stdio::piped(),
    )
    .unwrap();
    log.tee(room.child.stderr.take().unwrap());
    room
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
    let workspace_id = workspace_id(seed);
    let events = link.events();
    link.send(Cmd::Join(workspace_id)).unwrap();
    wait_for(
        &format!("{} to be back in the Lounge", person.name),
        || {
            events
                .drain()
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
    let log = RelayLog::default();
    let mut room = start_upgraded(&dir, &log);
    let started = started_or_why(&mut room);
    every_grant_is_kept_or_needs_none(&log);
    assert_eq!(
        started.workspace, seed.workspace,
        "the Lounge {} kept is the one that opens",
        seed.release
    );
    assert_eq!(started.workspace, seed.workspace);
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
        the_apps_the_release_granted_still_act(plain, &seed).await;
    });
    room.stop();
}

/// The grants the release's apps held (`app-author:<app>`, `session-app`,
/// `files-app`, `device-panel`) are held as no grant here, where an app
/// acts as the person whose session runs it. The relay names each as it
/// brings the governance up, with why its use needs none; any other grant
/// the release kept that it holds none for, or a row it could not read,
/// fails the upgrade.
fn every_grant_is_kept_or_needs_none(log: &RelayLog) {
    let lines = log.lines();
    let named = |grant: &str| {
        lines.iter().any(|line| {
            line.contains("an earlier release's grant needs no grant here")
                && line
                    .split_whitespace()
                    .any(|field| field == format!("grant={grant}"))
        })
    };
    for grant in [
        "session-app",
        "files-app",
        "device-panel",
        "app-author:so.tilo.chat",
        "app-author:so.tilo.notes",
        "app-author:so.tilo.file-explorer",
    ] {
        assert!(
            named(grant),
            "the relay did not say what became of the release's grant {grant}"
        );
    }
    let lost: Vec<&String> = lines
        .iter()
        .filter(|line| {
            line.contains("an earlier release's grant is not kept")
                || line.contains("a membership row an earlier release kept does not read")
        })
        .collect();
    assert!(
        lost.is_empty(),
        "grants the release kept are lost: {lost:#?}"
    );
    let carried = lines
        .iter()
        .filter(|line| line.contains("an earlier release's grant needs no grant here"))
        .count();
    eprintln!("release upgrade: {carried} grants the release gave its apps need none now");
}

/// What a member writes into an app's record after the upgrade.
const WRITTEN_THROUGH: &str = "written through the app after the upgrade";

/// What the release's app grants let apps do, done after the upgrade:
/// - `app-author:so.tilo.notes`: a member's Notes writes its record, and
///   another member reads it;
/// - `files-app`: a member's Files writes the workspace's files record, read
///   as a tree by another member;
/// - `session-app` and `device-panel`: on a member's device, under the
///   governance it holds from the host, the desktop session's own app may
///   change what the member may (their settings, windows), and quick settings
///   may read what the session publishes for them. Those happen in the
///   member's own session, which this relay does not run, so the device's own
///   decision on them is asked here.
async fn the_apps_the_release_granted_still_act(plain: std::net::SocketAddr, seed: &Seed) {
    use tilo_action_policy::roles::{DEVICE_PANEL, SESSION_APP, SESSION_KIND};
    use tilo_action_policy::{Acting, ActionKind, Answer, Authority, Context, TreePath};
    let (_, _, chat) = chat_route(plain, &seed.workspace).await;
    let catalogue = tilo_agent_participant::AppCatalogue::native();
    let notes = tilo_peer_state::document_frame::DocumentRoute {
        app_id: "so.tilo.notes".into(),
        cluster_id: chat.cluster_id.clone(),
        record: tilo_relay::room_bots::workspace_record(&catalogue, "so.tilo.notes"),
    };
    let files = tilo_peer_state::document_frame::DocumentRoute {
        app_id: fs_protocol::tree::FILES_APP.into(),
        cluster_id: chat.cluster_id.clone(),
        record: collab_types::person_record_id(fs_protocol::tree::TREE_KIND),
    };
    let writer = &seed.members[0];
    let reader = &seed.members[1];
    for route in [&notes, &files] {
        writes_through(plain, seed, writer, route).await;
        let link = reads_through(plain, seed, reader, route).await;
        link.close();
    }

    let link = back_in(plain, seed, writer).await;
    let policy = link
        .engine()
        .policy(&seed.workspace)
        .expect("the member's device holds the Lounge's governance");
    link.close();
    let person = hex::encode(writer.root());
    let decide = |app: &str, node: &TreePath, kind: ActionKind| {
        let acting = Acting::person(person.clone()).in_app(app);
        Authority::of_acting(&acting, &policy, &[])
            .decide(
                node,
                kind,
                &Context::at(tilo_clock::unix_now_ns() / 1_000_000),
            )
            .answer
    };
    let session = TreePath::workspace(policy.workspace.clone())
        .app(SESSION_APP)
        .kind(SESSION_KIND);
    assert_eq!(
        decide(SESSION_APP, &session, ActionKind::Undoable),
        Answer::Allow,
        "the desktop session's own app no longer acts for {}",
        writer.name
    );
    assert_eq!(
        decide(DEVICE_PANEL, &session, ActionKind::Read),
        Answer::Allow,
        "quick settings no longer reads {}'s session",
        writer.name
    );
}

/// `writer`'s page writes [`WRITTEN_THROUGH`] into the record `route`, as the
/// app the record is of does, and the host commits it.
async fn writes_through(
    plain: std::net::SocketAddr,
    seed: &Seed,
    writer: &Person,
    route: &tilo_peer_state::document_frame::DocumentRoute,
) {
    let link = back_in(plain, seed, writer).await;
    link.send(Cmd::OpenRoute(route.clone())).unwrap();
    // A record the Lounge holds no copy of yet, the member's app starts.
    let mut doc = link
        .engine()
        .record(&route.cluster_id, &route.app_id, &route.record)
        .ok()
        .flatten()
        .map_or_else(automerge::AutoCommit::new, |held| {
            automerge::AutoCommit::load(&held).unwrap()
        });
    let before = doc.get_heads();
    automerge::transaction::Transactable::put(
        &mut doc,
        automerge::ROOT,
        "written_through",
        WRITTEN_THROUGH,
    )
    .unwrap();
    doc.commit();
    let mutation = if before.is_empty() {
        collab_types::DocumentMutation::Snapshot(doc.save())
    } else {
        collab_types::DocumentMutation::Incremental(doc.save_after(&before))
    };
    link.write(route.clone(), &mutation).unwrap();
    let saved = std::sync::Mutex::new(link.barrier().unwrap());
    let refused = wait_for(
        &format!(
            "{}'s write to {} to be committed",
            writer.name, route.app_id
        ),
        || {
            let committed = matches!(saved.lock().unwrap().try_recv(), Ok(Ok(())));
            committed.then(|| link.status().failures)
        },
        || format!("{:?}", link.status()),
    )
    .await;
    assert!(
        refused.is_empty(),
        "{}'s write to {} was refused: {refused:?}",
        writer.name,
        route.app_id
    );
    link.close();
}

/// `reader`'s page once it read [`WRITTEN_THROUGH`] in the record `route`; a
/// files record read as the tree it is.
async fn reads_through(
    plain: std::net::SocketAddr,
    seed: &Seed,
    reader: &Person,
    route: &tilo_peer_state::document_frame::DocumentRoute,
) -> tilo_session_client::Link {
    let (host, host_cert, _) = chat_route(plain, &seed.workspace).await;
    let (key, cert) = reader.device();
    let link = page(host, key, cert, &host_cert);
    let workspace_id = workspace_id(seed);
    let events = link.events();
    link.send(Cmd::Join(workspace_id)).unwrap();
    link.send(Cmd::OpenRoute(route.clone())).unwrap();
    let mut doc = reads_the_chat(&link, &events, workspace_id, route, |doc| {
        automerge::ReadDoc::get(doc, automerge::ROOT, "written_through")
            .ok()
            .flatten()
            .and_then(|(value, _)| value.into_string().ok())
            .as_deref()
            == Some(WRITTEN_THROUGH)
    })
    .await;
    if route.app_id == fs_protocol::tree::FILES_APP {
        fs_protocol::tree::Tree::of_record(&doc.save()).unwrap_or_else(|error| {
            panic!(
                "the files record {} read is no tree: {error:?}",
                reader.name
            )
        });
    }
    link
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
    let reply = administer(&owner, WorkspacesRequest::List).await;
    let WorkspacesReply::Workspaces(workspaces) = reply else {
        panic!("no workspaces listed: {reply:?}");
    };
    let governed = workspaces
        .iter()
        .find(|workspace| workspace.workspace == seed.workspace)
        .expect("the owner lists the Lounge");
    for member in &governed.members {
        eprintln!(
            "release upgrade: {} {:?}{}",
            member.id.key(),
            member.role,
            if member.removed { ", removed" } else { "" }
        );
    }
    let role_of = |person: &Person| {
        governed
            .members
            .iter()
            .find(|member| member.root() == Some(person.root()) && !member.removed)
            .unwrap_or_else(|| panic!("{} is no longer a member", person.name))
            .role
    };
    role_of(&seed.declared_owner);
    assert_eq!(role_of(&seed.declared_moderator), Some(Role::Moderator));
    for member in &seed.members {
        assert_eq!(role_of(member), Some(Role::Editor), "{}", member.name);
    }

    the_open_invitation_follows_its_lifetime(&owner, seed).await;
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
    let reply = administer(
        &owner,
        WorkspacesRequest::WaitList {
            workspace: seed.workspace.clone(),
        },
    )
    .await;
    let WorkspacesReply::WaitList(waiting) = reply else {
        panic!("no wait list: {reply:?}");
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

/// The invitation the release left open is listed as long as it can be
/// redeemed, at the lifetime the release gave it, and no longer once it
/// lapsed.
async fn the_open_invitation_follows_its_lifetime(owner: &tilo_session_client::Link, seed: &Seed) {
    let reply = administer(
        owner,
        WorkspacesRequest::Pending {
            workspace: seed.workspace.clone(),
        },
    )
    .await;
    let WorkspacesReply::Pending(pending) = reply else {
        panic!("no invitations listed: {reply:?}");
    };
    let kept = pending
        .iter()
        .find(|invitation| hex::encode(invitation.invite_id) == seed.invitation);
    if tilo_clock::unix_now_ns() < seed.invitation_expires_at_ns {
        let kept = kept.unwrap_or_else(|| {
            panic!(
                "the invitation {} left open is gone: {pending:?}",
                seed.release
            )
        });
        assert_eq!(kept.expires_at_ns, seed.invitation_expires_at_ns);
    } else {
        assert!(
            kept.is_none(),
            "the invitation {} left open lapsed and is still listed: {pending:?}",
            seed.release
        );
    }
}

/// The member who made a workspace of their own still has it.
async fn the_member_keeps_their_own(plain: std::net::SocketAddr, seed: &Seed) {
    let maker = back_in(plain, seed, &seed.own_workspace.owner).await;
    let reply = administer(&maker, WorkspacesRequest::List).await;
    let WorkspacesReply::Workspaces(theirs) = reply else {
        panic!("no workspaces listed: {reply:?}");
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

/// What a member writes into the chat after the upgrade.
const WRITTEN_AFTER: &str = "written after the upgrade";

/// A member reads the chat as they wrote it, its channels as it had them,
/// and writes in it again; another member reads both.
async fn the_member_reads_what_they_wrote(plain: std::net::SocketAddr, seed: &Seed) {
    let link = reads_what_was_written(plain, seed, &seed.members[0], None).await;
    let (_, _, route) = chat_route(plain, &seed.workspace).await;
    let landed = link
        .engine()
        .record(&route.cluster_id, &route.app_id, &route.record)
        .unwrap()
        .expect("the chat as the member holds it");
    let mut chat = automerge::AutoCommit::load(&landed).unwrap();
    let before = chat.get_heads();
    automerge::transaction::Transactable::put(
        &mut chat,
        automerge::ROOT,
        "written_after",
        WRITTEN_AFTER,
    )
    .unwrap();
    chat.commit();
    link.write(
        route,
        &collab_types::DocumentMutation::Incremental(chat.save_after(&before)),
    )
    .unwrap();
    let saved = std::sync::Mutex::new(link.barrier().unwrap());
    let refused = wait_for(
        "the member's new message to be committed",
        || {
            let committed = matches!(saved.lock().unwrap().try_recv(), Ok(Ok(())));
            committed.then(|| link.status().failures)
        },
        || format!("{:?}", link.status()),
    )
    .await;
    assert!(
        refused.is_empty(),
        "the member's new message was refused: {refused:?}"
    );
    link.close();

    let other = reads_what_was_written(plain, seed, &seed.members[1], Some(WRITTEN_AFTER)).await;
    other.close();
}

/// `reader`'s page in the Lounge once it read the chat: what was written
/// before the upgrade, its channels, and `after`, what was written after it.
async fn reads_what_was_written(
    plain: std::net::SocketAddr,
    seed: &Seed,
    reader: &Person,
    after: Option<&str>,
) -> tilo_session_client::Link {
    let (host, host_cert, route) = chat_route(plain, &seed.workspace).await;
    let (key, cert) = reader.device();
    let link = page(host, key, cert, &host_cert);
    let workspace_id = workspace_id(seed);
    let events = link.events();
    link.send(Cmd::Join(workspace_id)).unwrap();
    link.send(Cmd::OpenRoute(route.clone())).unwrap();
    let chat = reads_the_chat(&link, &events, workspace_id, &route, |chat| {
        written_after(chat).as_deref() == after
    })
    .await;
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
        "{} reads what was written before the upgrade",
        reader.name
    );
    assert_eq!(
        written_after(&chat).as_deref(),
        after,
        "{} reads what was written after the upgrade",
        reader.name
    );
    link
}

/// What a member wrote into the chat after the upgrade, as `chat` holds it.
fn written_after(chat: &automerge::AutoCommit) -> Option<String> {
    automerge::ReadDoc::get(chat, automerge::ROOT, "written_after")
        .unwrap()
        .and_then(|(value, _)| value.into_string().ok())
}

/// The chat as `link`'s page holds it once the host let it in and the record
/// landed `whole`: what the host sent, read whether or not this build's Chat
/// reads it.
async fn reads_the_chat(
    link: &tilo_session_client::Link,
    events: &tilo_session_client::Events,
    workspace_id: tilo_relay_protocol::WorkspaceId,
    route: &tilo_peer_state::document_frame::DocumentRoute,
    whole: impl Fn(&automerge::AutoCommit) -> bool + Sync,
) -> automerge::AutoCommit {
    let mut ready = false;
    let mut doc = None;
    wait_for(
        "the chat to land",
        || {
            for event in events.drain() {
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
            (ready && doc.as_ref().is_some_and(&whole)).then_some(())
        },
        || format!("{:?}", link.status()),
    )
    .await;
    doc.expect("the chat landed")
}
