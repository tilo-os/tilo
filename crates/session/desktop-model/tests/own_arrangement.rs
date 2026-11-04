//! The arrangement a person's desk held moves into their own record without
//! losing anything (shell.md §2, workspace-model.md §9).
//!
//! The desk the native session kept for `Research` (the persisted shape
//! `portable_state.rs` captures, with windows and a canvas place on it) is
//! migrated: every workspace's row, every window group, every place and
//! every id is the record's, and reading the record back gives the desk it
//! came from.

#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]

use std::collections::BTreeMap;

use automerge::AutoCommit;
use serde_json::json;
use tilo_desktop_model::own_arrangement::{self, CONVERSATION_SHELL, Place, Strips};
use tilo_desktop_model::{DesktopTree, GroupId, StripId, TabId, WindowWidth, WorkspaceSlot};

/// A desk as a session saved it to `desk.json`, and the records its windows
/// show.
fn saved_desk() -> (DesktopTree, BTreeMap<TabId, String>) {
    let tree: DesktopTree = serde_json::from_value(json!({
        "schema_version": 1,
        "workspaces": [{
            "id": 10,
            "room": "6f1c2d9e-4b3a-4e8f-9a7b-1c2d3e4f5a6b",
            "name": "Research",
            "strips": [
                {
                    "id": 11, "name": "Main",
                    "window_groups": [{
                        "id": 12,
                        "active_tab": 9_007_199_254_741_001_u64,
                        "width": {"Fraction": 500},
                        "tabs": [
                            {"id": 13, "title": "Notes",
                             "tile": {"AppPlaceholder": {"kind": {"Other": "Notes"}, "label": "Notes"}}},
                            {"id": 9_007_199_254_741_001_u64, "title": "Draft",
                             "tile": {"AppPlaceholder": {"kind": {"Other": "Word"}, "label": "Writer"}}}
                        ]
                    }, {
                        "id": 14, "active_tab": 15, "width": "Wide",
                        "tabs": [
                            {"id": 15, "title": "Mail", "view": "reader", "pinned": true,
                             "tile": {"LocalApp": {"app_id": "so.tilo.lang.mail", "label": "Mail",
                                                    "note": ""}}}
                        ]
                    }]
                },
                {
                    "id": 16, "name": "Reading",
                    "window_groups": [{
                        "id": 17, "active_tab": 18, "width": "Narrow",
                        "tabs": [
                            {"id": 18, "title": "Files",
                             "tile": {"Stub": {"kind": "FileExplorer", "label": "Files"}}}
                        ]
                    }]
                }
            ],
            "arrangement": {"kind": "Canvas", "places": {"12": {"x": 0, "y": 0, "width": 800, "height": 600}}}
        }, {"id": 20, "room": "0b9a8c7d-6e5f-4a3b-8c2d-1e0f9a8b7c6d", "name": "Other", "strips": []}],
        "active": 10, "active_strip": {"10": 11}
    }))
    .unwrap();
    let records = BTreeMap::from([(15, "member:ab:tilolang.room".to_owned())]);
    (tree, records)
}

#[test]
fn a_kept_desk_migrates_with_every_window_strip_and_canvas_place() {
    let (tree, records) = saved_desk();
    let arrangement = Strips::from_tree(&tree, &records);

    // Every row of every workspace, in order, with its id and name.
    let rows: Vec<(StripId, String)> = arrangement
        .rows
        .iter()
        .map(|row| (row.id, row.name.clone()))
        .collect();
    assert_eq!(
        rows,
        vec![
            (StripId(11), "Main".to_owned()),
            (StripId(16), "Reading".to_owned())
        ]
    );
    // Every window group, with its width and the place in front.
    let strips: Vec<(GroupId, StripId, WindowWidth, Option<TabId>)> = arrangement
        .strips
        .iter()
        .map(|strip| (strip.id, strip.row, strip.width, strip.active))
        .collect();
    assert_eq!(
        strips,
        vec![
            (
                12,
                StripId(11),
                WindowWidth::Fraction(500),
                Some(9_007_199_254_741_001u64)
            ),
            (14, StripId(11), WindowWidth::Wide, Some(15)),
            (17, StripId(16), WindowWidth::Narrow, Some(18)),
        ]
    );
    // Every place, under the id and title it had; the record one window
    // shows is kept with it.
    let places: Vec<(TabId, String)> = arrangement
        .places()
        .map(|place| (place.id, place.title.clone()))
        .collect();
    assert_eq!(
        places,
        vec![
            (13, "Notes".to_owned()),
            (9_007_199_254_741_001, "Draft".to_owned()),
            (15, "Mail".to_owned()),
            (18, "Files".to_owned()),
        ]
    );
    assert_eq!(
        arrangement.place(15).unwrap().record,
        "member:ab:tilolang.room",
        "the record a place shows goes with it"
    );
    // The view a window shows its record through, and whether the person
    // pinned it, are the window's own and go with it.
    let pinned = arrangement.place(15).expect("the pinned place");
    assert_eq!(pinned.view, "reader", "the view it shows");
    assert!(pinned.pinned, "and the pin on it");
    assert!(
        !arrangement.place(13).expect("the other place").pinned,
        "a window the person did not pin stays unpinned"
    );
    // The canvas place the desk held is the strip's.
    assert_eq!(
        arrangement.strip(12).unwrap().canvas,
        Some(tilo_desktop_model::CanvasPlace {
            x: 0,
            y: 0,
            width: 800,
            height: 600
        })
    );
    assert!(arrangement.foreign.is_empty(), "nothing is another shell's");
}

#[test]
fn the_record_round_trips_to_the_desk_it_came_from() {
    let (tree, records) = saved_desk();
    let arrangement = Strips::from_tree(&tree, &records);

    // Written to the person's own record, and read back.
    let mut doc = AutoCommit::new();
    assert!(own_arrangement::write(&mut doc, &arrangement).unwrap());
    let read = own_arrangement::read(&doc);
    assert_eq!(read, arrangement, "the record holds the same arrangement");

    // The desk it describes names the same workspaces, rows, groups and
    // places, under the same ids.
    let name = |id: WorkspaceSlot| match id.0 {
        10 => Some("Research".to_owned()),
        20 => Some("Other".to_owned()),
        _ => None,
    };
    let desk = read.to_tree(name);
    assert_eq!(
        desk.workspaces
            .iter()
            .map(|workspace| (workspace.slot, workspace.name.clone()))
            .collect::<Vec<_>>(),
        vec![
            (WorkspaceSlot(10), "Research".to_owned()),
            (WorkspaceSlot(20), "Other".to_owned()),
        ]
    );
    let held: Vec<(StripId, GroupId, Vec<TabId>)> = desk.workspaces[0]
        .strips
        .iter()
        .flat_map(|row| {
            row.window_groups.iter().map(move |group| {
                (
                    row.id,
                    group.id,
                    group.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
                )
            })
        })
        .collect();
    assert_eq!(
        held,
        vec![
            (StripId(11), 12, vec![13, 9_007_199_254_741_001]),
            (StripId(11), 14, vec![15]),
            (StripId(16), 17, vec![18]),
        ]
    );
    assert_eq!(
        desk.workspaces[0].arrangement.kind,
        tilo_desktop_model::ArrangementKind::Canvas
    );
    assert_eq!(
        desk.workspaces[0].arrangement.place_of(12),
        arrangement.strip(12).unwrap().canvas
    );
    // A place keeps showing what it showed: the app's own window, not a
    // placeholder.
    let mail = desk.workspaces[0]
        .strips
        .iter()
        .flat_map(|row| &row.window_groups)
        .flat_map(|group| &group.tabs)
        .find(|tab| tab.id == 15)
        .unwrap();
    assert!(matches!(
        mail.tile,
        tilo_desktop_model::TileContent::LocalApp { ref app_id, .. } if app_id == "so.tilo.lang.mail"
    ));
}

#[test]
fn writing_twice_writes_nothing_the_second_time() {
    let (tree, records) = saved_desk();
    let arrangement = Strips::from_tree(&tree, &records);
    let mut doc = AutoCommit::new();
    assert!(own_arrangement::write(&mut doc, &arrangement).unwrap());
    let heads = doc.get_heads();
    assert!(!own_arrangement::write(&mut doc, &arrangement).unwrap());
    assert_eq!(
        doc.get_heads(),
        heads,
        "an unchanged arrangement is no change"
    );
}

#[test]
fn moving_and_resizing_an_arrangement_is_one_entry_each() {
    let (tree, records) = saved_desk();
    let mut arrangement = Strips::from_tree(&tree, &records);
    let mut doc = AutoCommit::new();
    own_arrangement::write(&mut doc, &arrangement).unwrap();

    // The person drags Notes into Reading, after Files.
    assert_eq!(
        arrangement.move_place(13, StripId(16), Some(18), 99),
        Some(17),
        "it lands in the strip holding the place it was dropped after"
    );
    // And resizes Main.
    assert!(arrangement.resize(12, WindowWidth::Fraction(700)));
    own_arrangement::write(&mut doc, &arrangement).unwrap();

    let read = own_arrangement::read(&doc);
    assert_eq!(read.strip_of(13).unwrap().id, 17);
    assert_eq!(
        read.strip(12).unwrap().places.len(),
        1,
        "the place left its old strip"
    );
    assert_eq!(read.strip(12).unwrap().width, WindowWidth::Fraction(700));
    // The strip Notes left keeps the place in front it now has.
    assert_eq!(
        read.strip(12).unwrap().active,
        Some(9_007_199_254_741_001u64)
    );
}

#[test]
fn a_shell_that_does_not_know_a_place_leaves_it_alone() {
    let (tree, records) = saved_desk();
    let mine = Strips::from_tree(&tree, &records);
    let mut doc = AutoCommit::new();
    own_arrangement::write(&mut doc, &mine).unwrap();

    // Another shell keeps its own arrangement in the same record: one place
    // of its own, in a strip of its own.
    let theirs = Strips {
        workspaces: mine.workspaces.clone(),
        rows: mine.rows.clone(),
        strips: vec![tilo_desktop_model::own_arrangement::Strip {
            id: 900,
            workspace: WorkspaceSlot(10),
            row: StripId(11),
            width: WindowWidth::Normal,
            maximized: false,
            active: Some(901),
            canvas: None,
            shell: CONVERSATION_SHELL.to_owned(),
            places: vec![Place {
                id: 901,
                shows: "so.tilo.lang.chat".to_owned(),
                view: "conversation".to_owned(),
                record: String::new(),
                state: json!(null),
                title: "Chat".to_owned(),
                pinned: false,
                shell: CONVERSATION_SHELL.to_owned(),
            }],
        }],
        foreign: Vec::new(),
    };
    assert!(
        own_arrangement::write_as(&mut doc, CONVERSATION_SHELL, &theirs).unwrap(),
        "their shell writes its own arrangement into the same record"
    );

    // The strips shell reads the record: their strip and place are kept,
    // and not one of its own.
    let read = own_arrangement::read(&doc);
    assert!(read.strip(900).is_none(), "their strip is not shown");
    assert!(read.place(901).is_none(), "their place is not shown");
    assert_eq!(
        read.foreign.len(),
        1,
        "their place is kept: {:?}",
        read.foreign
    );
    assert_eq!(read.foreign[0].id, 901);
    assert_eq!(
        read.strip(12).unwrap().places.len(),
        2,
        "this shell's own strips are as they were"
    );

    // This shell moves one of its own places and writes: their place is
    // still theirs, exactly as it was.
    let mut changed = read;
    assert!(changed.move_place(13, StripId(16), None, 99).is_some());
    assert!(own_arrangement::write(&mut doc, &changed).unwrap());
    let after = own_arrangement::read(&doc);
    assert!(after.foreign.iter().any(|place| place.id == 901));
    assert_eq!(after.strip_of(13).unwrap().row, StripId(16));
    assert_eq!(
        own_arrangement::read_as(&doc, CONVERSATION_SHELL)
            .place(901)
            .unwrap()
            .title,
        "Chat",
        "their place is theirs still"
    );

    // Closing this shell's place removes only its own.
    let mut closed = after;
    assert!(closed.close_place(13));
    assert!(own_arrangement::write(&mut doc, &closed).unwrap());
    let after = own_arrangement::read(&doc);
    assert!(after.place(13).is_none(), "its own closed place goes");
    assert!(
        after.foreign.iter().any(|place| place.id == 901),
        "and theirs stays"
    );
}
