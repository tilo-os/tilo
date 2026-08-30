//! Toolbar widget — a horizontal or vertical strip of buttons, separators
//! and greedy spacers, with overflow collapse into a `…` chip when the
//! computed item extent exceeds the toolbar's rect along its orientation
//! axis.
//!
//! Owns its layout end-to-end rather than delegating child sizing to Taffy:
//! `build_layout` measures each item (GPU-free `TextRenderer`-style
//! approximations) and writes the visible-count + overflow flag
//! `apply_layout` places rects from. Spacers soak the residual axis space;
//! the overflow chip always wins a trailing slot once visibility drops
//! below the item count. See `tabs.rs` for the strip-of-rects precedent.

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// Approximate per-character width used by `build_layout` to size button
/// labels without invoking `TextRenderer`. Matches `tabs.rs::apply_layout`'s
/// heuristic so a Toolbar reads at the same density as a `TabBar` beside it.
const APPROX_CHAR_W: f64 = 7.0;

/// Width (Horizontal) or height (Vertical) of a Separator line. Always
/// 1 logical pixel — a thinner divider reads as a hairline, a thicker
/// one starts to look like a rule.
const SEPARATOR_LINE_PX: f64 = 1.0;

/// Width of the overflow `…` chip on the trailing edge of an overflowing
/// toolbar. Sized to comfortably fit the three-dot glyph plus padding;
/// chosen to roughly match a one-character button so the chip doesn't
/// dominate the visible items it's replacing.
const OVERFLOW_CHIP_AXIS_PX: f64 = 24.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Orientation {
    Horizontal,
    Vertical,
}

pub enum ToolbarItem {
    Button {
        label: String,
        on_click: Box<dyn FnMut()>,
    },
    Separator,
    Spacer,
}

pub struct Toolbar {
    items: Vec<ToolbarItem>,
    orientation: Orientation,
    overflow_open: bool,
    rect: Rect,
    item_rects: Vec<Rect>,
    visible_count: usize,
    /// Per-item hover flag, populated by `handle_event`. Aligned 1:1 with
    /// `items`. Kept independent of `visible_count` so a hover that
    /// happened pre-overflow doesn't latch on after the items collapse.
    item_hover: Vec<bool>,
    /// True while the pointer is over the overflow chip. Drives the
    /// chip's hover background like a regular button. Painted only when
    /// `has_overflow()` is true.
    overflow_hover: bool,
    /// Cached overflow-chip hit rect (only meaningful when
    /// `has_overflow()` is true). Populated alongside `item_rects` in
    /// `apply_layout` so `handle_event` can hit-test without re-deriving
    /// the chip's position.
    overflow_rect: Rect,
}

impl Toolbar {
    pub fn new(orientation: Orientation) -> Self {
        Self {
            items: Vec::new(),
            orientation,
            overflow_open: false,
            rect: Rect::ZERO,
            item_rects: Vec::new(),
            visible_count: 0,
            item_hover: Vec::new(),
            overflow_hover: false,
            overflow_rect: Rect::ZERO,
        }
    }

    pub fn push_button(mut self, label: &str, on_click: impl FnMut() + 'static) -> Self {
        self.items.push(ToolbarItem::Button {
            label: label.to_string(),
            on_click: Box::new(on_click),
        });
        self.item_hover.push(false);
        self
    }

    pub fn push_separator(mut self) -> Self {
        self.items.push(ToolbarItem::Separator);
        self.item_hover.push(false);
        self
    }

    pub fn push_spacer(mut self) -> Self {
        self.items.push(ToolbarItem::Spacer);
        self.item_hover.push(false);
        self
    }

    /// True if the toolbar's computed item extent exceeded its rect's
    /// extent along the orientation axis on the last layout pass. While
    /// true, items past `visible_count` are collapsed into a single
    /// overflow chip on the trailing edge.
    pub fn has_overflow(&self) -> bool {
        self.visible_count < self.items.len()
    }

    /// Number of items currently rendered as their own rect (i.e. *not*
    /// folded into the overflow chip). Always `<= items.len()`. When
    /// `has_overflow()` is true this is strictly less.
    pub fn visible_count(&self) -> usize {
        self.visible_count
    }

    /// Number of `ToolbarItem::Separator` entries in the toolbar. Cheap
    /// accessor for the divider-paint test — keeps the test stable
    /// against future scene-encoding refactors that might shuffle which
    /// primitive emits the divider stroke.
    pub fn separator_count(&self) -> usize {
        self.items
            .iter()
            .filter(|i| matches!(i, ToolbarItem::Separator))
            .count()
    }

    /// Axis extent one item occupies: a button's label plus padding, a
    /// separator's hairline plus its surrounding air, and zero for a
    /// spacer (which is sized from the residual, not measured).
    fn measure_item_axis(&self, item: &ToolbarItem, theme: &Theme) -> f64 {
        match item {
            ToolbarItem::Spacer => 0.0,
            ToolbarItem::Separator => SEPARATOR_LINE_PX + 2.0 * f64::from(theme.tokens.spacing.s2),
            ToolbarItem::Button { label, .. } => {
                let pad = 2.0 * f64::from(theme.tokens.spacing.s3);
                let text_axis = match self.orientation {
                    Orientation::Horizontal => label.chars().count() as f64 * APPROX_CHAR_W,
                    Orientation::Vertical => f64::from(theme.tokens.typography.scale.sm),
                };
                (text_axis + pad).max(f64::from(theme.tokens.spacing.s8))
            }
        }
    }

    /// Decide how many leading items fit inside `axis_extent`, writing
    /// [`Self::visible_count`] (and therefore [`Self::has_overflow`]).
    ///
    /// Greedy from the leading edge: take items while they fit. The moment
    /// one doesn't, the trailing edge has to give up
    /// [`OVERFLOW_CHIP_AXIS_PX`] to the `…` chip, so the fit is re-checked
    /// against the smaller budget — otherwise the last item admitted would
    /// sit underneath the chip. Spacers measure zero and are always
    /// admitted; they soak the residual in `apply_layout`.
    pub fn recompute_visible(&mut self, axis_extent: f64, theme: &Theme) {
        if self.items.is_empty() {
            self.visible_count = 0;
            return;
        }
        let extents: Vec<f64> = self
            .items
            .iter()
            .map(|it| self.measure_item_axis(it, theme))
            .collect();
        let total: f64 = extents.iter().sum();
        if total <= axis_extent {
            self.visible_count = self.items.len();
            return;
        }
        let budget = (axis_extent - OVERFLOW_CHIP_AXIS_PX).max(0.0);
        let mut used = 0.0;
        let mut n = 0usize;
        for e in &extents {
            if used + e > budget {
                break;
            }
            used += e;
            n += 1;
        }
        // Collapsing everything leaves a toolbar that is only a `…` chip —
        // honest for a genuinely tiny strip, but never collapse when the
        // items would have fit without the chip's own reservation.
        self.visible_count = n.min(self.items.len().saturating_sub(1));
    }

    /// True while the overflow popover is open. (The popover itself is
    /// not painted by this widget — `WindowGroup`-style hosts paint it
    /// in a later pass.)
    pub fn is_overflow_open(&self) -> bool {
        self.overflow_open
    }
}

impl Widget for Toolbar {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Fixed cross-axis size, derived from the button padding + sm
        // typography slug so the strip reads as compact chrome.
        let cross_axis_size = theme.tokens.typography.scale.sm + 2.0 * theme.tokens.spacing.s2;
        let (w, h) = match self.orientation {
            Orientation::Horizontal => (Dimension::auto(), Dimension::length(cross_axis_size)),
            Orientation::Vertical => (Dimension::length(cross_axis_size), Dimension::auto()),
        };
        let node = taffy.leaf_node(Style {
            size: Size {
                width: w,
                height: h,
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        // Measure against whatever axis extent the last layout gave us.
        // On the very first pass `self.rect` is empty, so fall back to
        // "everything fits" rather than collapsing the whole strip into
        // an overflow chip before the toolbar has ever been sized.
        let axis_extent = match self.orientation {
            Orientation::Horizontal => self.rect.width(),
            Orientation::Vertical => self.rect.height(),
        };
        if axis_extent > 0.0 {
            self.recompute_visible(axis_extent, theme);
        } else if self.visible_count == 0 {
            self.visible_count = self.items.len();
        }
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );

        self.item_rects.clear();
        self.item_rects.resize(self.items.len(), Rect::ZERO);
        if self.items.is_empty() {
            self.visible_count = 0;
            self.overflow_rect = Rect::ZERO;
            return;
        }

        let axis_total = match self.orientation {
            Orientation::Horizontal => self.rect.width(),
            Orientation::Vertical => self.rect.height(),
        };

        // build_layout only has the theme (not the final width); apply_layout
        // has the post-taffy width, so the overflow fit is decided here,
        // using the same approximate, theme-free extents as the placement
        // pass below so the two agree.
        let item_axis = |it: &ToolbarItem| -> f64 {
            match it {
                ToolbarItem::Spacer => 0.0,
                ToolbarItem::Separator => SEPARATOR_LINE_PX + 8.0,
                ToolbarItem::Button { label, .. } => {
                    let text_axis = match self.orientation {
                        Orientation::Horizontal => label.chars().count() as f64 * APPROX_CHAR_W,
                        Orientation::Vertical => 16.0,
                    };
                    (text_axis + 16.0).max(32.0)
                }
            }
        };
        let total_extent: f64 = self.items.iter().map(&item_axis).sum();
        if total_extent <= axis_total {
            self.visible_count = self.items.len();
        } else {
            let budget = (axis_total - OVERFLOW_CHIP_AXIS_PX).max(0.0);
            let mut acc = 0.0;
            let mut fit = 0usize;
            for it in &self.items {
                let e = item_axis(it);
                if acc + e > budget {
                    break;
                }
                acc += e;
                fit += 1;
            }
            self.visible_count = fit;
        }

        let n_visible = self.visible_count.min(self.items.len());
        let overflow = self.has_overflow();

        // `apply_layout` has no `Theme`, so extents are recomputed here
        // with the same hardcoded constants as `item_axis` above rather
        // than calling `measure_item_axis` — approximate, but deterministic
        // and consistent with the overflow decision just made.
        let mut measured_axes = vec![0.0_f64; n_visible];
        let mut spacer_count = 0usize;
        for (i, it) in self.items.iter().take(n_visible).enumerate() {
            match it {
                ToolbarItem::Spacer => {
                    spacer_count += 1;
                }
                ToolbarItem::Separator => {
                    measured_axes[i] = SEPARATOR_LINE_PX + 8.0;
                }
                ToolbarItem::Button { label, .. } => {
                    let pad = 16.0;
                    let text_axis = match self.orientation {
                        Orientation::Horizontal => label.chars().count() as f64 * APPROX_CHAR_W,
                        Orientation::Vertical => 16.0,
                    };
                    measured_axes[i] = (text_axis + pad).max(32.0);
                }
            }
        }
        let used: f64 = measured_axes.iter().sum();
        let chip_reserve = if overflow { OVERFLOW_CHIP_AXIS_PX } else { 0.0 };
        let residual = (axis_total - used - chip_reserve).max(0.0);
        let per_spacer = if spacer_count > 0 {
            residual / spacer_count as f64
        } else {
            0.0
        };

        let cross_start = match self.orientation {
            Orientation::Horizontal => self.rect.y0,
            Orientation::Vertical => self.rect.x0,
        };
        let cross_end = match self.orientation {
            Orientation::Horizontal => self.rect.y1,
            Orientation::Vertical => self.rect.x1,
        };

        let mut axis_cursor = match self.orientation {
            Orientation::Horizontal => self.rect.x0,
            Orientation::Vertical => self.rect.y0,
        };

        for (i, item) in self.items.iter().take(n_visible).enumerate() {
            let extent = if matches!(item, ToolbarItem::Spacer) {
                per_spacer
            } else {
                measured_axes[i]
            };
            let r = match self.orientation {
                Orientation::Horizontal => {
                    Rect::new(axis_cursor, cross_start, axis_cursor + extent, cross_end)
                }
                Orientation::Vertical => {
                    Rect::new(cross_start, axis_cursor, cross_end, axis_cursor + extent)
                }
            };
            self.item_rects[i] = r;
            axis_cursor += extent;
        }
        // Items beyond `visible_count` keep `Rect::ZERO`: not rendered as
        // their own rect, and any hover targeting them is silently dropped
        // on the next `handle_event` (CursorMoved reads `item_rects`).
        for r in self.item_rects.iter_mut().skip(n_visible) {
            *r = Rect::ZERO;
        }

        if overflow {
            self.overflow_rect = match self.orientation {
                Orientation::Horizontal => Rect::new(
                    self.rect.x1 - OVERFLOW_CHIP_AXIS_PX,
                    cross_start,
                    self.rect.x1,
                    cross_end,
                ),
                Orientation::Vertical => Rect::new(
                    cross_start,
                    self.rect.y1 - OVERFLOW_CHIP_AXIS_PX,
                    cross_end,
                    self.rect.y1,
                ),
            };
        } else {
            self.overflow_rect = Rect::ZERO;
        }
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
        let radius = f64::from(tokens.radii.sm);
        let font_size = tokens.typography.scale.sm;
        let n_visible = self.visible_count.min(self.items.len());

        for i in 0..n_visible {
            let r = self.item_rects[i];
            if r.is_zero_area() {
                continue;
            }
            match &self.items[i] {
                ToolbarItem::Button { label, .. } => {
                    let hovered = self.item_hover.get(i).copied().unwrap_or(false);
                    let bg = if hovered {
                        theme.bg_hover
                    } else {
                        tokens.colors.surfaces.raised
                    };
                    let shape = RoundedRect::from_rect(r, radius);
                    scene.fill(vello::peniko::Fill::NonZero, transform, bg, None, &shape);
                    let (tw, th) = text_renderer.measure(label, font_size, None);
                    let tx = r.center().x - f64::from(tw) / 2.0;
                    let ty = r.center().y - f64::from(th) / 2.0;
                    text_renderer.draw_text(
                        scene,
                        label,
                        font_size,
                        tokens.colors.text_primary,
                        transform * Affine::translate((tx, ty)),
                        None,
                    );
                }
                ToolbarItem::Separator => {
                    // Hairline divider perpendicular to the orientation
                    // axis. Margin `s2` from each end along the cross
                    // axis (so the line doesn't kiss the toolbar edge).
                    let margin = f64::from(tokens.spacing.s2);
                    let line_rect = match self.orientation {
                        Orientation::Horizontal => {
                            let cx = r.center().x;
                            Rect::new(
                                cx - SEPARATOR_LINE_PX / 2.0,
                                r.y0 + margin,
                                cx + SEPARATOR_LINE_PX / 2.0,
                                r.y1 - margin,
                            )
                        }
                        Orientation::Vertical => {
                            let cy = r.center().y;
                            Rect::new(
                                r.x0 + margin,
                                cy - SEPARATOR_LINE_PX / 2.0,
                                r.x1 - margin,
                                cy + SEPARATOR_LINE_PX / 2.0,
                            )
                        }
                    };
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        tokens.colors.border_standard,
                        None,
                        &line_rect,
                    );
                }
                ToolbarItem::Spacer => {
                    // Greedy spacer — soaks remaining space, paints
                    // nothing. Stays in `item_rects` so apply_layout
                    // bookkeeping is uniform.
                }
            }
        }

        if self.has_overflow() && !self.overflow_rect.is_zero_area() {
            let bg = if self.overflow_hover {
                theme.bg_hover
            } else {
                tokens.colors.surfaces.raised
            };
            let shape = RoundedRect::from_rect(self.overflow_rect, radius);
            scene.fill(vello::peniko::Fill::NonZero, transform, bg, None, &shape);
            let label = "…";
            let (tw, th) = text_renderer.measure(label, font_size, None);
            let tx = self.overflow_rect.center().x - f64::from(tw) / 2.0;
            let ty = self.overflow_rect.center().y - f64::from(th) / 2.0;
            text_renderer.draw_text(
                scene,
                label,
                font_size,
                tokens.colors.text_primary,
                transform * Affine::translate((tx, ty)),
                None,
            );
        }
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
                let pos = Point::new(position.x / scale_factor, position.y / scale_factor);
                let n_visible = self.visible_count.min(self.items.len());
                if self.item_hover.len() != self.items.len() {
                    self.item_hover.resize(self.items.len(), false);
                }
                for (i, h) in self.item_hover.iter_mut().enumerate() {
                    let new_h = i < n_visible
                        && matches!(self.items[i], ToolbarItem::Button { .. })
                        && self.item_rects[i].contains(pos);
                    if new_h != *h {
                        *h = new_h;
                        *handled = true;
                    }
                }
                let new_oh = self.has_overflow() && self.overflow_rect.contains(pos);
                if new_oh != self.overflow_hover {
                    self.overflow_hover = new_oh;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let n_visible = self.visible_count.min(self.items.len());
                for i in 0..n_visible {
                    if self.item_hover.get(i).copied().unwrap_or(false)
                        && let ToolbarItem::Button { on_click, .. } = &mut self.items[i]
                    {
                        on_click();
                        *handled = true;
                    }
                }
                if self.overflow_hover && self.has_overflow() {
                    self.overflow_open = !self.overflow_open;
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "Toolbar"
    }

    /// The toolbar reports [`Role::Toolbar`] — a strip of controls. Its
    /// items are drawn directly (not child widgets), so they don't flow
    /// through `children()`; the container itself stands in for the strip.
    /// It carries no name of its own (a labelling host would supply one).
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        Some(AccessNode::new(Role::Toolbar))
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::PresenceCtx;
    use vello_encoding::DrawTag;

    /// Build a toolbar with `n` no-op buttons. Convenience for the
    /// overflow + paint tests that don't care which closure is invoked.
    fn n_button_toolbar(n: usize) -> Toolbar {
        let mut t = Toolbar::new(Orientation::Horizontal);
        for i in 0..n {
            t = t.push_button(&format!("B{i}"), || {});
        }
        t
    }

    /// Drive a Toolbar through the full `build_layout` + `apply_layout`
    /// pipeline at a fixed available width. Returns the toolbar with
    /// `item_rects` and `visible_count` populated.
    fn lay_out(mut tb: Toolbar, axis_extent: f64) -> Toolbar {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = tb.build_layout(&mut taffy, &theme, &mut node_map);
        // Force the outer rect to span `axis_extent`; taffy's auto sizing
        // would otherwise shrink to content.
        taffy
            .set_style(
                node,
                Style {
                    size: Size {
                        width: Dimension::length(axis_extent as f32),
                        height: Dimension::length(28.0),
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(axis_extent as f32),
                    height: taffy::AvailableSpace::Definite(28.0),
                },
            )
            .unwrap();
        tb.apply_layout(&taffy, node, Point::ZERO);
        tb
    }

    /// Render `tb` into a fresh scene and return the scene so tests can
    /// inspect `DrawTag::COLOR` counts.
    fn render(tb: &mut Toolbar) -> Scene {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        tb.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        scene
    }

    fn count_color_fills(scene: &Scene) -> usize {
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count()
    }

    #[test]
    fn accessibility_reports_toolbar_role() {
        use crate::a11y::Role;
        let tb = n_button_toolbar(3);
        let node = Widget::accessibility(&tb).expect("toolbar is a semantic strip");
        assert_eq!(node.role, Role::Toolbar);
        assert_eq!(node.name, None, "toolbar carries no name of its own");
    }

    /// Three buttons + ample width must lay out three non-empty rects
    /// and the draw pass must emit at least three `DrawTag::COLOR`
    /// entries (one per button background fill).
    #[test]
    fn toolbar_with_three_buttons_paints_three_button_rects() {
        let mut tb = lay_out(n_button_toolbar(3), 800.0);
        assert_eq!(
            tb.item_rects.len(),
            3,
            "three buttons must produce three item rects (got {})",
            tb.item_rects.len()
        );
        for (i, r) in tb.item_rects.iter().enumerate() {
            assert!(
                !r.is_zero_area(),
                "item_rects[{i}] must be non-empty (got {r:?})"
            );
        }
        assert_eq!(tb.visible_count(), 3);
        assert!(!tb.has_overflow());

        let scene = render(&mut tb);
        let fills = count_color_fills(&scene);
        assert!(
            fills >= 3,
            "expected at least 3 COLOR fills (one per button bg), got {fills}",
        );
    }

    /// Ten buttons in a width that only fits ~4 must collapse the rest
    /// into the overflow chip. `has_overflow()` true, `visible_count()`
    /// strictly less than 10.
    #[test]
    fn toolbar_overflow_creates_overflow_button() {
        // Ten buttons at ~30px each (~320px total) in a 150px budget must
        // overflow, fitting only a handful with room for the chip (24px).
        let tb = lay_out(n_button_toolbar(10), 150.0);
        assert!(
            tb.has_overflow(),
            "10 buttons in 150 px must overflow (visible_count={}, items={})",
            tb.visible_count(),
            tb.items.len()
        );
        assert!(
            tb.visible_count() < 10,
            "visible_count must drop below 10 when overflowing (got {})",
            tb.visible_count()
        );
        // And the visible items still produced real rects.
        for i in 0..tb.visible_count() {
            assert!(
                !tb.item_rects[i].is_zero_area(),
                "visible item_rects[{i}] must be non-empty"
            );
        }
        // Overflow chip rect exists.
        assert!(
            !tb.overflow_rect.is_zero_area(),
            "overflow chip rect must be populated when has_overflow() is true"
        );
    }

    /// `[Button, Separator, Button]` — the divider paints as a thin filled
    /// rect (an extra COLOR draw tag). `separator_count` pins the structural
    /// contract independently of the scene-encoding check.
    #[test]
    fn toolbar_separator_paints_a_divider_line() {
        let mut tb = Toolbar::new(Orientation::Horizontal)
            .push_button("A", || {})
            .push_separator()
            .push_button("B", || {});
        tb = lay_out(tb, 400.0);

        assert_eq!(
            tb.separator_count(),
            1,
            "expected exactly one Separator item (got {})",
            tb.separator_count()
        );
        assert_eq!(tb.visible_count(), 3);

        let scene = render(&mut tb);
        let fills = count_color_fills(&scene);
        // Two button backgrounds + the separator line = at least 3
        // COLOR fills. Strictly greater than a no-separator baseline,
        // proving the separator actually contributed paint.
        assert!(
            fills >= 3,
            "expected at least 3 COLOR fills (2 buttons + 1 separator), got {fills}"
        );

        // Baseline comparison: a two-button toolbar (no separator) emits
        // strictly fewer COLOR fills than the same toolbar with a
        // separator wedged between them.
        let mut bare = lay_out(
            Toolbar::new(Orientation::Horizontal)
                .push_button("A", || {})
                .push_button("B", || {}),
            400.0,
        );
        let bare_scene = render(&mut bare);
        let bare_fills = count_color_fills(&bare_scene);
        assert!(
            fills > bare_fills,
            "separator must add at least one COLOR fill (with={fills}, without={bare_fills})"
        );
    }
}
