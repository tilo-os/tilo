//! Each finger on the keys types on its own: overlapping thumbs type in the
//! order they pressed, and only a finger alone on the keys swipes.

use std::cell::RefCell;
use std::rc::Rc;

use osk::widget::{key_row_height, keyboard_gutter, suggestion_row_height, toolbar_row_height};
use osk::{GeometricSwipeModel, KeyAction, KeyboardLayout, OskWidget, ShiftState, TextMirror};
use ui::headless::FrameHarness;
use ui::tokens::{Metrics, Spacing};
use ui::{State, Widget};
use vello::kurbo::{Point, Rect};
use winit::event::TouchPhase;

const VIEWPORT: (u32, u32) = (720, 360);

struct Keyboard {
    driver: FrameHarness,
    typed: Rc<RefCell<TextMirror>>,
}

impl Keyboard {
    fn new() -> Self {
        Self::with_layout("qwerty", VIEWPORT)
    }

    fn with_layout(layout: &str, viewport: (u32, u32)) -> Self {
        Self::with_widget(
            OskWidget::new()
                .with_swipe_model(Box::new(GeometricSwipeModel::english()))
                .with_active_layout(layout),
            viewport,
        )
    }

    fn with_widget(widget: OskWidget, viewport: (u32, u32)) -> Self {
        let typed = Rc::new(RefCell::new(TextMirror::new(4096)));
        let sink = typed.clone();
        let widget = widget.on_output(move |output| sink.borrow_mut().apply(&output));
        let driver = FrameHarness::for_root(State::new(), widget, viewport, ui::Theme::dark());
        Self { driver, typed }
    }

    fn text(&self) -> String {
        self.typed.borrow().text().to_string()
    }

    fn transition(&mut self, transition: osk::imclient::ImTransition) {
        self.driver
            .root_mut::<OskWidget>()
            .follow_input_method(transition);
        self.driver.recompose().unwrap();
    }

    fn preferred_layout(&self) -> &str {
        self.driver.root::<OskWidget>().preferred_text_layout()
    }

    fn take_preference_edits(&mut self) -> Vec<osk::SetPreference> {
        self.driver.root_mut::<OskWidget>().take_preference_edits()
    }

    fn down(&mut self, finger: u64, at: Point) {
        self.driver.touch(finger, TouchPhase::Started, at.x, at.y);
    }

    fn drag(&mut self, finger: u64, to: Point) {
        self.driver.touch(finger, TouchPhase::Moved, to.x, to.y);
    }

    fn up(&mut self, finger: u64, at: Point) {
        self.driver.touch(finger, TouchPhase::Ended, at.x, at.y);
    }
}

/// Centre of the lower-case letter key `c`, from the same pure geometry the
/// keyboard lays its keys out with.
fn key(c: char) -> Point {
    key_in(&KeyboardLayout::qwerty(), VIEWPORT, c)
}

fn key_in(layout: &KeyboardLayout, viewport: (u32, u32), c: char) -> Point {
    action_in(layout, viewport, &KeyAction::Char(c.to_string()))
}

fn action_in(layout: &KeyboardLayout, viewport: (u32, u32), action: &KeyAction) -> Point {
    let rows = u32::try_from(layout.row_count()).expect("a handful of rows");
    let spacing = Spacing::BASE;
    let metrics = Metrics::BASE;
    let top = f64::from(toolbar_row_height(&spacing) + suggestion_row_height(&metrics));
    let gutter = keyboard_gutter(&spacing);
    let area = Rect::new(
        gutter,
        top,
        f64::from(viewport.0) - gutter,
        top + f64::from(rows) * f64::from(key_row_height(&metrics)),
    );
    osk::key_rects(layout, area, ShiftState::Off)
        .into_iter()
        .find(|(id, _)| layout.key(*id).is_some_and(|k| &k.action == action))
        .map_or_else(|| panic!("no key for {action:?}"), |(_, r)| r.center())
}

#[test]
fn language_key_obeys_configured_sources_and_touch_cancellation() {
    let mut widget = OskWidget::new();
    widget.present_preferences(&osk::OskPreferences {
        layouts: vec!["qwerty".into()],
        ..Default::default()
    });
    let mut keyboard = Keyboard::with_widget(widget, VIEWPORT);
    let at = action_in(
        &KeyboardLayout::qwerty(),
        VIEWPORT,
        &KeyAction::Layout("azerty".into()),
    );
    keyboard.down(1, at);
    keyboard.up(1, at);
    assert_eq!(keyboard.preferred_layout(), "qwerty");
    assert!(keyboard.take_preference_edits().is_empty());
    keyboard.driver.recompose().unwrap();
    keyboard.down(4, at);
    keyboard
        .driver
        .root_mut::<OskWidget>()
        .present_preferences(&osk::OskPreferences {
            layouts: vec!["azerty".into(), "qwerty".into()],
            ..Default::default()
        });
    keyboard.driver.recompose().unwrap();
    keyboard.up(4, at);
    assert_eq!(keyboard.preferred_layout(), "qwerty");
    assert!(keyboard.take_preference_edits().is_empty());
    keyboard.driver.recompose().unwrap();
    keyboard.down(2, at);
    keyboard.driver.touch(2, TouchPhase::Cancelled, at.x, at.y);
    assert_eq!(keyboard.preferred_layout(), "qwerty");
    assert!(keyboard.take_preference_edits().is_empty());
    keyboard.driver.recompose().unwrap();
    keyboard.down(3, at);
    keyboard.up(3, at);
    assert_eq!(keyboard.preferred_layout(), "azerty");
    assert_eq!(
        keyboard.take_preference_edits(),
        [osk::SetPreference::Layout("azerty".into())]
    );
    assert!(keyboard.text().is_empty());
}

/// A dense path through the centres of `word`'s keys.
fn path_through(word: &str) -> Vec<Point> {
    let mut points: Vec<Point> = Vec::new();
    for c in word.chars() {
        let centre = key(c);
        if let Some(prev) = points.last().copied() {
            for k in 1..=5 {
                points.push(prev.lerp(centre, f64::from(k) / 5.0));
            }
        } else {
            points.push(centre);
        }
    }
    points
}

#[test]
fn a_single_tap_types_on_lift_not_on_press() {
    let mut kb = Keyboard::new();
    kb.down(1, key('q'));
    assert_eq!(kb.text(), "", "a key waits for its finger to lift");
    kb.up(1, key('q'));
    assert_eq!(kb.text().to_lowercase(), "q");
}

#[test]
fn overlapping_taps_type_in_press_order() {
    // A down, B down, A up, B up.
    let mut kb = Keyboard::new();
    kb.down(1, key('i'));
    kb.down(2, key('s'));
    kb.up(1, key('i'));
    kb.up(2, key('s'));
    assert_eq!(kb.text().to_lowercase(), "is");

    // A down, B down, B up, A up: still press order.
    let mut kb = Keyboard::new();
    kb.down(1, key('i'));
    kb.down(2, key('s'));
    kb.up(2, key('s'));
    kb.up(1, key('i'));
    assert_eq!(kb.text().to_lowercase(), "is");
}

#[test]
fn fast_alternating_thumbs_drop_no_keys() {
    // Each thumb lands before the other lifts, as fast two-thumb typing
    // does; every key types exactly once, in order.
    let word = "keyboard";
    let mut kb = Keyboard::new();
    let mut held: Option<(u64, Point)> = None;
    for (i, c) in word.chars().enumerate() {
        let finger = (i % 2) as u64 + 1;
        let at = key(c);
        kb.down(finger, at);
        if let Some((f, p)) = held.take() {
            kb.up(f, p);
        }
        held = Some((finger, at));
    }
    if let Some((f, p)) = held {
        kb.up(f, p);
    }
    assert_eq!(kb.text().to_lowercase(), word);
}

#[test]
fn one_finger_alone_on_the_keys_swipes_a_word() {
    let mut kb = Keyboard::new();
    let path = path_through("quick");
    kb.down(1, path[0]);
    for p in &path[1..] {
        kb.drag(1, *p);
    }
    kb.up(1, *path.last().unwrap());
    assert_eq!(kb.text().to_lowercase(), "quick");
}

#[test]
fn a_fast_adjacent_letter_gesture_uses_its_release_endpoint() {
    let mut kb = Keyboard::new();
    kb.down(1, key('a'));
    // A coalesced fast gesture can arrive as down/up without a move event.
    kb.up(1, key('s'));
    assert_eq!(kb.text(), "as");
}

#[test]
fn changing_letters_without_changing_rectangles_cancels_old_contacts() {
    let mut swapped = KeyboardLayout::qwerty();
    swapped.name = "swapped".into();
    swapped.rows[0].swap(2, 3);
    let mut kb = Keyboard::with_widget(OskWidget::new().with_layout(swapped), VIEWPORT);
    let old_geometry = kb.driver.root::<OskWidget>().get_rect();
    kb.down(1, key('e'));
    kb.driver
        .root_mut::<OskWidget>()
        .present_preferences(&osk::OskPreferences {
            layout: "swapped".into(),
            ..osk::OskPreferences::default()
        });
    kb.driver.recompose().unwrap();
    assert_eq!(kb.driver.root::<OskWidget>().get_rect(), old_geometry);
    kb.up(1, key('e'));
    assert_eq!(kb.text(), "");
    kb.down(2, key('e'));
    kb.up(2, key('e'));
    assert_eq!(kb.text(), "r");
}

#[test]
fn a_drag_while_another_finger_is_down_is_a_tap_not_a_swipe() {
    let mut kb = Keyboard::new();
    kb.down(1, key('a'));
    let path = path_through("quick");
    kb.down(2, path[0]);
    for p in &path[1..] {
        kb.drag(2, *p);
    }
    kb.up(2, *path.last().unwrap());
    kb.up(1, key('a'));
    // 'a' typed when the second finger landed; the second finger typed the
    // key it lifted from.
    assert_eq!(kb.text().to_lowercase(), "ak");
}

#[test]
fn a_cancelled_contact_types_nothing() {
    let mut kb = Keyboard::new();
    let q = key('q');
    kb.down(1, q);
    kb.driver.touch(1, TouchPhase::Cancelled, q.x, q.y);
    assert_eq!(kb.text(), "");
}

#[test]
fn a_field_change_cancels_contacts_before_their_release() {
    let mut kb = Keyboard::new();
    kb.down(1, key('q'));
    kb.transition(osk::imclient::ImTransition {
        field_changed: true,
        ..Default::default()
    });
    kb.up(1, key('q'));
    assert_eq!(kb.text(), "", "an old finger must not type in a new field");
    kb.down(2, key('e'));
    kb.up(2, key('e'));
    assert_eq!(kb.text().to_lowercase(), "e", "fresh contacts still type");
}

#[test]
fn rotation_cancels_a_contact_in_the_old_geometry() {
    let mut kb = Keyboard::new();
    kb.down(1, key('q'));
    kb.driver.resize((390, 720)).unwrap();
    kb.up(1, key('q'));
    assert_eq!(kb.text(), "", "rotation must not reinterpret a held key");
}

#[test]
fn hiding_cancels_old_contacts() {
    let mut kb = Keyboard::new();
    kb.down(1, key('e'));
    kb.transition(osk::imclient::ImTransition {
        visibility_changed: Some(false),
        ..Default::default()
    });
    kb.transition(osk::imclient::ImTransition {
        visibility_changed: Some(true),
        ..Default::default()
    });
    kb.up(1, key('e'));
    assert_eq!(kb.text(), "");
}

#[test]
fn scaled_touch_coordinates_type_once_at_the_drawn_key() {
    for scale in [1.0, 1.25, 1.5, 2.0] {
        let mut kb = Keyboard::new();
        kb.driver.set_input_scale(scale).unwrap();
        kb.down(1, key('q'));
        kb.up(1, key('q'));
        assert_eq!(kb.text().to_lowercase(), "q", "scale {scale}");
    }
}

#[test]
fn english_and_french_touch_typing_follow_phone_and_tablet_geometry() {
    for (name, layout, word) in [
        ("qwerty", KeyboardLayout::qwerty(), "keyboard"),
        ("azerty", KeyboardLayout::azerty(), "bonjour"),
    ] {
        for viewport in [(390, 720), (720, 390), (960, 600)] {
            let mut kb = Keyboard::with_layout(name, viewport);
            for (index, ch) in word.chars().enumerate() {
                let at = key_in(&layout, viewport, ch);
                let finger = index as u64;
                kb.down(finger, at);
                kb.up(finger, at);
            }
            assert_eq!(kb.text().to_lowercase(), word, "{name} at {viewport:?}");
        }
    }
}
