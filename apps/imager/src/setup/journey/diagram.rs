//! Decorative connections beneath the shared identity avatar. No controls.
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{TaffyTreeExt, Widget};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, Stroke};

pub struct DeviceLinks {
    rect: Rect,
    started: ui::time::Instant,
    animate: bool,
}
impl DeviceLinks {
    pub fn new(animated: bool) -> Self {
        Self {
            rect: Rect::ZERO,
            started: ui::time::Instant::now(),
            animate: animated,
        }
    }
}
impl Widget for DeviceLinks {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(theme.tokens.spacing.rhythm.group * 3.0),
            },
            ..Default::default()
        });
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::from_origin_size(
            (
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
            ),
            (f64::from(layout.size.width), f64::from(layout.size.height)),
        );
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let progress = if theme.reduce_motion || !self.animate {
            1.0
        } else {
            (self.started.elapsed().as_secs_f64()
                / (f64::from(theme.tokens.motion.durations.slow_ms) / 1000.0).max(0.001))
            .min(1.0)
        };
        self.animate = progress < 1.0;
        let bottom = self.rect.y0 + self.rect.height() * progress;
        let start = self.rect.center().x;
        for end in [
            self.rect.x0 + self.rect.width() * 0.25,
            self.rect.x0 + self.rect.width() * 0.75,
        ] {
            let mut path = BezPath::new();
            path.move_to((start, self.rect.y0));
            path.curve_to((start, bottom), (end, self.rect.y0), (end, bottom));
            scene.stroke(
                &Stroke::new(f64::from(theme.ui_scale)),
                transform,
                theme.text_dim,
                None,
                &path,
            );
        }
    }
    fn needs_animation_frame(&self) -> bool {
        self.animate
    }
    fn name(&self) -> &'static str {
        "IdentityDeviceConnections"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}
