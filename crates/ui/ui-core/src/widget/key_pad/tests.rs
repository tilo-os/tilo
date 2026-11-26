//! The on-screen keys as a person on the glass uses them: a tap types on lift, two thumbs type in
//! press order, a long press offers a letter's forms, a held Backspace repeats, a swipe is reported
//! with the shape of the keys it crossed, and the space bar steers the caret.

use crate::time::{Duration, now};
use std::cell::RefCell;
use std::rc::Rc;

use vello::kurbo::{Point, Rect};
use winit::event::TouchPhase;

use super::{LONG_PRESS, PadKey, PadSize, PadTone};

use tilo_ui_base::a11y::Role;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::Widget;

/// What a keypad reported: presses by key with where the finger was, holds with the form chosen,
/// swipe paths, and caret steps.
#[derive(Clone, Debug, Default, PartialEq)]
struct Told {
    pressed: Vec<(i64, (f64, f64))>,
    held: Vec<(i64, String)>,
    swipes: Vec<(Vec<f64>, f64)>,
    caret: Vec<i64>,
}

fn pad_of(rows: &[&[(&str, f64)]], rect: Rect) -> (super::KeyPad, Rc<RefCell<Told>>) {
    let mut keys = Vec::new();
    for (row, keys_row) in rows.iter().enumerate() {
        for (column, (label, width)) in keys_row.iter().enumerate() {
            keys.push(PadKey {
                label: (*label).to_owned(),
                name: (*label).to_owned(),
                width: *width,
                row: i64::try_from(row).unwrap_or(0),
                column: i64::try_from(column).unwrap_or(0),
                caret: *label == "space",
                ..PadKey::default()
            });
        }
    }
    pad_with(keys, rect)
}

fn pad_with(mut keys: Vec<PadKey>, rect: Rect) -> (super::KeyPad, Rc<RefCell<Told>>) {
    keys.shrink_to_fit();
    let told = Rc::new(RefCell::new(Told::default()));
    let mut pad = super::KeyPad::default();
    pad.set_keys(keys);
    let seen = Rc::clone(&told);
    pad.on_press = Some(Box::new(move |index, point| {
        seen.borrow_mut().pressed.push((index, point));
    }));
    let seen = Rc::clone(&told);
    pad.on_hold = Some(Box::new(move |index, form| {
        seen.borrow_mut().held.push((index, form));
    }));
    let seen = Rc::clone(&told);
    pad.on_swipe = Some(Box::new(move |path, aspect| {
        seen.borrow_mut().swipes.push((path, aspect));
    }));
    let seen = Rc::clone(&told);
    pad.on_caret = Some(Box::new(move |steps| {
        seen.borrow_mut().caret.push(steps);
    }));
    pad.rect = rect;
    pad.place(&Theme::dark());
    (pad, told)
}

fn letters_rows() -> Vec<Vec<(&'static str, f64)>> {
    vec![
        "qwertyuiop"
            .chars()
            .map(|c| {
                (
                    Box::leak(c.to_string().into_boxed_str()) as &'static str,
                    1.0,
                )
            })
            .collect(),
        "asdfghjkl"
            .chars()
            .map(|c| {
                (
                    Box::leak(c.to_string().into_boxed_str()) as &'static str,
                    1.0,
                )
            })
            .collect(),
        "zxcvbnm"
            .chars()
            .map(|c| {
                (
                    Box::leak(c.to_string().into_boxed_str()) as &'static str,
                    1.0,
                )
            })
            .collect(),
        vec![("space", 5.0), ("Return", 1.5)],
    ]
}

fn letter_pad() -> (super::KeyPad, Rc<RefCell<Told>>) {
    let rows = letters_rows();
    let rows: Vec<Vec<(&str, f64)>> = rows
        .iter()
        .map(|row| row.iter().map(|(label, width)| (*label, *width)).collect())
        .collect();
    let borrowed: Vec<&[(&str, f64)]> = rows.iter().map(Vec::as_slice).collect();
    pad_of(&borrowed, Rect::new(0.0, 0.0, 400.0, 240.0))
}

fn centre(pad: &super::KeyPad, index: usize) -> Point {
    pad.key_geometry()
        .get(index)
        .map(|rect| rect.center())
        .expect("the key stands")
}

#[test]
fn the_rows_dock_at_the_bottom_by_the_theme_row_height_and_fill_the_width() {
    let (pad, _) = letter_pad();
    assert_eq!(pad.key_geometry().len(), 28, "every key stands somewhere");
    let row_height = pad.row_height;
    assert!(row_height > 0.0);
    // Four rows of the theme's height dock at the bottom of the box, not stretched over it.
    let top = pad.key_geometry()[0].y0;
    let bottom = pad
        .key_geometry()
        .iter()
        .map(|rect| rect.y1)
        .fold(f64::MIN, f64::max);
    assert!((bottom - 240.0).abs() < 0.001, "the keys stand on the foot");
    assert!(
        (240.0 - top - 4.0 * row_height).abs() < 0.001,
        "four rows of the theme's height: {top}"
    );
    // The top row is ten keys across the whole width.
    let q = pad.key_geometry()[0];
    let p = pad.key_geometry()[9];
    assert!((q.x0 - 0.0).abs() < 0.001 && (p.x1 - 400.0).abs() < 0.001);
    assert!((q.width() - 40.0).abs() < 0.001, "400 across ten keys");
    // A size the person chose scales the row height, not the width.
    let (mut compact, _) = letter_pad();
    compact.size = PadSize::Compact;
    compact.place(&Theme::dark());
    assert!(compact.row_height < row_height);
}

#[test]
fn a_tap_types_on_lift_and_a_slide_off_types_nothing() {
    let (mut pad, told) = letter_pad();
    let q = centre(&pad, 0);
    assert!(pad.finger(1, TouchPhase::Started, q));
    assert!(told.borrow().pressed.is_empty(), "a key waits for its lift");
    assert!(pad.finger(1, TouchPhase::Ended, q));
    assert_eq!(told.borrow().pressed.len(), 1);
    assert_eq!(told.borrow().pressed[0].0, 0, "the first key");
    // A finger that slid off the key and lifted types nothing.
    let (mut pad, told) = letter_pad();
    let q = centre(&pad, 0);
    pad.finger(1, TouchPhase::Started, q);
    pad.finger(1, TouchPhase::Moved, Point::new(200.0, 10.0));
    pad.finger(1, TouchPhase::Ended, Point::new(200.0, 10.0));
    assert!(
        told.borrow().pressed.is_empty(),
        "a finger that slid off types nothing"
    );
}

#[test]
fn two_thumbs_type_in_press_order_and_drop_no_key() {
    let (mut pad, told) = letter_pad();
    // A down, B down, A up, B up: the held key types as the second lands.
    pad.finger(1, TouchPhase::Started, centre(&pad, 0));
    pad.finger(2, TouchPhase::Started, centre(&pad, 1));
    pad.finger(1, TouchPhase::Ended, centre(&pad, 0));
    pad.finger(2, TouchPhase::Ended, centre(&pad, 1));
    let pressed: Vec<i64> = told.borrow().pressed.iter().map(|(at, _)| *at).collect();
    assert_eq!(pressed, [0, 1], "press order, each key once");
    // B up before A up: still press order.
    let (mut pad, told) = letter_pad();
    pad.finger(1, TouchPhase::Started, centre(&pad, 0));
    pad.finger(2, TouchPhase::Started, centre(&pad, 2));
    pad.finger(2, TouchPhase::Ended, centre(&pad, 2));
    pad.finger(1, TouchPhase::Ended, centre(&pad, 0));
    let pressed: Vec<i64> = told.borrow().pressed.iter().map(|(at, _)| *at).collect();
    assert_eq!(pressed, [0, 2]);
}

#[test]
fn a_long_press_offers_the_forms_and_the_slide_picks_one() {
    // Four keys across the pad, so the forms stand over the first with room beside it.
    let mut keys = Vec::new();
    for (column, label) in ["e", "r", "t", "y"].into_iter().enumerate() {
        keys.push(PadKey {
            label: label.to_owned(),
            name: label.to_owned(),
            row: 0,
            column: i64::try_from(column).unwrap_or(0),
            holds: if label == "e" {
                vec!["e".to_owned(), "é".to_owned(), "è".to_owned()]
            } else {
                Vec::new()
            },
            ..PadKey::default()
        });
    }
    let (mut pad, told) = pad_with(keys, Rect::new(0.0, 0.0, 200.0, 120.0));
    let at = centre(&pad, 0);
    pad.finger(1, TouchPhase::Started, at);
    let due = now() + LONG_PRESS + Duration::from_millis(10);
    assert!(
        pad.serve_deadline(due),
        "the forms open when the hold is due"
    );
    assert!(pad.alternates.is_some(), "the forms stand over the key");
    // Sliding one key right picks the second form.
    pad.finger(1, TouchPhase::Moved, Point::new(at.x + 50.0, at.y));
    pad.finger(1, TouchPhase::Ended, Point::new(at.x + 50.0, at.y));
    assert_eq!(
        told.borrow().held,
        [(0, "é".to_owned())],
        "the form the finger chose"
    );
    assert!(told.borrow().pressed.is_empty(), "a held key typed no tap");
    // Released without sliding, the plain key is the one chosen.
    let mut keys = Vec::new();
    for (column, label) in ["e", "r", "t", "y"].into_iter().enumerate() {
        keys.push(PadKey {
            label: label.to_owned(),
            name: label.to_owned(),
            row: 0,
            column: i64::try_from(column).unwrap_or(0),
            holds: if label == "e" {
                vec!["e".to_owned(), "é".to_owned()]
            } else {
                Vec::new()
            },
            ..PadKey::default()
        });
    }
    let (mut pad, told) = pad_with(keys, Rect::new(0.0, 0.0, 200.0, 120.0));
    let at = centre(&pad, 0);
    pad.finger(1, TouchPhase::Started, at);
    pad.serve_deadline(now() + LONG_PRESS + Duration::from_millis(10));
    pad.finger(1, TouchPhase::Ended, at);
    assert_eq!(told.borrow().held, [(0, "e".to_owned())]);
}

#[test]
fn the_backspace_repeats_after_the_delay() {
    let keys = vec![PadKey {
        label: "Backspace".to_owned(),
        name: "Backspace".to_owned(),
        icon: "delete".to_owned(),
        tone: PadTone::Modifier,
        repeat: true,
        ..PadKey::default()
    }];
    let (mut pad, told) = pad_with(keys, Rect::new(0.0, 0.0, 200.0, 120.0));
    let at = centre(&pad, 0);
    pad.finger(1, TouchPhase::Started, at);
    assert!(told.borrow().pressed.is_empty(), "a repeat waits its delay");
    pad.serve_deadline(now() + Duration::from_millis(500));
    assert_eq!(told.borrow().pressed.len(), 1, "it repeats once due");
    pad.serve_deadline(now() + Duration::from_millis(600));
    assert!(told.borrow().pressed.len() >= 2, "and again as it is held");
    pad.finger(1, TouchPhase::Ended, at);
    assert!(
        told.borrow().pressed.len() < 4,
        "lifting ends the repeat: {:?}",
        told.borrow().pressed.len()
    );
}

#[test]
fn a_swipe_over_the_keys_is_reported_with_the_shape_of_its_box() {
    let (mut pad, told) = letter_pad();
    let q = centre(&pad, 0);
    pad.finger(1, TouchPhase::Started, q);
    for at in 1..=8 {
        let key = pad.key_geometry()[at].center();
        pad.finger(1, TouchPhase::Moved, key);
    }
    let last = pad.key_geometry()[8].center();
    pad.finger(1, TouchPhase::Ended, last);
    let told = told.borrow();
    assert_eq!(told.swipes.len(), 1, "the finger alone on the keys swiped");
    let (path, aspect) = &told.swipes[0];
    assert!(path.len() >= 4, "the path is reported: {path:?}");
    assert!(
        path.iter().all(|value| (0.0..=1.0).contains(value)),
        "the path is fractions of the box: {path:?}"
    );
    assert!((aspect - 400.0 / 240.0).abs() < 0.001, "the box's shape");
    assert!(told.pressed.is_empty(), "a swipe types no single key");
    // A drag while another finger is down is a tap, not a swipe.
    let (mut pad, told) = letter_pad();
    pad.finger(1, TouchPhase::Started, centre(&pad, 0));
    pad.finger(2, TouchPhase::Started, centre(&pad, 1));
    for at in 2..=6 {
        pad.finger(1, TouchPhase::Moved, pad.key_geometry()[at].center());
    }
    pad.finger(1, TouchPhase::Ended, pad.key_geometry()[6].center());
    assert!(told.borrow().swipes.is_empty(), "two thumbs never swipe");
}

#[test]
fn dragging_the_space_bar_steps_the_caret() {
    let (mut pad, told) = letter_pad();
    let space = pad
        .keys()
        .iter()
        .position(|key| key.label == "space")
        .expect("the space key");
    let at = centre(&pad, space);
    pad.finger(1, TouchPhase::Started, at);
    let scale = pad.key_scale();
    // A drag of more than half a key starts steering; a quarter of a key after that is one step.
    pad.finger(1, TouchPhase::Moved, Point::new(at.x + scale * 0.6, at.y));
    assert!(
        told.borrow().caret.is_empty(),
        "steering starts at the drag"
    );
    pad.finger(1, TouchPhase::Moved, Point::new(at.x + scale * 1.1, at.y));
    assert_eq!(
        told.borrow().caret,
        [2],
        "two quarters of a key to the right"
    );
    pad.finger(1, TouchPhase::Moved, Point::new(at.x - scale * 1.1, at.y));
    assert_eq!(told.borrow().caret, [2, -8], "leftwards is negative");
    pad.finger(1, TouchPhase::Ended, Point::new(at.x - scale * 1.1, at.y));
    assert!(
        told.borrow().pressed.is_empty(),
        "steering the caret typed no space"
    );
}

#[test]
fn a_secret_pad_offers_no_forms_and_no_trail() {
    let keys = vec![PadKey {
        label: "e".to_owned(),
        name: "e".to_owned(),
        holds: vec!["e".to_owned(), "é".to_owned()],
        ..PadKey::default()
    }];
    let (mut pad, _) = pad_with(keys, Rect::new(0.0, 0.0, 200.0, 120.0));
    pad.secret = true;
    let at = centre(&pad, 0);
    pad.finger(1, TouchPhase::Started, at);
    pad.serve_deadline(now() + LONG_PRESS + Duration::from_millis(10));
    assert!(pad.alternates.is_none(), "a secret offers no forms");
    assert!(pad.preview().is_none(), "a secret shows no preview");
    pad.finger(1, TouchPhase::Moved, Point::new(at.x + 60.0, at.y));
    assert!(pad.trail().is_empty(), "a secret draws no trail");
}

#[test]
fn assistive_technology_reads_every_key_and_activating_one_types_it() {
    let (mut pad, told) = letter_pad();
    let tree = pad.accessibility().expect("the pad reads");
    assert_eq!(tree.role, Role::Group);
    let names: Vec<String> = tree
        .children
        .iter()
        .map(|child| child.name.clone().unwrap_or_default())
        .collect();
    assert_eq!(names.len(), pad.keys().len());
    assert!(
        names.contains(&"q".to_owned()),
        "each key is named: {names:?}"
    );
    assert!(
        tree.children.iter().all(|child| child
            .action_target
            .as_deref()
            .is_some_and(|target| target.starts_with("keypad:key:"))),
        "each is pressed where it stands"
    );
    pad.activate(5);
    assert_eq!(told.borrow().pressed.len(), 1);
    assert_eq!(told.borrow().pressed[0].0, 5);
}

#[test]
fn a_changed_geometry_cancels_a_contact_before_its_lift() {
    let (mut pad, told) = letter_pad();
    let q = centre(&pad, 0);
    pad.finger(1, TouchPhase::Started, q);
    // The keys change under the finger: its release must type nothing.
    let mut keys = pad.keys().to_vec();
    keys[0].label = "Q".to_owned();
    pad.set_keys(keys);
    pad.finger(1, TouchPhase::Ended, q);
    assert!(
        told.borrow().pressed.is_empty(),
        "a contact in geometry that moved types nothing"
    );
}
