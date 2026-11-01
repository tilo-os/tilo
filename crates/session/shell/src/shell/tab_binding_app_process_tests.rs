use super::*;

fn tabs(tree: &WorkspaceTree) -> Vec<&Tab> {
    tree.desktop
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .collect()
}

#[test]
fn a_packaged_app_opens_as_a_window_waiting_to_be_launched() {
    let mut tree = WorkspaceTree::bootstrap_demo();
    let tab = open_app_process(
        &mut tree,
        &AppKind::Other("Editor".into()),
        "notes.txt",
        Placement::NewGroup,
    )
    .expect("the editor is a package");
    let opened = tabs(&tree).into_iter().find(|t| t.id == tab).unwrap();
    assert_eq!(
        opened.tile,
        TileContent::LocalApp {
            app_id: "so.tilo.code-editor".into(),
            label: "notes.txt".into(),
            note: String::new(),
        },
        "an empty note is what the restore path launches",
    );
    assert!(
        open_app_process(
            &mut tree,
            &AppKind::Other(tilo_desktop::feedback::KIND.into()),
            "Feedback",
            Placement::NewGroup,
        )
        .is_none(),
        "feedback is the shell's own surface",
    );
    // Nothing else is drawn by the session: Messages reaches the person's
    // accounts through the messaging service, and Passwords keeps its vault
    // under its own secrets capability. A tab the session would draw would
    // never get a window.
    for app in ["Messages", "Passwords"] {
        assert!(
            open_app_process(
                &mut tree,
                &AppKind::Other(app.into()),
                app,
                Placement::NewGroup,
            )
            .is_some(),
            "{app} runs as its own process",
        );
    }
}

#[test]
fn saved_app_tabs_reopen_as_their_apps_windows() {
    let mut tree = WorkspaceTree::bootstrap_demo();
    assert!(reopen_app_tabs_as_processes(&mut tree));
    let apps: Vec<_> = tabs(&tree)
        .into_iter()
        .filter_map(|tab| match &tab.tile {
            TileContent::LocalApp { app_id, note, .. } if note.is_empty() => {
                assert!(
                    in_own_namespace(tab.id, tree.local_salt()),
                    "only this device's own tabs reopen as saved windows",
                );
                Some(app_id.as_str())
            }
            TileContent::PeerWindow { app_id, note, .. } if note.is_empty() => {
                Some(app_id.as_str())
            }
            TileContent::AppPlaceholder { kind, .. } => {
                panic!("{kind:?} is still drawn by the shell")
            }
            _ => None,
        })
        .collect();
    for app in [
        "so.tilo.notes",
        "so.tilo.file-explorer",
        "so.tilo.terminal",
        "so.tilo.word-processor",
    ] {
        assert!(apps.contains(&app), "{app} reopens: {apps:?}");
    }
    assert!(
        !reopen_app_tabs_as_processes(&mut tree),
        "nothing left to turn"
    );
}

/// A device that mints under its own salt reopens the tabs of a tree saved
/// before trees had one (salt `0`) as its own saved windows: no other device
/// holds them, so none would ever hand over a record for them to wait on.
#[test]
fn unsalted_saved_app_tabs_reopen_as_this_devices_windows() {
    let mut tree = WorkspaceTree::bootstrap_demo();
    let unsalted: Vec<TabId> = tabs(&tree)
        .into_iter()
        .filter(|tab| {
            matches!(&tab.tile, TileContent::AppPlaceholder { kind, .. }
                if own_process_package(kind).is_some())
        })
        .map(|tab| tab.id)
        .collect();
    assert!(!unsalted.is_empty());
    assert!(
        unsalted
            .iter()
            .all(|id| crate::shell::state::peer_salt_of(*id) == 0),
        "the demo tree mints in no namespace"
    );
    tree.set_peer_salt(0x1234);
    assert!(reopen_app_tabs_as_processes(&mut tree));
    for tab in tabs(&tree)
        .into_iter()
        .filter(|tab| unsalted.contains(&tab.id))
    {
        assert!(
            matches!(&tab.tile, TileContent::LocalApp { note, .. } if note.is_empty()),
            "{} reopens as {:?}, not this device's own window",
            tab.title,
            tab.tile
        );
    }
}
