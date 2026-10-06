//! Each finger on the keys types on its own: overlapping thumbs type in the
//! order they pressed, and only a finger alone on the keys swipes.

use std::cell::RefCell;
use std::rc::Rc;

use osk::widget::{key_row_height, keyboard_gutter, suggestion_row_height, toolbar_row_height};
use osk::{GeometricSwipeModel, KeyAction, KeyboardLayout, OskWidget, ShiftState, TextMirror};
use ui::State;
use ui::headless::Driver;
use ui::tokens::{Metrics, Spacing};
use vello::kurbo::{Point, Rect};
use winit::event::TouchPhase;

const VIEWPORT: (u32, u32) = (720, 360);

struct Keyboard {
    driver: Driver,
    typed: Rc<RefCell<TextMirror>>,
}

impl Keyboard {
    fn new() -> Self {
        let typed = Rc::new(RefCell::new(TextMirror::new(4096)));
        let sink = typed.clone();
        let widget = OskWidget::new()
            .with_swipe_model(Box::new(GeometricSwipeModel::english()))
            .on_output(move |output| sink.borrow_mut().apply(&output));
        let driver = Driver::new(State::new(), Box::new(widget), VIEWPORT);
        Self { driver, typed }
    }

    fn text(&self) -> String {
        self.typed.borrow().text().to_string()
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
    let layout = KeyboardLayout::qwerty();
    let rows = u32::try_from(layout.row_count()).expect("a handful of rows");
    let spacing = Spacing::BASE;
    let metrics = Metrics::BASE;
    let top = f64::from(toolbar_row_height(&spacing) + suggestion_row_height(&metrics));
    let gutter = keyboard_gutter(&spacing);
    let area = Rect::new(
        gutter,
        top,
        f64::from(VIEWPORT.0) - gutter,
        top + f64::from(rows) * f64::from(key_row_height(&metrics)),
    );
    osk::key_rects(&layout, area, ShiftState::Off)
        .into_iter()
        .find(|(id, _)| {
            layout
                .key(*id)
                .is_some_and(|k| k.action == KeyAction::Char(c.to_string()))
        })
        .map_or_else(|| panic!("no key for {c}"), |(_, r)| r.center())
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
