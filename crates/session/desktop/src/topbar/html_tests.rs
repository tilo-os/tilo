//! The bar as a page shows it: its parts where its layout stood them, each as what it is.

use super::fixture::FixtureChrome;
use super::*;
use tilo_desktop_model::{Collaborator, DesktopTree, Workspace, WorkspaceSlot};
use ui::composition::{Composed, Own};

fn bar_of(people: &[(&str, &str)]) -> TopBarChrome {
    ui::i18n::I18nManager::init("en-US");
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Hacked");
    workspace.collaborators = people
        .iter()
        .map(|(id, name)| Collaborator::new(*id, *name))
        .collect();
    let tree = DesktopTree {
        active: workspace.slot,
        workspaces: vec![workspace],
        ..DesktopTree::default()
    };
    let chrome = FixtureChrome::new(tree.clone());
    chrome.borrow_mut().can_invite = true;
    let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);
    bar.sync_from_tree(&tree);
    super::tests::lay_out_and_draw(&mut bar, 1600.0);
    bar
}

fn part<'a>(bar: &'a Composed, key: &str) -> &'a Composed {
    let path = format!("topbar#{key}");
    bar.children
        .iter()
        .find(|child| child.path == path)
        .unwrap_or_else(|| panic!("no {path} in {:#?}", bar.children))
}

/// The workspace's name is a real button with its words and chevron, the time is words, the
/// invite is a button with its glyph, and the faces are pictures of their paint: each where the
/// bar's layout stood it, never one picture of the whole bar.
#[test]
fn the_bar_shows_its_parts_as_what_they_are() {
    let bar = bar_of(&[("ada", "Ada Lovelace")]);
    let composed = ui::composition::compose_alone(&bar, &Theme::default(), "topbar");
    assert_eq!(composed.tag, Some("header"));
    assert!(composed.own.is_none(), "the bar is not a picture of itself");

    let workspace = part(&composed, "workspace");
    assert_eq!(workspace.tag, Some("button"));
    let Some(Own::Face(face)) = &workspace.own else {
        panic!("the workspace's name is a face: {workspace:#?}");
    };
    assert_eq!(
        face.label.as_ref().map(|label| label.text.as_str()),
        Some("Hacked")
    );
    assert!(face.trailing.is_some(), "its menu's chevron");
    let rect = bar.ws_pill_rect;
    assert_eq!(
        workspace.rect,
        [rect.x0, rect.y0, rect.width(), rect.height()],
        "where the layout stood it"
    );

    let centre = part(&composed, "centre");
    let Some(Own::Text(time)) = &centre.own else {
        panic!("the time is words: {centre:#?}");
    };
    assert_eq!(time.text, bar.current_clock_string());
    assert_eq!(time.align, "center");

    let invite = part(&composed, "invite");
    assert_eq!(invite.tag, Some("button"));
    assert!(
        matches!(&invite.own, Some(Own::Face(face)) if face.icon.is_some()),
        "the invite shows its glyph: {invite:#?}"
    );

    let faces = part(&composed, "faces");
    assert!(matches!(faces.own, Some(Own::Painted { .. })));

    // Every control a reader reaches is a button where its target stands.
    let buttons: Vec<&Composed> = composed
        .children
        .iter()
        .filter(|child| child.tag == Some("button"))
        .collect();
    for target in [
        TopBarChrome::CONTROL_WORKSPACE_SETTINGS,
        TopBarChrome::CONTROL_NOTIFICATIONS,
        TopBarChrome::CONTROL_COLLABORATORS,
        TopBarChrome::CONTROL_INVITE,
    ] {
        assert!(
            buttons.iter().any(|button| button
                .semantics
                .as_ref()
                .and_then(|node| node.action_target.as_deref())
                == Some(target)),
            "{target} is a button: {buttons:#?}"
        );
    }
}
