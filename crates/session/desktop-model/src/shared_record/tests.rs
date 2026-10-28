use tilo_automerge::ApplyIncoming as _;

use super::*;
use crate::layout::TileContent;

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

/// One workspace, strip 10, a group per `(tab, group, who)` unless two share.
fn desktop(windows: &[(u64, u64, &str)]) -> SharedDesktop {
    let mut workspace = Workspace::new(WorkspaceId(1), "Lounge");
    let mut strip = Strip::new(StripId(10), "Lounge");
    for (tab_id, group, _) in windows {
        match strip.window_groups.iter_mut().find(|g| g.id == *group) {
            Some(existing) => existing.tabs.push(tab(*tab_id)),
            None => strip.window_groups.push(WindowGroup {
                id: *group,
                tabs: vec![tab(*tab_id)],
                active_tab: *tab_id,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            }),
        }
    }
    workspace.strips.push(strip);
    SharedDesktop {
        tree: DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        },
        authors: windows
            .iter()
            .map(|(id, _, who)| (*id, (*who).to_owned()))
            .collect(),
        records: windows
            .iter()
            .map(|(id, _, _)| (*id, format!("record-{id}")))
            .collect(),
    }
}

/// The windows of strip 10 as `(tab, group)`, in order.
fn windows(doc: &AutoCommit) -> Vec<(u64, u64)> {
    let desktop = read(doc, None).unwrap();
    desktop.tree.workspaces[0].strips[0]
        .window_groups
        .iter()
        .flat_map(|g| g.tabs.iter().map(move |t| (t.id, g.id)))
        .collect()
}

fn fork(doc: &AutoCommit, actor: u8) -> AutoCommit {
    doc.clone()
        .with_actor(automerge::ActorId::from(vec![actor; 16]))
}

fn merge(into: &mut AutoCommit, from: &mut AutoCommit) {
    into.merge_incoming(from).unwrap();
}

#[test]
fn a_desktop_reads_back_as_written_and_an_unchanged_one_writes_nothing() {
    let mut doc = AutoCommit::new();
    let written = desktop(&[(1, 100, "ada"), (2, 200, "ben"), (3, 200, "ben")]);
    assert!(write(&mut doc, &written).unwrap());
    doc.commit();
    let back = read(&doc, None).unwrap();
    assert_eq!(windows(&doc), vec![(1, 100), (2, 200), (3, 200)]);
    assert_eq!(back.authors, written.authors);
    assert_eq!(back.records, written.records);
    assert_eq!(back.tree.workspaces[0].strips[0].name, "Lounge");
    let heads = doc.get_heads();
    assert!(!write(&mut doc, &back).unwrap(), "nothing differs");
    doc.commit();
    assert_eq!(doc.get_heads(), heads);
}

#[test]
fn concurrent_opens_and_moves_of_different_windows_all_land() {
    let mut room = AutoCommit::new();
    write(&mut room, &desktop(&[(1, 100, "ada"), (2, 200, "ben")])).unwrap();
    room.commit();
    let mut ada = fork(&room, 1);
    let mut ben = fork(&room, 2);
    // Ada moves her window after Ben's; Ben opens a third one.
    write(&mut ada, &desktop(&[(2, 200, "ben"), (1, 100, "ada")])).unwrap();
    ada.commit();
    write(
        &mut ben,
        &desktop(&[(1, 100, "ada"), (2, 200, "ben"), (3, 300, "ben")]),
    )
    .unwrap();
    ben.commit();
    merge(&mut room, &mut ada);
    merge(&mut room, &mut ben);
    assert_eq!(windows(&room), vec![(2, 200), (1, 100), (3, 300)]);
}

#[test]
fn a_page_coming_back_with_an_old_desktop_wipes_nothing() {
    let mut room = AutoCommit::new();
    write(&mut room, &desktop(&[(1, 100, "ada")])).unwrap();
    room.commit();
    let mut away = fork(&room, 1);
    // While Ada's page is away, Ben opens two windows.
    let mut ben = fork(&room, 2);
    write(
        &mut ben,
        &desktop(&[(1, 100, "ada"), (2, 200, "ben"), (3, 300, "ben")]),
    )
    .unwrap();
    ben.commit();
    merge(&mut room, &mut ben);
    // Ada's page comes back and writes its old desktop with her own change.
    let mut renamed = desktop(&[(1, 100, "ada")]);
    renamed.tree.workspaces[0].strips[0].window_groups[0].tabs[0].title = "renamed".into();
    write(&mut away, &renamed).unwrap();
    away.commit();
    merge(&mut room, &mut away);
    assert_eq!(windows(&room), vec![(1, 100), (2, 200), (3, 300)]);
}

#[test]
fn a_window_moved_into_a_group_closed_meanwhile_stays() {
    let mut room = AutoCommit::new();
    write(&mut room, &desktop(&[(1, 100, "ada"), (2, 200, "ben")])).unwrap();
    room.commit();
    let mut ada = fork(&room, 1);
    let mut ben = fork(&room, 2);
    // Ada closes her window, and with it group 100; Ben moves his into it.
    write(&mut ada, &desktop(&[(2, 200, "ben")])).unwrap();
    ada.commit();
    write(&mut ben, &desktop(&[(1, 100, "ada"), (2, 100, "ben")])).unwrap();
    ben.commit();
    merge(&mut room, &mut ada);
    merge(&mut room, &mut ben);
    assert_eq!(windows(&room), vec![(2, 100)]);
}

#[test]
fn moving_one_window_rewrites_only_its_order() {
    let mut doc = AutoCommit::new();
    write(
        &mut doc,
        &desktop(&[(1, 100, "a"), (2, 200, "a"), (3, 300, "a"), (4, 400, "a")]),
    )
    .unwrap();
    doc.commit();
    let before = held(&doc);
    write(
        &mut doc,
        &desktop(&[(1, 100, "a"), (3, 300, "a"), (2, 200, "a"), (4, 400, "a")]),
    )
    .unwrap();
    doc.commit();
    let after = held(&doc);
    let moved: Vec<u64> = after
        .groups
        .iter()
        .filter(|(id, entry)| {
            before.groups[*id].entry.order.to_bits() != entry.entry.order.to_bits()
        })
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(moved.len(), 1, "{moved:?}");
    assert_eq!(windows(&doc), vec![(1, 100), (3, 300), (2, 200), (4, 400)]);
}

#[test]
fn canonical_keeps_only_what_the_record_keeps() {
    let mut local = desktop(&[(1, 100, "ada")]);
    local.tree.workspaces[0].strips[0].window_groups[0].width = WindowWidth::Fraction(500);
    local.tree.workspaces[0].strips[0].window_groups[0].maximized = true;
    let canonical = canonical(&local);
    let group = &canonical.tree.workspaces[0].strips[0].window_groups[0];
    assert_eq!(group.width, WindowWidth::Normal);
    assert!(!group.maximized);
}

/// A page's fresh desktop for a room it has not received yet: one workspace
/// named after the room and one empty "Main" strip, under the page's own ids.
fn fresh(workspace: u64, strip: u64, name: &str) -> SharedDesktop {
    let mut fresh = Workspace::new(WorkspaceId(workspace), name);
    fresh.strips.push(Strip::new(StripId(strip), "Main"));
    SharedDesktop {
        tree: DesktopTree {
            active: fresh.id,
            workspaces: vec![fresh],
            ..DesktopTree::default()
        },
        ..SharedDesktop::default()
    }
}

fn group_of(id: u64, tab_id: u64) -> WindowGroup {
    WindowGroup {
        id,
        tabs: vec![tab(tab_id)],
        active_tab: tab_id,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }
}

#[test]
fn desktops_written_before_the_room_was_received_fold_into_its_workspace() {
    // The room's desktop: a window, and a second strip.
    let mut room = AutoCommit::new();
    let mut first = fresh(1, 10, "Lounge");
    first.tree.workspaces[0].strips[0]
        .window_groups
        .push(group_of(100, 1));
    first.tree.workspaces[0]
        .strips
        .push(Strip::new(StripId(11), "Design"));
    write(&mut room, &first).unwrap();
    room.commit();
    // Pages that published a fresh desktop of their own before the room's
    // reached them, each from a document of its own: every one adds its
    // own `workspaces` map, concurrent with the room's. One of them also
    // opened a window and made a workspace of another name.
    for page in 2..12_u8 {
        let mut blind = AutoCommit::new().with_actor(automerge::ActorId::from(vec![page; 16]));
        let id = u64::from(page);
        let mut written = fresh(id, id * 10, "Lounge");
        if page == 5 {
            written.tree.workspaces[0].strips[0]
                .window_groups
                .push(group_of(500, 5));
            let mut side = Workspace::new(WorkspaceId(55), "Side");
            side.strips.push(Strip::new(StripId(550), "Main"));
            written.tree.workspaces.push(side);
        }
        write(&mut blind, &written).unwrap();
        blind.commit();
        merge(&mut room, &mut blind);
    }
    let desktop = read(&room, None).unwrap();
    let names: Vec<&str> = desktop
        .tree
        .workspaces
        .iter()
        .map(|w| w.name.as_str())
        .collect();
    assert_eq!(names, vec!["Lounge", "Side"]);
    let lounge = &desktop.tree.workspaces[0];
    let strips: Vec<&str> = lounge.strips.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        strips.iter().filter(|name| **name == "Main").count(),
        1,
        "{strips:?}"
    );
    assert!(strips.contains(&"Design"), "{strips:?}");
    let mut tabs: Vec<u64> = lounge
        .strips
        .iter()
        .flat_map(|s| &s.window_groups)
        .flat_map(|g| g.tabs.iter().map(|t| t.id))
        .collect();
    tabs.compact_sort();
    assert_eq!(tabs, vec![1, 5], "no window anyone opened is lost");

    // Writing back what was read leaves the record holding only that.
    write(&mut room, &desktop).unwrap();
    room.commit();
    let after = held(&room);
    assert_eq!(after.workspaces.len(), 2);
    assert_eq!(after.strips.len(), 3, "Main and Design, and Side's Main");
    let again = read(&room, None).unwrap();
    assert!(!write(&mut room, &again).unwrap());
}
