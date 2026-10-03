//! The paint well at the foot of the tool rail: the colour the brush is
//! loaded with, as a drop of it, and the brush's width as a ring inside it.
//! Choosing it picks up the brush.

use std::collections::HashMap;

use ui::a11y::{AccessNode, Role};
use ui::taffy::{self, NodeId, Style, TaffyTree};
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Circle, Point, Rect, Stroke};
use ui::vello::peniko::Fill;
use ui::widget::{AppModel, EventCtx, ModelPath, Widget};
use ui::winit::event::{ElementState, MouseButton, WindowEvent};
use ui::{PresenceCtx, TextRenderer, Theme};

use crate::tools::{Tool, Tools};

pub struct Well {
    rect: Rect,
    tools: Tools,
    hovered: bool,
    name: String,
}

impl Well {
    pub fn new(tools: Tools) -> Self {
        Self {
            rect: Rect::ZERO,
            tools,
            hovered: false,
            name: String::new(),
        }
    }

    fn describe(&self) -> String {
        let paint = self.tools.pigment.with(|pigment| {
            pigment
                .as_ref()
                .map_or_else(|| self.tools.hex(), |(name, _)| name.replace('-', " "))
        });
        ui::i18n::I18nManager::format(
            "canvas-well",
            &[
                ("paint", paint.into()),
                ("size", format!("{:.0}", self.tools.brush_size()).into()),
            ],
        )
    }
}

impl Widget for Well {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use ui::widget::TaffyTreeExt as _;
        // As tall as a panel row: the rail's tools are that size too.
        let side = theme.tokens.metrics.panel_row_height;
        let node = taffy.leaf_node(Style {
            size: taffy::Size {
                width: taffy::Dimension::length(side),
                height: taffy::Dimension::length(side),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let x = origin.x + f64::from(layout.location.x);
        let y = origin.y + f64::from(layout.location.y);
        self.rect = Rect::new(
            x,
            y,
            x + f64::from(layout.size.width),
            y + f64::from(layout.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.name = self.describe();
        let centre = self.rect.center();
        let radius = self.rect.width().min(self.rect.height()) / 2.0 - 3.0;
        if radius <= 2.0 {
            return;
        }
        let colour = self.tools.colour();
        let drop = Circle::new(centre, radius);
        scene.fill(Fill::NonZero, transform, colour, None, &drop);
        // An edge that reads on the rail whatever the paint: the rail's own
        // edge, a ring of the surface inside it so a dark paint on a dark
        // rail still has a rim.
        scene.stroke(
            &Stroke::new(2.0),
            transform,
            theme.bg_primary,
            None,
            &Circle::new(centre, radius - 1.0),
        );
        let rim = if self.hovered {
            theme.text_dim
        } else {
            ui::surface::edge_on(theme.bg_primary)
        };
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            rim,
            None,
            &Circle::new(centre, radius + 0.5),
        );
        // The brush's width as a ring inside the drop, in the ink that
        // reads on the paint, so a wide brush still shows its colour.
        let travel = self.tools.size.with(|size| size.clamp(0.0, 1.0));
        let ring = 2.0 + travel.sqrt() * (radius - 6.0).max(2.0);
        let ink = theme.tokens.colors.ink_on_any(
            &[colour],
            theme.text_primary,
            ui::tokens::colors::ON_FILL_MIN_CONTRAST,
        );
        if travel < 0.05 {
            scene.fill(
                Fill::NonZero,
                transform,
                ink,
                None,
                &Circle::new(centre, 2.0),
            );
        } else {
            scene.stroke(
                &Stroke::new(1.5),
                transform,
                ink.multiply_alpha(0.8),
                None,
                &Circle::new(centre, ring),
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let at = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.hovered = self.rect.contains(at);
            }
            WindowEvent::CursorLeft { .. } => self.hovered = false,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.hovered => {
                self.tools.tool.set(Tool::Brush);
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn tooltip(&self) -> Option<&str> {
        Some(self.name.as_str())
    }

    fn accessibility(&self) -> Option<AccessNode> {
        Some(AccessNode::new(Role::Image).with_name(self.describe()))
    }

    fn sync(&mut self, _state: &mut AppModel, _path: &ModelPath) {}

    fn name(&self) -> &'static str {
        "Well"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
