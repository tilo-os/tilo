//! `MediaCard` and `MediaGrid`: albums, videos, books, photos, apps.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::kurbo::{Point, Rect, RoundedRect};
use vello::peniko::{self, Brush, Fill, Gradient};
use winit::event::WindowEvent;

use crate::widget::activation::{Activation, Interaction};
use crate::{widget::card_column_width, widget::card_columns};
use tilo_ui_base::ImageStore;
use tilo_ui_base::paint::PaintCtx;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::widget::DrawChild;
use tilo_ui_base::{icons::AppImage, icons::Icon};
use tilo_ui_base::{text_render::TextRenderer, text_render::TextStyle};
use tilo_ui_base::{widget::Element, widget::MediaAspect, widget::TaffyTreeExt, widget::Widget};

/// Where a card stands in its grid, and the cell the grid reads the card the pointer is over
/// from: what a range of cards is taken by.
#[derive(Clone, Default)]
pub struct CardSlot {
    /// The cell the card sets to its own place while the pointer is over it, and back to none as
    /// the pointer leaves: the grid reads the card a press or a drag lands on.
    pub hovered: Rc<Cell<Option<usize>>>,
    /// The card's place in the grid, in the order its cards stand.
    pub index: usize,
}

/// Paints a card's art into the rect it is given, from theme values.
pub type ArtPainter<'a> = &'a dyn Fn(&mut PaintCtx<'_>, Rect);

/// What a card's art is.
#[derive(Clone, Copy)]
pub enum MediaArt<'a> {
    Image(&'a peniko::ImageData),
    AppIcon(&'a AppImage),
    Mosaic(&'a [peniko::ImageData]),
    Kind(Icon),
    Paint(ArtPainter<'a>),
    Placeholder,
    Arriving(tilo_ui_base::arriving::Step),
}

impl std::fmt::Debug for MediaArt<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Image(_) => f.write_str("Image"),
            Self::AppIcon(_) => f.write_str("AppIcon"),
            Self::Mosaic(images) => write!(f, "Mosaic({})", images.len()),
            Self::Kind(icon) => write!(f, "Kind({icon:?})"),
            Self::Paint(_) => f.write_str("Paint"),
            Self::Placeholder => f.write_str("Placeholder"),
            Self::Arriving(step) => write!(f, "Arriving({step:?})"),
        }
    }
}

/// An overlay mark on the art.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MediaBadge<'a> {
    Duration(&'a str),
    Progress(f64),
    Loading(crate::time::Instant),
    Status {
        label: &'a str,
        detail: Option<&'a str>,
    },
    Live,
    Downloaded,
    Tag(&'a str, crate::widget::ChipStyle),
}

/// How large the cards of a grid are, in lines of body text so they grow with the text
/// scale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MediaSize {
    Small,
    #[default]
    Regular,
    Large,
}

tilo_ui_base::choice!(MediaSize, "MediaSize", {
    Small => "small",
    Regular => "regular",
    Large => "large",
});

impl MediaSize {
    /// The minimum column width (or, for justified rows, the target row height) under
    /// `theme`.
    #[must_use]
    pub fn extent(self, theme: &Theme) -> f64 {
        let line = f64::from(theme.text(TextRole::Body).line_height_px());
        let lines = match self {
            Self::Small => 8.0,
            Self::Regular => 11.0,
            Self::Large => 16.0,
        };
        (line * lines).round()
    }
}

#[derive(Clone, Debug)]
struct MediaMetrics {
    title: TextStyle,
    meta: TextStyle,
    badge: TextStyle,
    initials: TextStyle,
    radius: f64,
    text_gap: f64,
    inset: f64,
    lift: f64,
    action: f64,
    icon: f64,
    mark: f64,
    mark_gap: f64,
    creator_side: f64,
    /// A compact card's height: the bar's own control height, so a pill stands in a bar's
    /// band beside the controls there.
    compact: f64,
}

impl MediaMetrics {
    fn new(theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        let metrics = &theme.tokens.metrics;
        Self {
            title: theme.text(TextRole::Body),
            meta: theme.text(TextRole::Caption),
            badge: theme.text(TextRole::Caption).numeric().strong(),
            initials: theme.text(TextRole::Title).strong(),
            radius: f64::from(theme.tokens.radii.control),
            text_gap: f64::from(spacing.s2),
            inset: f64::from(spacing.s2),
            lift: f64::from(spacing.s2),
            action: f64::from(metrics.bar_height),
            icon: f64::from(metrics.icon_md),
            mark: f64::from(metrics.icon_sm),
            mark_gap: f64::from(spacing.s1),
            creator_side: tilo_ui_base::widget::AvatarSize::Small.side(theme),
            compact: f64::from(metrics.bar_control_height),
        }
    }

    fn line(text: &mut TextRenderer, style: &TextStyle) -> f64 {
        f64::from(text.measure_style("Ag", None, style).1)
    }
}

/// Immediate-mode media card.
#[derive(Clone, Copy, Debug)]
pub struct MediaCardFace<'a> {
    art: MediaArt<'a>,
    aspect: MediaAspect,
    title: Option<&'a str>,
    /// Seeds the placeholder's initials on an art-only card.
    seed: Option<&'a str>,
    meta: [Option<&'a str>; 2],
    meta_mark: Option<Icon>,
    creator: Option<tilo_ui_base::widget::AvatarFace<'a>>,
    badges: &'a [MediaBadge<'a>],
    hover_action: Option<Icon>,
    selected: bool,
    interaction: Interaction,
    /// It stands in a band as a pill: its art at the leading edge, its title beside it.
    compact: bool,
}

impl<'a> MediaCardFace<'a> {
    #[must_use]
    pub fn new(art: MediaArt<'a>) -> Self {
        Self {
            art,
            aspect: MediaAspect::Square,
            title: None,
            seed: None,
            meta: [None, None],
            meta_mark: None,
            creator: None,
            badges: &[],
            hover_action: None,
            selected: false,
            interaction: Interaction::default(),
            compact: false,
        }
    }

    /// It stands in a band as a pill rather than as a card: the art at the leading edge, a bar
    /// control high, and its title beside it on one line. A media session's live view asks for
    /// this so the bar shows what plays in its own band.
    #[must_use]
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    /// The name an art-only card's placeholder takes its initials from (a
    /// photo's place, an album's name).
    #[must_use]
    pub fn seed(mut self, name: &'a str) -> Self {
        self.seed = Some(name);
        self
    }

    /// See [`MediaCardFace::meta_mark`].
    #[must_use]
    pub fn meta_mark(mut self, icon: Icon) -> Self {
        self.meta_mark = Some(icon);
        self
    }

    #[must_use]
    pub fn aspect(mut self, aspect: MediaAspect) -> Self {
        self.aspect = aspect;
        self
    }

    /// The title under the art, up to two lines. Without one the card is art
    /// only (a photo).
    #[must_use]
    pub fn title(mut self, title: &'a str) -> Self {
        self.title = Some(title);
        self
    }

    /// One secondary line (an artist, a channel, an author). Call twice for
    /// two lines (a channel, then "1.2M views"); never join facts with dots.
    #[must_use]
    pub fn meta(mut self, line: &'a str) -> Self {
        if self.meta[0].is_none() {
            self.meta[0] = Some(line);
        } else {
            self.meta[1] = Some(line);
        }
        self
    }

    #[must_use]
    pub fn badges(mut self, badges: &'a [MediaBadge<'a>]) -> Self {
        self.badges = badges;
        self
    }

    /// A centred action over the art while the card is hovered (Play).
    #[must_use]
    pub fn hover_action(mut self, icon: Icon) -> Self {
        self.hover_action = Some(icon);
        self
    }

    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    #[must_use]
    pub fn interaction(mut self, interaction: Interaction) -> Self {
        self.interaction = interaction;
        self
    }

    /// Creator attribution beside the metadata, using the shared avatar painter.
    #[must_use]
    pub fn creator(mut self, avatar: tilo_ui_base::widget::AvatarFace<'a>) -> Self {
        self.creator = Some(avatar);
        self
    }

    /// The height of the text block under the art at `width` (0 for an
    /// art-only card). A grid passes the tallest of a row to every card in
    /// it, so their titles line up.
    pub fn text_height(&self, text: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        let m = MediaMetrics::new(theme);
        self.text_block(text, &m, Some(width)).0
    }

    fn text_block(
        &self,
        text: &mut TextRenderer,
        m: &MediaMetrics,
        width: Option<f64>,
    ) -> (f64, f64) {
        let Some(title) = self.title else {
            return (0.0, 0.0);
        };
        let lines = width.map_or(2, |width| {
            let wrap = tilo_ui_base::num::f32_of_f64(width.max(1.0));
            text.clamp_to_lines(title, &m.title, wrap, 2).len().max(1)
        });
        let title = tilo_ui_base::num::f64_of_usize(lines) * MediaMetrics::line(text, &m.title);
        let mut h = title;
        let meta_lines = self.meta.iter().flatten().count();
        let metadata =
            tilo_ui_base::num::f64_of_usize(meta_lines) * MediaMetrics::line(text, &m.meta);
        h += if self.creator.is_some() {
            metadata.max(m.creator_side)
        } else {
            metadata
        };
        ((m.text_gap + h).ceil(), title)
    }

    /// The card's total height at `width`, with its own text block.
    pub fn height(&self, text: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        let m = MediaMetrics::new(theme);
        width / self.aspect.ratio() + self.text_block(text, &m, Some(width)).0
    }

    /// The art's rect inside a card at `rect`: the whole width at the top, or, on a compact
    /// card, a square at the leading edge as tall as the pill.
    #[must_use]
    pub fn art_rect(&self, rect: Rect) -> Rect {
        if self.compact {
            let side = rect.height().min(rect.width());
            return Rect::new(rect.x0, rect.y0, rect.x0 + side, rect.y0 + side);
        }
        let h = (rect.width() / self.aspect.ratio()).min(rect.height());
        Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + h)
    }

    /// Where the hover action sits, when the card shows one: over the art of a card, and at the
    /// pill's trailing end on a compact card.
    pub fn hover_action_rect(&self, rect: Rect, theme: &Theme) -> Option<Rect> {
        self.hover_action.map(|_| {
            let m = MediaMetrics::new(theme);
            if self.compact {
                let side = m.action.min(rect.height());
                let inset = m.text_gap;
                Rect::from_center_size(
                    Point::new(rect.x1 - inset - side / 2.0, rect.center().y),
                    (side, side),
                )
            } else {
                Rect::from_center_size(self.art_rect(rect).center(), (m.action, m.action))
            }
        })
    }

    /// Paint the card into `rect`: the art at the top, the text under it; or, on a compact
    /// card, the art at the leading edge with the title beside it, one line high.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let theme = ctx.theme;
        let m = MediaMetrics::new(theme);
        let colors = &theme.tokens.colors;

        let plate = if self.selected || self.interaction.pressed {
            Some(colors.intent.selection)
        } else if self.interaction.hovered && !self.interaction.disabled {
            Some(colors.intent.selection_hover)
        } else {
            None
        };
        if let Some(fill) = plate {
            let around = rect.inflate(m.lift, m.lift);
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                fill,
                None,
                &RoundedRect::from_rect(around, m.radius + m.lift),
            );
        }

        let art = self.art_rect(rect);
        let radius = self.art_radius(&m, art);
        let name = self.title.or(self.seed).unwrap_or_default();
        paint_art(ctx, &m, art, radius, self.art, name);
        inner_hairline(ctx, art, radius);
        if !self.compact {
            // Badges take the room a grid column's art has; a pill's art is a control high.
            for badge in self.badges {
                paint_badge(ctx, &m, art, *badge);
            }
        }
        if let Some(icon) = self.hover_action
            && (self.interaction.hovered || self.interaction.focused)
            && let Some(target) = self.hover_action_rect(rect, theme)
        {
            let disc = RoundedRect::from_rect(target, target.width() / 2.0);
            tilo_ui_base::part_paint::material::paint_material(
                ctx.scene,
                ctx.transform,
                &disc,
                tilo_ui_base::part_paint::material::Material::Floating,
                theme,
            );
            let glyph = Rect::from_center_size(target.center(), (m.icon, m.icon));
            icon.draw(ctx.scene, ctx.transform, glyph, theme.text_primary);
        }
        if self.interaction.focused && !self.interaction.disabled {
            if self.compact {
                crate::widget::paint_focus_ring(ctx.scene, ctx.transform, rect, m.radius, theme);
            } else {
                crate::widget::paint_focus_ring(ctx.scene, ctx.transform, art, radius, theme);
            }
        }

        if self.compact {
            // A pill: its title on one line beside its art, centred on the pill's height. The
            // meta lines stay what assistive technology reads of it; there is no room to draw
            // them in a bar's band.
            if let Some(title) = self.title {
                let text_x = art.x1 + m.text_gap;
                let room = (rect.x1 - text_x).max(1.0);
                let line = ctx.text.elide_to_width_style(
                    title,
                    tilo_ui_base::num::f32_of_f64(room),
                    &m.title,
                );
                let title_line = MediaMetrics::line(ctx.text, &m.title);
                let y = rect.y0 + ((rect.height() - title_line) / 2.0).max(0.0);
                ctx.text(
                    &line,
                    &m.title,
                    theme.text_primary,
                    Point::new(text_x, y),
                    None,
                );
            }
            return;
        }

        if let Some(title) = self.title {
            let width = tilo_ui_base::num::f32_of_f64(rect.width().max(1.0));
            let mut y = art.y1 + m.text_gap;
            let centred = self.aspect == MediaAspect::Circle;
            let start = |ctx: &mut PaintCtx<'_>, line: &str, style: &TextStyle, extra: f64| {
                if centred {
                    let (w, _) = ctx.text.measure_style(line, None, style);
                    rect.x0 + ((rect.width() - f64::from(w) - extra) / 2.0).max(0.0)
                } else {
                    rect.x0
                }
            };
            let title_line = MediaMetrics::line(ctx.text, &m.title);
            for line in ctx.text.clamp_to_lines(title, &m.title, width, 2) {
                let x = start(ctx, &line, &m.title, 0.0);
                ctx.text(&line, &m.title, theme.text_primary, Point::new(x, y), None);
                y += title_line;
            }
            let meta_line = MediaMetrics::line(ctx.text, &m.meta);
            let creator_inset = self.creator.map_or(0.0, |avatar| {
                avatar.paint(
                    ctx,
                    Rect::from_origin_size((rect.x0, y), (m.creator_side, m.creator_side)),
                );
                m.creator_side + m.text_gap
            });
            for (index, meta) in self.meta.iter().flatten().enumerate() {
                let mark = if index == 0 { self.meta_mark } else { None };
                let mark_w = mark.map_or(0.0, |_| m.mark + m.mark_gap);
                let line = ctx.text.elide_to_width_style(
                    meta,
                    tilo_ui_base::num::f32_of_f64(
                        (f64::from(width) - mark_w - creator_inset).max(1.0),
                    ),
                    &m.meta,
                );
                let x = start(ctx, &line, &m.meta, mark_w) + creator_inset;
                ctx.text(&line, &m.meta, theme.text_dim, Point::new(x, y), None);
                if let Some(icon) = mark {
                    let (w, _) = ctx.text.measure_style(&line, None, &m.meta);
                    let glyph = Rect::new(
                        x + f64::from(w) + m.mark_gap,
                        y + (meta_line - m.mark) / 2.0,
                        x + f64::from(w) + m.mark_gap + m.mark,
                        y + (meta_line + m.mark) / 2.0,
                    );
                    icon.draw(ctx.scene, ctx.transform, glyph, theme.text_dim);
                }
                y += meta_line;
            }
        }
    }

    fn art_radius(&self, m: &MediaMetrics, art: Rect) -> f64 {
        if self.aspect == MediaAspect::Circle {
            art.width().min(art.height()) / 2.0
        } else {
            m.radius
        }
    }

    /// The card's accessibility node: named by its title, valued by its meta lines and
    /// badges.
    pub fn accessibility(&self, rect: Rect) -> tilo_ui_base::a11y::AccessNode {
        use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
        let mut value: Vec<String> = self
            .meta
            .iter()
            .flatten()
            .map(|s| (*s).to_owned())
            .collect();
        for badge in self.badges {
            match badge {
                MediaBadge::Duration(d) => value.push((*d).to_owned()),
                MediaBadge::Progress(p) => {
                    let done = if self.aspect == MediaAspect::Portrait {
                        "read"
                    } else {
                        "played"
                    };
                    value.push(format!("{}% {done}", (p.clamp(0.0, 1.0) * 100.0).round()));
                }
                MediaBadge::Live => value.push("Live".to_owned()),
                MediaBadge::Loading(_) => value.push("Loading".to_owned()),
                MediaBadge::Status { label, detail } => {
                    value.push((*label).to_owned());
                    value.extend(detail.map(str::to_owned));
                }
                MediaBadge::Downloaded => value.push("Downloaded".to_owned()),
                MediaBadge::Tag(tag, _) => value.push((*tag).to_owned()),
            }
        }
        let mut node = AccessNode::new(Role::Button)
            .with_optional_name(self.title.or(self.seed).map(str::to_owned))
            .with_bounds(rect)
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(self.interaction.focused)
                    .selected(self.selected),
            );
        if !value.is_empty() {
            node = node.with_value(value.join(", "));
        }
        node
    }
}

/// A hairline just inside the art's outline, derived
/// against the ground, so bright or dark art never bleeds into it.
fn inner_hairline(ctx: &mut PaintCtx<'_>, art: Rect, radius: f64) {
    let ground = ctx.theme.tokens.colors.surfaces.content;
    ctx.scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        ctx.transform,
        tilo_ui_base::tokens::colors::Colors::hairline_on(ground, INNER_HAIRLINE_ALPHA),
        None,
        &tilo_ui_base::surface::edge_outline(art, radius),
    );
}

const INNER_HAIRLINE_ALPHA: u8 = 26;

/// Paint `kind` as a small tile of art at `radius` (a list row's leading thumbnail): a
/// picture cover-fitted, an app icon in its quiet tile, or the generated placeholder seeded
/// with `title`'s initials.
pub(crate) fn paint_art_tile(
    ctx: &mut PaintCtx<'_>,
    art: Rect,
    kind: MediaArt<'_>,
    title: &str,
    radius: f64,
) {
    let m = MediaMetrics::new(ctx.theme);
    paint_art(ctx, &m, art, radius, kind, title);
}

fn paint_art(
    ctx: &mut PaintCtx<'_>,
    m: &MediaMetrics,
    art: Rect,
    radius: f64,
    kind: MediaArt<'_>,
    title: &str,
) {
    let theme = ctx.theme;
    let colors = &theme.tokens.colors;
    let neutral_tile = |ctx: &mut PaintCtx<'_>| {
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            colors.surfaces.raised,
            None,
            &RoundedRect::from_rect(art, radius),
        );
    };
    match kind {
        MediaArt::Image(image) => {
            tilo_ui_base::image_store::paint_image_cover(
                ctx.scene,
                ctx.transform,
                art,
                image,
                radius,
            );
        }
        MediaArt::AppIcon(icon) => {
            neutral_tile(ctx);
            let side = art.width().min(art.height()) * 0.6;
            let slot = Rect::from_center_size(art.center(), (side, side));
            if !tilo_ui_base::icons::draw_app_image(ctx.scene, ctx.transform, slot, icon) {
                paint_placeholder(ctx, m, art, radius, title);
            }
        }
        MediaArt::Kind(icon) => {
            neutral_tile(ctx);
            let side = art.width().min(art.height()) * 0.36;
            let slot = Rect::from_center_size(art.center(), (side, side));
            icon.draw(ctx.scene, ctx.transform, slot, theme.text_tertiary);
        }
        MediaArt::Mosaic(images) if !images.is_empty() => {
            let clip = RoundedRect::from_rect(art, radius);
            ctx.scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                ctx.transform,
                &clip,
            );
            let half = Point::new(art.center().x, art.center().y);
            let quads = if images.len() < 4 {
                vec![art]
            } else {
                vec![
                    Rect::new(art.x0, art.y0, half.x, half.y),
                    Rect::new(half.x, art.y0, art.x1, half.y),
                    Rect::new(art.x0, half.y, half.x, art.y1),
                    Rect::new(half.x, half.y, art.x1, art.y1),
                ]
            };
            for (quad, image) in quads.iter().zip(images) {
                tilo_ui_base::image_store::paint_image_cover(
                    ctx.scene,
                    ctx.transform,
                    *quad,
                    image,
                    0.0,
                );
            }
            ctx.scene.pop_layer();
        }
        MediaArt::Paint(painter) => {
            let clip = RoundedRect::from_rect(art, radius);
            ctx.scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                ctx.transform,
                &clip,
            );
            neutral_tile(ctx);
            painter(ctx, art);
            ctx.scene.pop_layer();
        }
        MediaArt::Mosaic(_) | MediaArt::Placeholder => {
            paint_placeholder(ctx, m, art, radius, title);
        }
        MediaArt::Arriving(step) => {
            if step == tilo_ui_base::arriving::Step::Skeleton {
                let tone = crate::widget::skeleton_tone(ctx.theme.tokens.colors.surfaces.content);
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    tone,
                    None,
                    &RoundedRect::from_rect(art, radius),
                );
            }
        }
    }
}

fn paint_placeholder(
    ctx: &mut PaintCtx<'_>,
    m: &MediaMetrics,
    art: Rect,
    radius: f64,
    title: &str,
) {
    let theme = ctx.theme;
    let colors = &theme.tokens.colors;
    let raised = colors.surfaces.raised;
    let wash = Gradient::new_linear(Point::new(art.x0, art.y0), Point::new(art.x1, art.y1))
        .with_stops([(0.0_f32, colors.lift_on(raised, 1.0)), (1.0_f32, raised)]);
    ctx.scene.fill(
        Fill::NonZero,
        ctx.transform,
        &Brush::Gradient(wash),
        None,
        &RoundedRect::from_rect(art, radius),
    );
    let initials = tilo_ui_base::widget::initials_for_name(title);
    if initials.is_empty() {
        return;
    }
    let side = art.width().min(art.height());
    let style = m
        .initials
        .clone()
        .with_content_size(tilo_ui_base::num::f32_of_f64(
            side * PLACEHOLDER_INITIALS_SHARE,
        ));
    let (w, h) = ctx.text.measure_style(&initials, None, &style);
    ctx.text(
        &initials,
        &style,
        theme.text_tertiary,
        Point::new(
            art.center().x - f64::from(w) / 2.0,
            art.center().y - f64::from(h) / 2.0,
        ),
        None,
    );
}

const PLACEHOLDER_INITIALS_SHARE: f64 = 0.2;

fn paint_badge(ctx: &mut PaintCtx<'_>, m: &MediaMetrics, art: Rect, badge: MediaBadge<'_>) {
    let theme = ctx.theme;
    let colors = &theme.tokens.colors;
    let pad = m.inset;
    match badge {
        MediaBadge::Duration(label) => {
            let (w, h) = ctx.text.measure_style(label, None, &m.badge);
            let pill = Rect::new(
                art.x1 - pad - f64::from(w) - pad,
                art.y1 - pad - f64::from(h) - pad / 2.0,
                art.x1 - pad,
                art.y1 - pad,
            );
            let shape = RoundedRect::from_rect(pill, pill.height() / 2.0);
            tilo_ui_base::part_paint::material::paint_material(
                ctx.scene,
                ctx.transform,
                &shape,
                tilo_ui_base::part_paint::material::Material::Floating,
                theme,
            );
            ctx.text(
                label,
                &m.badge,
                theme.text_primary,
                Point::new(
                    pill.x0 + pad / 2.0 + (pill.width() - pad - f64::from(w)) / 2.0,
                    pill.y0 + pad / 4.0,
                ),
                None,
            );
        }
        MediaBadge::Live => {
            let label = "Live";
            let (w, h) = ctx.text.measure_style(label, None, &m.badge);
            let pill = Rect::new(
                art.x0 + pad,
                art.y0 + pad,
                art.x0 + pad + f64::from(w) + pad,
                art.y0 + pad + f64::from(h) + pad / 2.0,
            );
            let fill = colors.semantic.danger;
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                fill,
                None,
                &RoundedRect::from_rect(pill, pill.height() / 2.0),
            );
            ctx.text(
                label,
                &m.badge,
                colors.ink_on(
                    fill,
                    theme.text_primary,
                    tilo_ui_base::tokens::colors::ON_FILL_MIN_CONTRAST,
                ),
                Point::new(pill.x0 + pad / 2.0, pill.y0 + pad / 4.0),
                None,
            );
        }
        MediaBadge::Downloaded => {
            let side = m.icon + pad;
            let disc = Rect::new(
                art.x1 - pad - side,
                art.y0 + pad,
                art.x1 - pad,
                art.y0 + pad + side,
            );
            tilo_ui_base::part_paint::material::paint_material(
                ctx.scene,
                ctx.transform,
                &RoundedRect::from_rect(disc, side / 2.0),
                tilo_ui_base::part_paint::material::Material::Floating,
                theme,
            );
            let glyph = Rect::from_center_size(disc.center(), (m.icon * 0.75, m.icon * 0.75));
            Icon::Download.draw(ctx.scene, ctx.transform, glyph, theme.text_primary);
        }
        MediaBadge::Tag(label, style) => {
            let h = crate::widget::BadgeFace::height(theme);
            let w = crate::widget::BadgeFace::intrinsic_width(label, ctx.text, theme);
            let rect = Rect::new(
                art.x0 + pad,
                art.y0 + pad,
                art.x0 + pad + w,
                art.y0 + pad + h,
            );
            crate::widget::BadgeFace::new(rect, label)
                .style(style)
                .paint(ctx.scene, ctx.transform, ctx.text, theme);
        }
        MediaBadge::Loading(started) => {
            let bar = progress_rect(art, m, theme);
            if bar.width() > 0.0 {
                crate::widget::ProgressBarFace::indeterminate(started).paint(ctx, bar);
            }
        }
        MediaBadge::Status { label, .. } => {
            let width = (art.width() - 4.0 * pad).max(1.0);
            #[allow(clippy::cast_possible_truncation, reason = "a widget width fits f32")]
            let (w, h) = ctx.text.measure_style(label, Some(width as f32), &m.badge);
            let plate = Rect::new(
                art.x0 + pad,
                art.y1 - pad - f64::from(h) - pad,
                art.x1 - pad,
                art.y1 - pad,
            );
            tilo_ui_base::part_paint::material::paint_material(
                ctx.scene,
                ctx.transform,
                &RoundedRect::from_rect(plate, pad),
                tilo_ui_base::part_paint::material::Material::Floating,
                theme,
            );
            ctx.text(
                label,
                &m.badge,
                theme.text_primary,
                Point::new(plate.center().x - f64::from(w) / 2.0, plate.y0 + pad / 2.0),
                #[allow(clippy::cast_possible_truncation, reason = "a widget width fits f32")]
                Some(width as f32),
            );
        }
        MediaBadge::Progress(share) => {
            let share = share.clamp(0.0, 1.0);
            if share > 0.0 {
                let bar = progress_rect(art, m, theme);
                if bar.width() > 0.0 {
                    #[allow(clippy::cast_possible_truncation, reason = "a share in 0..=1")]
                    crate::widget::ProgressBarFace::new(share as f32).paint(ctx, bar);
                }
            }
        }
    }
}

/// The badge strip a card face paints over its art, on a theme's own metrics: what a test
/// outside this crate paints and reads the ink of.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn paint_badge_for_theme(ctx: &mut PaintCtx<'_>, art: Rect, badge: MediaBadge<'_>) {
    paint_badge(ctx, &MediaMetrics::new(ctx.theme), art, badge);
}

/// Where a retained card's art comes from.
pub enum MediaSource {
    Image(peniko::ImageData),
    Url { store: ImageStore, url: String },
    AppIcon(AppImage),
    Mosaic(Vec<peniko::ImageData>),
    Kind(Icon),
    Paint(Box<dyn Fn(&mut PaintCtx<'_>, Rect)>),
    Placeholder,
}

fn progress_rect(art: Rect, m: &MediaMetrics, theme: &Theme) -> Rect {
    let height = crate::widget::ProgressBarFace::height(theme).min(art.height());
    Rect::new(
        art.x0 + m.inset,
        art.y1 - m.inset - height,
        art.x1 - m.inset,
        art.y1 - m.inset,
    )
}

/// Owned badge for the retained card.
#[derive(Clone, Debug, PartialEq)]
pub enum OwnedBadge {
    Duration(String),
    Progress(f64),
    Live,
    Downloaded,
    Tag(String, crate::widget::ChipStyle),
}

impl OwnedBadge {
    fn borrowed(&self) -> MediaBadge<'_> {
        match self {
            Self::Duration(d) => MediaBadge::Duration(d),
            Self::Progress(p) => MediaBadge::Progress(*p),
            Self::Live => MediaBadge::Live,
            Self::Downloaded => MediaBadge::Downloaded,
            Self::Tag(tag, style) => MediaBadge::Tag(tag, *style),
        }
    }
}

struct MediaCreator {
    name: String,
    identity: String,
    image: Option<peniko::ImageData>,
    image_url: Option<(ImageStore, String)>,
}

struct HoverAction {
    icon: Icon,
    label: String,
    press: HoverPress,
}

enum HoverPress {
    Bound(tilo_ui_base::widget::ActionEmitter<()>),
    Plain(Box<dyn FnMut()>),
}

impl HoverAction {
    fn run(&mut self) {
        match &mut self.press {
            HoverPress::Bound(emitter) => {
                if let Err(error) = emitter.emit(()) {
                    tracing::warn!(%error, "a hover action refused its arguments");
                }
            }
            HoverPress::Plain(press) => press(),
        }
    }

    fn emitter(&self) -> Option<&tilo_ui_base::widget::ActionEmitter<()>> {
        match &self.press {
            HoverPress::Bound(emitter) => Some(emitter),
            HoverPress::Plain(_) => None,
        }
    }

    fn emitter_mut(&mut self) -> Option<&mut tilo_ui_base::widget::ActionEmitter<()>> {
        match &mut self.press {
            HoverPress::Bound(emitter) => Some(emitter),
            HoverPress::Plain(_) => None,
        }
    }
}

pub struct MediaCard {
    source: MediaSource,
    loaded: Option<peniko::ImageData>,
    aspect: MediaAspect,
    title: Option<String>,
    seed: Option<String>,
    meta: Vec<String>,
    meta_mark: Option<Icon>,
    creator: Option<MediaCreator>,
    creator_title_height: f64,
    creator_rect: Rect,
    badges: Vec<OwnedBadge>,
    hover_action: Option<HoverAction>,
    /// Where it stands in its grid, when its grid takes a range of them.
    slot: Option<CardSlot>,
    /// It stands on its own, outside a grid: a card's width, wherever it is put.
    alone: bool,
    /// The width of a card on its own, as its grid lays a column: what the whole room it is put
    /// in never widens it past.
    bound: Option<f64>,
    /// It stands in a band as a pill: a bar control high, its art at the leading edge and its
    /// title beside it, rather than the card a grid lays.
    compact: bool,
    pressed_action: crate::pointer_replay::HeldPress,
    selected: bool,
    activation: Activation,
    metrics: Option<MediaMetrics>,
    shared_text_height: Option<f64>,
    text_estimated: bool,
    wants_measure: bool,
    rect: Rect,
    dirty: bool,
}

impl MediaCard {
    /// Take what `other`, the card these props make, shows and does, keeping where this card
    /// stands: the props a view gives the same key again are taken in place, so the person's
    /// selection, press and focus stay with it.
    pub(crate) fn adopt(&mut self, mut other: Self) {
        other.rect = self.rect;
        other.slot.clone_from(&self.slot);
        other.alone = self.alone;
        other.bound = self.bound;
        *self = other;
    }

    pub fn new(id: impl Into<String>, source: MediaSource) -> Self {
        Self::new_owned(id.into(), source)
    }

    fn new_owned(id: String, source: MediaSource) -> Self {
        let loaded = match &source {
            MediaSource::Image(image) => Some(image.clone()),
            _ => None,
        };
        Self {
            source,
            loaded,
            aspect: MediaAspect::Square,
            title: None,
            seed: None,
            meta: Vec::new(),
            meta_mark: None,
            creator: None,
            creator_title_height: 0.0,
            creator_rect: Rect::ZERO,
            badges: Vec::new(),
            hover_action: None,
            slot: None,
            alone: false,
            bound: None,
            compact: false,
            pressed_action: crate::pointer_replay::HeldPress::default(),
            selected: false,
            activation: Activation::new(id),
            metrics: None,
            shared_text_height: None,
            text_estimated: false,
            wants_measure: false,
            rect: Rect::ZERO,
            dirty: true,
        }
    }

    #[must_use]
    pub fn aspect(mut self, aspect: MediaAspect) -> Self {
        self.aspect = aspect;
        self
    }

    #[must_use]
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// See [`MediaCardFace::seed`].
    #[must_use]
    pub fn seed(mut self, name: impl Into<String>) -> Self {
        self.seed = Some(name.into());
        self
    }

    /// See [`MediaCardFace::meta_mark`].
    #[must_use]
    pub fn meta_mark(mut self, icon: Icon) -> Self {
        self.meta_mark = Some(icon);
        self
    }

    /// See [`MediaCardFace::meta`]; at most two lines.
    #[must_use]
    pub fn meta(mut self, line: impl Into<String>) -> Self {
        if self.meta.len() < 2 {
            self.meta.push(line.into());
        }
        self
    }

    /// A person's attribution below the title.
    #[must_use]
    pub fn creator(mut self, name: impl Into<String>, identity: impl Into<String>) -> Self {
        self.creator = Some(MediaCreator {
            name: name.into(),
            identity: identity.into(),
            image: None,
            image_url: None,
        });
        self
    }

    /// Load a creator picture from the same cache used by the retained Avatar.
    #[must_use]
    pub fn creator_image_url(mut self, store: ImageStore, url: impl Into<String>) -> Self {
        if let Some(creator) = &mut self.creator {
            creator.image_url = Some((store, url.into()));
        }
        self
    }

    #[must_use]
    pub fn badge(mut self, badge: OwnedBadge) -> Self {
        self.badges.push(badge);
        self
    }

    /// A centred action over the art on hover, through an exported resource
    /// action: the host resolves its policy before it runs, and the action
    /// shows only while it is available.
    #[must_use]
    pub fn hover_action_binding(
        mut self,
        icon: Icon,
        label: impl Into<String>,
        binding: impl crate::Bind<()>,
    ) -> Self {
        self.hover_action = Some(HoverAction {
            icon,
            label: label.into(),
            press: HoverPress::Bound(tilo_ui_base::widget::ActionEmitter::new(
                crate::Bind::bound(binding),
            )),
        });
        self
    }

    /// A centred action over the art on hover that calls `f`.
    #[must_use]
    pub fn on_hover_action(
        mut self,
        icon: Icon,
        label: impl Into<String>,
        f: impl FnMut() + 'static,
    ) -> Self {
        self.hover_action = Some(HoverAction {
            icon,
            label: label.into(),
            press: HoverPress::Plain(Box::new(f)),
        });
        self
    }

    #[must_use]
    pub fn on_activate(mut self, f: impl FnMut() + 'static) -> Self {
        self.activation.set_callback(Box::new(f));
        self
    }

    #[must_use]
    pub fn on_action(mut self, binding: impl crate::Bind<()>) -> Self {
        self.activation.set_action(binding);
        self
    }

    /// A press that opens or shows something: a double-click asks for it once.
    #[must_use]
    pub fn on_open(mut self, f: impl FnMut() + 'static) -> Self {
        self.activation.set_open(Box::new(f));
        self
    }

    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Where it stands in its grid, whose range gestures read the card the pointer is over from
    /// it.
    #[must_use]
    pub fn slot(mut self, slot: CardSlot) -> Self {
        self.slot = Some(slot);
        self
    }

    /// The card stands on its own, not in a grid: it takes a grid column's width, never the whole
    /// of the room it is put in.
    #[must_use]
    pub(crate) fn standing_alone(mut self) -> Self {
        self.alone = true;
        self
    }

    /// It stands in a band as a pill: a bar control high, its art at the leading edge and its
    /// title beside it, rather than the card a grid lays. What a live media view gives so the
    /// shell's bar shows what plays in the bar's own band.
    #[must_use]
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    fn with_face<R>(&self, f: impl FnOnce(&MediaCardFace<'_>) -> R) -> R {
        let badges: Vec<MediaBadge<'_>> = self.badges.iter().map(OwnedBadge::borrowed).collect();
        let art = match (&self.loaded, &self.source) {
            (Some(image), _) => MediaArt::Image(image),
            (None, MediaSource::AppIcon(icon)) => MediaArt::AppIcon(icon),
            (None, MediaSource::Mosaic(images)) => MediaArt::Mosaic(images),
            (None, MediaSource::Kind(icon)) => MediaArt::Kind(*icon),
            (None, MediaSource::Paint(painter)) => MediaArt::Paint(painter.as_ref()),
            _ => MediaArt::Placeholder,
        };
        let mut face = MediaCardFace::new(art)
            .aspect(self.aspect)
            .badges(&badges)
            .selected(self.selected)
            .compact(self.compact)
            .interaction(self.activation.state);
        if let Some(title) = &self.title {
            face = face.title(title);
        }
        if let Some(seed) = &self.seed {
            face = face.seed(seed);
        }
        for line in &self.meta {
            face = face.meta(line);
        }
        if let Some(mark) = self.meta_mark {
            face = face.meta_mark(mark);
        }
        if let Some(creator) = &self.creator {
            face = face.creator(
                tilo_ui_base::widget::AvatarFace::new(&creator.name, &creator.identity)
                    .image(creator.image.as_ref()),
            );
        }
        if let Some(action) = &self.hover_action {
            face = face.hover_action(action.icon);
        }
        f(&face)
    }

    fn own_text_height(&mut self, text: &mut TextRenderer, width: Option<f64>) -> f64 {
        let Some(m) = &self.metrics else {
            return 0.0;
        };
        let (height, title) = self.with_face(|face| face.text_block(text, m, width));
        if width.is_some() {
            self.creator_title_height = title;
        }
        height
    }

    fn hover_action_ref(&self) -> Option<&HoverAction> {
        self.hover_action.as_ref()
    }

    fn hover_action_mut(&mut self) -> Option<&mut HoverAction> {
        self.hover_action.as_mut()
    }

    fn action_target(&self) -> Option<Rect> {
        let m = self.metrics.as_ref()?;
        self.hover_action.as_ref().map(|_| {
            Rect::from_center_size(
                Rect::new(
                    self.rect.x0,
                    self.rect.y0,
                    self.rect.x1,
                    self.rect.y0 + self.rect.width() / self.aspect.ratio(),
                )
                .center(),
                (m.action, m.action),
            )
        })
    }
}

impl crate::widget::ReservedShape for MediaCard {
    fn reserved_shape(&self) -> tilo_ui_base::widget::SkeletonShape {
        tilo_ui_base::widget::SkeletonShape::Media(self.aspect)
    }
}

impl Widget for MediaCard {
    tilo_ui_base::described!();

    tilo_ui_base::provides!(
        dyn tilo_ui_base::widget::controls::ActionControls,
        dyn crate::widget::ReservedShape
    );

    fn sync(
        &mut self,
        _state: &tilo_ui_base::widget::AppModel,
        _path: &tilo_ui_base::widget::ModelPath,
    ) {
        if let Some(creator) = &mut self.creator
            && creator.image.is_none()
            && let Some((store, url)) = &creator.image_url
            && let Some(image) = store.get(url)
        {
            creator.image = Some(image);
            self.dirty = true;
        }
        // A picture that carries its own bytes in its address is here on the card's first
        // frame, as a `ui.Image` of it is; any other waits for its store.
        if self.loaded.is_none()
            && let MediaSource::Url { store, url } = &self.source
            && let Some(image) = store.get(url).or_else(|| crate::pictures::carried(url))
        {
            self.loaded = Some(image);
            self.dirty = true;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.metrics = Some(MediaMetrics::new(theme));
        self.bound = self.alone.then(|| MediaSize::Regular.extent(theme));
        let compact_height = f64::from(theme.tokens.metrics.bar_control_height);
        let style = Style {
            // A pill sizes to what it holds, a bar control high; a card fills its column and is
            // as tall as its art and text at that width.
            size: taffy::Size {
                width: if self.compact {
                    auto()
                } else {
                    percent(1.0_f32)
                },
                height: if self.compact {
                    length(tilo_ui_base::num::f32_of_f64(compact_height))
                } else {
                    auto()
                },
            },
            min_size: taffy::Size {
                width: length(0.0_f32),
                height: auto(),
            },
            // A card of its own is as wide as a grid's column at its smallest, so what it shows
            // stands on a page as a card, never as the page's whole width.
            max_size: taffy::Size {
                width: match self.bound {
                    Some(width) => length(tilo_ui_base::num::f32_of_f64(width)),
                    None => auto(),
                },
                height: auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let width = f64::from(known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w,
            _ => self.bound.map_or(160.0, tilo_ui_base::num::f32_of_f64),
        }));
        // The card's height is the height at the width taffy gives it, which a card of its own
        // is bounded at: measure is asked before the bound is applied.
        let width = self.bound.map_or(width, |bound| width.min(bound));
        if self.compact {
            let Some(m) = &self.metrics else {
                return taffy::Size {
                    width: 0.0,
                    height: 0.0,
                };
            };
            let height = m.compact;
            let text = self.title.as_deref().map_or(0.0, |title| {
                f64::from(text_renderer.measure_style(title, None, &m.title).0) + m.text_gap
            });
            let width = (height + text).min(width);
            return taffy::Size {
                width: tilo_ui_base::num::f32_of_f64(width),
                height: tilo_ui_base::num::f32_of_f64(height),
            };
        }
        let text = self.shared_text_height.unwrap_or_else(|| {
            self.own_text_height(text_renderer, (!self.text_estimated).then_some(width))
        });
        taffy::Size {
            width: tilo_ui_base::num::f32_of_f64(width),
            height: tilo_ui_base::num::f32_of_f64(width / self.aspect.ratio() + text),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.dirty = true;
        }
        self.rect = rect;
        if self.creator.is_some()
            && let Some(m) = &self.metrics
        {
            self.creator_rect = Rect::from_origin_size(
                (
                    rect.x0,
                    rect.y0
                        + rect.width() / self.aspect.ratio()
                        + m.text_gap
                        + self.creator_title_height,
                ),
                (m.creator_side, m.creator_side),
            );
        }
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        if env.should_cull(self.rect) {
            return;
        }
        let rect = self.rect;
        self.with_face(|face| {
            face.paint(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                rect,
            );
        });
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        use winit::event::{ElementState, MouseButton};
        let target = self.action_target();
        let shown = self.activation.state.hovered || self.activation.state.focused;
        if let WindowEvent::MouseInput {
            state,
            button: MouseButton::Left,
            ..
        } = event
            && shown
            && let Some(target) = target
        {
            use crate::pointer_replay::{PressAt, Release};
            let over = target.contains(self.activation.cursor());
            let key = if self.activation.id().is_empty() {
                String::new()
            } else {
                format!("{}/action", self.activation.id())
            };
            let at = over.then_some(PressAt::one(&key, target));
            match state {
                ElementState::Pressed if over => {
                    if let Some(at) = at {
                        self.pressed_action.press(at);
                    }
                    *ctx.handled = true;
                    return;
                }
                ElementState::Released => match self.pressed_action.release(at) {
                    Release::NotHeld => {}
                    release => {
                        *ctx.handled = true;
                        if release == Release::Click(())
                            && let Some(action) = &mut self.hover_action
                        {
                            action.run();
                        }
                        return;
                    }
                },
                ElementState::Pressed => {}
            }
        }
        if let Some(slot) = &self.slot {
            match event {
                WindowEvent::CursorMoved { position, .. } => {
                    if self.rect.contains(ctx.logical(position)) {
                        slot.hovered.set(Some(slot.index));
                    } else if slot.hovered.get() == Some(slot.index) {
                        slot.hovered.set(None);
                    }
                }
                WindowEvent::CursorLeft { .. } if slot.hovered.get() == Some(slot.index) => {
                    slot.hovered.set(None);
                }
                _ => {}
            }
        }
        let exclude: Vec<Rect> = target.filter(|_| shown).into_iter().collect();
        if self
            .activation
            .handle_pointer(event, ctx, self.rect, &exclude)
        {
            self.dirty = true;
        }
    }

    tilo_ui_base::pressable!(rect);

    fn needs_redraw(&self) -> bool {
        self.dirty || self.wants_measure
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        tilo_ui_base::widget::Widget::mark_children_drawn(self);
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO || self.wants_measure
    }

    fn name(&self) -> &'static str {
        "MediaCard"
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.activation.id())
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        let mut node = self
            .with_face(|face| face.accessibility(self.rect))
            .with_semantic_key(self.activation.id());
        if let Some(creator) = &self.creator {
            node.children.push(
                tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Image)
                    .with_name(creator.name.clone())
                    .with_bounds(self.creator_rect),
            );
        }
        if let Some(action) = &self.hover_action {
            node.children.push(
                tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Button)
                    .with_name(action.label.clone()),
            );
        }
        Some(node)
    }
}

impl tilo_ui_base::widget::controls::ActionControls for MediaCard {
    fn attach_controls(
        &mut self,
        domain: &tilo_ui_base::ControlDomain,
        attachment: tilo_ui_base::control_domain::DomainAttachment,
    ) {
        self.activation.attach(domain, attachment);
        if let Some(emitter) = self.hover_action_mut().and_then(HoverAction::emitter_mut) {
            let _ = emitter.attach(domain, attachment);
        }
        for child in tilo_ui_base::widget::Widget::children_mut(self) {
            tilo_ui_base::widget::controls::attach_controls(child, domain, attachment);
        }
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        let hover = self
            .hover_action_mut()
            .and_then(HoverAction::emitter_mut)
            .and_then(tilo_ui_base::widget::ActionEmitter::take);
        self.activation.take_invocation().or(hover).or_else(|| {
            tilo_ui_base::widget::Widget::children_mut(self)
                .into_iter()
                .find_map(tilo_ui_base::widget::controls::take_control_invocation)
        })
    }

    fn control_action_bindings(&self) -> Vec<tilo_ui_base::widget::ControlBindingMetadata> {
        let mut bindings = self.activation.bindings();
        if let Some(emitter) = self.hover_action_ref().and_then(HoverAction::emitter) {
            bindings.push(emitter.metadata(format!("{}/action", self.activation.id())));
        }
        for child in tilo_ui_base::widget::Widget::children(self) {
            bindings.extend(tilo_ui_base::widget::controls::control_action_bindings(
                child,
            ));
        }
        bindings
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        self.activation.refresh_availability(allowed);
        if let Some(emitter) = self.hover_action_mut().and_then(HoverAction::emitter_mut) {
            emitter.refresh_availability(allowed);
        }
        for child in tilo_ui_base::widget::Widget::children_mut(self) {
            tilo_ui_base::widget::controls::refresh_control_action_availability(child, allowed);
        }
    }
}

/// Lay `aspects` (width / height) out in justified rows `width` wide: each row is filled at
/// `target_height`, then scaled so it spans the width exactly.
#[must_use]
pub fn justified_rows(aspects: &[f64], width: f64, target_height: f64, gap: f64) -> Vec<Rect> {
    let mut out = Vec::with_capacity(aspects.len());
    if width <= 0.0 || target_height <= 0.0 {
        return out;
    }
    let ratio = |a: f64| if a.is_finite() && a > 0.0 { a } else { 1.0 };
    let mut y = 0.0;
    let mut start = 0;
    let mut previous: Option<f64> = None;
    while start < aspects.len() {
        let mut end = start;
        let mut row_ratio = 0.0;
        while end < aspects.len() {
            row_ratio += ratio(aspects[end]);
            end += 1;
            let gaps = tilo_ui_base::num::f64_of_usize(end - start - 1) * gap;
            if row_ratio * target_height + gaps >= width {
                break;
            }
        }
        let gaps = tilo_ui_base::num::f64_of_usize(end - start - 1) * gap;
        let full = row_ratio * target_height + gaps >= width;
        let height = if full {
            (width - gaps) / row_ratio
        } else {
            // A short last row is never taller than the row above it.
            target_height.min(previous.unwrap_or(target_height))
        };
        previous = Some(height);
        let mut x = 0.0;
        for &aspect in &aspects[start..end] {
            let w = ratio(aspect) * height;
            out.push(Rect::new(x, y, x + w, y + height));
            x += w + gap;
        }
        y += height + gap;
        start = end;
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GridMode {
    Columns,
    Justified,
}

/// A responsive grid of [`MediaCard`]s.
pub struct MediaGrid {
    cards: Vec<Element>,
    nodes: Vec<NodeId>,
    mode: GridMode,
    size: MediaSize,
    extent: f64,
    column_gap: f64,
    row_gap: f64,
    rect: Rect,
    /// What the viewport showed of the grid at its last draw, from the
    /// grid's top. Only cards within a viewport's height of it have their
    /// titles measured; the others are as tall as their text can be.
    window: (f64, f64),
    /// A justified grid's cards stand where its rows put them, out of its flow; this empty node
    /// in the flow is as tall as the rows, so the grid is too.
    rows_height: Option<NodeId>,

    /// The list that builds a host-rendered grid's cards as it shows them, in place of `cards`:
    /// a grid of ten thousand photographs costs the cards on screen. Every method of this widget
    /// is the list's, so the list stands where the grid would.
    inner: Option<Box<dyn Widget>>,

    /// The columns a host-rendered grid shows, written where the grid is laid out: the list's
    /// rows are bands of this many cards.
    columns: Option<Rc<Cell<usize>>>,

    /// Every card's item, by the place it stands in: what a range of cards reports.
    pub(crate) order: Rc<std::cell::RefCell<Vec<Option<tilo_ui_base::component::ResourceId>>>>,
    /// The place of the card the pointer is over, as the card itself says it.
    pub(crate) hovered: Rc<Cell<Option<usize>>>,
    /// The card the pointer pressed last, without shift: where a range taken with shift starts.
    anchor: Cell<Option<usize>>,
    /// The card the pointer is held on, while it is: where a drag's range starts.
    pressed: Cell<Option<usize>>,
    /// The places the drag has swept: the range reported, so a move reports only when it grows.
    swept: std::cell::RefCell<Vec<usize>>,
    /// The cards a range took, as the view takes them, in the order they stand.
    select: tilo_ui_base::component::Event,

    dirty: bool,
}

/// The window a grid measures before its first draw: a tall screen.
const FIRST_WINDOW: (f64, f64) = (0.0, 1200.0);

fn near(window: (f64, f64), top: f64, bottom: f64) -> bool {
    let margin = window.1 - window.0;
    top < window.1 + margin && bottom > window.0 - margin
}

impl MediaGrid {
    /// Equal columns, as many as fit `size` cards.
    pub fn new(cards: Vec<MediaCard>) -> Self {
        Self {
            dirty: true,
            cards: cards.into_iter().map(|c| Box::new(c) as Element).collect(),
            nodes: Vec::new(),
            mode: GridMode::Columns,
            size: MediaSize::Regular,
            extent: 0.0,
            column_gap: 0.0,
            row_gap: 0.0,
            rect: Rect::ZERO,
            window: FIRST_WINDOW,
            rows_height: None,
            inner: None,
            columns: None,
            order: Rc::default(),
            hovered: Rc::default(),
            anchor: Cell::new(None),
            pressed: Cell::new(None),
            swept: std::cell::RefCell::new(Vec::new()),
            select: tilo_ui_base::component::Event::None,
        }
    }

    /// A grid whose cards are built as they are shown by `inner`, the shared list over the
    /// host-rendered cards: the list stands for the grid in the tree, the grid's own layout and
    /// scrolling being the list's.
    pub(crate) fn hosting(inner: Box<dyn Widget>) -> Self {
        Self {
            inner: Some(inner),
            ..Self::new(Vec::new())
        }
    }

    /// The cell the host-rendered grid keeps its column count in, so its list builds a row of
    /// that many cards as it shows it.
    pub(crate) fn set_columns(&mut self, columns: Rc<Cell<usize>>) {
        self.columns = Some(columns);
    }

    /// The cell the places are kept in, and the one the card the pointer is over is read from:
    /// what a grid whose cards a host builds registers them through.
    pub(crate) fn set_range(
        &mut self,
        order: Rc<std::cell::RefCell<Vec<Option<tilo_ui_base::component::ResourceId>>>>,
        hovered: Rc<Cell<Option<usize>>>,
    ) {
        self.order = order;
        self.hovered = hovered;
    }

    /// The cards a range took, as the view takes them.
    pub(crate) fn set_select(&mut self, select: tilo_ui_base::component::Event) {
        self.select = select;
    }

    /// The places its cards stand in, and the cell the card the pointer is over is read from, as
    /// the grid whose cards it builds them again hands them over.
    pub(crate) fn range_cells(
        &self,
    ) -> (
        Rc<std::cell::RefCell<Vec<Option<tilo_ui_base::component::ResourceId>>>>,
        Rc<Cell<Option<usize>>>,
    ) {
        (Rc::clone(&self.order), Rc::clone(&self.hovered))
    }

    /// What a range gesture takes: the cards from `from` to `to`, each by the item it shows, in
    /// the order they stand.
    fn take_range(&self, from: usize, to: usize) {
        let (low, high) = if from <= to { (from, to) } else { (to, from) };
        let order = self.order.borrow();
        let picked: Vec<tilo_ui_base::component::Value> = (low..=high)
            .filter_map(|at| order.get(at).and_then(Option::as_ref))
            .map(|item| tilo_ui_base::component::Value::Resource(item.0.clone()))
            .collect();
        self.select
            .fire(Some(tilo_ui_base::component::Value::List(picked)));
    }

    /// A press with shift takes the cards from the one pressed last to the one pressed now; a
    /// press held and dragged over the cards takes the ones it sweeps. Whether it took the press.
    fn range_event(
        &self,
        event: &WindowEvent,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
    ) -> bool {
        if self.select.is_none() {
            return false;
        }
        match event {
            WindowEvent::CursorMoved { .. } => {
                let (Some(from), Some(to)) = (self.pressed.get(), self.hovered.get()) else {
                    return false;
                };
                // A press that has not left the card it started on is still that card's own
                // click: it opens rather than sweeping.
                if to == from {
                    return false;
                }
                let (low, high) = if from <= to { (from, to) } else { (to, from) };
                let range: Vec<usize> = (low..=high).collect();
                if *self.swept.borrow() != range {
                    self.take_range(from, to);
                    *self.swept.borrow_mut() = range;
                }
                false
            }
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                let Some(from) = self.pressed.take() else {
                    return false;
                };
                let swept = std::mem::take(&mut *self.swept.borrow_mut());
                let Some(to) = self.hovered.get().or(swept.last().copied()) else {
                    return false;
                };
                if swept.is_empty() {
                    // A press let go where it started is the card's own: it opens or selects.
                    return false;
                }
                self.take_range(from, to);
                self.anchor.set(Some(to));
                *ctx.handled = true;
                true
            }
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                let Some(at) = self.hovered.get() else {
                    return false;
                };
                if ctx.modifiers.state().shift_key() {
                    let from = self.anchor.get().unwrap_or(at);
                    self.take_range(from, at);
                    self.anchor.set(Some(at));
                    self.pressed.set(None);
                    *self.swept.borrow_mut() = Vec::new();
                    *ctx.handled = true;
                    return true;
                }
                self.pressed.set(Some(at));
                self.anchor.set(Some(at));
                *self.swept.borrow_mut() = Vec::new();
                false
            }
            _ => false,
        }
    }

    /// Whether the grid builds its cards as a host renders them, rather than holding them whole.
    #[cfg(test)]
    pub(crate) fn hosted(&self) -> bool {
        self.inner.is_some()
    }

    /// Justified rows at each card's own [`MediaAspect`] (a photo library): rows of equal
    /// height that span the width exactly.
    pub fn justified(cards: Vec<MediaCard>) -> Self {
        Self {
            dirty: true,
            mode: GridMode::Justified,
            ..Self::new(cards)
        }
    }

    #[must_use]
    pub fn with_size(mut self, size: MediaSize) -> Self {
        self.set_size(size);
        self
    }

    /// How large the cards are, where the grid stands.
    pub(crate) fn set_size(&mut self, size: MediaSize) {
        if self.size != size {
            self.size = size;
            self.dirty = true;
        }
    }

    /// Whether the rows are justified at each card's aspect, where the grid stands.
    pub(crate) fn set_justified(&mut self, justified: bool) {
        let mode = if justified {
            GridMode::Justified
        } else {
            GridMode::Columns
        };
        if self.mode != mode {
            self.mode = mode;
            self.dirty = true;
        }
    }

    /// The cards the grid shows, where it stands.
    pub(crate) fn set_cards(&mut self, cards: Vec<MediaCard>) {
        self.cards = cards
            .into_iter()
            .map(|card| Box::new(card) as Element)
            .collect();
        self.nodes.clear();
        self.dirty = true;
    }

    /// The card `index`, out of the grid: for a host that places a card itself.
    #[must_use]
    pub fn take_card(&mut self, index: usize) -> Option<MediaCard> {
        let card = self.cards.get_mut(index)?;
        card.as_any_mut()
            .and_then(|any| any.downcast_mut::<MediaCard>())
            .map(|card| {
                std::mem::replace(
                    card,
                    MediaCard::new(String::new(), MediaSource::Placeholder),
                )
            })
    }

    fn cards_mut(&mut self) -> impl Iterator<Item = &mut MediaCard> {
        self.cards.iter_mut().filter_map(|card| {
            card.as_any_mut()
                .and_then(|any| any.downcast_mut::<MediaCard>())
        })
    }

    /// How many columns fit `width`.
    pub fn columns_for_width(&self, width: f64) -> usize {
        card_columns(width, self.extent, self.column_gap)
    }

    /// `count` cards at `aspect` on their way from `from`, asked at
    /// `since`: each reserves a card's art and text block and steps through
    /// the framework's placeholder ([`crate::widget::ArrivingView`]). The
    /// cards stand in one region, so the grid names its source once.
    pub fn arriving(
        count: usize,
        aspect: MediaAspect,
        from: &tilo_ui_base::arriving::Source,
        since: crate::time::Instant,
    ) -> Self {
        Self::of(
            (0..count)
                .map(|_| {
                    crate::widget::ArrivingView::placeholder_since(
                        from.clone(),
                        since,
                        tilo_ui_base::widget::SkeletonShape::Media(aspect),
                    )
                })
                .collect(),
        )
    }

    /// A grid of `cards` that are not all [`MediaCard`]s yet: cards on their way, each an
    /// [`crate::widget::ArrivingView`] shaped as a card.
    pub fn of(cards: Vec<Element>) -> Self {
        let mut grid = Self::new(Vec::new());
        grid.cards = cards;
        grid
    }
}

impl MediaGrid {
    /// Its columns, and the text height each row of cards shares.
    fn size_columns(
        &mut self,
        styles: &mut taffy::Restyle<'_>,
        width: f64,
        text: &mut TextRenderer,
    ) {
        let node = styles.node();
        let columns = self.columns_for_width(width);
        let column = card_column_width(width, columns, self.column_gap).floor();
        // A bare fraction retains an automatic intrinsic minimum.
        // MediaCard measures its artwork from the available width,
        // which during intrinsic sizing may be the whole parent.
        // Zero-minimum tracks let the chosen columns share that
        // width instead of each claiming the whole viewport.
        let mut style = styles.style(node).clone();
        style.grid_template_columns =
            vec![
                taffy::style_helpers::minmax(length(0.0_f32), taffy::style_helpers::fr(1.0_f32));
                columns
            ];
        styles.set_style(node, style);
        // Every card of a row shares the row's tallest text block.
        // Cards far from the viewport keep the height they were
        // measured at, or are as tall as their text can be, so laying
        // out a long grid shapes only the titles near what is shown.
        // Where a row stands follows from the rows above it: their artwork at this width and the
        // text they share.
        let window = self.window;
        let row_gap = self.row_gap;
        let mut changed = false;
        let mut shared = Vec::with_capacity(self.cards.len());
        let mut top = 0.0_f64;
        for row in self.cards.chunks_mut(columns.max(1)) {
            let held = |card: &mut MediaCard, text: &mut TextRenderer| {
                column / card.aspect.ratio()
                    + if card.text_estimated {
                        card.own_text_height(text, None)
                    } else {
                        card.shared_text_height.unwrap_or_default()
                    }
            };
            let bottom = top
                + row
                    .iter_mut()
                    .filter_map(|card| card.as_any_mut()?.downcast_mut::<MediaCard>())
                    .map(|card| held(card, text))
                    .fold(0.0, f64::max);
            let measure = near(window, top, bottom);
            let mut tallest = 0.0_f64;
            let mut art = 0.0_f64;
            for card in row
                .iter_mut()
                .filter_map(|card| card.as_any_mut()?.downcast_mut::<MediaCard>())
            {
                art = art.max(column / card.aspect.ratio());
                let height = if measure {
                    changed |= card.text_estimated;
                    card.text_estimated = false;
                    card.wants_measure = false;
                    card.own_text_height(text, Some(column))
                } else if card.text_estimated {
                    card.own_text_height(text, None)
                } else {
                    card.shared_text_height.unwrap_or_default()
                };
                tallest = tallest.max(height);
            }
            top += art + tallest + row_gap;
            shared.extend(std::iter::repeat_n(tallest, row.len()));
        }
        for (card, height) in self.cards_mut().zip(shared) {
            if card.shared_text_height != Some(height) {
                card.shared_text_height = Some(height);
                changed = true;
            }
        }
        if changed {
            for &child in &self.nodes {
                styles.mark_dirty(child);
            }
        }
    }

    /// Its cards in rows as tall as fills the width, and itself as tall as the rows.
    fn justify(&mut self, styles: &mut taffy::Restyle<'_>, width: f64) {
        let aspects: Vec<f64> = self.cards_mut().map(|card| card.aspect.ratio()).collect();
        let rects = justified_rows(&aspects, width, self.extent, self.column_gap);
        let height = rects.iter().map(|r| r.y1).fold(0.0, f64::max);
        for (&child, rect) in self.nodes.iter().zip(&rects) {
            let current = styles.style(child);
            let mut style = current.clone();
            style.position = taffy::Position::Absolute;
            style.inset = taffy::Rect {
                left: length(tilo_ui_base::num::f32_of_f64(rect.x0)),
                top: length(tilo_ui_base::num::f32_of_f64(rect.y0)),
                right: auto(),
                bottom: auto(),
            };
            style.size = taffy::Size {
                width: length(tilo_ui_base::num::f32_of_f64(rect.width())),
                height: length(tilo_ui_base::num::f32_of_f64(rect.height())),
            };
            if *current != style {
                styles.set_style(child, style);
            }
        }
        let target = length(tilo_ui_base::num::f32_of_f64(height));
        if let Some(rows) = self.rows_height
            && styles.style(rows).size.height != target
        {
            let mut style = styles.style(rows).clone();
            style.size.height = target;
            styles.set_style(rows, style);
        }
    }
}

impl Widget for MediaGrid {
    tilo_ui_base::described!();

    /// A grid whose cards a host renders draws the rows its list holds in place of the list, so
    /// what that list scrolls stands in the grid's box: a row past the grid's edge is a row of a
    /// scroller, not content over its container.
    fn layout_info(&self) -> Option<tilo_ui_base::widget::LayoutInfo> {
        use tilo_ui_base::widget::{LayoutInfo, ScrollDirection, ScrollLayout};
        self.inner.as_ref()?;
        Some(LayoutInfo::Scroll(ScrollLayout {
            direction: ScrollDirection::Vertical,
        }))
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        if self.inner.is_some() {
            let rhythm = &theme.tokens.spacing.rhythm;
            self.extent = self.size.extent(theme);
            self.column_gap = f64::from(rhythm.section);
            self.row_gap = f64::from(rhythm.group);
        }
        if let Some(inner) = self.inner.as_mut() {
            let node = inner.build_layout(taffy, theme, node_map);
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            return node;
        }
        let rhythm = &theme.tokens.spacing.rhythm;
        self.extent = self.size.extent(theme);
        let (column_gap, row_gap) = match self.mode {
            GridMode::Columns => (f64::from(rhythm.section), f64::from(rhythm.group)),
            GridMode::Justified => {
                let tight = f64::from(rhythm.tight);
                (tight, tight)
            }
        };
        self.column_gap = column_gap;
        self.row_gap = row_gap;
        if matches!(self.mode, GridMode::Columns) {
            // A card the grid has not sized yet is solved at an estimate,
            // and only the ones that land near the viewport are measured.
            for card in self.cards_mut() {
                card.text_estimated |= card.shared_text_height.is_none();
            }
        }
        self.nodes = self
            .cards
            .iter_mut()
            .map(|card| card.build_layout(taffy, theme, node_map))
            .collect();
        let style = match self.mode {
            GridMode::Columns => Style {
                display: taffy::Display::Grid,
                grid_template_columns: vec![taffy::style_helpers::minmax(
                    length(0.0_f32),
                    taffy::style_helpers::fr(1.0_f32),
                )],
                grid_auto_rows: vec![taffy::style_helpers::min_content()],
                gap: taffy::Size {
                    width: length(tilo_ui_base::num::f32_of_f64(column_gap)),
                    height: length(tilo_ui_base::num::f32_of_f64(row_gap)),
                },
                size: taffy::Size {
                    width: percent(1.0_f32),
                    height: auto(),
                },
                min_size: taffy::Size {
                    width: length(0.0_f32),
                    height: auto(),
                },
                align_items: Some(taffy::AlignItems::Start),
                flex_shrink: 0.0,
                ..Default::default()
            },
            GridMode::Justified => Style {
                display: taffy::Display::Block,
                size: taffy::Size {
                    width: percent(1.0_f32),
                    height: auto(),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
        };
        self.rows_height = matches!(self.mode, GridMode::Justified).then(|| {
            taffy.leaf_node(Style {
                size: taffy::Size {
                    width: length(0.0_f32),
                    height: length(0.0_f32),
                },
                ..Default::default()
            })
        });
        let children: Vec<NodeId> = self.nodes.iter().copied().chain(self.rows_height).collect();
        let node = taffy.parent_node(style, &children);
        let _ = taffy.set_restyled_for_width(node, true);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    /// Its columns, and the text height each row of cards shares, or its justified rows, for the
    /// width it is laid out at.
    fn style_for_width(
        &mut self,
        styles: &mut taffy::Restyle<'_>,
        width: taffy::AvailableSpace,
        text: &mut TextRenderer,
    ) {
        if let Some(inner) = self.inner.as_mut() {
            if let taffy::AvailableSpace::Definite(width) = width {
                let count = card_columns(f64::from(width), self.extent, self.column_gap).max(1);
                if let Some(cell) = &self.columns {
                    cell.set(count);
                }
            }
            inner.style_for_width(styles, width, text);
            return;
        }
        let taffy::AvailableSpace::Definite(width) = width else {
            return;
        };
        let width = f64::from(width);
        match self.mode {
            GridMode::Columns => self.size_columns(styles, width, text),
            GridMode::Justified => self.justify(styles, width),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        if let Some(inner) = self.inner.as_mut() {
            inner.apply_layout(taffy, node, origin);
            // The width the grid was finally laid out at, not the space it was offered while
            // the solve read it: a hosted grid's bands read how many columns fit here, and a
            // width the solve measured without one leaves every band a single card wide.
            let rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
            if let Some(cell) = &self.columns {
                cell.set(self.columns_for_width(rect.width()).max(1));
            }
            self.rect = rect;
            return;
        }
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        let origin = self.rect.origin();
        for (card, &child) in self.cards.iter_mut().zip(&self.nodes) {
            card.apply_layout(taffy, child, origin);
        }
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        if let Some(inner) = self.inner.as_mut() {
            tilo_ui_base::widget::draw_child(
                &mut **inner,
                scene,
                transform,
                text_renderer,
                theme,
                &env,
            );
            return;
        }
        if matches!(self.mode, GridMode::Columns) {
            let top = self.rect.y0;
            self.window = env
                .viewport
                .map_or((f64::NEG_INFINITY, f64::INFINITY), |vp| {
                    (vp.y0 - top, vp.y1 - top)
                });
            let window = self.window;
            for card in self.cards_mut() {
                card.wants_measure |=
                    card.text_estimated && near(window, card.rect.y0 - top, card.rect.y1 - top);
            }
        }
        for card in &mut self.cards {
            card.draw_child(scene, transform, text_renderer, theme, &env);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        if self.range_event(event, ctx) {
            return;
        }
        if let Some(inner) = self.inner.as_mut() {
            inner.handle_event(event, ctx);
            return;
        }
        self.dirty = true;
        for (card, &node) in self.cards.iter_mut().zip(&self.nodes) {
            card.handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if *ctx.handled {
                break;
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if let Some(inner) = &self.inner {
            return inner.children();
        }
        self.cards.iter().map(AsRef::as_ref).collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if let Some(inner) = &self.inner {
            inner.for_each_child(visitor);
            return;
        }
        for card in &self.cards {
            visitor(card.as_ref());
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if let Some(inner) = self.inner.as_mut() {
            return inner.children_mut();
        }
        self.cards
            .iter_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    /// Its box is the rows of cards it holds: it asks layout when a card does, and a grid with
    /// none has no row to size.
    fn is_layout_dirty(&self) -> bool {
        if let Some(inner) = &self.inner {
            return inner.is_layout_dirty();
        }
        self.cards.iter().any(|card| card.is_layout_dirty())
    }

    fn mark_drawn(&mut self) {
        if let Some(inner) = self.inner.as_mut() {
            inner.mark_drawn();
            return;
        }
        self.dirty = false;
        for card in &mut self.cards {
            card.mark_drawn();
        }
    }

    fn get_rect(&self) -> Rect {
        if let Some(inner) = &self.inner {
            return inner.get_rect();
        }
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        if let Some(inner) = &self.inner {
            return inner.needs_redraw();
        }
        tilo_ui_base::widget::redraw_or_children(self.dirty, self)
    }

    // A grid a host renders stands in the tree for its list: the list's rows, its scroll and the
    // pages it asks for are the grid's, so the frames that pull a list's model into it — a page
    // of rows arriving, the columns a new width holds — reach the list through the grid, never
    // around it.
    fn sync(
        &mut self,
        state: &tilo_ui_base::widget::AppModel,
        path: &tilo_ui_base::widget::ModelPath,
    ) {
        if let Some(inner) = self.inner.as_mut() {
            inner.sync(state, path);
            return;
        }
        for card in &mut self.cards {
            card.sync(state, path);
        }
    }

    fn update(
        &mut self,
        state: &tilo_ui_base::widget::AppModel,
        path: &tilo_ui_base::widget::ModelPath,
    ) -> bool {
        if let Some(inner) = self.inner.as_mut() {
            return inner.update(state, path);
        }
        let mut changed = false;
        for card in &mut self.cards {
            changed |= card.update(state, path);
        }
        changed
    }

    fn take_structure_dirty(&mut self) -> bool {
        if let Some(inner) = self.inner.as_mut() {
            return inner.take_structure_dirty();
        }
        let mut dirty = false;
        for card in &mut self.cards {
            dirty |= card.take_structure_dirty();
        }
        dirty
    }

    fn needs_animation_frame(&self) -> bool {
        if let Some(inner) = &self.inner {
            return inner.needs_animation_frame();
        }
        self.cards.iter().any(|card| card.needs_animation_frame())
    }

    fn next_wake_deadline(&self) -> Option<tilo_ui_base::time::Instant> {
        if let Some(inner) = &self.inner {
            return inner.next_wake_deadline();
        }
        self.cards
            .iter()
            .filter_map(|card| card.next_wake_deadline())
            .min()
    }

    fn has_pending_visual_work(&self) -> bool {
        if let Some(inner) = &self.inner {
            return inner.has_pending_visual_work();
        }
        self.cards.iter().any(|card| card.has_pending_visual_work())
    }

    fn layout_needs_measure(&self) -> bool {
        if let Some(inner) = &self.inner {
            return inner.layout_needs_measure();
        }
        self.cards.iter().any(|card| card.layout_needs_measure()) || self.is_layout_dirty()
    }

    fn name(&self) -> &'static str {
        "MediaGrid"
    }
}
