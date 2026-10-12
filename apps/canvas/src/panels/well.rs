//! The paint well at the foot of the tool rail: the colour the brush is
//! loaded with, as a drop of it, and the brush's width as a ring inside it.
//! Choosing it picks up the brush.

use std::collections::HashMap;

use ui::a11y::{AccessNode, Role};
use ui::taffy::{self, NodeId, Style, TaffyTree};
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Circle, Point, Rect, Stroke};
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
        let style = Style {
            size: taffy::Size {
                width: taffy::Dimension::length(side),
                height: taffy::Dimension::length(side),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.name = self.describe();
        let side =
            self.rect.width().min(self.rect.height()) - 2.0 * f64::from(theme.tokens.spacing.s1);
        if side <= 4.0 {
            return;
        }
        // The paint as a swatch of it: its edge reads on the rail whatever
        // the colour, and it rings under the pointer like any swatch.
        let colour = self.tools.colour();
        let swatch = Rect::from_center_size(self.rect.center(), (side, side));
        ui::widget::ColorSwatchFace::new(swatch, colour, self.name.clone())
            .hovered(self.hovered)
            .paint(scene, transform, text_renderer, theme);
        // The brush's width as a ring inside it, in the ink that reads on
        // the paint, so a wide brush still shows its colour; the narrowest
        // brush is a dot.
        let travel = self.tools.size.with(|size| size.clamp(0.0, 1.0));
        let stroke = 2.0 * f64::from(theme.tokens.metrics.chrome_rule_px);
        let ring = stroke + travel.sqrt() * (side / 2.0 - 2.0 * stroke - stroke).max(stroke);
        let ink = theme.tokens.colors.ink_on_any(
            &[colour],
            theme.tokens.colors.text_primary,
            ui::tokens::colors::ON_FILL_MIN_CONTRAST,
        );
        scene.stroke(
            &Stroke::new(stroke),
            transform,
            ink,
            None,
            &Circle::new(swatch.center(), ring),
        );
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

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {}

    fn name(&self) -> &'static str {
        "Well"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
