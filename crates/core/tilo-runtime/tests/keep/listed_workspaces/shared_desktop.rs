//! A workspace's shared desktop through a real server, written sealed as every member writes it:
//! its members close and move any of its windows, whoever opened them, but rename no strip, which
//! a moderator the workspace made does. A rename is moderation its change claims, so the host
//! refuses a member's rename by its writer's grants without reading it, and a member's device
//! that a faulty host hands one refuses it the same way.

use std::time::SystemTime;

use automerge::AutoCommit;
use tilo_desktop_model::layout::{Strip, Tab, TileContent, WindowGroup};
use tilo_desktop_model::ownership::{Authors, DESKTOP_APP, DESKTOP_DOCUMENT, SharedDesktop};
use tilo_desktop_model::{Layout, StripId, WindowWidth, Workspace, WorkspaceId, shared_resource};
use tilo_peer_state::direct::Kept;

use super::*;

/// The desktop's route.
const DESKTOP: (&str, &str) = (DESKTOP_APP, DESKTOP_DOCUMENT);

/// The windows of a one-strip desktop: `(tab, group, who opened it)`, in
/// order, each tab in its own group unless two share one.
fn desktop(windows: &[(u64, u64, String)]) -> SharedDesktop {
    let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Lounge");
    let mut strip = Strip::new(StripId(10), "Lounge");
    for (tab, group, _) in windows {
        let entry = Tab {
            id: *tab,
            title: format!("Notes {tab}"),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.notes".into(),
                label: "Notes".into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        match strip.window_groups.iter_mut().find(|g| g.id == *group) {
            Some(existing) => existing.tabs.push(entry),
            None => strip.window_groups.push(WindowGroup {
                id: *group,
                tabs: vec![entry],
                active_tab: *tab,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            }),
        }
    }
    workspace.strips.push(strip);
    let tree = Layout {
        active: workspace.id(),
        workspaces: vec![workspace],
        ..Layout::default()
    };
    let authors: Authors = windows
        .iter()
        .map(|(tab, _, who)| (*tab, who.clone()))
        .collect();
    SharedDesktop {
        tree,
        authors,
        records: Vec::new(),
        items: Vec::new(),
        opened_by: std::collections::BTreeMap::new(),
    }
}

/// `desktop` with its strip named `name`.
fn named(mut desktop: SharedDesktop, name: &str) -> SharedDesktop {
    name.clone_into(&mut desktop.tree.workspaces[0].strips[0].name);
    desktop
}

/// The person id a workspace knows `user` by.
fn person(user: &TestUser) -> String {
    hex::encode(user.user_pk)
}

/// The workspace's desktop as `peer` reads it with the members' keys.
fn desktop_in(peer: &StatePeer) -> Option<SharedDesktop> {
    let bytes = opened(peer, &KEYS, DESKTOP)?;
    shared_resource::read(&AutoCommit::load(&bytes).unwrap(), None)
}

/// The windows on the desktop `peer` holds, as `(tab, group)` in order.
fn windows_in(peer: &StatePeer) -> Vec<(u64, u64)> {
    desktop_in(peer).map_or_else(Vec::new, |desktop| {
        desktop.tree.workspaces[0].strips[0]
            .window_groups
            .iter()
            .flat_map(|g| g.tabs.iter().map(move |t| (t.id, g.id)))
            .collect()
    })
}

/// The name of the strip on the desktop `peer` holds.
fn strip_name_in(peer: &StatePeer) -> Option<String> {
    desktop_in(peer).map(|desktop| desktop.tree.workspaces[0].strips[0].name.clone())
}

/// `person`'s write of the desktop as `desktop`, over what `link`'s device holds of it, sealed and
/// checked by that device as every write it makes is: committed as the desktop's writers commit
/// it, so a rename says it is one.
fn write_desktop(
    link: &Linked,
    device: &TestDevice,
    person: &str,
    desktop: &SharedDesktop,
) -> Result<NeighbourMsg, String> {
    link.write(&device.sk, DESKTOP, |doc| {
        shared_resource::write(doc, desktop).unwrap();
        shared_resource::commit_change(doc, person, 0, None);
    })
}

/// `device`'s link to the workspace: a page's way in, its own copy of what the workspace holds
/// and the link its writes travel on.
async fn page_link(server: &Server, device: &TestDevice, workspace_id: WorkspaceId) -> Linked {
    Linked::open(server, device, workspace_id)
        .await
        .expect("the workspace admits the page's device")
}

/// Send `message` as a page does, over `linked`, and what the host made of it: its order once it
/// keeps the write, or why it refused it.
async fn commit(linked: &Linked, message: &NeighbourMsg) -> Result<u64, String> {
    let write = *blake3::hash(&postcard::to_stdvec(message).unwrap()).as_bytes();
    let seq = match message {
        NeighbourMsg::AppDocument { envelope, .. } => envelope.seq,
        _ => panic!("a desktop write is a document frame"),
    };
    let mut answers = linked.answers();
    linked.send(message.clone())?;
    tilo_test_env::within("the host answers the write", async {
        loop {
            match answers.recv().await {
                Ok(answered) if answered.write == write || answered.seq == seq => {
                    return match answered.kept {
                        Kept::Durable => Ok(answered.seq),
                        Kept::Refused { reason } => Err(reason),
                    };
                }
                Ok(_) => {}
                Err(error) => panic!("the link ended before the host answered the write: {error}"),
            }
        }
    })
    .await
}

/// Admit `person`'s `device` to `workspace` as `role`.
async fn admit(
    server: &Server,
    owner: &TestDevice,
    workspace: &str,
    (person, device): (&TestUser, &TestDevice),
    role: Role,
    request: u8,
) {
    let invitation = invite(server, owner, workspace, role, person.user_pk).await;
    let verified = decode_and_verify_invitation(
        &invitation.signed_invitation,
        None,
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    WorkspaceJoinClient::new(device_endpoint(device).await)
        .redeem_hint(
            &verified.grant().transport[0],
            invitation.signed_invitation.clone(),
            device.cert.clone(),
            &device.sk,
            [request; 32],
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn members_close_and_move_any_window_and_only_a_moderator_renames_the_strip() {
    let alice = TestUser::fresh("Alice");
    let ada = TestUser::fresh("Ada");
    let ben = TestUser::fresh("Ben");
    let mo = TestUser::fresh("Mo");
    let cy = TestUser::fresh("Cy");
    let host = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let ada_browser = ada.enrol("ada-browser", DeviceCapabilities::member());
    let ben_browser = ben.enrol("ben-browser", DeviceCapabilities::member());
    let mo_browser = mo.enrol("mo-browser", DeviceCapabilities::member());
    let cy_laptop = cy.enrol("cy-laptop", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start(state.path(), &alice, &host).await;
    let hosted = create_and_enter(&server, &laptop, "Lounge").await;
    // Guests are Editors, as a public workspace admits them; Mo is made the
    // workspace's Moderator once he is in.
    for (who, request) in [
        ((&ada, &ada_browser), 31),
        ((&ben, &ben_browser), 32),
        ((&mo, &mo_browser), 33),
        ((&cy, &cy_laptop), 35),
    ] {
        admit(
            &server,
            &laptop,
            &hosted.workspace,
            who,
            Role::Editor,
            request,
        )
        .await;
    }
    let workspace = hosted.workspace_id;
    let (a, b, c, m) = (person(&ada), person(&ben), person(&cy), person(&mo));
    let laptop_link = Linked::open(&server, &laptop, workspace)
        .await
        .expect("the workspace admits Alice's laptop");
    let ada_link = page_link(&server, &ada_browser, workspace).await;
    let ben_link = page_link(&server, &ben_browser, workspace).await;
    let cy_link = page_link(&server, &cy_laptop, workspace).await;
    let mo_link = page_link(&server, &mo_browser, workspace).await;
    let members = [&laptop_link, &ada_link, &ben_link, &cy_link, &mo_link];

    // Alice starts the workspace's desktop: its one strip, the workspace's own.
    write_desktop(&laptop_link, &laptop, &person(&alice), &desktop(&[]))
        .expect("the workspace's desktop starts");
    for link in members {
        crate::keep::common::receives(link, |peer| strip_name_in(peer).is_some()).await;
    }

    // Mo is made the workspace's moderator while his page is in it, and every copy learns it,
    // the laptop that signed it included: the host relays a signed change back to its signer too.
    laptop
        .makes(
            server.workspaces().unwrap(),
            &hosted.workspace,
            tilo_action_policy::Change::SetRole {
                person: hex::encode(mo.user_pk),
                role: Role::Moderator,
            },
        )
        .await
        .unwrap();
    for link in [&mo_link, &ada_link, &laptop_link] {
        crate::keep::common::receives(link, |peer| {
            role(peer, mo_browser.device_pubkey()) == Some(collab_types::Role::Moderator)
        })
        .await;
    }
    // What each page asks before it offers a close on someone else's window.
    let moderates = |device: &TestDevice| {
        let workspaces = server.workspaces().unwrap();
        let cert = device.cert.clone();
        let request = tilo_server_protocol::workspaces::WorkspacesRequest::Permits {
            workspace: hosted.workspace.clone(),
            node: tilo_desktop_model::ownership::moderation_node(&hosted.workspace).to_string(),
        };
        async move { workspaces.page_workspaces(&cert, request).await }
    };
    assert_eq!(
        moderates(&mo_browser).await,
        tilo_server_protocol::workspaces::WorkspacesReply::Permits(true)
    );
    assert_eq!(
        moderates(&ada_browser).await,
        tilo_server_protocol::workspaces::WorkspacesReply::Permits(false)
    );

    // Ada opens window 1, Ben window 2.
    let ada_opens = write_desktop(
        &ada_link,
        &ada_browser,
        &a,
        &desktop(&[(1, 100, a.clone())]),
    )
    .expect("Ada opens a window");
    crate::keep::common::receives(&ben_link, |peer| windows_in(peer) == [(1, 100)]).await;
    let both = desktop(&[(1, 100, a.clone()), (2, 200, b.clone())]);
    write_desktop(&ben_link, &ben_browser, &b, &both).expect("Ben opens a window");
    for link in members {
        crate::keep::common::receives(link, |peer| windows_in(peer) == [(1, 100), (2, 200)]).await;
    }

    // The strip is the workspace's, not Ada's: her own device will not make the rename,
    // and one that does, sealed as every write is, says it renames the strip.
    let renamed = named(both.clone(), "Ada's");
    assert!(
        write_desktop(&ada_link, &ada_browser, &a, &renamed).is_err(),
        "Ada's own device refuses her rename"
    );
    let held = ada_link.opened(DESKTOP).expect("Ada holds the desktop");
    let mut doc = AutoCommit::load(&held)
        .unwrap()
        .with_actor(automerge::ActorId::from(
            ada_browser.device_pubkey().to_vec(),
        ));
    let heads = doc.get_heads();
    shared_resource::write(&mut doc, &renamed).unwrap();
    shared_resource::commit_change(&mut doc, &a, 0, None);
    let claims =
        collab_types::claim::claims_of(&doc.get_changes(&heads), collab_types::claim::record_unit)
            .expect("the change names what it did");
    let mutation = collab_types::DocumentMutation::Incremental(doc.save_after(&heads));
    // Her next write of the desktop: the refused one took no number.
    let NeighbourMsg::AppDocument { envelope, .. } = &ada_opens else {
        panic!("a desktop write is a document frame");
    };
    let seq = envelope.seq.saturating_add(1);
    let renames = crate::keep::common::signed_by_hand(
        &ada_browser.sk,
        (&hosted.workspace, DESKTOP_APP, DESKTOP_DOCUMENT),
        &mutation,
        seq,
        None,
        claims,
    );
    // The host refuses it by Ada's grants, without reading it.
    let refused = commit(&ada_link, &renames).await.unwrap_err();
    assert!(refused.contains("field:moderation"), "{refused}");
    // A host that skipped its check hands it to Ben's device, which refuses it the same way as it
    // takes it in, on the path every write from the workspace's host takes.
    let mut ben_copy = ben_link.read(StatePeer::clone);
    let refused = ben_copy
        .receive_from(
            TransportOrigin::ServerDelta(ada_browser.device_pubkey()),
            renames,
            SystemTime::now(),
        )
        .expect_err("Ben's device refuses the rename a faulty host let through")
        .to_string();
    assert!(refused.contains("field:moderation"), "{refused}");
    assert_eq!(strip_name_in(&ben_copy).as_deref(), Some("Lounge"));

    // A rename whose change leaves its claim out claims only an edit of the desktop, which the
    // host, deciding by the claim without reading it, takes from Ada. Every device that reads it
    // sees the rename the claim hides, and leaves the write out.
    let mut doc = AutoCommit::load(&held)
        .unwrap()
        .with_actor(automerge::ActorId::from(
            ada_browser.device_pubkey().to_vec(),
        ));
    let heads = doc.get_heads();
    shared_resource::write(&mut doc, &renamed).unwrap();
    doc.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(shared_resource::change_message(&a, &doc)),
    );
    let claims =
        collab_types::claim::claims_of(&doc.get_changes(&heads), collab_types::claim::record_unit)
            .expect("the change names what it did");
    let hidden = crate::keep::common::signed_by_hand(
        &ada_browser.sk,
        (&hosted.workspace, DESKTOP_APP, DESKTOP_DOCUMENT),
        &collab_types::DocumentMutation::Incremental(doc.save_after(&heads)),
        seq,
        None,
        claims,
    );
    commit(&ada_link, &hidden)
        .await
        .expect("the host takes the write by what it claims");
    let left_out = |peer: &StatePeer| {
        let mut open = |op: &collab_types::SignedOp| {
            let metadata = op.encryption.ok_or("not sealed")?;
            let ciphertext =
                collab_types::sealed_ciphertext(op).map_err(|error| error.to_string())?;
            KEYS.open_content(op.artifact, &metadata, &ciphertext)
                .map(|plaintext| plaintext.to_vec())
                .map_err(|error| error.to_string())
        };
        peer.publication()
            .opened_document(DESKTOP_APP, DESKTOP_DOCUMENT, &mut open)
            .unwrap()
            .map_or_else(Vec::new, |opened| opened.unmerged)
            .into_iter()
            .any(|write| {
                write.author == ada_browser.device_pubkey()
                    && write.seq == seq
                    && matches!(&write.reason, tilo_peer_state::UnmergedReason::Refused { refusal }
                        if refusal.contains("without claiming"))
            })
    };
    crate::keep::common::receives(&ben_link, left_out).await;
    assert_eq!(
        ben_link.read(strip_name_in).as_deref(),
        Some("Lounge"),
        "Ben's device reads the desktop without the rename"
    );

    // The workspace's windows are every member's: Cy moves Ben's window into Ada's, and closes it.
    let into_hers = desktop(&[(1, 100, a.clone()), (2, 100, b.clone())]);
    write_desktop(&cy_link, &cy_laptop, &c, &into_hers).expect("Cy moves Ben's window");
    crate::keep::common::receives(&ada_link, |peer| windows_in(peer) == [(1, 100), (2, 100)]).await;
    write_desktop(&cy_link, &cy_laptop, &c, &desktop(&[(1, 100, a.clone())]))
        .expect("Cy closes Ben's window");
    crate::keep::common::receives(&ben_link, |peer| windows_in(peer) == [(1, 100)]).await;
    // Ben closes Ada's.
    write_desktop(&ben_link, &ben_browser, &b, &desktop(&[])).expect("Ben closes Ada's window");
    for link in members {
        crate::keep::common::receives(link, |peer| windows_in(peer).is_empty()).await;
    }

    // Mo moderates the workspace: he renames its strip, and every member's device takes it.
    write_desktop(&mo_link, &mo_browser, &m, &named(desktop(&[]), "Mo's"))
        .expect("Mo renames the strip");
    for link in members {
        crate::keep::common::receives(link, |peer| strip_name_in(peer).as_deref() == Some("Mo's"))
            .await;
    }
    server.shutdown().await;
}

/// What a guest's page asks the workspace to learn its person's role
/// (`tilo_action_policy::roles::telling_nodes`) names the role the workspace
/// admitted them with, in a public workspace too, so the page's apps let them
/// write where the workspace would take it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_public_workspaces_guest_is_answered_the_role_it_was_admitted_with() {
    let alice = TestUser::fresh("Alice");
    let ada = TestUser::fresh("Ada");
    let host = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let ada_browser = ada.enrol("ada-browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start(state.path(), &alice, &host).await;
    let hosted = create_and_enter(&server, &laptop, "Lounge").await;
    admit(
        &server,
        &laptop,
        &hosted.workspace,
        (&ada, &ada_browser),
        Role::Editor,
        41,
    )
    .await;
    let answered = |device: &TestDevice| {
        let workspaces = server.workspaces().unwrap();
        let cert = device.cert.clone();
        let workspace = hosted.workspace.clone();
        async move {
            let mut answers = [false; 3];
            for (answer, node) in answers
                .iter_mut()
                .zip(tilo_action_policy::roles::telling_nodes(&workspace))
            {
                let request = tilo_server_protocol::workspaces::WorkspacesRequest::Permits {
                    workspace: workspace.clone(),
                    node: node.to_string(),
                };
                *answer = matches!(
                    workspaces.page_workspaces(&cert, request).await,
                    tilo_server_protocol::workspaces::WorkspacesReply::Permits(true)
                );
            }
            tilo_action_policy::roles::role_answered(answers)
        }
    };
    assert_eq!(
        answered(&ada_browser).await,
        tilo_action_policy::Role::Editor
    );
    server.shutdown().await;
}
