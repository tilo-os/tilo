use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{WindowEvent, MouseScrollDelta};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use std::time::{Duration, Instant};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::PresenceCtx;

pub const STRIP_COOLDOWN_MS: u64 = 600;

pub struct StripView {
    strip_names: Vec<String>,
    active_index: usize,
    rect: Rect,
    last_paginate_ms: Option<u128>,
    // Internal clock for real-time handling.
    start: Instant,
}

impl StripView {
    pub fn new(strip_names: Vec<String>, active_index: usize) -> Self {
        let clamped = if strip_names.is_empty() { 0 } else { active_index.min(strip_names.len() - 1) };
        Self {
            strip_names,
            active_index: clamped,
            rect: Rect::ZERO,
            last_paginate_ms: None,
            start: Instant::now(),
        }
    }

    pub fn active_index(&self) -> usize { self.active_index }
    pub fn strip_names(&self) -> &[String] { &self.strip_names }

    /// Returns `true` if the paginate applied. `direction` is +1 or -1.
    pub fn try_paginate(&mut self, direction: i32, now_since_start: Duration) -> bool {
        let now_ms = now_since_start.as_millis();
        if let Some(last) = self.last_paginate_ms {
            if now_ms.saturating_sub(last) < STRIP_COOLDOWN_MS as u128 { return false; }
        }
        let new_idx = self.active_index as i64 + direction as i64;
        if new_idx < 0 || new_idx >= self.strip_names.len() as i64 { return false; }
        self.active_index = new_idx as usize;
        self.last_paginate_ms = Some(now_ms);
        true
    }
}

impl Widget for StripView {
    fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        // Zero-height leaf — the StripView's job today is scroll-wheel
        // pagination, not rendering. Visible strip UI lives on the real
        // window-groups that the shell tiles next to it.
        let node = taffy.new_leaf(Style { size: Size { width: Dimension::percent(1.0), height: Dimension::length(0.0) }, flex_shrink: 0.0, ..Default::default() }).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
    }
    fn draw(&mut self, _scene: &mut Scene, _transform: Affine, _tr: &mut TextRenderer, _theme: &Theme, _presence: &PresenceCtx) {
        // Invisible today — real strip chrome lives in the shell's
        // window-group layout + top-bar.
    }
    fn handle_event(&mut self, event: &WindowEvent, _scale: f64, _m: &winit::event::Modifiers, _s: &mut AutoCommit, _p: &ObjId, handled: &mut bool, _focus: &mut Option<NodeId>, _n: NodeId, _t: &TaffyTree<()>) {
        if let WindowEvent::MouseWheel { delta, .. } = event {
            let sign = match delta {
                MouseScrollDelta::LineDelta(_x, y) => -y.signum() as i32,
                MouseScrollDelta::PixelDelta(pp) => -(pp.y.signum() as i32),
            };
            if sign != 0 {
                let now = Instant::now().duration_since(self.start);
                if self.try_paginate(sign, now) { *handled = true; }
            }
        }
    }
    fn name(&self) -> &str { "StripView" }
    fn get_rect(&self) -> Rect { self.rect }
}
