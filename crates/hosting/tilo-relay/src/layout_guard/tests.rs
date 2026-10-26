use automerge::transaction::Transactable as _;
use tilo_desktop_model::commands::{BoundCommand, CommandSource, DesktopCommand, TabLocation};
use tilo_desktop_model::compose_id;
use tilo_desktop_model::ids::namespace_of_device;
use tilo_desktop_model::layout::{Strip, Tab, TileContent, WindowGroup};
use tilo_desktop_model::ownership::{SharedDesktop, Touched};
use tilo_desktop_model::projection::LayoutRequest;
use tilo_desktop_model::{
    DesktopTree, RoomId, StripId, TabId, WindowWidth, Workspace, WorkspaceId, room_layout,
};

use super::*;

const ADA: &str = "ada-device";
const BEN: &str = "ben-device";
const MOD: &str = "moderator-device";
const ROOM: &str = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";

fn room_id() -> RoomId {
    RoomId::parse(ROOM).unwrap()
}

fn tab(id: u64) -> Tab {
    Tab {
        id,
        title: format!("tab {id}"),
        tile: TileContent::LocalApp {
            app_id: "so.tilo.notes".into(),
            label: "Notes".into(),
            note: String::new(),
        },
        app_title: None,
    }
}

/// One strip of `device`'s: strip `n`, one group, one window.
fn strip_of(device: &str, n: u64) -> Strip {
    let id = |base: u64| compose_id(namespace_of_device(device), base);
    Strip {
        id: StripId(id(n)),
        name: format!("{device} {n}"),
        window_groups: vec![WindowGroup {
            id: id(n + 10),
            tabs: vec![tab(id(n + 20))],
            active_tab: id(n + 20),
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    }
}

fn empty_layout() -> SharedDesktop {
    let workspace = Workspace::in_room(WorkspaceId::of_room(room_id()), room_id(), "Room");
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

/// A room whose layout holds Ada's and Ben's windows, as their devices
/// published them.
fn room() -> LayoutGuard {
    let mut doc = AutoCommit::new();
    let mut layout = empty_layout();
    for (device, n) in [(ADA, 1), (BEN, 1)] {
        layout.tree.workspaces[0].strips.push(strip_of(device, n));
        room_layout::write(&mut doc, &layout, namespace_of_device(device), Some(device)).unwrap();
    }
    doc.commit();
    LayoutGuard::new(doc)
}

/// A device's copy of the room's layout, and a change to publish from it.
struct Page {
    doc: AutoCommit,
}

impl Page {
    fn of(guard: &LayoutGuard) -> Self {
        Self {
            doc: AutoCommit::load(&guard.doc.save()).unwrap(),
        }
    }

    /// Publish `change`'s layout as `device`'s, as one incremental write.
    fn publish(
        &mut self,
        device: &str,
        change: impl FnOnce(&mut SharedDesktop),
    ) -> DocumentMutation {
        let heads = self.doc.get_heads();
        let mut layout = room_layout::read(&self.doc, None).unwrap();
        change(&mut layout);
        room_layout::write(
            &mut self.doc,
            &layout,
            namespace_of_device(device),
            Some(device),
        )
        .unwrap();
        DocumentMutation::Incremental(self.doc.save_after(&heads))
    }
}

/// Windows on the room's layout, in order.
fn windows(guard: &LayoutGuard) -> Vec<u64> {
    room_layout::read(&guard.doc, None)
        .unwrap()
        .tree
        .workspaces
        .into_iter()
        .flat_map(|workspace| workspace.strips)
        .flat_map(|strip| strip.window_groups)
        .flat_map(|group| group.tabs)
        .map(|tab| tab.id)
        .collect()
}

fn tab_of(device: &str, n: u64) -> TabId {
    compose_id(namespace_of_device(device), n + 20)
}

/// Only what `writer` may moderate is allowed.
fn moderates(writer: &'static str) -> impl Fn(&Touched) -> bool {
    move |_| writer == MOD
}

fn close_request(device: &str, tab: TabId) -> LayoutRequest {
    let strip = StripId(compose_id(namespace_of_device(device), 1));
    let group = compose_id(namespace_of_device(device), 11);
    let command = BoundCommand {
        source: CommandSource {
            room: ROOM.into(),
            heads: Vec::new(),
        },
        command: DesktopCommand::CloseTab {
            location: TabLocation {
                strip: strip.raw(),
                group,
                tab,
            },
        },
    };
    let command = serde_json::to_vec(&command).unwrap();
    LayoutRequest {
        id: LayoutRequest::id("ada", namespace_of_device(device), 10, &command),
        from: "ada".into(),
        to_salt: namespace_of_device(device),
        at: 10,
        command,
    }
}

/// A device writes the window it opened: admitted, and only that entry
/// changes.
#[test]
fn a_device_writes_its_own_window() {
    let mut guard = room();
    let mut page = Page::of(&guard);
    let write = page.publish(ADA, |layout| {
        layout.tree.workspaces[0].strips[0].window_groups[0].tabs[0].title = "renamed".into();
    });
    guard
        .decide(&write, ROOM, ADA, "ada", &moderates(ADA))
        .unwrap();
    let read = room_layout::read(&guard.doc, None).unwrap();
    assert!(
        read.authors.contains_key(&tab_of(BEN, 1)),
        "Ben's window is untouched"
    );
    assert_eq!(
        room_layout::read(&guard.doc, None).unwrap().tree.workspaces[0].strips[0].window_groups[0]
            .tabs[0]
            .title,
        "renamed"
    );
}

/// Nobody writes, moves or closes a window another device opened, however
/// the room ranks them: that is what requests are for.
#[test]
fn a_device_may_not_write_anothers_window() {
    let mut guard = room();
    let mut closing = Page::of(&guard);
    let write = closing.publish(ADA, |layout| {
        // Ada's page drops Ben's window from its own copy.
        layout.tree.workspaces[0]
            .strips
            .retain(|strip| strip.id != StripId(compose_id(namespace_of_device(BEN), 1)));
    });
    let refusal = guard
        .decide(&write, ROOM, ADA, "ada", &moderates(ADA))
        .unwrap_err();
    assert!(
        matches!(&refusal, LayoutRefusal::NotTheirs { entry, .. } if *entry == tab_of(BEN, 1)),
        "refused on Ben's window: {refusal}"
    );
    assert!(
        windows(&guard).contains(&tab_of(BEN, 1)),
        "refused, the layout is as it was"
    );

    let mut moving = Page::of(&guard);
    let write = moving.publish(MOD, |layout| {
        layout.tree.workspaces[0].strips[0].window_groups[0].tabs[0].title = "moderated".into();
    });
    assert!(
        guard
            .decide(&write, ROOM, MOD, "mod", &moderates(MOD))
            .is_err(),
        "even a moderator asks instead of writing"
    );
}

/// A request is retained: one for a moderator's standing is taken in, one
/// for a window its asker may not rearrange is refused, and one asked in
/// someone else's name is refused too.
#[test]
fn a_request_is_judged_where_it_enters() {
    let mut guard = room();
    let ask = close_request(BEN, tab_of(BEN, 1));
    let mut page = Page::of(&guard);
    let heads = page.doc.get_heads();
    room_layout::request(&mut page.doc, &ask).unwrap();
    let write = DocumentMutation::Incremental(page.doc.save_after(&heads));
    assert!(
        matches!(
            guard.decide(&write, ROOM, ADA, "ada", &moderates(ADA)),
            Err(LayoutRefusal::Request { .. })
        ),
        "an editor may not close someone else's window"
    );

    // The same request from a moderator, addressed to Ben's device, is kept.
    let mut moderator = Page::of(&guard);
    let heads = moderator.doc.get_heads();
    let mut allowed = close_request(BEN, tab_of(BEN, 1));
    allowed.from = "mod".into();
    allowed.id = LayoutRequest::id("mod", allowed.to_salt, allowed.at, &allowed.command);
    room_layout::request(&mut moderator.doc, &allowed).unwrap();
    let write = DocumentMutation::Incremental(moderator.doc.save_after(&heads));
    guard
        .decide(&write, ROOM, MOD, "mod", &moderates(MOD))
        .unwrap();
    assert_eq!(
        room_layout::pending(&guard.doc, namespace_of_device(BEN)).len(),
        1,
        "kept for Ben's device to apply"
    );

    // Asked in Ada's name but written by Ben's device: refused.
    let mut forged = Page::of(&guard);
    let heads = forged.doc.get_heads();
    room_layout::request(&mut forged.doc, &ask).unwrap();
    let write = DocumentMutation::Incremental(forged.doc.save_after(&heads));
    assert!(matches!(
        guard.decide(&write, ROOM, BEN, "ben", &moderates(BEN)),
        Err(LayoutRefusal::Request { .. })
    ));
}

/// An owner records the requests it applied in its own namespace; marking
/// another device's request is refused.
#[test]
fn only_the_owner_records_what_it_applied() {
    let mut guard = room();
    let ask = close_request(BEN, tab_of(BEN, 1));
    let mut ben = Page::of(&guard);
    let heads = ben.doc.get_heads();
    let mut applied = room_layout::read(&ben.doc, None).unwrap();
    applied.tree.workspaces[0]
        .strips
        .retain(|strip| strip.id != StripId(compose_id(namespace_of_device(BEN), 1)));
    room_layout::write(&mut ben.doc, &applied, namespace_of_device(BEN), Some(BEN)).unwrap();
    room_layout::claim(&mut ben.doc, namespace_of_device(BEN), &ask).unwrap();
    let write = DocumentMutation::Incremental(ben.doc.save_after(&heads));
    guard
        .decide(&write, ROOM, BEN, "ben", &moderates(BEN))
        .unwrap();
    assert!(
        room_layout::pending(&guard.doc, namespace_of_device(BEN)).is_empty(),
        "the request is recorded as applied"
    );

    // Ada's device claims it too.
    let mut ada = Page::of(&guard);
    let heads = ada.doc.get_heads();
    room_layout::claim(&mut ada.doc, namespace_of_device(BEN), &ask).unwrap();
    let write = DocumentMutation::Incremental(ada.doc.save_after(&heads));
    assert!(
        matches!(
            guard.decide(&write, ROOM, ADA, "ada", &moderates(ADA)),
            Err(LayoutRefusal::NotItsMark { .. })
        ),
        "a mark in another device's namespace is refused"
    );
}

/// A page from before the one layout schema writes the desktop shape rooms
/// used to keep: its write is refused rather than read as touching nothing.
#[test]
fn a_write_of_the_old_shape_is_refused() {
    let mut guard = room();
    let mut page = Page::of(&guard);
    let heads = page.doc.get_heads();
    page.doc
        .put(automerge::ROOT, "strips", "the old shape")
        .unwrap();
    page.doc.commit();
    let write = DocumentMutation::Incremental(page.doc.save_after(&heads));
    assert!(
        matches!(
            guard.decide(&write, ROOM, ADA, "ada", &moderates(ADA)),
            Err(LayoutRefusal::NotALayout { .. })
        ),
        "the old desktop shape is no room layout"
    );
}

/// A document that names no room carries no layout this build reads, whoever
/// writes to it.
#[test]
fn a_document_that_names_no_room_is_no_layout() {
    let mut guard = LayoutGuard::new(AutoCommit::new());
    let mut page = AutoCommit::new();
    let heads = page.get_heads();
    let strip = strip_of(ADA, 1);
    tilo_desktop_model::projection::project_owned_contents(
        &mut page,
        std::slice::from_ref(&strip),
        namespace_of_device(ADA),
        None,
        tilo_desktop_model::projection::EmptyStrips::Skip,
    )
    .unwrap();
    page.commit();
    let write = DocumentMutation::Incremental(page.save_after(&heads));
    assert!(matches!(
        guard.decide(&write, ROOM, ADA, "ada", &moderates(ADA)),
        Err(LayoutRefusal::NotALayout { .. })
    ));
}

/// The strips the room itself lists are minted in a namespace of the room's
/// own, never a device's: no device writes there, however its key folds.
#[test]
fn a_device_may_not_write_the_rooms_own_strips() {
    let mut guard = room();
    let room_salt = tilo_desktop_model::ids::room_strips_salt(ROOM);
    assert_ne!(room_salt, namespace_of_device(ADA), "fixture namespaces");
    let id = |base: u64| compose_id(room_salt, base);
    let listed = Strip {
        id: StripId(id(1)),
        name: "Main".into(),
        window_groups: Vec::new(),
    };
    tilo_desktop_model::projection::project_owned_contents(
        &mut guard.doc,
        std::slice::from_ref(&listed),
        room_salt,
        None,
        tilo_desktop_model::projection::EmptyStrips::Retain,
    )
    .unwrap();
    guard.doc.commit();

    // Ada's page rewrites the room's strip in the room's own namespace.
    let mut page = Page::of(&guard);
    let heads = page.doc.get_heads();
    page.doc
        .put(
            automerge::ROOT,
            tilo_desktop_model::projection::ROOM_NAME_KEY,
            "Mine",
        )
        .unwrap();
    let mut layout = room_layout::read(&page.doc, None).unwrap();
    layout.tree.workspaces[0].strips.push(Strip {
        id: StripId(id(2)),
        name: "Not mine".into(),
        window_groups: Vec::new(),
    });
    room_layout::write(&mut page.doc, &layout, room_salt, Some(ADA)).unwrap();
    let write = DocumentMutation::Incremental(page.doc.save_after(&heads));
    assert!(matches!(
        guard.decide(&write, ROOM, ADA, "ada", &moderates(ADA)),
        Err(LayoutRefusal::NotTheirs { entry, .. }) if entry == id(2)
    ));
}

#[test]
fn a_write_that_is_not_automerge_changes_nothing() {
    let mut guard = room();
    let _ = guard.decide(
        &DocumentMutation::Incremental(vec![1, 2, 3]),
        ROOM,
        ADA,
        "ada",
        &moderates(ADA),
    );
    assert_eq!(windows(&guard), vec![tab_of(ADA, 1), tab_of(BEN, 1)]);
}
