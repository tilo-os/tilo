//! A room's layout through a real relay: each page writes the windows its
//! own device opened and nobody else's, a moderator rearranges someone
//! else's only by a request the room retains, and the owner applies that
//! request once. A refused write is neither kept nor forwarded, and a page
//! committing one is told.

use std::time::Duration;

use tilo_desktop_model::commands::{BoundCommand, CommandSource, DesktopCommand, TabLocation};
use tilo_desktop_model::ids::namespace_of_device;
use tilo_desktop_model::layout::{Strip, Tab, TileContent, WindowGroup};
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_RECORD, SharedDesktop};
use tilo_desktop_model::projection::LayoutRequest;
use tilo_desktop_model::{
    DesktopTree, RoomId, StripId, WindowWidth, Workspace, WorkspaceId, compose_id, room_layout,
};
use tilo_relay::rooms::SetMemberRoleRequest;
use tilo_relay::{CoeditPayload, RelayClient};

use super::*;

/// The layout a page holds: one workspace, whose strips are minted in the
/// device's own namespace.
fn layout(room: &str, shown: &str) -> SharedDesktop {
    let room = RoomId::parse(room).unwrap();
    let workspace = Workspace::in_room(WorkspaceId::of_room(room), room, shown);
    SharedDesktop {
        tree: DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        },
        records: Vec::new(),
        authors: Default::default(),
    }
}

/// `device`'s namespace: the one the relay admits its layout entries in.
fn salt(device: &TestDevice) -> u16 {
    namespace_of_device(&hex::encode(device.device_pubkey()))
}

/// A window of `device`'s: `(window counter, group counter)`, in a strip of
/// its own named `name` unless `strip` is given.
fn window(device: &TestDevice, strip: u64, group: u64, tab: u64, title: &str) -> Strip {
    let salt = salt(device);
    Strip {
        id: StripId(compose_id(salt, strip)),
        name: format!("{title} strip"),
        window_groups: vec![WindowGroup {
            id: compose_id(salt, group),
            tabs: vec![Tab {
                id: compose_id(salt, tab),
                title: title.into(),
                tile: TileContent::LocalApp {
                    app_id: "so.tilo.notes".into(),
                    label: "Notes".into(),
                    note: String::new(),
                },
                app_title: None,
            }],
            active_tab: compose_id(salt, tab),
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    }
}

fn tab_id(device: &TestDevice, tab: u64) -> u64 {
    compose_id(salt(device), tab)
}

/// The windows on the room's layout as its governor follows them, as
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
    let Some(layout) = room_layout::read(&doc, None) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for workspace in layout.tree.workspaces {
        for strip in workspace.strips {
            for group in strip.window_groups {
                for tab in group.tabs {
                    out.push((tab.id, group.id));
                }
            }
        }
    }
    out
}

async fn wait_for_windows(relay: &Relay, room: &str, want: &[(u64, u64)]) {
    tilo_test_env::within(format!("the room's layout holds {want:?}"), async {
        while windows(relay, room).await != want {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// A member's page: their device, and their writes of the layout signed as
/// that device signs them, each over the layout the room holds now.
struct Page<'a> {
    device: &'a TestDevice,
    room: String,
    actor: u8,
    seq: u64,
}

impl Page<'_> {
    /// `change` applied to the room's layout as this page holds it, written
    /// as `self.device`'s own entries: one signed document message.
    async fn write(
        &mut self,
        relay: &Relay,
        change: impl FnOnce(&mut SharedDesktop),
    ) -> NeighbourMsg {
        let mut frames = self.write_frames(relay, change).await;
        assert_eq!(frames.len(), 1, "a small write is one frame");
        frames.remove(0)
    }

    /// [`Self::write`] as the signed frames that carry it: one, or one per
    /// segment of a larger write.
    async fn write_frames(
        &mut self,
        relay: &Relay,
        change: impl FnOnce(&mut SharedDesktop),
    ) -> Vec<NeighbourMsg> {
        let held = relay
            .rooms()
            .unwrap()
            .record_document(&self.room, DESKTOP_APP, DESKTOP_RECORD)
            .await
            .unwrap();
        // A page's every write is its own: a refused one is never the base
        // of the next, which takes up from the room's layout again.
        self.seq = self.seq.saturating_add(1);
        let mut actor = vec![self.actor; 8];
        actor.extend(self.seq.to_le_bytes());
        let mut doc = held
            .map_or_else(AutoCommit::new, |bytes| AutoCommit::load(&bytes).unwrap())
            .with_actor(automerge::ActorId::from(actor));
        let mut value =
            room_layout::read(&doc, None).unwrap_or_else(|| layout(&self.room, "Lounge"));
        change(&mut value);
        let heads = doc.get_heads();
        let author = hex::encode(self.device.device_pubkey());
        room_layout::write(&mut doc, &value, salt(self.device), Some(&author)).unwrap();
        doc.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(tilo_actions::FrameworkChange::Native.change_message()),
        );
        let mutation = collab_types::DocumentMutation::Incremental(doc.save_after(&heads));
        self.sign(mutation)
    }

    /// A request this page makes of `to`'s device, written into the room.
    async fn ask(&mut self, relay: &Relay, command: DesktopCommand, to: u16) -> NeighbourMsg {
        let heads = self.held_heads(relay).await;
        let asker = person_of(self.device);
        let command = BoundCommand {
            source: CommandSource {
                room: self.room.clone(),
                heads: heads.iter().map(|hash| hash.0).collect(),
            },
            command,
        };
        let command = serde_json::to_vec(&command).unwrap();
        let request = LayoutRequest {
            id: LayoutRequest::id(&asker, to, 1, &command),
            from: asker,
            to_salt: to,
            at: 1,
            command,
        };
        let held = relay
            .rooms()
            .unwrap()
            .record_document(&self.room, DESKTOP_APP, DESKTOP_RECORD)
            .await
            .unwrap();
        self.seq = self.seq.saturating_add(1);
        let mut actor = vec![self.actor; 8];
        actor.extend(self.seq.to_le_bytes());
        let mut doc = held
            .map_or_else(AutoCommit::new, |bytes| AutoCommit::load(&bytes).unwrap())
            .with_actor(automerge::ActorId::from(actor));
        let heads = doc.get_heads();
        room_layout::request(&mut doc, &request).unwrap();
        doc.commit();
        self.sign(collab_types::DocumentMutation::Incremental(
            doc.save_after(&heads),
        ))
    }

    /// This page's device records that it applied `request`, as its owner.
    async fn claim(&mut self, relay: &Relay, request: &LayoutRequest) -> NeighbourMsg {
        let held = relay
            .rooms()
            .unwrap()
            .record_document(&self.room, DESKTOP_APP, DESKTOP_RECORD)
            .await
            .unwrap();
        self.seq = self.seq.saturating_add(1);
        let mut actor = vec![self.actor; 8];
        actor.extend(self.seq.to_le_bytes());
        let mut doc = held
            .map_or_else(AutoCommit::new, |bytes| AutoCommit::load(&bytes).unwrap())
            .with_actor(automerge::ActorId::from(actor));
        let heads = doc.get_heads();
        assert!(
            room_layout::claim(&mut doc, salt(self.device), request).unwrap(),
            "the owner applies it once"
        );
        doc.commit();
        let frames = self.sign(collab_types::DocumentMutation::Incremental(
            doc.save_after(&heads),
        ));
        assert_eq!(frames.len(), 1);
        frames.into_iter().next().unwrap()
    }

    /// The rooms heads this page's next request is pinned to.
    async fn held_heads(&self, relay: &Relay) -> Vec<automerge::ChangeHash> {
        let held = relay
            .rooms()
            .unwrap()
            .record_document(&self.room, DESKTOP_APP, DESKTOP_RECORD)
            .await
            .unwrap();
        held.map_or_else(Vec::new, |bytes| {
            AutoCommit::load(&bytes).unwrap().get_heads()
        })
    }

    fn sign(&mut self, mutation: collab_types::DocumentMutation) -> Vec<NeighbourMsg> {
        let author = collab_types::Ed25519PublicKey::from_bytes(self.device.device_pubkey());
        mutation
            .encode_frames()
            .unwrap()
            .into_iter()
            .map(|frame| {
                self.seq = self.seq.saturating_add(1);
                let mut op = collab_types::SignedOp::new_unsigned(
                    frame,
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
            })
            .collect()
    }
}

/// The person a test device writes for: its root key, as the room knows it.
fn person_of(device: &TestDevice) -> String {
    hex::encode(device.cert.user_public_key)
}

/// A write that opens nothing: a page's first layout publication.
async fn ada_page_probe(relay: &Relay, device: &TestDevice, room: &str) -> NeighbourMsg {
    let mut page = Page {
        device,
        room: room.to_owned(),
        actor: 9,
        seq: 1,
    };
    page.write(relay, |_| {}).await
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
pub(super) async fn commit(
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
async fn each_page_writes_its_own_windows_and_a_moderator_asks_instead() {
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
    let workspace = room.workspace_id;
    let ada_client = page_client(&relay, &ada_browser, workspace).await;
    let ben_client = page_client(&relay, &ben_browser, workspace).await;
    let mo_client = page_client(&relay, &mo_browser, workspace).await;
    // Mo is made the room's moderator while his page is in it.
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
    // His page goes on taking the room's changes, live.
    let probe = ada_page_probe(&relay, &ada_browser, &room.room).await;
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
        let rooms = relay.rooms().unwrap();
        let cert = device.cert.clone();
        let request = tilo_relay_protocol::rooms::RoomsRequest::Permits {
            room: room.room.clone(),
            node: tilo_desktop_model::ownership::moderation_node(&room.room).to_string(),
        };
        async move { rooms.page_rooms(&cert, request).await }
    };
    assert_eq!(
        moderates(&mo_browser).await,
        tilo_relay_protocol::rooms::RoomsReply::Permits(true)
    );
    assert_eq!(
        moderates(&ada_browser).await,
        tilo_relay_protocol::rooms::RoomsReply::Permits(false)
    );
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
    let (a, b) = (tab_id(&ada_browser, 21), tab_id(&ben_browser, 21));

    // Ada opens her window, Ben his.
    let opens = ada_page
        .write(&relay, |layout| {
            layout.tree.workspaces[0]
                .strips
                .push(window(&ada_browser, 1, 11, 21, "Ada's notes"));
        })
        .await;
    commit(&ada_client, &ada_browser, workspace, &opens)
        .await
        .unwrap();
    wait_for_windows(
        &relay,
        &room.room,
        &[(a, compose_id(salt(&ada_browser), 11))],
    )
    .await;
    let opens = ben_page
        .write(&relay, |layout| {
            layout.tree.workspaces[0]
                .strips
                .push(window(&ben_browser, 1, 11, 21, "Ben's notes"));
        })
        .await;
    commit(&ben_client, &ben_browser, workspace, &opens)
        .await
        .unwrap();
    wait_for_windows(
        &relay,
        &room.room,
        &[
            (a, compose_id(salt(&ada_browser), 11)),
            (b, compose_id(salt(&ben_browser), 11)),
        ],
    )
    .await;

    // Ada may not close Ben's window, nor move it into hers, however she
    // moderates nothing: both are writes into his namespace.
    let closes = ada_page
        .write(&relay, |layout| {
            layout.tree.workspaces[0]
                .strips
                .retain(|strip| strip.id != StripId(compose_id(salt(&ben_browser), 1)));
        })
        .await;
    let refused = commit(&ada_client, &ada_browser, workspace, &closes)
        .await
        .unwrap_err();
    assert!(refused.contains("minted by another device"), "{refused}");
    let moves = ada_page
        .write(&relay, |layout| {
            // Ben's window put into Ada's group.
            if let Some(mine) = layout.tree.workspaces[0].strips.first_mut() {
                mine.window_groups[0].tabs.push(Tab {
                    id: b,
                    title: "Ben's notes".into(),
                    tile: TileContent::LocalApp {
                        app_id: "so.tilo.notes".into(),
                        label: "Notes".into(),
                        note: String::new(),
                    },
                    app_title: None,
                });
            }
        })
        .await;
    let refused = commit(&ada_client, &ada_browser, workspace, &moves)
        .await
        .unwrap_err();
    assert!(refused.contains("minted by another device"), "{refused}");
    // Nor through a write large enough to travel in segments: they go on as
    // they come, and the last one, which completes it, is refused, so none
    // of it applies anywhere.
    let frames = ada_page
        .write_frames(&relay, |layout| {
            layout.tree.workspaces[0]
                .strips
                .retain(|strip| strip.id != StripId(compose_id(salt(&ben_browser), 1)));
            if let Some(mine) = layout.tree.workspaces[0].strips.first_mut() {
                mine.window_groups[0].tabs[0].title =
                    (0..400_000u32).fold(String::new(), |mut title, n| {
                        use std::fmt::Write as _;
                        write!(title, "{:x}", n.wrapping_mul(2_654_435_761)).unwrap();
                        title
                    });
            }
        })
        .await;
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
    assert!(refused.contains("minted by another device"), "{refused}");
    // Nor may Cy, through a native session's plain delta: it is dropped, and
    // his next write, which is his to make, lands without it.
    let cy_session = session(&relay, &cy_laptop, workspace).await;
    crate::common::wait_until_subscribed(&relay, workspace, &cy_laptop.device_pubkey()).await;
    let cy_closes = cy_page
        .write(&relay, |layout| {
            layout.tree.workspaces[0]
                .strips
                .retain(|strip| strip.id != StripId(compose_id(salt(&ada_browser), 1)));
        })
        .await;
    let cy_opens = cy_page
        .write(&relay, |layout| {
            layout.tree.workspaces[0]
                .strips
                .push(window(&cy_laptop, 1, 11, 21, "Cy's notes"));
        })
        .await;
    publish(&cy_session, &cy_closes);
    publish(&cy_session, &cy_opens);
    wait_for_windows(
        &relay,
        &room.room,
        &[
            (a, compose_id(salt(&ada_browser), 11)),
            (b, compose_id(salt(&ben_browser), 11)),
            (tab_id(&cy_laptop, 21), compose_id(salt(&cy_laptop), 11)),
        ],
    )
    .await;
    // Cy closes his own window.
    let cy_closes_his = cy_page
        .write(&relay, |layout| {
            layout.tree.workspaces[0]
                .strips
                .retain(|strip| strip.id != StripId(compose_id(salt(&cy_laptop), 1)));
        })
        .await;
    publish(&cy_session, &cy_closes_his);
    wait_for_windows(
        &relay,
        &room.room,
        &[
            (a, compose_id(salt(&ada_browser), 11)),
            (b, compose_id(salt(&ben_browser), 11)),
        ],
    )
    .await;

    // Ben closes his own window.
    let closes = ben_page
        .write(&relay, |layout| {
            layout.tree.workspaces[0]
                .strips
                .retain(|strip| strip.id != StripId(compose_id(salt(&ben_browser), 1)));
        })
        .await;
    commit(&ben_client, &ben_browser, workspace, &closes)
        .await
        .unwrap();
    wait_for_windows(
        &relay,
        &room.room,
        &[(a, compose_id(salt(&ada_browser), 11))],
    )
    .await;

    // Mo moderates the room: he asks Ada's device to close her window, the
    // room retains the request, and Ada's page applies it once and records
    // that it did.
    let close_ada = DesktopCommand::CloseTab {
        location: TabLocation {
            strip: compose_id(salt(&ada_browser), 1),
            group: compose_id(salt(&ada_browser), 11),
            tab: a,
        },
    };
    let ask = mo_page
        .ask(&relay, close_ada.clone(), salt(&ada_browser))
        .await;
    commit(&mo_client, &mo_browser, workspace, &ask)
        .await
        .unwrap();
    let held = relay
        .rooms()
        .unwrap()
        .record_document(&room.room, DESKTOP_APP, DESKTOP_RECORD)
        .await
        .unwrap()
        .unwrap();
    let held = AutoCommit::load(&held).unwrap();
    let pending = room_layout::pending(&held, salt(&ada_browser));
    assert_eq!(pending.len(), 1, "the request waits for Ada's device");
    // Ada's page applies it, as the owner: it writes only its own entries
    // and records the request as applied.
    let applied = ada_page
        .write(&relay, |layout| {
            layout.tree.workspaces[0]
                .strips
                .retain(|strip| strip.id != StripId(compose_id(salt(&ada_browser), 1)));
        })
        .await;
    commit(&ada_client, &ada_browser, workspace, &applied)
        .await
        .unwrap();
    let mut owner = Page {
        device: &ada_browser,
        room: room.room.clone(),
        actor: 5,
        seq: 5_000,
    };
    let claimed = owner.claim(&relay, &pending[0]).await;
    commit(&ada_client, &ada_browser, workspace, &claimed)
        .await
        .unwrap();
    wait_for_windows(&relay, &room.room, &[]).await;
    let held = relay
        .rooms()
        .unwrap()
        .record_document(&room.room, DESKTOP_APP, DESKTOP_RECORD)
        .await
        .unwrap()
        .unwrap();
    let held = AutoCommit::load(&held).unwrap();
    assert!(
        room_layout::pending(&held, salt(&ada_browser)).is_empty(),
        "the owner recorded it as applied"
    );
    relay.shutdown().await;
}

/// What a guest's page asks the room to learn its person's role
/// (`tilo_action_policy::roles::telling_nodes`) names the role the room
/// admitted them with, in a public room too, so the page's apps let them
/// write where the room would take it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_public_rooms_guest_is_answered_the_role_it_was_admitted_with() {
    let alice = TestUser::fresh("Alice");
    let ada = TestUser::fresh("Ada");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let ada_browser = ada.enrol("ada-browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;
    let room = create_and_enter(&relay, &host, &laptop, "Lounge").await;
    admit(
        &relay,
        &laptop,
        &room.room,
        (&ada, &ada_browser),
        MemberRole::Editor,
        41,
    )
    .await;
    let rooms = relay.rooms().unwrap();
    rooms
        .set_layout(
            &room.room,
            tilo_desktop_model::projection::RoomLayout::Personal,
        )
        .await
        .unwrap();
    let answered = |device: &TestDevice| {
        let rooms = relay.rooms().unwrap();
        let cert = device.cert.clone();
        let room = room.room.clone();
        async move {
            let mut answers = [false; 3];
            for (answer, node) in answers
                .iter_mut()
                .zip(tilo_action_policy::roles::telling_nodes(&room))
            {
                let request = tilo_relay_protocol::rooms::RoomsRequest::Permits {
                    room: room.clone(),
                    node: node.to_string(),
                };
                *answer = matches!(
                    rooms.page_rooms(&cert, request).await,
                    tilo_relay_protocol::rooms::RoomsReply::Permits(true)
                );
            }
            tilo_action_policy::roles::role_answered(answers)
        }
    };
    assert_eq!(
        answered(&ada_browser).await,
        tilo_action_policy::Role::Editor
    );
    relay.shutdown().await;
}
