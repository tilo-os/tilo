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
fn two_edits_of_one_workspace_made_at_once_both_land() {
    let base = desktop(&[(1, 100, "ada")]);
    let mut room = AutoCommit::new();
    write(&mut room, &base).unwrap();
    room.commit();
    let mut ada = fork(&room, 1);
    let mut ben = fork(&room, 2);
    // Ada renames the workspace while Ben, who cannot see that, gives it a
    // shortcut: both are fields of the same workspace.
    let mut renamed = base.clone();
    renamed.tree.workspaces[0].name = "Ada's lounge".into();
    write(&mut ada, &renamed).unwrap();
    ada.commit();
    let mut furnished = base;
    furnished.tree.workspaces[0]
        .shortcuts
        .push(tilo_automation::SavedShortcut::parse(r#"{"id":"morning"}"#).expect("a shortcut"));
    write(&mut ben, &furnished).unwrap();
    ben.commit();
    merge(&mut room, &mut ada);
    merge(&mut room, &mut ben);
    let back = read(&room, None).unwrap();
    let workspace = &back.tree.workspaces[0];
    assert_eq!(workspace.name, "Ada's lounge");
    let shortcuts: Vec<&str> = workspace
        .shortcuts
        .iter()
        .map(tilo_automation::SavedShortcut::id)
        .collect();
    assert_eq!(shortcuts, vec!["morning"]);
}

/// `doc` as saved before a workspace and a window were maps: each of those
/// fields held its value as JSON text.
fn as_saved_with_text_fields(doc: &mut AutoCommit) {
    for (level, field) in [(WORKSPACES, WORKSPACE), (WINDOWS, TAB)] {
        let (_, container) = doc.get(ROOT, level).unwrap().expect("level");
        for id in doc.keys(&container).collect::<Vec<_>>() {
            let (_, entry) = doc.get(&container, &id).unwrap().expect("entry");
            let (value, at) = doc.get(&entry, field).unwrap().expect("field");
            let value = tilo_automerge::json::read_value(&*doc, &value, &at).unwrap();
            doc.put(&entry, field, value.to_string()).unwrap();
        }
    }
    doc.commit();
}

#[test]
fn a_desktop_saved_with_text_fields_reads_whole() {
    let saved = desktop(&[(1, 100, "ada"), (2, 200, "ben")]);
    let mut room = AutoCommit::new();
    write(&mut room, &saved).unwrap();
    room.commit();
    let as_maps = read(&room, None).unwrap();
    as_saved_with_text_fields(&mut room);
    let back = read(&room, None).unwrap();
    assert_eq!(
        serde_json::to_value(&back.tree).unwrap(),
        serde_json::to_value(&as_maps.tree).unwrap()
    );
    assert_eq!(back.authors, as_maps.authors);
    assert_eq!(as_maps.tree.workspaces[0].strips[0].window_groups.len(), 2);
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

fn on_canvas(desktop: &mut SharedDesktop, places: &[(u64, CanvasPlace)]) {
    let arrangement = &mut desktop.tree.workspaces[0].arrangement;
    arrangement.kind = ArrangementKind::Canvas;
    arrangement.places.extend(places.iter().copied());
}

#[test]
fn a_canvas_and_its_places_read_back_as_written() {
    let mut doc = AutoCommit::new();
    let mut written = desktop(&[(1, 100, "ada"), (2, 200, "ben")]);
    on_canvas(&mut written, &[(100, CanvasPlace::new(-40, 12, 640, 480))]);
    assert!(write(&mut doc, &written).unwrap());
    doc.commit();
    let back = read(&doc, None).unwrap();
    let arrangement = &back.tree.workspaces[0].arrangement;
    assert_eq!(arrangement.kind, ArrangementKind::Canvas);
    assert_eq!(
        arrangement.place_of(100),
        Some(CanvasPlace::new(-40, 12, 640, 480))
    );
    assert_eq!(arrangement.place_of(200), None, "never placed");
    assert!(
        !write(&mut doc, &written).unwrap(),
        "unchanged writes nothing"
    );
}

#[test]
fn two_people_moving_different_windows_on_the_canvas_both_keep_their_move() {
    let mut doc = AutoCommit::new();
    let mut base = desktop(&[(1, 100, "ada"), (2, 200, "ben")]);
    on_canvas(
        &mut base,
        &[
            (100, CanvasPlace::new(0, 0, 400, 300)),
            (200, CanvasPlace::new(500, 0, 400, 300)),
        ],
    );
    write(&mut doc, &base).unwrap();
    doc.commit();
    let mut ada = fork(&doc, 1);
    let mut ben = fork(&doc, 2);
    let mut ada_moved = base.clone();
    on_canvas(&mut ada_moved, &[(100, CanvasPlace::new(0, 900, 400, 300))]);
    write(&mut ada, &ada_moved).unwrap();
    ada.commit();
    let mut ben_moved = base.clone();
    on_canvas(
        &mut ben_moved,
        &[(200, CanvasPlace::new(500, 900, 400, 300))],
    );
    write(&mut ben, &ben_moved).unwrap();
    ben.commit();
    merge(&mut ada, &mut ben);
    let back = read(&ada, None).unwrap();
    let arrangement = &back.tree.workspaces[0].arrangement;
    assert_eq!(arrangement.place_of(100).map(|p| p.y), Some(900));
    assert_eq!(arrangement.place_of(200).map(|p| p.y), Some(900));
}

#[test]
fn moving_someone_elses_window_on_the_canvas_is_moving_it() {
    use crate::ownership::{Act, foreign_acts};
    let mut before = desktop(&[(1, 100, "ada"), (2, 200, "ben")]);
    on_canvas(&mut before, &[(200, CanvasPlace::new(0, 0, 400, 300))]);
    let mut after = before.clone();
    on_canvas(&mut after, &[(200, CanvasPlace::new(10, 0, 400, 300))]);
    let acts = foreign_acts(&before, &after, "ada", None);
    assert!(
        acts.iter().any(|t| t.id == 2 && t.act == Act::Move),
        "{acts:?}"
    );
    // Placing a window that stood nowhere yet moves nothing.
    let unplaced = desktop(&[(1, 100, "ada"), (2, 200, "ben")]);
    let mut first = unplaced.clone();
    on_canvas(&mut first, &[(200, CanvasPlace::new(0, 0, 400, 300))]);
    assert!(foreign_acts(&unplaced, &first, "ada", None).is_empty());
}
