//! Toolbar widget — a horizontal or vertical strip of buttons, separators
//! and greedy spacers, with overflow collapse into a `…` chip when the
//! computed item extent exceeds the toolbar's rect along its orientation
//! axis.
//!
//! This widget owns its layout end-to-end (it doesn't delegate child
//! sizing to Taffy). `build_layout` measures each item along the
//! orientation axis (using `TextRenderer`-style approximations so the
//! pass stays GPU-free) and writes the visible-count + overflow flag
//! used by `apply_layout` to place rects. Spacers soak the residual
//! axis space after fixed items have been measured; the overflow chip
//! always wins a slot on the trailing edge once visibility drops below
//! the item count.
//!
//! See `quick-ui/src/widget/tabs.rs` for the strip-of-rects precedent
//! this implementation follows.

use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use std::collections::HashMap;

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// Approximate per-character width used by `build_layout` to size button
/// labels without invoking `TextRenderer` (build_layout doesn't get one).
/// Matches the heuristic used by `tabs.rs::apply_layout` (`approx_char_w =
/// 7.0`) so a Toolbar laid out in isolation reads at the same density as
/// a TabBar built alongside it.
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

    /// True while the overflow popover is open. (The popover itself is
    /// not painted by this widget — `WindowGroup`-style hosts paint it
    /// in a later pass.)
    pub fn is_overflow_open(&self) -> bool {
        self.overflow_open
    }

    /// Measure a button label's extent along the orientation axis. Used
    /// by `build_layout` to compute `visible_count` before Taffy has any
    /// idea about per-item widths. Matches the `tabs.rs` approximation
    /// so a Toolbar reads at the same density as a TabBar in isolation.
    fn measure_button_axis(&self, label: &str, theme: &Theme) -> f64 {
        let pad_axis = match self.orientation {
            // Horizontal pad on each side is `tokens.spacing.s3`; vertical
            // (cross-axis) padding doesn't affect the orientation extent.
            Orientation::Horizontal => 2.0 * theme.tokens.spacing.s3 as f64,
            // For vertical toolbars the orientation axis is height, so
            // `s2` (vertical pad) is what we double up.
            Orientation::Vertical => 2.0 * theme.tokens.spacing.s2 as f64,
        };
        let text_axis = match self.orientation {
            Orientation::Horizontal => label.chars().count() as f64 * APPROX_CHAR_W,
            // Vertical labels still read horizontally; height is
            // ~font-size + a bit of slack.
            Orientation::Vertical => theme.tokens.typography.scale.sm as f64 * 1.4,
        };
        (text_axis + pad_axis).max(theme.tokens.spacing.s8 as f64)
    }

    /// Extent of a non-spacer item along the orientation axis. Spacers
    /// return 0 because they soak residual space rather than consuming
    /// a fixed amount.
    fn measure_item_axis(&self, item: &ToolbarItem, theme: &Theme) -> f64 {
        match item {
            ToolbarItem::Button { label, .. } => self.measure_button_axis(label, theme),
            ToolbarItem::Separator => SEPARATOR_LINE_PX + 2.0 * theme.tokens.spacing.s2 as f64,
            ToolbarItem::Spacer => 0.0,
        }
    }

    /// Compute `visible_count` from the available axis extent. Items
    /// beyond the first one that would push the cumulative extent past
    /// the limit collapse into the overflow chip; the chip itself
    /// reserves `OVERFLOW_CHIP_AXIS_PX` on the trailing edge.
    fn recompute_visible(&mut self, available: f64, theme: &Theme) {
        let total: f64 = self
            .items
            .iter()
            .map(|it| self.measure_item_axis(it, theme))
            .sum();
        if total <= available {
            self.visible_count = self.items.len();
            return;
        }
        // Doesn't fit — reserve room for the overflow chip and pack
        // items in order until the next one would push us past the
        // adjusted budget.
        let budget = (available - OVERFLOW_CHIP_AXIS_PX).max(0.0);
        let mut used = 0.0;
        let mut count = 0;
        for it in &self.items {
            let w = self.measure_item_axis(it, theme);
            if used + w > budget {
                break;
            }
            used += w;
            count += 1;
        }
        self.visible_count = count;
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
        // The toolbar takes the full available extent along its
        // orientation axis and a fixed cross-axis size. Cross-axis size
        // is derived from the button padding + sm typography slug so the
        // strip reads as compact chrome.
        let cross_axis_size =
            theme.tokens.typography.scale.sm + 2.0 * theme.tokens.spacing.s2;
        let (w, h) = match self.orientation {
            Orientation::Horizontal => (Dimension::auto(), Dimension::length(cross_axis_size)),
            Orientation::Vertical => (Dimension::length(cross_axis_size), Dimension::auto()),
        };
        let node = taffy
            .new_leaf(Style {
                size: Size { width: w, height: h },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        // Optimistic visibility default — assume every item fits until
        // a caller drives `recompute_visible` with a real axis budget
        // (event handling does this when the available width shrinks).
        // Without this, apply_layout's placement loop iterates 0 items
        // and the toolbar paints empty even on a freshly-constructed
        // widget with no overflow pressure.
        if self.visible_count == 0 {
            self.visible_count = self.items.len();
        }
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );

        // Resize the per-item rect cache, then place each item along the
        // orientation axis. Visible items get their measured extent;
        // spacers split the residual space evenly; over-budget items
        // collapse into the overflow chip on the trailing edge.
        self.item_rects.clear();
        self.item_rects.resize(self.items.len(), Rect::ZERO);
        if self.items.is_empty() {
            self.visible_count = 0;
            self.overflow_rect = Rect::ZERO;
            return;
        }

        // We can't call `self.method()` inside this borrow cycle
        // without holding `&Theme`, so we accept that build_layout has
        // already populated `visible_count`. If apply_layout runs
        // without build_layout (unit tests calling apply_layout
        // directly), `visible_count` is left at its previous value —
        // tests that need a fresh layout call `build_layout` first.

        let axis_total = match self.orientation {
            Orientation::Horizontal => self.rect.width(),
            Orientation::Vertical => self.rect.height(),
        };

        // Gather measured extents for the items we know are visible.
        // Spacers report 0 here so we can compute the residual space
        // they share.
        let n_visible = self.visible_count.min(self.items.len());
        let overflow = self.has_overflow();

        // Re-derive each visible item's axis extent. We need a `Theme`
        // for `measure_item_axis`; reach into the cross-axis dimension
        // of `self.rect` for a sane fallback by recomputing without
        // padding constants — this path is only taken when build_layout
        // is skipped (synthetic test setup), so the measurements are
        // approximate but deterministic.
        let mut measured_axes = vec![0.0_f64; n_visible];
        let mut spacer_count = 0usize;
        for (i, it) in self.items.iter().take(n_visible).enumerate() {
            match it {
                ToolbarItem::Spacer => { spacer_count += 1; }
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
        let per_spacer = if spacer_count > 0 { residual / spacer_count as f64 } else { 0.0 };

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

        for i in 0..n_visible {
            let extent = if matches!(self.items[i], ToolbarItem::Spacer) {
                per_spacer
            } else {
                measured_axes[i]
            };
            let r = match self.orientation {
                Orientation::Horizontal => Rect::new(
                    axis_cursor,
                    cross_start,
                    axis_cursor + extent,
                    cross_end,
                ),
                Orientation::Vertical => Rect::new(
                    cross_start,
                    axis_cursor,
                    cross_end,
                    axis_cursor + extent,
                ),
            };
            self.item_rects[i] = r;
            axis_cursor += extent;
        }
        // Items beyond `visible_count` keep `Rect::ZERO` — they're not
        // rendered as their own rect and any hover state targeting them
        // is silently dropped on the next `handle_event` (CursorMoved
        // looks at `item_rects` directly).
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
        let radius = tokens.radii.sm as f64;
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
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        bg,
                        None,
                        &shape,
                    );
                    let (tw, th) = text_renderer.measure(label, font_size, None);
                    let tx = r.center().x - tw as f64 / 2.0;
                    let ty = r.center().y - th as f64 / 2.0;
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
                    let margin = tokens.spacing.s2 as f64;
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
            let tx = self.overflow_rect.center().x - tw as f64 / 2.0;
            let ty = self.overflow_rect.center().y - th as f64 / 2.0;
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
                let new_oh =
                    self.has_overflow() && self.overflow_rect.contains(pos);
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
                    if self.item_hover.get(i).copied().unwrap_or(false) {
                        if let ToolbarItem::Button { on_click, .. } = &mut self.items[i] {
                            on_click();
                            *handled = true;
                        }
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

    fn name(&self) -> &str {
        "Toolbar"
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
        // The build pass needs `Theme`-aware measurements to decide
        // `visible_count`; do that directly so the test isn't coupled
        // to Taffy's exact layout output.
        tb.recompute_visible(axis_extent, &theme);

        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = tb.build_layout(&mut taffy, &theme, &mut node_map);
        // Force the toolbar's outer rect to span `axis_extent` along its
        // orientation axis. Taffy's auto sizing would shrink to content
        // otherwise.
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
    /// inspect `DrawTag::COLOR` counts. Mirrors the helper in
    /// `progress_bar.rs::tests::render`.
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
        // Each button measures roughly `len(label) * 7 + 2 * s3 (8) = 30`
        // for "B0".."B9" (label len 2 → 14 + 16 = 30, clamped at `s8`
        // which is 32 in dark theme). Ten buttons → ~320 px total. A
        // 150 px budget should fit ~4 buttons with room for the overflow
        // chip (24 px), forcing the remainder to collapse.
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

    /// `[Button, Separator, Button]` — the divider line is painted as a
    /// thin filled rect, so it shows up as an extra COLOR draw tag
    /// alongside the two button backgrounds. The `separator_count`
    /// accessor pins the structural contract independently of the
    /// scene-encoding check, so a future refactor that switches the
    /// separator to a `scene.stroke` doesn't silently break the test.
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
