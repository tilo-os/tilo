//! A workspace's shared desktop through a real server: its members close and
//! move any of its windows, whoever opened them, but rename no strip someone
//! else made, which a moderator the workspace made does. A refused write is
//! neither kept nor forwarded, and a page committing one is told.

use std::time::Duration;

use tilo_desktop_model::layout::{Strip, Tab, TileContent, WindowGroup};
use tilo_desktop_model::ownership::{Authors, DESKTOP_APP, DESKTOP_DOCUMENT, SharedDesktop};
use tilo_desktop_model::{Layout, StripId, WindowWidth, Workspace, WorkspaceId, shared_resource};
use tilo_server::workspaces::SetMemberRoleRequest;
use tilo_server::{CoeditPayload, ServerClient};

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

/// The windows on the workspace's desktop as its governor follows them, as
/// `(tab, group)` in order; empty while it holds none.
async fn windows(server: &Server, workspace: &str) -> Vec<(u64, u64)> {
    let Some(bytes) = server
        .workspaces()
        .unwrap()
        .resource_document(workspace, DESKTOP_APP, DESKTOP_DOCUMENT)
        .await
        .unwrap()
    else {
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

/// The name of the strip on the workspace's desktop as its governor follows it.
async fn strip_name(server: &Server, workspace: &str) -> Option<String> {
    let bytes = server
        .workspaces()
        .unwrap()
        .resource_document(workspace, DESKTOP_APP, DESKTOP_DOCUMENT)
        .await
        .unwrap()?;
    let doc = AutoCommit::load(&bytes).unwrap();
    let desktop = shared_resource::read(&doc, None)?;
    Some(desktop.tree.workspaces[0].strips[0].name.clone())
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
        let held = server
            .workspaces()
            .unwrap()
            .resource_document(&self.workspace, DESKTOP_APP, DESKTOP_DOCUMENT)
            .await
            .unwrap();
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
        doc.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(tilo_actions::FrameworkChange::Native.change_message()),
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

/// `device`'s page session in the workspace: connected, subscribed.
async fn page_client(
    server: &Server,
    device: &TestDevice,
    workspace_id: WorkspaceId,
) -> ServerClient {
    let client = ServerClient::connect_to_pinned_host(
        device_endpoint(device).await,
        server.node_addr(),
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    tilo_test_env::within("the page's subscription is ready", async {
        while !matches!(client.recv().await, Some(Envelope::Ready { .. })) {}
    })
    .await;
    client
}

/// Commit `message` as a page does, and the host's answer: `Ok` with its
/// order, or why it refused.
pub(super) async fn commit(
    client: &ServerClient,
    device: &TestDevice,
    workspace_id: WorkspaceId,
    message: &NeighbourMsg,
) -> Result<u64, String> {
    let (channel, _) = neighbour_wire::retention(message, false).unwrap();
    let delta = CoeditPayload::RetainedDelta {
        channel,
        body: postcard::to_stdvec(message).unwrap(),
    }
    .encode()
    .unwrap();
    let op = *blake3::hash(&delta).as_bytes();
    let op: [u8; 16] = op[..16].try_into().unwrap();
    client
        .send(Envelope::Commit {
            workspace_id,
            op,
            author_device_public_key: device.device_pubkey(),
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta,
        })
        .unwrap();
    tilo_test_env::within("the host answers the commit", async {
        let mut seen = Vec::new();
        loop {
            let Some(envelope) = client.recv().await else {
                panic!("the session closed after {seen:?}");
            };
            seen.push(
                format!("{envelope:?}")
                    .chars()
                    .take(200)
                    .collect::<String>(),
            );
            match envelope {
                Envelope::Committed { op: acked, seq, .. } if acked == op => return Ok(seq),
                Envelope::CommitRefused {
                    op: acked, refusal, ..
                } if acked == op => return Err(refusal.to_string()),
                _ => {}
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
    let ada_client = page_client(&server, &ada_browser, workspace).await;
    let ben_client = page_client(&server, &ben_browser, workspace).await;
    let mo_client = page_client(&server, &mo_browser, workspace).await;
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
    commit(&ada_client, &ada_browser, workspace, &probe)
        .await
        .unwrap();
    tilo_test_env::within("Mo's page takes a change made after his promotion", async {
        loop {
            match mo_client.recv().await {
                Some(Envelope::Delta {
                    author_device_public_key,
                    ..
                }) if author_device_public_key == ada_browser.device_pubkey() => break,
                Some(_) => {}
                None => panic!("Mo's page was disconnected by his promotion"),
            }
        }
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
    commit(&ada_client, &ada_browser, workspace, &opens)
        .await
        .unwrap();
    wait_for_windows(&server, &hosted.workspace, &[(1, 100)]).await;
    let both = desktop(&[(1, 100, a.clone()), (2, 200, b.clone())]);
    let opens = ben_page.write(&server, &both).await;
    commit(&ben_client, &ben_browser, workspace, &opens)
        .await
        .unwrap();
    wait_for_windows(&server, &hosted.workspace, &[(1, 100), (2, 200)]).await;

    // The strip is the workspace's, not Ada's: she may not rename it.
    let renames = ada_page.write(&server, &named(both.clone(), "Ada's")).await;
    let refused = commit(&ada_client, &ada_browser, workspace, &renames)
        .await
        .unwrap_err();
    assert!(refused.contains("may not rename"), "{refused}");
    // Nor through a write large enough to travel in segments: they go on as
    // they come, and the last one, which completes it, is refused, so none
    // of it applies anywhere.
    let mut huge = named(both.clone(), "Ada's");
    huge.tree.workspaces[0].strips[0].window_groups[0].tabs[0].title =
        (0..400_000u32).fold(String::new(), |mut title, n| {
            use std::fmt::Write as _;
            write!(title, "{:x}", n.wrapping_mul(2_654_435_761)).unwrap();
            title
        });
    let frames = ada_page.write_frames(&server, &huge).await;
    assert!(frames.len() > 1, "{} frames", frames.len());
    let (last, first) = frames.split_last().unwrap();
    for frame in first {
        commit(&ada_client, &ada_browser, workspace, frame)
            .await
            .unwrap();
    }
    let refused = commit(&ada_client, &ada_browser, workspace, last)
        .await
        .unwrap_err();
    assert!(refused.contains("may not rename"), "{refused}");
    // Nor may Cy, through a native session's plain delta: it is dropped,
    // and his next write, which is his to make, lands without it.
    let cy_session = session(&server, &cy_laptop, workspace).await;
    crate::common::wait_until_subscribed(&server, workspace, &cy_laptop.device_pubkey()).await;
    // Both over the same desktop: had the workspace kept the rename, the
    // strip would be renamed beside Cy's new window.
    let three = desktop(&[
        (1, 100, a.clone()),
        (2, 200, b.clone()),
        (3, 300, person(&cy)),
    ]);
    let cy_opens = cy_page.write(&server, &three).await;
    let cy_renames = cy_page.write(&server, &named(both.clone(), "Cy's")).await;
    publish(&cy_session, &cy_renames);
    publish(&cy_session, &cy_opens);
    wait_for_windows(&server, &hosted.workspace, &[(1, 100), (2, 200), (3, 300)]).await;
    assert_eq!(
        strip_name(&server, &hosted.workspace).await.as_deref(),
        Some("Lounge")
    );
    let cy_closes_his = cy_page.write(&server, &both).await;
    publish(&cy_session, &cy_closes_his);
    wait_for_windows(&server, &hosted.workspace, &[(1, 100), (2, 200)]).await;

    // The workspace's windows are every member's: Ada moves Ben's window
    // into hers, and closes it.
    let into_hers = desktop(&[(1, 100, a.clone()), (2, 100, b.clone())]);
    let moves = ada_page.write(&server, &into_hers).await;
    commit(&ada_client, &ada_browser, workspace, &moves)
        .await
        .unwrap();
    wait_for_windows(&server, &hosted.workspace, &[(1, 100), (2, 100)]).await;
    let closes = ada_page
        .write(&server, &desktop(&[(1, 100, a.clone())]))
        .await;
    commit(&ada_client, &ada_browser, workspace, &closes)
        .await
        .unwrap();
    wait_for_windows(&server, &hosted.workspace, &[(1, 100)]).await;

    // Ben closes Ada's.
    let closes = ben_page.write(&server, &desktop(&[])).await;
    commit(&ben_client, &ben_browser, workspace, &closes)
        .await
        .unwrap();
    wait_for_windows(&server, &hosted.workspace, &[]).await;

    // Mo moderates the workspace: he renames its strip.
    let renames = mo_page.write(&server, &named(desktop(&[]), "Mo's")).await;
    commit(&mo_client, &mo_browser, workspace, &renames)
        .await
        .unwrap();
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
