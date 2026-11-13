//! A part of a tree that only fades, grows or moves is posed by the compositor: drawn once into a
//! layer of its own below the rest, and only its pose changes while it animates.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed lookup is the assertion"
)]

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_session_core::surface::SurfaceHost as _;

use tilo_surface_host::{Placement, WidgetHost};
use ui::kurbo::{Affine, Rect};
use ui::peniko::{Color, Fill};
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::widget::{TaffyTreeExt as _, Widget};

/// Where the part rests.
const PART: Rect = Rect::new(40.0, 20.0, 140.0, 80.0);

/// A square that fades by its opacity, and optionally something drawn before it.
struct Fader {
    opacity: Rc<Cell<f32>>,
    under: Rc<Cell<bool>>,
    rect: ui::kurbo::Rect,
}

impl Widget for Fader {
    fn name(&self) -> &'static str {
        "Fader"
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &ui::Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(
            Style {
                size: ui::taffy::Size::from_percent(1.0, 1.0),
                ..Style::default()
            },
            nodes,
            self,
        )
    }

    ui::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        _text: &mut ui::TextRenderer,
        _theme: &ui::Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
        if self.under.get() {
            scene.fill(
                Fill::NonZero,
                transform,
                Color::BLACK,
                None,
                &Rect::new(0.0, 0.0, 10.0, 10.0),
            );
        }
        ui::lift::lifted(
            scene,
            transform,
            (ui::scene_parts::PartKey::new("test.part", 0), PART),
            ui::lift::Pose::faded(self.opacity.get()),
            |scene, transform| scene.fill(Fill::NonZero, transform, Color::WHITE, None, &PART),
        );
    }
}

#[test]
fn a_fading_part_is_drawn_once_and_posed_by_the_compositor() {
    let _settled = ui::motion::testing::settled();
    let opacity = Rc::new(Cell::new(0.25));
    let under = Rc::new(Cell::new(false));
    let mut host = WidgetHost::new(
        Box::new(Fader {
            opacity: Rc::clone(&opacity),
            under: Rc::clone(&under),
            rect: Rect::ZERO,
        }),
        Placement {
            origin: (100, 50),
            size: (320, 240),
            scale: 2.0,
        },
        false,
    );
    // The tree drew once as it was made, before its host lifted anything; its frames from here on
    // lift the part.
    host.relayout();
    assert!(host.prepare_frame(), "the first frame draws it");
    let layers = host.layers();
    assert_eq!(layers.len(), 2, "the part below the rest of the tree");
    let part = &layers[0];
    assert_eq!(part.location, (140, 70), "where it rests on the desktop");
    assert_eq!(part.size, (100, 60));
    assert_eq!(
        part.frame.size(),
        (200, 120),
        "drawn at the surface's scale"
    );
    assert!((part.pose.opacity - 0.25).abs() < f32::EPSILON);
    assert!(part.frame.take_changed().is_some(), "drawn once");
    assert!(
        host.accepts_pointer(150.0, 75.0),
        "a press on the part reaches the surface"
    );
    assert!(!host.accepts_pointer(300.0, 200.0));

    // Fading further changes only its pose: the compositor draws the same pixels fainter.
    opacity.set(0.75);
    host.relayout();
    assert!(host.prepare_frame(), "the compositor draws the new pose");
    let layers = host.layers();
    assert!((layers[0].pose.opacity - 0.75).abs() < f32::EPSILON);
    assert!(
        layers[0].frame.take_changed().is_none(),
        "the part is not drawn again"
    );

    // Something drawn before it would stand under it: the part is drawn in place instead.
    under.set(true);
    host.relayout();
    assert!(host.prepare_frame());
    assert_eq!(host.layers().len(), 1, "drawn into the tree's own frame");

    // Lifted again, it shows what it drew before: nothing of it is drawn again.
    under.set(false);
    host.relayout();
    assert!(host.prepare_frame());
    let layers = host.layers();
    assert_eq!(layers.len(), 2);
    assert!(
        layers[0].frame.take_changed().is_none(),
        "the part comes back as it was drawn"
    );
}
