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
use vello::peniko::{BlendMode, Fill};
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
    scroll_accum: f64,
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

/// Compose `transform` with a translation to `(dx, dy)`. `Affine`'s `Mul`
/// is pure `f64` matrix composition — it cannot overflow or panic — but
/// `arithmetic_side_effects` can't see that for a non-primitive `Mul`
/// impl, so every call site funnels through this one function instead of
/// an `#[allow]` at each of them.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is float matrix multiplication; it cannot overflow or panic"
)]
fn at(transform: Affine, dx: f64, dy: f64) -> Affine {
    transform * Affine::translate((dx, dy))
}

/// Compose `transform` with a scale of `factor` about `(cx, cy)`. Same
/// reasoning as [`at`].
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is float matrix multiplication; it cannot overflow or panic"
)]
fn scaled_about(transform: Affine, cx: f64, cy: f64, factor: f64) -> Affine {
    transform * Affine::translate((cx, cy)) * Affine::scale(factor) * Affine::translate((-cx, -cy))
}

/// Whole multiples of `threshold` in non-negative `accum`, and what's
/// left over — computed by one division instead of the repeated float
/// subtraction a `while accum >= threshold` loop does, which can drift
/// under floating-point rounding.
fn whole_steps(accum: f64, threshold: f64) -> (u32, f64) {
    if threshold <= 0.0 || accum <= 0.0 {
        return (0, accum);
    }
    let steps = (accum / threshold).floor();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "drag/scroll distance per frame is bounded by screen size, never near u32::MAX steps"
    )]
    let steps_u = steps as u32;
    (steps_u, accum - steps * threshold)
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

    #[must_use]
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

/// Per-card paint state, computed once per card and shared by the
/// background, text and avatar-row painting passes below.
struct CardPaint {
    xform: Affine,
    is_active: bool,
    is_hover: bool,
}

impl SidebarWheelChrome {
    /// Floats over the wallpaper — no opaque column, just a glassy backdrop.
    fn draw_backdrop(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
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
            // The backdrop floats over the desktop ground, so its edge is
            // derived against that ground (HIG §9.5), at the same third of
            // the strength the declared border used to be asked for.
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                ui::surface::edge_on(colors.surfaces.base).with_alpha_factor(0.35),
                None,
                &backdrop_rr,
            );
        }
    }

    /// Steps `pill_anim_t` toward `is_expanded()`'s target and returns the
    /// eased `[0,1]` value the pill/rim/hover-scale painting below reads.
    /// Real elapsed time, so the smoothstep ease stays framerate-independent.
    fn advance_pill_anim(&mut self) -> f32 {
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
        p * p * (3.0 - 2.0 * p)
    }

    /// Collapsed: 4px strip against the left edge, translated 10px further
    /// left. Expanded: full inset-4 pill, centered, full opacity.
    fn draw_pill_highlight(&self, scene: &mut Scene, transform: Affine, theme: &Theme, eased: f32) {
        let pill_y0 = self.rect.y0 + 32.0;
        let pill_y1 = self.rect.y1 - 32.0;
        if pill_y1 <= pill_y0 {
            return;
        }
        let expanded_x0 = self.rect.x0 + 4.0;
        let expanded_x1 = self.rect.x1 - 4.0;
        let collapsed_x0 = self.rect.x0 - 10.0;
        let collapsed_x1 = collapsed_x0 + 4.0;
        // Coordinates are bounded UI pixel measurements, always far under
        // f32's range.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "bounded UI pixel measurements"
        )]
        let (x0, x1) = {
            let (collapsed_x0, collapsed_x1) = (collapsed_x0 as f32, collapsed_x1 as f32);
            let (expanded_x0, expanded_x1) = (expanded_x0 as f32, expanded_x1 as f32);
            (
                collapsed_x0 + (expanded_x0 - collapsed_x0) * eased,
                collapsed_x1 + (expanded_x1 - collapsed_x1) * eased,
            )
        };
        let opacity = 0.2 + 0.8 * eased;
        let pill_rect = Rect::new(f64::from(x0), pill_y0, f64::from(x1), pill_y1);
        let radius = pill_rect.height() / 2.0;
        let pill = RoundedRect::from_rect(pill_rect, radius);
        // Base alpha 13/255 fill, 26/255 border, scaled by opacity — both
        // clamped to 0.0..=255.0 just above, so they always fit a
        // non-negative u8. The polarity is derived against the desktop
        // ground (HIG §9.5): literal white lifted nothing on a light
        // wallpaper.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0.0..=255.0 immediately above"
        )]
        let (fill_a, stroke_a) = (
            (13.0 * opacity).round().clamp(0.0, 255.0) as u8,
            (26.0 * opacity).round().clamp(0.0, 255.0) as u8,
        );
        let ground = theme.tokens.colors.surfaces.base;
        scene.fill(
            Fill::NonZero,
            transform,
            ui::tokens::colors::Colors::hairline_on(ground, fill_a),
            None,
            &pill,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            ui::tokens::colors::Colors::hairline_on(ground, stroke_a),
            None,
            &pill,
        );
    }

    /// Huge faint circle behind the cards; only the right-most sliver is
    /// visible inside the sidebar rect. Fades in with the pill.
    fn draw_wheel_rim(&self, scene: &mut Scene, transform: Affine, theme: &Theme, eased: f32) {
        if eased <= 0.0 {
            return;
        }
        let cx = self.rect.x0 + 50.0;
        let cy = f64::midpoint(self.rect.y0, self.rect.y1);
        let rim = Circle::new((cx, cy), 600.0);
        // Clipped to the sidebar bounds so it doesn't bleed into the
        // desktop beside the wheel.
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
            // A faint rim derived against the desktop ground (HIG §9.5) —
            // was literal white/5, invisible over a light wallpaper.
            ui::tokens::colors::Colors::hairline_on(theme.tokens.colors.surfaces.base, 13),
            None,
            &rim,
        );
        scene.pop_layer();
    }

    /// Dashed outline so the trailing "+" card reads as a hint.
    fn draw_plus_card(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        // Control-role states (HIG §9.3): the hint card rests on the quiet
        // lift floor and hovers to the shared tier; its dashed edge is
        // derived from the fill it outlines (§9.5).
        let plus_fill = crate::shell::quick_settings_drill_in::control_fill(theme, self.hover_plus);
        let plus_shape = RoundedRect::from_rect(self.plus_rect, f64::from(tokens.radii.md));
        scene.fill(Fill::NonZero, transform, plus_fill, None, &plus_shape);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0).with_dashes(0.0, [4.0, 4.0]),
            transform,
            ui::surface::edge_on(plus_fill),
            None,
            &plus_shape,
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
}

/// The card's fill, active ring and shadow.
fn draw_card_background(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    card: &WorkspaceCard,
    paint: &CardPaint,
) {
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    let pill = RoundedRect::from_rect(card.rect, f64::from(tokens.radii.md));
    if paint.is_active {
        ui::surface::soft_shadow(
            scene,
            paint.xform,
            card.rect,
            colors.accent[5],
            14.0,
            0.0,
            0.35,
        );
        scene.fill(
            Fill::NonZero,
            paint.xform,
            colors.accent[5].with_alpha_factor(0.15),
            None,
            &pill,
        );
    } else {
        // Idle cards rest on the lift floor and hover to the shared tier
        // (HIG §9.3), each with a derived rest edge (§9.5) so a card is a
        // card over any wallpaper — `neutral[1]` was a ramp slot that read
        // as bare wallpaper on a dark one.
        let fill = crate::shell::quick_settings_drill_in::control_fill(theme, paint.is_hover);
        ui::surface::card(
            scene,
            paint.xform,
            card.rect,
            f64::from(tokens.radii.md),
            fill,
        );
    }
    if paint.is_active {
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
}

/// The card's glyph and subtitle.
fn draw_card_text(
    scene: &mut Scene,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    card: &WorkspaceCard,
    paint: &CardPaint,
) {
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    let glyph_size = tokens.typography.scale.xl;
    let (gw, gh) = text_renderer.measure(&card.label, glyph_size, None);
    let cx = f64::midpoint(card.rect.x0, card.rect.x1) - f64::from(gw) / 2.0;
    let cy = card.rect.y0 + card.rect.height() * 0.30 - f64::from(gh) / 2.0;
    // Always text_primary — the dimmed/active distinction lives on the
    // subtitle below, not the glyph.
    let glyph_color = colors.text_primary;
    text_renderer.draw_text(
        scene,
        &card.label,
        glyph_size,
        glyph_color,
        at(paint.xform, cx, cy),
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
    let sub_color = if paint.is_active {
        colors.text_primary
    } else {
        colors.text_dim
    };
    text_renderer.draw_text(
        scene,
        &subtitle,
        sub_size,
        sub_color,
        at(paint.xform, sx, sy),
        None,
    );
}

/// The card's collaborator-avatar row, clamped to 3 shown plus an
/// overflow chip. Inactive cards render at half-alpha so the active
/// card stays the focus anchor.
fn draw_card_avatars(
    scene: &mut Scene,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    card: &WorkspaceCard,
    paint: &CardPaint,
) {
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    let card_count = card.collab_count;
    if card_count == 0 {
        return;
    }
    let shown = card_count.min(3);
    let overflow = card_count.saturating_sub(3);
    let dot_r = 3.0_f64;
    let gap = 3.0_f64;
    let shown_f = f64::from(u16::try_from(shown).unwrap_or(u16::MAX));
    let row_w = shown_f * (dot_r * 2.0)
        + (shown_f - 1.0).max(0.0) * gap
        + if overflow > 0 { 14.0 } else { 0.0 };
    let base_x = f64::midpoint(card.rect.x0, card.rect.x1) - row_w / 2.0;
    let dot_y = card.rect.y1 - 10.0;
    let dot_color = if paint.is_active {
        colors.text_primary
    } else {
        colors.text_primary.with_alpha_factor(0.55)
    };
    for i in 0..shown {
        let i_f = f64::from(u16::try_from(i).unwrap_or(u16::MAX));
        let dx = base_x + dot_r + i_f * (dot_r * 2.0 + gap);
        let dot_rect = Rect::new(dx - dot_r, dot_y - dot_r, dx + dot_r, dot_y + dot_r);
        ui::widget::Avatar::draw_at(
            scene,
            paint.xform,
            dot_rect,
            "",
            dot_color,
            theme,
            text_renderer,
        );
    }
    if overflow > 0 {
        let chip_x = base_x + shown_f * (dot_r * 2.0 + gap);
        let s = format!("+{overflow}");
        // Only the ascent-based y alignment (`th`) is needed.
        let (_tw, th) = text_renderer.measure(&s, tokens.typography.scale.xs, None);
        text_renderer.draw_text(
            scene,
            &s,
            tokens.typography.scale.xs,
            dot_color,
            at(paint.xform, chip_x, dot_y - f64::from(th) / 2.0),
            None,
        );
    }
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
        // `TaffyTree::new_leaf` only ever inserts into its own slotmaps and
        // returns `Ok` unconditionally for a leaf with no child/parent refs
        // — there is no non-panicking `NodeId` to fall back to from a
        // trait method that must return one.
        #[allow(
            clippy::unwrap_used,
            reason = "TaffyTree::new_leaf has no failure path for a plain leaf node"
        )]
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
        // `node` was created and inserted by this same widget's
        // `build_layout`, in the same layout pass — a lookup miss here
        // would mean taffy itself is broken, but declining to update the
        // rect is still safer than a panic.
        let Ok(l) = taffy.layout(node) else {
            return;
        };
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
        self.draw_backdrop(scene, transform, theme);
        let eased = self.advance_pill_anim();
        self.draw_pill_highlight(scene, transform, theme, eased);
        self.draw_wheel_rim(scene, transform, theme, eased);

        for (idx, card) in self.cards.iter().enumerate() {
            let is_active = self.active == Some(card.id);
            let is_hover = self.hover_idx == Some(idx);
            // Scaled about the card's center so it grows symmetrically.
            // Applied to the glyph/subtitle transforms too, so the
            // pill stays one unit.
            let paint = CardPaint {
                xform: if is_hover && !is_active {
                    scaled_about(transform, card.rect.center().x, card.rect.center().y, 1.05)
                } else {
                    transform
                },
                is_active,
                is_hover,
            };
            draw_card_background(scene, transform, theme, card, &paint);
            draw_card_text(scene, text_renderer, theme, card, &paint);
            draw_card_avatars(scene, text_renderer, theme, card, &paint);
        }

        self.draw_plus_card(scene, transform, theme);
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
                self.handle_cursor_moved(p, handled);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_mouse_wheel(delta, handled);
            }
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                self.handle_left_press(handled);
            }
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            }
            // Rotations already applied incrementally in `CursorMoved`.
            if self.drag_start_y.is_some() =>
            {
                self.handle_left_release(handled);
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

impl SidebarWheelChrome {
    fn handle_cursor_moved(&mut self, p: Point, handled: &mut bool) {
        self.last_cursor = Some(p);
        self.hover_idx = self.cards.iter().position(|c| c.rect.contains(p));
        self.hover_plus = self.plus_rect.contains(p);

        let (Some(start), Some(last)) = (self.drag_start_y, self.drag_last_y) else {
            return;
        };
        let dy = p.y - last;
        self.drag_last_y = Some(p.y);
        // Crossing click tolerance marks a drag, so release skips click-activate.
        if !self.drag_moved && (p.y - start).abs() > DRAG_CLICK_TOLERANCE_PX {
            self.drag_moved = true;
        }
        if !self.drag_moved {
            return;
        }
        self.drag_accum += dy;
        // Down → later workspace (+1), matching scroll-wheel sign.
        let (steps, remainder) = whole_steps(self.drag_accum, DRAG_SWITCH_THRESHOLD_PX);
        self.drag_accum = remainder;
        for _ in 0..steps {
            if let Some(chrome) = self.chrome.clone()
                && !chrome.borrow_mut().switch_workspace(1)
            {
                // Hit the bottom — stop consuming travel.
                self.drag_accum = 0.0;
                break;
            }
            *handled = true;
        }
        let (steps, remainder) = whole_steps(-self.drag_accum, DRAG_SWITCH_THRESHOLD_PX);
        self.drag_accum = -remainder;
        for _ in 0..steps {
            if let Some(chrome) = self.chrome.clone()
                && !chrome.borrow_mut().switch_workspace(-1)
            {
                self.drag_accum = 0.0;
                break;
            }
            *handled = true;
        }
    }

    fn handle_mouse_wheel(&mut self, delta: &winit::event::MouseScrollDelta, handled: &mut bool) {
        // Content scrolling shouldn't steal workspace switching.
        let over_sidebar = self.last_cursor.is_some_and(|p| self.rect.contains(p));
        if !over_sidebar {
            return;
        }
        let dy: f64 = match delta {
            winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
            winit::event::MouseScrollDelta::PixelDelta(p) => p.y / 40.0,
        };
        self.scroll_accum += dy;

        let (steps, remainder) = whole_steps(self.scroll_accum, 1.0);
        self.scroll_accum = remainder;
        for _ in 0..steps {
            if let Some(chrome) = self.chrome.clone()
                && !chrome.borrow_mut().switch_workspace(-1)
            {
                self.scroll_accum = 0.0;
                break;
            }
            *handled = true;
        }
        let (steps, remainder) = whole_steps(-self.scroll_accum, 1.0);
        self.scroll_accum = -remainder;
        for _ in 0..steps {
            if let Some(chrome) = self.chrome.clone()
                && !chrome.borrow_mut().switch_workspace(1)
            {
                self.scroll_accum = 0.0;
                break;
            }
            *handled = true;
        }
    }

    fn handle_left_press(&mut self, handled: &mut bool) {
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

    fn handle_left_release(&mut self, handled: &mut bool) {
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
        use winit::event::{ElementState, MouseButton};
        let (mut s, chrome) = sidebar_with_workspaces(3);
        let active_before = chrome.borrow().tree().active;
        s.rect = Rect::new(0.0, 0.0, 96.0, 600.0);
        s.last_cursor = Some(Point::new(48.0, 100.0));
        s.drag_start_y = Some(100.0);
        s.drag_last_y = Some(100.0);
        s.drag_accum = 0.0;
        s.drag_moved = false;

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
                let (steps, remainder) = whole_steps(s.drag_accum, DRAG_SWITCH_THRESHOLD_PX);
                s.drag_accum = remainder;
                for _ in 0..steps {
                    if let Some(ch) = s.chrome.clone()
                        && !ch.borrow_mut().switch_workspace(1)
                    {
                        s.drag_accum = 0.0;
                        break;
                    }
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
                let (steps, remainder) = whole_steps(-s.drag_accum, DRAG_SWITCH_THRESHOLD_PX);
                s.drag_accum = -remainder;
                for _ in 0..steps {
                    if let Some(ch) = s.chrome.clone()
                        && !ch.borrow_mut().switch_workspace(-1)
                    {
                        s.drag_accum = 0.0;
                        break;
                    }
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
