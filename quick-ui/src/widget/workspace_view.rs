use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::PresenceCtx;
use crate::animation::Spring;
use crate::tokens::SpringParams;

#[derive(Clone, Copy, Debug)]
pub enum WorkspaceSwitchDirection { Up, Down }

pub struct WorkspaceView {
    workspace_count: usize,
    active_index: usize,
    slide: Spring, // animates signed offset back to 0
    rect: Rect,
}

impl WorkspaceView {
    pub fn new(workspace_count: usize, active_index: usize) -> Self {
        let wc = workspace_count.max(1);
        Self {
            workspace_count: wc,
            active_index: active_index.min(wc - 1),
            slide: Spring::new(0.0, SpringParams { stiffness: 220.0, damping: 24.0, mass: 1.0 }),
            rect: Rect::ZERO,
        }
    }
    pub fn active_index(&self) -> usize { self.active_index }
    /// Returns the current pending slide offset (non-zero while a switch
    /// transition is in flight). The spring eases this back to 0.
    pub fn slide_target(&self) -> f32 { self.slide.value() }
    pub fn switch(&mut self, d: WorkspaceSwitchDirection) -> bool {
        let (new_idx, direction_sign) = match d {
            WorkspaceSwitchDirection::Up => {
                if self.active_index == 0 { return false; }
                (self.active_index - 1, -1.0_f32)
            }
            WorkspaceSwitchDirection::Down => {
                if self.active_index + 1 >= self.workspace_count { return false; }
                (self.active_index + 1, 1.0_f32)
            }
        };
        self.active_index = new_idx;
        // Kick the slide spring: snap to incoming offset then relax to 0.
        let h = self.rect.height().max(300.0) as f32;
        self.slide.snap(direction_sign * h);
        self.slide.set_target(0.0);
        true
    }
    pub fn tick_animation(&mut self, dt: f32) { self.slide.step(dt); }
}

impl Widget for WorkspaceView {
    fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
    fn update(&mut self, _s: &mut AutoCommit, _p: &ObjId) -> bool {
        if !self.slide.is_settled() { self.tick_animation(1.0 / 60.0); true } else { false }
    }
    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let n = taffy.new_leaf(Style { size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) }, ..Default::default() }).unwrap();
        node_map.insert(n, self as *mut dyn Widget);
        n
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, n: NodeId, origin: Point) {
        let l = taffy.layout(n).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
    }
    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, _p: &PresenceCtx) {
        let offset = self.slide.value();
        let t = transform * Affine::translate((0.0, offset as f64));
        scene.fill(vello::peniko::Fill::NonZero, t, theme.tokens.colors.surfaces.base, None, &self.rect);
        let s = format!("workspace {}/{}", self.active_index + 1, self.workspace_count);
        let text_size = theme.tokens.typography.scale.lg;
        let (tw, _) = tr.measure(&s, text_size, None);
        let x = self.rect.center().x - tw as f64 / 2.0;
        let y = self.rect.y0 + theme.tokens.spacing.s2 as f64;
        tr.draw_text(scene, &s, text_size, theme.tokens.colors.text_dim, t * Affine::translate((x, y)), None);
    }
    fn handle_event(&mut self, _e: &WindowEvent, _sf: f64, _m: &winit::event::Modifiers, _s: &mut AutoCommit, _p: &ObjId, _h: &mut bool, _f: &mut Option<NodeId>, _n: NodeId, _t: &TaffyTree<()>) {}
    fn name(&self) -> &str { "WorkspaceView" }
    fn get_rect(&self) -> Rect { self.rect }
}
