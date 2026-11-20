//! The bar as a page shows it: its parts where its layout stood them, each as what it is.

use super::fixture::FixtureChrome;
use super::*;
use tilo_desktop_model::{Layout, Member, Workspace, WorkspaceId};
use ui::composition::{Composed, Own};

fn bar_of(people: &[(&str, &str)]) -> TopBarChrome {
    ui::i18n::I18nManager::init("en-US");
    let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Hacked");
    workspace.members_here(
        people
            .iter()
            .map(|(id, name)| Member::named(collab_types::MemberId::person(*id), *name)),
    );
    for (here, member) in workspace.present.iter_mut().zip(&workspace.members) {
        here.device.clone_from(&member.id.id().to_owned());
    }
    let tree = Layout {
        active: workspace.id(),
        workspaces: vec![workspace],
        ..Layout::default()
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
/// invite is a button with its glyph, and each face is a button of its own over the picture of
/// its paint: each where the bar's layout stood it, never one picture of the whole bar.
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

    // Every control a reader reaches is a button where its target stands. Each face is a button
    // of its own, named for its person; nothing here opens everyone, so none says it does.
    let buttons: Vec<&Composed> = composed
        .children
        .iter()
        .filter(|child| child.tag == Some("button"))
        .collect();
    let target_of = |button: &Composed| {
        button
            .semantics
            .as_ref()
            .and_then(|node| node.action_target.clone())
    };
    for target in [
        TopBarChrome::CONTROL_WORKSPACE_SETTINGS,
        TopBarChrome::CONTROL_NOTIFICATIONS,
        TopBarChrome::CONTROL_INVITE,
        &TopBarChrome::face_target("ada"),
    ] {
        assert!(
            buttons
                .iter()
                .any(|button| target_of(button).as_deref() == Some(target)),
            "{target} is a button: {buttons:#?}"
        );
    }
    assert!(
        !buttons
            .iter()
            .any(|button| target_of(button).as_deref() == Some(TopBarChrome::CONTROL_PEOPLE)),
        "nothing in the bar opens everyone: the one face opens its person"
    );

    let ada = part(&composed, "face-ada");
    assert_eq!(ada.tag, Some("button"), "the face is a button of its own");
    let face_rect = bar.face_rect("ada").expect("Ada's face");
    assert_eq!(
        ada.rect,
        [
            face_rect.x0,
            face_rect.y0,
            face_rect.width(),
            face_rect.height()
        ],
        "where a press reaches it"
    );
    assert_eq!(
        ada.semantics.as_ref().and_then(|node| node.name.as_deref()),
        Some("Ada Lovelace"),
        "named for the person it opens"
    );
}

/// Where the bar counts people past the faces, the "+N" badge is the button that opens everyone,
/// at the badge's own place.
#[test]
fn the_badge_that_counts_the_rest_is_the_button_that_opens_everyone() {
    let people: Vec<(String, String)> = (0..6)
        .map(|i| (format!("p{i}"), format!("Person {i}")))
        .collect();
    let people: Vec<(&str, &str)> = people
        .iter()
        .map(|(id, name)| (id.as_str(), name.as_str()))
        .collect();
    let bar = bar_of(&people);
    assert!(bar.face_overflow() > 0, "crowded: a +N badge");
    let composed = ui::composition::compose_alone(&bar, &Theme::default(), "topbar");
    let button = composed
        .children
        .iter()
        .find(|child| {
            child.tag == Some("button")
                && child
                    .semantics
                    .as_ref()
                    .and_then(|node| node.action_target.as_deref())
                    == Some(TopBarChrome::CONTROL_PEOPLE)
        })
        .expect("the badge is the button for everyone here");
    assert_eq!(
        button
            .semantics
            .as_ref()
            .and_then(|node| node.name.as_deref()),
        Some("Everyone here"),
        "named for what a press opens"
    );
    let everyone = bar.control_rect(TopBarChrome::CONTROL_PEOPLE);
    assert_eq!(
        button.rect,
        [
            everyone.x0,
            everyone.y0,
            everyone.width(),
            everyone.height()
        ],
        "where a press opens everyone"
    );
    assert_eq!(
        everyone, bar.face_badge_rect,
        "the badge itself, not the faces beside it"
    );
}
