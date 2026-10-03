//! Chips and badges: one family of pill-shaped marks.
//!
//! A [`Chip`] is a compact interactive pill for tags, filters and tokens: it
//! carries a label and can be **selectable** (a filter that toggles, lifted
//! when chosen) and/or **removable** (a trailing × that removes a token, a
//! recipient or an applied filter). A [`super::Badge`] is the
//! non-interactive member of the family: a count, a dot or a short status.
//!
//! Both are pills (`radii.full`): pill-shaped by meaning. Both take a
//! [`ChipStyle`], never a colour: [`ChipStyle::Neutral`] is the quiet raised
//! pill, [`ChipStyle::Tinted`] carries a [`Tone`] (a status or a person) as a
//! faint wash and edge with neutral ink, [`ChipStyle::Live`] is the one filled
//! pill, for something happening right now. Canvas hosts paint the same
//! pills with [`ChipFace`] and [`super::BadgeFace`].
//!
//! Interactions surface through the toolkit's drain channel —
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
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::tokens::colors::{
    NON_TEXT_MIN_CONTRAST, ON_FILL_MIN_CONTRAST, SMALL_TEXT_MIN_CONTRAST, mix8,
};
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

/// How strongly a tone washes the pill's body over its ground.
const TINT_FILL: f32 = 0.15;
/// How strongly the same tone draws the pill's edge: enough to close the
/// shape on a dark and a light ground, quieter than the label.
const TINT_EDGE: f32 = 0.38;

/// What a tinted chip or badge is about. Status tones are the theme's muted
/// status colours; a person or a series is a presence colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tone {
    /// No hue: a grey wash.
    Neutral,
    /// Live, syncing, in flight.
    Info,
    Success,
    Warning,
    /// Failed, recording, destructive.
    Danger,
    /// A person or a data series: a colour from `colors.presence`
    /// (`presence.of_rgb(user_color(id))`, `presence.series(i)`), carried
    /// at the active material's lightness and chroma.
    Presence(Color),
}

impl Tone {
    /// The hue this tone washes with, in `theme`.
    pub fn color(self, theme: &Theme) -> Color {
        let colors = &theme.tokens.colors;
        match self {
            Self::Neutral => colors.text_dim,
            Self::Info => colors.semantic.info,
            Self::Success => colors.semantic.success,
            Self::Warning => colors.semantic.warning,
            Self::Danger => colors.semantic.danger,
            Self::Presence(color) => colors.presence.of(color),
        }
    }
}

/// What a chip or badge is. The style is the whole appearance; there is no
/// colour, size or radius to set.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum ChipStyle {
    /// The quiet pill: the raised tier with its derived edge and primary
    /// ink. A selected chip is a lift of its ground.
    #[default]
    Neutral,
    /// A tone washed into the body and drawn as the edge, with neutral ink
    /// measured on the washed body.
    Tinted(Tone),
    /// Something happening right now (live, recording): the one filled
    /// pill, in the danger status colour with ink measured on it.
    Live,
}

/// The resolved colours of one pill.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChipColors {
    pub fill: Color,
    pub edge: Option<Color>,
    pub ink: Color,
    /// The lead (dot or glyph) where it is not a content colour of its own.
    pub lead: Color,
}

impl ChipStyle {
    /// The colours of a pill in this style on `ground`. `selected` lifts a
    /// neutral chip; `hovered` lifts it halfway.
    pub fn colors(self, theme: &Theme, ground: Color, selected: bool, hovered: bool) -> ChipColors {
        let colors = &theme.tokens.colors;
        match self {
            Self::Neutral => {
                let state = crate::chrome::SelectionState::new(selected, hovered, false);
                let tier = crate::chrome::selection_tier_on(theme, state, ground);
                let raised = colors.surfaces.raised;
                let (fill, edge) = if selected {
                    (tier.fill, None)
                } else if hovered {
                    (
                        colors.lift_on(raised, 0.5),
                        Some(crate::surface::edge_on(raised)),
                    )
                } else {
                    (raised, Some(crate::surface::edge_on(raised)))
                };
                let ink = colors.ink_on_any(&[fill], colors.text_primary, SMALL_TEXT_MIN_CONTRAST);
                ChipColors {
                    fill,
                    edge,
                    ink,
                    lead: colors.ink_on_any(&[fill], colors.text_dim, NON_TEXT_MIN_CONTRAST),
                }
            }
            Self::Tinted(tone) => {
                let tint = tone.color(theme);
                let fill = mix8(ground, tint, TINT_FILL);
                let edge = mix8(ground, tint, TINT_EDGE);
                ChipColors {
                    fill,
                    edge: Some(edge),
                    ink: colors.on_surface_at(fill, SMALL_TEXT_MIN_CONTRAST).ink,
                    lead: colors.ink_on(fill, tint, NON_TEXT_MIN_CONTRAST),
                }
            }
            Self::Live => {
                let fill = colors.semantic.danger;
                let ink = colors.on_surface_at(fill, ON_FILL_MIN_CONTRAST).ink;
                ChipColors {
                    fill,
                    edge: None,
                    ink,
                    lead: ink,
                }
            }
        }
    }
}

/// What sits in a chip's leading slot.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum ChipLead {
    /// Label only.
    #[default]
    None,
    /// A colour-coded dot: a calendar's or a category's own colour (content),
    /// or a person's presence colour.
    Dot(Color),
    /// A glyph, for chips that name an action rather than a value.
    Glyph(Icon),
}

/// A pill's geometry in one theme: everything the text scale moves.
#[derive(Clone, Debug, PartialEq)]
struct PillMetrics {
    /// The selector height, grown with the label's line.
    height: f64,
    /// Inset of the content: half the height, so the label clears the
    /// round ends at every text scale.
    pad_x: f64,
    gap: f64,
    dot: f64,
    glyph: f64,
    label: TextStyle,
}

impl PillMetrics {
    fn of(theme: &Theme, role: TextRole) -> Self {
        let label = theme.text(role);
        let line = f64::from(label.line_height_px());
        let height = f64::from(theme.tokens.metrics.selector_height)
            .max((line + 2.0 * f64::from(theme.tokens.spacing.s1)).ceil());
        Self {
            height,
            pad_x: (height / 2.0).round(),
            gap: f64::from(theme.tokens.metrics.icon_label_gap),
            dot: f64::from(theme.tokens.spacing.s2),
            glyph: f64::from(theme.tokens.metrics.icon_sm),
            label,
        }
    }

    fn lead_width(&self, lead: ChipLead) -> f64 {
        match lead {
            ChipLead::None => 0.0,
            ChipLead::Dot(_) => self.dot,
            ChipLead::Glyph(_) => self.glyph,
        }
    }

    /// The narrowest pill showing everything without eliding.
    fn intrinsic_width(
        &self,
        label: &str,
        lead: ChipLead,
        trailing: bool,
        text: &mut TextRenderer,
    ) -> f64 {
        let lead_w = self.lead_width(lead);
        if label.is_empty() {
            // An icon-only or dot-only pill is a circle.
            return self.height;
        }
        let mut width =
            2.0 * self.pad_x + f64::from(text.measure_style(label, None, &self.label).0);
        if lead_w > 0.0 {
            width += lead_w + self.gap;
        }
        if trailing {
            width += self.gap + self.glyph;
        }
        width.ceil().max(self.height)
    }
}

/// An immediate-mode chip for a canvas that lays out its own rows (a rail,
/// a curve, a toolbar): the [`Chip`] widget's look with no layout node.
///
/// Size the rect from the face: [`ChipFace::height`] and
/// [`ChipFace::intrinsic_width`]. A rect narrower than that elides the label.
pub struct ChipFace<'a> {
    rect: Rect,
    label: &'a str,
    lead: ChipLead,
    style: ChipStyle,
    selected: bool,
    hovered: bool,
    removable: bool,
    remove_hovered: bool,
    bare: bool,
    opacity: f32,
}

impl<'a> ChipFace<'a> {
    pub fn new(rect: Rect, label: &'a str) -> Self {
        Self {
            rect,
            label,
            lead: ChipLead::None,
            style: ChipStyle::Neutral,
            selected: false,
            hovered: false,
            removable: false,
            remove_hovered: false,
            bare: false,
            opacity: 1.0,
        }
    }
    #[must_use]
    pub fn lead(mut self, lead: ChipLead) -> Self {
        self.lead = lead;
        self
    }
    #[must_use]
    pub fn style(mut self, style: ChipStyle) -> Self {
        self.style = style;
        self
    }
    /// The chosen one of a set of filter chips: a lift.
    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
    #[must_use]
    pub fn hovered(mut self, hovered: bool) -> Self {
        self.hovered = hovered;
        self
    }
    /// A trailing × that removes the token; `hovered` raises its ink.
    #[must_use]
    pub fn removable(mut self, hovered: bool) -> Self {
        self.removable = true;
        self.remove_hovered = hovered;
        self
    }
    /// Only the lead and the label, for a chip that already sits inside
    /// something doing the containing (a lit sector, a row that is itself
    /// the selection), where a pill would read as a box inside a box.
    #[must_use]
    pub fn bare(mut self) -> Self {
        self.bare = true;
        self
    }
    /// Fade the whole chip for an entrance or exit transition.
    #[must_use]
    pub fn opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity.clamp(0.0, 1.0);
        self
    }

    /// The pill height in `theme`: the selector height, grown with the label.
    pub fn height(theme: &Theme) -> f64 {
        PillMetrics::of(theme, TextRole::Label).height
    }

    /// The narrowest pill that shows `label` and `lead` without eliding.
    pub fn intrinsic_width(
        label: &str,
        lead: ChipLead,
        removable: bool,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        PillMetrics::of(theme, TextRole::Label).intrinsic_width(label, lead, removable, text)
    }

    /// Where the × is painted, for the host's remove hit-test.
    pub fn remove_rect(&self, theme: &Theme) -> Rect {
        if !self.removable {
            return Rect::ZERO;
        }
        let metrics = PillMetrics::of(theme, TextRole::Label);
        let x1 = self.rect.x1 - metrics.pad_x;
        Rect::from_center_size(
            Point::new(x1 - metrics.glyph / 2.0, self.rect.center().y),
            (metrics.glyph, metrics.glyph),
        )
    }

    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        paint_pill(
            &PillPaint {
                rect: self.rect,
                label: self.label,
                lead: self.lead,
                style: self.style,
                selected: self.selected,
                hovered: self.hovered,
                remove: self.removable.then_some(self.remove_hovered),
                bare: self.bare,
                opacity: self.opacity,
                role: TextRole::Label,
                centred: false,
            },
            scene,
            transform,
            text_renderer,
            theme,
        );
    }
}

/// Everything one pill paint needs; shared by [`ChipFace`], the [`Chip`]
/// widget and the badges.
pub(crate) struct PillPaint<'a> {
    pub rect: Rect,
    pub label: &'a str,
    pub lead: ChipLead,
    pub style: ChipStyle,
    pub selected: bool,
    pub hovered: bool,
    /// `Some(hovered)` paints a trailing ×.
    pub remove: Option<bool>,
    pub bare: bool,
    pub opacity: f32,
    pub role: TextRole,
    /// Centre the content (a badge) instead of starting it at the inset.
    pub centred: bool,
}

/// Only the body of a pill (fill and edge) in `rect`, for a toolkit host
/// that lays its own content inside the shape (window chrome chips).
pub(crate) fn paint_pill_body(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    style: ChipStyle,
    selected: bool,
    theme: &Theme,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let colors = style.colors(theme, theme.tokens.colors.surfaces.content, selected, false);
    let radius = f64::from(theme.tokens.radii.full).min(rect.height() / 2.0);
    scene.fill(
        Fill::NonZero,
        transform,
        colors.fill,
        None,
        &RoundedRect::from_rect(rect, radius),
    );
    if let Some(edge) = colors.edge {
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            edge,
            None,
            &crate::surface::edge_outline(rect, radius),
        );
    }
}

/// The one pill painter.
pub(crate) fn paint_pill(
    pill: &PillPaint<'_>,
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    let rect = pill.rect;
    if rect.width() <= 1.0 || rect.height() <= 1.0 || pill.opacity <= 0.0 {
        return;
    }
    let fade = |c: Color| c.with_alpha_factor(pill.opacity);
    let metrics = PillMetrics::of(theme, pill.role);
    let ground = theme.tokens.colors.surfaces.content;
    let colors = pill
        .style
        .colors(theme, ground, pill.selected, pill.hovered);
    let radius = f64::from(theme.tokens.radii.full).min(rect.height() / 2.0);
    if !pill.bare {
        scene.fill(
            Fill::NonZero,
            transform,
            fade(colors.fill),
            None,
            &RoundedRect::from_rect(rect, radius),
        );
        if let Some(edge) = colors.edge {
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                fade(edge),
                None,
                &crate::surface::edge_outline(rect, radius),
            );
        }
    }

    let cy = rect.center().y;
    // A chip insets its content by half its height; a badge is tighter
    // (a third of its height, at least `s2`), as measured by its sizer.
    let inset = if pill.centred {
        (rect.height() / 3.0)
            .round()
            .max(f64::from(theme.tokens.spacing.s2))
            .min(rect.width() / 4.0)
    } else {
        metrics.pad_x.min(rect.width() / 4.0)
    };
    let lead_w = metrics.lead_width(pill.lead);
    let trailing = if pill.remove.is_some() {
        metrics.gap + metrics.glyph
    } else {
        0.0
    };
    let label = if pill.label.is_empty() {
        String::new()
    } else {
        let room = rect.width()
            - 2.0 * inset
            - if lead_w > 0.0 {
                lead_w + metrics.gap
            } else {
                0.0
            }
            - trailing;
        if room <= 1.0 {
            String::new()
        } else {
            text_renderer.elide_to_width_style(
                pill.label,
                // Half a pixel of slack over shaping round-off.
                crate::num::f32_of_f64(room + 0.5),
                &metrics.label,
            )
        }
    };
    let (label_w, label_h) = if label.is_empty() {
        (0.0, 0.0)
    } else {
        let (w, h) = text_renderer.measure_style(&label, None, &metrics.label);
        (f64::from(w), f64::from(h))
    };
    let content_w = lead_w
        + if lead_w > 0.0 && label_w > 0.0 {
            metrics.gap
        } else {
            0.0
        }
        + label_w;
    // A pill with nothing but a lead, and a badge, centre their content.
    let mut x = if pill.centred || (label_w <= 0.0 && pill.remove.is_none()) {
        rect.center().x - content_w / 2.0
    } else {
        rect.x0 + inset
    };
    match pill.lead {
        ChipLead::None => {}
        ChipLead::Dot(dot) => {
            scene.fill(
                Fill::NonZero,
                transform,
                fade(dot),
                None,
                &Circle::new(Point::new(x + metrics.dot / 2.0, cy), metrics.dot / 2.0),
            );
            x += metrics.dot + metrics.gap;
        }
        ChipLead::Glyph(icon) => {
            let glyph = Rect::from_center_size(
                Point::new(x + metrics.glyph / 2.0, cy),
                (metrics.glyph, metrics.glyph),
            );
            let ink = if pill.bare { colors.ink } else { colors.lead };
            icon.draw_with_stroke(
                scene,
                transform,
                glyph,
                fade(ink),
                crate::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            x += metrics.glyph + metrics.gap;
        }
    }
    if label_w > 0.0 {
        text_renderer.draw_style(
            scene,
            &label,
            fade(colors.ink),
            translated(transform, x, cy - label_h / 2.0),
            None,
            &metrics.label,
        );
    }
    if let Some(hovered) = pill.remove {
        let x1 = rect.x1 - inset;
        let glyph = Rect::from_center_size(
            Point::new(x1 - metrics.glyph / 2.0, cy),
            (metrics.glyph, metrics.glyph),
        );
        let ink = if hovered { colors.ink } else { colors.lead };
        Icon::X.draw_with_stroke(
            scene,
            transform,
            glyph,
            fade(ink),
            crate::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
    }
}

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
    lead: ChipLead,
    style: ChipStyle,
    toggled: bool,
    removed: bool,
    rect: Rect,
    x_rect: Rect,
    last_cursor: Point,
    hovered: bool,
    /// The geometry resolved at `build_layout`, for `measure` and
    /// `adjust_layout_with_measure`, which have no theme of their own.
    metrics: PillMetrics,
    /// `metrics.min_pointer_target`: the hit rect grows to this even though
    /// the painted pill stays at the 28 px selector height.
    min_target: f64,
    /// Pointer precision; a coarse pointer raises the target past the
    /// theme's metric (48 px), as it does for every other control.
    interaction: crate::interaction::InteractionEnvironment,
    /// Settled pill size — real measured width, height following the label
    /// line — seeding the next `build_layout` so a stable label costs no
    /// extra pass. Cleared when the theme's geometry changes.
    footprint: Option<taffy::Size<f32>>,
}

impl Chip {
    pub fn new(label: impl Into<String>) -> Self {
        static STOCK: std::sync::LazyLock<PillMetrics> =
            std::sync::LazyLock::new(|| PillMetrics::of(&Theme::dark(), TextRole::Label));
        Self {
            label: label.into(),
            selectable: false,
            selected: false,
            removable: false,
            lead: ChipLead::None,
            style: ChipStyle::Neutral,
            toggled: false,
            removed: false,
            rect: Rect::ZERO,
            x_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            hovered: false,
            metrics: STOCK.clone(),
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

    /// What sits before the label: a content-colour dot or a glyph.
    #[must_use]
    pub fn with_lead(mut self, lead: ChipLead) -> Self {
        self.lead = lead;
        self.footprint = None;
        self
    }

    /// What the chip is: neutral, tinted with a tone, or live.
    #[must_use]
    pub fn style(mut self, style: ChipStyle) -> Self {
        self.style = style;
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

    #[allow(
        clippy::cast_possible_truncation,
        reason = "pill dimensions are small pixel counts well within f32's exact-integer range; taffy's Size requires f32"
    )]
    fn measured_footprint(&self, text_renderer: &mut TextRenderer) -> taffy::Size<f32> {
        let width =
            self.metrics
                .intrinsic_width(&self.label, self.lead, self.removable, text_renderer);
        taffy::Size {
            width: width.ceil() as f32,
            height: self.metrics.height.ceil() as f32,
        }
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
        reason = "pill dimensions are small pixel counts well within f32's \
                  exact-integer range; taffy's Style requires f32"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let metrics = PillMetrics::of(theme, TextRole::Label);
        if metrics != self.metrics {
            // A new type step re-shapes the label and a new spacing scale
            // moves the pill's inset; the settled pill no longer describes
            // either (a relayout-only host such as `FrameHarness::set_theme`
            // never rebuilds the widget, so this is the only invalidation).
            self.footprint = None;
            self.metrics = metrics;
        }
        self.min_target = f64::from(theme.tokens.metrics.min_pointer_target);
        // Both axes are pinned as `length`, so taffy never asks `measure`
        // for this leaf; the settled footprint seeds the pass and
        // `adjust_layout_with_measure` corrects it from the real label.
        // Until the label is measured the width is `auto`, so a host that
        // solves with a measure function asks `measure` for it; the shared
        // solver pins the measured pill in `adjust_layout_with_measure`.
        let width = self
            .footprint
            .map_or_else(taffy::Dimension::auto, |size| length(size.width));
        let node = taffy.leaf_node(Style {
            flex_shrink: 0.0,
            size: taffy::Size {
                width,
                height: length(self.metrics.height as f32),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    /// Replace the seed with the measured pill.
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
    /// host that lays the chip out `auto` gets the same settled pill.
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
        let face = ChipFace::new(self.rect, &self.label);
        self.x_rect = if self.removable {
            face.removable(false).remove_rect(theme)
        } else {
            Rect::ZERO
        };
        // Highlight over the same grown rect the press handler and
        // `cursor_shape` test, so feedback and behaviour agree in the bands
        // above and below the glyph.
        let remove_hovered = self.remove_hit_rect().contains(self.last_cursor);
        paint_pill(
            &PillPaint {
                rect: self.rect,
                label: &self.label,
                lead: self.lead,
                style: self.style,
                selected: self.selectable && self.selected,
                hovered: self.selectable && self.hovered && !remove_hovered,
                remove: self.removable.then_some(remove_hovered),
                bare: false,
                opacity: 1.0,
                role: TextRole::Label,
                centred: false,
            },
            scene,
            transform,
            text_renderer,
            theme,
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.hovered = self.hit_rect().contains(self.last_cursor);
            }
            WindowEvent::CursorLeft { .. } => self.hovered = false,
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
            let (label_w, _) = text.measure_style("Rust", None, &theme.text(TextRole::Label));
            let metrics = PillMetrics::of(&theme, TextRole::Label);
            let expected = (metrics.pad_x * 2.0 + f64::from(label_w)).ceil();
            let rect = settled_chip_rect(Chip::new("Rust"), 1.0);
            assert!(
                (rect.width() - expected).abs() < 1e-6,
                "settled width {} should be the measured pill {expected}",
                rect.width()
            );
            assert_eq!(
                rect.height(),
                f64::from(theme.tokens.metrics.selector_height),
                "1× pill keeps the 28 px selector height"
            );
        }

        /// The pill height follows the label's measured line once that line
        /// outgrows the 28 px selector height: the 13 px `Label` is 26 px at 2×,
        /// its line 33.8 px — measured once, from the resolved role, never a
        /// line doubled a second time by the renderer.
        fn pill_is_28_at_one_x_and_follows_its_measured_label_at_two_x() {
            let theme = Theme::dark().with_text_scale(2.0);
            let mut text = TextRenderer::new();
            text.set_text_scale(2.0);
            let style = theme.text(TextRole::Label);
            let (_, line_h) = text.measure_style("Filter", None, &style);
            assert!(
                (line_h - 13.0 * 2.0 * 1.3).abs() < 1e-3,
                "2× label line: {line_h}"
            );
            let expected = (f64::from(line_h) + f64::from(theme.tokens.spacing.s1) * 2.0).ceil();
            assert!(
                expected > f64::from(theme.tokens.metrics.selector_height),
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
            assert_eq!(
                c.get_rect().height(),
                f64::from(crate::tokens::Metrics::BASE.selector_height)
            );
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
            ChipFace::new(
                Rect::new(0.0, 0.0, 240.0, ChipFace::height(&theme)),
                "Group: All",
            )
            .paint(&mut scene, Affine::IDENTITY, &mut tr, &theme);
            let run = scene
                .encoding()
                .resources
                .glyph_runs
                .first()
                .expect("chip label glyph run");
            assert_eq!(
                run.font_size,
                theme.tokens.typography.size(TextRole::Label),
                "theme and renderer scale must not multiply each other"
            );
        }

        retained_chip_reserves_large_text_metrics_and_hit_height();
        chip_width_is_measured_not_estimated();
        pill_is_28_at_one_x_and_follows_its_measured_label_at_two_x();
        hit_rect_meets_the_minimum_pointer_target_while_the_pill_stays_28();
        immediate_chip_large_text_scales_once_inside_scaled_geometry();
    }

    /// A pill's height and inset grow with the text scale, so a label at
    /// 200 % still clears the round ends; a tone colours only the wash and
    /// the edge, and the label stays readable neutral ink.
    #[test]
    fn pills_grow_with_text_scale_and_tones_keep_neutral_readable_ink() {
        let one = PillMetrics::of(&Theme::dark(), TextRole::Label);
        let two = PillMetrics::of(&Theme::dark().with_text_scale(2.0), TextRole::Label);
        assert!(two.height > one.height && two.pad_x > one.pad_x);
        assert_eq!(one.pad_x, (one.height / 2.0).round());

        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let ground = theme.tokens.colors.surfaces.content;
            for tone in [
                Tone::Neutral,
                Tone::Info,
                Tone::Success,
                Tone::Warning,
                Tone::Danger,
                Tone::Presence(theme.tokens.colors.presence.series(3)),
            ] {
                let colors = ChipStyle::Tinted(tone).colors(&theme, ground, false, false);
                let ratio = crate::a11y::contrast_ratio(colors.ink, colors.fill);
                assert!(
                    ratio >= SMALL_TEXT_MIN_CONTRAST,
                    "{} {tone:?}: label {ratio:.2}:1",
                    theme.name
                );
                assert!(colors.edge.is_some(), "a tinted pill is closed by its edge");
            }
            let live = ChipStyle::Live.colors(&theme, ground, false, false);
            assert_eq!(live.fill, theme.tokens.colors.semantic.danger);
            assert!(crate::a11y::contrast_ratio(live.ink, live.fill) >= ON_FILL_MIN_CONTRAST);
            let neutral = ChipStyle::Neutral.colors(&theme, ground, false, false);
            assert_eq!(neutral.fill, theme.tokens.colors.surfaces.raised);
            let chosen = ChipStyle::Neutral.colors(&theme, ground, true, false);
            assert_ne!(chosen.fill, neutral.fill, "a selected chip is lifted");
            assert_eq!(chosen.edge, None);
        }
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
