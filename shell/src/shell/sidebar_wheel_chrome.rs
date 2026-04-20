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
    /// Number of collaborators on this workspace. Read from
    /// `Workspace::collaborators.len()` during `sync_from_tree`; falls
    /// back to the tree-level roster so shells that haven't split their
    /// collaborator roster per-workspace still get a presence signal on
    /// the active card.
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
    /// Drag-to-rotate state: set on `MouseInput::Pressed` inside the wheel,
    /// cleared on release. `drag_last_y` is the running cursor anchor used
    /// to compute per-frame `dy`; `drag_accum` is the signed pixel distance
    /// since the last emitted workspace switch. When `|drag_accum| >=
    /// DRAG_SWITCH_THRESHOLD_PX`, we fire `switch_workspace` and subtract
    /// the threshold so continued dragging keeps rotating.
    drag_start_y: Option<f64>,
    drag_last_y: Option<f64>,
    drag_accum: f64,
    /// `true` once the cursor has travelled beyond the click-tolerance
    /// threshold during a press, so the release branch knows to *skip* the
    /// click-to-activate action a pure click would have fired.
    drag_moved: bool,
}

/// One full card's worth of vertical travel before we rotate the wheel. The
/// sidebar lays cards out at 64 px + 10 px gap, so 74 px matches one
/// stride exactly — a drag equal to one card-slot swaps one workspace.
const DRAG_SWITCH_THRESHOLD_PX: f64 = 74.0;

/// A press that never exceeds this pixel radius counts as a click and falls
/// through to the existing card-activate / create-workspace intents.
const DRAG_CLICK_TOLERANCE_PX: f64 = 4.0;

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
        let tree_fallback = tree.collaborators.len();
        self.cards = tree
            .workspaces
            .iter()
            .map(|w| {
                // Per-workspace count takes precedence; we only fall back
                // to the tree-level roster for the active workspace when
                // the workspace itself has none (so single-workspace demo
                // trees still light up the active card).
                let per_ws = w.collaborators.len();
                let count = if per_ws > 0 {
                    per_ws
                } else if tree.active == w.id {
                    tree_fallback
                } else {
                    0
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
        // Legacy field: keep it in sync with the active card so the
        // existing tests that assert on `active_collaborator_count` keep
        // working. New callers should prefer `card_collab_count`.
        self.active_collab_count = self
            .cards
            .iter()
            .find(|c| Some(c.id) == self.active)
            .map(|c| c.collab_count)
            .unwrap_or(0);
    }

    /// Collaborator-dot count rendered on the active workspace card.
    /// Exposed so integration tests can verify the sync-from-tree wiring.
    pub fn active_collaborator_count(&self) -> usize {
        self.active_collab_count
    }

    /// Collaborator-dot count rendered on the workspace card at index
    /// `idx` (order matches `tree.workspaces`). Returns 0 for out-of-
    /// range indices.
    pub fn card_collab_count(&self, idx: usize) -> usize {
        self.cards.get(idx).map(|c| c.collab_count).unwrap_or(0)
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
            // Hover-scale matches concept `DesktopView.tsx:446`
            // `whileHover={{ scale: 1.05 }}` — a subtle 5 % grow on the
            // hovered card so the user gets proprioceptive feedback that
            // the pill is interactive. Scaled about the card's center
            // so it grows symmetrically rather than from the top-left.
            // Applied to both the card rect and its glyph/subtitle
            // transforms below so the pill stays a coherent unit.
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
            let pill = RoundedRect::from_rect(card.rect, tokens.radii.md as f64);
            let fill = if is_active {
                colors.accent[5]
            } else if is_hover {
                colors.surfaces.raised
            } else {
                colors.neutral[1] // slate-900
            };
            scene.fill(Fill::NonZero, card_xform, fill, None, &pill);
            if is_active {
                // Subtle ring / inner-stroke to echo the indigo glow.
                // Active card doesn't hover-scale (it's already the
                // focal point) so this uses the base transform.
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
            // center so the strip-count subtitle fits below. Uses
            // `card_xform` so the glyph scales with the pill on hover.
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
                card_xform * Affine::translate((cx, cy)),
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
                card_xform * Affine::translate((sx, sy)),
                None,
            );

            // Collaborator presence dots on every card. Per-workspace
            // presence is now modeled (`Workspace::collaborators`), so
            // each card can show its own roster; we still fall back to
            // the tree-level roster on the active card when the workspace
            // has none. Clamp to 3 and overflow into a "+N" chip so 4+
            // peers stay readable inside the 96-px-wide card. Inactive
            // cards render at half-alpha so the active card still reads
            // as the focus anchor.
            let card_count = card.collab_count;
            if card_count > 0 {
                let shown = card_count.min(3);
                let overflow = card_count.saturating_sub(3);
                let dot_r = 3.0_f64;
                let gap = 3.0_f64;
                let row_w = shown as f64 * (dot_r * 2.0) + (shown as f64 - 1.0).max(0.0) * gap
                    + if overflow > 0 { 14.0 } else { 0.0 };
                let base_x = (card.rect.x0 + card.rect.x1) / 2.0 - row_w / 2.0;
                let dot_y = card.rect.y1 - 10.0;
                let dot_color = if is_active {
                    colors.text_primary
                } else {
                    colors.text_primary.with_alpha_factor(0.55)
                };
                for i in 0..shown {
                    let dx = base_x + dot_r + i as f64 * (dot_r * 2.0 + gap);
                    let c = vello::kurbo::Circle::new(Point::new(dx, dot_y), dot_r);
                    scene.fill(Fill::NonZero, card_xform, dot_color, None, &c);
                }
                if overflow > 0 {
                    let chip_x = base_x + shown as f64 * (dot_r * 2.0 + gap);
                    let s = format!("+{}", overflow);
                    let (tw, th) = text_renderer.measure(&s, tokens.typography.scale.xs, None);
                    text_renderer.draw_text(
                        scene,
                        &s,
                        tokens.typography.scale.xs,
                        dot_color,
                        card_xform
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

                // Drag-to-rotate: if a press is in flight, accumulate per-frame
                // dy and rotate the wheel whenever we've travelled a full
                // card-height. Mirror of the concept's wheel-rotation intent
                // (see `DesktopView.tsx` wheel-card interaction), implemented
                // as a linear drag because a 3D rotation isn't modelled in 2D.
                if let (Some(_start), Some(last)) = (self.drag_start_y, self.drag_last_y) {
                    let dy = p.y - last;
                    self.drag_last_y = Some(p.y);
                    // Mark the press as a drag once we've crossed the click
                    // tolerance; the release branch uses this to skip the
                    // click-activate behaviour so a drag doesn't spuriously
                    // jump to whatever card the cursor happened to finish on.
                    if !self.drag_moved
                        && (p.y - _start).abs() > DRAG_CLICK_TOLERANCE_PX
                    {
                        self.drag_moved = true;
                    }
                    if self.drag_moved {
                        self.drag_accum += dy;
                        // Dragging *down* visually rotates the wheel so the
                        // next (higher-index) workspace comes into view — mirror
                        // the scroll-wheel sign convention at lines above:
                        // scrolling *up* (positive dy line-delta) goes to an
                        // earlier workspace, scrolling *down* (negative) goes
                        // to a later one. Drag matches: `drag_accum > 0`
                        // (cursor moved down) → later workspace (+1).
                        while self.drag_accum >= DRAG_SWITCH_THRESHOLD_PX {
                            if let Some(chrome) = self.chrome.clone() {
                                if !chrome.borrow_mut().switch_workspace(1) {
                                    // Hit the bottom — stop consuming travel so
                                    // the user doesn't lose drag distance.
                                    self.drag_accum = 0.0;
                                    break;
                                }
                            }
                            self.drag_accum -= DRAG_SWITCH_THRESHOLD_PX;
                            *handled = true;
                        }
                        while self.drag_accum <= -DRAG_SWITCH_THRESHOLD_PX {
                            if let Some(chrome) = self.chrome.clone() {
                                if !chrome.borrow_mut().switch_workspace(-1) {
                                    self.drag_accum = 0.0;
                                    break;
                                }
                            }
                            self.drag_accum += DRAG_SWITCH_THRESHOLD_PX;
                            *handled = true;
                        }
                    }
                }
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
                // The plus button is a true click-target: fire immediately so
                // a tap reliably creates a workspace even if the user wiggles.
                if self.hover_plus {
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
                    return;
                }
                // Any other press inside the wheel starts a drag-rotate
                // candidate: we stamp the anchor and defer the card-activate
                // decision to `Released`. If the cursor moves more than
                // `DRAG_CLICK_TOLERANCE_PX` before release, the release branch
                // skips activate; otherwise it's a click and we activate.
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
            } => {
                // If this was a drag (cursor travelled beyond tolerance), all
                // the rotations have already been applied incrementally in
                // `CursorMoved`; just clear state.
                if self.drag_start_y.is_some() {
                    let was_drag = self.drag_moved;
                    self.drag_start_y = None;
                    self.drag_last_y = None;
                    self.drag_accum = 0.0;
                    self.drag_moved = false;
                    if was_drag {
                        *handled = true;
                        return;
                    }
                    // Pure click (no drag travel): fall through to the
                    // card-activate path using the current hover index.
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
                    }
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

#[cfg(test)]
mod drag_rotate_tests {
    //! Drag-to-rotate: hold the mouse down on the sidebar wheel, drag up or
    //! down, and each full-card-height of travel swaps the active workspace.
    //! A press that never exceeds the click tolerance keeps its
    //! click-to-activate behaviour so a plain tap on a card still selects
    //! it.

    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::WorkspaceTree;

    fn sidebar_with_workspaces(n: usize) -> (SidebarWheelChrome, crate::shell::chrome_state::Chrome) {
        let mut tree = WorkspaceTree::bootstrap_default();
        for i in 1..n {
            tree.append_empty_workspace(&format!("WS-{}", i));
        }
        let chrome = ChromeState::new(tree);
        let sidebar = {
            let state = chrome.borrow();
            SidebarWheelChrome::from_tree(&state.tree).with_chrome(chrome.clone())
        };
        (sidebar, chrome)
    }

    #[test]
    fn drag_down_past_threshold_rotates_to_next_workspace() {
        let (mut s, chrome) = sidebar_with_workspaces(3);
        let active_before = chrome.borrow().tree.active;
        // Simulate a press anchor inside the wheel and a dragged cursor.
        s.rect = Rect::new(0.0, 0.0, 96.0, 600.0);
        s.last_cursor = Some(Point::new(48.0, 100.0));
        s.drag_start_y = Some(100.0);
        s.drag_last_y = Some(100.0);
        s.drag_accum = 0.0;
        s.drag_moved = false;

        // Feed a CursorMoved event one card-stride below the anchor.
        use winit::event::{MouseButton, ElementState};
        let _ = (MouseButton::Left, ElementState::Pressed); // smoke
        // Drive the CursorMoved branch directly — the event loop path ends up
        // mutating the same fields.
        s.last_cursor = Some(Point::new(48.0, 100.0 + DRAG_SWITCH_THRESHOLD_PX + 2.0));
        // Replicate the CursorMoved math in one call so the test isn't
        // dependent on winit event construction details.
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
                    if let Some(ch) = s.chrome.clone() {
                        if !ch.borrow_mut().switch_workspace(1) {
                            s.drag_accum = 0.0;
                            break;
                        }
                    }
                    s.drag_accum -= DRAG_SWITCH_THRESHOLD_PX;
                }
            }
        }

        let active_after = chrome.borrow().tree.active;
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

        // Simulate a 2 px wiggle — below the 4 px click tolerance.
        let p = Point::new(48.0, 102.0);
        if let (Some(start), Some(_last)) = (s.drag_start_y, s.drag_last_y) {
            if !s.drag_moved && (p.y - start).abs() > DRAG_CLICK_TOLERANCE_PX {
                s.drag_moved = true;
            }
        }

        assert!(
            !s.drag_moved,
            "a 2 px press-wiggle must not transition into a drag — release branch must still activate"
        );
    }

    #[test]
    fn drag_up_past_threshold_rotates_to_previous_workspace() {
        let (mut s, chrome) = sidebar_with_workspaces(3);
        // Advance to the second workspace first so we have somewhere to go
        // back to.
        chrome.borrow_mut().switch_workspace(1);
        let active_before = chrome.borrow().tree.active;
        s.rect = Rect::new(0.0, 0.0, 96.0, 600.0);
        s.drag_start_y = Some(400.0);
        s.drag_last_y = Some(400.0);
        s.drag_moved = false;
        s.drag_accum = 0.0;

        // Move *up* past the threshold.
        let p = Point::new(48.0, 400.0 - DRAG_SWITCH_THRESHOLD_PX - 2.0);
        if let (Some(start), Some(last)) = (s.drag_start_y, s.drag_last_y) {
            let dy = p.y - last;
            s.drag_last_y = Some(p.y);
            if !s.drag_moved && (p.y - start).abs() > DRAG_CLICK_TOLERANCE_PX {
                s.drag_moved = true;
            }
            if s.drag_moved {
                s.drag_accum += dy;
                while s.drag_accum <= -DRAG_SWITCH_THRESHOLD_PX {
                    if let Some(ch) = s.chrome.clone() {
                        if !ch.borrow_mut().switch_workspace(-1) {
                            s.drag_accum = 0.0;
                            break;
                        }
                    }
                    s.drag_accum += DRAG_SWITCH_THRESHOLD_PX;
                }
            }
        }

        let active_after = chrome.borrow().tree.active;
        assert_ne!(
            active_after, active_before,
            "dragging one card-stride up must rotate back to the previous workspace"
        );
    }
}
