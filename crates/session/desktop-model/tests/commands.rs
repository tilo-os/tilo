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
        },
        &mut minter(200),
        &mut create,
    )
    .unwrap();
    assert_eq!(workspace.strips[0].window_groups.len(), 2);
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
