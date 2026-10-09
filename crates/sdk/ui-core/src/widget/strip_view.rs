use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::{Duration, Instant};
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{MouseScrollDelta, WindowEvent};

pub const STRIP_COOLDOWN_MS: u64 = 600;

/// `-1`, `0`, or `1` for a wheel delta's sign (inverted, matching scroll
/// convention: a positive delta pages backward). Never anything else, so
/// this sidesteps the narrowing `f64 -> i32` cast and the negation
/// entirely rather than relying on `-v.signum() as i32` to stay in range.
fn wheel_sign(v: f64) -> i32 {
    if v > 0.0 { -1 } else { i32::from(v < 0.0) }
}

pub struct StripView {
    strip_names: Vec<String>,
    active_index: usize,
    rect: Rect,
    last_paginate_ms: Option<u128>,
    start: Instant,
}

impl StripView {
    pub fn new(strip_names: Vec<String>, active_index: usize) -> Self {
        let clamped = if strip_names.is_empty() {
            0
        } else {
            active_index.min(strip_names.len().saturating_sub(1))
        };
        Self {
            strip_names,
            active_index: clamped,
            rect: Rect::ZERO,
            last_paginate_ms: None,
            start: crate::time::now(),
        }
    }

    /// Reconcile canonical labels and local selection without restarting the view.
    pub fn reconcile(&mut self, names: Vec<String>, active: usize) -> bool {
        let active = active.min(names.len().saturating_sub(1));
        let changed = self.strip_names != names || self.active_index != active;
        self.strip_names = names;
        self.active_index = active;
        changed
    }
    pub fn active_index(&self) -> usize {
        self.active_index
    }
    pub fn strip_names(&self) -> &[String] {
        &self.strip_names
    }

    /// Returns `true` if the paginate applied. `direction` is +1 or -1.
    pub fn try_paginate(&mut self, direction: i32, now_since_start: Duration) -> bool {
        let now_ms = now_since_start.as_millis();
        if let Some(last) = self.last_paginate_ms
            && now_ms.saturating_sub(last) < u128::from(STRIP_COOLDOWN_MS)
        {
            return false;
        }
        let new_idx = i64::try_from(self.active_index)
            .unwrap_or(i64::MAX)
            .saturating_add(i64::from(direction));
        let len = i64::try_from(self.strip_names.len()).unwrap_or(i64::MAX);
        if new_idx < 0 || new_idx >= len {
            return false;
        }
        self.active_index = usize::try_from(new_idx).unwrap_or(self.active_index);
        self.last_paginate_ms = Some(now_ms);
        true
    }
}

impl Widget for StripView {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Zero-height leaf — this widget's job is scroll-wheel pagination,
        // not rendering; visible strip UI lives on the real window-groups
        // the shell tiles next to it.
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(0.0),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
    }
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _tr: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Invisible — real strip chrome lives in the shell's window-group
        // layout + top-bar.
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let WindowEvent::MouseWheel { delta, .. } = event {
            let sign = match delta {
                MouseScrollDelta::LineDelta(_x, y) => wheel_sign(f64::from(*y)),
                MouseScrollDelta::PixelDelta(pp) => wheel_sign(pp.y),
            };
            if sign != 0 {
                let now = crate::time::now().duration_since(self.start);
                if self.try_paginate(sign, now) {
                    *ctx.handled = true;
                }
            }
        }
    }
    fn name(&self) -> &'static str {
        "StripView"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}
