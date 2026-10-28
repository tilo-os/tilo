//! A Tilo app's window a member shares with a room whose layout is
//! personal, on the shared fake net: the other member opens the same app on
//! the same resource, as a tab of the sharer's, and nothing of it streams.
//!
//! Device A and device B are members of a room a host governs, whose layout
//! is personal. A has Chat and Notes open, Notes at a note of the room's
//! notebook, and shares Notes: the host records the share of the window and
//! of what it shows ([`tilo_desktop_model::window_share`]), and the room's
//! sync carries both to the members. B shows A's Notes as a tab under A's
//! tab id, marked with A's presence, that opens Notes on the room's notebook
//! at A's note; the notebook itself travels on its own route, so an edit A
//! makes reaches B's copy. When the share ends, the window stays B's if B
//! used it, and closes otherwise.

use super::personal_window_shares::{CHAT, Member, NOTES, ROOM, b_tabs, device_key, stop};
use super::*;
use crate::shell::state::{RoomId, StripId, WorkspaceTree};
use automerge::AutoCommit;
use automerge::transaction::Transactable as _;
use tilo_action_policy::share::{ShareAccess, share_grants};
use tilo_desktop_model::projection::{
    ROOM_LAYOUT_KEY, RoomLayout, project_room_policy, room_policy_of,
};
use tilo_desktop_model::window_share::{
    WindowShows, shows_node, window_node, window_share_id, window_shows_share_id,
};
use tilo_resource::Replica;
use tilo_test_env::FakeDocumentNet;

/// The kind of resource Notes keeps in its record of the room.
const NOTEBOOK: &str = "notes.notebook";

/// The governor records `person`'s share of the Tilo app window `tab` of
/// `app`, with what it shows, as the room's host writes the two resource
/// shares the sharer asks for.
fn share_app_window(governor: &mut AutoCommit, person: &str, tab: u64, shows: &WindowShows) {
    let mut policy = room_policy_of(governor, ROOM);
    for (id, node) in [
        (
            window_shows_share_id(tab),
            shows_node(ROOM, NOTES, tab, NOTEBOOK, shows),
        ),
        (window_share_id(tab), window_node(ROOM, NOTES, tab)),
    ] {
        policy.grants.extend(
            share_grants(ROOM, person, &id, &node, ShareAccess::View)
                .expect("one resource of the room"),
        );
    }
    project_room_policy(governor, &policy).expect("the room's authority");
    governor.commit();
}

/// A's workspace of the room: Chat and Notes, both launched here as Tilo
/// packages.
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
            active_tab: notes,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    }];
    tree.remember_package_launch(chat, CHAT.into());
    tree.remember_package_launch(notes, NOTES.into());
    tree
}

/// The text of the note `page` in `notebook`.
fn note_text(notebook: &AutoCommit, page: notes::model::PageId) -> String {
    notes::read::page_blocks(notebook, page)
        .into_iter()
        .map(|block| block.text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The tab of B's workspace showing a window of A's own namespace.
fn a_tab_on_b(tree: &WorkspaceTree, room: RoomId, salt: u16) -> Option<(u64, TileContent)> {
    b_tabs(tree, room)
        .into_iter()
        .find(|(id, _)| tilo_desktop_model::peer_salt_of(*id) == salt)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one journey across two devices, their governor and the notebook's route, told in order"
)]
fn a_shared_tilo_app_window_opens_the_same_note_on_the_others_with_the_sharers_presence() {
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

    // The room's notebook, Notes' own record of the room, on its own route:
    // A has a note open in it, and B holds a copy.
    let mut notebook_route = FakeDocumentNet::new();
    let (a_route, b_route) = (notebook_route.peer("a"), notebook_route.peer("b"));
    let mut a_notebook = Replica::<notes::notebook::Notebook>::new();
    let page = notes::model::PageId::from_u128(0x7e57);
    a_notebook
        .apply_action(&notes::actions::create_page_action(page, None, "Agenda"))
        .expect("A writes a note");
    a_notebook
        .apply_action(&notes::actions::write_body_action(
            page,
            &[ui::widget::BlockContent::paragraph("p1", "Lunch at noon")],
        ))
        .expect("A writes its body");
    let mut b_notebook = AutoCommit::new();
    notebook_route.publish(&a_route, a_notebook.doc_mut());
    notebook_route.deliver_into(&mut [(&b_route, &mut b_notebook)]);
    assert_eq!(note_text(&b_notebook, page), "Lunch at noon");

    let mut a_tree = a_tree(room, salt, chat, notes);
    a_tree.project_room_document_for_test(room, a.admitted());
    let mut b_tree = WorkspaceTree::bootstrap_default_with_salt(b_salt);
    let b_workspace = b_tree
        .join_native_room("Lounge", room)
        .expect("B's workspace of the room");
    b_tree.desktop.active = b_workspace;
    b_tree.project_room_document_for_test(room, b.admitted());
    assert_eq!(
        a_tab_on_b(&b_tree, room, salt),
        None,
        "nothing of A's on B yet"
    );

    // A shares Notes, at the note it has open. The host records the window
    // and what it shows, and the room's sync carries them to both members.
    let shows = WindowShows {
        record: String::new(),
        item: Some(page.to_string()),
    };
    share_app_window(&mut governor, &hex::encode(a_person), notes, &shows);
    net.publish(&gov, &mut governor);
    a.take_in(&mut net, &host_key);
    b.take_in(&mut net, &host_key);
    a_tree.project_room_document_for_test(room, a.admitted());
    let now_ms = tilo_clock::unix_now_ms();
    assert!(a_tree.is_window_shared(room, notes, now_ms));
    assert!(
        !a_tree.streams_shared_window(room, notes, now_ms),
        "A streams no pixels of a Tilo app's window"
    );

    assert!(b_tree.project_room_document_for_test(room, b.admitted()));
    let (id, tile) = a_tab_on_b(&b_tree, room, salt).expect("B shows A's Notes");
    assert_eq!(id, notes, "under A's own tab id");
    assert!(
        matches!(&tile, TileContent::PeerWindow { app_id, note, .. } if app_id == NOTES && note.is_empty()),
        "as Notes, to be opened here: {tile:?}"
    );
    assert_eq!(
        b_tabs(&b_tree, room).len(),
        1,
        "only the shared window of A's reaches B: {:?}",
        b_tabs(&b_tree, room)
    );
    assert_eq!(
        crate::shell::peer_view::stream_tab_in(&b_tree, NOTES, notes, |_| true),
        None,
        "no stream of A's Notes has anywhere to show"
    );
    // The same resource: Notes' own record of the room, at A's note.
    let record = b_tree
        .room_tab_record(room, notes)
        .and_then(|route| ui::record::RecordId::from_route(&route))
        .expect("B's tab names the record it opens");
    assert!(record.is_package_primary(), "the room's notebook");
    let shared = b_tree
        .shared_windows(room, now_ms)
        .find(|share| share.tab == notes)
        .expect("B reads A's share");
    assert_eq!(shared.shows.as_ref(), Some(&shows), "at A's note");
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
    // The frame keeps that mark on the tab, and takes it off with the share.
    let mut frame_presence = ui::presence::PresenceCtx::default();
    let mut marks = Vec::new();
    assert!(crate::shell::mark_shared_windows(
        &b_tree,
        &mut frame_presence,
        &mut marks
    ));
    let tab_key = ui::presence::PresenceKey::Tab {
        id: format!("{notes:x}"),
    };
    assert_eq!(
        frame_presence.peers_on(&tab_key),
        &[ui::permissions::UserId("ana".into())]
    );
    assert!(
        !crate::shell::mark_shared_windows(&b_tree, &mut frame_presence, &mut marks),
        "restated, nothing changes"
    );

    // Notes opens here into that tab, and A's edit to the note reaches the
    // notebook B's Notes shows.
    let surface = 0xB0_0001;
    crate::shell::tab_binding::adopt_toplevel_at(
        &mut b_tree,
        surface,
        Some(NOTES.into()),
        crate::shell::tab_binding::Placement::IntoTab(notes),
    );
    b_tree.remember_package_launch(notes, NOTES.into());
    assert!(matches!(
        a_tab_on_b(&b_tree, room, salt),
        Some((id, TileContent::Foreign { surface_id, .. })) if id == notes && surface_id == surface
    ));
    b_tree.project_room_document_for_test(room, b.admitted());
    assert!(
        a_tab_on_b(&b_tree, room, salt).is_some(),
        "the open window stays while the share lasts"
    );
    assert!(
        !b_tree.streams_shared_window(room, notes, now_ms),
        "B streams nothing of a window of A's"
    );
    a_notebook
        .apply_action(&notes::actions::write_body_action(
            page,
            &[ui::widget::BlockContent::paragraph(
                "p1",
                "Lunch at noon, then the review",
            )],
        ))
        .expect("A edits the note");
    notebook_route.publish(&a_route, a_notebook.doc_mut());
    notebook_route.deliver_into(&mut [(&b_route, &mut b_notebook)]);
    assert_eq!(
        note_text(&b_notebook, page),
        "Lunch at noon, then the review",
        "A's edit shows in the note B's Notes has open"
    );

    // B types into it, so the window is B's to keep: when A stops sharing,
    // it stays, as B's own window.
    assert!(b_tree.shared_window_work(&[surface]));
    assert_eq!(
        b_tree.note_used_and_take_ended(&[surface]),
        Vec::<u64>::new()
    );
    assert!(!b_tree.shared_window_work(&[surface]), "noted once");
    stop(&mut governor, notes);
    net.publish(&gov, &mut governor);
    a.take_in(&mut net, &host_key);
    b.take_in(&mut net, &host_key);
    assert!(b_tree.project_room_document_for_test(room, b.admitted()));
    assert_eq!(a_tab_on_b(&b_tree, room, salt), None, "no window of A's");
    assert!(crate::shell::mark_shared_windows(
        &b_tree,
        &mut frame_presence,
        &mut marks
    ));
    assert!(
        frame_presence.peers_on(&tab_key).is_empty(),
        "the mark goes with the share"
    );
    let kept: Vec<_> = b_tabs(&b_tree, room)
        .into_iter()
        .filter(|(_, tile)| matches!(tile, TileContent::Foreign { surface_id, .. } if *surface_id == surface))
        .collect();
    assert_eq!(
        kept.len(),
        1,
        "the used window stays: {:?}",
        b_tabs(&b_tree, room)
    );
    let (own_id, _) = kept[0].clone();
    assert_eq!(
        tilo_desktop_model::peer_salt_of(own_id),
        b_salt,
        "as B's own window"
    );
    assert_eq!(
        b_tree.package_launches.get(&own_id).map(String::as_str),
        Some(NOTES),
        "still Notes, launched here"
    );
    assert!(
        b_tree.note_used_and_take_ended(&[]).is_empty(),
        "nothing to close"
    );

    // A shares it again, and this time B leaves it alone: when the share
    // ends, the window B opened for it closes.
    share_app_window(&mut governor, &hex::encode(a_person), notes, &shows);
    net.publish(&gov, &mut governor);
    b.take_in(&mut net, &host_key);
    b_tree.project_room_document_for_test(room, b.admitted());
    let unused = 0xB0_0002;
    crate::shell::tab_binding::adopt_toplevel_at(
        &mut b_tree,
        unused,
        Some(NOTES.into()),
        crate::shell::tab_binding::Placement::IntoTab(notes),
    );
    stop(&mut governor, notes);
    net.publish(&gov, &mut governor);
    b.take_in(&mut net, &host_key);
    assert!(b_tree.project_room_document_for_test(room, b.admitted()));
    assert_eq!(
        a_tab_on_b(&b_tree, room, salt),
        None,
        "the unused window goes"
    );
    assert!(b_tree.shared_window_work(&[]));
    assert_eq!(
        b_tree.note_used_and_take_ended(&[]),
        [unused],
        "and the compositor is asked to close it"
    );
    assert!(
        b_tabs(&b_tree, room).iter().any(|(id, _)| *id == own_id),
        "B's kept window is untouched"
    );
}

/// The governor records that `person`'s Tilo app window `tab` now shows
/// `shows`, as the room's host writes a share made again under the same
/// share id: over the one before.
fn move_app_window(governor: &mut AutoCommit, person: &str, tab: u64, shows: &WindowShows) {
    let mut policy = room_policy_of(governor, ROOM);
    let id = window_shows_share_id(tab);
    policy
        .grants
        .retain(|grant| !tilo_action_policy::share::is_of_share(grant, &id));
    policy.grants.extend(
        share_grants(
            ROOM,
            person,
            &id,
            &shows_node(ROOM, NOTES, tab, NOTEBOOK, shows),
            ShareAccess::View,
        )
        .expect("one resource of the room"),
    );
    project_room_policy(governor, &policy).expect("the room's authority");
    governor.commit();
}

/// What A's Notes window shows at the note `page`.
fn at_note(page: &str) -> WindowShows {
    WindowShows {
        record: String::new(),
        item: Some(page.to_owned()),
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one journey across two devices, their governor and a restart, told in order"
)]
fn a_shared_tilo_app_window_follows_its_sharers_note_reads_as_theirs_and_stays_used_across_a_restart()
 {
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
    let ana = hex::encode(a_person);
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
    let (agenda, minutes, budget) = (
        notes::model::PageId::from_u128(0x7e57).to_string(),
        notes::model::PageId::from_u128(0x7e58).to_string(),
        notes::model::PageId::from_u128(0x7e59).to_string(),
    );

    // A shares Notes at the agenda, and B's Notes opens into A's tab.
    share_app_window(&mut governor, &ana, notes, &at_note(&agenda));
    net.publish(&gov, &mut governor);
    a.take_in(&mut net, &host_key);
    b.take_in(&mut net, &host_key);
    a_tree.project_room_document_for_test(room, a.admitted());
    b_tree.project_room_document_for_test(room, b.admitted());
    let surface = 0xB0_0001;
    crate::shell::tab_binding::adopt_toplevel_at(
        &mut b_tree,
        surface,
        Some(NOTES.into()),
        crate::shell::tab_binding::Placement::IntoTab(notes),
    );
    b_tree.remember_package_launch(notes, NOTES.into());
    assert!(b_tree.follows_shared_window(notes));
    assert!(
        !a_tree.follows_shared_window(notes),
        "A's own window follows nobody"
    );

    // B's Notes opens at A's note, through its own `open`, once.
    assert_eq!(
        b_tree.shared_window_opening(notes, None),
        Some(agenda.clone()),
        "B's Notes opens at the note A has open"
    );
    assert_eq!(
        b_tree.shared_window_opening(notes, None),
        Some(agenda.clone()),
        "asked again until it was opened there"
    );
    b_tree.shared_window_opened(notes, &agenda);
    assert_eq!(b_tree.shared_window_opening(notes, None), None);
    assert_eq!(
        b_tree.shared_window_opening(notes, Some(&agenda)),
        None,
        "it is there"
    );

    // A moves to the minutes: B follows.
    move_app_window(&mut governor, &ana, notes, &at_note(&minutes));
    net.publish(&gov, &mut governor);
    b.take_in(&mut net, &host_key);
    b_tree.project_room_document_for_test(room, b.admitted());
    assert_eq!(
        b_tree
            .shared_windows(room, tilo_clock::unix_now_ms())
            .find(|share| share.tab == notes)
            .and_then(|share| share.shows.clone()),
        Some(at_note(&minutes)),
        "the share says where A's window is now"
    );
    assert_eq!(
        b_tree.shared_window_opening(notes, Some(&agenda)),
        Some(minutes.clone()),
        "B's window follows A to the minutes"
    );
    b_tree.shared_window_opened(notes, &minutes);
    assert_eq!(
        b_tree.shared_window_opening(notes, Some(&agenda)),
        None,
        "still on its way there: not moved by B"
    );
    assert_eq!(b_tree.shared_window_opening(notes, Some(&minutes)), None);
    assert!(b_tree.follows_shared_window(notes));

    // The tab reads as A's: her name and face, while she is here.
    let a_session = crate::shell::state::host_handle(&hex::encode(a_device))
        .expect("A's device is an endpoint");
    assert!(b_tree.admit_collaborator_in_room(
        room,
        crate::shell::state::Collaborator::new("ana", "Ana").with_session(a_session),
    ));
    let marks =
        crate::shell::overlay_host::windows_shared_by_others(&b_tree, tilo_clock::unix_now_ms());
    assert_eq!(
        marks.get(&notes),
        Some(&ui::widget::SharedBy {
            sharer: (ui::permissions::UserId("ana".into()), "Ana".into()),
        }),
        "B's tab of A's window says it is Ana's"
    );
    assert!(
        crate::shell::overlay_host::windows_shared_by_others(&a_tree, tilo_clock::unix_now_ms(),)
            .is_empty(),
        "A's own window is not marked as someone else's"
    );

    // B moves the window to the budget itself: from then on it stays there,
    // whatever A opens.
    assert_eq!(b_tree.shared_window_opening(notes, Some(&budget)), None);
    assert!(
        !b_tree.follows_shared_window(notes),
        "B moved it: it no longer follows A"
    );
    move_app_window(&mut governor, &ana, notes, &at_note(&agenda));
    net.publish(&gov, &mut governor);
    b.take_in(&mut net, &host_key);
    b_tree.project_room_document_for_test(room, b.admitted());
    assert_eq!(
        b_tree.shared_window_opening(notes, Some(&budget)),
        None,
        "B's window stays where B put it"
    );

    // B types into it, and the device restarts while the share lasts: the
    // window is still B's to keep, and still stays where B put it.
    assert!(b_tree.note_used_and_take_ended(&[surface]).is_empty());
    let dir = tempfile::tempdir().expect("a state directory");
    let path = dir.path().join("state.json");
    crate::shell::persist::save_tree_to(&b_tree, &path).expect("B's layout is saved");
    let mut restored =
        crate::shell::persist::load_device_tree_from(&path, b_salt).expect("B's layout restored");
    restored.project_room_document_for_test(room, b.admitted());
    assert!(
        restored.desktop.workspaces.iter().any(|workspace| workspace
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .any(|tab| tab.id == notes)),
        "A's window is still in B's layout"
    );
    assert!(
        !restored.follows_shared_window(notes),
        "and still stays where B put it"
    );
    stop(&mut governor, notes);
    net.publish(&gov, &mut governor);
    b.take_in(&mut net, &host_key);
    assert!(restored.project_room_document_for_test(room, b.admitted()));
    let kept: Vec<u64> = b_tabs(&restored, room)
        .into_iter()
        .map(|(id, _)| id)
        .filter(|id| restored.package_launches.get(id).map(String::as_str) == Some(NOTES))
        .collect();
    assert_eq!(
        kept.len(),
        1,
        "the used window stays after the restart: {:?}",
        b_tabs(&restored, room)
    );
    assert_eq!(
        tilo_desktop_model::peer_salt_of(kept[0]),
        b_salt,
        "as B's own window"
    );
    assert!(
        restored.note_used_and_take_ended(&[]).is_empty(),
        "nothing to close"
    );
}
