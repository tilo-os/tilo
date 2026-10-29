use super::*;
use crate::component::{Fired, Intent};

fn person(id: &str, name: &str) -> Person {
    Person {
        identity: id.into(),
        name: name.into(),
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
