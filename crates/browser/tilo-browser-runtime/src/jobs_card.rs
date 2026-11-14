//! The page's long work on the desk: a card at the desk's foot with a row for each system job (a
//! folder going up into the workspace, say), drawn by the system's own jobs band
//! ([`ui::jobs::Band`]) so it reads as long work reads in every app: what it is, what it is doing
//! now and how far, Stop while it runs, and how it ended until the person reads it.
//!
//! The card stands on a layer of its own over the desk ([`crate::layers`]) and shows only while
//! there is a job to show; the shell lays it out again whenever the jobs move
//! ([`JobsCard::follow`]).

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use ui::kurbo::{Affine, Rect};
use ui::presence::PresenceCtx;
use ui::taffy::{
    Display, LengthPercentage, LengthPercentageAuto, NodeId, Position, Style, TaffyTree,
};
use ui::vello::Scene;
use ui::widget::{DrawChild as _, EventCtx, TaffyTreeExt as _, Widget};
use ui::winit::event::WindowEvent;
use ui::{TextRenderer, Theme};

/// How wide the card stands, in logical pixels.
const WIDTH: f32 = 380.0;

/// What the shell keeps of the card it placed: the presses its rows report, and the jobs as it
/// last laid them out.
pub(crate) struct JobsCard {
    presses: ui::jobs::Presses,
    seen: Cell<u64>,
}

impl JobsCard {
    /// Place the card on `layers`, asking `wake` for the frame it shows in.
    pub(crate) fn place(layers: &mut crate::layers::Layers, wake: Rc<dyn Fn()>) -> Self {
        use tilo_session::platform::surfaces::{Heard, Placement, Surfaces as _};
        let presses = ui::jobs::Presses::new();
        let card = Card {
            band: ui::jobs::Band::with_presses(presses.clone()),
            rect: Rect::ZERO,
            shown: false,
        };
        // The page lays its layers out over the whole desk: where the card stands is its own.
        layers.placing(wake).place(
            "jobs",
            Box::new(card),
            Placement {
                origin: (0, 0),
                size: (0, 0),
                scale: 1.0,
            },
            Heard {
                wants_keyboard: Rc::new(|| false),
                pressed_elsewhere: Rc::new(|| {}),
                takes_bar_point: Rc::new(|_| false),
            },
        );
        Self {
            presses,
            seen: Cell::new(u64::MAX),
        }
    }

    /// Carry out what the person pressed on the card (Stop, a finished row read), and say whether
    /// the jobs moved since the card was last laid out: the shell lays it out again then.
    pub(crate) fn follow(&self) -> bool {
        for press in self.presses.take() {
            // A row of the page's own work goes to nothing of its own to show.
            let _shown = ui::jobs::apply(press);
        }
        let revision = ui::jobs::revision();
        self.seen.replace(revision) != revision
    }
}

/// The card itself, on its layer: the band in a floating card at the desk's bottom right.
struct Card {
    band: ui::jobs::Band,
    rect: Rect,
    /// Whether any job is there to show: with none, the card neither draws nor takes input.
    shown: bool,
}

impl Widget for Card {
    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        self.band.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The registry moved since the rows were made: they are made again from it.
        if self.band.presses().changed() {
            self.band = ui::jobs::Band::with_presses(self.band.presses().clone());
        }
        self.shown = !self.band.is_empty();
        let inner = self.band.build_layout(taffy, theme, map);
        let margin = theme.tokens.spacing.s4;
        let padding = LengthPercentage::length(theme.tokens.spacing.s3);
        let style = Style {
            display: if self.shown {
                Display::Flex
            } else {
                Display::None
            },
            position: Position::Absolute,
            inset: ui::taffy::Rect {
                left: LengthPercentageAuto::auto(),
                right: LengthPercentageAuto::length(margin),
                top: LengthPercentageAuto::auto(),
                bottom: LengthPercentageAuto::length(margin),
            },
            size: ui::taffy::Size {
                width: ui::taffy::Dimension::length(WIDTH),
                height: ui::taffy::Dimension::auto(),
            },
            padding: ui::taffy::Rect {
                left: padding,
                right: padding,
                top: padding,
                bottom: padding,
            },
            flex_direction: ui::taffy::FlexDirection::Column,
            ..Default::default()
        };
        taffy.widget_node(style, &[inner], map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: ui::kurbo::Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        if let Some(inner) = taffy.children(node).unwrap_or_default().first().copied() {
            self.band.apply_layout(taffy, inner, self.rect.origin());
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.shown {
            return;
        }
        ui::widget::floating_surface::paint_card(scene, transform, self.rect, theme);
        self.band
            .draw_child(scene, transform, text, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if !self.shown {
            return;
        }
        let Some(&inner) = ctx.taffy.children(ctx.node_id).unwrap_or_default().first() else {
            return;
        };
        self.band
            .handle_event(event, &mut ctx.reborrow(ctx.path, inner));
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if self.shown {
            vec![&self.band]
        } else {
            Vec::new()
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.shown {
            vec![&mut self.band]
        } else {
            Vec::new()
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.band.take_structure_dirty()
    }

    fn needs_redraw(&self) -> bool {
        self.band.needs_redraw()
    }

    fn name(&self) -> &'static str {
        "JobsCard"
    }

    fn get_rect(&self) -> Rect {
        if self.shown { self.rect } else { Rect::ZERO }
    }
}
