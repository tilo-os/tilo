//! The shortcut guide as a person, a screen reader and an agent meet it:
//! what it shows for each input, and what pressing a key or Done asks the
//! shell.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_sheets::Seat as _;
use tilo_sheets::shortcut_guide::{
    Guide, GuideEffect, GuideSeat, Modality, Pair, Shortcut, ShortcutGuideApp,
};
use ui::a11y::Action as A11yAction;
use ui::framework::testing::HeadlessApp;
use ui::input::{KeyLegend, KeyboardBinding, KeyboardModifier};

const VIEWPORT: (u32, u32) = (1280, 800);

fn staged(scenario: &str) -> HeadlessApp<ShortcutGuideApp> {
    HeadlessApp::<ShortcutGuideApp>::capture_at(Some(scenario), VIEWPORT).unwrap()
}

/// Whether the guide says `text`, as a name or a description.
fn says(app: &mut HeadlessApp<ShortcutGuideApp>, text: &str) -> bool {
    app.harness().a11y_nodes().iter().any(|(_, node)| {
        node.label().is_some_and(|label| label.contains(text))
            || node.description().is_some_and(|said| said.contains(text))
            || node.value().is_some_and(|value| value.contains(text))
    })
}

fn guide() -> Guide {
    let keys =
        |key: &str| KeyboardBinding::new([KeyboardModifier::System], KeyLegend::from_name(key));
    Guide {
        modality: Modality::Keys,
        pairs: vec![Pair {
            label: "Windows".to_owned(),
            previous: Shortcut {
                action: "focus-previous-window".to_owned(),
                label: "Previous window".to_owned(),
                keys: keys("Left"),
            },
            next: Shortcut {
                action: "focus-next-window".to_owned(),
                label: "Next window".to_owned(),
                keys: keys("Right"),
            },
            changed: true,
        }],
        ..Guide::default()
    }
}

fn seated(guide: Guide) -> (HeadlessApp<ShortcutGuideApp>, GuideSeat) {
    let seat = GuideSeat::default();
    seat.show(guide);
    seat.presentation().open(false);
    let mut app =
        HeadlessApp::<ShortcutGuideApp>::for_person_with_lent("person", seat.clone()).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    (app, seat)
}

#[test]
fn every_input_has_its_guide() {
    let mut app = staged("keyboard");
    assert!(says(&mut app, "Windows"));
    assert!(says(&mut app, "Close window"), "the essentials are listed");
    let mut app = staged("controller");
    assert!(says(&mut app, "This guide"));
    let mut app = staged("touch");
    assert!(
        app.harness().a11y_nodes().len() > 8,
        "the gestures are listed"
    );
}

#[test]
fn the_guide_paints_a_sheet_and_leaves_the_rest_to_the_desktop_scrim() {
    let mut app = staged("keyboard");
    let pixels = app.harness().render_rgba_transparent();
    let alpha = |x: u32, y: u32| pixels[usize::try_from((y * VIEWPORT.0 + x) * 4 + 3).unwrap()];
    assert!(alpha(640, 400) > 0, "the sheet is painted");
}

#[test]
fn pressing_a_key_asks_the_shell_for_its_new_chord() {
    let (mut app, seat) = seated(guide());
    app.harness()
        .perform_named("guide-key-focus-next-window", &A11yAction::Activate)
        .unwrap();
    app.settle();
    assert_eq!(
        seat.take_asked(),
        [GuideEffect::Listen {
            action: "focus-next-window".to_owned()
        }]
    );
}

#[test]
fn a_changed_pair_can_go_back_to_its_defaults() {
    let (mut app, seat) = seated(guide());
    app.harness()
        .perform_named("guide-reset-focus-previous-window", &A11yAction::Activate)
        .unwrap();
    app.settle();
    assert_eq!(
        seat.take_asked(),
        [GuideEffect::Reset {
            first: "focus-previous-window".to_owned(),
            second: "focus-next-window".to_owned(),
        }]
    );
}

#[test]
fn listening_says_which_shortcut_takes_the_next_chord() {
    let (mut app, seat) = seated(guide());
    seat.show(Guide {
        listening: Some("focus-next-window".to_owned()),
        ..guide()
    });
    app.settle();
    assert!(says(&mut app, "Next window"));
}

#[test]
fn escape_closes_the_guide() {
    let (mut app, seat) = seated(guide());
    app.harness().key("Escape");
    app.settle();
    assert_eq!(seat.take_asked(), [GuideEffect::Done]);
    assert!(!seat.presentation().is_open());
}

#[test]
fn an_agent_closes_the_guide_through_the_same_door() {
    let (mut app, seat) = seated(guide());
    app.effect(&GuideEffect::Done).unwrap();
    app.settle();
    assert_eq!(seat.take_asked(), [GuideEffect::Done]);
}

#[test]
fn every_scenario_renders() {
    for scenario in ["keyboard", "listening", "controller", "touch"] {
        let mut app = staged(scenario);
        assert!(
            says(&mut app, "Done") || says(&mut app, "Close"),
            "{scenario}"
        );
    }
}

#[test]
fn a_listening_key_says_what_to_press_with_escape_as_its_own_hint() {
    let mut app = staged("listening");
    assert!(says(&mut app, "Press a new shortcut for “Next window”"));
    assert!(says(&mut app, "Cancel"), "Escape's hint is named");
    assert!(
        !says(&mut app, "·") && !says(&mut app, "Esc cancels"),
        "the way out is a key hint, not a glyph-joined sentence"
    );
}
