//! Naming a host window's tab: the person's name stands against the
//! window's own titles, and clearing it shows the latest of them.

use super::*;

fn tab_for(tree: &WorkspaceTree, surface_id: u64) -> crate::shell::state::Tab {
    tree.desktop
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .find(|tab| {
            matches!(&tab.tile, TileContent::Foreign { surface_id: s, .. } if *s == surface_id)
        })
        .cloned()
        .expect("the surface has a tab")
}

#[test]
fn a_window_title_while_the_tab_is_named_keeps_the_name() {
    let mut tree = WorkspaceTree::bootstrap_demo();
    let tab = adopt_toplevel(&mut tree, 41, Some("Alacritty".into()));
    assert!(rename_tab_for_title(&mut tree, 41, "~/src"));
    assert_eq!(rename_host_tab(&mut tree, tab, "Build"), Some(true));
    assert_eq!(tab_for(&tree, 41).title, "Build");

    // The terminal keeps announcing what runs in it.
    assert!(rename_tab_for_title(&mut tree, 41, "cargo build"));
    let named = tab_for(&tree, 41);
    assert_eq!(named.title, "Build");
    assert_eq!(named.app_title.as_deref(), Some("cargo build"));
    let revision = tree.desktop.external_revision;
    assert!(rename_tab_for_title(&mut tree, 41, "cargo build"));
    assert_eq!(
        tree.desktop.external_revision, revision,
        "the same title again is no change"
    );
    // A late app id carries a label of its own; the name still stands.
    assert!(rename_tab_for_app_id_with_title(
        &mut tree,
        41,
        "Alacritty",
        Some("vim")
    ));
    assert_eq!(tab_for(&tree, 41).title, "Build");

    // Clearing the name shows the window's latest title.
    assert_eq!(rename_host_tab(&mut tree, tab, "  "), Some(true));
    let cleared = tab_for(&tree, 41);
    assert_eq!(cleared.title, "vim");
    assert_eq!(cleared.app_title, None);
    assert_eq!(rename_host_tab(&mut tree, tab, ""), Some(false));
}

#[test]
fn a_name_is_recorded_as_a_change_to_the_tree() {
    let mut tree = WorkspaceTree::bootstrap_demo();
    let tab = adopt_toplevel(&mut tree, 42, Some("Alacritty".into()));
    let revision = tree.desktop.external_revision;
    assert_eq!(rename_host_tab(&mut tree, tab, "Logs"), Some(true));
    assert_ne!(tree.desktop.external_revision, revision);
    let mut from_doc = tilo_desktop_model::DesktopTree::default();
    tilo_desktop_model::desktop_doc::rebuild(&tree.doc, &mut from_doc);
    let rebuilt = from_doc
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .find(|t| t.id == tab)
        .cloned()
        .expect("the tab is in the desktop document");
    assert_eq!(rebuilt.title, "Logs");
    assert!(rebuilt.is_named());
}

#[test]
fn only_a_host_window_this_device_writes_may_be_renamed() {
    let mut tree = WorkspaceTree::bootstrap_demo();
    let host = adopt_toplevel(&mut tree, 43, Some("Alacritty".into()));
    assert!(may_rename_tab(&tree, host));

    let app_tab = tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].id;
    assert!(
        !matches!(
            tree.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].tile,
            TileContent::Foreign { .. }
        ),
        "the demo's first tab is a Tilo app"
    );
    assert!(!may_rename_tab(&tree, app_tab));
    assert_eq!(rename_host_tab(&mut tree, app_tab, "Mine"), None);

    // A Tilo package that connected as a client still titles itself.
    tree.remember_package_launch(host, "Alacritty".into());
    assert!(!may_rename_tab(&tree, host));
}

#[test]
fn a_window_another_device_hosts_is_that_devices_to_rename() {
    let mut tree = WorkspaceTree::bootstrap_demo();
    let local = tree.local_salt();
    let other = if local == 0x7A7A { 0x7B7B } else { 0x7A7A };
    let hosted = crate::shell::state::compose_id(other, 900);
    let group = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
    group.tabs.push(Tab {
        id: hosted,
        title: "Poster.xcf".into(),
        tile: TileContent::HostedWindow {
            app_id: "org.gimp.GIMP".into(),
            label: "Poster.xcf".into(),
            host: "ab".repeat(32),
            host_label: "Laptop".into(),
        },
        app_title: None,
    });
    assert!(!may_rename_tab(&tree, hosted));
    assert_eq!(rename_host_tab(&mut tree, hosted, "Poster"), None);

    // The same kind of window in this device's own namespace is its own.
    let mine = crate::shell::state::compose_id(local, 901);
    let group = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
    group.tabs.push(Tab {
        id: mine,
        title: "Board".into(),
        tile: TileContent::HostedWindow {
            app_id: "org.gimp.GIMP".into(),
            label: "Board".into(),
            host: "cd".repeat(32),
            host_label: "Desk".into(),
        },
        app_title: None,
    });
    assert!(may_rename_tab(&tree, mine));
}
