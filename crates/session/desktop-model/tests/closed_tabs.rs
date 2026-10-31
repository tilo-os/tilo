//! A close can be undone: where it is remembered, where a reopen puts the
//! window back, and what a workspace forgets.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "a failed lookup in a test is the assertion"
)]

use tilo_desktop_model::commands::{DesktopCommand, TabLocation, apply_command};
use tilo_desktop_model::{
    CLOSED_TABS_PER_WORKSPACE, DesktopTree, Strip, StripId, Tab, TabId, TileContent, WindowGroup,
    WindowWidth, Workspace, WorkspaceId,
};

fn app(tab: TabId, title: &str, app: &str) -> Tab {
    Tab {
        id: tab,
        title: title.into(),
        tile: TileContent::LocalApp {
            app_id: app.into(),
            label: title.into(),
            note: String::new(),
        },
        app_title: None,
    }
}

fn window(tab: TabId, title: &str, app_id: &str) -> Tab {
    Tab {
        id: tab,
        title: title.into(),
        tile: TileContent::Foreign {
            app_id: app_id.into(),
            surface_id: tab.saturating_add(900),
        },
        app_title: None,
    }
}

fn group(id: u64, width: WindowWidth, tabs: Vec<Tab>) -> WindowGroup {
    let active_tab = tabs.first().map_or(0, |tab| tab.id);
    WindowGroup {
        id,
        tabs,
        active_tab,
        width,
        maximized: false,
        focus_history: Vec::new(),
    }
}

/// A desk of two strips: the first holds two windows, the second one, and
/// the workspace shows the first.
fn desk() -> DesktopTree {
    let mut workspace = Workspace::new(WorkspaceId(1), "Work");
    workspace.strips = vec![
        Strip {
            id: StripId(10),
            name: "Main".into(),
            window_groups: vec![
                group(
                    20,
                    WindowWidth::Normal,
                    vec![
                        app(30, "Notes", "so.tilo.notes"),
                        app(31, "Chat", "so.tilo.chat"),
                        app(32, "Files", "so.tilo.files"),
                    ],
                ),
                group(21, WindowWidth::Wide, vec![app(40, "Docs", "so.tilo.docs")]),
            ],
        },
        Strip {
            id: StripId(11),
            name: "Elsewhere".into(),
            window_groups: vec![group(
                22,
                WindowWidth::Narrow,
                vec![app(50, "Mail", "so.tilo.mail")],
            )],
        },
    ];
    let mut tree = DesktopTree {
        active: workspace.id,
        workspaces: vec![workspace],
        ..DesktopTree::default()
    };
    tree.active_strip.insert(WorkspaceId(1), StripId(10));
    tree.active_group.insert(StripId(10), 20);
    tree
}

fn ids(tree: &DesktopTree, group_id: u64) -> Vec<TabId> {
    tree.workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .find(|group| group.id == group_id)
        .map(|group| group.tabs.iter().map(|tab| tab.id).collect())
        .unwrap_or_default()
}

fn groups(tree: &DesktopTree, strip_id: StripId) -> Vec<u64> {
    tree.workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .find(|strip| strip.id == strip_id)
        .map(|strip| strip.window_groups.iter().map(|group| group.id).collect())
        .unwrap_or_default()
}

fn closes(tree: &DesktopTree) -> usize {
    tree.active_workspace().unwrap().closed_tabs().len()
}

/// A window comes back into its group at the index it had, and the newest
/// close is the one that comes back first.
#[test]
fn a_close_is_undone_where_it_happened_newest_first() {
    let mut tree = desk();
    assert!(tree.close_tab(31).is_some());
    assert!(tree.close_tab(32).is_some());
    assert_eq!(ids(&tree, 20), [30]);

    let reopened = tree.reopen_closed(WorkspaceId(1)).unwrap();
    assert_eq!(
        (reopened.strip, reopened.group, reopened.tab),
        (StripId(10), 20, 32)
    );
    assert_eq!(ids(&tree, 20), [30, 32], "back at the index it closed from");
    assert_eq!(tree.active_group_for(StripId(10)), Some(20), "and focused");

    let reopened = tree.reopen_closed(WorkspaceId(1)).unwrap();
    assert_eq!(reopened.tab, 31);
    assert_eq!(ids(&tree, 20), [30, 31, 32]);
    assert_eq!(
        tree.active_workspace().unwrap().strips[0].window_groups[0].active_tab,
        31,
        "the window that came back is in front of its group"
    );
    assert_eq!(closes(&tree), 0);
}

/// A window's app is what reopens: the session it ran in is gone with the
/// close, so the tab comes back as the app that opens it again, under the
/// same identity and the same title.
#[test]
fn a_window_is_remembered_as_the_app_that_opens_it_again() {
    let mut tree = desk();
    let workspace = tree.active_workspace_mut().unwrap();
    workspace.strips[0].window_groups[0].tabs[1] = window(31, "~/src", "org.example.Editor");
    tree.close_tab(31).unwrap();
    assert_eq!(closes(&tree), 1);

    let reopened = tree.reopen_closed(WorkspaceId(1)).unwrap();
    let tab = tree
        .active_workspace()
        .unwrap()
        .strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .find(|tab| tab.id == reopened.tab)
        .unwrap();
    assert_eq!(tab.title, "~/src");
    assert_eq!(
        tilo_desktop_model::apps::canonical_app_id(&tab.tile).as_deref(),
        Some("org.example.Editor"),
        "the same app opens again"
    );
    assert!(
        matches!(&tab.tile, TileContent::LocalApp { label, note, .. }
            if label == "~/src" && note.is_empty()),
        "{:?}",
        tab.tile
    );
}

/// A window that was its group's only one takes the group with it; a reopen
/// makes the group again where it stood, as wide as it was.
#[test]
fn a_group_closed_whole_is_made_again_where_it_stood() {
    let mut tree = desk();
    tree.close_tab(40).unwrap();
    assert_eq!(groups(&tree, StripId(10)), [20]);

    let reopened = tree.reopen_closed(WorkspaceId(1)).unwrap();
    assert_eq!(reopened.group, 21);
    assert_eq!(groups(&tree, StripId(10)), [20, 21], "its old place back");
    let made = &tree.active_workspace().unwrap().strips[0].window_groups[1];
    assert_eq!(made.width, WindowWidth::Wide, "as wide as it was");
    assert_eq!(tree.active_group_for(StripId(10)), Some(21));
}

/// A close whose strip is gone comes back in the strip the workspace shows,
/// in the place its group had.
#[test]
fn a_close_whose_strip_is_gone_comes_back_in_the_shown_strip() {
    let mut tree = desk();
    tree.close_tab(50).unwrap();
    tree.workspaces[0].strips.remove(1);
    assert_eq!(
        tree.reopen_closed(WorkspaceId(1)).unwrap().strip,
        StripId(10)
    );
    assert_eq!(groups(&tree, StripId(10)), [22, 20, 21]);
}

/// Each workspace remembers a bounded number of closes: the oldest are
/// forgotten, and a close is taken off the stack the one time it is undone.
#[test]
fn a_workspace_remembers_a_bounded_number_of_closes_each_undone_once() {
    let mut tree = desk();
    let many: Vec<Tab> = (0..CLOSED_TABS_PER_WORKSPACE + 10)
        .map(|i| app(100 + i as TabId, "Term", "so.tilo.terminal"))
        .collect();
    let strip = &mut tree.workspaces[0].strips[0];
    strip.window_groups = vec![group(20, WindowWidth::Normal, many)];
    let closed: Vec<TabId> = (0..CLOSED_TABS_PER_WORKSPACE as TabId + 10)
        .map(|i| 100 + i)
        .collect();
    for &tab in &closed {
        tree.close_tab(tab).unwrap();
    }
    assert_eq!(closes(&tree), CLOSED_TABS_PER_WORKSPACE);

    let mut reopened = Vec::new();
    while let Some(back) = tree.reopen_closed(WorkspaceId(1)) {
        reopened.push(back.tab);
    }
    assert_eq!(reopened.len(), CLOSED_TABS_PER_WORKSPACE);
    let last = CLOSED_TABS_PER_WORKSPACE;
    assert_eq!(
        reopened,
        closed[closed.len() - last..]
            .iter()
            .rev()
            .copied()
            .collect::<Vec<_>>(),
        "the newest first, the ten oldest forgotten"
    );
    assert!(tree.reopen_closed(WorkspaceId(1)).is_none(), "undone once");
}

/// A workspace that left takes its closes with it: nothing is reopened into
/// a workspace that is no longer there, and another workspace's closes are
/// untouched.
#[test]
fn a_workspace_that_left_takes_its_closes_with_it() {
    let mut tree = desk();
    let mut other = Workspace::new(WorkspaceId(2), "Other");
    other.strips = vec![Strip {
        id: StripId(12),
        name: "Main".into(),
        window_groups: vec![group(
            23,
            WindowWidth::Normal,
            vec![app(60, "Notes", "so.tilo.notes")],
        )],
    }];
    tree.workspaces.push(other);
    tree.close_tab(30).unwrap();
    tree.close_tab(60).unwrap();
    tree.workspaces
        .retain(|workspace| workspace.id != WorkspaceId(1));

    assert!(tree.reopen_closed(WorkspaceId(1)).is_none());
    assert_eq!(tree.reopen_closed(WorkspaceId(2)).unwrap().tab, 60);
}

/// A window a peer's layout brought back has had its close undone already:
/// reopening it again would make a second window under one identity.
#[test]
fn a_window_that_is_back_is_not_reopened_again() {
    let mut tree = desk();
    let window = tree.workspaces[0].strips[0].window_groups[0].tabs[1].clone();
    tree.close_tab(31).unwrap();
    tree.workspaces[0].strips[0].window_groups[0]
        .tabs
        .push(window);
    assert!(tree.reopen_closed(WorkspaceId(1)).is_none());
    assert_eq!(closes(&tree), 0);
    assert_eq!(ids(&tree, 20), [30, 32, 31], "no second window under it");
}

/// The desktop's reopen command reduces to the same restore, so a host that
/// holds only a workspace puts its last close back at the strip the caller
/// names when the one it remembers is gone.
#[test]
fn the_reopen_command_puts_back_the_last_close_of_its_workspace() {
    let mut workspace = desk().workspaces.remove(0);
    let mut mint = || Ok(999);
    let mut create = |_: &str, _: u64| Err("a reopen opens no record".to_owned());
    let close = DesktopCommand::CloseTab {
        location: TabLocation {
            strip: 11,
            group: 22,
            tab: 50,
        },
    };
    apply_command(&mut workspace, &close, &mut mint, &mut create).unwrap();
    workspace.strips.remove(1);
    assert_eq!(
        apply_command(
            &mut workspace,
            &DesktopCommand::ReopenClosed { strip: Some(10) },
            &mut mint,
            &mut create
        ),
        Ok(())
    );
    assert_eq!(workspace.strips[0].window_groups[0].id, 22, "its old place");
    assert_eq!(workspace.strips[0].window_groups[0].tabs[0].id, 50);
    // Nothing is left to reopen: the close was offered the one time it was
    // undone, so the command refuses rather than reopening anything twice.
    assert_eq!(
        apply_command(
            &mut workspace,
            &DesktopCommand::ReopenClosed { strip: None },
            &mut mint,
            &mut create
        )
        .unwrap_err(),
        "there is no closed window to reopen"
    );
}
