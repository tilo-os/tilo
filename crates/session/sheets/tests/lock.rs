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
use tilo_sheets::lock::{Battery, LockApp, LockEffect, LockSeat, Media, Shown, Status};
use ui::a11y::Action as A11yAction;
use ui::framework::testing::HeadlessApp;
use ui::passcode_keyboard::{PasscodeKey, PasscodeKeyboard};
use ui::tokens::Spacing;
use ui::vello::kurbo::{Point, Rect};

const VIEWPORT: (u32, u32) = (1280, 800);

/// A locked session's screen, showing `shown` on the seat the app reports
/// to, and what it asks the session.
///
/// Every reading of it is at rest: the screen the shell puts up arrives
/// growing and fading from nothing, so a frame taken while it comes up says
/// how it moves, not what it says.
fn locked(shown: Shown) -> (HeadlessApp<LockApp>, LockSeat, Receiver<LockEffect>) {
    let _settled = ui::motion::testing::settled();
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
        artwork: String::new(),
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

/// The screen says what the lock is holding back: the notifications that
/// wait behind it, and a battery about to give out on its own.
#[test]
fn the_screen_says_how_many_wait_and_warns_of_a_failing_battery() {
    let mut waiting = screen();
    waiting.status.notifications = 3;
    let (mut app, _seat, _asked) = locked(waiting);
    let said = labels(&mut app);
    assert!(
        said.iter().any(|label| label == "3 notifications"),
        "what waits behind the unlock: {said:?}"
    );

    let mut failing = screen();
    failing.status.battery = Some(Battery {
        percent: Some(3),
        charging: false,
    });
    let (mut app, _seat, _asked) = locked(failing);
    let said = labels(&mut app);
    assert!(
        said.iter().any(|label| label.starts_with("Battery at 3%")),
        "a battery about to give out says so: {said:?}"
    );

    // Charging at the same reading is nothing to warn about: the charger is
    // the answer already.
    let mut charging = screen();
    charging.status.battery = Some(Battery {
        percent: Some(3),
        charging: true,
    });
    let (mut app, _seat, _asked) = locked(charging);
    assert!(
        !labels(&mut app)
            .iter()
            .any(|label| label.starts_with("Battery at 3%")),
        "a charging battery is not failing"
    );
}

/// Where the screen puts the node labelled `label`.
fn place(app: &mut HeadlessApp<LockApp>, label: &str) -> Rect {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find_map(|(_, node)| {
            let bounds = node.bounds()?;
            (node.label() == Some(label))
                .then(|| Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1))
        })
        .unwrap_or_else(|| panic!("no {label} on the screen"))
}

/// The clock's type, as the sheet asks for it: its size, its weight, lining
/// figures of one width, and no leading above the line.
fn clock_style() -> ui::text_render::TextStyle {
    use ui::text_render::{NumericStyle, OpenTypeTag, TextScaling};

    let theme = ui::theme::Theme::dark();
    let mut style = theme.tokens.typography.style(ui::tokens::TextRole::Body);
    style.size = tilo_sheets::lock::CLOCK_SIZE;
    style.scaling = TextScaling::Content;
    style
        .with_font_variation(OpenTypeTag::WEIGHT, tilo_sheets::lock::CLOCK_WEIGHT)
        .with_numeric_style(NumericStyle::TabularLining)
        .with_line_height(1.0)
}

#[test]
fn the_clock_sets_each_character_one_figure_advance_from_the_last() {
    let (mut app, _seat, _asked) = locked(screen());
    let clock = place(&mut app, "10:42");
    let frame = app.harness().frame_output();
    let scale = f64::from(frame.device_size.0) / f64::from(frame.logical_size.0);
    let encoding = frame.scene.encoding();
    let run = encoding
        .resources
        .glyph_runs
        .iter()
        .find(|run| {
            (f64::from(run.font_size) - f64::from(tilo_sheets::lock::CLOCK_SIZE) * scale).abs()
                < 0.5
        })
        .expect("the clock is drawn at its own size");
    let xs: Vec<f64> = encoding.resources.glyphs[run.glyphs.clone()]
        .iter()
        .map(|glyph| f64::from(glyph.x))
        .collect();
    assert_eq!(
        xs.len(),
        "10:42".chars().count(),
        "the time is one run of its characters"
    );
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a display scale is small and exact enough in f32"
    )]
    let text_scale = scale as f32;
    let theme = ui::Theme::light().with_text_scale(text_scale);
    let figure = f64::from(ui::text_render::with_layout_renderer(&theme, |text| {
        text.measure_style("0", None, &clock_style()).0
    }));
    for pair in xs.windows(2) {
        let step = pair[1] - pair[0];
        assert!(
            step <= figure + 0.5,
            "characters {step} px apart, wider than the font's {figure} px figure advance: {xs:?}"
        );
    }
    let span = xs[xs.len() - 1] - xs[0] + figure;
    assert!(
        span <= clock.width().mul_add(scale, 1.0),
        "the clock paints {span} px wide in a room {} px wide",
        clock.width() * scale
    );
    assert!(
        (xs[1] - xs[0] - (xs[4] - xs[3])).abs() < 0.05,
        "each digit keeps one advance, whichever digit it is: {xs:?}"
    );
    assert!(
        xs[3] - xs[2] < xs[1] - xs[0],
        "the colon is narrower than a figure: {xs:?}"
    );
}

/// A locked session's screen whose footer has `link` for its connection.
fn with_link(link: Option<ui::widget::LinkStatus>) -> Shown {
    Shown {
        status: Status {
            link,
            ..screen().status
        },
        ..screen()
    }
}

/// The frame the screen paints for `shown`: what two runs compare.
fn painted(shown: Shown) -> ui::vello::Scene {
    let (mut app, _seat, _asked) = locked(shown);
    app.harness().frame_output().scene.clone()
}

/// A connection of `kind` called `name`.
fn connection(kind: ui::widget::LinkKind, name: Option<&str>) -> ui::widget::LinkStatus {
    ui::widget::LinkStatus {
        kind,
        name: name.map(str::to_owned),
        signal: 4,
    }
}

/// Whether two painted screens draw the same shapes and letters, both at
/// rest: the sheet arrives growing and fading, so only a settled screen draws
/// the same shapes twice, and the ink alone is what a footer's glyph changes.
fn same_shapes(left: &ui::vello::Scene, right: &ui::vello::Scene) -> bool {
    let (a, b) = (left.encoding(), right.encoding());
    a.path_tags == b.path_tags
        && a.path_data == b.path_data
        && a.resources.glyphs.len() == b.resources.glyphs.len()
        && a.resources
            .glyphs
            .iter()
            .zip(&b.resources.glyphs)
            .all(|(left, right)| {
                left.x.to_bits() == right.x.to_bits()
                    && left.y.to_bits() == right.y.to_bits()
                    && left.id == right.id
            })
}

/// The footer draws the glyph of the connection the session read: a cable's
/// port on a cable, Wi-Fi's arcs on Wi-Fi, crossed-out arcs with nothing
/// connected — not Wi-Fi's arcs whatever the machine is on.
#[test]
fn the_footer_draws_the_glyph_of_the_connection_the_session_read() {
    use ui::widget::LinkKind;

    let cable = painted(with_link(Some(connection(LinkKind::Wired, Some("Home")))));
    let radio = painted(with_link(Some(connection(LinkKind::Wifi, Some("Home")))));
    let unplugged = painted(with_link(Some(connection(LinkKind::Offline, Some("Home")))));
    assert!(
        same_shapes(
            &cable,
            &painted(with_link(Some(connection(LinkKind::Wired, Some("Home")))))
        ),
        "the same connection paints the same screen"
    );
    assert!(!same_shapes(&cable, &radio), "a cable does not draw Wi-Fi");
    assert!(
        !same_shapes(&radio, &unplugged),
        "Wi-Fi is not the offline glyph"
    );
    assert!(
        !same_shapes(&cable, &unplugged),
        "a cable is not the offline glyph"
    );
    assert!(
        !same_shapes(&cable, &painted(with_link(None))),
        "a machine whose link the session did not read draws no glyph at all"
    );
}

/// The words beside the glyph are the connection's own name, and a connection
/// with no name is a glyph alone.
#[test]
fn the_footer_says_what_the_connection_is_called_when_it_has_a_name() {
    use ui::widget::LinkKind;

    let (mut app, _seat, _asked) = locked(with_link(Some(connection(
        LinkKind::Wifi,
        Some("Café WiFi"),
    ))));
    let names = labels(&mut app);
    assert!(
        names.iter().any(|name| name == "Café WiFi"),
        "the connection is named beside its glyph: {names:?}"
    );

    let called = painted(with_link(Some(connection(LinkKind::Wired, Some("Home")))));
    let unnamed = painted(with_link(Some(connection(LinkKind::Wired, None))));
    assert!(
        !same_shapes(&called, &unnamed),
        "a named connection writes its name where an unnamed one stops at the glyph"
    );
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
        .perform_named("lock-play", &A11yAction::Activate)
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
fn keyboard_rect(theme: &ui::theme::Theme, keyboard: PasscodeKeyboard) -> Rect {
    let key_height = f64::from(theme.tokens.metrics.panel_row_height);
    let gap = f64::from(theme.tokens.spacing.s2);
    let height = keyboard.height(key_height, gap);
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
            .layout(keyboard_rect(&theme, self.keyboard), key_height, gap)
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

/// A control the screen publishes by name, where it sits and whether it is
/// offering to do anything.
fn placed(app: &mut HeadlessApp<LockApp>, name: &str) -> (Rect, bool) {
    let nodes = app.harness().a11y_nodes();
    let Some((_, node)) = nodes
        .into_iter()
        .find(|(_, node)| node.label() == Some(name))
    else {
        panic!("the screen publishes {name}: {:?}", labels(app));
    };
    let bounds = node.bounds().expect("it is placed");
    (
        Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1),
        node.is_disabled(),
    )
}

/// How many times the screen has asked the session to unlock since the last
/// look.
fn unlocks(asked: &Receiver<LockEffect>) -> usize {
    asked
        .try_iter()
        .filter(|effect| *effect == LockEffect::Unlock)
        .count()
}

/// What has the keyboard, named as the screen reader announces it.
fn holds_keyboard(app: &mut HeadlessApp<LockApp>) -> Option<String> {
    app.harness()
        .focused_access_node()
        .and_then(|node| node.name)
}

#[test]
fn the_field_holds_the_keyboard_as_the_screen_comes_up() {
    let (mut app, _seat, _asked) = locked(screen());
    assert_eq!(
        holds_keyboard(&mut app).as_deref(),
        Some("Password"),
        "a person who is unlocking types; nothing else has to be found first"
    );
}

/// The screen reads in the language the person chose: the field asks for the
/// password in German, and in nothing of the source language.
#[test]
fn the_screen_reads_in_the_persons_language() {
    settings_store::appearance::LANGUAGE
        .set(Some("de-DE".to_owned()))
        .expect("the person's language is saved");
    let (mut app, _seat, _asked) = locked(screen());
    let said = labels(&mut app);
    let field = holds_keyboard(&mut app);
    settings_store::appearance::LANGUAGE
        .set(None)
        .expect("the language is the system's again");
    assert_eq!(
        field.as_deref(),
        Some("Passwort"),
        "the field asks for the password in German: {said:?}"
    );
    for english in ["Password", "Show password", "Unlock", "On-screen keyboard"] {
        assert!(
            !said.iter().any(|label| label == english),
            "nothing on the screen says {english:?}: {said:?}"
        );
    }
}

#[test]
fn enter_unlocks_and_so_does_the_arrow_inside_the_field() {
    let (mut app, seat, asked) = locked(screen());
    app.harness().type_text("sesame");
    app.settle();
    app.harness().key("Enter");
    app.settle();
    assert_eq!(unlocks(&asked), 1, "the Enter key is a submit, not a hint");

    // The session answers: the field is the person's again.
    seat.show(Shown {
        checking: false,
        ..seat.shown()
    });
    app.settle();
    app.harness().type_text("sesame");
    app.settle();
    let (arrow, disabled) = placed(&mut app, "Unlock");
    assert!(
        !disabled,
        "a field with something in it has something to hand over"
    );
    app.harness().click_at(arrow.center().x, arrow.center().y);
    app.settle();
    assert_eq!(
        unlocks(&asked),
        1,
        "the arrow inside the field does what Enter does"
    );
}

#[test]
fn an_empty_field_has_nothing_to_hand_over() {
    let (mut app, _seat, asked) = locked(screen());
    let (arrow, disabled) = placed(&mut app, "Unlock");
    assert!(disabled, "nothing typed is nothing to unlock with");
    app.harness().click_at(arrow.center().x, arrow.center().y);
    app.settle();
    app.harness().key("Enter");
    app.settle();
    assert_eq!(
        unlocks(&asked),
        0,
        "an empty field asks the session nothing, however it is asked"
    );
    assert_eq!(
        holds_keyboard(&mut app).as_deref(),
        Some("Password"),
        "and the field keeps the keyboard"
    );
}

#[test]
fn a_refused_password_says_why_below_the_field() {
    let reason = "That password did not unlock. Try again.";
    let (mut app, _seat, asked) = locked(Shown {
        error: Some(reason.to_owned()),
        ..screen()
    });
    let (field, _) = placed(&mut app, "Password");
    let (said, _) = placed(&mut app, reason);
    assert!(
        said.y0 >= field.y1,
        "the reason sits under the field it is about: {said:?} against {field:?}"
    );
    assert!(
        holds_keyboard(&mut app).is_some(),
        "the keyboard stays usable"
    );
    app.harness().type_text("sesame");
    app.settle();
    app.harness().key("Enter");
    app.settle();
    assert_eq!(unlocks(&asked), 1, "a refusal is not the end of the try");
}

#[test]
fn the_field_stays_put_whatever_the_line_under_it_says() {
    let (mut app, seat, _asked) = locked(screen());
    let (quiet, _) = placed(&mut app, "Password");
    let reason = "That password did not unlock. Try again.";
    for said in [
        Shown {
            caps_lock: true,
            ..screen()
        },
        Shown {
            checking: true,
            ..screen()
        },
        Shown {
            error: Some(reason.to_owned()),
            ..screen()
        },
        Shown {
            caps_lock: true,
            error: Some(reason.to_owned()),
            ..screen()
        },
    ] {
        seat.show(said);
        app.settle();
        let (field, _) = placed(&mut app, "Password");
        assert!(
            (field.y0 - quiet.y0).abs() < 1.0 && (field.y1 - quiet.y1).abs() < 1.0,
            "the field waits where it was while the line under it changes: {field:?} against {quiet:?}"
        );
    }
}

#[test]
fn nothing_tells_a_person_what_the_keyboard_already_does() {
    let (mut app, _seat, _asked) = locked(screen());
    let spoken: Vec<String> = labels(&mut app)
        .into_iter()
        .map(|label| label.to_lowercase())
        .collect();
    for filler in ["press enter", "enter to unlock", "type your password"] {
        assert!(
            !spoken.iter().any(|label| label.contains(filler)),
            "no instruction where the field is: {filler} in {spoken:?}"
        );
    }
}

#[test]
#[ignore = "timing: the numbers are the report, run by hand"]
fn the_unlock_dissolve_paints_its_frames_within_a_frame() {
    use ui::framework::testing::perf;
    let (mut app, seat, _asked) = locked(screen());
    seat.show(Shown {
        unlocking: true,
        ..seat.shown()
    });
    let report = perf::measure(&mut app, "lock-unlock", (1, 1), |app, _| {
        for _ in 0..24 {
            let _painted = app.frame();
        }
    });
    report.print();
}

#[test]
fn the_field_is_a_prompt_wide_line_with_the_arrow_at_its_right_end() {
    let (mut app, _seat, _asked) = locked(screen());
    let (field, _) = placed(&mut app, "Password");
    let (arrow, _) = placed(&mut app, "Unlock");
    let (eye, _) = placed(&mut app, "Show password");
    assert!(
        (field.width() - 420.0).abs() < 1.0,
        "the prompt is one readable line, not a sliver: {field:?}"
    );
    assert!(
        (field.center().x - f64::from(VIEWPORT.0) / 2.0).abs() < 1.0,
        "and it is centred under the clock: {field:?}"
    );
    assert!(
        (arrow.x1 - field.x1).abs() < 1.0 && arrow.x0 >= field.x0,
        "the arrow ends the field: {arrow:?} in {field:?}"
    );
    assert!(
        eye.x1 <= arrow.x0,
        "the show/hide control steps inboard of the arrow rather than under it: {eye:?} beside {arrow:?}"
    );
    assert!(
        eye.x0 >= field.x0,
        "and it stays inside the field it belongs to: {eye:?} in {field:?}"
    );
}
