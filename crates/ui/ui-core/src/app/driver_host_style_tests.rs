//! What a host's own node around the root gives it: a root positioned inside the viewport it
//! does not fill, and a root laid out at its content height past it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixture: a failure is the assertion"
)]

use std::collections::HashMap;

use taffy::prelude::*;

use crate::app::driver::{Driver, FrameOptions, Framed};
use crate::state::State;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{TaffyTreeExt as _, Widget};
use vello::kurbo::Rect;

/// A leaf `width` wide and 40 tall, which records nothing but where it was placed.
struct Probe {
    width: f32,
    rect: Rect,
}

impl Widget for Probe {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size::from_lengths(self.width, 40.0),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

    fn name(&self) -> &'static str {
        "Probe"
    }
}

#[test]
fn host_style_positions_the_root_inside_the_viewport() {
    let probe = Framed::with_options(
        State::new(),
        Probe {
            width: 120.0,
            rect: Rect::ZERO,
        },
        (400, 200),
        FrameOptions {
            host_style: Some(Style {
                justify_content: Some(JustifyContent::Center),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    assert!((probe.rect.x0 - 140.0).abs() < f64::EPSILON);
}

/// A leaf 300 px tall that records the heights it was offered.
struct Tall {
    rect: Rect,
    offered: Vec<AvailableSpace>,
}

impl Widget for Tall {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }

    fn measure(
        &mut self,
        _known: Size<Option<f32>>,
        available: Size<AvailableSpace>,
        _text_renderer: &mut TextRenderer,
    ) -> Size<f32> {
        self.offered.push(available.height);
        Size {
            width: 100.0,
            height: 300.0,
        }
    }

    crate::rect_geometry!();

    fn name(&self) -> &'static str {
        "Tall"
    }
}

#[test]
fn content_height_lays_the_root_out_past_the_viewport() {
    let tall = || Tall {
        rect: Rect::ZERO,
        offered: Vec::new(),
    };
    let bounded = Framed::new(State::new(), tall(), (400, 200), Theme::dark());
    assert!((bounded.rect.height() - 200.0).abs() < f64::EPSILON);
    assert!(!bounded.offered.contains(&AvailableSpace::MaxContent));

    let unbounded = Framed::with_options(
        State::new(),
        tall(),
        (400, 200),
        FrameOptions {
            content_height: true,
            ..Default::default()
        },
    );
    assert!((unbounded.rect.height() - 300.0).abs() < f64::EPSILON);
    assert!(unbounded.offered.contains(&AvailableSpace::MaxContent));
}

#[test]
fn a_root_laid_out_at_its_content_height_grows_and_shrinks_with_it() {
    let mut driver = Driver::for_root_with(
        State::new(),
        Tall {
            rect: Rect::ZERO,
            offered: Vec::new(),
        },
        (400, 200),
        FrameOptions {
            content_height: true,
            ..Default::default()
        },
    );
    assert!(
        (driver.root::<Tall>().rect.height() - 300.0).abs() < f64::EPSILON,
        "the root is as tall as its content"
    );
    driver.relayout();
    assert!(
        (driver.root::<Tall>().rect.height() - 300.0).abs() < f64::EPSILON,
        "another frame keeps it"
    );
}
