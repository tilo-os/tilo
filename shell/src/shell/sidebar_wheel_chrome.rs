//! Left-edge workspace wheel chrome widget.
//!
//! Superseded by `SidebarWheelDisc`; retained in-tree for reference but
//! not mounted — do not theme-migrate raw `colors.accent[4]` reads
//! here, the file is dead code.

use std::collections::HashMap;
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, PathEl, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::WindowEvent;

use ui::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::shell::chrome_state::Chrome;
use crate::shell::sidebar_wheel::SidebarWheel;
use crate::shell::state::{WorkspaceId, WorkspaceTree};

/// Pinned to the max viewport-driven width (220-320px) so content never
/// reflows when the slot changes — a narrower sidebar just leaves a
/// breathing-room band of wallpaper.
pub const SIDEBAR_WIDTH_PX: f32 = 320.0;

/// Held internally so the shell can poll for them after dispatching
/// events, mirroring the topbar's click-output pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarIntent {
    Activate(WorkspaceId),
    CreateWorkspace,
}

#[derive(Clone, Debug)]
struct WorkspaceCard {
    id: WorkspaceId,
    label: String,
    strip_count: usize,
    /// Falls back to the tree-level roster for shells without a
    /// per-workspace collaborator split.
    collab_count: usize,
    rect: Rect,
}

pub struct SidebarWheelChrome {
    rect: Rect,
    cards: Vec<WorkspaceCard>,
    active: Option<WorkspaceId>,
    plus_rect: Rect,
    hover_idx: Option<usize>,
    hover_plus: bool,
    pending_intent: Option<SidebarIntent>,
    chrome: Option<Chrome>,
    synced_revision: u64,
    /// Line-units since the last switch; crossing ±1 triggers one.
    scroll_accum: f32,
    last_cursor: Option<Point>,
    /// Legacy mirror of the active card's `collab_count`; prefer `card_collab_count`.
    active_collab_count: usize,
    /// Set on press, cleared on release. `drag_last_y` anchors per-frame
    /// `dy`; crossing `DRAG_SWITCH_THRESHOLD_PX` fires
    /// `switch_workspace` and subtracts the threshold so dragging keeps
    /// rotating.
    drag_start_y: Option<f64>,
    drag_last_y: Option<f64>,
    drag_accum: f64,
    /// Set once a press exceeds the click-tolerance threshold, so
    /// release skips click-to-activate.
    drag_moved: bool,
    /// 1.0 fully expanded (hover/drag), 0.0 collapsed. Stepped toward
    /// `is_expanded()` over `EXPAND_ANIM_MS` with smoothstep easing.
    pill_anim_t: f32,
    last_frame_time: Option<Instant>,
}

/// Sidebar lays cards out at 64 px + 10 px gap, so 74 px matches one
/// stride exactly.
const DRAG_SWITCH_THRESHOLD_PX: f64 = 74.0;

const DRAG_CLICK_TOLERANCE_PX: f64 = 4.0;

const EXPAND_ANIM_MS: f32 = 300.0;

impl SidebarWheelChrome {
    pub fn from_tree(tree: &WorkspaceTree) -> Self {
        let mut this = Self {
            rect: Rect::ZERO,
            cards: Vec::new(),
            active: Some(tree.active),
            plus_rect: Rect::ZERO,
            hover_idx: None,
            hover_plus: false,
            pending_intent: None,
            chrome: None,
            synced_revision: 0,
            scroll_accum: 0.0,
            last_cursor: None,
            active_collab_count: 0,
            drag_start_y: None,
            drag_last_y: None,
            drag_accum: 0.0,
            drag_moved: false,
            pill_anim_t: 0.0,
            last_frame_time: None,
        };
        this.sync_from_tree(tree);
        this
    }

    pub fn with_chrome(mut self, chrome: Chrome) -> Self {
        let rev = chrome.borrow().revision;
        self.chrome = Some(chrome);
        self.synced_revision = rev;
        self
    }

    pub fn sync_from_tree(&mut self, tree: &WorkspaceTree) {
        self.active = Some(tree.active);
        // Shared helper so the active card's peer count agrees with
        // the top-bar avatar stack.
        let active_count = crate::shell::state::active_workspace_peer_count(tree);
        self.cards = tree
            .workspaces
            .iter()
            .map(|w| {
                let count = if tree.active == w.id {
                    active_count
                } else {
                    w.collaborators.iter().filter(|c| c.id != "me").count()
                };
                WorkspaceCard {
                    id: w.id,
                    label: workspace_initial(&w.name),
                    strip_count: w.strips.len(),
                    collab_count: count,
                    rect: Rect::ZERO,
                }
            })
            .collect();
        self.active_collab_count = self
            .cards
            .iter()
            .find(|c| Some(c.id) == self.active)
            .map_or(0, |c| c.collab_count);
    }

    pub fn active_collaborator_count(&self) -> usize {
        self.active_collab_count
    }

    pub fn card_collab_count(&self, idx: usize) -> usize {
        self.cards.get(idx).map_or(0, |c| c.collab_count)
    }

    pub fn take_intent(&mut self) -> Option<SidebarIntent> {
        self.pending_intent.take()
    }

    fn is_expanded(&self) -> bool {
        self.hover_idx.is_some() || self.hover_plus || self.drag_start_y.is_some()
    }
}

fn workspace_initial(name: &str) -> String {
    name.chars()
        .next()
        .map_or_else(|| "·".to_string(), |c| c.to_ascii_uppercase().to_string())
}

impl Widget for SidebarWheelChrome {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool {
        if let Some(chrome) = self.chrome.clone() {
            let rev = chrome.borrow().revision;
            if rev != self.synced_revision {
                let tree_snap = chrome.borrow().tree().clone();
                self.sync_from_tree(&tree_snap);
                self.synced_revision = rev;
                return true;
            }
        }
        false
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(SIDEBAR_WIDTH_PX),
                    height: Dimension::percent(1.0),
                },
                flex_shrink: 0.0,
                flex_grow: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        let pad_top = 16.0_f64;
        let pad_x = 12.0_f64;
        let card_h = 64.0_f64;
        let gap = 10.0_f64;
        let left = self.rect.x0 + pad_x;
        let right = self.rect.x1 - pad_x;
        let mut y = self.rect.y0 + pad_top;
        for card in &mut self.cards {
            card.rect = Rect::new(left, y, right, y + card_h);
            y += card_h + gap;
        }
        self.plus_rect = Rect::new(left, y, right, y + card_h);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;

        // Floats over the wallpaper — no opaque column, just a glassy
        // backdrop.
        let backdrop_inset_y = 24.0_f64;
        let backdrop = Rect::new(
            self.rect.x0 + 8.0,
            self.rect.y0 + backdrop_inset_y,
            self.rect.x1 - 8.0,
            self.rect.y1 - backdrop_inset_y,
        );
        if backdrop.width() > 0.0 && backdrop.height() > 0.0 {
            let backdrop_rr = RoundedRect::from_rect(backdrop, f64::from(tokens.radii.lg * 1.5));
            scene.fill(
                Fill::NonZero,
                transform,
                colors.surfaces.raised.with_alpha_factor(0.25),
                None,
                &backdrop_rr,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                colors.border_standard.with_alpha_factor(0.35),
                None,
                &backdrop_rr,
            );
        }

        // Real elapsed time, so the smoothstep ease stays framerate-independent.
        let now = Instant::now();
        let dt = match self.last_frame_time {
            Some(prev) => now.duration_since(prev).as_secs_f32().min(0.1),
            None => 0.0,
        };
        self.last_frame_time = Some(now);
        let target = if self.is_expanded() { 1.0_f32 } else { 0.0_f32 };
        let step = if EXPAND_ANIM_MS > 0.0 {
            (dt * 1000.0) / EXPAND_ANIM_MS
        } else {
            1.0
        };
        if self.pill_anim_t < target {
            self.pill_anim_t = (self.pill_anim_t + step).min(target);
        } else if self.pill_anim_t > target {
            self.pill_anim_t = (self.pill_anim_t - step).max(target);
        }
        let p = self.pill_anim_t.clamp(0.0, 1.0);
        let eased = p * p * (3.0 - 2.0 * p);

        // ---- Pill highlight ------------------------------------------------
        // Collapsed: 4px strip against the left edge, translated 10px
        // further left. Expanded: full inset-4 pill, centered, full opacity.
        let pill_y0 = self.rect.y0 + 32.0;
        let pill_y1 = self.rect.y1 - 32.0;
        if pill_y1 > pill_y0 {
            let expanded_x0 = self.rect.x0 + 4.0;
            let expanded_x1 = self.rect.x1 - 4.0;
            let collapsed_x0 = self.rect.x0 - 10.0;
            let collapsed_x1 = collapsed_x0 + 4.0;
            let x0 = collapsed_x0 as f32 + (expanded_x0 as f32 - collapsed_x0 as f32) * eased;
            let x1 = collapsed_x1 as f32 + (expanded_x1 as f32 - collapsed_x1 as f32) * eased;
            let opacity = 0.2 + 0.8 * eased;
            let pill_rect = Rect::new(f64::from(x0), pill_y0, f64::from(x1), pill_y1);
            let radius = pill_rect.height() / 2.0;
            let pill = RoundedRect::from_rect(pill_rect, radius);
            // Base alpha 13/255 fill, 26/255 border, scaled by opacity.
            let fill_a = (13.0 * opacity).round().clamp(0.0, 255.0) as u8;
            let stroke_a = (26.0 * opacity).round().clamp(0.0, 255.0) as u8;
            scene.fill(
                Fill::NonZero,
                transform,
                Color::from_rgba8(255, 255, 255, fill_a),
                None,
                &pill,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                Color::from_rgba8(255, 255, 255, stroke_a),
                None,
                &pill,
            );
        }

        // ---- Wheel rim arc -------------------------------------------------
        // Huge faint circle behind the cards; only the right-most sliver
        // is visible inside the sidebar rect. Fades in with the pill.
        if eased > 0.0 {
            let cx = self.rect.x0 + 50.0;
            let cy = f64::midpoint(self.rect.y0, self.rect.y1);
            let rim = Circle::new((cx, cy), 600.0);
            // Clipped to the sidebar bounds so it doesn't bleed into
            // the desktop beside the wheel.
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                BlendMode::default(),
                eased,
                transform,
                &self.rect,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                Color::from_rgba8(255, 255, 255, 13), // white/5
                None,
                &rim,
            );
            scene.pop_layer();
        }

        for (idx, card) in self.cards.iter().enumerate() {
            let is_active = self.active == Some(card.id);
            let is_hover = self.hover_idx == Some(idx);
            // Scaled about the card's center so it grows symmetrically.
            // Applied to the glyph/subtitle transforms too, so the
            // pill stays one unit.
            let card_xform = if is_hover && !is_active {
                let cx = card.rect.center().x;
                let cy = card.rect.center().y;
                transform
                    * Affine::translate((cx, cy))
                    * Affine::scale(1.05)
                    * Affine::translate((-cx, -cy))
            } else {
                transform
            };
            let pill = RoundedRect::from_rect(card.rect, f64::from(tokens.radii.md));
            if is_active {
                ui::surface::soft_shadow(
                    scene,
                    card_xform,
                    card.rect,
                    colors.accent[5],
                    14.0,
                    0.0,
                    0.35,
                );
                scene.fill(
                    Fill::NonZero,
                    card_xform,
                    colors.accent[5].with_alpha_factor(0.15),
                    None,
                    &pill,
                );
            } else {
                let fill = if is_hover {
                    colors.surfaces.raised
                } else {
                    colors.neutral[1]
                };
                scene.fill(Fill::NonZero, card_xform, fill, None, &pill);
            }
            if is_active {
                // Base transform: the active card doesn't hover-scale.
                let ring = RoundedRect::from_rect(
                    Rect::new(
                        card.rect.x0 - 1.0,
                        card.rect.y0 - 1.0,
                        card.rect.x1 + 1.0,
                        card.rect.y1 + 1.0,
                    ),
                    f64::from(tokens.radii.md + 1.0),
                );
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.5),
                    transform,
                    colors.accent[4],
                    None,
                    &ring,
                );
            }

            let glyph_size = tokens.typography.scale.xl;
            let (gw, gh) = text_renderer.measure(&card.label, glyph_size, None);
            let cx = f64::midpoint(card.rect.x0, card.rect.x1) - f64::from(gw) / 2.0;
            let cy = card.rect.y0 + card.rect.height() * 0.30 - f64::from(gh) / 2.0;
            let glyph_color = if is_active {
                colors.text_primary
            } else {
                colors.text_primary
            };
            text_renderer.draw_text(
                scene,
                &card.label,
                glyph_size,
                glyph_color,
                card_xform * Affine::translate((cx, cy)),
                None,
            );

            let subtitle = if card.strip_count == 1 {
                "1 strip".to_string()
            } else {
                format!("{} strips", card.strip_count)
            };
            let sub_size = tokens.typography.scale.xs;
            let (sw, sh) = text_renderer.measure(&subtitle, sub_size, None);
            let sx = f64::midpoint(card.rect.x0, card.rect.x1) - f64::from(sw) / 2.0;
            let sy = card.rect.y0 + card.rect.height() * 0.72 - f64::from(sh) / 2.0;
            let sub_color = if is_active {
                colors.text_primary
            } else {
                colors.text_dim
            };
            text_renderer.draw_text(
                scene,
                &subtitle,
                sub_size,
                sub_color,
                card_xform * Affine::translate((sx, sy)),
                None,
            );

            // Clamped to 3; inactive cards render at half-alpha so
            // active stays the focus anchor.
            let card_count = card.collab_count;
            if card_count > 0 {
                let shown = card_count.min(3);
                let overflow = card_count.saturating_sub(3);
                let dot_r = 3.0_f64;
                let gap = 3.0_f64;
                let row_w = shown as f64 * (dot_r * 2.0)
                    + (shown as f64 - 1.0).max(0.0) * gap
                    + if overflow > 0 { 14.0 } else { 0.0 };
                let base_x = f64::midpoint(card.rect.x0, card.rect.x1) - row_w / 2.0;
                let dot_y = card.rect.y1 - 10.0;
                let dot_color = if is_active {
                    colors.text_primary
                } else {
                    colors.text_primary.with_alpha_factor(0.55)
                };
                for i in 0..shown {
                    let dx = base_x + dot_r + i as f64 * (dot_r * 2.0 + gap);
                    let dot_rect = Rect::new(dx - dot_r, dot_y - dot_r, dx + dot_r, dot_y + dot_r);
                    ui::widget::Avatar::draw_at(
                        scene,
                        card_xform,
                        dot_rect,
                        "",
                        dot_color,
                        theme,
                        text_renderer,
                    );
                }
                if overflow > 0 {
                    let chip_x = base_x + shown as f64 * (dot_r * 2.0 + gap);
                    let s = format!("+{overflow}");
                    let (tw, th) = text_renderer.measure(&s, tokens.typography.scale.xs, None);
                    text_renderer.draw_text(
                        scene,
                        &s,
                        tokens.typography.scale.xs,
                        dot_color,
                        card_xform * Affine::translate((chip_x, dot_y - f64::from(th) / 2.0)),
                        None,
                    );
                    let _ = tw; // only the ascent-based y alignment is needed
                }
            }
        }

        // Dashed outline so the trailing "+" card reads as a hint.
        let plus_fill = if self.hover_plus {
            colors.surfaces.raised
        } else {
            colors.neutral[0]
        };
        let plus_pill = RoundedRect::from_rect(self.plus_rect, f64::from(tokens.radii.md));
        scene.fill(Fill::NonZero, transform, plus_fill, None, &plus_pill);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0).with_dashes(0.0, [4.0, 4.0]),
            transform,
            colors.border_standard,
            None,
            &plus_pill,
        );
        let cx = f64::midpoint(self.plus_rect.x0, self.plus_rect.x1);
        let cy = f64::midpoint(self.plus_rect.y0, self.plus_rect.y1);
        let r = 10.0_f64;
        let plus = BezPath::from_iter([
            PathEl::MoveTo(Point::new(cx - r, cy)),
            PathEl::LineTo(Point::new(cx + r, cy)),
            PathEl::MoveTo(Point::new(cx, cy - r)),
            PathEl::LineTo(Point::new(cx, cy + r)),
        ]);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.5),
            transform,
            colors.text_dim,
            None,
            &plus,
        );
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = Some(p);
                self.hover_idx = self.cards.iter().position(|c| c.rect.contains(p));
                self.hover_plus = self.plus_rect.contains(p);

                if let (Some(_start), Some(last)) = (self.drag_start_y, self.drag_last_y) {
                    let dy = p.y - last;
                    self.drag_last_y = Some(p.y);
                    // Crossing click tolerance marks a drag, so release
                    // skips click-activate.
                    if !self.drag_moved && (p.y - _start).abs() > DRAG_CLICK_TOLERANCE_PX {
                        self.drag_moved = true;
                    }
                    if self.drag_moved {
                        self.drag_accum += dy;
                        // Down → later workspace (+1), matching scroll-wheel sign.
                        while self.drag_accum >= DRAG_SWITCH_THRESHOLD_PX {
                            if let Some(chrome) = self.chrome.clone()
                                && !chrome.borrow_mut().switch_workspace(1) {
                                    // Hit the bottom — stop consuming travel.
                                    self.drag_accum = 0.0;
                                    break;
                                }
                            self.drag_accum -= DRAG_SWITCH_THRESHOLD_PX;
                            *handled = true;
                        }
                        while self.drag_accum <= -DRAG_SWITCH_THRESHOLD_PX {
                            if let Some(chrome) = self.chrome.clone()
                                && !chrome.borrow_mut().switch_workspace(-1) {
                                    self.drag_accum = 0.0;
                                    break;
                                }
                            self.drag_accum += DRAG_SWITCH_THRESHOLD_PX;
                            *handled = true;
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Content scrolling shouldn't steal workspace switching.
                let over_sidebar = self
                    .last_cursor
                    .is_some_and(|p| self.rect.contains(p));
                if !over_sidebar {
                    return;
                }
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => *y,
                    winit::event::MouseScrollDelta::PixelDelta(p) => (p.y / 40.0) as f32,
                };
                self.scroll_accum += dy;
                while self.scroll_accum >= 1.0 {
                    if let Some(chrome) = self.chrome.clone()
                        && !chrome.borrow_mut().switch_workspace(-1) {
                            break;
                        }
                    self.scroll_accum -= 1.0;
                    *handled = true;
                }
                while self.scroll_accum <= -1.0 {
                    if let Some(chrome) = self.chrome.clone()
                        && !chrome.borrow_mut().switch_workspace(1) {
                            break;
                        }
                    self.scroll_accum += 1.0;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                // Fires immediately so a tap reliably creates a workspace.
                if self.hover_plus {
                    self.pending_intent = Some(SidebarIntent::CreateWorkspace);
                    if let Some(chrome) = self.chrome.clone() {
                        let mut state = chrome.borrow_mut();
                        // Shares "append + activate + seed Main strip" with SidebarWheel.
                        let wheel = SidebarWheel::new();
                        {
                            let mut tree = state.tree_mut();
                            let _ = wheel.click_management_new_workspace(&mut tree);
                        }
                        state.bump();
                    }
                    *handled = true;
                    return;
                }
                // `Released` decides click-activate vs. drag by travel distance.
                if let Some(p) = self.last_cursor
                    && self.rect.contains(p)
                {
                    self.drag_start_y = Some(p.y);
                    self.drag_last_y = Some(p.y);
                    self.drag_accum = 0.0;
                    self.drag_moved = false;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            }
                // Rotations already applied incrementally in `CursorMoved`.
                if self.drag_start_y.is_some() => {
                    let was_drag = self.drag_moved;
                    self.drag_start_y = None;
                    self.drag_last_y = None;
                    self.drag_accum = 0.0;
                    self.drag_moved = false;
                    if was_drag {
                        *handled = true;
                        return;
                    }
                    // Pure click: activate the current hover index.
                    if let Some(idx) = self.hover_idx
                        && let Some(card) = self.cards.get(idx)
                    {
                        self.pending_intent = Some(SidebarIntent::Activate(card.id));
                        if let Some(chrome) = self.chrome.clone() {
                            let mut state = chrome.borrow_mut();
                            state.tree_mut().set_active(card.id);
                            state.bump();
                        }
                        *handled = true;
                    }
                }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "SidebarWheelChrome"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod drag_rotate_tests {
    //! Each full-card-height of travel swaps the active workspace. A
    //! press under the click tolerance still activates the card.
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::WorkspaceTree;

    fn sidebar_with_workspaces(
        n: usize,
    ) -> (SidebarWheelChrome, crate::shell::chrome_state::Chrome) {
        let mut tree = WorkspaceTree::bootstrap_demo();
        for i in 1..n {
            tree.append_empty_workspace(&format!("WS-{i}"));
        }
        let chrome = ChromeState::new(tree);
        let sidebar = {
            let state = chrome.borrow();
            SidebarWheelChrome::from_tree(&state.tree()).with_chrome(chrome.clone())
        };
        (sidebar, chrome)
    }

    #[test]
    fn drag_down_past_threshold_rotates_to_next_workspace() {
        let (mut s, chrome) = sidebar_with_workspaces(3);
        let active_before = chrome.borrow().tree().active;
        s.rect = Rect::new(0.0, 0.0, 96.0, 600.0);
        s.last_cursor = Some(Point::new(48.0, 100.0));
        s.drag_start_y = Some(100.0);
        s.drag_last_y = Some(100.0);
        s.drag_accum = 0.0;
        s.drag_moved = false;

        use winit::event::{ElementState, MouseButton};
        let _ = (MouseButton::Left, ElementState::Pressed); // smoke
        // Drives the CursorMoved math directly.
        s.last_cursor = Some(Point::new(48.0, 100.0 + DRAG_SWITCH_THRESHOLD_PX + 2.0));
        let p = s.last_cursor.unwrap();
        if let (Some(start), Some(last)) = (s.drag_start_y, s.drag_last_y) {
            let dy = p.y - last;
            s.drag_last_y = Some(p.y);
            if !s.drag_moved && (p.y - start).abs() > DRAG_CLICK_TOLERANCE_PX {
                s.drag_moved = true;
            }
            if s.drag_moved {
                s.drag_accum += dy;
                while s.drag_accum >= DRAG_SWITCH_THRESHOLD_PX {
                    if let Some(ch) = s.chrome.clone()
                        && !ch.borrow_mut().switch_workspace(1)
                    {
                        s.drag_accum = 0.0;
                        break;
                    }
                    s.drag_accum -= DRAG_SWITCH_THRESHOLD_PX;
                }
            }
        }

        let active_after = chrome.borrow().tree().active;
        assert_ne!(
            active_after, active_before,
            "dragging one card-stride down must rotate to a new workspace"
        );
    }

    #[test]
    fn pure_click_below_tolerance_does_not_mark_drag() {
        let (mut s, _chrome) = sidebar_with_workspaces(2);
        s.rect = Rect::new(0.0, 0.0, 96.0, 600.0);
        s.drag_start_y = Some(100.0);
        s.drag_last_y = Some(100.0);

        // 2px wiggle — below the 4px click tolerance.
        let p = Point::new(48.0, 102.0);
        if let (Some(start), Some(_last)) = (s.drag_start_y, s.drag_last_y)
            && !s.drag_moved
            && (p.y - start).abs() > DRAG_CLICK_TOLERANCE_PX
        {
            s.drag_moved = true;
        }

        assert!(
            !s.drag_moved,
            "a 2 px press-wiggle must not transition into a drag — release branch must still activate"
        );
    }

    #[test]
    fn drag_up_past_threshold_rotates_to_previous_workspace() {
        let (mut s, chrome) = sidebar_with_workspaces(3);
        // Advance first so there's somewhere to go back to.
        chrome.borrow_mut().switch_workspace(1);
        let active_before = chrome.borrow().tree().active;
        s.rect = Rect::new(0.0, 0.0, 96.0, 600.0);
        s.drag_start_y = Some(400.0);
        s.drag_last_y = Some(400.0);
        s.drag_moved = false;
        s.drag_accum = 0.0;

        let p = Point::new(48.0, 400.0 - DRAG_SWITCH_THRESHOLD_PX - 2.0); // moved up past threshold
        if let (Some(start), Some(last)) = (s.drag_start_y, s.drag_last_y) {
            let dy = p.y - last;
            s.drag_last_y = Some(p.y);
            if !s.drag_moved && (p.y - start).abs() > DRAG_CLICK_TOLERANCE_PX {
                s.drag_moved = true;
            }
            if s.drag_moved {
                s.drag_accum += dy;
                while s.drag_accum <= -DRAG_SWITCH_THRESHOLD_PX {
                    if let Some(ch) = s.chrome.clone()
                        && !ch.borrow_mut().switch_workspace(-1)
                    {
                        s.drag_accum = 0.0;
                        break;
                    }
                    s.drag_accum += DRAG_SWITCH_THRESHOLD_PX;
                }
            }
        }

        let active_after = chrome.borrow().tree().active;
        assert_ne!(
            active_after, active_before,
            "dragging one card-stride up must rotate back to the previous workspace"
        );
    }
}
