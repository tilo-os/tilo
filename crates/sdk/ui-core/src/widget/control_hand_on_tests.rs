//! Controls in a view that builds them anew on every change, as most apps
//! do: the switch, the box, the radio, the slider and the badge built in
//! the place of the one a rebuild dropped go on from its motion
//! (`motion::keep`), so a click still moves them and a key still lands.

use std::cell::RefCell;
use std::rc::Rc;

use crate::State;
use crate::a11y::Action;
use crate::headless::FrameHarness;
use crate::theme::Theme;
use crate::tokens::MotionSource;
use crate::widget::{Badge, Checkbox, Column, Slider, Toggle, Widget};

#[derive(Clone, Copy)]
struct Model {
    on: bool,
    remember: bool,
    nearby: bool,
    level: f64,
    unread: u32,
}

const SWITCH: usize = 0;
const BOX: usize = 1;
const RADIO: usize = 2;
const SLIDER: usize = 3;
const BADGE: usize = 4;

/// The view: every control built from the model, writing back to it.
fn view(model: &Rc<RefCell<Model>>) -> Vec<Box<dyn Widget>> {
    let m = *model.borrow();
    let set = |write: fn(&mut Model, bool)| {
        let model = Rc::clone(model);
        move |value| write(&mut model.borrow_mut(), value)
    };
    let level = Rc::clone(model);
    vec![
        Box::new(
            Toggle::from_state(&crate::LocalState::new(m.on))
                .on_change(set(|m, value| m.on = value)),
        ),
        Box::new(
            Checkbox::from_state(&crate::LocalState::new(m.remember))
                .with_label("Remember me")
                .on_change(set(|m, value| m.remember = value)),
        ),
        Box::new(
            Checkbox::from_state(&crate::LocalState::new(m.nearby))
                .radio()
                .with_label("Nearby only")
                .on_change(set(|m, value| m.nearby = value)),
        ),
        Box::new(
            Slider::from_state(&crate::LocalState::new(m.level))
                .on_change(move |value| level.borrow_mut().level = value),
        ),
        Box::new(Badge::count(m.unread)),
    ]
}

fn rebuild(d: &mut FrameHarness, model: &Rc<RefCell<Model>>) {
    let children = view(model);
    d.root_mut::<Column>().set_children(children);
    d.relayout();
}

fn moving(d: &FrameHarness, index: usize) -> bool {
    d.root::<Column>().children()[index].needs_animation_frame()
}

fn rect(d: &FrameHarness, index: usize) -> vello::kurbo::Rect {
    d.root::<Column>().children()[index].get_rect()
}

/// Act on control `index` as a key or an assistive tool does.
fn act(d: &mut FrameHarness, index: usize, action: &Action, source: MotionSource) {
    let mut doc = automerge::AutoCommit::new();
    crate::motion::note_source(source);
    let column = d.root_mut::<Column>();
    let control = &mut column.children_mut()[index];
    assert!(Widget::perform_action(
        *control,
        action,
        &[],
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
}

#[test]
fn rebuilt_controls_move_on_a_click_and_land_on_a_key() {
    let model = Rc::new(RefCell::new(Model {
        on: false,
        remember: false,
        nearby: false,
        level: 0.1,
        unread: 3,
    }));
    let mut d = FrameHarness::for_root(State::new(), Column::new(), (420, 400), Theme::dark());
    rebuild(&mut d, &model);
    rebuild(&mut d, &model);
    let _frame = crate::motion::pin_frame();
    for index in [SWITCH, BOX, RADIO, SLIDER, BADGE] {
        assert!(
            !moving(&d, index),
            "{index}: built again unchanged, at rest"
        );
    }

    // A click: the view builds every control anew, and the clicked one
    // moves from where the one it replaces was.
    for index in [SWITCH, BOX, RADIO] {
        let at = rect(&d, index);
        let at = if index == SWITCH {
            at.center()
        } else {
            vello::kurbo::Point::new(at.x0 + 12.0, at.center().y)
        };
        d.click_at(at.x, at.y);
        rebuild(&mut d, &model);
        assert!(moving(&d, index), "{index}: a click moves it");
    }
    let m = *model.borrow();
    assert!(m.on && m.remember && m.nearby, "every click took");
    // A press on the track away from the thumb glides it there.
    let track = rect(&d, SLIDER);
    d.click_at(track.x0 + track.width() * 0.9, track.center().y);
    assert!(model.borrow().level > 0.5, "the press set the value");
    rebuild(&mut d, &model);
    assert!(moving(&d, SLIDER), "the thumb glides to the press");
    // A count that changes on screen cross-fades.
    model.borrow_mut().unread = 4;
    rebuild(&mut d, &model);
    assert!(moving(&d, BADGE), "the count cross-fades");
    drop(_frame);

    // Once at rest, a key or an assistive action lands at once.
    let mut d = FrameHarness::for_root(State::new(), Column::new(), (420, 400), Theme::dark());
    rebuild(&mut d, &model);
    rebuild(&mut d, &model);
    let _frame = crate::motion::pin_frame();
    for (index, source) in [
        (SWITCH, MotionSource::Keyboard),
        (BOX, MotionSource::Keyboard),
        (RADIO, MotionSource::Action),
    ] {
        act(&mut d, index, &Action::Activate, source);
        rebuild(&mut d, &model);
        assert!(!moving(&d, index), "{index}: {source:?} lands");
    }
    let m = *model.borrow();
    assert!(!m.on && !m.remember && !m.nearby, "every key took");
    act(&mut d, SLIDER, &Action::Decrement, MotionSource::Keyboard);
    rebuild(&mut d, &model);
    assert!(!moving(&d, SLIDER), "a key puts the thumb on the value");
}

/// Another page's switch in the same place does not slide: the switch it
/// stands in for showed another value, so it rests on its own.
#[test]
fn a_rebuilt_switch_showing_another_value_rests() {
    let model = Rc::new(RefCell::new(Model {
        on: true,
        remember: false,
        nearby: false,
        level: 0.5,
        unread: 0,
    }));
    let mut d = FrameHarness::for_root(State::new(), Column::new(), (420, 400), Theme::dark());
    rebuild(&mut d, &model);
    let _frame = crate::motion::pin_frame();
    crate::motion::note_source(MotionSource::Pointer);
    model.borrow_mut().on = false;
    rebuild(&mut d, &model);
    assert!(!moving(&d, SWITCH));
}
