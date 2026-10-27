//! A window one member opens in a room whose layout is shared reaches a
//! device that joins the room afterwards as a tab, and the window's stream
//! finds that tab there: the whole way the tab travels, below the
//! integration level, on the shared fake net.
//!
//! Device A, a member of a room a host governs, publishes its windows into
//! the room's document ([`plan`]); the governor admits them in A's
//! topology namespace. Device B joins later: the governor starts the room
//! over from a base, as it does on every admission, and the room's sync
//! carries that base to its members. Each member takes what arrives through
//! the one admission rule every receiver applies
//! ([`tilo_peer_state::room::admit_room_changes`]), B's tree folds the room's
//! windows into its workspace, and a stream naming A's tab mounts into it
//! ([`crate::shell::peer_view::stream_tab_in`]).

use super::*;
use crate::shell::state::{RoomId, StripId, WorkspaceTree};
use automerge::AutoCommit;
use tilo_peer_state::room::{
    AuthorityPin, ContentOwnership, RoomAdmission, Standing, Writer, admit_room_changes,
};
use tilo_test_env::{FakeDocumentNet, Peer};

const ROOM: &str = "6f1d2c3b-4a59-4e8d-9c7b-1a2b3c4d5e6f";
const CHAT: &str = "so.tilo.chat";
const NOTES: &str = "so.tilo.notes";
const NOW_NS: u64 = 1_000_000_000_000;

/// The changes one message of the room's sync carries, each on its own as a
/// room's sync sends them, whether or not the receiver holds what they build
/// on yet.
fn changes_in(bytes: &[u8]) -> Vec<Vec<u8>> {
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

/// What a member's device holds of the room: the document it admitted from
/// the governor, nothing until it joined.
struct Member {
    peer: Peer,
    admitted: Option<AutoCommit>,
}

impl Member {
    /// Take every message the room's sync brought this device since it last
    /// looked, as the governor's, through the admission every receiver
    /// applies.
    fn take_in(&mut self, net: &mut FakeDocumentNet, host_key: &str) {
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
                contents: ContentOwnership::Registry,
                now_ns: NOW_NS,
            },
            &arrived,
        )
        .unwrap_or_else(|refusal| panic!("{} refused the governor's room: {refusal}", self.peer));
        self.admitted = Some(admitted.candidate);
    }

    fn admitted(&mut self) -> &mut AutoCommit {
        self.admitted.as_mut().expect("the device holds the room")
    }

    /// The device as the net's convergence check reads it: its name on the
    /// net and what it admitted.
    fn on_net(&mut self) -> (&Peer, &mut AutoCommit) {
        (
            &self.peer,
            self.admitted.as_mut().expect("the device holds the room"),
        )
    }
}

/// A's workspace of the room with `tabs` open, each a window of an app,
/// all in one window group of one strip, as A's tree holds them.
fn a_tree(room: RoomId, salt: u16, tabs: &[(u64, &str, u64)]) -> WorkspaceTree {
    let own = |counter: u64| (u64::from(salt) << 48) | counter;
    let mut tree = WorkspaceTree::bootstrap_default_with_salt(salt);
    tree.join_native_room("Garden", room)
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
            tabs: tabs
                .iter()
                .map(|&(id, app, surface)| {
                    tab(
                        id,
                        TileContent::Foreign {
                            app_id: app.into(),
                            surface_id: surface,
                        },
                    )
                })
                .collect(),
            active_tab: tabs.first().map_or(0, |&(id, ..)| id),
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    }];
    tree
}

/// The tab B shows the stream of `app`'s window that A's share names `tab`
/// in, once B's tree took in what B admitted of the room.
fn b_mounts(b_tree: &WorkspaceTree, app: &str, tab: u64) -> Option<u64> {
    crate::shell::peer_view::stream_tab_in(b_tree, app, tab, |_| true)
}

/// The room document path of tab `tab` in A's one strip and group.
fn tab_path(salt: u16, tab: u64) -> String {
    let own = |counter: u64| (u64::from(salt) << 48) | counter;
    format!(
        "contents.{}.groups.{}.tabs.{}.title",
        tilo_desktop_model::projection::contents_key(own(1)),
        tilo_desktop_model::projection::contents_key(own(2)),
        tilo_desktop_model::projection::contents_key(tab),
    )
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one journey across two devices and their governor, told in order"
)]
fn a_window_opened_before_and_after_a_device_joins_reaches_it_as_a_tab_its_stream_mounts_in() {
    let room = RoomId::parse(ROOM).expect("a room id");
    let (salt, b_salt) = (0x0A51, 0x0B52);
    let own = |counter: u64| (u64::from(salt) << 48) | counter;
    let (a_device, a_person) = ([0x5a; 32], [0x22; 32]);
    let host = [0xa0; 32];
    let (host_key, a_key) = (hex::encode(host), hex::encode(a_device));
    let mut governor = governed_room(ROOM, host, a_person, (salt, a_device));
    assert!(
        !tilo_desktop_model::projection::room_layout(&governor).is_personal(),
        "an invite-only room shares its layout"
    );
    let admit_from_a = |before: &AutoCommit, changes: &[Vec<u8>]| {
        admit_room_changes(
            RoomAdmission {
                before: Some(before),
                room: ROOM,
                source: Writer {
                    key: &a_key,
                    standing: Standing::Member {
                        person: a_person,
                        foreign: false,
                    },
                },
                authority: AuthorityPin::Device(&host_key),
                contents: ContentOwnership::Registry,
                now_ns: NOW_NS,
            },
            changes,
        )
        .unwrap_or_else(|refusal| panic!("the governor refused A's windows: {refusal}"))
        .candidate
    };
    let (mut settled, mut pending) = (None, Vec::new());

    // A opens Chat in the room before anyone else is in it, and publishes
    // the window to the governor, which admits it in A's namespace. A holds
    // what the governor admitted.
    let chat = own(3);
    let before = a_tree(room, salt, &[(chat, CHAT, 9)]);
    let (published_salt, windows) = windows_to_publish(&before, room, Kept::All);
    assert_eq!(published_salt, salt);
    let Ok(Some(Plan::Publish(changes))) = plan(
        &governor,
        Look {
            device: a_device,
            windows: &windows,
            salt,
        },
        &mut settled,
        &mut pending,
    ) else {
        panic!("A publishes the window the room does not show yet");
    };
    governor = admit_from_a(&governor, &changes);
    assert!(
        matches!(
            plan(
                &governor,
                Look {
                    device: a_device,
                    windows: &windows,
                    salt
                },
                &mut settled,
                &mut pending
            ),
            Ok(None)
        ),
        "once admitted, A has nothing more to send"
    );

    // B joins. The governor starts the room over from a base, and the
    // room's sync carries it to its members: A, and B, which was not there
    // when the window was published.
    let mut net = FakeDocumentNet::new();
    let gov = net.peer("governor");
    let mut a = Member {
        peer: net.peer("a"),
        admitted: Some(governor.clone()),
    };
    let mut b = Member {
        peer: net.peer("b"),
        admitted: None,
    };
    let mut based = tilo_peer_state::room::base::rebase(&governor, governor.get_actor().clone())
        .expect("the governor's base");
    net.publish(&gov, &mut based);
    a.take_in(&mut net, &host_key);
    b.take_in(&mut net, &host_key);
    net.assert_converged(
        &mut [(&gov, &mut based), a.on_net(), b.on_net()],
        &[&tab_path(salt, chat)],
    );

    let mut b_tree = WorkspaceTree::bootstrap_default_with_salt(b_salt);
    b_tree
        .join_native_room("Garden", room)
        .expect("B's workspace of the room");
    assert_eq!(
        b_mounts(&b_tree, CHAT, chat),
        None,
        "before B holds the room, A's stream has no tab to show in"
    );
    assert!(
        b_tree.project_room_document_for_test(room, b.admitted()),
        "the room's windows arrive in B's workspace"
    );
    assert_eq!(
        b_mounts(&b_tree, CHAT, chat),
        Some(chat),
        "A's Chat, opened before B joined, is a tab on B its stream shows in"
    );

    // After B joined, A opens Notes beside Chat. A builds on the base it
    // holds now; the governor admits it, and the room's sync carries it to
    // both members.
    let notes = own(4);
    let after = a_tree(room, salt, &[(chat, CHAT, 9), (notes, NOTES, 10)]);
    let (_, windows) = windows_to_publish(&after, room, Kept::All);
    let Ok(Some(Plan::Publish(changes))) = plan(
        a.admitted(),
        Look {
            device: a_device,
            windows: &windows,
            salt,
        },
        &mut settled,
        &mut pending,
    ) else {
        panic!("A publishes the window it opened after B joined");
    };
    based = admit_from_a(&based, &changes);
    net.publish(&gov, &mut based);
    a.take_in(&mut net, &host_key);
    b.take_in(&mut net, &host_key);
    net.assert_converged(
        &mut [(&gov, &mut based), a.on_net(), b.on_net()],
        &[&tab_path(salt, chat), &tab_path(salt, notes)],
    );
    assert!(b_tree.project_room_document_for_test(room, b.admitted()));
    assert_eq!(
        b_mounts(&b_tree, NOTES, notes),
        Some(notes),
        "A's Notes, opened after B joined, is a tab on B its stream shows in"
    );
    assert_eq!(b_mounts(&b_tree, CHAT, chat), Some(chat), "Chat stays");
}

/// In a room whose layout is personal no member's window becomes a tab on
/// another device, so a stream of one has nowhere to show: each member's
/// windows stay on their device. A publishes none, and the governor refuses
/// the window contents of a member that would.
#[test]
fn in_a_room_whose_layout_is_personal_a_members_window_reaches_nobody_as_a_tab() {
    use automerge::transaction::Transactable as _;
    let room = RoomId::parse(ROOM).expect("a room id");
    let salt = 0x0A51;
    let own = |counter: u64| (u64::from(salt) << 48) | counter;
    let (a_device, a_person) = ([0x5a; 32], [0x22; 32]);
    let host = [0xa0; 32];
    let (host_key, a_key) = (hex::encode(host), hex::encode(a_device));
    let mut governor = governed_room(ROOM, host, a_person, (salt, a_device));
    governor
        .put(
            automerge::ROOT,
            tilo_desktop_model::projection::ROOM_LAYOUT_KEY,
            tilo_desktop_model::projection::RoomLayout::Personal.id(),
        )
        .expect("the governor makes the layout personal");
    governor.commit();

    let tree = a_tree(room, salt, &[(own(3), CHAT, 9)]);
    let (_, windows) = windows_to_publish(&tree, room, Kept::All);
    let (mut settled, mut pending) = (None, Vec::new());
    let Ok(Some(Plan::Publish(changes))) = plan(
        &governor,
        Look {
            device: a_device,
            windows: &windows,
            salt,
        },
        &mut settled,
        &mut pending,
    ) else {
        panic!("the plan alone does not read the layout; the session does");
    };
    let refused = admit_room_changes(
        RoomAdmission {
            before: Some(&governor),
            room: ROOM,
            source: Writer {
                key: &a_key,
                standing: Standing::Member {
                    person: a_person,
                    foreign: false,
                },
            },
            authority: AuthorityPin::Device(&host_key),
            contents: ContentOwnership::Registry,
            now_ns: NOW_NS,
        },
        &changes,
    );
    assert!(
        matches!(
            refused,
            Err(tilo_peer_state::room::RoomRefusal::PersonalLayout)
        ),
        "the governor takes no member's window into a personal room: {:?}",
        refused.map(|_| ())
    );
}
