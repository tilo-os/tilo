use super::*;
use crate::component::{Fired, Intent};

fn person(id: &str, name: &str) -> Person {
    Person {
        hue: crate::tokens::PresenceHue::of_identity(id),
        identity: tilo_actions::MemberId::person(id).into(),
        name: name.into(),
        picture: None,
    }
}

#[test]
fn a_described_strip_reports_the_emoji_its_pill_toggles() {
    let _settled = crate::motion::testing::settled();
    let toggles: crate::Intents<Fired> = crate::Intents::default();
    let strip = ReactionStripWidget::from_props(
        "reactions",
        ReactionsProps {
            reactions: BTreeMap::new(),
            emoji: vec!["👍".into(), "🎉".into()],
            by: vec![
                vec![person("u_mia", "Mia"), person("u_leo", "Leo")],
                vec![person("u_leo", "Leo")],
            ],
            relayed: Vec::new(),
            can_react: None,
            toggle: Event::View(Intent::new(&toggles, "reactions", "toggle")),
        },
    );
    let mut frame = crate::headless::FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(strip),
        (420, 80),
        Theme::light(),
    );
    frame.key("Tab");
    frame.key("Enter");
    let fired = toggles.take();
    assert_eq!(fired.len(), 1, "the first pill toggled");
    assert_eq!(fired[0].value, Some(Value::Text("👍".into())));
}

#[test]
fn reactions_given_elsewhere_read_by_their_emoji_and_the_name_they_were_given_under() {
    let props = ReactionsProps {
        relayed: vec![
            "👍\tBo".to_owned(),
            "👍\tCy".to_owned(),
            ":party:\tDee".to_owned(),
        ],
        ..ReactionsProps::default()
    };
    let (_, _, relayed, ..) = said(props);
    assert_eq!(
        relayed,
        [
            ("👍".to_owned(), vec!["Bo".to_owned(), "Cy".to_owned()]),
            (":party:".to_owned(), vec!["Dee".to_owned()]),
        ]
    );
}

#[test]
fn a_strip_of_the_map_form_shows_each_emoji_in_key_order_with_who_reacted() {
    let _settled = crate::motion::testing::settled();
    let toggles: crate::Intents<Fired> = crate::Intents::default();
    let reactions = BTreeMap::from([
        (
            "👍".to_owned(),
            BTreeSet::from([person("u_mia", "Mia"), person("u_leo", "Leo")]),
        ),
        ("🎉".to_owned(), BTreeSet::from([person("u_leo", "Leo")])),
    ]);
    let groups = ReactionsProps::groups(reactions.clone(), Vec::new(), Vec::new());
    assert_eq!(
        groups
            .iter()
            .map(|(emoji, people)| (emoji.as_str(), people.len()))
            .collect::<Vec<_>>(),
        [("🎉", 1), ("👍", 2)]
    );
    let strip = ReactionStripWidget::from_props(
        "reactions",
        ReactionsProps {
            reactions,
            toggle: Event::View(Intent::new(&toggles, "reactions", "toggle")),
            ..ReactionsProps::default()
        },
    );
    let mut frame = crate::headless::FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(strip),
        (420, 80),
        Theme::light(),
    );
    frame.key("Tab");
    frame.key("Enter");
    let fired = toggles.take();
    assert_eq!(fired.len(), 1, "the first pill toggled");
    assert_eq!(fired[0].value, Some(Value::Text("🎉".into())));
}

#[test]
fn the_emoji_picker_button_opens_the_picker_and_reports_a_pick() {
    let _settled = crate::motion::testing::settled();
    let picks: crate::Intents<Fired> = crate::Intents::default();
    let button = EmojiPickerButton::from_props(
        "react",
        EmojiPickerProps {
            label: None,
            pick: Event::View(Intent::new(&picks, "react", "pick")),
        },
    );
    let mut frame = crate::headless::FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(button),
        (420, 520),
        Theme::light(),
    );
    frame.key("Tab");
    frame.key("Enter");
    let open = frame
        .with_widget_mut::<EmojiPickerButton, _>(|button| button.is_open())
        .unwrap_or(false);
    assert!(open, "activating the button opened the picker");
    assert!(picks.take().is_empty(), "nothing is picked yet");
}

/// A pill ends in the faces of who reacted with it, each in their own colour: who reacted reads
/// at a glance, not only in the tooltip.
#[test]
fn a_pill_shows_the_face_of_each_person_who_reacted_in_their_colour() {
    let _settled = crate::motion::testing::settled();
    let theme = Theme::light();
    let wearing = |slot: usize| Person {
        hue: crate::tokens::PresenceHue::from_slot(slot),
        ..person("u_mia", "Mia")
    };
    // How many pixels of the strip `who` reacting draws in `colour`.
    let painted = |who: Person, colour: crate::tokens::PresenceHue| {
        let strip = ReactionStripWidget::from_props(
            "reactions",
            ReactionsProps {
                reactions: BTreeMap::from([("👍".to_owned(), BTreeSet::from([who]))]),
                can_react: Some(false),
                ..ReactionsProps::default()
            },
        );
        let frame = crate::headless::FrameHarness::new_with_theme(
            crate::State::new(),
            Box::new(strip),
            (240, 60),
            theme.clone(),
        );
        let [r, g, b, _] = theme
            .tokens
            .colors
            .presence
            .hue(colour)
            .to_rgba8()
            .to_u8_array();
        frame
            .render_rgba()
            .chunks_exact(4)
            .filter(|px| {
                px[0].abs_diff(r) <= 24 && px[1].abs_diff(g) <= 24 && px[2].abs_diff(b) <= 24
            })
            .count()
    };
    let (blue, orange) = (
        crate::tokens::PresenceHue::from_slot(8),
        crate::tokens::PresenceHue::from_slot(1),
    );
    let mia_in_blue = painted(wearing(8), blue);
    assert!(
        mia_in_blue > 20,
        "her face shows in her colour: {mia_in_blue} pixels"
    );
    assert!(
        painted(wearing(8), orange) * 4 < mia_in_blue,
        "a colour she does not wear stays off the strip"
    );
    assert!(
        painted(wearing(1), orange) > 20,
        "in another colour, her face wears it"
    );
}
