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
