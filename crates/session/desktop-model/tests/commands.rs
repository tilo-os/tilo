#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable,
    clippy::arithmetic_side_effects,
    clippy::unnecessary_wraps,
    reason = "test fixtures: a failed lookup is the assertion"
)]

use tilo_desktop_model::commands::{
    DesktopCommand, OpenContext, TabLocation, TabPlacement, apply_command,
    apply_to_active_workspace, open_app_placement, resolve_open_app,
};
use tilo_desktop_model::{
    AppKind, DesktopTree, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot,
};

fn notes() -> TileContent {
    TileContent::AppPlaceholder {
        kind: AppKind::Other("so.tilo.notes".into()),
        label: "Notes".into(),
    }
}

fn minter(start: u64) -> impl FnMut() -> Result<u64, String> {
    let mut id = start;
    move || {
        id += 1;
        Ok(id)
    }
}

fn create(_: &str, _: u64) -> Result<(String, TileContent), String> {
    Ok(("Notes".into(), notes()))
}

fn workspace_with_group() -> Workspace {
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Shared");
    workspace.strips.push(tilo_desktop_model::Strip {
        id: StripId(10),
        name: "Main".into(),
        window_groups: vec![WindowGroup {
            id: 20,
            active_tab: 30,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
            tabs: [30, 31]
                .into_iter()
                .map(|id| Tab {
                    id,
                    title: "Notes".into(),
                    tile: notes(),
                    app_title: None,
                    view: String::new(),
                    pinned: false,
                })
                .collect(),
        }],
    });
    workspace
}

#[test]
fn a_failed_record_leaves_no_topology_and_the_last_close_keeps_its_strip() {
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Empty");
    let mut mint = minter(100);
    let open = DesktopCommand::OpenTab {
        app: "so.tilo.notes".into(),
        placement: TabPlacement::EmptyWorkspace,
    };
    assert!(
        apply_command(&mut workspace, &open, &mut mint, &mut |_, _| Err(
            "record flush failed".into()
        ))
        .is_err()
    );
    assert!(workspace.strips.is_empty());
    apply_command(&mut workspace, &open, &mut mint, &mut create).unwrap();
    let strip = &workspace.strips[0];
    let group = &strip.window_groups[0];
    let at = TabLocation {
        strip: strip.id.0,
        group: group.id,
        tab: group.tabs[0].id,
    };
    apply_command(
        &mut workspace,
        &DesktopCommand::CloseTab { location: at },
        &mut mint,
        &mut |_, _| panic!("close cannot recreate a record"),
    )
    .unwrap();
    assert_eq!(workspace.strips.len(), 1);
    assert!(workspace.strips[0].window_groups.is_empty());
    assert!(
        apply_command(
            &mut workspace,
            &DesktopCommand::CloseTab { location: at },
            &mut mint,
            &mut |_, _| unreachable!()
        )
        .is_err()
    );
}

#[test]
fn open_app_lands_in_a_new_group_of_the_last_owned_strip() {
    let mut workspace = workspace_with_group();
    assert_eq!(
        open_app_placement(&workspace, &OpenContext::default(), |_| true).unwrap(),
        TabPlacement::NewGroup {
            strip: 10,
            after: None
        }
    );
    assert!(open_app_placement(&workspace, &OpenContext::default(), |_| false).is_err());
    apply_command(
        &mut workspace,
        &DesktopCommand::OpenApp {
            app: "so.tilo.notes".into(),
            beside: None,
        },
        &mut minter(200),
        &mut create,
    )
    .unwrap();
    assert_eq!(workspace.strips[0].window_groups.len(), 2);
}

/// A window the desk opens takes a place on the canvas without anyone naming one: the framed
/// canvas, the spaces canvas and the traditional desktop draw a window group by its place, so
/// a group with none stands on none of them and a desk a person opened an app on looks empty.
/// The place runs on from the strip's windows, and a place already given is kept.
#[test]
fn an_opened_window_takes_a_place_the_canvas_can_draw_where_none_was_named() {
    let mut workspace = workspace_with_group();
    let held = workspace.arrangement.place_of(20);
    assert_eq!(held, None, "nothing has placed this window yet");
    apply_command(
        &mut workspace,
        &DesktopCommand::OpenApp {
            app: "so.tilo.notes".into(),
            beside: None,
        },
        &mut minter(200),
        &mut create,
    )
    .unwrap();
    let opened = workspace.strips[0].window_groups[1].id;
    let place = workspace
        .arrangement
        .place_of(opened)
        .expect("the window the desk opened stands on the canvas");
    assert!(place.width > 0 && place.height > 0, "{place:?}");
    // The group that was there took one too, at the strip's first point.
    let first = workspace
        .arrangement
        .place_of(20)
        .expect("the window that was there stands too");
    assert_eq!((first.x, first.y), (0, 0), "a strip starts at its corner");
    assert!(
        i64::from(place.x) >= first.right(),
        "the window the desk opened runs on from the one before it: {place:?} after {first:?}"
    );

    // A place someone gave is never moved: the desk only fills in what none stands on.
    let given = tilo_desktop_model::arrangement::CanvasPlace::new(-400, 250, 640, 480);
    apply_command(
        &mut workspace,
        &DesktopCommand::PlaceGroup {
            group: opened,
            place: given,
        },
        &mut minter(300),
        &mut create,
    )
    .unwrap();
    assert_eq!(workspace.arrangement.place_of(opened), Some(given));
    apply_command(
        &mut workspace,
        &DesktopCommand::OpenApp {
            app: "so.tilo.files".into(),
            beside: None,
        },
        &mut minter(400),
        &mut create,
    )
    .unwrap();
    assert_eq!(
        workspace.arrangement.place_of(opened),
        Some(given),
        "a window that stands where the person put it is not moved"
    );
}

/// An open for an actor names the group its view is in: the owner places
/// the new view right after it, on that group's strip, never at the end.
#[test]
fn open_app_beside_an_actors_group_lands_right_after_it() {
    let mut workspace = workspace_with_group();
    workspace.strips.push(tilo_desktop_model::Strip {
        id: StripId(11),
        name: "Agent".into(),
        window_groups: vec![WindowGroup {
            id: 40,
            active_tab: 41,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
            tabs: vec![Tab {
                id: 41,
                title: "Notes".into(),
                tile: notes(),
                app_title: None,
                view: String::new(),
                pinned: false,
            }],
        }],
    });
    let resolved = resolve_open_app(
        &workspace,
        DesktopCommand::OpenApp {
            app: "so.tilo.files".into(),
            beside: Some(20),
        },
        |_| true,
    )
    .unwrap();
    assert_eq!(
        resolved,
        DesktopCommand::OpenTab {
            app: "so.tilo.files".into(),
            placement: TabPlacement::NewGroup {
                strip: 10,
                after: Some(20)
            }
        }
    );
    let gone = resolve_open_app(
        &workspace,
        DesktopCommand::OpenApp {
            app: "so.tilo.files".into(),
            beside: Some(99),
        },
        |_| true,
    )
    .unwrap();
    assert_eq!(
        gone,
        DesktopCommand::OpenTab {
            app: "so.tilo.files".into(),
            placement: TabPlacement::NewGroup {
                strip: 11,
                after: None
            }
        },
        "a group that is gone falls back to the owner's own placement"
    );
}

#[test]
fn split_and_move_use_the_identities_the_owner_reserved() {
    let mut workspace = workspace_with_group();
    let at = TabLocation {
        strip: 10,
        group: 20,
        tab: 31,
    };
    apply_command(
        &mut workspace,
        &DesktopCommand::SplitTab {
            location: at,
            before: None,
        },
        &mut minter(500),
        &mut |_, _| unreachable!(),
    )
    .unwrap();
    let groups = &workspace.strips[0].window_groups;
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[1].id, 501, "the reserved group id, not a fresh one");
    assert_eq!(groups[1].tabs[0].id, 31);

    workspace
        .strips
        .push(tilo_desktop_model::Strip::new(StripId(11), "Side"));
    apply_command(
        &mut workspace,
        &DesktopCommand::MoveTab {
            location: TabLocation {
                strip: 10,
                group: 501,
                tab: 31,
            },
            placement: TabPlacement::NewGroup {
                strip: 11,
                after: None,
            },
        },
        &mut minter(600),
        &mut |_, _| unreachable!(),
    )
    .unwrap();
    assert_eq!(workspace.strips[1].window_groups[0].id, 601);
    assert_eq!(workspace.strips[0].window_groups.len(), 1);
}

/// Every host (the native shell, the browser) edits strips and workspace
/// names through this one path, so a name one refuses the other refuses
/// too, and a new strip is shown wherever it was made.
#[test]
fn edits_to_the_active_workspace_validate_names_and_show_a_new_strip() {
    let mut tree = DesktopTree {
        workspaces: vec![
            workspace_with_group(),
            Workspace::new(WorkspaceSlot(2), "Other"),
        ],
        active: WorkspaceSlot(1),
        ..DesktopTree::default()
    };
    let mut mint = minter(800);
    let made = apply_to_active_workspace(
        &mut tree,
        &DesktopCommand::CreateStrip {
            name: " Later ".into(),
        },
        &mut mint,
    )
    .unwrap()
    .expect("a created strip is returned");
    assert_eq!(made, StripId(801));
    assert_eq!(tree.workspaces[0].strips[1].name, "Later");
    assert_eq!(tree.active_strip.get(&WorkspaceSlot(1)), Some(&made));
    assert!(
        tree.workspaces[1].strips.is_empty(),
        "only the active workspace changes"
    );

    let renamed = apply_to_active_workspace(
        &mut tree,
        &DesktopCommand::RenameWorkspace {
            name: " Plans ".into(),
        },
        &mut mint,
    )
    .unwrap();
    assert_eq!(renamed, None);
    assert_eq!(tree.workspaces[0].name, "Plans");
    for refused in [
        DesktopCommand::RenameWorkspace { name: "  ".into() },
        DesktopCommand::CreateStrip {
            name: "\u{7}".into(),
        },
        DesktopCommand::OpenApp {
            app: "so.tilo.notes".into(),
            beside: None,
        },
    ] {
        assert!(apply_to_active_workspace(&mut tree, &refused, &mut mint).is_err());
    }
    assert_eq!(tree.workspaces[0].name, "Plans");
    assert_eq!(tree.workspaces[0].strips.len(), 2);

    // Removing the shown strip shows the workspace's first; a strip with
    // windows stays.
    assert!(
        apply_to_active_workspace(
            &mut tree,
            &DesktopCommand::RemoveStrip { strip: 10 },
            &mut mint
        )
        .is_err()
    );
    assert_eq!(
        apply_to_active_workspace(
            &mut tree,
            &DesktopCommand::RemoveStrip { strip: made.0 },
            &mut mint
        ),
        Ok(None)
    );
    assert_eq!(tree.workspaces[0].strips.len(), 1);
    assert_eq!(tree.active_strip.get(&WorkspaceSlot(1)), Some(&StripId(10)));
}

#[test]
fn names_are_validated_and_the_last_strip_cannot_be_removed() {
    let mut workspace = workspace_with_group();
    let mut mint = minter(700);
    assert!(
        apply_command(
            &mut workspace,
            &DesktopCommand::RenameWorkspace { name: "  ".into() },
            &mut mint,
            &mut create,
        )
        .is_err()
    );
    apply_command(
        &mut workspace,
        &DesktopCommand::CreateStrip {
            name: " Later ".into(),
        },
        &mut mint,
        &mut create,
    )
    .unwrap();
    let later = workspace.strips[1].id.0;
    assert_eq!(workspace.strips[1].name, "Later");
    apply_command(
        &mut workspace,
        &DesktopCommand::RemoveStrip { strip: later },
        &mut mint,
        &mut create,
    )
    .unwrap();
    assert!(
        apply_command(
            &mut workspace,
            &DesktopCommand::RemoveStrip { strip: 10 },
            &mut mint,
            &mut create,
        )
        .is_err(),
        "a strip with windows is not removed"
    );
}

#[test]
fn a_strip_moves_before_another_or_to_the_bottom_and_a_stale_move_changes_nothing() {
    let mut workspace = workspace_with_group();
    let mut mint = minter(800);
    for name in ["Second", "Third"] {
        apply_command(
            &mut workspace,
            &DesktopCommand::CreateStrip { name: name.into() },
            &mut mint,
            &mut create,
        )
        .unwrap();
    }
    let order = |workspace: &Workspace| {
        workspace
            .strips
            .iter()
            .map(|strip| strip.name.clone())
            .collect::<Vec<_>>()
    };
    let (main, third) = (workspace.strips[0].id.0, workspace.strips[2].id.0);
    apply_command(
        &mut workspace,
        &DesktopCommand::MoveStrip {
            strip: third,
            before: Some(main),
        },
        &mut mint,
        &mut create,
    )
    .unwrap();
    assert_eq!(order(&workspace), ["Third", "Main", "Second"]);
    apply_command(
        &mut workspace,
        &DesktopCommand::MoveStrip {
            strip: third,
            before: None,
        },
        &mut mint,
        &mut create,
    )
    .unwrap();
    assert_eq!(order(&workspace), ["Main", "Second", "Third"]);
    for stale in [
        DesktopCommand::MoveStrip {
            strip: 4040,
            before: None,
        },
        DesktopCommand::MoveStrip {
            strip: main,
            before: Some(4040),
        },
        DesktopCommand::MoveStrip {
            strip: main,
            before: Some(main),
        },
    ] {
        assert!(apply_command(&mut workspace, &stale, &mut mint, &mut create).is_err());
        assert_eq!(order(&workspace), ["Main", "Second", "Third"], "{stale:?}");
    }
}

#[test]
fn a_group_moves_whole_into_an_empty_strip_and_a_stale_move_changes_nothing() {
    let mut workspace = workspace_with_group();
    let mut mint = minter(700);
    apply_command(
        &mut workspace,
        &DesktopCommand::CreateStrip {
            name: "Below".into(),
        },
        &mut mint,
        &mut create,
    )
    .unwrap();
    let below = workspace.strips[1].id.0;
    let layout = |workspace: &Workspace| {
        workspace
            .strips
            .iter()
            .map(|strip| {
                let groups = strip
                    .window_groups
                    .iter()
                    .map(|group| (group.id, group.tabs.iter().map(|tab| tab.id).collect()))
                    .collect::<Vec<(u64, Vec<u64>)>>();
                (strip.id.0, groups)
            })
            .collect::<Vec<_>>()
    };
    let untouched = layout(&workspace);
    for stale in [
        DesktopCommand::MoveGroup {
            group: 20,
            strip: below,
            before: Some(404),
        },
        DesktopCommand::MoveGroup {
            group: 404,
            strip: below,
            before: None,
        },
        DesktopCommand::MoveGroup {
            group: 20,
            strip: 404,
            before: None,
        },
    ] {
        assert!(apply_command(&mut workspace, &stale, &mut mint, &mut create).is_err());
        assert_eq!(
            layout(&workspace),
            untouched,
            "{stale:?} must leave the desk alone"
        );
    }
    apply_command(
        &mut workspace,
        &DesktopCommand::MoveGroup {
            group: 20,
            strip: below,
            before: None,
        },
        &mut mint,
        &mut create,
    )
    .unwrap();
    assert!(workspace.strips[0].window_groups.is_empty());
    let moved = &workspace.strips[1].window_groups;
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].id, 20);
    assert_eq!(
        moved[0].tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
        [30, 31],
        "the group keeps its tabs and their order"
    );
}

/// A window sent to another workspace keeps being the same window, lands on
/// the strip that workspace shows and where the person is stays put.
#[test]
fn a_window_moves_to_another_workspaces_last_strip_and_the_person_stays() {
    let mut tree = DesktopTree {
        workspaces: vec![
            workspace_with_group(),
            Workspace::new(WorkspaceSlot(2), "Other"),
        ],
        active: WorkspaceSlot(1),
        ..DesktopTree::default()
    };
    let mut mint = minter(800);
    let moved = 31;
    tilo_desktop_model::commands::move_tab_to_workspace(
        &mut tree,
        moved,
        WorkspaceSlot(2),
        &mut mint,
    )
    .unwrap();
    assert_eq!(
        tree.active,
        WorkspaceSlot(1),
        "the person stays where they are"
    );
    assert_eq!(
        tree.workspaces[0].strips[0].window_groups[0]
            .tabs
            .iter()
            .map(|tab| tab.id)
            .collect::<Vec<_>>(),
        [30],
        "the rest of the group stays behind"
    );
    let landed = tree.workspaces[1]
        .strips
        .last()
        .expect("the other workspace has a strip");
    assert_eq!(
        landed.window_groups[0].tabs[0].id, moved,
        "the same window arrives"
    );
    assert_eq!(tree.active_strip.get(&WorkspaceSlot(2)), Some(&landed.id));
    assert_eq!(
        tree.active_group_for(landed.id),
        Some(landed.window_groups[0].id)
    );
    assert!(
        tilo_desktop_model::commands::move_tab_to_workspace(
            &mut tree,
            moved,
            WorkspaceSlot(2),
            &mut mint
        )
        .is_err(),
        "a window already there is not moved again"
    );
    assert!(
        tilo_desktop_model::commands::move_tab_to_workspace(
            &mut tree,
            9999,
            WorkspaceSlot(2),
            &mut mint
        )
        .is_err()
    );
}
