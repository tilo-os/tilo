//! `Chip` — a compact interactive pill for tags, filters, and tokens. Distinct
//! from [`super::Badge`] (a tiny non-interactive count/dot): a chip carries a
//! label and can be **selectable** (a filter that toggles on/off, tinting to
//! the accent when active) and/or **removable** (a trailing "×" that removes a
//! token, e.g. a recipient or an applied filter).
//!
//! Both interactions surface through the toolkit's drain channel —
//! [`Chip::take_toggled`] and [`Chip::take_removed`] — rather than callbacks,
//! so a CRDT-native host reads them each frame and mutates its own model.

use std::collections::HashMap;

use taffy::style_helpers::length;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::color_compat::ColorCompat;
use crate::icons::Icon;
use crate::presence::PresenceCtx;
use crate::text_metrics::renderer_type_size;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::colors::TintedChip;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// `kurbo::Affine` composition is plain f64 matrix math with no panic
/// path; `arithmetic_side_effects` flags the `*` operator overload anyway.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine * Affine composes two 2D transforms via f64 \
              matrix math; it never panics or overflows, unlike integer +/-/*"
)]
fn translated(transform: Affine, x: f64, y: f64) -> Affine {
    transform * Affine::translate((x, y))
}

/// Default pill height: the dense control step (HIG §9.6, 28 px). Hosts that
/// lay a chip out taller get a proportionally larger label via
/// [`Chip::label_size_for_height`].
const HEIGHT: f64 = 28.0;
/// Horizontal inset, `spacing.s3` at 1× — the same inset a button uses, so a
/// chip and a button in one row start their content on the same line.
const PAD_X: f64 = 12.0;
/// Lead-to-label and label-to-dismiss gap; 6 px like the button's icon gap,
/// between `s1` (crowds the glyph) and `s2` (splits the pill into two marks).
const GAP: f64 = 6.0;
/// Status dot diameter, `spacing.s2` at 1×.
const DOT_D: f64 = 8.0;
/// Dismiss glyph edge: the label's cap height plus a hit margin.
const X_D: f64 = 14.0;

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent configuration/state bit (is a \
              toggle, current toggle value, is removable, toggled-this-frame, \
              removed-this-frame); merging them into a bitflags/enum would \
              not change behavior"
)]
pub struct Chip {
    label: String,
    selectable: bool,
    selected: bool,
    removable: bool,
    dot: Option<Color>,
    toggled: bool,
    removed: bool,
    rect: Rect,
    x_rect: Rect,
    last_cursor: Point,
    label_w: f64,
    /// The theme's `typography.scale.sm`, captured in `build_layout` for
    /// `measure` and `adjust_layout_with_measure`, which have no theme of
    /// their own (the old `measure` reached for `Theme::dark()` instead).
    label_size: f32,
    /// Vertical inset between the measured label line and the pill edge
    /// when the line outgrows [`HEIGHT`] (`spacing.s1` a side).
    pad_y: f64,
    /// `metrics.min_pointer_target`: the hit rect grows to this even though
    /// the painted pill stays at the 28 px selector height.
    min_target: f64,
    /// Pointer precision; a coarse pointer raises the target past the
    /// theme's metric (48 px), as it does for every other control.
    interaction: crate::interaction::InteractionEnvironment,
    /// Settled pill size — real measured width, height following the label
    /// line — seeding the next `build_layout` so a stable label costs no
    /// extra pass. Cleared when the theme's label size changes.
    footprint: Option<taffy::Size<f32>>,
}

impl Chip {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            selectable: false,
            selected: false,
            removable: false,
            dot: None,
            toggled: false,
            removed: false,
            rect: Rect::ZERO,
            x_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            label_w: 0.0,
            label_size: crate::tokens::TypeScale::default().sm,
            pad_y: f64::from(crate::tokens::Spacing::BASE.s1),
            min_target: f64::from(crate::tokens::Metrics::BASE.min_pointer_target),
            interaction: crate::interaction::InteractionEnvironment::default(),
            footprint: None,
        }
    }

    /// The larger of the theme's metric and the interaction environment's
    /// minimum (48 px for a coarse pointer).
    fn pointer_target(&self) -> f64 {
        self.min_target
            .max(f64::from(self.interaction.minimum_target()))
    }

    /// The pointer target: the pill's rect grown vertically to the minimum
    /// pointer target. A chip paints at the 28 px selector height (HIG §9.6)
    /// but no target is smaller than 32 px, so the 2 px above and below the
    /// pill belong to it as well.
    pub fn hit_rect(&self) -> Rect {
        Self::grown_to_target(self.rect, self.pointer_target())
    }

    /// The "×" target, grown vertically like [`Chip::hit_rect`]. Only
    /// vertically: sideways it would reach under the last letters of the
    /// label and turn a body click into a removal.
    pub fn remove_hit_rect(&self) -> Rect {
        if self.x_rect.is_zero_area() {
            return self.x_rect;
        }
        Self::grown_to_target(self.x_rect, self.pointer_target())
    }

    fn grown_to_target(rect: Rect, target: f64) -> Rect {
        let slack = ((target - rect.height()) / 2.0).max(0.0);
        rect.inflate(0.0, slack)
    }

    /// Make the chip a toggle (a filter). Clicking the body flips selection.
    #[must_use]
    pub fn selectable(mut self) -> Self {
        self.selectable = true;
        self
    }

    /// Set the initial selected state (implies [`selectable`](Self::selectable)).
    #[must_use]
    pub fn selected(mut self, on: bool) -> Self {
        self.selectable = true;
        self.selected = on;
        self
    }

    /// Show a trailing "×" that removes the chip when clicked.
    #[must_use]
    pub fn removable(mut self) -> Self {
        self.removable = true;
        self
    }

    /// Add a small leading colour dot (e.g. a calendar/category colour).
    #[must_use]
    pub fn with_dot(mut self, color: Color) -> Self {
        self.dot = Some(color);
        self
    }

    pub fn is_selected(&self) -> bool {
        self.selected
    }

    /// Programmatically set selection (host syncing from its model).
    pub fn set_selected(&mut self, on: bool) {
        self.selected = on;
    }

    /// Drain the "selection toggled" signal (fires once per click on the body
    /// of a selectable chip). Read `is_selected()` for the new value.
    pub fn take_toggled(&mut self) -> bool {
        std::mem::take(&mut self.toggled)
    }

    /// Drain the "× clicked / remove me" signal (fires once).
    pub fn take_removed(&mut self) -> bool {
        std::mem::take(&mut self.removed)
    }

    /// The "×" hit rect (empty until laid out + drawn). Exposed for tests.
    pub fn remove_rect(&self) -> Rect {
        self.x_rect
    }

    /// Measured pill size: padding + lead + label + dismiss across, and the
    /// selector height unless the label's measured line (plus `pad_y` a
    /// side) needs more — at 1× a 12 px label shapes a 15 px line and the
    /// pill stays 28; at 2× text the 30 px line lifts it to 42 instead of
    /// spilling over the pill's top and bottom.
    fn intrinsic_size(&self, text_renderer: &mut TextRenderer) -> (f64, f64) {
        // `label_size` is the theme's resolved step (text scale included);
        // the size-based renderer APIs scale again, so hand them the
        // renderer-relative twin — one multiplier, not two. (`draw` still
        // feeds the raw step to the renderer; that expression is X1's.)
        let size = renderer_type_size(text_renderer, self.label_size);
        let (w, lh) = text_renderer.measure(&self.label, size, None);
        let mut total = PAD_X * 2.0 + f64::from(w);
        if self.dot.is_some() {
            total += DOT_D + GAP;
        }
        if self.removable {
            total += GAP + X_D;
        }
        let height = HEIGHT.max(f64::from(lh) + self.pad_y * 2.0);
        (total, height)
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "pill dimensions are small pixel counts well within f32's exact-integer range; taffy's Size requires f32"
    )]
    fn measured_footprint(&self, text_renderer: &mut TextRenderer) -> taffy::Size<f32> {
        let (width, height) = self.intrinsic_size(text_renderer);
        taffy::Size {
            width: width.ceil() as f32,
            height: height.ceil() as f32,
        }
    }
}

/// What sits in a chip's leading slot when it is painted immediately by
/// [`Chip::draw_at`].
#[derive(Clone, Copy, Debug, Default)]
pub enum ChipLead {
    /// Label only.
    #[default]
    None,
    /// A colour-coded dot — the same affordance [`Chip::with_dot`] paints.
    Dot(Color),
    /// A stroked glyph, for chips that name an action rather than a value.
    Glyph(Icon),
}

/// Paint-time variant of an immediate-mode chip. Mirrors the states the
/// [`Chip`] widget itself has — quiet by default, tinted when it is the
/// selected one of a set — plus a `ghost` slot-shape for "there could be
/// another one here" affordances.
#[derive(Clone, Copy, Debug)]
pub struct ChipStyle {
    /// Accent for this chip. `Some` paints the selected treatment (tinted
    /// fill, tinted border, tinted label); `None` the quiet one.
    pub tint: Option<Color>,
    /// Dashed border and dimmed label — an empty slot, not a value.
    pub ghost: bool,
    /// Drop the pill entirely and paint only the lead and the label — for
    /// chips that already sit inside something doing the containing (a lit
    /// disc sector, a row that is itself the selection), where a pill reads
    /// as a box inside a box.
    pub bare: bool,
    /// Multiplies every painted alpha. Lets a caller fade a whole chip
    /// without recolouring it stop by stop.
    pub alpha: f32,
    /// Overrides the derivation from [`Self::tint`] — `accent × 0.18` is a
    /// different colour on a white panel than on a dark menu, and only the
    /// composite can be measured, so a caller that knows its ground resolves
    /// the three through
    /// [`Colors::accent_chip_on`](crate::tokens::colors::Colors::accent_chip_on).
    pub resolved: Option<TintedChip>,
}

impl Default for ChipStyle {
    fn default() -> Self {
        Self {
            tint: None,
            ghost: false,
            bare: false,
            alpha: 1.0,
            resolved: None,
        }
    }
}

impl ChipStyle {
    /// Quiet chip — theme surface, standard border, primary label.
    pub fn quiet() -> Self {
        Self::default()
    }

    /// Selected chip — `tint` carries the fill, border and label colour.
    pub fn tinted(tint: Color) -> Self {
        Self {
            tint: Some(tint),
            ..Self::default()
        }
    }

    /// Empty-slot chip — dashed border, dimmed contents.
    pub fn ghost() -> Self {
        Self {
            ghost: true,
            ..Self::default()
        }
    }

    /// `tint` still colours the lead and the label, so a bare chip has
    /// the same quiet/selected pair the pill form does. See
    /// [`ChipStyle::bare`] for when this is the right shape.
    pub fn bare(tint: Option<Color>) -> Self {
        Self {
            tint,
            bare: true,
            ..Self::default()
        }
    }

    /// Chip painted from colours the caller has already measured against the
    /// ground it lands on. See [`ChipStyle::resolved`].
    pub fn measured(chip: TintedChip) -> Self {
        Self {
            resolved: Some(chip),
            ..Self::default()
        }
    }

    /// Fade the whole chip by `alpha` (clamped to `0.0..=1.0`).
    #[must_use]
    pub fn with_alpha(mut self, alpha: f32) -> Self {
        self.alpha = alpha.clamp(0.0, 1.0);
        self
    }
}

impl Chip {
    /// Text size a chip of `height` px paints its label at. The widget's
    /// own `HEIGHT` maps to `typography.scale.sm`, whether the caller passes
    /// authored geometry or geometry already grown by the responsive layout.
    /// Anything intentionally taller or shorter scales with it, while the
    /// already-scaled type token is still applied exactly once.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "clamped to 0.75..=1.6 just above, so the f32 cast can't \
                  truncate anything that matters for a UI scale factor"
    )]
    pub fn label_size_for_height(height: f64, theme: &Theme) -> f32 {
        let resolved_height = HEIGHT * theme.responsive_scale();
        let relative_height = if height >= resolved_height {
            height / resolved_height
        } else {
            height / HEIGHT
        };
        theme.tokens.typography.scale.sm * relative_height.clamp(0.75, 1.6) as f32
    }

    /// Intrinsic width of an immediate-mode chip — what [`Chip::draw_at`]
    /// needs to paint `label` and `lead` at `height` without truncating.
    pub fn natural_width_at(
        label: &str,
        height: f64,
        lead: ChipLead,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) -> f64 {
        let size = renderer_type_size(text_renderer, Self::label_size_for_height(height, theme));
        let (w, _) = text_renderer.measure(label, size, None);
        let lead_w = match lead {
            ChipLead::None => 0.0,
            ChipLead::Dot(_) => DOT_D + GAP,
            ChipLead::Glyph(_) => height * 0.44 + GAP,
        };
        PAD_X * 2.0 + lead_w + f64::from(w)
    }

    /// No layout node, caching, or widget instance — for a list that lays
    /// out its own rows (the sibling of
    /// [`Avatar::draw_at`](super::Avatar::draw_at)): on a curve, in a rail,
    /// or at any position taffy won't hand back.
    ///
    /// The label is **truncated with an ellipsis** to whatever width `rect`
    /// allows — use [`Chip::natural_width_at`] to size `rect` when the space is free.
    // Standard immediate-mode paint-helper signature for this vello
    // renderer (scene + transform + text_renderer + theme, plus this
    // draw's own content); every parameter is independently load-bearing.
    pub fn draw_at(
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        label: &str,
        lead: ChipLead,
        style: ChipStyle,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) {
        if rect.width() <= 1.0 || rect.height() <= 1.0 {
            return;
        }
        let a = style.alpha.clamp(0.0, 1.0);
        if a <= 0.0 {
            return;
        }
        let fade = |c: Color, f: f32| c.with_alpha_factor(f * a);

        let colors = &theme.tokens.colors;
        let pill = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.full));

        // Surface + border. A tinted chip carries its accent at the same
        // 0.18 fill / full-strength border the selected widget state uses,
        // so a rail of these reads identically to a row of real Chips.
        if !style.bare {
            // Quiet chip: the raised tier with an edge derived from it (HIG
            // §9.5) — a white veneer over a light card was invisible.
            let quiet = (
                colors.surfaces.raised,
                crate::surface::edge_on(colors.surfaces.raised),
            );
            let (bg, border) = if style.ghost {
                quiet
            } else if let Some(c) = style.resolved {
                (c.fill, c.border)
            } else if let Some(t) = style.tint {
                (t.with_alpha_factor(0.18), t)
            } else {
                quiet
            };
            scene.fill(Fill::NonZero, transform, fade(bg, 1.0), None, &pill);
            let stroke = if style.ghost {
                Stroke::new(1.0).with_dashes(0.0, [4.0, 4.0])
            } else {
                Stroke::new(1.0)
            };
            scene.stroke(&stroke, transform, fade(border, 1.0), None, &pill);
        }

        let cy = rect.center().y;
        // An icon-only chip centres its lead: with no label to sit beside,
        // a glyph pinned to the left padding reads as a pill someone
        // forgot to finish rather than as a compact action.
        let lead_w = match lead {
            ChipLead::None => 0.0,
            ChipLead::Dot(_) => DOT_D,
            ChipLead::Glyph(_) => rect.height() * 0.44,
        };
        let mut x = if label.is_empty() && lead_w > 0.0 {
            rect.center().x - lead_w / 2.0
        } else {
            rect.x0 + PAD_X
        };
        // A bare chip has no fill to sit the lead against, so a `text_dim`
        // glyph beside a `text_primary` label reads as two unrelated marks
        // rather than as one token. Pull the lead up to the label there.
        let lead_col = match (style.resolved, style.tint) {
            (Some(c), _) if !style.bare => c.ink,
            (_, Some(t)) => t,
            (_, None) if style.bare => colors.text_primary,
            (_, None) => colors.text_dim,
        };
        match lead {
            ChipLead::None => {}
            ChipLead::Dot(c) => {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    fade(c, 1.0),
                    None,
                    &Circle::new(Point::new(x + DOT_D / 2.0, cy), DOT_D / 2.0),
                );
                x += DOT_D + GAP;
            }
            ChipLead::Glyph(icon) => {
                let d = rect.height() * 0.44;
                let gr = Rect::from_center_size(Point::new(x + d / 2.0, cy), (d, d));
                icon.draw_with_stroke(scene, transform, gr, fade(lead_col, 1.0), 1.6);
                x += d + GAP;
            }
        }

        // Label, truncated to the space actually left inside the pill.
        let size = renderer_type_size(
            text_renderer,
            Self::label_size_for_height(rect.height(), theme),
        );
        let avail = rect.x1 - PAD_X - x;
        if avail <= 1.0 {
            return;
        }
        let shown = truncate_to_width(label, size, avail, text_renderer);
        if shown.is_empty() {
            return;
        }
        let (_, lh) = text_renderer.measure(&shown, size, None);
        let label_col = match (style.resolved, style.tint, style.ghost) {
            (Some(c), _, false) if !style.bare => c.ink,
            (_, _, true) => colors.text_dim,
            (_, Some(t), _) => t,
            (_, None, _) => colors.text_primary,
        };
        text_renderer.draw_text(
            scene,
            &shown,
            size,
            fade(label_col, 1.0),
            translated(transform, x, cy - f64::from(lh) / 2.0),
            None,
        );
    }
}

/// Longest prefix of `label` that fits `avail` px at `size`, with an
/// ellipsis appended when anything was dropped. Returns an empty string
/// when not even one character plus the ellipsis fits — callers treat that
/// as "no room for a label here" rather than painting a lone `…`.
fn truncate_to_width(
    label: &str,
    size: f32,
    avail: f64,
    text_renderer: &mut TextRenderer,
) -> String {
    let out = text_renderer.elide_to_width(label, size, crate::num::f32_of_f64(avail));
    if out != label && out.starts_with(crate::text_render::ELLIPSIS) {
        String::new()
    } else {
        out
    }
}

impl Widget for Chip {
    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        self.interaction = environment;
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "approx/HEIGHT are small pixel dimensions well within f32's \
                  exact-integer range; taffy's Style requires f32"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let label_size = theme.tokens.typography.scale.sm;
        let pad_y = f64::from(theme.tokens.spacing.s1);
        if label_size.to_bits() != self.label_size.to_bits()
            || pad_y.to_bits() != self.pad_y.to_bits()
        {
            // A new type step re-shapes the label and a new spacing scale
            // moves the pill's inset; the settled pill no longer describes
            // either (a relayout-only host such as `FrameHarness::set_theme`
            // never rebuilds the widget, so this is the only invalidation).
            self.footprint = None;
        }
        self.label_size = label_size;
        self.pad_y = pad_y;
        self.min_target = f64::from(theme.tokens.metrics.min_pointer_target);
        // Both axes are pinned as `length`, so taffy never asks `measure`
        // for this leaf; the settled footprint seeds the pass and
        // `adjust_layout_with_measure` corrects it from the real label.
        // Before the first settlement, estimate from a fixed advance.
        let size = self.footprint.unwrap_or_else(|| {
            let char_count = u32::try_from(self.label.chars().count()).unwrap_or(u32::MAX);
            let approx = PAD_X * 2.0
                + f64::from(char_count) * (f64::from(label_size) * 0.6)
                + if self.dot.is_some() { DOT_D + GAP } else { 0.0 }
                + if self.removable { GAP + X_D } else { 0.0 };
            let estimated_line_height = crate::text_metrics::line_height_px(label_size, 1.0);
            taffy::Size {
                width: approx as f32,
                height: HEIGHT.max(estimated_line_height + pad_y * 2.0) as f32,
            }
        });
        let node = taffy.leaf_node(Style {
            flex_shrink: 0.0,
            size: taffy::Size {
                width: length(size.width),
                height: length(size.height),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    /// Replace the advance estimate with the measured pill (see
    /// `Chip::intrinsic_size`) — the estimate was all the retained chip
    /// ever laid out with, since a `length × length` leaf skips `measure`.
    fn adjust_layout_with_measure(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text_renderer: &mut TextRenderer,
    ) -> bool {
        let footprint = if let Some(footprint) = self.footprint {
            footprint
        } else {
            let footprint = self.measured_footprint(text_renderer);
            self.footprint = Some(footprint);
            footprint
        };
        let Ok(current) = taffy.style(node) else {
            return false;
        };
        let size = taffy::Size {
            width: length(footprint.width),
            height: length(footprint.height),
        };
        if current.size == size {
            return false;
        }
        let mut style = current.clone();
        style.size = size;
        taffy.set_style(node, style).is_ok()
    }

    /// Not reached while both axes are pinned (see `build_layout`); kept so a
    /// host that lays the chip out `auto` gets the same settled pill from the
    /// same cache rather than the trait's zero default.
    fn measure(
        &mut self,
        _k: taffy::Size<Option<f32>>,
        _a: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        if let Some(footprint) = self.footprint {
            footprint
        } else {
            let footprint = self.measured_footprint(text_renderer);
            self.footprint = Some(footprint);
            footprint
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
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
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.rect.width() <= 0.0 {
            return;
        }
        let accent = theme.tokens.colors.intent.inverted;
        let selected = self.selectable && self.selected;

        let pill = RoundedRect::from_rect(self.rect, f64::from(theme.tokens.radii.full));
        // Quiet chip: the raised tier with an edge derived from it (HIG
        // §9.5); selected keeps its accent tint and accent edge.
        let radius = f64::from(theme.tokens.radii.full);
        if selected {
            scene.fill(
                Fill::NonZero,
                transform,
                accent.with_alpha_factor(0.18),
                None,
                &pill,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                accent,
                None,
                &crate::surface::edge_outline(self.rect, radius),
            );
        } else {
            crate::surface::card(
                scene,
                transform,
                self.rect,
                radius,
                theme.tokens.colors.surfaces.raised,
            );
        }

        let mut x = self.rect.x0 + PAD_X;
        let cy = self.rect.center().y;

        if let Some(dot) = self.dot {
            scene.fill(
                Fill::NonZero,
                transform,
                dot,
                None,
                &Circle::new(Point::new(x + DOT_D / 2.0, cy), DOT_D / 2.0),
            );
            x += DOT_D + GAP;
        }

        let size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);
        let (lw, lh) = text_renderer.measure(&self.label, size, None);
        self.label_w = f64::from(lw);
        let label_col = if selected { accent } else { theme.text_primary };
        text_renderer.draw_text(
            scene,
            &self.label,
            size,
            label_col,
            translated(transform, x, cy - f64::from(lh) / 2.0),
            None,
        );
        x += f64::from(lw);

        if self.removable {
            x += GAP;
            let xr = Rect::from_center_size(Point::new(x + X_D / 2.0, cy), (X_D, X_D));
            self.x_rect = xr;
            // Highlight over the same grown rect the press handler and
            // `cursor_shape` test, so feedback and behaviour agree in the
            // bands above and below the glyph.
            let hovered = self.remove_hit_rect().contains(self.last_cursor);
            let col = if hovered {
                theme.text_primary
            } else {
                theme.text_dim
            };
            Icon::X.draw_with_stroke(scene, transform, xr, col, 1.6);
        } else {
            self.x_rect = Rect::ZERO;
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.removable && self.remove_hit_rect().contains(self.last_cursor) {
                    self.removed = true;
                    *ctx.handled = true;
                } else if self.selectable && self.hit_rect().contains(self.last_cursor) {
                    self.selected = !self.selected;
                    self.toggled = true;
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if (self.selectable || self.removable) && self.hit_rect().contains(point) {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
    }

    fn name(&self) -> &'static str {
        "Chip"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        if self.selectable {
            Some(
                AccessNode::new(Role::ToggleButton)
                    .with_name(self.label.clone())
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .selected(self.selected)
                            .checked(self.selected),
                    ),
            )
        } else {
            Some(AccessNode::new(Role::StaticText).with_value(self.label.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;

    fn lay_out(w: &mut dyn Widget) {
        lay_out_with_theme(w, &Theme::dark());
    }

    fn lay_out_with_theme(w: &mut dyn Widget, theme: &Theme) {
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();
        tr.set_text_scale(theme.text_scale);
        let node = w.build_layout(&mut taffy, theme, &mut node_map);
        taffy
            .compute_layout_with_measure(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(300.0),
                    height: taffy::AvailableSpace::Definite(60.0),
                },
                |k, a, id, _, _| {
                    node_map.get(&id).map_or(taffy::Size::ZERO, |&p| unsafe {
                        (*p).measure(k, a, &mut tr)
                    })
                },
            )
            .unwrap();
        w.apply_layout(&taffy, node, Point::ZERO);
    }

    #[test]
    fn chip_geometry_is_measured_and_scales_once() {
        fn retained_chip_reserves_large_text_metrics_and_hit_height() {
            let mut normal = Chip::new("Collaboration").removable();
            lay_out(&mut normal);
            let mut large = Chip::new("Collaboration").removable();
            lay_out_with_theme(&mut large, &Theme::dark().with_text_scale(2.0));

            assert!(
                large.get_rect().width() > normal.get_rect().width() * 1.45,
                "200% label width must be measured rather than painted outside a 100% chip"
            );
            assert!(
                large.get_rect().height() > normal.get_rect().height()
                    && large.hit_rect().height() >= large.get_rect().height(),
                "200% chip and its pointer target must grow with its measured line, got {:?}",
                large.get_rect()
            );
        }

        /// The retained chip pinned both axes as `length`, so taffy never asked
        /// `measure` and every chip was laid out at a 0.6 em-per-character
        /// estimate. The settled width is now the measured pill.
        fn chip_width_is_measured_not_estimated() {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let (label_w, _) = text.measure("Rust", theme.tokens.typography.scale.sm, None);
            let expected = (PAD_X * 2.0 + f64::from(label_w)).ceil();
            let rect = settled_chip_rect(Chip::new("Rust"), 1.0);
            assert!(
                (rect.width() - expected).abs() < 1e-6,
                "settled width {} should be the measured pill {expected}",
                rect.width()
            );
            assert_eq!(
                rect.height(),
                HEIGHT,
                "1× pill keeps the 28 px selector height"
            );
        }

        /// The pill height follows the label's measured line once that line
        /// outgrows the 28 px selector height: the 12 px step is 24 px at 2×,
        /// its line 30 px, so the pill is 38 — measured once, at the
        /// renderer-relative size, not the doubled 60 px line the raw step
        /// would shape.
        fn pill_is_28_at_one_x_and_follows_its_measured_label_at_two_x() {
            let theme = Theme::dark().with_text_scale(2.0);
            let mut text = TextRenderer::new();
            text.set_text_scale(2.0);
            let size =
                crate::text_metrics::renderer_type_size(&text, theme.tokens.typography.scale.sm);
            let (_, line_h) = text.measure("Filter", size, None);
            assert!((line_h - 30.0).abs() < 1e-3, "2× sm line: {line_h}");
            let expected = (f64::from(line_h) + f64::from(theme.tokens.spacing.s1) * 2.0).ceil();
            assert!(
                expected > HEIGHT,
                "fixture: the 2× line must outgrow the 28 px pill"
            );
            let rect = settled_chip_rect(Chip::new("Filter"), 2.0);
            assert!(
                (rect.height() - expected).abs() < 1e-6,
                "2× pill height {} should be the measured line + 2×s1 = {expected}",
                rect.height()
            );
        }

        /// The painted pill is 28 px (a selector), the pointer target 32 px: the
        /// two pixels above and below the pill belong to the chip, and a click
        /// there toggles it.
        fn hit_rect_meets_the_minimum_pointer_target_while_the_pill_stays_28() {
            let mut c = Chip::new("Rust").selectable();
            lay_out(&mut c);
            draw(&mut c);
            assert_eq!(c.get_rect().height(), HEIGHT);
            let hit = c.hit_rect();
            assert_eq!(
                hit.height(),
                f64::from(crate::tokens::Metrics::BASE.min_pointer_target),
                "hit rect grows to the pointer target"
            );
            assert_eq!(hit.width(), c.get_rect().width(), "and not sideways");
            let just_above = Point::new(c.get_rect().center().x, c.get_rect().y0 - 1.0);
            assert!(!c.get_rect().contains(just_above) && hit.contains(just_above));
            dispatch(&mut c, &cursor(just_above));
            assert!(
                dispatch(&mut c, &press()),
                "a click in the grown target is consumed"
            );
            assert!(c.is_selected());
        }

        fn immediate_chip_large_text_scales_once_inside_scaled_geometry() {
            let theme = Theme::dark().with_text_scale(2.0);
            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);
            let mut scene = Scene::new();
            Chip::draw_at(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(0.0, 0.0, 240.0, HEIGHT * 2.0),
                "Group: All",
                ChipLead::None,
                ChipStyle::quiet(),
                &theme,
                &mut tr,
            );
            let run = scene
                .encoding()
                .resources
                .glyph_runs
                .first()
                .expect("chip label glyph run");
            assert_eq!(
                run.font_size, theme.tokens.typography.scale.sm,
                "theme and renderer scale must not multiply each other"
            );
        }

        retained_chip_reserves_large_text_metrics_and_hit_height();
        chip_width_is_measured_not_estimated();
        pill_is_28_at_one_x_and_follows_its_measured_label_at_two_x();
        hit_rect_meets_the_minimum_pointer_target_while_the_pill_stays_28();
        immediate_chip_large_text_scales_once_inside_scaled_geometry();
    }

    fn draw(w: &mut dyn Widget) {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        w.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    fn synth() -> winit::event::DeviceId {
        unsafe { std::mem::zeroed() }
    }
    fn cursor(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: synth(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }
    fn press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }
    fn dispatch(w: &mut dyn Widget, e: &WindowEvent) -> bool {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.leaf_node(Style::default());
        let mut handled = false;
        let mut ft = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        w.handle_event(
            e,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut ft,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    #[test]
    fn chip_pointer_toggles_and_removes_by_kind() {
        fn selectable_chip_toggles_on_body_click() {
            let mut c = Chip::new("Rust").selectable();
            lay_out(&mut c);
            draw(&mut c);
            assert!(!c.is_selected());
            let center = c.get_rect().center();
            dispatch(&mut c, &cursor(center));
            assert!(
                dispatch(&mut c, &press()),
                "body click on a selectable chip is consumed"
            );
            assert!(c.is_selected(), "click selects");
            assert!(c.take_toggled(), "toggle signal fires");
            assert!(!c.take_toggled(), "toggle drains after one read");
            // Click again → deselect.
            dispatch(&mut c, &press());
            assert!(!c.is_selected(), "second click deselects");
        }

        fn plain_chip_does_not_toggle() {
            let mut c = Chip::new("Label");
            lay_out(&mut c);
            draw(&mut c);
            let center = c.get_rect().center();
            dispatch(&mut c, &cursor(center));
            assert!(!dispatch(&mut c, &press()), "a plain chip ignores clicks");
            assert!(!c.take_toggled());
        }

        fn removable_chip_fires_remove_on_x_click() {
            let mut c = Chip::new("alice@example.com").removable();
            lay_out(&mut c);
            draw(&mut c);
            let x = c.remove_rect().center();
            assert!(x.x > 0.0, "× rect resolves after draw");
            dispatch(&mut c, &cursor(x));
            assert!(dispatch(&mut c, &press()), "× click is consumed");
            assert!(c.take_removed(), "remove signal fires");
            assert!(!c.take_removed(), "remove drains after one read");
        }

        fn removable_and_selectable_x_wins_over_toggle() {
            let mut c = Chip::new("tag").selectable().removable();
            lay_out(&mut c);
            draw(&mut c);
            let x = c.remove_rect().center();
            dispatch(&mut c, &cursor(x));
            dispatch(&mut c, &press());
            assert!(c.take_removed(), "× removes");
            assert!(!c.take_toggled(), "× does not also toggle selection");
            assert!(!c.is_selected());
        }

        fn accessibility_toggle_vs_static() {
            use crate::a11y::Role;
            let sel = Chip::new("Filter").selected(true);
            let node = Widget::accessibility(&sel).unwrap();
            assert_eq!(node.role, Role::ToggleButton);
            assert_eq!(
                node.state.checked,
                Some(true),
                "a selected chip reports checked"
            );
            assert!(node.state.focusable);

            let plain = Chip::new("tag");
            assert_eq!(
                Widget::accessibility(&plain).unwrap().role,
                Role::StaticText
            );
        }

        selectable_chip_toggles_on_body_click();
        plain_chip_does_not_toggle();
        removable_chip_fires_remove_on_x_click();
        removable_and_selectable_x_wins_over_toggle();
        accessibility_toggle_vs_static();
    }

    /// Lay a chip out through the shared solver (the `App` / `FrameHarness`
    /// path), which runs `adjust_layout_with_measure`; the raw
    /// `compute_layout_with_measure` in `lay_out` above never does.
    fn settled_chip_rect(chip: Chip, text_scale: f32) -> Rect {
        let harness = crate::headless::FrameHarness::new_with_theme(
            crate::State::new(),
            Box::new(crate::widget::Row::new().push(Box::new(chip))),
            (600, 200),
            Theme::dark().with_text_scale(text_scale),
        );
        let rect = harness.inspection().root.children[0].rect;
        Rect::new(rect[0], rect[1], rect[0] + rect[2], rect[1] + rect[3])
    }
}
