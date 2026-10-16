//! A room's shared desktop through a real relay: guests close and move the
//! windows they opened, never someone else's, and a moderator the room
//! made closes anyone's. A refused write is neither kept nor forwarded, and
//! a page committing one is told.

use std::time::Duration;

use tilo_desktop_model::layout::{Strip, Tab, TileContent, WindowGroup};
use tilo_desktop_model::ownership::{Authors, DESKTOP_APP, DESKTOP_RECORD};
use tilo_desktop_model::{DesktopTree, StripId, WindowWidth, Workspace, WorkspaceId};
use tilo_relay::rooms::SetMemberRoleRequest;
use tilo_relay::{CoeditPayload, RelayClient};

use super::*;

/// The windows of a one-strip desktop: `(tab, group, who opened it)`, in
/// order, each tab in its own group unless two share one.
fn desktop(windows: &[(u64, u64, String)]) -> String {
    let mut workspace = Workspace::new(WorkspaceId(1), "Lounge");
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
    let tree = DesktopTree {
        active: workspace.id,
        workspaces: vec![workspace],
        ..DesktopTree::default()
    };
    let authors: Authors = windows
        .iter()
        .map(|(tab, _, who)| (*tab, who.clone()))
        .collect();
    serde_json::json!({ "tree": tree, "records": [], "authors": authors }).to_string()
}

/// The person id a room knows `user` by.
fn person(user: &TestUser) -> String {
    hex::encode(user.user_pk)
}

/// The windows on the room's desktop as its governor follows them, as
/// `(tab, group)` in order; empty while it holds none.
async fn windows(relay: &Relay, room: &str) -> Vec<(u64, u64)> {
    let Some(bytes) = relay
        .rooms()
        .unwrap()
        .record_document(room, DESKTOP_APP, DESKTOP_RECORD)
        .await
        .unwrap()
    else {
        return Vec::new();
    };
    let doc = AutoCommit::load(&bytes).unwrap();
    let Some((automerge::Value::Scalar(text), _)) = doc.get(ROOT, "desktop").unwrap() else {
        return Vec::new();
    };
    let automerge::ScalarValue::Str(text) = text.as_ref() else {
        return Vec::new();
    };
    let desktop = tilo_desktop_model::ownership::SharedDesktop::parse(text).unwrap();
    desktop.tree.workspaces[0].strips[0]
        .window_groups
        .iter()
        .flat_map(|g| g.tabs.iter().map(move |t| (t.id, g.id)))
        .collect()
}

async fn wait_for_windows(relay: &Relay, room: &str, want: &[(u64, u64)]) {
    tilo_test_env::within(format!("the room's desktop holds {want:?}"), async {
        while windows(relay, room).await != want {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// A member's page: their device, and their writes of the desktop signed
/// as that device signs them, each over the desktop the room holds now.
struct Page<'a> {
    device: &'a TestDevice,
    room: String,
    actor: u8,
    seq: u64,
}

impl Page<'_> {
    /// `desktop` written over what the room's desktop holds now, as one
    /// signed document message.
    async fn write(&mut self, relay: &Relay, desktop: &str) -> NeighbourMsg {
        let held = relay
            .rooms()
            .unwrap()
            .record_document(&self.room, DESKTOP_APP, DESKTOP_RECORD)
            .await
            .unwrap();
        // A page's every write is its own: a refused one is never the base
        // of the next, which takes up from the room's desktop again.
        self.seq = self.seq.saturating_add(1);
        let mut actor = vec![self.actor; 8];
        actor.extend(self.seq.to_le_bytes());
        let mut doc = held
            .map_or_else(AutoCommit::new, |bytes| AutoCommit::load(&bytes).unwrap())
            .with_actor(automerge::ActorId::from(actor));
        let heads = doc.get_heads();
        doc.put(ROOT, "desktop", desktop).unwrap();
        doc.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(tilo_actions::FrameworkChange::Native.change_message()),
        );
        let mutation = collab_types::DocumentMutation::Incremental(doc.save_after(&heads));
        let author = collab_types::Ed25519PublicKey::from_bytes(self.device.device_pubkey());
        let mut op = collab_types::SignedOp::new_unsigned(
            mutation.encode().unwrap(),
            collab_types::document_artifact_id(DESKTOP_APP, &self.room, DESKTOP_RECORD),
            author,
            collab_types::document_attestation_id(DESKTOP_APP, &self.room, DESKTOP_RECORD),
            collab_types::OpKind::Change,
            SystemTime::now(),
            self.seq,
        );
        collab_types::sign_op(&mut op, &self.device.sk).unwrap();
        NeighbourMsg::AppDocument {
            peer: tilo_peer_state::peer_handle(author.as_bytes()),
            app_id: DESKTOP_APP.into(),
            cluster_id: self.room.clone(),
            record_id: DESKTOP_RECORD.into(),
            envelope: Box::new(op),
        }
    }
}

/// `device`'s page session in the room's workspace: connected, subscribed.
async fn page_client(relay: &Relay, device: &TestDevice, workspace_id: u64) -> RelayClient {
    let client = RelayClient::connect_to_pinned_host(
        device_endpoint(device).await,
        relay.node_addr(),
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
async fn commit(
    client: &RelayClient,
    device: &TestDevice,
    workspace_id: u64,
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
                    op: acked, reason, ..
                } if acked == op => return Err(reason),
                _ => {}
            }
        }
    })
    .await
}

/// Admit `person`'s `device` to `room` as `role`.
async fn admit(
    relay: &Relay,
    owner: &TestDevice,
    room: &str,
    (person, device): (&TestUser, &TestDevice),
    role: MemberRole,
    request: u8,
) {
    let invitation = invite(relay, owner, room, role, person.user_pk).await;
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
async fn guests_close_and_move_only_their_own_windows_and_a_moderator_any() {
    let alice = TestUser::fresh("Alice");
    let ada = TestUser::fresh("Ada");
    let ben = TestUser::fresh("Ben");
    let mo = TestUser::fresh("Mo");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let ada_browser = ada.enrol("ada-browser", DeviceCapabilities::member());
    let ben_browser = ben.enrol("ben-browser", DeviceCapabilities::member());
    let mo_browser = mo.enrol("mo-browser", DeviceCapabilities::member());
    let cy = TestUser::fresh("Cy");
    let cy_laptop = cy.enrol("cy-laptop", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;
    let room = create_and_enter(&relay, &host, &laptop, "Lounge").await;
    // Guests are Editors, as a public room admits them; Mo is made the
    // room's Moderator once he is in.
    admit(
        &relay,
        &laptop,
        &room.room,
        (&ada, &ada_browser),
        MemberRole::Editor,
        31,
    )
    .await;
    admit(
        &relay,
        &laptop,
        &room.room,
        (&ben, &ben_browser),
        MemberRole::Editor,
        32,
    )
    .await;
    admit(
        &relay,
        &laptop,
        &room.room,
        (&mo, &mo_browser),
        MemberRole::Editor,
        33,
    )
    .await;
    admit(
        &relay,
        &laptop,
        &room.room,
        (&cy, &cy_laptop),
        MemberRole::Editor,
        35,
    )
    .await;
    relay
        .rooms()
        .unwrap()
        .set_member_role(
            &room.room,
            &laptop.cert,
            SetMemberRoleRequest {
                request_id: [34; 32],
                user_public_key: mo.user_pk,
                role: MemberRole::Moderator,
            },
        )
        .await
        .unwrap();
    let workspace = room.workspace_id;
    let ada_client = page_client(&relay, &ada_browser, workspace).await;
    let ben_client = page_client(&relay, &ben_browser, workspace).await;
    let mo_client = page_client(&relay, &mo_browser, workspace).await;
    let page = |device, actor| Page {
        device,
        room: room.room.clone(),
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
        .write(&relay, &desktop(&[(1, 100, a.clone())]))
        .await;
    commit(&ada_client, &ada_browser, workspace, &opens)
        .await
        .unwrap();
    wait_for_windows(&relay, &room.room, &[(1, 100)]).await;
    let both = desktop(&[(1, 100, a.clone()), (2, 200, b.clone())]);
    let opens = ben_page.write(&relay, &both).await;
    commit(&ben_client, &ben_browser, workspace, &opens)
        .await
        .unwrap();
    wait_for_windows(&relay, &room.room, &[(1, 100), (2, 200)]).await;

    // Ada may not close Ben's window, nor move it into hers.
    let closes = ada_page
        .write(&relay, &desktop(&[(1, 100, a.clone())]))
        .await;
    let refused = commit(&ada_client, &ada_browser, workspace, &closes)
        .await
        .unwrap_err();
    assert!(refused.contains("may not close"), "{refused}");
    let into_hers = desktop(&[(1, 100, a.clone()), (2, 100, b.clone())]);
    let moves = ada_page.write(&relay, &into_hers).await;
    let refused = commit(&ada_client, &ada_browser, workspace, &moves)
        .await
        .unwrap_err();
    assert!(refused.contains("may not move"), "{refused}");
    // Nor may Cy, through a native session's plain delta: it is dropped,
    // and his next write, which is his to make, lands without it.
    let cy_session = session(&relay, &cy_laptop, workspace).await;
    crate::common::wait_until_subscribed(&relay, workspace, &cy_laptop.device_pubkey()).await;
    // Both over the same desktop, the close written last: had the room
    // kept it, it would win the last-writer race over the open.
    let three = desktop(&[
        (1, 100, a.clone()),
        (2, 200, b.clone()),
        (3, 300, person(&cy)),
    ]);
    let cy_opens = cy_page.write(&relay, &three).await;
    let cy_closes = cy_page
        .write(&relay, &desktop(&[(2, 200, b.clone())]))
        .await;
    publish(&cy_session, &cy_closes);
    publish(&cy_session, &cy_opens);
    wait_for_windows(&relay, &room.room, &[(1, 100), (2, 200), (3, 300)]).await;
    let cy_closes_his = cy_page.write(&relay, &both).await;
    publish(&cy_session, &cy_closes_his);
    wait_for_windows(&relay, &room.room, &[(1, 100), (2, 200)]).await;
    // She may move her own window past his.
    let reordered = desktop(&[(2, 200, b.clone()), (1, 100, a.clone())]);
    let hers = ada_page.write(&relay, &reordered).await;
    commit(&ada_client, &ada_browser, workspace, &hers)
        .await
        .unwrap();
    wait_for_windows(&relay, &room.room, &[(2, 200), (1, 100)]).await;

    // Ben closes his own window.
    let closes = ben_page
        .write(&relay, &desktop(&[(1, 100, a.clone())]))
        .await;
    commit(&ben_client, &ben_browser, workspace, &closes)
        .await
        .unwrap();
    wait_for_windows(&relay, &room.room, &[(1, 100)]).await;

    // Mo moderates the room: he closes Ada's.
    let clears = mo_page.write(&relay, &desktop(&[])).await;
    commit(&mo_client, &mo_browser, workspace, &clears)
        .await
        .unwrap();
    wait_for_windows(&relay, &room.room, &[]).await;
    relay.shutdown().await;
}
