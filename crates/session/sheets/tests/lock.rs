//! The lock screen as a person, a screen reader and an agent meet it: the
//! clock, whose session it is, the password field, what plays, and the
//! controls each publishes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::mpsc::Receiver;

use tilo_sheets::Seat as _;
use tilo_sheets::lock::{LockApp, LockEffect, LockSeat, Media, Shown, Status};
use ui::a11y::Action as A11yAction;
use ui::framework::testing::HeadlessApp;
use ui::passcode_keyboard::{PasscodeKey, PasscodeKeyboard};
use ui::tokens::Spacing;
use ui::vello::kurbo::{Point, Rect};

const VIEWPORT: (u32, u32) = (1280, 800);

/// A locked session's screen, showing `shown` on the seat the app reports
/// to, and what it asks the session.
fn locked(shown: Shown) -> (HeadlessApp<LockApp>, LockSeat, Receiver<LockEffect>) {
    let seat = LockSeat::default();
    // What the screen asks goes to the session through the connection its
    // app opens; the test stands in for it here.
    let (outgoing, asked) = std::sync::mpsc::channel();
    seat.ask_with(outgoing);
    seat.show(shown);
    seat.presentation().open(false);
    let mut app = HeadlessApp::<LockApp>::for_person_with_lent("person", seat.clone()).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    (app, seat, asked)
}

/// Everything the screen has asked the session since the last look.
fn asks(asked: &Receiver<LockEffect>) -> Vec<LockEffect> {
    asked.try_iter().collect()
}

/// The passwords the screen handed the session since the last look, oldest
/// first.
fn typed(asked: &Receiver<LockEffect>) -> Vec<String> {
    asked
        .try_iter()
        .filter_map(|effect| match effect {
            LockEffect::TypePassword { password } => Some(password),
            _ => None,
        })
        .collect()
}

fn screen() -> Shown {
    Shown {
        clock: "10:42".to_owned(),
        date: "Monday, 27 October".to_owned(),
        person: "Ada Lovelace".to_owned(),
        status: Status {
            keyboard: true,
            ..Status::default()
        },
        ..Shown::default()
    }
}

fn song() -> Media {
    Media {
        title: "Clair de lune".to_owned(),
        subtitle: "Claude Debussy".to_owned(),
        playing: true,
        can_toggle: true,
        can_previous: true,
        can_next: true,
    }
}

/// Every control the screen offers, named as the screen reader says it, with
/// where it sits.
fn controls(app: &mut HeadlessApp<LockApp>) -> Vec<(String, Rect)> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.role() == ui::accesskit::Role::Button)
        .filter_map(|(_, node)| {
            let bounds = node.bounds()?;
            Some((
                node.label()?.to_owned(),
                Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1),
            ))
        })
        .collect()
}

fn labels(app: &mut HeadlessApp<LockApp>) -> Vec<String> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect()
}

#[test]
fn the_screen_says_the_time_whose_session_it_is_and_what_unlocks_it() {
    let (mut app, _seat, _asked) = locked(screen());
    let labels = labels(&mut app);
    for spoken in ["10:42", "Monday, 27 October", "Ada Lovelace", "Unlock"] {
        assert!(
            labels.iter().any(|label| label == spoken),
            "{spoken} is on the screen: {labels:?}"
        );
    }
}

#[test]
fn the_screen_shows_what_plays_and_a_press_asks_the_player() {
    let (mut app, _seat, asked) = locked(Shown {
        media: Some(song()),
        ..screen()
    });
    let shown = controls(&mut app);
    let names: Vec<&str> = shown.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Show password",
            "Unlock",
            "Previous song",
            "Pause",
            "Next song",
            "On-screen keyboard"
        ],
        "the field, then the transport, then what the device says"
    );
    assert!(
        labels(&mut app)
            .iter()
            .any(|label| label == "Clair de lune"),
        "the song is on the screen"
    );

    app.harness()
        .perform_named("lock-pause", &A11yAction::Activate)
        .unwrap();
    app.settle();
    app.harness()
        .perform_named("lock-next", &A11yAction::Activate)
        .unwrap();
    app.settle();
    assert_eq!(
        asks(&asked),
        [LockEffect::PlayPause, LockEffect::NextTrack],
        "each press is what the screen asks of the session"
    );
}

#[test]
fn a_player_that_cannot_skip_offers_only_what_it_can_do() {
    let (mut app, _seat, _asked) = locked(Shown {
        media: Some(Media {
            playing: false,
            can_toggle: true,
            can_previous: false,
            can_next: false,
            ..song()
        }),
        ..screen()
    });
    let names: Vec<String> = controls(&mut app)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        names,
        ["Show password", "Unlock", "Play", "On-screen keyboard"],
        "what it cannot do is not there"
    );
}

#[test]
fn an_agent_asking_for_the_same_thing_reaches_the_same_door() {
    let (mut app, _seat, asked) = locked(Shown {
        media: Some(song()),
        ..screen()
    });
    app.effect(&LockEffect::PlayPause).unwrap();
    app.settle();
    app.effect(&LockEffect::PreviousTrack).unwrap();
    app.settle();
    assert_eq!(
        asks(&asked),
        [LockEffect::PlayPause, LockEffect::PreviousTrack]
    );
}

#[test]
fn nothing_shows_while_nothing_plays() {
    let (mut app, _seat, _asked) = locked(screen());
    let names: Vec<String> = controls(&mut app)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert!(
        !names
            .iter()
            .any(|name| ["Play", "Pause", "Next song", "Previous song"].contains(&name.as_str())),
        "no transport without a player: {names:?}"
    );
}

/// Where the screen stands its own keyboard on the viewport: along the
/// bottom edge, no wider than a wide sheet, so its keys stay a thumb's reach
/// apart. A test duplicates the placement so that a tap lands where a person
/// sees a key — if the screen moved its keyboard, the taps stop typing.
fn keyboard_rect(theme: &ui::theme::Theme) -> Rect {
    let key_height = f64::from(theme.tokens.metrics.panel_row_height);
    let gap = f64::from(theme.tokens.spacing.s2);
    let height = PasscodeKeyboard::height(key_height, gap);
    let margin = f64::from(Spacing::BASE.s4);
    let bounds = Rect::new(0.0, 0.0, f64::from(VIEWPORT.0), f64::from(VIEWPORT.1));
    let widest = (bounds.width() - margin * 2.0)
        .max(1.0)
        .min(f64::from(theme.tokens.metrics.panel_width_sheet) * 1.25);
    Rect::from_center_size(
        Point::new(bounds.center().x, bounds.y1 - margin - height / 2.0),
        (widest, height),
    )
}

/// The screen's keyboard as a finger on the glass meets it: where each key
/// sits on the layer showing now, and what tapping one leaves the layer at.
struct Touch {
    keyboard: PasscodeKeyboard,
}

impl Touch {
    fn new() -> Self {
        Self {
            keyboard: PasscodeKeyboard::default(),
        }
    }

    /// Tap `key` where the screen draws it.
    fn tap(&mut self, app: &mut HeadlessApp<LockApp>, key: PasscodeKey) {
        let theme = app.harness().theme().clone();
        let (key_height, gap) = (
            f64::from(theme.tokens.metrics.panel_row_height),
            f64::from(theme.tokens.spacing.s2),
        );
        let (_, slot) = self
            .keyboard
            .layout(keyboard_rect(&theme), key_height, gap)
            .into_iter()
            .find(|(candidate, _)| *candidate == key)
            .unwrap_or_else(|| panic!("the keyboard has no {key:?} key"));
        app.harness().click_at(slot.center().x, slot.center().y);
        app.settle();
        self.keyboard.press(key);
    }
}

/// A finger on a point of the screen that is not a control.
fn touch_screen(app: &mut HeadlessApp<LockApp>, at: Point) {
    app.harness().click_at(at.x, at.y);
    app.settle();
}

#[test]
fn a_press_on_the_screen_brings_up_a_keyboard_that_types_the_password() {
    let (mut app, seat, asked) = locked(screen());
    assert!(!seat.shown().keyboard, "no keyboard until one is asked for");

    // A finger on the screen itself, away from the field and the controls.
    touch_screen(&mut app, Point::new(120.0, 200.0));
    assert!(seat.shown().keyboard, "the press brought it up");

    let mut touch = Touch::new();
    for key in ['s', 'e', 's', 'a', 'm', 'e'] {
        touch.tap(&mut app, PasscodeKey::Char(key));
    }
    touch.tap(&mut app, PasscodeKey::Enter);
    assert_eq!(
        typed(&asked),
        ["sesame"],
        "the keys typed the password and Enter asked to unlock"
    );
    assert!(
        !seat.shown().keyboard,
        "the keyboard is down again once what was typed is in the field's hands"
    );
}

#[test]
fn the_shift_and_symbols_keys_reach_the_other_layers() {
    let (mut app, seat, asked) = locked(screen());
    app.effect(&LockEffect::ShowKeyboard).unwrap();
    app.settle();
    assert!(seat.shown().keyboard, "an agent can ask for it too");

    let mut touch = Touch::new();
    touch.tap(&mut app, PasscodeKey::Shift);
    touch.tap(&mut app, PasscodeKey::Char('a'));
    touch.tap(&mut app, PasscodeKey::Symbols);
    touch.tap(&mut app, PasscodeKey::Char('!'));
    touch.tap(&mut app, PasscodeKey::Enter);
    assert_eq!(
        typed(&asked),
        ["A!"],
        "shift capitalises one letter, and symbols type themselves"
    );
}

#[test]
fn a_key_typed_on_a_real_keyboard_puts_the_screens_own_away() {
    let (mut app, seat, _asked) = locked(screen());
    app.effect(&LockEffect::ShowKeyboard).unwrap();
    app.settle();
    assert!(seat.shown().keyboard);
    app.harness().key("x");
    app.settle();
    assert!(
        !seat.shown().keyboard,
        "a keyboard is at hand: the drawn one steps aside"
    );
}

#[test]
fn the_same_screen_is_shown_whatever_the_display() {
    // A lock surface covers one output; its content has to fit the output it
    // lands on, a small one and one turned upright included.
    for size in [(1920, 1080), (1080, 1920), (1024, 600)] {
        let (mut app, _seat, _asked) = locked(screen());
        app.harness().resize(size).unwrap();
        app.settle();
        let labels = labels(&mut app);
        for spoken in ["10:42", "Monday, 27 October", "Ada Lovelace", "Unlock"] {
            assert!(
                labels.iter().any(|label| label == spoken),
                "{size:?} shows {spoken}: {labels:?}"
            );
        }
        let (width, height) = (f64::from(size.0), f64::from(size.1));
        for (label, rect) in controls(&mut app) {
            assert!(
                rect.x0 >= 0.0 && rect.y0 >= 0.0 && rect.x1 <= width && rect.y1 <= height,
                "{label} at {rect:?} is on {size:?}"
            );
        }
    }
}
