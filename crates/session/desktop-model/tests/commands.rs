//! The owner's reduction of desktop commands over a workspace.

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
    DesktopCommand, OpenContext, TabLocation, TabPlacement, apply_command, open_app_placement,
    resolve_open_app,
};
use tilo_desktop_model::{
    AppKind, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
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
    let mut workspace = Workspace::new(WorkspaceId(1), "Shared");
    workspace.strips.push(tilo_desktop_model::Strip {
        id: StripId(10),
        name: "Main".into(),
        window_groups: vec![WindowGroup {
            id: 20,
            active_tab: 30,
            width: WindowWidth::Normal,
            maximized: false,
            tabs: [30, 31]
                .into_iter()
                .map(|id| Tab {
                    id,
                    title: "Notes".into(),
                    tile: notes(),
                })
                .collect(),
        }],
    });
    workspace
}

#[test]
fn a_failed_record_leaves_no_topology_and_the_last_close_keeps_its_strip() {
    let mut workspace = Workspace::new(WorkspaceId(1), "Empty");
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
            tabs: vec![Tab {
                id: 41,
                title: "Notes".into(),
                tile: notes(),
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

#[test]
fn shortcut_commands_save_replace_and_delete_by_id_with_the_owner_as_saver() {
    let mut workspace = Workspace::new(WorkspaceId(1), "Desk");
    let json = r#"{"id":"tidy","name":"Tidy","steps":[],"saved_by":"someone-else"}"#;
    let stamped = tilo_desktop_model::commands::stamp_saver(json, "owner").unwrap();
    let save = |shortcut_json: String| DesktopCommand::SaveShortcut { shortcut_json };
    apply_command(&mut workspace, &save(stamped), &mut minter(0), &mut create).unwrap();
    assert_eq!(workspace.shortcuts.len(), 1);
    assert_eq!(workspace.shortcuts[0].saved_by.as_deref(), Some("owner"));
    assert!(
        workspace.strips.is_empty(),
        "a shortcut changes no topology"
    );

    let renamed = r#"{"id":"tidy","name":"Tidy up","steps":[]}"#.to_string();
    apply_command(&mut workspace, &save(renamed), &mut minter(0), &mut create).unwrap();
    assert_eq!(workspace.shortcuts.len(), 1, "the same id replaces");
    assert_eq!(workspace.shortcuts[0].name, "Tidy up");

    assert!(
        apply_command(
            &mut workspace,
            &save("{}".into()),
            &mut minter(0),
            &mut create
        )
        .is_err()
    );
    let delete = |id: &str| DesktopCommand::DeleteShortcut { id: id.into() };
    apply_command(&mut workspace, &delete("tidy"), &mut minter(0), &mut create).unwrap();
    assert!(workspace.shortcuts.is_empty());
    assert!(apply_command(&mut workspace, &delete("tidy"), &mut minter(0), &mut create).is_err());
}
