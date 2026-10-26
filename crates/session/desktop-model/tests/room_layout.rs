//! A room's layout document carries one workspace, every device publishing
//! only its own entries, and reads it back at any heads.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed lookup is the assertion"
)]

use automerge::AutoCommit;
use tilo_automerge::ApplyIncoming as _;
use tilo_desktop_model::layout::{Strip, Tab, TileContent, WindowGroup};
use tilo_desktop_model::ownership::SharedDesktop;
use tilo_desktop_model::projection::LayoutRequest;
use tilo_desktop_model::{
    DesktopTree, RoomId, StripId, WindowWidth, Workspace, WorkspaceId, compose_id, room_layout,
};

const ADA: u16 = 0x0ADA;
const BEN: u16 = 0x0BEB;

fn tab(id: u64, title: &str) -> Tab {
    Tab {
        id,
        title: title.into(),
        tile: TileContent::LocalApp {
            app_id: format!("so.tilo.{}", title.to_lowercase()),
            label: title.into(),
            note: String::new(),
        },
        app_title: None,
    }
}

/// One strip of `salt`'s: strip `n`, one group, one window titled `title`.
fn strip_of(salt: u16, n: u64, title: &str) -> Strip {
    let id = |base: u64| compose_id(salt, base);
    Strip {
        id: StripId(id(n)),
        name: format!("strip {n}"),
        window_groups: vec![WindowGroup {
            id: id(n + 10),
            tabs: vec![tab(id(n + 20), title)],
            active_tab: id(n + 20),
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    }
}

fn tab_id(salt: u16, n: u64) -> u64 {
    compose_id(salt, n + 20)
}

fn layout(room: RoomId, strips: Vec<Strip>) -> SharedDesktop {
    let mut workspace = Workspace::in_room(WorkspaceId::of_room(room), room, "Studio");
    workspace.strips = strips;
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

fn room() -> RoomId {
    RoomId::parse("6ba7b810-9dad-11d1-80b4-00c04fd430c8").unwrap()
}

fn titles(desktop: &SharedDesktop) -> Vec<String> {
    desktop.tree.workspaces[0]
        .strips
        .iter()
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .map(|tab| tab.title.clone())
        .collect()
}

/// A page's copy of the room, its own writes published onto `room_doc` the
/// way a peer's batch reaches it.
fn fork(room_doc: &AutoCommit) -> AutoCommit {
    AutoCommit::load(&room_doc.save()).unwrap()
}

fn publish(room_doc: &mut AutoCommit, page: &AutoCommit, since: Vec<automerge::ChangeHash>) {
    room_doc
        .load_incoming(&page.save_after(&since))
        .expect("the room takes the page's change");
}

/// Two devices publish into the same room: both windows are there, each
/// under the namespace it was minted in, and each says who opened it.
#[test]
fn two_devices_windows_merge_and_each_keeps_its_owner() {
    let room = room();
    let mut room_doc = AutoCommit::new();
    room_layout::write(
        &mut room_doc,
        &layout(room, vec![strip_of(ADA, 1, "Notes")]),
        ADA,
        Some("ada"),
    )
    .unwrap();
    room_doc.commit();

    let mut ben = fork(&room_doc);
    let since = ben.get_heads();
    let mut ben_layout = room_layout::read(&ben, None).expect("the room's layout");
    ben_layout.tree.workspaces[0]
        .strips
        .push(strip_of(BEN, 1, "Chat"));
    room_layout::write(&mut ben, &ben_layout, BEN, Some("ben")).unwrap();
    publish(&mut room_doc, &ben, since);

    let read = room_layout::read(&room_doc, None).unwrap();
    assert_eq!(titles(&read), ["Notes", "Chat"]);
    assert_eq!(
        read.authors.get(&tab_id(ADA, 1)).map(String::as_str),
        Some("ada")
    );
    assert_eq!(
        read.authors.get(&tab_id(BEN, 1)).map(String::as_str),
        Some("ben")
    );
}

/// A page that never saw a colleague's window writes only its own entries:
/// the colleague's window is still there afterwards, and nothing of the
/// page's own vanished.
#[test]
fn a_page_writing_an_old_layout_keeps_the_others_windows() {
    let room = room();
    let mut room_doc = AutoCommit::new();
    room_layout::write(
        &mut room_doc,
        &layout(room, vec![strip_of(ADA, 1, "Notes")]),
        ADA,
        Some("ada"),
    )
    .unwrap();
    room_doc.commit();
    // Ada's page before Ben opened his window.
    let mut ada = fork(&room_doc);

    let mut ben = fork(&room_doc);
    let since = ben.get_heads();
    let mut ben_layout = room_layout::read(&ben, None).unwrap();
    ben_layout.tree.workspaces[0]
        .strips
        .push(strip_of(BEN, 1, "Chat"));
    room_layout::write(&mut ben, &ben_layout, BEN, Some("ben")).unwrap();
    publish(&mut room_doc, &ben, since);

    // Ada closes her window, having seen only her own.
    let ada_since = ada.get_heads();
    let mut ada_layout = room_layout::read(&ada, None).unwrap();
    ada_layout.tree.workspaces[0].strips.clear();
    assert!(room_layout::write(&mut ada, &ada_layout, ADA, Some("ada")).unwrap());
    publish(&mut room_doc, &ada, ada_since);

    let read = room_layout::read(&room_doc, None).unwrap();
    assert_eq!(titles(&read), ["Chat"], "Ben's window survived Ada's write");
    assert!(
        read.authors.get(&tab_id(ADA, 1)).is_none(),
        "Ada's window is gone, not left as a ghost"
    );
}

/// A host that shows the room's history reads the layout as it stood at a
/// change: those heads hold only what had been published by then.
#[test]
fn the_layout_reads_as_it_stood_at_any_heads() {
    let room = room();
    let mut doc = AutoCommit::new();
    room_layout::write(
        &mut doc,
        &layout(room, vec![strip_of(ADA, 1, "Notes")]),
        ADA,
        Some("ada"),
    )
    .unwrap();
    doc.commit();
    let first = doc.get_heads();
    let mut two = layout(room, vec![strip_of(ADA, 1, "Notes")]);
    two.tree.workspaces[0]
        .strips
        .push(strip_of(ADA, 2, "Sheets"));
    room_layout::write(&mut doc, &two, ADA, Some("ada")).unwrap();
    doc.commit();

    let then = room_layout::read(&doc, Some(&first)).expect("a layout at those heads");
    assert_eq!(titles(&then), ["Notes"]);
    assert!(
        then.authors.get(&tab_id(ADA, 2)).is_none(),
        "the second window was not there yet"
    );
    assert_eq!(
        titles(&room_layout::read(&doc, None).unwrap()),
        ["Notes", "Sheets"]
    );
}

/// The record a window shows travels with the window, written by the device
/// that owns it; a later layout naming another record does not rebind it.
#[test]
fn a_window_keeps_the_record_its_owner_bound() {
    let room = room();
    let mut doc = AutoCommit::new();
    let mut first = layout(room, vec![strip_of(ADA, 1, "Notes")]);
    first.records = vec![(tab_id(ADA, 1), "note-1".into())];
    room_layout::write(&mut doc, &first, ADA, Some("ada")).unwrap();
    let mut second = first.clone();
    second.records = vec![(tab_id(ADA, 1), "note-2".into())];
    room_layout::write(&mut doc, &second, ADA, Some("ada")).unwrap();
    let read = room_layout::read(&doc, None).unwrap();
    assert_eq!(
        read.records,
        vec![(tab_id(ADA, 1), "note-1".to_owned())],
        "a binding is permanent"
    );
}

/// A document of the shape an older build wrote describes no room, and is
/// refused rather than read as an empty one.
#[test]
fn an_older_document_is_refused() {
    let mut doc = AutoCommit::new();
    let workspace = layout(room(), vec![strip_of(ADA, 1, "Notes")]);
    tilo_desktop_model::projection::project_owned_contents(
        &mut doc,
        &workspace.tree.workspaces[0].strips,
        ADA,
        None,
        tilo_desktop_model::projection::EmptyStrips::Skip,
    )
    .unwrap();
    doc.commit();
    assert!(room_layout::read(&doc, None).is_none());
}

fn request(command: &[u8]) -> LayoutRequest {
    LayoutRequest {
        id: LayoutRequest::id("ada", BEN, 1_000, command),
        from: "ada".into(),
        to_salt: BEN,
        at: 1_000,
        command: command.to_vec(),
    }
}

/// A request for another device's window is room state: it waits for that
/// device, is applied once, and a restart neither loses nor repeats it.
#[test]
fn a_request_waits_for_its_owner_and_is_applied_once() {
    let mut doc = AutoCommit::new();
    let close = request(b"{\"CloseTab\":1}");
    assert!(room_layout::request(&mut doc, &close).unwrap());
    assert!(
        !room_layout::request(&mut doc, &close).unwrap(),
        "asking twice is the same request"
    );
    assert!(
        room_layout::pending(&doc, ADA).is_empty(),
        "not Ada's to apply"
    );
    assert_eq!(room_layout::pending(&doc, BEN), vec![close.clone()]);

    assert!(
        room_layout::claim(&mut doc, BEN, &close).unwrap(),
        "the owner applies it"
    );
    assert!(
        !room_layout::claim(&mut doc, BEN, &close).unwrap(),
        "never twice"
    );
    doc.commit();
    assert!(room_layout::pending(&doc, BEN).is_empty());
    assert_eq!(
        room_layout::applied(&doc, BEN)
            .into_iter()
            .collect::<Vec<_>>(),
        vec![close.id.clone()]
    );
    assert!(room_layout::applied(&doc, ADA).is_empty());
    assert!(
        !room_layout::claim(&mut doc, ADA, &close).unwrap_or(false),
        "another namespace cannot claim someone else's request"
    );

    // A restarted owner loads the document: the mark is still there.
    let reloaded = AutoCommit::load(&doc.save()).unwrap();
    assert!(room_layout::pending(&reloaded, BEN).is_empty());
    assert!(!room_layout::claim(&mut doc, BEN, &close).unwrap());
}

/// Requests are retained room state: one made while its owner was away is
/// still there when the owner's page next reads the document.
#[test]
fn a_request_outlives_the_owner_being_offline() {
    let room = room();
    let mut doc = AutoCommit::new();
    room_layout::write(
        &mut doc,
        &layout(room, vec![strip_of(BEN, 1, "Chat")]),
        BEN,
        Some("ben"),
    )
    .unwrap();
    doc.commit();
    let close = request(b"{\"CloseTab\":2}");
    room_layout::request(&mut doc, &close).unwrap();
    doc.commit();

    let saved = doc.save();
    drop(doc);
    let mut owner = AutoCommit::load(&saved).unwrap();
    for pending in room_layout::pending(&owner, BEN) {
        assert!(
            room_layout::claim(&mut owner, BEN, &pending).unwrap(),
            "applied once when the owner next holds the room"
        );
        owner.commit();
    }
    let reloaded = AutoCommit::load(&owner.save()).unwrap();
    assert!(room_layout::pending(&reloaded, BEN).is_empty());
    assert_eq!(
        room_layout::requests(&reloaded).len(),
        1,
        "the request stays as room state"
    );
}
