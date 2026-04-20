//! Left-edge workspace wheel chrome widget.
//!
//! Paints a 96 px wide vertical strip of workspace cards. The active
//! workspace's card is highlighted with accent-500 fill; inactive cards
//! render as raised-surface pills with a dim label. A trailing "+" card
//! is reserved for creating a new workspace (click wiring lands together
//! with the topbar-click plumbing).
//!
//! Layout is 100 % height under the top bar — the shell splits the
//! below-top-bar area horizontally into `[SidebarWheelChrome | content]`.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, PathEl, Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::event::WindowEvent;

use quick_ui::presence::PresenceCtx;
use quick_ui::text_render::TextRenderer;
use quick_ui::theme::Theme;
use quick_ui::widget::Widget;

use crate::shell::chrome_state::Chrome;
use crate::shell::sidebar_wheel::SidebarWheel;
use crate::shell::state::{WorkspaceId, WorkspaceTree};

pub const SIDEBAR_WIDTH_PX: f32 = 96.0;

/// Intents the sidebar wheel can emit. Held internally so the shell can
/// poll for them after dispatching events, mirroring the topbar's
/// click-output pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarIntent {
    /// User clicked the card for workspace `id` — switch to it.
    Activate(WorkspaceId),
    /// User clicked the trailing "+" card — create a new workspace.
    CreateWorkspace,
}

/// Snapshot of a single workspace card.
#[derive(Clone, Debug)]
struct WorkspaceCard {
    id: WorkspaceId,
    label: String,
    strip_count: usize,
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
    /// Shared chrome state; activate/create requests mutate the tree here.
    chrome: Option<Chrome>,
    /// Cached revision for the re-sync loop.
    synced_revision: u64,
    /// Accumulated vertical wheel delta (in line-units) since the last
    /// workspace switch. Crossing ±1 triggers a switch and resets to 0.
    scroll_accum: f32,
    /// Last cursor position in logical coords — used to gate wheel events
    /// so scrolling only cycles workspaces when the pointer is over the
    /// sidebar.
    last_cursor: Option<Point>,
    /// How many collaborators are present on the active workspace. Today we
    /// don't track per-workspace presence yet, so we treat tree-level
    /// collaborators as "everyone is on the active workspace" — good enough
    /// for the visual parity signal "this card has people on it". Clamped
    /// to 3 when drawn so the chrome doesn't overflow the card.
    active_collab_count: usize,
}

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
        self.cards = tree
            .workspaces
            .iter()
            .map(|w| WorkspaceCard {
                id: w.id,
                label: workspace_initial(&w.name),
                strip_count: w.strips.len(),
                rect: Rect::ZERO,
            })
            .collect();
        self.active_collab_count = tree.collaborators.len();
    }

    /// Collaborator-dot count rendered on the active workspace card.
    /// Exposed so integration tests can verify the sync-from-tree wiring.
    pub fn active_collaborator_count(&self) -> usize {
        self.active_collab_count
    }

    /// Drain any pending intent queued by a previous click. Shell calls this
    /// each frame to forward to its state mutation handlers.
    pub fn take_intent(&mut self) -> Option<SidebarIntent> {
        self.pending_intent.take()
    }
}

fn workspace_initial(name: &str) -> String {
    name.chars().next().map(|c| c.to_ascii_uppercase().to_string()).unwrap_or_else(|| "·".to_string())
}

impl Widget for SidebarWheelChrome {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool {
        if let Some(chrome) = self.chrome.clone() {
            let rev = chrome.borrow().revision;
            if rev != self.synced_revision {
                let tree_snap = chrome.borrow().tree.clone();
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
        // Place cards vertically inside the strip.
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
        // Trailing "+" card, same metrics as the regular ones.
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

        // Column background: slightly darker than the workspace canvas so
        // the wheel reads as a distinct chrome column (concept: slate-950
        // under slate-900 content).
        scene.fill(Fill::NonZero, transform, colors.neutral[0], None, &self.rect);

        // Right edge divider line.
        let divider = Rect::new(self.rect.x1 - 1.0, self.rect.y0, self.rect.x1, self.rect.y1);
        scene.fill(Fill::NonZero, transform, colors.border_standard, None, &divider);

        // Rim / gradient line on expand — concept fades workspace cards out
        // at the top and bottom of the wheel so they appear to slip into /
        // out of view. We approximate the CSS `mask-image: linear-gradient`
        // with stepped alpha strips at each edge. The top strip is opaque
        // near the edge and fades transparent downward (stacking
        // `neutral[0]` at decreasing alpha produces the appearance of cards
        // being "eaten" by the chrome); the bottom strip mirrors it.
        let fade_h = 36.0_f64;
        let steps = 8_usize;
        let step_h = fade_h / steps as f64;
        let bg = colors.neutral[0];
        for i in 0..steps {
            // Top: near the edge = high alpha → low alpha further down.
            let alpha = 1.0 - (i as f32) / (steps as f32);
            let top = Rect::new(
                self.rect.x0,
                self.rect.y0 + step_h * i as f64,
                self.rect.x1 - 1.0,
                self.rect.y0 + step_h * (i + 1) as f64,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                bg.with_alpha_factor(alpha * 0.6),
                None,
                &top,
            );
            let bottom = Rect::new(
                self.rect.x0,
                self.rect.y1 - step_h * (i + 1) as f64,
                self.rect.x1 - 1.0,
                self.rect.y1 - step_h * i as f64,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                bg.with_alpha_factor(alpha * 0.6),
                None,
                &bottom,
            );
        }
        // Hairline accent rim at the very top and bottom — a 1 px indigo
        // line at low alpha, echoing the concept's focus-glow trim.
        let top_rim = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1 - 1.0, self.rect.y0 + 1.0);
        let bot_rim = Rect::new(
            self.rect.x0,
            self.rect.y1 - 1.0,
            self.rect.x1 - 1.0,
            self.rect.y1,
        );
        let rim = colors.accent[5].with_alpha_factor(0.35);
        scene.fill(Fill::NonZero, transform, rim, None, &top_rim);
        scene.fill(Fill::NonZero, transform, rim, None, &bot_rim);

        // Draw each workspace card.
        for (idx, card) in self.cards.iter().enumerate() {
            let is_active = self.active == Some(card.id);
            let is_hover = self.hover_idx == Some(idx);
            let pill = RoundedRect::from_rect(card.rect, tokens.radii.md as f64);
            let fill = if is_active {
                colors.accent[5]
            } else if is_hover {
                colors.surfaces.raised
            } else {
                colors.neutral[1] // slate-900
            };
            scene.fill(Fill::NonZero, transform, fill, None, &pill);
            if is_active {
                // Subtle ring / inner-stroke to echo the indigo glow.
                let ring = RoundedRect::from_rect(
                    Rect::new(
                        card.rect.x0 - 1.0,
                        card.rect.y0 - 1.0,
                        card.rect.x1 + 1.0,
                        card.rect.y1 + 1.0,
                    ),
                    (tokens.radii.md + 1.0) as f64,
                );
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.5),
                    transform,
                    colors.accent[4],
                    None,
                    &ring,
                );
            }

            // Big initial glyph, centered horizontally + slightly above
            // center so the strip-count subtitle fits below.
            let glyph_size = tokens.typography.scale.xl;
            let (gw, gh) = text_renderer.measure(&card.label, glyph_size, None);
            let cx = (card.rect.x0 + card.rect.x1) / 2.0 - gw as f64 / 2.0;
            let cy = card.rect.y0 + card.rect.height() * 0.30 - gh as f64 / 2.0;
            let glyph_color = if is_active { colors.text_primary } else { colors.text_primary };
            text_renderer.draw_text(
                scene,
                &card.label,
                glyph_size,
                glyph_color,
                transform * Affine::translate((cx, cy)),
                None,
            );

            // Subtitle: "{n} strip" / "{n} strips".
            let subtitle = if card.strip_count == 1 {
                "1 strip".to_string()
            } else {
                format!("{} strips", card.strip_count)
            };
            let sub_size = tokens.typography.scale.xs;
            let (sw, sh) = text_renderer.measure(&subtitle, sub_size, None);
            let sx = (card.rect.x0 + card.rect.x1) / 2.0 - sw as f64 / 2.0;
            let sy = card.rect.y0 + card.rect.height() * 0.72 - sh as f64 / 2.0;
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
                transform * Affine::translate((sx, sy)),
                None,
            );

            // Collaborator presence dots on the active card. Per-workspace
            // presence isn't modeled yet, so tree-level collaborators all
            // attribute to the active workspace as a "people are here"
            // signal. Clamp to 3 and overflow into a "+N" chip so 4+ peers
            // stay readable inside the 96-px-wide card.
            if is_active && self.active_collab_count > 0 {
                let shown = self.active_collab_count.min(3);
                let overflow = self.active_collab_count.saturating_sub(3);
                let dot_r = 3.0_f64;
                let gap = 3.0_f64;
                let row_w = shown as f64 * (dot_r * 2.0) + (shown as f64 - 1.0).max(0.0) * gap
                    + if overflow > 0 { 14.0 } else { 0.0 };
                let base_x = (card.rect.x0 + card.rect.x1) / 2.0 - row_w / 2.0;
                let dot_y = card.rect.y1 - 10.0;
                for i in 0..shown {
                    let dx = base_x + dot_r + i as f64 * (dot_r * 2.0 + gap);
                    let c = vello::kurbo::Circle::new(Point::new(dx, dot_y), dot_r);
                    scene.fill(Fill::NonZero, transform, colors.text_primary, None, &c);
                }
                if overflow > 0 {
                    let chip_x = base_x + shown as f64 * (dot_r * 2.0 + gap);
                    let s = format!("+{}", overflow);
                    let (tw, th) = text_renderer.measure(&s, tokens.typography.scale.xs, None);
                    text_renderer.draw_text(
                        scene,
                        &s,
                        tokens.typography.scale.xs,
                        colors.text_primary,
                        transform
                            * Affine::translate((chip_x, dot_y - th as f64 / 2.0)),
                        None,
                    );
                    let _ = tw; // silence unused: we just want ascent-based y alignment
                }
            }
        }

        // Trailing "+" card — dashed outline so it reads as a hint.
        let plus_fill = if self.hover_plus { colors.surfaces.raised } else { colors.neutral[0] };
        let plus_pill = RoundedRect::from_rect(self.plus_rect, tokens.radii.md as f64);
        scene.fill(Fill::NonZero, transform, plus_fill, None, &plus_pill);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0).with_dashes(0.0, [4.0, 4.0]),
            transform,
            colors.border_standard,
            None,
            &plus_pill,
        );
        let cx = (self.plus_rect.x0 + self.plus_rect.x1) / 2.0;
        let cy = (self.plus_rect.y0 + self.plus_rect.y1) / 2.0;
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
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Only cycle workspaces when the pointer is over the
                // sidebar — scrolling over window content should go to
                // the focused tile, not steal workspace switching.
                let over_sidebar = self
                    .last_cursor
                    .map(|p| self.rect.contains(p))
                    .unwrap_or(false);
                if !over_sidebar {
                    return;
                }
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => *y,
                    winit::event::MouseScrollDelta::PixelDelta(p) => (p.y / 40.0) as f32,
                };
                self.scroll_accum += dy;
                while self.scroll_accum >= 1.0 {
                    if let Some(chrome) = self.chrome.clone() {
                        if !chrome.borrow_mut().switch_workspace(-1) {
                            break;
                        }
                    }
                    self.scroll_accum -= 1.0;
                    *handled = true;
                }
                while self.scroll_accum <= -1.0 {
                    if let Some(chrome) = self.chrome.clone() {
                        if !chrome.borrow_mut().switch_workspace(1) {
                            break;
                        }
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
                if let Some(idx) = self.hover_idx
                    && let Some(card) = self.cards.get(idx)
                {
                    self.pending_intent = Some(SidebarIntent::Activate(card.id));
                    if let Some(chrome) = self.chrome.clone() {
                        let mut state = chrome.borrow_mut();
                        state.tree.set_active(card.id);
                        state.bump();
                    }
                    *handled = true;
                } else if self.hover_plus {
                    self.pending_intent = Some(SidebarIntent::CreateWorkspace);
                    if let Some(chrome) = self.chrome.clone() {
                        let mut state = chrome.borrow_mut();
                        // Reuse SidebarWheel's commit_new_workspace helper
                        // so we share the "append + activate + seed Main
                        // strip" semantics with the overscroll-gesture path.
                        let wheel = SidebarWheel::new();
                        let _ = wheel.click_management_new_workspace(&mut state.tree);
                        state.bump();
                    }
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "SidebarWheelChrome"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
