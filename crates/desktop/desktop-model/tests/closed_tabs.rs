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
    CLOSED_TABS_PER_WORKSPACE, Display, Layout, Strip, StripId, Tab, TabId, TileContent,
    WindowGroup, WindowWidth, Workspace, WorkspaceId,
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
        view: String::new(),
        pinned: false,
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
        view: String::new(),
        pinned: false,
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

/// A layout of two strips: the first holds two windows, the second one, and
/// the workspace shows the first.
fn layout() -> Layout {
    let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Work");
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
    let mut tree = Layout {
        active: workspace.id(),
        workspaces: vec![workspace],
        ..Layout::default()
    };
    tree.active_strip
        .insert(WorkspaceId::from_u128(1), StripId(10));
    tree.active_group.insert(StripId(10), 20);
    tree
}

/// The tab `id`, wherever it stands.
fn tab(tree: &Layout, id: TabId) -> Tab {
    tree.tab(id).cloned().expect("the tab stands")
}

/// Put the card an ended app leaves in tab `id`'s place, as the shell's window placement does.
fn card(tree: &mut Layout, id: TabId) {
    let held = tree
        .workspaces
        .iter_mut()
        .flat_map(|workspace| workspace.strips.iter_mut())
        .flat_map(|strip| strip.window_groups.iter_mut())
        .flat_map(|group| group.tabs.iter_mut())
        .find(|held| held.id == id)
        .expect("the tab stands");
    held.tile = TileContent::AppPlaceholder {
        kind: tilo_desktop_model::AppKind::Other("so.tilo.notes".into()),
        label: "so.tilo.notes".into(),
    };
}

fn ids(tree: &Layout, group_id: u64) -> Vec<TabId> {
    tree.workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .find(|group| group.id == group_id)
        .map(|group| group.tabs.iter().map(|tab| tab.id).collect())
        .unwrap_or_default()
}

fn groups(tree: &Layout, strip_id: StripId) -> Vec<u64> {
    tree.workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .find(|strip| strip.id == strip_id)
        .map(|strip| strip.window_groups.iter().map(|group| group.id).collect())
        .unwrap_or_default()
}

fn display(id: u32, monitor: &str, x: f64) -> Display {
    Display {
        id,
        monitor: monitor.into(),
        x,
        y: 0.0,
        width: 1280.0,
        height: 800.0,
        primary: false,
    }
}

fn closes(tree: &Layout) -> usize {
    tree.active_workspace().unwrap().closed_tabs().len()
}

tilo_test_env::scenario! {
    fn a_closed_tab_is_reopened_where_it_was;

    phase a_close_is_undone_where_it_happened_newest_first;
    // A window comes back into its group at the index it had, and the newest
    // close is the one that comes back first.
    {
        let mut tree = layout();
        assert!(tree.close_tab(31).is_some());
        assert!(tree.close_tab(32).is_some());
        assert_eq!(ids(&tree, 20), [30]);

        let reopened = tree.reopen_closed(WorkspaceId::from_u128(1), None).unwrap();
        assert_eq!(
            (reopened.strip, reopened.group, reopened.tab),
            (StripId(10), 20, 32)
        );
        assert_eq!(ids(&tree, 20), [30, 32], "back at the index it closed from");
        assert_eq!(tree.active_group_for(StripId(10)), Some(20), "and focused");

        let reopened = tree.reopen_closed(WorkspaceId::from_u128(1), None).unwrap();
        assert_eq!(reopened.tab, 31);
        assert_eq!(ids(&tree, 20), [30, 31, 32]);

        assert_eq!(
            tree.active_workspace().unwrap().strips[0].window_groups[0].active_tab,
            31,
            "the window that came back is in front of its group"
        );
        assert_eq!(closes(&tree), 0);
        // An app's reopen takes back only its own window, whatever closed after it, and leaves
        // the others' closes for whoever reopens them.
        assert!(tree.close_tab(31).is_some());
        assert!(tree.close_tab(32).is_some());
        assert!(tree.reopen_closed(WorkspaceId::from_u128(1), Some("so.tilo.mail")).is_none());
        let reopened = tree.reopen_closed(WorkspaceId::from_u128(1), Some("so.tilo.chat")).unwrap();
        assert_eq!(reopened.tab, 31, "the chat window, not the newer files one");
        assert_eq!(closes(&tree), 1, "the files close is still there");
        assert_eq!(tree.reopen_closed(WorkspaceId::from_u128(1), None).unwrap().tab, 32);
    }

    phase a_window_is_remembered_as_the_app_that_opens_it_again;
    // A window's app is what reopens: the session it ran in is gone with the
    // close, so the tab comes back as the app that opens it again, under the
    // same identity and the same title.
    {
        let mut tree = layout();
        let workspace = tree.active_workspace_mut().unwrap();
        workspace.strips[0].window_groups[0].tabs[1] = window(31, "~/src", "org.example.Editor");
        tree.close_tab(31).unwrap();
        assert_eq!(closes(&tree), 1);

        let reopened = tree.reopen_closed(WorkspaceId::from_u128(1), None).unwrap();
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

    phase a_group_closed_whole_is_made_again_where_it_stood;
    // A window that was its group's only one takes the group with it; a reopen
    // makes the group again where it stood, as wide as it was.
    {
        let mut tree = layout();
        tree.close_tab(40).unwrap();
        assert_eq!(groups(&tree, StripId(10)), [20]);

        let reopened = tree.reopen_closed(WorkspaceId::from_u128(1), None).unwrap();
        assert_eq!(reopened.group, 21);
        assert_eq!(groups(&tree, StripId(10)), [20, 21], "its old place back");
        let made = &tree.active_workspace().unwrap().strips[0].window_groups[1];
        assert_eq!(made.width, WindowWidth::Wide, "as wide as it was");
        assert_eq!(tree.active_group_for(StripId(10)), Some(21));
    }

    phase a_close_whose_strip_is_gone_comes_back_in_the_shown_strip;
    // A close whose strip is gone comes back in the strip the workspace shows,
    // in the place its group had.
    {
        let mut tree = layout();
        tree.close_tab(50).unwrap();
        tree.workspaces[0].strips.remove(1);
        assert_eq!(
            tree.reopen_closed(WorkspaceId::from_u128(1), None).unwrap().strip,
            StripId(10)
        );
        assert_eq!(groups(&tree, StripId(10)), [22, 20, 21]);
    }

    phase a_workspace_remembers_a_bounded_number_of_closes_each_undone_once;
    // Each workspace remembers a bounded number of closes: the oldest are
    // forgotten, and a close is taken off the stack the one time it is undone.
    {
        let mut tree = layout();
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
        while let Some(back) = tree.reopen_closed(WorkspaceId::from_u128(1), None) {
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
        assert!(
            tree.reopen_closed(WorkspaceId::from_u128(1), None).is_none(),
            "undone once"
        );
    }

    phase a_workspace_that_left_takes_its_closes_with_it;
    // A workspace that left takes its closes with it: nothing is reopened into
    // a workspace that is no longer there, and another workspace's closes are
    // untouched.
    {
        let mut tree = layout();
        let mut other = Workspace::new(WorkspaceId::from_u128(2), "Other");
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
            .retain(|workspace| workspace.id() != WorkspaceId::from_u128(1));

        assert!(tree.reopen_closed(WorkspaceId::from_u128(1), None).is_none());
        assert_eq!(tree.reopen_closed(WorkspaceId::from_u128(2), None).unwrap().tab, 60);
    }

    phase a_window_that_is_back_is_not_reopened_again;
    // A window a peer's layout brought back has had its close undone already:
    // reopening it again would make a second window under one identity.
    {
        let mut tree = layout();
        let window = tree.workspaces[0].strips[0].window_groups[0].tabs[1].clone();
        tree.close_tab(31).unwrap();
        tree.workspaces[0].strips[0].window_groups[0]
            .tabs
            .push(window);
        assert!(tree.reopen_closed(WorkspaceId::from_u128(1), None).is_none());
        assert_eq!(closes(&tree), 0);
        assert_eq!(ids(&tree, 20), [30, 32, 31], "no second window under it");
    }

    phase only_a_window_that_went_by_itself_comes_back_to_say_its_app_stopped;
    // The place a stopped app answers in is a window that went away by itself. A window the
    // person closed stays closed until they reopen it, and its app's end is theirs.
    {
        let mut tree = layout();
        tree.close_tab(31).unwrap();
        tree.window_went(40).unwrap();
        let workspace = &mut tree.workspaces[0];
        assert!(workspace.closed_tabs().closed_by_person(31));
        assert!(!workspace.closed_tabs().closed_by_person(40));
        assert!(
            !workspace.hold_ended(31, "so.tilo.chat"),
            "a window the person closed is not put back for its app"
        );
        assert!(workspace.hold_ended(40, "record:so.tilo.docs:plan"));
        assert_eq!(
            ids(&tree, 21),
            [40],
            "a window that went is put back where it stood"
        );
        assert_eq!(
            tree.reopen_closed(WorkspaceId::from_u128(1), None)
                .map(|put| (put.group, put.tab)),
            Some((20, 31)),
            "the person's reopen still brings back what they closed"
        );
    }

    phase the_window_an_ended_app_left_reopens_exactly_over_its_card;
    // An app ends under its window: the card says so in the window's place, and the window's
    // close stays remembered with the launch that opens it on what it showed. Another window of
    // the same app closes after it; reopening the ended one by its id brings back that window,
    // over its card, not the newest close of its app.
    {
        let mut tree = layout();
        tree.workspaces[0].strips[0].window_groups[0]
            .tabs
            .push(app(33, "Notes", "so.tilo.notes"));
        tree.window_went(30).unwrap();
        assert!(tree.workspaces[0].hold_ended(30, "record:so.tilo.notes:plan"));
        card(&mut tree, 30);
        tree.close_tab(33).unwrap();
        assert_eq!(ids(&tree, 20), [30, 31, 32], "the card stands in the window's place");

        let back = tree.reopen_closed_window(30).expect("the ended window reopens");
        assert_eq!((back.group, back.tab), (20, 30));
        assert_eq!(back.launch.as_deref(), Some("record:so.tilo.notes:plan"));
        assert_eq!(ids(&tree, 20), [30, 31, 32], "in its own place, once");
        assert!(
            matches!(&tab(&tree, 30).tile, TileContent::LocalApp { app_id, .. } if app_id == "so.tilo.notes"),
            "the window, not its card: {:?}",
            tab(&tree, 30).tile
        );
        assert!(tree.reopen_closed_window(30).is_none(), "a close is undone once");
        assert_eq!(
            tree.reopen_closed(WorkspaceId::from_u128(1), Some("so.tilo.notes"))
                .map(|put| put.tab),
            Some(33),
            "the other window of the app is still there to reopen"
        );
    }

    phase dismissing_the_card_leaves_the_window_to_reopen;
    // The person dismisses the card: what they closed is the window the card stood for, so their
    // reopen brings back that window on what it showed, not the card.
    {
        let mut tree = layout();
        tree.window_went(40).unwrap();
        assert!(tree.workspaces[0].hold_ended(40, "record:so.tilo.docs:plan"));
        card(&mut tree, 40);
        tree.close_tab(40).unwrap();
        assert!(tree.workspaces[0].closed_tabs().closed_by_person(40));
        assert_eq!(tree.workspaces[0].closed_tabs().len(), 1, "one close, the window's");
        let back = tree.reopen_closed(WorkspaceId::from_u128(1), None).unwrap();
        assert_eq!(back.tab, 40);
        assert_eq!(back.launch.as_deref(), Some("record:so.tilo.docs:plan"));
        assert!(matches!(tab(&tree, 40).tile, TileContent::LocalApp { .. }));
    }

    phase the_reopen_command_puts_back_the_last_close_of_its_workspace;
    // The desktop's reopen command reduces to the same restore, so a host that
    // holds only a workspace puts its last close back at the strip the caller
    // names when the one it remembers is gone.
    {
        let mut workspace = layout().workspaces.remove(0);
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
}

/// A window closed on one display comes back on that display, though the person went on working on
/// the other: the reopen puts it back where it was and the person with it.
#[test]
fn a_window_reopens_on_the_display_it_closed_on() {
    let mut tree = layout();
    let displays = [display(0, "LEFT", 0.0), display(1, "RIGHT", 1280.0)];
    tree.outputs.reconcile(&tree.workspaces, &displays);
    tree.outputs.set_focused(0);
    assert!(tree.outputs.place_unhomed(&tree.workspaces));
    assert!(tree.outputs.move_group(&tree.workspaces, 21, 1));
    assert_eq!(tree.outputs.display_of(21), Some(1));

    tree.close_tab(40).unwrap();
    // The displays are reconciled as every pass does, which forgets the group that closed, and the
    // person works on the other display.
    tree.outputs.reconcile(&tree.workspaces, &displays);
    tree.outputs.set_focused(0);

    let reopened = tree.reopen_closed(WorkspaceId::from_u128(1), None).unwrap();
    assert_eq!(reopened.group, 21);
    tree.outputs.place_unhomed(&tree.workspaces);
    assert_eq!(
        tree.outputs.display_of(21),
        Some(1),
        "back on the display it closed on"
    );
    assert_eq!(
        tree.outputs.focused_display(),
        Some(1),
        "and the person is there with it"
    );
}
