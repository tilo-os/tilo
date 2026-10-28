//! The reaction strip: one pill per emoji people reacted with, its count
//! beside it, and a trailing button to add one.
//!
//! Everything that takes reactions ([`tilo_resource::Reactions`]) shows them
//! with this strip, so a reaction reads the same under a chat message, on a
//! page and in a window's toolbar. Your own reaction is the chosen pill: the
//! accent washed in, with an accent edge ([`ChipStyle::Neutral`] selected).
//! A workspace emoji (`:name:`) is drawn as its picture from the window's
//! workspace emoji ([`crate::workspace_emoji`]), and as its `:name:` while
//! the picture is not there. Pointing at a pill names who reacted, with the
//! shared tooltip. A pill arriving appears from its centre and a count
//! changing cross-fades (`motion::Appear`, `motion::CrossFade`), both only
//! once the strip is on screen, and as reduced motion allows.
//!
//! Two ways in:
//!
//! - **By hand** (a canvas that paints its own rows, a toolbar):
//!   [`ReactionStrip`] keeps one strip's motion; [`ReactionStrip::layout`]
//!   lays the pills out from an origin within a width, wrapping onto
//!   further rows, and [`ReactionStrip::paint`], [`ReactionStrip::hit`],
//!   [`ReactionStrip::tooltip_at`] and [`ReactionStrip::accessibility`] work
//!   on that layout. [`measure_height`] sizes a row before painting.
//! - **Retained**: [`ReactionStripWidget`], a widget a view places like any
//!   other; it reports a toggle as [`ReactionToggle`] (to an action binding or
//!   a callback) and opens the shared emoji picker from its add button.

use std::collections::HashMap;

use tilo_resource::{ReactionGroup, Reactor};
use vello::kurbo::{Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use crate::a11y::{AccessNode, AccessState, Role};
use crate::color_compat::ColorCompat as _;
use crate::icons::Icon;
use crate::num::f32_of_f64;
use crate::paint::PaintCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::{EffectiveMotion, MotionSource, TextRole};
use crate::widget::ChipStyle;

#[path = "reaction_strip_widget.rs"]
mod retained;
pub use retained::{ReactionStripWidget, ReactionToggle};

/// What a point on the strip is.
#[derive(Clone, Debug, PartialEq)]
pub enum StripHit {
    /// A pill: react with its emoji, or take your reaction back.
    Toggle(String),
    /// The add button, at `anchor`: open the emoji picker there.
    Add { anchor: Rect },
}

/// A strip's geometry in one theme: everything the text scale moves.
#[derive(Clone, Debug, PartialEq)]
pub struct StripMetrics {
    height: f64,
    pad_x: f64,
    /// Between a pill's emoji and its count.
    inner_gap: f64,
    /// Between pills, and between rows.
    gap: f64,
    /// The side of the box an emoji is drawn in.
    emoji: f64,
    count: TextStyle,
    icon: f64,
}

impl StripMetrics {
    #[must_use]
    pub fn of(theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        let count = theme.text(TextRole::Label).numeric();
        let line = f64::from(count.line_height_px());
        // The selector height, grown with the label, as every pill.
        let height = f64::from(theme.tokens.metrics.selector_height)
            .max((line + 2.0 * f64::from(spacing.s1)).ceil());
        Self {
            height,
            pad_x: f64::from(spacing.s2),
            inner_gap: f64::from(spacing.s1),
            gap: f64::from(spacing.s1),
            emoji: (height - 2.0 * f64::from(spacing.s1)).round(),
            count,
            icon: f64::from(theme.tokens.metrics.icon_sm) + 2.0,
        }
    }

    /// A pill's height.
    #[must_use]
    pub fn height(&self) -> f64 {
        self.height
    }
}

/// One laid-out pill.
#[derive(Clone, Debug, PartialEq)]
pub struct StripPill {
    pub emoji: String,
    pub count: usize,
    /// You reacted with it yourself.
    pub mine: bool,
    pub rect: Rect,
    /// Who reacted, first first.
    pub reactors: Vec<Reactor>,
    /// Where its emoji is drawn.
    glyph: Rect,
}

/// The pills and the add button of one strip, laid out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StripLayout {
    pills: Vec<StripPill>,
    add: Option<Rect>,
    bounds: Rect,
    /// Whose strip it is (`me` reacting toggles their own reaction).
    me: String,
    can_react: bool,
}

/// How wide `emoji` is drawn in a pill: a square for a picture or a
/// Unicode emoji, the `:name:` text for a workspace emoji without one.
fn emoji_width(emoji: &str, metrics: &StripMetrics, text: &mut TextRenderer) -> f64 {
    if tilo_resource::custom_emoji_name(emoji).is_some() && custom_picture(emoji).is_none() {
        f64::from(text.measure_style(emoji, None, &metrics.count).0).ceil()
    } else {
        metrics.emoji
    }
}

/// The decoded picture of a workspace emoji, when `emoji` names one the
/// window's workspace has and it is decoded.
fn custom_picture(emoji: &str) -> Option<vello::peniko::ImageData> {
    let name = tilo_resource::custom_emoji_name(emoji)?;
    crate::workspace_emoji::emoji().image(name)
}

/// What the count of `count` reactions reads.
fn count_label(count: usize) -> String {
    count.to_string()
}

/// Lay out `groups` from `origin`, wrapping at `max_width`: one pill per
/// group, then the add button when `can_react`.
#[must_use]
pub fn layout(
    groups: &[ReactionGroup],
    me: &str,
    can_react: bool,
    (origin, max_width): (Point, f64),
    text: &mut TextRenderer,
    metrics: &StripMetrics,
) -> StripLayout {
    let right = origin.x + max_width.max(metrics.height);
    let mut x = origin.x;
    let mut y = origin.y;
    let mut x_max = origin.x;
    let mut place = |width: f64| {
        if x > origin.x && x + width > right {
            x = origin.x;
            y += metrics.height + metrics.gap;
        }
        let rect = Rect::new(x, y, x + width, y + metrics.height);
        x = rect.x1 + metrics.gap;
        x_max = x_max.max(rect.x1);
        rect
    };
    let mut pills = Vec::with_capacity(groups.len());
    for group in groups {
        let glyph_w = emoji_width(&group.emoji, metrics, text);
        let count = group.count();
        let count_w = f64::from(
            text.measure_style(&count_label(count), None, &metrics.count)
                .0,
        )
        .ceil();
        let width = metrics.pad_x * 2.0 + glyph_w + metrics.inner_gap + count_w;
        let rect = place(width.max(metrics.height));
        let glyph = Rect::from_center_size(
            Point::new(rect.x0 + metrics.pad_x + glyph_w / 2.0, rect.center().y),
            (glyph_w, metrics.emoji),
        );
        pills.push(StripPill {
            emoji: group.emoji.clone(),
            count,
            mine: group.includes(me),
            rect,
            reactors: group.reactors.clone(),
            glyph,
        });
    }
    let add = can_react.then(|| place(metrics.height + metrics.pad_x));
    let bottom = pills
        .iter()
        .map(|pill| pill.rect.y1)
        .chain(add.map(|add| add.y1))
        .fold(origin.y, f64::max);
    StripLayout {
        pills,
        add,
        bounds: Rect::new(origin.x, origin.y, x_max, bottom),
        me: me.to_owned(),
        can_react,
    }
}

/// How tall the strip of `groups` is within `max_width`: nothing when there
/// is nothing to show.
#[must_use]
pub fn measure_height(
    groups: &[ReactionGroup],
    can_react: bool,
    max_width: f64,
    text: &mut TextRenderer,
    theme: &Theme,
) -> f64 {
    if groups.is_empty() && !can_react {
        return 0.0;
    }
    layout(
        groups,
        "",
        can_react,
        (Point::ZERO, max_width),
        text,
        &StripMetrics::of(theme),
    )
    .bounds
    .height()
}

impl StripLayout {
    /// Everything laid out.
    #[must_use]
    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    #[must_use]
    pub fn height(&self) -> f64 {
        self.bounds.height()
    }

    #[must_use]
    pub fn pills(&self) -> &[StripPill] {
        &self.pills
    }

    /// The add button, when the person may react.
    #[must_use]
    pub fn add_rect(&self) -> Option<Rect> {
        self.add
    }

    /// What is at `point`. A pill of someone who may not react is not a
    /// control.
    #[must_use]
    pub fn hit(&self, point: Point) -> Option<StripHit> {
        if !self.can_react {
            return None;
        }
        if let Some(add) = self.add.filter(|add| add.contains(point)) {
            return Some(StripHit::Add { anchor: add });
        }
        self.pills
            .iter()
            .find(|pill| pill.rect.contains(point))
            .map(|pill| StripHit::Toggle(pill.emoji.clone()))
    }

    /// Every control with its rect, pills first: a canvas host registers
    /// them as its hit targets.
    #[must_use]
    pub fn targets(&self) -> Vec<(Rect, StripHit)> {
        if !self.can_react {
            return Vec::new();
        }
        self.pills
            .iter()
            .map(|pill| (pill.rect, StripHit::Toggle(pill.emoji.clone())))
            .chain(self.add.map(|add| (add, StripHit::Add { anchor: add })))
            .collect()
    }

    /// The tooltip of what is at `point`: who reacted with a pill's emoji
    /// (`name` names each of them; a reactor shown under a name of their
    /// own, a bridged account, keeps it), or what the add button does.
    #[must_use]
    pub fn tooltip_at(
        &self,
        point: Point,
        name: &dyn Fn(&Reactor) -> String,
    ) -> Option<(Rect, String)> {
        if let Some(add) = self.add.filter(|add| add.contains(point)) {
            return Some((add, crate::i18n::widget_format("reaction-strip-add", &[])));
        }
        let pill = self.pills.iter().find(|pill| pill.rect.contains(point))?;
        Some((pill.rect, who_reacted(pill, &self.me, name)))
    }

    /// What a pill or the add button says to assistive technology: a toggle
    /// per pill ("Remove 👍 reaction", with its count), then the add button.
    #[must_use]
    pub fn accessibility(&self) -> Vec<AccessNode> {
        let mut nodes: Vec<AccessNode> = self
            .pills
            .iter()
            .map(|pill| {
                AccessNode::new(Role::ToggleButton)
                    .with_name(pill_name(&pill.emoji, pill.mine))
                    .with_value(pill_count(pill.count))
                    .with_bounds(pill.rect)
                    .with_hashed_key(("reaction", &pill.emoji))
                    .with_state(
                        AccessState::new()
                            .checked(pill.mine)
                            .focusable(self.can_react)
                            .disabled(!self.can_react),
                    )
            })
            .collect();
        if let Some(add) = self.add {
            nodes.push(
                AccessNode::new(Role::Button)
                    .with_name(crate::i18n::widget_format("reaction-strip-add", &[]))
                    .with_bounds(add)
                    .with_hashed_key("reaction-add")
                    .with_state(AccessState::new().focusable(true)),
            );
        }
        nodes
    }
}

/// A pill's accessible value: how many reacted with its emoji.
#[must_use]
pub fn pill_count(count: usize) -> String {
    crate::i18n::widget_format("reaction-strip-count", &[("count", count.into())])
}

/// A pill's accessible name: what pressing it does.
#[must_use]
pub fn pill_name(emoji: &str, mine: bool) -> String {
    crate::i18n::widget_format(
        if mine {
            "reaction-strip-unreact"
        } else {
            "reaction-strip-react"
        },
        &[("emoji", emoji.into())],
    )
}

/// "Ada, Bo and 3 others reacted with thumbs up": you first, as "You".
fn who_reacted(pill: &StripPill, me: &str, name: &dyn Fn(&Reactor) -> String) -> String {
    let mut names: Vec<String> = Vec::with_capacity(pill.reactors.len());
    let mut own = false;
    for reactor in &pill.reactors {
        if reactor.persona.is_none() && reactor.by == me {
            own = true;
        } else {
            names.push(
                reactor
                    .name
                    .clone()
                    .filter(|shown| !shown.is_empty())
                    .unwrap_or_else(|| name(reactor)),
            );
        }
    }
    if own {
        names.insert(0, crate::i18n::widget_format("reaction-strip-you", &[]));
    }
    let emoji = crate::emoji_catalogue::emoji_name(&pill.emoji);
    let arg = |names: &[String], at: usize| names.get(at).cloned().unwrap_or_default();
    match names.len() {
        0 | 1 => crate::i18n::widget_format(
            "reaction-strip-reacted-one",
            &[("first", arg(&names, 0).into()), ("emoji", emoji.into())],
        ),
        2 => crate::i18n::widget_format(
            "reaction-strip-reacted-two",
            &[
                ("first", arg(&names, 0).into()),
                ("second", arg(&names, 1).into()),
                ("emoji", emoji.into()),
            ],
        ),
        3 => crate::i18n::widget_format(
            "reaction-strip-reacted-three",
            &[
                ("first", arg(&names, 0).into()),
                ("second", arg(&names, 1).into()),
                ("third", arg(&names, 2).into()),
                ("emoji", emoji.into()),
            ],
        ),
        n => crate::i18n::widget_format(
            "reaction-strip-reacted-more",
            &[
                ("first", arg(&names, 0).into()),
                ("second", arg(&names, 1).into()),
                ("others", (n - 2).into()),
                ("emoji", emoji.into()),
            ],
        ),
    }
}

/// How one pill moves: its arrival and its count.
#[derive(Clone, Debug)]
struct PillMotion {
    presence: crate::motion::Presence,
    count: crate::motion::CrossFade<usize>,
}

/// One strip's pills and how they move: what a host keeps per strip it
/// paints (a message's reactions), so a reaction arriving appears and a
/// count changing cross-fades.
#[derive(Clone, Debug, Default)]
pub struct ReactionStrip {
    layout: StripLayout,
    motion: HashMap<String, PillMotion>,
    /// Laid out before: pills from then on arrive.
    seen: bool,
}

impl ReactionStrip {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Lay `groups` out from `origin` within `max_width` (see [`layout`]);
    /// a pill new since the last layout starts arriving, a changed count
    /// cross-fading.
    pub fn layout(
        &mut self,
        groups: &[ReactionGroup],
        me: &str,
        can_react: bool,
        (origin, max_width): (Point, f64),
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> &StripLayout {
        self.layout = layout(
            groups,
            me,
            can_react,
            (origin, max_width),
            text,
            &StripMetrics::of(theme),
        );
        self.follow(theme.motion());
        &self.layout
    }

    /// Take a layout made elsewhere (a retained widget's measure).
    fn set_layout(&mut self, layout: StripLayout, motion: EffectiveMotion) {
        self.layout = layout;
        self.follow(motion);
    }

    fn follow(&mut self, motion: EffectiveMotion) {
        let now = crate::motion::frame_now();
        let source = crate::motion::recent_input(now).unwrap_or(MotionSource::Pointer);
        let seen = self.seen;
        self.motion
            .retain(|emoji, _| self.layout.pills.iter().any(|pill| pill.emoji == *emoji));
        let mut settles = None;
        for pill in &self.layout.pills {
            let state = self.motion.entry(pill.emoji.clone()).or_insert_with(|| {
                let mut presence = if seen {
                    crate::motion::Presence::closed()
                } else {
                    crate::motion::Presence::opened()
                };
                presence.show(true, crate::motion::Appear::SPEC, motion, source, now);
                PillMotion {
                    presence,
                    count: crate::motion::CrossFade::new(pill.count),
                }
            });
            state.count.show(pill.count, motion, source, now);
            settles = settles
                .into_iter()
                .chain(state.presence.settles_at())
                .chain(state.count.settles_at())
                .max();
        }
        if let Some(until) = settles {
            crate::motion::want_frames_until(until);
        }
        self.seen = true;
    }

    /// The last layout.
    #[must_use]
    pub fn laid_out(&self) -> &StripLayout {
        &self.layout
    }

    /// See [`StripLayout::hit`].
    #[must_use]
    pub fn hit(&self, point: Point) -> Option<StripHit> {
        self.layout.hit(point)
    }

    /// See [`StripLayout::tooltip_at`].
    #[must_use]
    pub fn tooltip_at(
        &self,
        point: Point,
        name: &dyn Fn(&Reactor) -> String,
    ) -> Option<(Rect, String)> {
        self.layout.tooltip_at(point, name)
    }

    /// See [`StripLayout::accessibility`].
    #[must_use]
    pub fn accessibility(&self) -> Vec<AccessNode> {
        self.layout.accessibility()
    }

    /// Whether a pill is still arriving or a count still changing.
    #[must_use]
    pub fn is_moving(&self) -> bool {
        let now = crate::motion::frame_now();
        self.motion
            .values()
            .any(|pill| pill.presence.is_moving(now) || pill.count.is_moving(now))
    }

    /// Paint the last layout; `hover` is the pointer, `focus` the control
    /// keyboard focus is on.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, hover: Option<Point>, focus: Option<&StripHit>) {
        let now = crate::motion::frame_now();
        let metrics = StripMetrics::of(ctx.theme);
        let motion = ctx.theme.motion();
        let hovered = |rect: Rect| self.layout.can_react && hover.is_some_and(|p| rect.contains(p));
        for pill in &self.layout.pills {
            let state = self.motion.get(&pill.emoji);
            let pose = state.map_or(crate::motion::Pose::REST, |state| {
                crate::motion::Appear::pose(
                    state.presence.progress(now),
                    pill.rect.center(),
                    motion,
                )
            });
            let counts = state.map_or_else(
                || vec![(pill.count, 1.0)],
                |state| {
                    std::iter::once((*state.count.shown(), state.count.incoming(now)))
                        .chain(
                            state
                                .count
                                .outgoing(now)
                                .map(|(count, opacity)| (*count, opacity)),
                        )
                        .collect()
                },
            );
            let focused = matches!(focus, Some(StripHit::Toggle(emoji)) if *emoji == pill.emoji);
            let bounds = pill.rect.inflate(2.0, 2.0);
            let (scene, text, theme) = (&mut *ctx.scene, &mut *ctx.text, ctx.theme);
            pose.paint(scene, ctx.transform, bounds, |scene, xform| {
                let mut ctx = PaintCtx::new(scene, xform, text, theme);
                paint_pill(&mut ctx, pill, &counts, hovered(pill.rect), &metrics);
                if focused {
                    paint_ring(&mut ctx, pill.rect);
                }
            });
        }
        if let Some(add) = self.layout.add {
            paint_add(ctx, add, hovered(add), &metrics);
            if matches!(focus, Some(StripHit::Add { .. })) {
                paint_ring(ctx, add);
            }
        }
    }
}

fn paint_ring(ctx: &mut PaintCtx<'_>, rect: Rect) {
    let radius = f64::from(ctx.theme.tokens.radii.full).min(rect.height() / 2.0);
    crate::widget::paint_focus_ring(ctx.scene, ctx.transform, rect, radius, ctx.theme);
}

/// The pill's body: the chip family's fill and edge.
fn paint_body(ctx: &mut PaintCtx<'_>, rect: Rect, fill: Color, edge: Option<Color>) {
    let radius = f64::from(ctx.theme.tokens.radii.full).min(rect.height() / 2.0);
    ctx.scene.fill(
        Fill::NonZero,
        ctx.transform,
        fill,
        None,
        &RoundedRect::from_rect(rect, radius),
    );
    if let Some(edge) = edge {
        ctx.scene.stroke(
            &Stroke::new(1.0),
            ctx.transform,
            edge,
            None,
            &crate::surface::edge_outline(rect, radius),
        );
    }
}

fn paint_pill(
    ctx: &mut PaintCtx<'_>,
    pill: &StripPill,
    counts: &[(usize, f32)],
    hovered: bool,
    metrics: &StripMetrics,
) {
    let ground = ctx.theme.tokens.colors.surfaces.content;
    let colors = ChipStyle::Neutral.colors(ctx.theme, ground, pill.mine, hovered);
    paint_body(ctx, pill.rect, colors.fill, colors.edge);
    paint_emoji(ctx, pill.glyph, &pill.emoji, &metrics.count, colors.ink);
    let x = pill.glyph.x1 + metrics.inner_gap;
    for (count, opacity) in counts {
        if *opacity <= 0.0 {
            continue;
        }
        let label = count_label(*count);
        let (_, h) = ctx.text.measure_style(&label, None, &metrics.count);
        let y = pill.rect.center().y - f64::from(h) / 2.0;
        ctx.text(
            &label,
            &metrics.count,
            colors.ink.with_alpha_factor(*opacity),
            Point::new(x, y),
            None,
        );
    }
}

fn paint_add(ctx: &mut PaintCtx<'_>, rect: Rect, hovered: bool, metrics: &StripMetrics) {
    let ground = ctx.theme.tokens.colors.surfaces.content;
    let colors = ChipStyle::Neutral.colors(ctx.theme, ground, false, hovered);
    paint_body(ctx, rect, colors.fill, colors.edge);
    let ink = if hovered { colors.ink } else { colors.lead };
    crate::icons::draw_icon(
        ctx.scene,
        ctx.transform,
        Rect::from_center_size(rect.center(), (metrics.icon, metrics.icon)),
        Icon::SmilePlus,
        ink,
        crate::icons::LUCIDE_DEFAULT_STROKE_PX,
    );
}

/// Draw `emoji` in `rect`: a workspace emoji's picture, its `:name:` in
/// `style` and `ink` while it has none, or the Unicode emoji centred and
/// sized to the rect.
pub fn paint_emoji(ctx: &mut PaintCtx<'_>, rect: Rect, emoji: &str, style: &TextStyle, ink: Color) {
    if tilo_resource::custom_emoji_name(emoji).is_some() {
        if let Some(picture) = custom_picture(emoji) {
            let side = rect.width().min(rect.height());
            crate::image_store::paint_image_contain(
                ctx.scene,
                ctx.transform,
                Rect::from_center_size(rect.center(), (side, side)),
                &picture,
                0.0,
            );
        } else {
            let (w, h) = ctx.text.measure_style(emoji, None, style);
            ctx.text(
                emoji,
                style,
                ink,
                Point::new(
                    rect.center().x - f64::from(w) / 2.0,
                    rect.center().y - f64::from(h) / 2.0,
                ),
                None,
            );
        }
        return;
    }
    // Emoji glyphs fill about four fifths of their em box. The rect is laid
    // out from the theme already, so the size is final, not scaled again.
    let size = f32_of_f64((rect.height().min(rect.width()) * 0.82).max(8.0));
    let mut glyph = style.clone();
    glyph.size = size;
    glyph.line_height = size;
    glyph.scaling = crate::text_render::TextScaling::Resolved;
    let (w, h) = ctx.text.measure_style(emoji, None, &glyph);
    ctx.text(
        emoji,
        &glyph,
        ctx.theme.tokens.colors.text_primary,
        Point::new(
            rect.center().x - f64::from(w) / 2.0,
            rect.center().y - f64::from(h) / 2.0,
        ),
        None,
    );
}

#[cfg(test)]
#[path = "reaction_strip_tests.rs"]
mod tests;
