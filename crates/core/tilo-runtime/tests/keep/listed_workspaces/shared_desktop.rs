//! A workspace's shared desktop through a real server: its members close and
//! move any of its windows, whoever opened them, but rename no strip someone
//! else made, which a moderator the workspace made does. A refused write is
//! neither kept nor forwarded, and a device sending one is told.

use std::time::Duration;

use tilo_desktop_model::layout::{Strip, Tab, TileContent, WindowGroup};
use tilo_desktop_model::ownership::{Authors, DESKTOP_APP, DESKTOP_DOCUMENT, SharedDesktop};
use tilo_desktop_model::{Layout, StripId, WindowWidth, Workspace, WorkspaceId, shared_resource};
use tilo_peer_state::direct::Kept;
use tilo_workspace_service::workspaces::SetMemberRoleRequest;

use super::*;

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

/// The windows on the workspace's desktop as its host follows them, as
/// `(tab, group)` in order; empty while it holds none.
async fn windows(server: &Server, workspace: &str) -> Vec<(u64, u64)> {
    let Some(bytes) = desktop_record(server, workspace).await else {
        return Vec::new();
    };
    let doc = AutoCommit::load(&bytes).unwrap();
    let Some(desktop) = shared_resource::read(&doc, None) else {
        return Vec::new();
    };
    desktop.tree.workspaces[0].strips[0]
        .window_groups
        .iter()
        .flat_map(|g| g.tabs.iter().map(move |t| (t.id, g.id)))
        .collect()
}

/// The name of the strip on the workspace's desktop as its host follows it.
async fn strip_name(server: &Server, workspace: &str) -> Option<String> {
    let bytes = desktop_record(server, workspace).await?;
    let doc = AutoCommit::load(&bytes).unwrap();
    let desktop = shared_resource::read(&doc, None)?;
    Some(desktop.tree.workspaces[0].strips[0].name.clone())
}

/// The workspace's desktop record as the host's copy holds it: a page's writes, which the
/// host follows as it does any record's, and not a record of the workspace's store.
async fn desktop_record(server: &Server, workspace: &str) -> Option<Vec<u8>> {
    server
        .workspaces()
        .unwrap()
        .state(workspace)
        .await?
        .document(DESKTOP_APP, DESKTOP_DOCUMENT)
}

async fn wait_for_strip_name(server: &Server, workspace: &str, want: &str) {
    tilo_test_env::within(format!("the workspace's strip is named {want}"), async {
        while strip_name(server, workspace).await.as_deref() != Some(want) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

async fn wait_for_windows(server: &Server, workspace: &str, want: &[(u64, u64)]) {
    tilo_test_env::within(format!("the workspace's desktop holds {want:?}"), async {
        while windows(server, workspace).await != want {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// A member's page: their device, and their writes of the desktop signed
/// as that device signs them, each over the desktop the workspace holds now.
struct Page<'a> {
    device: &'a TestDevice,
    workspace: String,
    actor: u8,
    seq: u64,
}

impl Page<'_> {
    /// `desktop` written over what the workspace's desktop holds now, as one
    /// signed document message.
    async fn write(&mut self, server: &Server, desktop: &SharedDesktop) -> NeighbourMsg {
        let mut frames = self.write_frames(server, desktop).await;
        assert_eq!(frames.len(), 1, "a small write is one frame");
        frames.remove(0)
    }

    /// `desktop` written as [`Self::write`] does, as the signed frames that
    /// carry it: one, or one per segment of a larger write.
    async fn write_frames(
        &mut self,
        server: &Server,
        desktop: &SharedDesktop,
    ) -> Vec<NeighbourMsg> {
        let held = desktop_record(server, &self.workspace).await;
        // A page's every write is its own: a refused one is never the base
        // of the next, which takes up from the workspace's desktop again.
        self.seq = self.seq.saturating_add(1);
        let mut actor = vec![self.actor; 8];
        actor.extend(self.seq.to_le_bytes());
        let mut doc = held
            .map_or_else(AutoCommit::new, |bytes| AutoCommit::load(&bytes).unwrap())
            .with_actor(automerge::ActorId::from(actor));
        let heads = doc.get_heads();
        shared_resource::write(&mut doc, desktop).unwrap();
        // A member's page writes its desktop as the person whose device it is.
        doc.commit_with(
            automerge::transaction::CommitOptions::default().with_message(
                tilo_actions::ActionContext::whole_edit_by(&tilo_actions::Acting::person(
                    hex::encode(self.device.cert.user_public_key),
                )),
            ),
        );
        let mutation = collab_types::DocumentMutation::Incremental(doc.save_after(&heads));
        let author = collab_types::Ed25519PublicKey::from_bytes(self.device.device_pubkey());
        mutation
            .encode_frames()
            .unwrap()
            .into_iter()
            .map(|frame| {
                self.seq = self.seq.saturating_add(1);
                let mut op = collab_types::SignedOp::new_unsigned(
                    frame,
                    collab_types::document_artifact_id(
                        DESKTOP_APP,
                        &self.workspace,
                        DESKTOP_DOCUMENT,
                    ),
                    author,
                    collab_types::document_attestation_id(
                        DESKTOP_APP,
                        &self.workspace,
                        DESKTOP_DOCUMENT,
                    ),
                    collab_types::OpKind::Change,
                    SystemTime::now(),
                    self.seq,
                );
                collab_types::sign_op(&mut op, &self.device.sk).unwrap();
                NeighbourMsg::AppDocument {
                    peer: tilo_peer_state::peer_handle(author.as_bytes()),
                    app_id: DESKTOP_APP.into(),
                    cluster_id: self.workspace.clone(),
                    document_id: DESKTOP_DOCUMENT.into(),
                    envelope: Box::new(op),
                    readers: None,
                }
            })
            .collect()
    }
}

/// A write that opens nothing: a page's first desktop publication.
async fn ada_page_probe(server: &Server, device: &TestDevice, workspace: &str) -> NeighbourMsg {
    let mut page = Page {
        device,
        workspace: workspace.to_owned(),
        actor: 9,
        seq: 1,
    };
    page.write(server, &desktop(&[])).await
}

/// `device`'s link to the workspace: a page's way in, its own copy of what the workspace holds
/// and the link its writes travel on.
async fn page_link(server: &Server, device: &TestDevice, workspace_id: WorkspaceId) -> Linked {
    Linked::open(server, device, workspace_id)
        .await
        .expect("the workspace admits the page's device")
}

/// Send `message` as a page does, over `linked`, and what the host made of it: its order once it
/// keeps the write whole, why it refused it, or nothing while it waits for the rest of a write
/// that travels in segments.
async fn commit(linked: &Linked, message: &NeighbourMsg) -> Result<Option<u64>, String> {
    /// How long one write is given to be answered: a whole write is answered as it is decided,
    /// and a segment of a larger one only when the segment completes it.
    const ANSWERED: Duration = Duration::from_secs(2);
    let write = *blake3::hash(&postcard::to_stdvec(message).unwrap()).as_bytes();
    // The write's own sequence number, as its signer gave it: each of a page's writes has one of
    // its own, so an answer naming it is this write's too.
    let seq = match message {
        NeighbourMsg::AppDocument { envelope, .. } => Some(envelope.seq),
        _ => None,
    };
    let mut answers = linked.answers();
    linked.send(message.clone())?;
    let answered = tokio::time::timeout(ANSWERED, async {
        loop {
            match answers.recv().await {
                Ok(answered) if answered.write == write || Some(answered.seq) == seq => {
                    return match answered.kept {
                        Kept::Durable => Ok(Some(answered.seq)),
                        Kept::Refused { reason } => Err(reason),
                    };
                }
                Ok(_) => {}
                Err(error) => panic!("the link ended before the host answered the write: {error}"),
            }
        }
    })
    .await;
    match answered {
        Ok(outcome) => outcome,
        // No verdict is no refusal: the write waits for what its segments still lack.
        Err(_) => Ok(None),
    }
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
    let host = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let ada_browser = ada.enrol("ada-browser", DeviceCapabilities::member());
    let ben_browser = ben.enrol("ben-browser", DeviceCapabilities::member());
    let mo_browser = mo.enrol("mo-browser", DeviceCapabilities::member());
    let cy = TestUser::fresh("Cy");
    let cy_laptop = cy.enrol("cy-laptop", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start(state.path(), &alice, &host).await;
    let hosted = create_and_enter(&server, &laptop, "Lounge").await;
    // Guests are Editors, as a public workspace admits them; Mo is made the
    // workspace's Moderator once he is in.
    admit(
        &server,
        &laptop,
        &hosted.workspace,
        (&ada, &ada_browser),
        Role::Editor,
        31,
    )
    .await;
    admit(
        &server,
        &laptop,
        &hosted.workspace,
        (&ben, &ben_browser),
        Role::Editor,
        32,
    )
    .await;
    admit(
        &server,
        &laptop,
        &hosted.workspace,
        (&mo, &mo_browser),
        Role::Editor,
        33,
    )
    .await;
    admit(
        &server,
        &laptop,
        &hosted.workspace,
        (&cy, &cy_laptop),
        Role::Editor,
        35,
    )
    .await;
    let workspace = hosted.workspace_id;
    // The desktop record starts as a member's first write of a part it never held does: a
    // device the workspace lets create it starts it, and the pages' native changes fill it.
    let laptop_link = Linked::open(&server, &laptop, workspace)
        .await
        .expect("the workspace admits Alice's laptop");
    laptop_link
        .publish(|peer| {
            peer.publication_mut().publish_document(
                DESKTOP_APP,
                DESKTOP_DOCUMENT,
                AutoCommit::new().save(),
                &laptop.sk,
                SystemTime::now(),
            )
        })
        .expect("the workspace's desktop starts");
    let ada_link = page_link(&server, &ada_browser, workspace).await;
    let ben_link = page_link(&server, &ben_browser, workspace).await;
    let mut mo_link = page_link(&server, &mo_browser, workspace).await;
    // Mo is made the workspace's moderator while his page is in it.
    server
        .workspaces()
        .unwrap()
        .set_member_role(
            &hosted.workspace,
            &laptop.cert,
            SetMemberRoleRequest {
                request_id: [34; 32],
                user_public_key: mo.user_pk,
                role: Role::Moderator,
            },
        )
        .await
        .unwrap();
    // His page goes on taking the workspace's changes, live.
    let probe = ada_page_probe(&server, &ada_browser, &hosted.workspace).await;
    let _ = commit(&ada_link, &probe).await.unwrap();
    crate::keep::common::next(&mut mo_link, |heard| {
        heard.author == ada_browser.device_pubkey()
    })
    .await;
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
    let page = |device, actor| Page {
        device,
        workspace: hosted.workspace.clone(),
        actor,
        seq: 1_000,
    };
    let (mut ada_page, mut ben_page, mut mo_page, mut cy_page) = (
        page(&ada_browser, 1),
        page(&ben_browser, 2),
        page(&mo_browser, 3),
        page(&cy_laptop, 4),
    );
    let (a, b) = (person(&ada), person(&ben));

    // Ada opens window 1, Ben window 2.
    let opens = ada_page
        .write(&server, &desktop(&[(1, 100, a.clone())]))
        .await;
    let _ = commit(&ada_link, &opens).await.unwrap();
    wait_for_windows(&server, &hosted.workspace, &[(1, 100)]).await;
    let both = desktop(&[(1, 100, a.clone()), (2, 200, b.clone())]);
    let opens = ben_page.write(&server, &both).await;
    let _ = commit(&ben_link, &opens).await.unwrap();
    wait_for_windows(&server, &hosted.workspace, &[(1, 100), (2, 200)]).await;

    // The strip is the workspace's, not Ada's: she may not rename it.
    let renames = ada_page.write(&server, &named(both.clone(), "Ada's")).await;
    let refused = commit(&ada_link, &renames).await.unwrap_err();
    assert!(refused.contains("may not rename"), "{refused}");
    // A write larger than one frame is no case here: a page's write over this host is in the
    // clear, and only a sealed record's history is carried in segments (the segments' gathering
    // and a refused completion are `document_frame`'s and the sealed records' tests').
    // Nor may Cy, through a plain write over his link: it is dropped,
    // and his next write, which is his to make, lands without it.
    let cy_link = page_link(&server, &cy_laptop, workspace).await;
    crate::keep::common::wait_until_subscribed(&server, workspace, &cy_laptop.device_pubkey())
        .await;
    // Both over the same desktop: had the workspace kept the rename, the
    // strip would be renamed beside Cy's new window.
    let three = desktop(&[
        (1, 100, a.clone()),
        (2, 200, b.clone()),
        (3, 300, person(&cy)),
    ]);
    let cy_opens = cy_page.write(&server, &three).await;
    let cy_renames = cy_page.write(&server, &named(both.clone(), "Cy's")).await;
    cy_link
        .send(cy_renames)
        .expect("Cy's refused rename goes out on his link");
    cy_link
        .send(cy_opens)
        .expect("Cy's write goes out on his link");
    wait_for_windows(&server, &hosted.workspace, &[(1, 100), (2, 200), (3, 300)]).await;
    assert_eq!(
        strip_name(&server, &hosted.workspace).await.as_deref(),
        Some("Lounge")
    );
    let cy_closes_his = cy_page.write(&server, &both).await;
    cy_link
        .send(cy_closes_his)
        .expect("Cy's close goes out on his link");
    wait_for_windows(&server, &hosted.workspace, &[(1, 100), (2, 200)]).await;

    // The workspace's windows are every member's: Ada moves Ben's window
    // into hers, and closes it.
    let into_hers = desktop(&[(1, 100, a.clone()), (2, 100, b.clone())]);
    let moves = ada_page.write(&server, &into_hers).await;
    let _ = commit(&ada_link, &moves).await.unwrap();
    wait_for_windows(&server, &hosted.workspace, &[(1, 100), (2, 100)]).await;
    let closes = ada_page
        .write(&server, &desktop(&[(1, 100, a.clone())]))
        .await;
    let _ = commit(&ada_link, &closes).await.unwrap();
    wait_for_windows(&server, &hosted.workspace, &[(1, 100)]).await;

    // Ben closes Ada's.
    let closes = ben_page.write(&server, &desktop(&[])).await;
    let _ = commit(&ben_link, &closes).await.unwrap();
    wait_for_windows(&server, &hosted.workspace, &[]).await;

    // Mo moderates the workspace: he renames its strip.
    let renames = mo_page.write(&server, &named(desktop(&[]), "Mo's")).await;
    let _ = commit(&mo_link, &renames).await.unwrap();
    wait_for_strip_name(&server, &hosted.workspace, "Mo's").await;
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
