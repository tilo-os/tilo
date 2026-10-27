//! A window a member shares on purpose with a room whose layout is
//! personal: on the shared fake net, from the room's record of the share to
//! the tab another member shows it in, and away again when the share ends.
//!
//! Device A and device B are members of a room a host governs, whose layout
//! is personal, so neither publishes its windows into the room. A shares its
//! Notes window: the host records the share in the room's governance, as it
//! records every share of a resource ([`tilo_desktop_model::window_share`]),
//! and the room's sync carries it to the members. B's tree shows it as a tab
//! of A's window in B's own layout, where A's stream of it mounts
//! ([`crate::shell::peer_view::stream_tab_in`]), marked with A's presence
//! while A is here. A stops, and the tab goes from B.

use super::*;
use crate::shell::state::{RoomId, StripId, WorkspaceTree};
use automerge::AutoCommit;
use automerge::transaction::Transactable as _;
use tilo_action_policy::share::{ShareAccess, is_of_share, share_grants};
use tilo_desktop_model::projection::{
    ROOM_LAYOUT_KEY, RoomLayout, project_room_policy, room_policy_of,
};
use tilo_desktop_model::window_share::{window_node, window_share_id};
use tilo_peer_state::room::{AuthorityPin, RoomAdmission, Standing, Writer, admit_room_changes};
use tilo_test_env::{FakeDocumentNet, Peer};

pub(super) const ROOM: &str = "2c4e6a8b-1d3f-4a5b-8c7d-9e0f1a2b3c4d";
pub(super) const CHAT: &str = "so.tilo.chat";
pub(super) const NOTES: &str = "so.tilo.notes";
const NOW_NS: u64 = 1_000_000_000_000;

/// The changes one message of the room's sync carries.
pub(super) fn changes_in(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut scratch = AutoCommit::new();
    scratch
        .load_incremental(bytes)
        .expect("the room's sync carries changes");
    let mut changes: Vec<Vec<u8>> = scratch
        .get_changes(&[])
        .iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    changes.extend(
        scratch
            .committed_document()
            .queued_changes()
            .map(|change| change.raw_bytes().to_vec()),
    );
    changes
}

/// What a member's device holds of the room: what it admitted from the
/// governor.
pub(super) struct Member {
    pub(super) peer: Peer,
    pub(super) admitted: Option<AutoCommit>,
}

impl Member {
    /// Take what the room's sync brought this device, as the governor's,
    /// through the admission every receiver applies.
    pub(super) fn take_in(&mut self, net: &mut FakeDocumentNet, host_key: &str) {
        let mut arrived = Vec::new();
        net.net().deliver_to(self.peer.name(), |_, bytes| {
            arrived.extend(changes_in(bytes))
        });
        if arrived.is_empty() {
            return;
        }
        let admitted = admit_room_changes(
            RoomAdmission {
                before: self.admitted.as_ref(),
                room: ROOM,
                source: Writer {
                    key: host_key,
                    standing: Standing::TransportAuthorized,
                },
                authority: AuthorityPin::Device(host_key),
                now_ns: NOW_NS,
            },
            &arrived,
        )
        .unwrap_or_else(|refusal| panic!("{} refused the governor's room: {refusal}", self.peer));
        self.admitted = Some(admitted.candidate);
    }

    pub(super) fn admitted(&self) -> &AutoCommit {
        self.admitted.as_ref().expect("the device holds the room")
    }

    pub(super) fn on_net(&mut self) -> (&Peer, &mut AutoCommit) {
        (
            &self.peer,
            self.admitted.as_mut().expect("the device holds the room"),
        )
    }
}

/// A device key that is a valid endpoint, as every device's is.
pub(super) fn device_key(seed: u8) -> [u8; 32] {
    *iroh::SecretKey::from_bytes(&[seed; 32]).public().as_bytes()
}

/// A's workspace of the room with Chat and Notes open, both its own legacy
/// windows.
fn a_tree(room: RoomId, salt: u16, chat: u64, notes: u64) -> WorkspaceTree {
    let own = |counter: u64| (u64::from(salt) << 48) | counter;
    let mut tree = WorkspaceTree::bootstrap_default_with_salt(salt);
    tree.join_native_room("Lounge", room)
        .expect("A's workspace of the room");
    let workspace = tree
        .desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.room() == room)
        .expect("the room's workspace");
    workspace.strips = vec![Strip {
        id: StripId(own(1)),
        name: "Main".into(),
        window_groups: vec![WindowGroup {
            id: own(2),
            tabs: vec![
                tab(
                    chat,
                    TileContent::Foreign {
                        app_id: CHAT.into(),
                        surface_id: 9,
                    },
                ),
                tab(
                    notes,
                    TileContent::Foreign {
                        app_id: NOTES.into(),
                        surface_id: 10,
                    },
                ),
            ],
            active_tab: chat,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    }];
    tree
}

/// Every tab B's workspace of the room shows, with what it holds.
pub(super) fn b_tabs(tree: &WorkspaceTree, room: RoomId) -> Vec<(u64, TileContent)> {
    tree.desktop
        .workspaces
        .iter()
        .filter(|workspace| workspace.room() == room)
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .map(|tab| (tab.id, tab.tile.clone()))
        .collect()
}

/// The governor records `person`'s share of the window `tab` of `app` in
/// the room's governance, as the room's host writes a resource share.
fn share(governor: &mut AutoCommit, person: &str, app: &str, tab: u64) {
    let mut policy = room_policy_of(governor, ROOM);
    policy.grants.extend(
        share_grants(
            ROOM,
            person,
            &window_share_id(tab),
            &window_node(ROOM, app, tab),
            ShareAccess::View,
        )
        .expect("a window is one resource of the room"),
    );
    project_room_policy(governor, &policy).expect("the room's authority");
    governor.commit();
}

/// The governor ends the share of the window `tab`.
pub(super) fn stop(governor: &mut AutoCommit, tab: u64) {
    let mut policy = room_policy_of(governor, ROOM);
    policy
        .grants
        .retain(|grant| !is_of_share(grant, &window_share_id(tab)));
    project_room_policy(governor, &policy).expect("the room's authority");
    governor.commit();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one journey across two devices and their governor, told in order"
)]
fn a_window_shared_in_a_personal_room_is_a_tab_of_the_sharers_on_the_others_until_the_share_ends() {
    let room = RoomId::parse(ROOM).expect("a room id");
    // A's windows carry the salt A's device key derives, by which the room
    // knows they run on A's device.
    let (salt, b_salt) = (
        tilo_desktop_model::ids::device_salt_of_key(&device_key(5)),
        0x0B52,
    );
    assert_ne!(salt, b_salt);
    let own = |counter: u64| (u64::from(salt) << 48) | counter;
    let (chat, notes) = (own(3), own(4));
    let (a_device, a_person) = (device_key(5), [0x22; 32]);
    let host = [0xa0; 32];
    let host_key = hex::encode(host);
    let mut governor = governed_room(ROOM, host, a_person, a_device);
    governor
        .put(automerge::ROOT, ROOM_LAYOUT_KEY, RoomLayout::Personal.id())
        .expect("the governor makes the layout personal");
    governor.commit();

    let mut net = FakeDocumentNet::new();
    let gov = net.peer("governor");
    let mut a = Member {
        peer: net.peer("a"),
        admitted: None,
    };
    let mut b = Member {
        peer: net.peer("b"),
        admitted: None,
    };
    net.publish(&gov, &mut governor);
    a.take_in(&mut net, &host_key);
    b.take_in(&mut net, &host_key);

    let mut a_tree = a_tree(room, salt, chat, notes);
    a_tree.project_room_document_for_test(room, a.admitted());
    let mut b_tree = WorkspaceTree::bootstrap_default_with_salt(b_salt);
    let b_workspace = b_tree
        .join_native_room("Lounge", room)
        .expect("B's workspace of the room");
    b_tree.desktop.active = b_workspace;
    b_tree.project_room_document_for_test(room, b.admitted());
    assert!(
        b_tabs(&b_tree, room)
            .iter()
            .all(|(id, _)| tilo_desktop_model::peer_salt_of(*id) != salt),
        "in a personal room none of A's windows is B's tab: {:?}",
        b_tabs(&b_tree, room)
    );
    assert_eq!(
        crate::shell::peer_view::stream_tab_in(&b_tree, NOTES, notes, |_| true),
        None
    );

    // A shares Notes. The host records it, and the room's sync carries it
    // to both members.
    share(&mut governor, &hex::encode(a_person), NOTES, notes);
    net.publish(&gov, &mut governor);
    a.take_in(&mut net, &host_key);
    b.take_in(&mut net, &host_key);
    net.assert_converged(
        &mut [(&gov, &mut governor), a.on_net(), b.on_net()],
        &[tilo_desktop_model::projection::ROOM_GRANTS_KEY],
    );
    a_tree.project_room_document_for_test(room, a.admitted());
    let now_ms = tilo_clock::unix_now_ms();
    assert!(a_tree.is_window_shared(room, notes, now_ms));
    assert!(!a_tree.is_window_shared(room, chat, now_ms));

    assert!(b_tree.project_room_document_for_test(room, b.admitted()));
    let shown = b_tabs(&b_tree, room);
    assert!(
        shown.contains(&(
            notes,
            TileContent::HostedWindow {
                app_id: NOTES.into(),
                label: NOTES.into(),
                host: hex::encode(a_device),
                host_label: String::new(),
            }
        )),
        "B shows A's Notes as a tab of A's window: {shown:?}"
    );
    assert!(
        !shown.iter().any(|(id, _)| *id == chat),
        "A's Chat, not shared, stays A's alone"
    );
    assert_eq!(
        crate::shell::peer_view::stream_tab_in(&b_tree, NOTES, notes, |_| true),
        Some(notes),
        "A's stream of Notes mounts in that tab"
    );
    assert_eq!(
        crate::shell::peer_view::stream_tab_in(&b_tree, CHAT, chat, |_| true),
        None,
        "a stream of A's Chat has nowhere to show"
    );
    assert!(
        !b_tree.project_room_document_for_test(room, b.admitted()),
        "taking in the same room again changes nothing"
    );

    // A is here: the tab wears A's presence.
    let a_session = crate::shell::state::host_handle(&hex::encode(a_device))
        .expect("A's device is an endpoint");
    assert!(b_tree.admit_collaborator_in_room(
        room,
        crate::shell::state::Collaborator::new("ana", "Ana").with_session(a_session),
    ));
    let mut presence = ui::presence::PresenceCtx::default();
    crate::shell::sync_tab_presence_from_tree(&b_tree, &mut presence);
    assert_eq!(
        presence.peers_on(&ui::presence::PresenceKey::Tab {
            id: format!("{notes:x}"),
        }),
        &[ui::permissions::UserId("ana".into())],
        "the shared tab is marked with the sharer's presence"
    );

    // A stops sharing, and the tab goes from B.
    stop(&mut governor, notes);
    net.publish(&gov, &mut governor);
    a.take_in(&mut net, &host_key);
    b.take_in(&mut net, &host_key);
    a_tree.project_room_document_for_test(room, a.admitted());
    assert!(!a_tree.is_window_shared(room, notes, tilo_clock::unix_now_ms()));
    assert!(b_tree.project_room_document_for_test(room, b.admitted()));
    assert!(
        !b_tabs(&b_tree, room).iter().any(|(id, _)| *id == notes),
        "the share ended, and A's tab with it: {:?}",
        b_tabs(&b_tree, room)
    );
    assert_eq!(
        crate::shell::peer_view::stream_tab_in(&b_tree, NOTES, notes, |_| true),
        None
    );
}

/// Presence in a room whose layout is personal: B sees that A is here, with
/// nothing shared and with a share, and only a share marks a tab.
#[test]
fn in_a_personal_room_a_member_sees_who_is_here_with_and_without_a_share() {
    let room = RoomId::parse(ROOM).expect("a room id");
    // A's windows carry the salt A's device key derives, by which the room
    // knows they run on A's device.
    let (salt, b_salt) = (
        tilo_desktop_model::ids::device_salt_of_key(&device_key(5)),
        0x0B52,
    );
    assert_ne!(salt, b_salt);
    let notes = (u64::from(salt) << 48) | 4;
    let (a_device, a_person) = (device_key(5), [0x22; 32]);
    let host = [0xa0; 32];
    let host_key = hex::encode(host);
    let mut governor = governed_room(ROOM, host, a_person, a_device);
    governor
        .put(automerge::ROOT, ROOM_LAYOUT_KEY, RoomLayout::Personal.id())
        .expect("the governor makes the layout personal");
    governor.commit();
    let mut net = FakeDocumentNet::new();
    let gov = net.peer("governor");
    let mut b = Member {
        peer: net.peer("b"),
        admitted: None,
    };
    net.publish(&gov, &mut governor);
    b.take_in(&mut net, &host_key);

    let mut b_tree = WorkspaceTree::bootstrap_default_with_salt(b_salt);
    let b_workspace = b_tree
        .join_native_room("Lounge", room)
        .expect("B's workspace of the room");
    b_tree.desktop.active = b_workspace;
    b_tree.project_room_document_for_test(room, b.admitted());
    let a_session = crate::shell::state::host_handle(&hex::encode(a_device))
        .expect("A's device is an endpoint");
    let b_session = 0xB0B;
    let roster = |tree: &WorkspaceTree| {
        let ws = tree
            .desktop
            .workspaces
            .iter()
            .find(|w| w.room() == room)
            .expect("B's workspace of the room");
        crate::shell::peer_presence::room_roster(
            ws,
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeSet::from([a_session]),
            b_session,
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
        )
    };
    let here = |tree: &WorkspaceTree| {
        roster(tree)
            .into_iter()
            .filter(|row| row.session == Some(a_session) && !row.invited)
            .count()
    };
    let marked = |tree: &mut WorkspaceTree| {
        for row in roster(tree) {
            tree.admit_collaborator_in_room(room, row);
        }
        let mut presence = ui::presence::PresenceCtx::default();
        crate::shell::sync_tab_presence_from_tree(tree, &mut presence);
        presence
            .peers_on(&ui::presence::PresenceKey::Tab {
                id: format!("{notes:x}"),
            })
            .len()
    };

    assert_eq!(here(&b_tree), 1, "with nothing shared, A is here");
    assert_eq!(marked(&mut b_tree), 0, "and no tab of B's wears A");

    share(&mut governor, &hex::encode(a_person), NOTES, notes);
    net.publish(&gov, &mut governor);
    b.take_in(&mut net, &host_key);
    b_tree.project_room_document_for_test(room, b.admitted());
    assert_eq!(here(&b_tree), 1, "with Notes shared, A is here");
    assert_eq!(marked(&mut b_tree), 1, "and A's shared Notes wears A");
}
