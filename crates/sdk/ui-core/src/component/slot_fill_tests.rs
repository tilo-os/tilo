//! A described component's slot sizes a view as the view asked: a view that says `fill` fills the slot
//! it is put in, for every component with a body slot, never only for the layout primitives whose slots
//! are the view arguments.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion"
)]

use std::collections::BTreeMap;

use super::*;
use crate::component::{Component, Described, Source};
use crate::headless::FrameHarness;
use crate::theme::Theme;
use crate::widget::arrange::{Fit, Sizing};
use crate::widget::{Card, CardProps, Element, Fitted};

struct Node {
    key: String,
    props: BTreeMap<String, Value>,
    slots: Vec<(&'static str, Element)>,
}

impl Node {
    fn new(key: &str) -> Self {
        Self {
            key: key.to_owned(),
            props: BTreeMap::new(),
            slots: Vec::new(),
        }
    }

    fn holding(mut self, slot: &'static str, view: Element) -> Self {
        self.slots.push((slot, view));
        self
    }
}

impl Source for Node {
    fn key(&self) -> &str {
        &self.key
    }

    fn prop(&mut self, name: &str) -> Option<Value> {
        self.props.get(name).cloned()
    }

    fn event(&mut self, _name: &str) -> Event {
        Event::None
    }

    fn binding(&mut self, _name: &str) -> Option<BoundValue> {
        None
    }

    fn children(&mut self, slot: &str) -> Vec<Element> {
        self.slots
            .iter_mut()
            .filter(|(name, _)| *name == slot)
            .map(|(_, view)| std::mem::replace(view, Box::new(crate::widget::Absent::new())))
            .collect()
    }

    fn components(
        &mut self,
        _slot: &str,
        _read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<usize, String> {
        Ok(0)
    }
}

const SUBJECT: &str = "subject";

fn subject() -> Element {
    Box::new(
        crate::widget::arrange::Stack::stack()
            .fill_width()
            .fill_height()
            .with_id(SUBJECT)
            .push(Box::new(crate::widget::Spacer::fixed(20.0))),
    )
}

fn filling_view() -> Element {
    Box::new(Fitted::new(subject(), Fit::hug().height(Sizing::fill(1.0))))
}

fn body_height(card: Card, height: u32) -> f64 {
    let harness = FrameHarness::for_root(crate::State::new(), card, (600, height), Theme::light());
    crate::widget::find_test_id(harness.root_widget(), SUBJECT)
        .expect("the view in the body is laid out")
        .get_rect()
        .height()
}

#[test]
fn a_body_slot_child_that_fills_gets_the_slots_size() {
    let mut node = Node::new("card").holding("body", filling_view());
    let props = CardProps::from_source(&mut node).expect("the card reads its body");
    let card = Card::from_props("card", props);
    let height = body_height(card, 500);
    assert!(
        height > 400.0,
        "the view that fills took the card's room, not one line: {height}"
    );
}

#[test]
fn the_fill_is_the_slots_and_scales_with_it() {
    for (height, least) in [(300u32, 200.0), (500u32, 400.0)] {
        let mut node = Node::new("card").holding("body", filling_view());
        let props = CardProps::from_source(&mut node).expect("the card reads its body");
        let card = Card::from_props("card", props);
        let got = body_height(card, height);
        assert!(got > least, "at {height} tall the body has {got}");
    }
}

#[test]
fn a_view_that_asks_for_nothing_hugs_its_content() {
    let mut node = Node::new("card").holding("body", subject());
    let props = CardProps::from_source(&mut node).expect("the card reads its body");
    let card = Card::from_props("card", props);
    let height = body_height(card, 500);
    assert!(
        height < 200.0,
        "a view that says nothing is as tall as its content: {height}"
    );
}

/// The same view saying `aspect: 16/9` and `fill` is 16:9 wide in the card's body: the fit
/// reaches a described component's slot as a `stack`'s does, aspect included, and the ratio
/// decides the height from the width the body gives it.
#[test]
fn a_body_slot_child_that_fills_at_an_aspect_keeps_its_ratio() {
    let view = Box::new(Fitted::new(subject(), Fit::fill(1.0).aspect(16.0 / 9.0))) as Element;
    let mut node = Node::new("card").holding("body", view);
    let props = CardProps::from_source(&mut node).expect("the card reads its body");
    let card = Card::from_props("card", props);
    let harness = FrameHarness::for_root(crate::State::new(), card, (600, 800), Theme::light());
    let rect = crate::widget::find_test_id(harness.root_widget(), SUBJECT)
        .expect("the view in the body is laid out")
        .get_rect();
    let (width, height) = (rect.width(), rect.height());
    assert!(
        (width - height * 16.0 / 9.0).abs() < 1.0,
        "the view in the card's body is {width}x{height}, not 16:9"
    );
}
