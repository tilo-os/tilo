//! A reaction strip reports the emoji and the person a pill or face toggles,
//! and its faces wear their own colours.

use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::Event;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::motion::*;
use tilo_ui_base::offscreen::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::peniko::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_render::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

fn person(id: &str, name: &str) -> Person {
    Person {
        hue: PresenceHue::of_identity(id),
        identity: tilo_actions::MemberId::person(id).into(),
        name: name.into(),
        picture: None,
    }
}

#[test]
fn a_described_strip_reports_the_emoji_its_pill_toggles() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let toggles: tilo_ui_base::Intents<Fired> = tilo_ui_base::Intents::default();
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
            person: Event::default(),
        },
    );
    let mut frame = Driver::new_with_theme(
        tilo_ui_app::State::new(),
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
    let _settled = tilo_ui_base::motion::testing::settled();
    let toggles: tilo_ui_base::Intents<Fired> = tilo_ui_base::Intents::default();
    let reactions = BTreeMap::from([
        (
            "👍".to_owned(),
            BTreeSet::from([person("u_mia", "Mia"), person("u_leo", "Leo")]),
        ),
        ("🎉".to_owned(), BTreeSet::from([person("u_leo", "Leo")])),
    ]);
    let groups = ReactionsProps::groups_for_tests(reactions.clone(), Vec::new(), Vec::new());
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
    let mut frame = Driver::new_with_theme(
        tilo_ui_app::State::new(),
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
    let _settled = tilo_ui_base::motion::testing::settled();
    let picks: tilo_ui_base::Intents<Fired> = tilo_ui_base::Intents::default();
    let button = EmojiPickerButton::from_props(
        "react",
        EmojiPickerProps {
            label: None,
            pick: Event::View(Intent::new(&picks, "react", "pick")),
        },
    );
    let mut frame = Driver::new_with_theme(
        tilo_ui_app::State::new(),
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
    let _settled = tilo_ui_base::motion::testing::settled();
    let theme = Theme::light();
    let wearing = |slot: usize| Person {
        hue: tilo_ui_base::tokens::PresenceHue::from_slot(slot),
        ..person("u_mia", "Mia")
    };
    // How many pixels of the strip `who` reacting draws in `colour`.
    let painted = |who: Person, colour: tilo_ui_base::tokens::PresenceHue| {
        let strip = ReactionStripWidget::from_props(
            "reactions",
            ReactionsProps {
                reactions: BTreeMap::from([("👍".to_owned(), BTreeSet::from([who]))]),
                can_react: Some(false),
                ..ReactionsProps::default()
            },
        );
        let frame = Driver::new_with_theme(
            tilo_ui_app::State::new(),
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
        tilo_ui_base::tokens::PresenceHue::from_slot(8),
        tilo_ui_base::tokens::PresenceHue::from_slot(1),
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

/// A face among the reactions is chosen as any control is, from the keyboard too, and the strip
/// says whose it is: the person the view gave it, for the view to open their card.
#[test]
fn choosing_a_face_on_a_described_strip_reports_who_reacted() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let chosen: tilo_ui_base::Intents<Fired> = tilo_ui_base::Intents::default();
    let strip = ReactionStripWidget::from_props(
        "reactions",
        ReactionsProps {
            emoji: vec!["👍".into()],
            by: vec![vec![person("u_mia", "Mia"), person("u_leo", "Leo")]],
            person: Event::View(Intent::new(&chosen, "reactions", "person")),
            ..ReactionsProps::default()
        },
    );
    let mut frame = Driver::new_with_theme(
        tilo_ui_app::State::new(),
        Box::new(strip),
        (420, 80),
        Theme::light(),
    );
    // The pill, the add button, then each face in the order they reacted.
    frame.key("Tab");
    frame.key("ArrowRight");
    frame.key("ArrowRight");
    frame.key("ArrowRight");
    frame.key("Enter");
    let fired = chosen.take();
    assert_eq!(fired.len(), 1, "one face chosen");
    let Some(Value::Actor { identity, name, .. }) = &fired[0].value else {
        panic!("the strip says who: {:?}", fired[0].value);
    };
    assert_eq!(name, "Leo");
    assert_eq!(*identity, person("u_leo", "Leo").identity);
}
