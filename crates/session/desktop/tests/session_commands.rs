#![allow(
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_desktop::intents::{DesktopIntent, LocalViewIntent, SessionIntent};
use tilo_desktop::session_commands::{DesktopCommandSink, bind_intent, dispatch};
use tilo_desktop_model::commands::{
    BoundCommand, CommandSource, DesktopCommand, TabLocation, TabPlacement,
};
use tilo_desktop_model::{
    AppKind, DesktopTree, RoomId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
    Workspace, WorkspaceId,
};

fn source() -> (DesktopTree, CommandSource) {
    let mut tree = DesktopTree {
        workspaces: vec![Workspace::new(WorkspaceId(1), "Home")],
        active: WorkspaceId(1),
        ..DesktopTree::default()
    };
    let workspace = &mut tree.workspaces[0];
    workspace.strips.push(Strip::new(StripId(10), "Main"));
    workspace.strips[0].window_groups = vec![WindowGroup {
        id: 20,
        active_tab: 31,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
        tabs: [30, 31, 32]
            .into_iter()
            .map(|id| Tab {
                id,
                title: "Renamed presentation".into(),
                tile: TileContent::AppPlaceholder {
                    kind: AppKind::Other("so.tilo.notes".into()),
                    label: "Not authority".into(),
                },
                app_title: None,
            })
            .collect(),
    }];
    let source = CommandSource {
        room: workspace.room().to_string(),
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
        bound.source,
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
fn a_group_move_binds_to_its_destination_strip_and_refuses_a_stale_one() {
    let (mut tree, source) = source();
    tree.workspaces[0]
        .strips
        .push(Strip::new(StripId(11), "Below"));
    let bound = bind_intent(
        &tree,
        source.clone(),
        SessionIntent::MoveGroup {
            group: 20,
            to: StripId(11),
            before: None,
        },
    )
    .unwrap();
    assert_eq!(
        bound.command,
        DesktopCommand::MoveGroup {
            group: 20,
            strip: 11,
            before: None,
        }
    );
    for stale in [
        SessionIntent::MoveGroup {
            group: 20,
            to: StripId(404),
            before: None,
        },
        SessionIntent::MoveGroup {
            group: 20,
            to: StripId(11),
            before: Some(20),
        },
    ] {
        assert!(bind_intent(&tree, source.clone(), stale).is_err());
    }
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
            app: "so.tilo.notes".into(),
            placement: TabPlacement::InGroup {
                strip: 10,
                group: 20,
                after: Some(31),
            },
        }
    );
}

/// A host that records what the controller hands it.
struct Recorder {
    tree: DesktopTree,
    heads: Vec<[u8; 32]>,
    local: Vec<LocalViewIntent>,
    submitted: Vec<BoundCommand>,
}
impl DesktopCommandSink for Recorder {
    fn with_tree<R>(&self, read: impl FnOnce(&DesktopTree) -> R) -> R {
        read(&self.tree)
    }
    fn source(&mut self, room: RoomId) -> Result<CommandSource, String> {
        Ok(CommandSource {
            room: room.to_string(),
            heads: self.heads.clone(),
        })
    }
    fn apply_local(&mut self, intent: LocalViewIntent) {
        self.local.push(intent);
    }
    fn submit(&mut self, command: BoundCommand) -> Result<(), String> {
        self.submitted.push(command);
        Ok(())
    }
}

#[test]
fn dispatch_keeps_view_changes_local_and_binds_session_changes_to_the_group_room() {
    let (tree, source) = source();
    let mut sink = Recorder {
        tree,
        heads: source.heads.clone(),
        local: Vec::new(),
        submitted: Vec::new(),
    };
    let maximize = LocalViewIntent::Maximized {
        group: 20,
        maximized: true,
    };
    dispatch(&mut sink, DesktopIntent::Local(maximize.clone())).unwrap();
    dispatch(
        &mut sink,
        DesktopIntent::Session(SessionIntent::CloseTab { group: 20, tab: 32 }),
    )
    .unwrap();
    assert_eq!(sink.local, vec![maximize]);
    assert_eq!(
        sink.submitted,
        vec![BoundCommand {
            source,
            command: DesktopCommand::CloseTab {
                location: TabLocation {
                    strip: 10,
                    group: 20,
                    tab: 32
                }
            },
        }]
    );
    assert!(
        dispatch(
            &mut sink,
            DesktopIntent::Session(SessionIntent::CloseTab { group: 99, tab: 30 }),
        )
        .is_err(),
        "a group no desktop holds never reaches the owner"
    );
    assert_eq!(sink.submitted.len(), 1);
}
