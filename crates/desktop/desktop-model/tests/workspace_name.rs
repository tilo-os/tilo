//! A workspace's name is its governance's alone. The layout keeps none: it is
//! projected onto the shell of each device showing the workspace
//! ([`Workspace::named_as`]), never written into the layout's document or its
//! stored form, so no device's layout holds a second, older name.

use automerge::AutoCommit;
use tilo_desktop_model::commands::{DesktopCommand, apply_to_active_workspace};
use tilo_desktop_model::desktop_doc;
use tilo_desktop_model::{Layout, Strip, StripId, Workspace, WorkspaceId};

fn named(name: &str) -> Layout {
    let identity = WorkspaceId::from_u128(42);
    let mut workspace = Workspace::new(identity, "");
    workspace.named_as(name);
    workspace.strips = vec![Strip::new(StripId(1), "A")];
    Layout {
        workspaces: vec![workspace],
        active: identity,
        ..Layout::default()
    }
}

#[test]
fn the_layout_keeps_no_name_of_its_own() {
    let tree = named("Studio");

    // Neither its stored form nor its document carries the name.
    let stored = serde_json::to_value(&tree).expect("the layout serializes");
    assert_eq!(stored["workspaces"][0].get("name"), None, "{stored}");
    let mut doc = AutoCommit::new();
    desktop_doc::seed(&mut doc, &tree);
    let mut fresh = Layout::default();
    desktop_doc::rebuild(&doc, &mut fresh);
    assert_eq!(fresh.workspaces.len(), 1);
    assert_eq!(
        fresh.workspaces[0].name, "",
        "a device that never projected it has none"
    );

    // A shell that projected the name keeps it as its layout changes underneath.
    let mut shown = named("Studio");
    desktop_doc::rebuild(&doc, &mut shown);
    assert_eq!(shown.workspaces[0].name, "Studio");
}

#[test]
fn a_rename_is_the_governances_to_take() {
    let mut tree = named("Studio");
    let mut next = 10;
    apply_to_active_workspace(
        &mut tree,
        &DesktopCommand::RenameWorkspace {
            name: "Plans".into(),
        },
        &mut || {
            next += 1;
            Ok(next)
        },
    )
    .expect("a valid name is taken");
    assert_eq!(
        tree.workspaces[0].name, "Studio",
        "the shell shows the governance's name until the governance takes the new one"
    );
    assert!(
        tree.workspaces[0].named_as("Plans"),
        "the projection brings it"
    );
    assert!(
        apply_to_active_workspace(
            &mut tree,
            &DesktopCommand::RenameWorkspace { name: " ".into() },
            &mut || Ok(99),
        )
        .is_err(),
        "a blank name is refused before anything is asked"
    );
}
