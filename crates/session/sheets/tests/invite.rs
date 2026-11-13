//! Invite people as a person, a screen reader and an agent meet them: the
//! link the sheet makes, the code it shows, the copy button, the links that
//! still work and taking one back, and what it says when this person may not
//! invite here.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_sheets::Seat as _;
use tilo_sheets::invite::{
    Created, Invite, InviteApp, InviteEffect, InviteSeat, Note, Pending, Person, Role,
};
use ui::a11y::Action as A11yAction;
use ui::framework::testing::HeadlessApp;

const VIEWPORT: (u32, u32) = (1280, 900);
/// The link a made invitation hands over, and the invitation's own bytes
/// behind it: what the code carries.
const LINK: &str = "tilo://join?invite=8f2c";
const CODE: &[u8] = b"the invitation's signed bytes";

/// The sheet as the session hands it out with a link made.
fn with_link() -> Invite {
    Invite {
        title: "Invite people".to_owned(),
        description: "Make a link that brings someone into this workspace.".to_owned(),
        close: "Close".to_owned(),
        may_invite: true,
        refusal: String::new(),
        role_label: "They join as".to_owned(),
        roles: vec![
            Role {
                key: "viewer".to_owned(),
                label: "Viewer".to_owned(),
                brings: "Can view shared work.".to_owned(),
            },
            Role {
                key: "editor".to_owned(),
                label: "Editor".to_owned(),
                brings: "Can view, comment on, and edit shared work.".to_owned(),
            },
        ],
        for_label: "This link is for".to_owned(),
        people: vec![Person {
            key: "9a1f".to_owned(),
            name: "Ada Lovelace-Byron".to_owned(),
        }],
        anyone: "Anyone with the link".to_owned(),
        create: "Create link".to_owned(),
        created: Some(Created {
            invite: "8f2c".to_owned(),
            link: LINK.to_owned(),
            code: CODE.to_vec(),
            link_label: "The link".to_owned(),
            brings: "Ada Lovelace-Byron joins as an editor.".to_owned(),
            lasts: "The link works for the next 60 minutes.".to_owned(),
            copy: "Copy link".to_owned(),
            revoke: "Take back".to_owned(),
            alt: "The invitation link as a code to scan".to_owned(),
        }),
        pending_title: "Links that still work".to_owned(),
        pending: vec![Pending {
            key: "3b9d".to_owned(),
            title: "Editor".to_owned(),
            whose: "For anyone with the link".to_owned(),
            lasts: "59 min left".to_owned(),
            revoke: "Take back".to_owned(),
        }],
        pending_empty: "No link is waiting to be used.".to_owned(),
        note: Some(Note {
            text: "The link is on the clipboard.".to_owned(),
            bad: false,
        }),
    }
}

/// The sheet shown through a seat as the shell lends it, which is where its
/// asks arrive.
fn seated(invite: Invite) -> (HeadlessApp<InviteApp>, InviteSeat) {
    let seat = InviteSeat::default();
    seat.show(invite);
    seat.presentation().open(false);
    let mut app = HeadlessApp::<InviteApp>::for_person_with_lent("person", seat.clone()).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    (app, seat)
}

fn press(app: &mut HeadlessApp<InviteApp>, id: &str) {
    app.harness()
        .perform_named(id, &A11yAction::Activate)
        .unwrap_or_else(|error| panic!("press {id}: {error}"));
    app.settle();
}

/// Whether some control or text the screen reader hears holds `text`.
fn says(app: &mut HeadlessApp<InviteApp>, text: &str) -> bool {
    app.harness().a11y_nodes().iter().any(|(_, node)| {
        node.label().is_some_and(|label| label.contains(text))
            || node.description().is_some_and(|said| said.contains(text))
            || node.value().is_some_and(|value| value.contains(text))
    })
}

/// Whether the control named `name` takes input.
fn enabled(app: &mut HeadlessApp<InviteApp>, name: &str) -> bool {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some(name))
        .is_some_and(|(_, node)| !node.is_disabled())
}

#[test]
fn the_sheet_lists_the_part_the_link_brings_someone_in_at_and_who_it_is_for() {
    let mut app = HeadlessApp::<InviteApp>::capture_at(Some("ready"), VIEWPORT).expect("the sheet");
    for text in [
        "Invite people",
        "They join as",
        "Editor",
        "This link is for",
        "Anyone with the link",
        "Ada Lovelace-Byron",
        "Links that still work",
        "For anyone with the link",
    ] {
        assert!(says(&mut app, text), "{text:?} is shown");
    }
    assert!(enabled(&mut app, "Create link"));
}

/// The person picks the part and whose device may redeem the link, and the
/// create button asks for the link they picked at: the sheet's own choices
/// reach the shell.
#[test]
fn the_choices_reach_the_link_the_sheet_asks_for() {
    let (mut app, seat) = seated(with_link());
    press(&mut app, "invite-role:segment:0");
    press(&mut app, "invite-create");
    let asked = seat.take_asked();
    assert!(
        asked.contains(&InviteEffect::Create {
            role: "viewer".to_owned(),
            recipient: String::new(),
        }),
        "the part picked is the one asked for: {asked:?}"
    );
}

#[test]
fn the_create_button_asks_for_an_editor_for_the_person_chipped() {
    let (mut app, seat) = seated(with_link());
    // The first chip is anyone holding the link, the next the person listed.
    press(&mut app, "invite-for:segment:1");
    press(&mut app, "invite-create");
    assert_eq!(
        seat.take_asked(),
        vec![InviteEffect::Create {
            role: "editor".to_owned(),
            recipient: "9a1f".to_owned(),
        }],
        "the sheet opens on the part a newcomer usually joins at, for the person chipped"
    );
}

/// The link made is shown as text and as a code, with what it brings and how
/// long it works, and its button hands it to the clipboard.
#[test]
fn the_link_made_shows_its_code_and_its_copy_button_hands_it_over() {
    let (mut app, seat) = seated(with_link());
    for text in [
        LINK,
        "Ada Lovelace-Byron joins as an editor.",
        "The link works for the next 60 minutes.",
        "The link is on the clipboard.",
    ] {
        assert!(says(&mut app, text), "{text:?} is shown");
    }
    press(&mut app, "invite-copy-link");
    assert_eq!(
        seat.take_asked(),
        vec![InviteEffect::Copy {
            link: LINK.to_owned(),
        }],
        "the copy button hands over the link shown"
    );
}

#[test]
fn taking_a_link_back_asks_for_that_invitation() {
    let (mut app, seat) = seated(with_link());
    press(&mut app, "invite-revoke-3b9d");
    assert_eq!(
        seat.take_asked(),
        vec![InviteEffect::Revoke {
            invite: "3b9d".to_owned(),
        }],
        "the button takes back the link of its own row"
    );
}

#[test]
fn a_person_who_may_not_invite_here_is_told_why_and_cannot_make_a_link() {
    let mut app =
        HeadlessApp::<InviteApp>::capture_at(Some("unable"), VIEWPORT).expect("the sheet");
    assert!(
        says(
            &mut app,
            "Only a member whose part here allows inviting can make a link."
        ),
        "the sheet says why not"
    );
    assert!(
        app.harness()
            .a11y_nodes()
            .into_iter()
            .all(|(_, node)| node.label() != Some("Create link")),
        "no button offers a link they may not make"
    );
}

/// What the part picked brings whoever joins with it, in a line under the
/// choice, and it follows the pick.
#[test]
fn the_part_on_offer_says_in_a_line_what_it_brings() {
    let (mut app, _seat) = seated(with_link());
    assert!(
        says(&mut app, "Can view, comment on, and edit shared work."),
        "the part the sheet opens on says what it brings"
    );
    press(&mut app, "invite-role:segment:0");
    assert!(
        says(&mut app, "Can view shared work."),
        "picking another part says what that one brings"
    );
}

/// The link made is taken back beside the button that copies it.
#[test]
fn taking_back_the_link_just_made_asks_for_that_invitation() {
    let (mut app, seat) = seated(with_link());
    press(&mut app, "invite-revoke-made");
    assert_eq!(
        seat.take_asked(),
        vec![InviteEffect::Revoke {
            invite: "8f2c".to_owned(),
        }],
        "taking back the link shown takes back the invitation it names"
    );
}

/// Where the widget with test id `id` stands, in logical window coordinates.
fn rect_of(app: &mut HeadlessApp<InviteApp>, id: &str) -> [f64; 4] {
    fn find(node: &ui::headless::InspectionNode, id: &str) -> Option<[f64; 4]> {
        if node.test_id.as_deref() == Some(id) && node.rect[2] > 0.0 && node.rect[3] > 0.0 {
            return Some(node.rect);
        }
        node.children.iter().find_map(|child| find(child, id))
    }
    find(&app.harness().inspection().root, id).unwrap_or_else(|| panic!("{id} is drawn"))
}

/// On a phone, the sheet reads in the order the person goes: the part they join as, the button
/// that makes the link, the link with the button that copies it and what that press did, and the
/// code to scan last and small. Nothing of it is wider than the screen.
#[test]
fn on_a_phone_the_link_follows_the_choice_and_the_code_comes_last_and_small() {
    const PHONE: (u32, u32) = (390, 844);
    let (mut app, _seat) = seated(with_link());
    app.harness().resize(PHONE).unwrap();
    app.settle();
    let order = [
        "invite-role",
        "invite-create",
        "invite-link",
        "invite-copy-link",
        "invite-note",
        "invite-code",
    ];
    let rects: Vec<[f64; 4]> = order.iter().map(|id| rect_of(&mut app, id)).collect();
    for (pair, ids) in rects.windows(2).zip(order.windows(2)) {
        assert!(
            pair[0][1] + pair[0][3] <= pair[1][1] + 0.5,
            "{} stands above {}: {:?} then {:?}",
            ids[0],
            ids[1],
            pair[0],
            pair[1]
        );
    }
    let width = f64::from(PHONE.0);
    for (id, rect) in order.iter().zip(&rects) {
        assert!(
            rect[0] >= 0.0 && rect[0] + rect[2] <= width,
            "{id} fits the phone's width: {rect:?}"
        );
    }
    let code = rect_of(&mut app, "invite-code");
    let field = rect_of(&mut app, "invite-link");
    assert!(
        code[2] <= 160.0,
        "the code is a small second way, not the sheet's subject: {code:?}"
    );
    assert!(
        code[2] < field[2],
        "the link, not the code, is the widest thing handed over"
    );
}

/// An invitation handed over as its own text, with no short link for it, is too dense for a code
/// a phone reads off a screen: the sheet leaves the code out rather than let it fill the sheet,
/// and the link and its Copy stay.
#[test]
fn an_invitation_too_dense_for_a_small_code_is_handed_over_by_copy_alone() {
    fn drawn(node: &ui::headless::InspectionNode, id: &str) -> bool {
        (node.test_id.as_deref() == Some(id) && node.rect[2] > 0.0)
            || node.children.iter().any(|child| drawn(child, id))
    }
    let mut invite = with_link();
    if let Some(created) = invite.created.as_mut() {
        created.code = vec![b'x'; 1200];
    }
    let (mut app, _seat) = seated(invite);
    let root = app.harness().inspection().root;
    assert!(!drawn(&root, "invite-code"), "no code fills the sheet");
    assert!(drawn(&root, "invite-link") && drawn(&root, "invite-copy-link"));
}
