use super::*;
use crate::shell::chrome_state::ChromeState;
use crate::shell::state::{Tab, WindowGroup, WindowWidth};

fn tab(id: TabId, app_id: &str, surface_id: u64) -> Tab {
    Tab {
        id,
        title: app_id.into(),
        tile: TileContent::Foreign {
            app_id: app_id.into(),
            surface_id,
        },
        app_title: None,
    }
}

#[test]
fn closing_an_app_closes_each_of_its_tabs_in_that_workspace_only() {
    let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
    let room = tree.desktop.workspaces[0].room();
    tree.desktop.workspaces[0].strips[0].window_groups = vec![
        WindowGroup {
            id: 100,
            tabs: vec![
                tab(1, "so.tilo.word-processor", 501),
                tab(2, "so.tilo.sheets", 502),
            ],
            active_tab: 1,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        },
        WindowGroup {
            id: 200,
            tabs: vec![tab(3, "so.tilo.Word-Processor", 503)],
            active_tab: 3,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        },
    ];
    tree.seed_doc_from_cache();
    let request = AppCloseRequest {
        app_id: "so.tilo.word-processor".into(),
        room,
    };
    let elsewhere = AppCloseRequest {
        room: RoomId::new(),
        ..request.clone()
    };
    assert!(tabs_of(&tree.desktop, &elsewhere).is_empty());
    assert_eq!(tabs_of(&tree.desktop, &request), [1, 3]);

    for surface in [501, 503] {
        crate::shell::foreign::store().note_adopted(surface);
    }
    let chrome = ChromeState::new(tree);
    let tabs = tabs_of(&chrome.borrow().tree().desktop, &request);
    for tab in tabs {
        crate::shell::window_actions::close_tab(&mut chrome.borrow_mut(), tab).unwrap();
    }
    assert_eq!(
        tabs_of(&chrome.borrow().tree().desktop, &request),
        [1, 3],
        "clients may defer their close decisions"
    );
    for surface in [501, 503] {
        crate::shell::tab_binding::close_tab_for_destroyed_surface_and_persist(
            &mut chrome.borrow_mut().tree_mut(),
            surface,
        );
        crate::shell::foreign::store().forget(surface);
    }
    let left = chrome
        .borrow()
        .tree()
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter().map(|tab| tab.id))
        .collect::<Vec<_>>();
    assert_eq!(left, [2], "the other app keeps its window");
    let mut asked = chrome.borrow_mut().take_surface_close_requests();
    asked.sort_unstable();
    assert_eq!(asked, [501, 503], "each client is asked to close itself");
}
