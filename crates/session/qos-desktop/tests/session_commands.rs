use qos_desktop::{intents::SessionIntent, session_commands::*, *};

fn source() -> (WorkspaceTree, CommandSource) {
    let mut tree = WorkspaceTree::bootstrap_default();
    let workspace = &mut tree.workspaces[0];
    workspace.strips[0].id = StripId(10);
    workspace.strips[0].window_groups = vec![WindowGroup {
        id: 20,
        active_tab: 31,
        width: WindowWidth::Normal,
        maximized: false,
        tabs: [30, 31, 32]
            .into_iter()
            .map(|id| Tab {
                id,
                title: "Renamed presentation".into(),
                tile: TileContent::AppPlaceholder {
                    kind: AppKind::Other("dev.qos.notes".into()),
                    label: "Not authority".into(),
                },
            })
            .collect(),
    }];
    let source = CommandSource {
        room: workspace.room.to_string(),
        heads: vec![[7; 32]],
    };
    (tree, source)
}

#[test]
fn drag_binds_source_identity_and_post_removal_destination_before_async_dispatch() {
    let (mut tree, source) = source();
    let bound = bind_intent(
        &tree,
        source.clone(),
        SessionIntent::ReorderTab {
            group: 20,
            from: 0,
            to: 1,
        },
    )
    .unwrap();
    // Another peer changes the source order after the gesture was bound.
    tree.workspaces[0].strips[0].window_groups[0]
        .tabs
        .swap(0, 2);
    assert_eq!(bound.source, source);
    assert_eq!(
        bound.command,
        DesktopCommand::ReorderTab {
            location: TabLocation {
                strip: 10,
                group: 20,
                tab: 30
            },
            before: Some(32),
        }
    );
    let last = bind_intent(
        &tree,
        bound.source.clone(),
        SessionIntent::ReorderTab {
            group: 20,
            from: 0,
            to: 2,
        },
    )
    .unwrap();
    assert!(matches!(
        last.command,
        DesktopCommand::ReorderTab { before: None, .. }
    ));
}

#[test]
fn missing_targets_and_wrong_room_never_bind_to_active_fallback() {
    let (tree, mut source) = source();
    assert!(
        bind_intent(
            &tree,
            source.clone(),
            SessionIntent::CloseTab { group: 99, tab: 30 }
        )
        .is_err()
    );
    assert!(
        bind_intent(
            &tree,
            source.clone(),
            SessionIntent::ReorderTab {
                group: 20,
                from: 3,
                to: 0
            }
        )
        .is_err()
    );
    source.room = RoomId::new().to_string();
    assert!(
        bind_intent(
            &tree,
            source,
            SessionIntent::CloseTab { group: 20, tab: 30 }
        )
        .is_err()
    );
}

#[test]
fn new_tab_uses_exact_selected_app_identity_and_stable_placement() {
    let (tree, source) = source();
    let command = bind_intent(
        &tree,
        source,
        SessionIntent::OpenTab {
            strip: StripId(10),
            group: 20,
        },
    )
    .unwrap();
    assert_eq!(
        command.command,
        DesktopCommand::OpenTab {
            app: "dev.qos.notes".into(),
            placement: TabPlacement::InGroup {
                strip: 10,
                group: 20,
                after: Some(31),
            },
        }
    );
}

#[test]
fn checked_record_failure_precedes_empty_workspace_topology_and_last_close_keeps_strip() {
    let mut workspace = Workspace::new(WorkspaceId(1), "Empty");
    let mut id = 100;
    let mut mint = || {
        id += 1;
        Ok(id)
    };
    let open = DesktopCommand::OpenTab {
        app: "dev.qos.notes".into(),
        placement: TabPlacement::EmptyWorkspace,
    };
    assert!(
        apply_command(&mut workspace, &open, &mut mint, &mut |_, _| Err(
            "record flush failed".into()
        ))
        .is_err()
    );
    assert!(
        workspace.strips.is_empty(),
        "failed checked record never publishes topology"
    );
    apply_command(&mut workspace, &open, &mut mint, &mut |_, _| {
        Ok((
            "Notes".into(),
            TileContent::AppPlaceholder {
                kind: AppKind::Other("dev.qos.notes".into()),
                label: "Notes".into(),
            },
        ))
    })
    .unwrap();
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
