//! A workspace's members arrange it together, native devices and browser
//! pages alike: each writes the room's layout record as it changes, offline
//! too, and folds in what the others wrote once it arrives. A window that
//! runs on one device is arranged from any of them, and nothing snaps back
//! when that device was away.
use super::super::*;
use automerge::AutoCommit;
use collab_types::DocumentMutation;

const PERSON: [u8; 32] = [0x7a; 32];
const LAPTOP: u16 = 0x1a1a;
const DESK: u16 = 0x2b2b;

/// The device key of the device minting in `salt`.
fn device_key(salt: u16) -> [u8; 32] {
    let seed = u8::try_from(salt & 0xff).unwrap_or(0);
    *iroh::SecretKey::from_bytes(&[seed; 32]).public().as_bytes()
}

fn key(salt: u16) -> String {
    hex::encode(device_key(salt))
}

/// One of the person's devices, minting in `salt`.
fn device(salt: u16) -> WorkspaceTree {
    let mut tree = WorkspaceTree::default();
    tree.set_peer_salt(salt);
    tree.local_person = Some(PERSON);
    tree.window_hosting = Some(WindowHosting {
        device: key(salt),
        device_label: format!("device {salt:x}"),
    });
    tree
}

fn tab(id: u64, tile: TileContent) -> Tab {
    Tab {
        id,
        title: format!("tab {id:x}"),
        tile,
        app_title: None,
    }
}

fn notes(id: u64) -> Tab {
    tab(
        id,
        TileContent::AppPlaceholder {
            kind: AppKind::Other("so.tilo.notes".into()),
            label: "Notes".into(),
        },
    )
}

/// A legacy window, live on the device that runs it.
fn editor(id: u64) -> Tab {
    tab(
        id,
        TileContent::Foreign {
            app_id: "org.gnome.TextEditor".into(),
            surface_id: 77,
        },
    )
}

fn group(id: u64, tabs: Vec<Tab>) -> WindowGroup {
    WindowGroup {
        id,
        active_tab: tabs.first().map_or(0, |tab| tab.id),
        tabs,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }
}

fn laptop_id(counter: u64) -> u64 {
    compose_id(LAPTOP, counter)
}

fn desk_id(counter: u64) -> u64 {
    compose_id(DESK, counter)
}

fn workspace_mut(tree: &mut WorkspaceTree, room: RoomId) -> &mut Workspace {
    tree.desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.room() == room)
        .expect("the room's workspace")
}

/// Where each window is, `(strip, group, tab)`, in order.
fn places(tree: &WorkspaceTree, room: RoomId) -> Vec<(u64, u64, u64)> {
    tree.desktop
        .workspaces
        .iter()
        .filter(|workspace| workspace.room() == room)
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| {
            strip.window_groups.iter().flat_map(move |group| {
                group
                    .tabs
                    .iter()
                    .map(move |tab| (strip.id.raw(), group.id, tab.id))
            })
        })
        .collect()
}

fn tile(tree: &WorkspaceTree, room: RoomId, id: u64) -> TileContent {
    tree.desktop
        .workspaces
        .iter()
        .filter(|workspace| workspace.room() == room)
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .find(|tab| tab.id == id)
        .map(|tab| tab.tile.clone())
        .expect("the window")
}

/// Move window `id` into group `to` of strip `strip`, made when absent.
fn move_tab(tree: &mut WorkspaceTree, room: RoomId, id: u64, strip: u64, to: u64) {
    let workspace = workspace_mut(tree, room);
    let mut moved = None;
    for group in workspace
        .strips
        .iter_mut()
        .flat_map(|strip| &mut strip.window_groups)
    {
        if let Some(index) = group.tabs.iter().position(|tab| tab.id == id) {
            moved = Some(group.tabs.remove(index));
        }
    }
    for strip in &mut workspace.strips {
        strip.window_groups.retain(|group| !group.tabs.is_empty());
    }
    let moved = moved.expect("the window");
    let strip = workspace
        .strips
        .iter_mut()
        .find(|candidate| candidate.id.raw() == strip)
        .expect("the strip");
    match strip.window_groups.iter_mut().find(|group| group.id == to) {
        Some(group) => group.tabs.push(moved),
        None => strip.window_groups.push(group(to, vec![moved])),
    }
}

fn close(tree: &mut WorkspaceTree, room: RoomId, id: u64) {
    for strip in &mut workspace_mut(tree, room).strips {
        for group in &mut strip.window_groups {
            group.tabs.retain(|tab| tab.id != id);
        }
        strip.window_groups.retain(|group| !group.tabs.is_empty());
    }
}

/// What `from` wrote to the room's layout record reaches `to`, as its
/// record route delivers it, and `to` folds it in.
fn hear(to: &mut WorkspaceTree, from: &mut WorkspaceTree, room: RoomId, from_salt: u16) {
    to.hear_layout(from, room, device_key(from_salt))
        .expect("the room admits it");
}

/// `to` takes in `mutation`, which the device `author` signed.
fn receive(
    to: &mut WorkspaceTree,
    room: RoomId,
    author: [u8; 32],
    mutation: &DocumentMutation,
) -> Result<(), String> {
    to.receive_layout(room, author, mutation)
}

/// The laptop's workspace: notes and a legacy editor on its main strip,
/// notes on a second one; the desk holds it as one of the person's.
fn two_devices() -> (WorkspaceTree, WorkspaceTree, RoomId) {
    let person = person_id_of(PERSON).expect("a person id");
    let mut laptop = device(LAPTOP);
    let (_, room) = laptop.create_hosted_workspace("Home");
    laptop.own_workspaces_for_test(|_| person.clone());
    workspace_mut(&mut laptop, room).strips = vec![
        Strip {
            id: StripId(laptop_id(1)),
            name: "Main".into(),
            window_groups: vec![
                group(laptop_id(2), vec![notes(laptop_id(3))]),
                group(laptop_id(4), vec![editor(laptop_id(5))]),
            ],
        },
        Strip {
            id: StripId(laptop_id(6)),
            name: "Later".into(),
            window_groups: vec![group(laptop_id(7), vec![notes(laptop_id(8))])],
        },
    ];
    let mut desk = device(DESK);
    assert!(desk.adopt_personal_room(room, "Home"));
    hear(&mut desk, &mut laptop, room, LAPTOP);
    assert_eq!(places(&desk, room), places(&laptop, room));
    (laptop, desk, room)
}

#[test]
fn a_move_made_while_the_window_s_device_is_away_sticks_and_applies_on_reconnect() {
    let (mut laptop, mut desk, room) = two_devices();

    // The laptop is away. At the desk the person moves the laptop's editor
    // into a group of its own on the other strip, and closes a laptop window.
    move_tab(&mut desk, room, laptop_id(5), laptop_id(6), desk_id(1));
    close(&mut desk, room, laptop_id(8));
    desk.capture_workspace_layout(room);
    let at_desk = vec![
        (laptop_id(1), laptop_id(2), laptop_id(3)),
        (laptop_id(6), desk_id(1), laptop_id(5)),
    ];
    assert_eq!(places(&desk, room), at_desk);
    // Nothing the desk folds back while the laptop stays away undoes it.
    desk.fold_workspace_layout(room);
    desk.capture_workspace_layout(room);
    assert_eq!(places(&desk, room), at_desk);

    // Meanwhile the laptop, away too, opens a window of its own.
    workspace_mut(&mut laptop, room).strips[0].window_groups[0]
        .tabs
        .push(notes(laptop_id(9)));

    // Back: each takes in what the other did.
    hear(&mut desk, &mut laptop, room, LAPTOP);
    hear(&mut laptop, &mut desk, room, DESK);
    let merged = vec![
        (laptop_id(1), laptop_id(2), laptop_id(3)),
        (laptop_id(1), laptop_id(2), laptop_id(9)),
        (laptop_id(6), desk_id(1), laptop_id(5)),
    ];
    assert_eq!(places(&desk, room), merged);
    assert_eq!(places(&laptop, room), merged);
    // The laptop shows its own editor live where the desk put it.
    assert!(matches!(
        tile(&laptop, room, laptop_id(5)),
        TileContent::Foreign { surface_id: 77, .. }
    ));
}

#[test]
fn concurrent_moves_on_both_devices_merge_the_same_way_on_each() {
    for laptop_first in [true, false] {
        let (mut laptop, mut desk, room) = two_devices();
        // Both away from each other: each moves the same notes window
        // somewhere else.
        move_tab(&mut laptop, room, laptop_id(3), laptop_id(1), laptop_id(4));
        move_tab(&mut desk, room, laptop_id(3), laptop_id(6), laptop_id(7));
        laptop.capture_workspace_layout(room);
        desk.capture_workspace_layout(room);
        if laptop_first {
            hear(&mut desk, &mut laptop, room, LAPTOP);
            hear(&mut laptop, &mut desk, room, DESK);
        } else {
            hear(&mut laptop, &mut desk, room, DESK);
            hear(&mut desk, &mut laptop, room, LAPTOP);
        }
        let merged = places(&laptop, room);
        assert_eq!(places(&desk, room), merged);
        assert_eq!(
            merged
                .iter()
                .filter(|(_, _, tab)| *tab == laptop_id(3))
                .count(),
            1,
            "the window is in exactly one place: {merged:?}"
        );
        assert!(merged.contains(&(laptop_id(1), laptop_id(4), laptop_id(5))));
        assert!(merged.contains(&(laptop_id(6), laptop_id(7), laptop_id(8))));
    }
}

#[test]
fn a_window_whose_app_runs_on_one_device_is_shown_there_wherever_it_is_placed() {
    let (mut laptop, mut desk, room) = two_devices();
    // On the desk the laptop's editor is that window, running on the laptop.
    assert_eq!(
        tile(&desk, room, laptop_id(5)),
        TileContent::HostedWindow {
            app_id: "org.gnome.TextEditor".into(),
            label: format!("tab {:x}", laptop_id(5)),
            host: key(LAPTOP),
            host_label: format!("device {LAPTOP:x}"),
        }
    );
    // The desk puts it beside the notes on the other strip; the laptop shows
    // its live surface there.
    move_tab(&mut desk, room, laptop_id(5), laptop_id(6), laptop_id(7));
    hear(&mut laptop, &mut desk, room, DESK);
    assert_eq!(
        places(&laptop, room),
        vec![
            (laptop_id(1), laptop_id(2), laptop_id(3)),
            (laptop_id(6), laptop_id(7), laptop_id(8)),
            (laptop_id(6), laptop_id(7), laptop_id(5)),
        ]
    );
    assert_eq!(tile(&laptop, room, laptop_id(5)), editor(laptop_id(5)).tile);
    // And the desk still shows it as running on the laptop.
    assert!(matches!(
        tile(&desk, room, laptop_id(5)),
        TileContent::HostedWindow { .. }
    ));
}

/// A browser page in the room, keeping the room's shared desktop as a page
/// does: it writes the whole desktop it shows into its copy of the record,
/// stamped with its person, and reads the record back after what arrives.
struct Page {
    doc: AutoCommit,
    person: String,
}

impl Page {
    /// A page of `person` that joined with what `device` holds of `room`'s
    /// record.
    fn join(device: &mut WorkspaceTree, room: RoomId, person: &str) -> Self {
        device.capture_workspace_layout(room);
        Self {
            doc: AutoCommit::load(&device.layout_docs.get_mut(&room).expect("a record").save())
                .expect("a record"),
            person: person.to_owned(),
        }
    }

    fn desktop(&self) -> tilo_desktop_model::ownership::SharedDesktop {
        tilo_desktop_model::shared_record::read(&self.doc, None).expect("a desktop")
    }

    /// Where each window is on the page, `(strip, group, tab)`, in order.
    fn places(&self) -> Vec<(u64, u64, u64)> {
        self.desktop()
            .tree
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| {
                strip.window_groups.iter().flat_map(move |group| {
                    group
                        .tabs
                        .iter()
                        .map(move |tab| (strip.id.raw(), group.id, tab.id))
                })
            })
            .collect()
    }

    /// The person changes the page's desktop: what the page publishes.
    fn change(&mut self, edit: impl FnOnce(&mut Workspace)) -> DocumentMutation {
        let heads = self.doc.get_heads();
        let mut desktop = self.desktop();
        edit(&mut desktop.tree.workspaces[0]);
        let value = serde_json::to_value(&desktop).expect("a desktop value");
        assert!(tilo_desktop::history_log::record(
            &mut self.doc,
            &value,
            &self.person,
            0
        ));
        DocumentMutation::Incremental(self.doc.save_after(&heads))
    }

    /// What `device` wrote reaches the page.
    fn hear(&mut self, device: &mut WorkspaceTree, room: RoomId) {
        device.capture_workspace_layout(room);
        let sent = device.layout_docs.get_mut(&room).expect("a record").save();
        tilo_automerge::ApplyIncoming::load_incoming(&mut self.doc, &sent).expect("changes");
    }
}

/// The device of the person's phone, whose browser shows the workspace.
/// The room's host decided its writes before they reach `_tree`.
fn phone(_tree: &mut WorkspaceTree) -> [u8; 32] {
    *iroh::SecretKey::from_bytes(&[0x77; 32]).public().as_bytes()
}

/// The page's own copy of the workspace, for its edits.
fn page_strip(workspace: &mut Workspace, strip: u64) -> &mut Strip {
    workspace
        .strips
        .iter_mut()
        .find(|candidate| candidate.id.raw() == strip)
        .expect("the strip")
}

/// A native device and a browser page of one workspace arrange it at once,
/// each away from the other: the page moves a window and closes one, the
/// device moves another and opens a new one. Back in touch, both hold the
/// same layout, with every change in it.
#[test]
fn a_device_and_a_browser_page_arrange_one_workspace_offline_and_converge() {
    let (mut laptop, _desk, room) = two_devices();
    let person = hex::encode(PERSON);
    let phone = phone(&mut laptop);
    let mut page = Page::join(&mut laptop, room, &person);
    assert_eq!(page.places(), places(&laptop, room));

    // Away from each other. On the page: notes 3 into the later strip's
    // group, and notes 8 closed.
    let moved = page.change(|workspace| {
        let notes = page_strip(workspace, laptop_id(1)).window_groups.remove(0);
        page_strip(workspace, laptop_id(6))
            .window_groups
            .iter_mut()
            .find(|group| group.id == laptop_id(7))
            .expect("the later group")
            .tabs
            .extend(notes.tabs);
    });
    let closed = page.change(|workspace| {
        for group in &mut page_strip(workspace, laptop_id(6)).window_groups {
            group.tabs.retain(|tab| tab.id != laptop_id(8));
        }
    });
    // On the laptop: its editor beside nothing, in a group of its own on the
    // later strip, and a new notes window on the main one.
    move_tab(&mut laptop, room, laptop_id(5), laptop_id(6), laptop_id(10));
    workspace_mut(&mut laptop, room).strips[0]
        .window_groups
        .push(group(laptop_id(11), vec![notes(laptop_id(12))]));
    laptop.capture_workspace_layout(room);

    // Back in touch, each takes in what the other did.
    for change in [&moved, &closed] {
        receive(&mut laptop, room, phone, change).expect("the person's own changes");
    }
    page.hear(&mut laptop, room);
    let merged = places(&laptop, room);
    assert_eq!(page.places(), merged, "both hold one layout");
    for (place, what) in [
        (
            (laptop_id(6), laptop_id(7), laptop_id(3)),
            "the page's move",
        ),
        (
            (laptop_id(6), laptop_id(10), laptop_id(5)),
            "the laptop's move",
        ),
        (
            (laptop_id(1), laptop_id(11), laptop_id(12)),
            "the laptop's new window",
        ),
    ] {
        assert!(merged.contains(&place), "{what} is lost: {merged:?}");
    }
    assert!(
        merged.iter().all(|(_, _, tab)| *tab != laptop_id(8)),
        "the page's close is lost: {merged:?}"
    );
    // The laptop still shows its own editor live.
    assert!(matches!(
        tile(&laptop, room, laptop_id(5)),
        TileContent::Foreign { surface_id: 77, .. }
    ));
}

/// The page and the laptop move the same window at once: it lands in one
/// place, the same on both, whichever hears the other first.
#[test]
fn a_device_and_a_browser_page_moving_one_window_at_once_agree_where_it_is() {
    for laptop_first in [true, false] {
        let (mut laptop, _desk, room) = two_devices();
        let phone = phone(&mut laptop);
        let mut page = Page::join(&mut laptop, room, &hex::encode(PERSON));
        let moved = page.change(|workspace| {
            let notes = page_strip(workspace, laptop_id(1)).window_groups.remove(0);
            page_strip(workspace, laptop_id(6))
                .window_groups
                .push(notes);
        });
        move_tab(&mut laptop, room, laptop_id(3), laptop_id(1), laptop_id(4));
        laptop.capture_workspace_layout(room);
        if laptop_first {
            page.hear(&mut laptop, room);
            receive(&mut laptop, room, phone, &moved).expect("admitted");
        } else {
            receive(&mut laptop, room, phone, &moved).expect("admitted");
            page.hear(&mut laptop, room);
        }
        let merged = places(&laptop, room);
        assert_eq!(page.places(), merged);
        assert_eq!(
            merged
                .iter()
                .filter(|(_, _, tab)| *tab == laptop_id(3))
                .count(),
            1,
            "the window is in exactly one place: {merged:?}"
        );
    }
}
