use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::{AppModel, ModelPath};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect, Vec2};
use winit::event::WindowEvent;

#[derive(Clone, Copy, Debug)]
pub enum WorkspaceSwitchDirection {
    Up,
    Down,
}

pub struct WorkspaceView {
    workspace_count: usize,
    active_index: usize,
    /// The switch's direction cue: the view's offset, settling to 0 on the
    /// drill (`motion::Drill`, a whole-view change in depth).
    slide: crate::motion::MotionValue<f64>,
    rect: Rect,
}

impl WorkspaceView {
    pub fn new(workspace_count: usize, active_index: usize) -> Self {
        let wc = workspace_count.max(1);
        Self {
            workspace_count: wc,
            active_index: active_index.min(wc.saturating_sub(1)),
            slide: crate::motion::MotionValue::new(0.0).with_tolerance(0.25),
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
    /// Non-zero while a switch transition is in flight; the drill settles
    /// it back to 0.
    pub fn slide_target(&self) -> f32 {
        crate::num::f32_of_f64(self.slide.get(crate::motion::frame_now()))
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
        let h = self.rect.height().max(300.0);
        let now = crate::time::now();
        self.slide.snap(f64::from(direction_sign) * h);
        let timing = crate::motion::Drill::SPEC.timing(
            crate::motion::Direction::Enter,
            crate::tokens::EffectiveMotion::default(),
            crate::tokens::MotionSource::Pointer,
        );
        self.slide.set(0.0, timing, now);
        true
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
        self.slide.is_moving(crate::motion::frame_now())
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
    crate::rect_geometry!();
    /// Sized by its style alone (no `measure`): nothing it changes needs
    /// taffy to measure it again, and saying it does re-solved the layout of
    /// everything above it on every frame.
    fn is_layout_dirty(&self) -> bool {
        false
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
        let offset = self.slide.get(crate::motion::frame_now());
        if offset.abs() > f64::EPSILON {
            let t = transform.pre_translate(Vec2::new(0.0, offset));
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
}
