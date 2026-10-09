use crate::animation::Spring;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::SpringParams;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::{AppModel, ModelPath};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use winit::event::WindowEvent;

#[derive(Clone, Copy, Debug)]
pub enum WorkspaceSwitchDirection {
    Up,
    Down,
}

pub struct WorkspaceView {
    workspace_count: usize,
    active_index: usize,
    slide: Spring,
    rect: Rect,
}

impl WorkspaceView {
    pub fn new(workspace_count: usize, active_index: usize) -> Self {
        let wc = workspace_count.max(1);
        Self {
            workspace_count: wc,
            active_index: active_index.min(wc.saturating_sub(1)),
            slide: Spring::new(
                0.0,
                SpringParams {
                    stiffness: 220.0,
                    damping: 24.0,
                    mass: 1.0,
                },
            ),
            rect: Rect::ZERO,
        }
    }
    /// Apply the canonical workspace count and host-local selection.
    pub fn reconcile(&mut self, count: usize, active: usize) -> bool {
        let count = count.max(1);
        let active = active.min(count.saturating_sub(1));
        let changed = self.workspace_count != count || self.active_index != active;
        self.workspace_count = count;
        self.active_index = active;
        changed
    }
    pub fn active_index(&self) -> usize {
        self.active_index
    }
    /// Non-zero while a switch transition is in flight; the spring eases it
    /// back to 0.
    pub fn slide_target(&self) -> f32 {
        self.slide.value()
    }
    pub fn switch(&mut self, d: WorkspaceSwitchDirection) -> bool {
        let (new_idx, direction_sign) = match d {
            WorkspaceSwitchDirection::Up => {
                if self.active_index == 0 {
                    return false;
                }
                (self.active_index.saturating_sub(1), -1.0_f32)
            }
            WorkspaceSwitchDirection::Down => {
                if self.active_index.saturating_add(1) >= self.workspace_count {
                    return false;
                }
                (self.active_index.saturating_add(1), 1.0_f32)
            }
        };
        self.active_index = new_idx;
        // The workspace view's own on-screen height is always many orders
        // of magnitude below f32::MAX, so this narrowing can't truncate.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "screen-space pixel height never approaches f32::MAX"
        )]
        let h = self.rect.height().max(300.0) as f32;
        self.slide.snap(direction_sign * h);
        self.slide.set_target(0.0);
        true
    }
    pub fn tick_animation(&mut self, dt: f32) {
        self.slide.step(dt);
    }
}

impl Widget for WorkspaceView {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn update(&mut self, _s: &AppModel, _p: &ModelPath) -> bool {
        if self.slide.is_settled() {
            false
        } else {
            self.tick_animation(1.0 / 60.0);
            true
        }
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Zero-height: this is a keyboard-handler + animation driver, not a
        // visible rect. Workspace chrome lives in the top bar.
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
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, n: NodeId, origin: Point) {
        let l = taffy.layout(n).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _tr: &mut TextRenderer,
        theme: &Theme,
        _p: &PresenceCtx,
    ) {
        // A static workspace inherits the app background and doesn't need
        // to re-paint it.
        let offset = self.slide.value();
        if offset.abs() > f32::EPSILON {
            let t = transform.pre_translate(Vec2::new(0.0, f64::from(offset)));
            scene.fill(
                vello::peniko::Fill::NonZero,
                t,
                theme.tokens.colors.surfaces.base,
                None,
                &self.rect,
            );
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let WindowEvent::KeyboardInput {
            event:
                winit::event::KeyEvent {
                    logical_key: winit::keyboard::Key::Named(k),
                    state: winit::event::ElementState::Pressed,
                    ..
                },
            ..
        } = event
            && (ctx.modifiers.state().super_key() || ctx.modifiers.state().control_key())
        {
            match k {
                winit::keyboard::NamedKey::ArrowUp => {
                    if self.switch(WorkspaceSwitchDirection::Up) {
                        *ctx.handled = true;
                    }
                }
                winit::keyboard::NamedKey::ArrowDown
                    if self.switch(WorkspaceSwitchDirection::Down) =>
                {
                    *ctx.handled = true;
                }
                _ => {}
            }
        }
    }
    fn name(&self) -> &'static str {
        "WorkspaceView"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}
