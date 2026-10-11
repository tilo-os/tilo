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
//! pill, in your colour, for something that wants your attention (an unread
//! count, something live). Danger red is only for errors and failures, a
//! `Tinted(Tone::Danger)` status. Canvas hosts paint the same
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
use crate::widget::Tone;
use crate::widget::Widget;
use crate::widget::activation::Activation;

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
    /// Something that wants your attention (an unread count, a mention,
    /// something live): the one filled pill, in your colour
    /// (`intent.you`) with ink measured on it, the same mark as a sidebar's
    /// unread count. Never danger red: red is an error or a failure.
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
                let tint = tone.colour(theme);
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
                let fill = colors.intent.you;
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

    /// The narrowest pill showing everything without eliding. `trailing` is
    /// a remove × or a menu chevron.
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
    menu: bool,
    focused: bool,
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
            menu: false,
            focused: false,
            bare: false,
            opacity: 1.0,
        }
    }
    /// A trailing chevron: the chip opens a menu of choices.
    #[must_use]
    pub fn menu(mut self) -> Self {
        self.menu = true;
        self
    }
    /// Keyboard focus: the focus ring around the pill.
    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
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
        // A slot wider than the pill needs (a key on a keyboard) centres the
        // label in it.
        let intrinsic = PillMetrics::of(theme, TextRole::Label).intrinsic_width(
            self.label,
            self.lead,
            self.removable || self.menu,
            text_renderer,
        );
        paint_pill(
            &PillPaint {
                rect: self.rect,
                label: self.label,
                lead: self.lead,
                style: self.style,
                selected: self.selected,
                hovered: self.hovered,
                remove: self.removable.then_some(self.remove_hovered),
                menu: self.menu,
                bare: self.bare,
                opacity: self.opacity,
                role: TextRole::Label,
                centred: !self.removable && !self.menu && self.rect.width() > intrinsic + 1.0,
            },
            scene,
            transform,
            text_renderer,
            theme,
        );
        if self.focused {
            let radius = f64::from(theme.tokens.radii.full).min(self.rect.height() / 2.0);
            crate::widget::paint_focus_ring(scene, transform, self.rect, radius, theme);
        }
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
    /// A trailing chevron: the chip opens a menu.
    pub menu: bool,
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
    let trailing = if pill.remove.is_some() || pill.menu {
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
    let mut x = if pill.centred || (label_w <= 0.0 && pill.remove.is_none() && !pill.menu) {
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
    } else if pill.menu {
        let x1 = rect.x1 - inset;
        let glyph = Rect::from_center_size(
            Point::new(x1 - metrics.glyph / 2.0, cy),
            (metrics.glyph, metrics.glyph),
        );
        Icon::ChevronDown.draw_with_stroke(
            scene,
            transform,
            glyph,
            fade(colors.lead),
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
    menu: bool,
    lead: ChipLead,
    style: ChipStyle,
    toggled: bool,
    removed: bool,
    rect: Rect,
    x_rect: Rect,
    last_cursor: Point,
    hovered: bool,
    focused: bool,
    /// What pressing the chip does when it navigates or opens something (a
    /// tag, a backlink, a filter menu): a callback or a bound action.
    activation: Activation,
    tooltip: Option<String>,
    accessible_name: Option<String>,
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
        let label = label.into();
        Self {
            activation: Activation::new(label.clone()),
            tooltip: None,
            accessible_name: None,
            label,
            selectable: false,
            selected: false,
            removable: false,
            menu: false,
            lead: ChipLead::None,
            style: ChipStyle::Neutral,
            toggled: false,
            removed: false,
            rect: Rect::ZERO,
            x_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            hovered: false,
            focused: false,
            metrics: STOCK.clone(),
            min_target: f64::from(crate::tokens::Metrics::BASE.min_pointer_target),
            interaction: crate::interaction::InteractionEnvironment::default(),
            footprint: None,
        }
    }

    /// A stable id for tests, inspection and the chip's bound action
    /// (defaults to the label). Set it before [`Self::on_click`].
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.activation = Activation::new(id);
        self
    }

    /// Run `f` when the chip is pressed (a pointer, Enter or Space, or
    /// assistive technology): a tag that opens its notes, a backlink.
    #[must_use]
    pub fn on_click(mut self, f: impl FnMut() + 'static) -> Self {
        self.activation.set_callback(Box::new(f));
        self
    }

    /// [`Self::on_click`] through an exported resource action.
    #[must_use]
    pub fn on_action(mut self, binding: tilo_actions::ActionBinding<()>) -> Self {
        self.activation.set_action(binding);
        self
    }

    /// What the chip does or stands for, shown when the pointer rests on it.
    #[must_use]
    pub fn tooltip(mut self, text: impl Into<String>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    /// The name the chip is announced by when its label alone is not enough
    /// ("Tag: design").
    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self
    }

    /// A trailing chevron: pressing the chip opens a menu of choices.
    #[must_use]
    pub fn menu(mut self) -> Self {
        self.menu = true;
        self.footprint = None;
        self
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

    /// Whether pressing the body does something.
    fn pressable(&self) -> bool {
        self.selectable || self.activation.is_bound()
    }

    /// Press the body: toggle a filter, or run the bound activation.
    fn press(&mut self) -> bool {
        if self.selectable {
            self.selected = !self.selected;
            self.toggled = true;
            return true;
        }
        self.activation.activate()
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "pill dimensions are small pixel counts well within f32's exact-integer range; taffy's Size requires f32"
    )]
    fn measured_footprint(&self, text_renderer: &mut TextRenderer) -> taffy::Size<f32> {
        let width = self.metrics.intrinsic_width(
            &self.label,
            self.lead,
            self.removable || self.menu,
            text_renderer,
        );
        taffy::Size {
            width: width.ceil() as f32,
            height: self.metrics.height.ceil() as f32,
        }
    }
}

impl Widget for Chip {
    crate::provides!(dyn crate::widget::controls::ActionControls);

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
        let style = Style {
            // A pill wider than its line gives way and elides its label rather
            // than running past the edge.
            flex_shrink: 1.0,
            min_size: taffy::Size {
                width: length(self.metrics.height as f32),
                height: taffy::Dimension::auto(),
            },
            size: taffy::Size {
                width,
                height: length(self.metrics.height as f32),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
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
        self.rect = crate::widget::layout_rect(taffy, node, origin);
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
        let pressed = self.activation.state.pressed;
        paint_pill(
            &PillPaint {
                rect: self.rect,
                label: &self.label,
                lead: self.lead,
                style: self.style,
                selected: (self.selectable && self.selected) || pressed,
                hovered: self.pressable() && self.hovered && !remove_hovered,
                remove: self.removable.then_some(remove_hovered),
                menu: self.menu,
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
        if self.focused {
            let radius = f64::from(theme.tokens.radii.full).min(self.rect.height() / 2.0);
            crate::widget::paint_focus_ring(scene, transform, self.rect, radius, theme);
        }
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
                state,
                button: MouseButton::Left,
                ..
            } => {
                let over_x = self.removable && self.remove_hit_rect().contains(self.last_cursor);
                let over_body = self.hit_rect().contains(self.last_cursor);
                match state {
                    ElementState::Pressed if over_x => {
                        self.removed = true;
                        *ctx.handled = true;
                    }
                    ElementState::Pressed if self.selectable && over_body => {
                        self.selected = !self.selected;
                        self.toggled = true;
                        *ctx.focus_target = Some(ctx.node_id);
                        *ctx.handled = true;
                    }
                    ElementState::Pressed if self.activation.is_enabled() && over_body => {
                        self.activation.press(self.hit_rect());
                        *ctx.focus_target = Some(ctx.node_id);
                        *ctx.handled = true;
                    }
                    ElementState::Released => {
                        let release = self
                            .activation
                            .release(self.hit_rect(), over_body && !over_x);
                        *ctx.handled |= release.ended();
                        if release == crate::pointer_replay::Release::Click(()) {
                            self.activation.activate();
                        }
                    }
                    ElementState::Pressed => {}
                }
            }
            _ => {}
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if (self.pressable() || self.removable) && self.hit_rect().contains(point) {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
    }

    fn is_focusable(&self) -> bool {
        self.pressable()
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    /// Enter, Space and assistive technology press the chip as a click does;
    /// Delete is the × of a removable one.
    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _root: &crate::widget::ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        match action {
            Action::Activate | Action::ToggleCheck if self.pressable() => self.press(),
            _ => false,
        }
    }

    fn name(&self) -> &'static str {
        "Chip"
    }

    fn tooltip(&self) -> Option<&str> {
        self.tooltip.as_deref()
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.activation.id())
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let name = self
            .accessible_name
            .clone()
            .unwrap_or_else(|| self.label.clone());
        if self.selectable {
            Some(
                AccessNode::new(Role::ToggleButton)
                    .with_name(name)
                    .with_bounds(self.rect)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.focused)
                            .selected(self.selected)
                            .checked(self.selected),
                    ),
            )
        } else if self.activation.is_bound() {
            let mut state = AccessState::new()
                .focusable(true)
                .focused(self.focused)
                .disabled(!self.activation.is_enabled());
            if self.menu {
                state = state.expanded(false);
            }
            Some(
                AccessNode::new(Role::Button)
                    .with_name(name)
                    .with_bounds(self.rect)
                    .with_state(state),
            )
        } else {
            Some(AccessNode::new(Role::StaticText).with_value(self.label.clone()))
        }
    }
}

crate::widget::activation::activation_controls!(Chip, activation);
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::dispatch;

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
                dispatch(&mut c, &mouse_press()),
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
                Tone::Presence(crate::tokens::PresenceHue::from_slot(3)),
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
            // Unread and attention are your colour, as a sidebar's unread
            // count; red is only for errors.
            let live = ChipStyle::Live.colors(&theme, ground, false, false);
            assert_eq!(live.fill, theme.tokens.colors.intent.you);
            assert_ne!(live.fill, theme.tokens.colors.semantic.danger);
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

    fn cursor(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }
    use crate::synthetic_input::mouse_press;

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
                dispatch(&mut c, &mouse_press()),
                "body click on a selectable chip is consumed"
            );
            assert!(c.is_selected(), "click selects");
            assert!(c.take_toggled(), "toggle signal fires");
            assert!(!c.take_toggled(), "toggle drains after one read");
            // Click again → deselect.
            dispatch(&mut c, &mouse_press());
            assert!(!c.is_selected(), "second click deselects");
        }

        fn plain_chip_does_not_toggle() {
            let mut c = Chip::new("Label");
            lay_out(&mut c);
            draw(&mut c);
            let center = c.get_rect().center();
            dispatch(&mut c, &cursor(center));
            assert!(
                !dispatch(&mut c, &mouse_press()),
                "a plain chip ignores clicks"
            );
            assert!(!c.take_toggled());
        }

        fn removable_chip_fires_remove_on_x_click() {
            let mut c = Chip::new("alice@example.com").removable();
            lay_out(&mut c);
            draw(&mut c);
            let x = c.remove_rect().center();
            assert!(x.x > 0.0, "× rect resolves after draw");
            dispatch(&mut c, &cursor(x));
            assert!(dispatch(&mut c, &mouse_press()), "× click is consumed");
            assert!(c.take_removed(), "remove signal fires");
            assert!(!c.take_removed(), "remove drains after one read");
        }

        fn removable_and_selectable_x_wins_over_toggle() {
            let mut c = Chip::new("tag").selectable().removable();
            lay_out(&mut c);
            draw(&mut c);
            let x = c.remove_rect().center();
            dispatch(&mut c, &cursor(x));
            dispatch(&mut c, &mouse_press());
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

#[cfg(test)]
mod activation_tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn a_chip_that_navigates_is_a_focusable_button() {
        let opened = Rc::new(Cell::new(0));
        let counter = Rc::clone(&opened);
        let mut chip = Chip::new("design")
            .with_id("tag-design")
            .on_click(move || counter.set(counter.get() + 1));
        assert!(chip.is_focusable());
        let node = Widget::accessibility(&chip).unwrap();
        assert_eq!(node.role, crate::a11y::Role::Button);
        assert_eq!(node.name.as_deref(), Some("design"));
        assert_eq!(chip.test_id(), Some("tag-design"));
        let mut doc = automerge::AutoCommit::new();
        assert!(Widget::perform_action(
            &mut chip,
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(opened.get(), 1, "Enter, Space and AT press the chip");
        // A pointer click runs it too.
        let mut harness = crate::headless::FrameHarness::new(
            crate::State::new(),
            Box::new(crate::widget::Row::new().push(Box::new(chip))),
            (400, 100),
        );
        let [x, y, w, h] = harness.inspection().root.children[0].rect;
        harness.click_at(x + w / 2.0, y + h / 2.0);
        assert_eq!(opened.get(), 2);
        // A plain label chip is not a control.
        assert!(!Chip::new("label").is_focusable());
    }

    #[test]
    fn a_menu_chip_reserves_its_chevron_and_a_wide_face_centres_its_label() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let plain = ChipFace::intrinsic_width("Sort", ChipLead::None, false, &mut text, &theme);
        let menu = PillMetrics::of(&theme, TextRole::Label).intrinsic_width(
            "Sort",
            ChipLead::None,
            true,
            &mut text,
        );
        assert!(menu > plain);
        let chip = Chip::new("Sort").menu().on_click(|| {});
        let node = Widget::accessibility(&chip).unwrap();
        assert_eq!(node.state.expanded, Some(false));
        // A key wider than its label: the label sits in the middle.
        let paint_x = |width: f64, text: &mut TextRenderer| {
            let mut scene = Scene::new();
            ChipFace::new(Rect::new(0.0, 0.0, width, ChipFace::height(&theme)), "q").paint(
                &mut scene,
                Affine::IDENTITY,
                text,
                &theme,
            );
            scene.encoding().resources.glyph_runs[0]
                .transform
                .translation[0]
        };
        let narrow = paint_x(plain.max(28.0), &mut text);
        let wide = paint_x(120.0, &mut text);
        assert!(f64::from(wide) > 40.0, "centred: {wide}");
        assert!(wide > narrow);
    }
}
