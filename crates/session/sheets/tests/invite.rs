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
use tilo_sheets::invite::{Invite, InviteApp, InviteEffect, InviteSeat, sample};
use ui::a11y::Action as A11yAction;
use ui::framework::testing::HeadlessApp;

const VIEWPORT: (u32, u32) = (1280, 900);
/// The link a made invitation hands over: the site's short link, which the
/// code carries too.
const LINK: &str = "https://tilo.sh/i/abcdefghijklmnop#AAABBBCCCDDDEEEFFFGGGHHHIIIJJJKKKLLLMMMNNNO";

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

/// Whether the widget with test id `id` is drawn.
fn drawn(app: &mut HeadlessApp<InviteApp>, id: &str) -> bool {
    fn find(node: &ui::headless::InspectionNode, id: &str) -> bool {
        (node.test_id.as_deref() == Some(id) && node.rect[2] > 0.0 && node.rect[3] > 0.0)
            || node.children.iter().any(|child| find(child, id))
    }
    find(&app.harness().inspection().root, id)
}

/// Which of `segments` stand chosen, in the order they are drawn.
fn chosen(app: &mut HeadlessApp<InviteApp>, segments: &[&str]) -> Vec<String> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.toggled() == Some(ui::accesskit::Toggled::True))
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .filter(|label| segments.contains(&label.as_str()))
        .collect()
}

/// Whether the control named `name` takes input.
fn enabled(app: &mut HeadlessApp<InviteApp>, name: &str) -> bool {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some(name))
        .is_some_and(|(_, node)| !node.is_disabled())
}

tilo_test_env::scenario! {
    /// What the invite sheet shows: the parts on offer, the link made, and who may make one.
    fn the_invite_sheet_shows_the_link_and_who_may_make_one;

    phase the_sheet_lists_the_part_the_link_brings_someone_in_at_and_who_it_is_for;
    {
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

    phase the_link_made_shows_its_code_and_its_copy_button_hands_it_over;
    // The link made is shown as text and as a code, with what it brings and how
    // long it works, and its button hands it to the clipboard.
    {
        let (mut app, seat) = seated(sample(Some("made")));
        for text in [LINK, "Works for the next hour."] {
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

    phase a_person_who_may_not_invite_here_is_told_why_and_cannot_make_a_link;
    {
        let mut app =
            HeadlessApp::<InviteApp>::capture_at(Some("unable"), VIEWPORT).expect("the sheet");
        assert!(
            says(
                &mut app,
                "Only someone whose part in this workspace allows inviting can make a link."
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

    phase the_part_on_offer_says_in_a_line_what_it_brings;
    // What the part picked brings whoever joins with it, in a line under the
    // choice, and it follows the pick.
    {
        let (mut app, _seat) = seated(sample(Some("made")));
        assert!(
            says(&mut app, "Can view, comment on, and edit shared work."),
            "the part the sheet opens on says what it brings"
        );
        press(&mut app, "invite-role:segment:0");
        assert!(
            says(&mut app, "Can view shared work, but cannot comment or edit it."),
            "picking another part says what that one brings"
        );
    }

    phase the_link_shown_is_the_one_the_choices_describe;
    // The choosers stand at what the link shown was made at, so nothing under them contradicts
    // them; picking another part or person puts that link away among those that still work and
    // offers to make one at the new choice, and picking back shows it again.
    {
        let (mut app, _seat) = seated(sample(Some("made")));
        let segments = ["Viewer", "Editor", "Anyone with the link", "Ada Lovelace-Byron"];
        assert_eq!(chosen(&mut app, &segments), ["Editor", "Ada Lovelace-Byron"]);
        assert!(drawn(&mut app, "invite-link"));
        assert!(!drawn(&mut app, "invite-pending-8f2c"), "the link shown is not listed again");

        press(&mut app, "invite-for:segment:0");
        assert_eq!(chosen(&mut app, &segments), ["Editor", "Anyone with the link"]);
        assert!(!drawn(&mut app, "invite-link"), "Ada's link is not shown as anyone's");
        assert!(!drawn(&mut app, "invite-code"));
        assert!(enabled(&mut app, "Create link"));
        assert!(drawn(&mut app, "invite-pending-8f2c"), "it still works, listed below");

        press(&mut app, "invite-for:segment:1");
        assert!(drawn(&mut app, "invite-link"));
        press(&mut app, "invite-role:segment:0");
        assert!(!drawn(&mut app, "invite-link"), "an editor's link is not shown as a viewer's");
    }

    phase the_choices_come_first_then_the_link_and_it_fits_without_scrolling;
    // In the order the person goes: the part and the person, then the link made at them, its code
    // centred as Add a device draws its own, then the links that still work. At 720 by 900 all of
    // it stands above the Close button, with nothing to scroll.
    {
        let (mut app, _seat) = seated(sample(Some("made")));
        app.harness().resize((720, 900)).unwrap();
        app.settle();
        let segments = ["Editor", "Viewer", "Anyone with the link", "Ada Lovelace-Byron"];
        assert_eq!(chosen(&mut app, &segments), ["Editor", "Ada Lovelace-Byron"]);
        let order = [
            "invite-role",
            "invite-for",
            "invite-code",
            "invite-link",
            "invite-copy-link",
            "invite-pending-77ad",
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
        let code = rect_of(&mut app, "invite-code");
        let field = rect_of(&mut app, "invite-link");
        let centre = |rect: [f64; 4]| rect[0] + rect[2] / 2.0;
        assert!((centre(code) - centre(field)).abs() <= 1.0, "the code is centred: {code:?}");
        let last = *rects.last().expect("the last row");
        let close = app
            .harness()
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.label() == Some("Close"))
            .and_then(|(_, node)| node.bounds())
            .expect("the Close button");
        assert!(
            last[1] + last[3] <= close.y0,
            "the last link that still works stands above Close: {last:?}, {close:?}"
        );
    }

    phase on_a_phone_nothing_is_wider_than_the_screen;
    {
        const PHONE: (u32, u32) = (390, 844);
        let (mut app, _seat) = seated(sample(Some("made")));
        app.harness().resize(PHONE).unwrap();
        app.settle();
        let width = f64::from(PHONE.0);
        for id in ["invite-role", "invite-for", "invite-code", "invite-link", "invite-copy-link", "invite-create"] {
            let rect = rect_of(&mut app, id);
            assert!(
                rect[0] >= 0.0 && rect[0] + rect[2] <= width,
                "{id} fits the phone's width: {rect:?}"
            );
        }
    }

    phase with_a_link_made_the_button_makes_another;
    // With a link made, making another is the secondary press, and says so.
    {
        let (mut app, _seat) = seated(sample(Some("made")));
        assert!(enabled(&mut app, "Create another link"));
        assert!(
            app.harness()
                .a11y_nodes()
                .into_iter()
                .all(|(_, node)| node.label() != Some("Create link")),
            "the first link's button is not offered beside one already made"
        );
    }
}

tilo_test_env::scenario! {
    /// What making a link and taking one back ask the shell.
    fn the_invite_sheets_buttons_ask_for_the_link_they_name;

    phase the_choices_reach_the_link_the_sheet_asks_for;
    // The person picks the part and whose device may redeem the link, and the
    // create button asks for the link they picked at: the sheet's own choices
    // reach the shell, and what they did not change stays as the link shown.
    {
        let (mut app, seat) = seated(sample(Some("made")));
        press(&mut app, "invite-role:segment:0");
        press(&mut app, "invite-create");
        let asked = seat.take_asked();
        assert!(
            asked.contains(&InviteEffect::Create {
                role: "viewer".to_owned(),
                recipient: "9a1f".to_owned(),
            }),
            "the part picked is the one asked for: {asked:?}"
        );
    }

    phase the_create_button_asks_for_an_editor_for_whoever_holds_it;
    {
        let (mut app, seat) = seated(sample(Some("made")));
        // The first chip is anyone holding the link, the next the person listed.
        press(&mut app, "invite-for:segment:0");
        press(&mut app, "invite-create");
        assert_eq!(
            seat.take_asked(),
            vec![InviteEffect::Create {
                role: "editor".to_owned(),
                recipient: String::new(),
            }],
            "the part stays the link's, for anyone holding it"
        );
    }

    phase taking_a_link_back_asks_for_that_invitation;
    {
        let (mut app, seat) = seated(sample(Some("made")));
        press(&mut app, "invite-revoke-3b9d");
        assert_eq!(
            seat.take_asked(),
            vec![InviteEffect::Revoke {
                invite: "3b9d".to_owned(),
            }],
            "the button takes back the link of its own row"
        );
    }

    phase taking_back_the_link_just_made_asks_for_that_invitation;
    // The link made is taken back beside the button that copies it.
    {
        let (mut app, seat) = seated(sample(Some("made")));
        press(&mut app, "invite-revoke-made");
        assert_eq!(
            seat.take_asked(),
            vec![InviteEffect::Revoke {
                invite: "8f2c".to_owned(),
            }],
            "taking back the link shown takes back the invitation it names"
        );
    }
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
