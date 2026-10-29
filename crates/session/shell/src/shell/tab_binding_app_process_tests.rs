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
}

#[test]
fn saved_app_tabs_reopen_as_their_apps_windows() {
    let mut tree = WorkspaceTree::bootstrap_demo();
    assert!(reopen_app_tabs_as_processes(&mut tree));
    let apps: Vec<_> = tabs(&tree)
        .into_iter()
        .filter_map(|tab| match &tab.tile {
            TileContent::LocalApp { app_id, note, .. } if note.is_empty() => {
                assert_eq!(
                    crate::shell::state::peer_salt_of(tab.id),
                    tree.local_salt(),
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
