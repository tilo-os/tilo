//! The device's desktop document records exactly what the tree holds, and
//! each narrow edit mirrors the matching tree change.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed lookup is the assertion"
)]

use automerge::AutoCommit;
use collab_types::caps::{CapOverride, Scope, Subject, Verb};
use tilo_desktop_model::commands::{DesktopCommand, TabLocation, apply_command};
use tilo_desktop_model::desktop_doc;
use tilo_desktop_model::{
    DesktopTree, DropTarget, IdMinter, RoomId, Strip, StripId, Tab, TileContent, WindowGroup,
    WindowWidth, Workspace, WorkspaceId,
};

fn tab(id: u64) -> Tab {
    Tab {
        id,
        title: format!("tab {id}"),
        tile: TileContent::LocalApp {
            app_id: "org.example.App".into(),
            label: "App".into(),
            note: String::new(),
        },
    }
}

fn group(id: u64, tabs: &[u64]) -> WindowGroup {
    WindowGroup {
        id,
        tabs: tabs.iter().copied().map(tab).collect(),
        active_tab: tabs[0],
        width: WindowWidth::Wide,
        maximized: true,
    }
}

/// Workspace 1: strip 1 holds groups 10 [100, 101] and 11 [110]; strip 2
/// holds group 20 [200].
fn tree() -> DesktopTree {
    let mut ws = Workspace::new(WorkspaceId(1), "Desk");
    ws.room = RoomId::parse("00000000-0000-4000-8000-000000000042").unwrap();
    ws.set_owner("ada");
    let mut a = Strip::new(StripId(1), "A");
    a.window_groups = vec![group(10, &[100, 101]), group(11, &[110])];
    let mut b = Strip::new(StripId(2), "B");
    b.window_groups = vec![group(20, &[200])];
    ws.strips = vec![a, b];
    DesktopTree {
        workspaces: vec![ws],
        active: WorkspaceId(1),
        ..DesktopTree::default()
    }
}

/// `(strip, [(group, [tab])])` for the first workspace.
type Layout = Vec<(u64, Vec<(u64, Vec<u64>)>)>;

fn layout(tree: &DesktopTree) -> Layout {
    tree.workspaces[0]
        .strips
        .iter()
        .map(|s| {
            (
                s.id.raw(),
                s.window_groups
                    .iter()
                    .map(|g| (g.id, g.tabs.iter().map(|t| t.id).collect()))
                    .collect(),
            )
        })
        .collect()
}

/// What a fresh reader sees: the document folded into an empty tree.
fn read_back(doc: &AutoCommit) -> DesktopTree {
    let mut fresh = DesktopTree::default();
    desktop_doc::rebuild(doc, &mut fresh);
    fresh
}

fn seeded() -> (DesktopTree, AutoCommit) {
    let tree = tree();
    let mut doc = AutoCommit::new();
    desktop_doc::seed(&mut doc, &tree);
    (tree, doc)
}

#[test]
fn desktop_document_round_trips_the_desktop_and_governance() {
    // Phase `a_seeded_document_reads_back_as_the_same_desktop`.
    {
        let (tree, doc) = seeded();
        let back = read_back(&doc);
        assert_eq!(back.active, tree.active);
        assert_eq!(layout(&back), layout(&tree));
        let ws = &back.workspaces[0];
        assert_eq!(ws.room, tree.workspaces[0].room);
        assert_eq!(ws.owner.as_deref(), Some("ada"));
        assert_eq!(ws.capabilities, tree.workspaces[0].capabilities);
        // Presentation state is this device's and is not in the document.
        assert!(!ws.strips[0].window_groups[0].maximized);
        assert_eq!(ws.strips[0].window_groups[0].width, WindowWidth::Normal);
    }
    // Phase `rebuilding_keeps_local_presentation_for_known_groups`.
    {
        let (mut tree, doc) = seeded();
        desktop_doc::rebuild(&doc, &mut tree);
        assert!(tree.workspaces[0].strips[0].window_groups[0].maximized);
    }
    // Phase `governance_edits_round_trip`.
    {
        let (_, mut doc) = seeded();
        let cap = CapOverride::grant(
            Subject::app("org.example.App"),
            Scope::FsPath("/srv".into()),
            Verb::Read,
        )
        .with_id("cap-x");
        assert!(desktop_doc::insert_capability(
            &mut doc,
            WorkspaceId(1),
            &cap
        ));
        assert!(desktop_doc::set_owner(&mut doc, WorkspaceId(1), "bob"));
        let ws = &read_back(&doc).workspaces[0];
        assert!(ws.capabilities.iter().any(|c| c.id == "cap-x"));
        assert_eq!(ws.owner.as_deref(), Some("bob"));

        assert!(desktop_doc::revoke_capability(
            &mut doc,
            WorkspaceId(1),
            "cap-x"
        ));
        assert!(
            !read_back(&doc).workspaces[0]
                .capabilities
                .iter()
                .any(|c| c.id == "cap-x")
        );
        assert!(!desktop_doc::set_owner(&mut doc, WorkspaceId(9), "eve"));
    }
}

#[test]
fn topology_edits_are_mirrored_into_the_document() {
    // Phase `a_cross_strip_move_is_mirrored_exactly`.
    {
        let (mut tree, mut doc) = seeded();
        let target = DropTarget::ExistingGroup {
            group_id: 20,
            tab_index: 1,
        };
        let moved = tree
            .move_tab_across_strips(StripId(1), StripId(2), 110, 99, target)
            .unwrap();
        assert!(desktop_doc::apply_tab_move(
            &mut doc,
            StripId(1),
            StripId(2),
            &moved
        ));
        assert_eq!(layout(&read_back(&doc)), layout(&tree));
    }
    // Phase `a_split_is_mirrored_and_a_stale_move_leaves_the_document_alone`.
    {
        let (mut tree, mut doc) = seeded();
        let mut ids = IdMinter::resume(0, 500);
        let moved = tree
            .split_tab_into_new_group(StripId(1), 101, 2, &mut ids)
            .unwrap();
        assert!(desktop_doc::apply_tab_move(
            &mut doc,
            StripId(1),
            StripId(1),
            &moved
        ));
        assert_eq!(layout(&read_back(&doc)), layout(&tree));

        let before = layout(&read_back(&doc));
        let mut stale = moved;
        stale.source_group = 404;
        assert!(!desktop_doc::apply_tab_move(
            &mut doc,
            StripId(1),
            StripId(2),
            &stale
        ));
        assert_eq!(layout(&read_back(&doc)), before);
    }
    // Phase `reorders_removals_and_appends_follow_the_tree`.
    {
        let (mut tree, mut doc) = seeded();

        let apply = |tree: &mut DesktopTree, command: DesktopCommand| {
            apply_command(
                &mut tree.workspaces[0],
                &command,
                &mut || Err("a reorder mints nothing".into()),
                &mut |_, _| Err("a reorder creates nothing".into()),
            )
            .unwrap();
        };
        apply(
            &mut tree,
            DesktopCommand::ReorderGroup {
                strip: 1,
                group: 10,
                before: None,
            },
        );
        let moved = tree.workspaces[0].strips[0].window_groups[1].clone();
        assert!(desktop_doc::reorder_group(&mut doc, StripId(1), &moved, 1));
        apply(
            &mut tree,
            DesktopCommand::ReorderTab {
                location: TabLocation {
                    strip: 1,
                    group: 10,
                    tab: 100,
                },
                before: None,
            },
        );
        let tabs = tree.workspaces[0].strips[0].window_groups[1].tabs.clone();
        assert!(desktop_doc::rewrite_group_tabs(&mut doc, 10, &tabs));
        assert_eq!(layout(&read_back(&doc)), layout(&tree));

        assert!(desktop_doc::remove_tab(
            &mut doc,
            StripId(1),
            10,
            101,
            false
        ));
        assert!(desktop_doc::remove_tab(&mut doc, StripId(2), 20, 200, true));
        assert!(desktop_doc::append_tab(
            &mut doc,
            WorkspaceId(1),
            None,
            300,
            "new".into(),
            Vec::new(),
        ));
        // A fresh reader learns strips through their groups, so the emptied
        // strip 2 does not appear.
        let back = layout(&read_back(&doc));
        assert_eq!(back, vec![(1, vec![(11, vec![110, 300]), (10, vec![100])])]);
    }
}

fn shortcut(id: &str, name: &str) -> tilo_automation::Shortcut {
    serde_json::from_value(serde_json::json!({ "id": id, "name": name, "steps": [] }))
        .expect("a shortcut with no steps")
}

#[test]
fn shortcuts_are_seeded_saved_replaced_and_deleted() {
    let mut tree = tree();
    tree.workspaces[0].shortcuts = vec![shortcut("morning", "Morning")];
    let mut doc = AutoCommit::new();
    desktop_doc::seed(&mut doc, &tree);
    assert_eq!(
        read_back(&doc).workspaces[0].shortcuts,
        vec![shortcut("morning", "Morning")]
    );

    assert!(desktop_doc::upsert_shortcut(
        &mut doc,
        WorkspaceId(1),
        &shortcut("evening", "Evening")
    ));
    assert!(desktop_doc::upsert_shortcut(
        &mut doc,
        WorkspaceId(1),
        &shortcut("morning", "Wake up")
    ));
    assert_eq!(
        read_back(&doc).workspaces[0].shortcuts,
        vec![
            shortcut("evening", "Evening"),
            shortcut("morning", "Wake up")
        ]
    );

    assert!(desktop_doc::remove_shortcut(
        &mut doc,
        WorkspaceId(1),
        "morning"
    ));
    assert!(!desktop_doc::remove_shortcut(
        &mut doc,
        WorkspaceId(1),
        "morning"
    ));
    assert_eq!(
        read_back(&doc).workspaces[0].shortcuts,
        vec![shortcut("evening", "Evening")]
    );
    assert!(!desktop_doc::upsert_shortcut(
        &mut doc,
        WorkspaceId(9),
        &shortcut("x", "X")
    ));
}
