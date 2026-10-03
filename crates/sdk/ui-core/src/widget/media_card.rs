//! `MediaCard` and `MediaGrid`: albums, videos, books, photos, apps.
//!
//! A media card is the art (a cover, a thumbnail, a photo) with its title and
//! up to two meta lines under it. The art sits in the content with a seated
//! edge and no shadow (only floating things cast one). Overlay badges over
//! the art (a duration, watched progress, a live mark, a downloaded mark)
//! use the floating material, the same as all chrome over media. Under the
//! pointer the card lifts; keyboard focus rings the art in your colour, so
//! the two never look the same.
//!
//! Art that is not loaded yet, or does not exist, is generated: a smooth
//! neutral gradient with the title's initials sized from the tile, or the
//! app's icon for an app.
//!
//! [`MediaGrid`] reflows cards into columns from the available width, with
//! every row's text block as tall as its tallest so titles line up, or into
//! justified rows of photos at their own aspect ratios
//! ([`MediaGrid::justified`], [`justified_rows`]).

use std::collections::HashMap;

use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{self, Brush, Fill, Gradient};
use winit::event::WindowEvent;

use crate::ImageStore;
use crate::icons::{AppImage, Icon};
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::activation::{Activation, Interaction, activation_controls};
use crate::widget::{Element, TaffyTreeExt, Widget, card_column_width, card_columns};

/// The shape of a card's art.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum MediaAspect {
    /// Album art, podcasts, app icons.
    #[default]
    Square,
    /// Video thumbnails (16:9).
    Wide,
    /// Book covers and posters (2:3).
    Portrait,
    /// A photo's own width / height.
    Ratio(f64),
}

impl MediaAspect {
    /// Width over height.
    #[must_use]
    pub fn ratio(self) -> f64 {
        match self {
            Self::Wide => 16.0 / 9.0,
            Self::Portrait => 2.0 / 3.0,
            Self::Ratio(r) if r.is_finite() && r > 0.0 => r,
            Self::Square | Self::Ratio(_) => 1.0,
        }
    }
}

/// What a card's art is.
#[derive(Clone, Copy, Debug)]
pub enum MediaArt<'a> {
    /// A decoded picture, cover-fitted.
    Image(&'a peniko::ImageData),
    /// An app's icon, fitted inside a quiet tile.
    AppIcon(&'a AppImage),
    /// No art (yet): the generated gradient and initials.
    Placeholder,
}

/// An overlay mark on the art.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MediaBadge<'a> {
    /// A duration or count at the bottom-trailing corner ("12:04").
    Duration(&'a str),
    /// Watched / read share, `0.0..=1.0`, along the bottom edge.
    Progress(f64),
    /// Broadcasting now, at the top-leading corner.
    Live,
    /// Available offline, at the top-trailing corner.
    Downloaded,
}

/// How large the cards of a grid are, in lines of body text so they grow
/// with the text scale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MediaSize {
    Small,
    #[default]
    Regular,
    Large,
}

impl MediaSize {
    /// The minimum column width (or, for justified rows, the target row
    /// height) under `theme`.
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

/// The resolved geometry of a media card in `theme`.
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
    progress: f64,
    action: f64,
    icon: f64,
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
            progress: f64::from(metrics.chrome_rule_px) * 3.0,
            action: f64::from(metrics.bar_height),
            icon: f64::from(metrics.icon_md),
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
    meta: [Option<&'a str>; 2],
    badges: &'a [MediaBadge<'a>],
    hover_action: Option<Icon>,
    selected: bool,
    interaction: Interaction,
}

impl<'a> MediaCardFace<'a> {
    #[must_use]
    pub fn new(art: MediaArt<'a>) -> Self {
        Self {
            art,
            aspect: MediaAspect::Square,
            title: None,
            meta: [None, None],
            badges: &[],
            hover_action: None,
            selected: false,
            interaction: Interaction::default(),
        }
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

    /// The height of the text block under the art at `width` (0 for an
    /// art-only card). A grid passes the tallest of a row to every card in
    /// it, so their titles line up.
    pub fn text_height(&self, text: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        let m = MediaMetrics::new(theme);
        self.text_height_with(text, &m, width)
    }

    fn text_height_with(&self, text: &mut TextRenderer, m: &MediaMetrics, width: f64) -> f64 {
        let Some(title) = self.title else {
            return 0.0;
        };
        let wrap = crate::num::f32_of_f64(width.max(1.0));
        let lines = text.clamp_to_lines(title, &m.title, wrap, 2).len().max(1);
        let mut h = crate::num::f64_of_usize(lines) * MediaMetrics::line(text, &m.title);
        let meta_lines = self.meta.iter().flatten().count();
        h += crate::num::f64_of_usize(meta_lines) * MediaMetrics::line(text, &m.meta);
        (m.text_gap + h).ceil()
    }

    /// The card's total height at `width`, with its own text block.
    pub fn height(&self, text: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        let m = MediaMetrics::new(theme);
        width / self.aspect.ratio() + self.text_height_with(text, &m, width)
    }

    /// The art's rect inside a card at `rect`.
    #[must_use]
    pub fn art_rect(&self, rect: Rect) -> Rect {
        let h = (rect.width() / self.aspect.ratio()).min(rect.height());
        Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + h)
    }

    /// Where the hover action sits, when the card shows one.
    pub fn hover_action_rect(&self, rect: Rect, theme: &Theme) -> Option<Rect> {
        self.hover_action.map(|_| {
            let m = MediaMetrics::new(theme);
            Rect::from_center_size(self.art_rect(rect).center(), (m.action, m.action))
        })
    }

    /// Paint the card into `rect`: the art at the top, the text under it.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let theme = ctx.theme;
        let m = MediaMetrics::new(theme);
        let colors = &theme.tokens.colors;

        // Hover and selection are a lift plate behind the whole card.
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
        paint_art(ctx, &m, art, self.art, self.title.unwrap_or_default());
        seat(ctx, art, m.radius);
        for badge in self.badges {
            paint_badge(ctx, &m, art, *badge);
        }
        if let Some(icon) = self.hover_action
            && (self.interaction.hovered || self.interaction.focused)
            && let Some(target) = self.hover_action_rect(rect, theme)
        {
            let disc = RoundedRect::from_rect(target, target.width() / 2.0);
            crate::floating_surface::paint_material(
                ctx.scene,
                ctx.transform,
                &disc,
                crate::floating_surface::Material::Floating,
                theme,
            );
            let glyph = Rect::from_center_size(target.center(), (m.icon, m.icon));
            icon.draw(ctx.scene, ctx.transform, glyph, theme.text_primary);
        }
        if self.interaction.focused && !self.interaction.disabled {
            crate::widget::paint_focus_ring(ctx.scene, ctx.transform, art, m.radius, theme);
        }

        if let Some(title) = self.title {
            let width = crate::num::f32_of_f64(rect.width().max(1.0));
            let mut y = art.y1 + m.text_gap;
            let title_line = MediaMetrics::line(ctx.text, &m.title);
            for line in ctx.text.clamp_to_lines(title, &m.title, width, 2) {
                ctx.text(
                    &line,
                    &m.title,
                    theme.text_primary,
                    Point::new(rect.x0, y),
                    None,
                );
                y += title_line;
            }
            let meta_line = MediaMetrics::line(ctx.text, &m.meta);
            for meta in self.meta.iter().flatten() {
                let line = ctx.text.elide_to_width_style(meta, width, &m.meta);
                ctx.text(&line, &m.meta, theme.text_dim, Point::new(rect.x0, y), None);
                y += meta_line;
            }
        }
    }

    /// The card's accessibility node: named by its title, valued by its
    /// meta lines and badges.
    pub fn accessibility(&self, rect: Rect) -> crate::a11y::AccessNode {
        use crate::a11y::{AccessNode, AccessState, Role};
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
                    value.push(format!("{}% watched", (p.clamp(0.0, 1.0) * 100.0).round()));
                }
                MediaBadge::Live => value.push("Live".to_owned()),
                MediaBadge::Downloaded => value.push("Downloaded".to_owned()),
            }
        }
        let mut node = AccessNode::new(Role::Button)
            .with_optional_name(self.title.map(str::to_owned))
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

/// The seated edge: a hairline just inside the art's outline, derived
/// against the ground, so bright or dark art never bleeds into it.
fn seat(ctx: &mut PaintCtx<'_>, art: Rect, radius: f64) {
    let ground = ctx.theme.tokens.colors.surfaces.content;
    ctx.scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        ctx.transform,
        crate::tokens::colors::Colors::hairline_on(ground, SEAT_ALPHA),
        None,
        &crate::surface::edge_outline(art, radius),
    );
}

/// The seated edge's strength: the control hairline (HIG §9.5).
const SEAT_ALPHA: u8 = 26;

fn paint_art(ctx: &mut PaintCtx<'_>, m: &MediaMetrics, art: Rect, kind: MediaArt<'_>, title: &str) {
    let theme = ctx.theme;
    let colors = &theme.tokens.colors;
    match kind {
        MediaArt::Image(image) => {
            crate::image_store::paint_image_cover(ctx.scene, ctx.transform, art, image, m.radius);
        }
        MediaArt::AppIcon(icon) => {
            let raised = colors.surfaces.raised;
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                raised,
                None,
                &RoundedRect::from_rect(art, m.radius),
            );
            let side = art.width().min(art.height()) * 0.6;
            let slot = Rect::from_center_size(art.center(), (side, side));
            if !crate::icons::draw_app_image(ctx.scene, ctx.transform, slot, icon) {
                paint_placeholder(ctx, m, art, title);
            }
        }
        MediaArt::Placeholder => paint_placeholder(ctx, m, art, title),
    }
}

/// One smooth diagonal gradient (no stacked rects, so no seams) between two
/// steps of the raised surface, with the title's initials sized from the
/// tile.
fn paint_placeholder(ctx: &mut PaintCtx<'_>, m: &MediaMetrics, art: Rect, title: &str) {
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
        &RoundedRect::from_rect(art, m.radius),
    );
    let initials = crate::widget::initials_for_name(title);
    if initials.is_empty() {
        return;
    }
    let side = art.width().min(art.height());
    let style = m
        .initials
        .clone()
        .with_content_size(crate::num::f32_of_f64(side * PLACEHOLDER_INITIALS_SHARE));
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

/// The generated art's initials, as a share of the tile's side.
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
            crate::floating_surface::paint_material(
                ctx.scene,
                ctx.transform,
                &shape,
                crate::floating_surface::Material::Floating,
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
                crate::widget::avatar::initial_ink(fill, theme),
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
            crate::floating_surface::paint_material(
                ctx.scene,
                ctx.transform,
                &RoundedRect::from_rect(disc, side / 2.0),
                crate::floating_surface::Material::Floating,
                theme,
            );
            let glyph = Rect::from_center_size(disc.center(), (m.icon * 0.75, m.icon * 0.75));
            Icon::Download.draw(ctx.scene, ctx.transform, glyph, theme.text_primary);
        }
        MediaBadge::Progress(share) => {
            let share = share.clamp(0.0, 1.0);
            let track = Rect::new(art.x0, art.y1 - m.progress, art.x1, art.y1);
            // Clip to the art's rounded corners so the bar follows them.
            let clip = RoundedRect::from_rect(art, m.radius);
            ctx.scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                ctx.transform,
                &clip,
            );
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                colors.surfaces.overlay,
                None,
                &track,
            );
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                colors.intent.inverted,
                None,
                &Rect::new(
                    track.x0,
                    track.y0,
                    track.x0 + track.width() * share,
                    track.y1,
                ),
            );
            ctx.scene.pop_layer();
        }
    }
}

/// Where a retained card's art comes from.
pub enum MediaSource {
    Image(peniko::ImageData),
    /// Loaded in the background; the placeholder shows until it lands.
    Url {
        store: ImageStore,
        url: String,
    },
    AppIcon(AppImage),
    Placeholder,
}

/// Owned badge for the retained card.
#[derive(Clone, Debug, PartialEq)]
pub enum OwnedBadge {
    Duration(String),
    Progress(f64),
    Live,
    Downloaded,
}

impl OwnedBadge {
    fn borrowed(&self) -> MediaBadge<'_> {
        match self {
            Self::Duration(d) => MediaBadge::Duration(d),
            Self::Progress(p) => MediaBadge::Progress(*p),
            Self::Live => MediaBadge::Live,
            Self::Downloaded => MediaBadge::Downloaded,
        }
    }
}

/// A retained media card. See the module docs.
pub struct MediaCard {
    source: MediaSource,
    loaded: Option<peniko::ImageData>,
    aspect: MediaAspect,
    title: Option<String>,
    meta: Vec<String>,
    badges: Vec<OwnedBadge>,
    hover_action: Option<(Icon, String, Box<dyn FnMut()>)>,
    pressed_action: bool,
    selected: bool,
    activation: Activation,
    metrics: Option<MediaMetrics>,
    /// Set by a [`MediaGrid`] so every card of a row shares one text height.
    shared_text_height: Option<f64>,
    rect: Rect,
    dirty: bool,
}

impl MediaCard {
    pub fn new(id: impl Into<String>, source: MediaSource) -> Self {
        let loaded = match &source {
            MediaSource::Image(image) => Some(image.clone()),
            _ => None,
        };
        Self {
            source,
            loaded,
            aspect: MediaAspect::Square,
            title: None,
            meta: Vec::new(),
            badges: Vec::new(),
            hover_action: None,
            pressed_action: false,
            selected: false,
            activation: Activation::new(id),
            metrics: None,
            shared_text_height: None,
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

    /// See [`MediaCardFace::meta`]; at most two lines.
    #[must_use]
    pub fn meta(mut self, line: impl Into<String>) -> Self {
        if self.meta.len() < 2 {
            self.meta.push(line.into());
        }
        self
    }

    #[must_use]
    pub fn badge(mut self, badge: OwnedBadge) -> Self {
        self.badges.push(badge);
        self
    }

    /// A centred action over the art on hover; `label` names it.
    #[must_use]
    pub fn hover_action(
        mut self,
        icon: Icon,
        label: impl Into<String>,
        on_activate: impl FnMut() + 'static,
    ) -> Self {
        self.hover_action = Some((icon, label.into(), Box::new(on_activate)));
        self
    }

    #[must_use]
    pub fn on_activate(mut self, f: impl FnMut() + 'static) -> Self {
        self.activation.set_callback(Box::new(f));
        self
    }

    #[must_use]
    pub fn on_action(mut self, binding: tilo_actions::ActionBinding<()>) -> Self {
        self.activation.set_action(binding);
        self
    }

    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn with_face<R>(&self, f: impl FnOnce(&MediaCardFace<'_>) -> R) -> R {
        let badges: Vec<MediaBadge<'_>> = self.badges.iter().map(OwnedBadge::borrowed).collect();
        let art = match (&self.loaded, &self.source) {
            (Some(image), _) => MediaArt::Image(image),
            (None, MediaSource::AppIcon(icon)) => MediaArt::AppIcon(icon),
            _ => MediaArt::Placeholder,
        };
        let mut face = MediaCardFace::new(art)
            .aspect(self.aspect)
            .badges(&badges)
            .selected(self.selected)
            .interaction(self.activation.state);
        if let Some(title) = &self.title {
            face = face.title(title);
        }
        for line in &self.meta {
            face = face.meta(line);
        }
        if let Some((icon, _, _)) = &self.hover_action {
            face = face.hover_action(*icon);
        }
        f(&face)
    }

    fn own_text_height(&self, text: &mut TextRenderer, width: f64) -> f64 {
        let Some(m) = &self.metrics else {
            return 0.0;
        };
        self.with_face(|face| face.text_height_with(text, m, width))
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

impl Widget for MediaCard {
    crate::provides!(dyn crate::widget::controls::ActionControls);

    fn sync(&mut self, _state: &mut crate::widget::AppModel, _path: &crate::widget::ModelPath) {
        if self.loaded.is_none()
            && let MediaSource::Url { store, url } = &self.source
            && let Some(image) = store.get(url)
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
        let node = taffy.leaf_node(Style {
            size: taffy::Size {
                width: percent(1.0_f32),
                height: auto(),
            },
            min_size: taffy::Size {
                width: length(0.0_f32),
                height: auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let width = f64::from(known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w,
            _ => 160.0,
        }));
        let text = self
            .shared_text_height
            .unwrap_or_else(|| self.own_text_height(text_renderer, width));
        taffy::Size {
            width: crate::num::f32_of_f64(width),
            height: crate::num::f32_of_f64(width / self.aspect.ratio() + text),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        let rect = Rect::from_origin_size(
            (
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
            ),
            (f64::from(l.size.width), f64::from(l.size.height)),
        );
        if rect != self.rect {
            self.dirty = true;
        }
        self.rect = rect;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if presence.should_cull(self.rect) {
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
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
            let over = target.contains(self.activation.cursor());
            match state {
                ElementState::Pressed if over => {
                    self.pressed_action = true;
                    *ctx.handled = true;
                    return;
                }
                ElementState::Released if self.pressed_action => {
                    self.pressed_action = false;
                    *ctx.handled = true;
                    if over && let Some((_, _, on_activate)) = &mut self.hover_action {
                        on_activate();
                    }
                    return;
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

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        (self.activation.is_bound() && self.rect.contains(point))
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn is_focusable(&self) -> bool {
        self.activation.is_bound()
    }

    fn set_focus(&mut self, focused: bool) {
        if self.activation.state.focused != focused {
            self.activation.state.focused = focused;
            self.dirty = true;
        }
    }

    fn is_focused(&self) -> bool {
        self.activation.state.focused
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::AppModel,
        _root: &crate::widget::ModelPath,
    ) -> bool {
        matches!(action, crate::a11y::Action::Activate) && self.activation.activate()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
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

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let mut node = self
            .with_face(|face| face.accessibility(self.rect))
            .with_semantic_key(self.activation.id());
        if let Some((_, label, _)) = &self.hover_action {
            node = node.with_children(vec![
                crate::a11y::AccessNode::new(crate::a11y::Role::Button).with_name(label.clone()),
            ]);
        }
        Some(node)
    }
}

activation_controls!(MediaCard, activation);

// ---------------------------------------------------------------------------
// Grid geometry
// ---------------------------------------------------------------------------

/// Lay `aspects` (width / height) out in justified rows `width` wide: each
/// row is filled at `target_height`, then scaled so it spans the width
/// exactly. The last row keeps the target height instead of stretching.
/// Returns one rect per item, relative to the origin.
#[must_use]
pub fn justified_rows(aspects: &[f64], width: f64, target_height: f64, gap: f64) -> Vec<Rect> {
    let mut out = Vec::with_capacity(aspects.len());
    if width <= 0.0 || target_height <= 0.0 {
        return out;
    }
    let ratio = |a: f64| if a.is_finite() && a > 0.0 { a } else { 1.0 };
    let mut y = 0.0;
    let mut start = 0;
    while start < aspects.len() {
        let mut end = start;
        let mut row_ratio = 0.0;
        while end < aspects.len() {
            row_ratio += ratio(aspects[end]);
            end += 1;
            let gaps = crate::num::f64_of_usize(end - start - 1) * gap;
            if row_ratio * target_height + gaps >= width {
                break;
            }
        }
        let gaps = crate::num::f64_of_usize(end - start - 1) * gap;
        let full = row_ratio * target_height + gaps >= width;
        let height = if full {
            (width - gaps) / row_ratio
        } else {
            target_height
        };
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

enum GridMode {
    Columns,
    Justified,
}

/// A responsive grid of [`MediaCard`]s. See the module docs.
pub struct MediaGrid {
    cards: Vec<Element>,
    nodes: Vec<NodeId>,
    mode: GridMode,
    size: MediaSize,
    extent: f64,
    column_gap: f64,
    row_gap: f64,
    rect: Rect,
}

impl MediaGrid {
    /// Equal columns, as many as fit `size` cards.
    pub fn new(cards: Vec<MediaCard>) -> Self {
        Self {
            cards: cards.into_iter().map(|c| Box::new(c) as Element).collect(),
            nodes: Vec::new(),
            mode: GridMode::Columns,
            size: MediaSize::Regular,
            extent: 0.0,
            column_gap: 0.0,
            row_gap: 0.0,
            rect: Rect::ZERO,
        }
    }

    /// Justified rows at each card's own [`MediaAspect`] (a photo library):
    /// rows of equal height that span the width exactly.
    pub fn justified(cards: Vec<MediaCard>) -> Self {
        Self {
            mode: GridMode::Justified,
            ..Self::new(cards)
        }
    }

    #[must_use]
    pub fn with_size(mut self, size: MediaSize) -> Self {
        self.size = size;
        self
    }

    fn cards_mut(&mut self) -> impl Iterator<Item = &mut MediaCard> {
        self.cards.iter_mut().filter_map(|card| {
            card.as_any_mut()
                .and_then(|any| any.downcast_mut::<MediaCard>())
        })
    }

    pub fn columns_for_width(&self, width: f64) -> usize {
        card_columns(width, self.extent, self.column_gap).min(self.cards.len().max(1))
    }
}

impl Widget for MediaGrid {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
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
        self.nodes = self
            .cards
            .iter_mut()
            .map(|card| card.build_layout(taffy, theme, node_map))
            .collect();
        let style = match self.mode {
            GridMode::Columns => Style {
                display: taffy::Display::Grid,
                grid_template_columns: vec![taffy::style_helpers::fr(1.0_f32)],
                grid_auto_rows: vec![taffy::style_helpers::min_content()],
                gap: taffy::Size {
                    width: length(crate::num::f32_of_f64(column_gap)),
                    height: length(crate::num::f32_of_f64(row_gap)),
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
                    height: length(0.0_f32),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
        };
        let node = taffy.parent_node(style, &self.nodes);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn adjust_layout_with_measure(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        let Ok(layout) = taffy.layout(node) else {
            return false;
        };
        let width = f64::from(layout.size.width);
        let mut changed = false;
        match self.mode {
            GridMode::Columns => {
                let columns = self.columns_for_width(width);
                let column = card_column_width(width, columns, self.column_gap);
                let tracks = vec![length(crate::num::f32_of_f64(column)); columns];
                if let Ok(current) = taffy.style(node)
                    && current.grid_template_columns != tracks
                {
                    let mut style = current.clone();
                    style.grid_template_columns = tracks;
                    changed |= taffy.set_style(node, style).is_ok();
                }
                // Every card of a row shares the row's tallest text block.
                let heights: Vec<f64> = self
                    .cards_mut()
                    .map(|card| card.own_text_height(text, column))
                    .collect();
                let mut shared = Vec::with_capacity(heights.len());
                for row in heights.chunks(columns.max(1)) {
                    let tallest = row.iter().copied().fold(0.0, f64::max);
                    shared.extend(std::iter::repeat_n(tallest, row.len()));
                }
                let mut cards: Vec<&mut MediaCard> = self.cards_mut().collect();
                for (card, height) in cards.iter_mut().zip(shared) {
                    if card.shared_text_height != Some(height) {
                        card.shared_text_height = Some(height);
                        changed = true;
                    }
                }
                if changed {
                    for &child in &self.nodes {
                        let _ = taffy.mark_dirty(child);
                    }
                }
            }
            GridMode::Justified => {
                let aspects: Vec<f64> = self.cards_mut().map(|card| card.aspect.ratio()).collect();
                let rects = justified_rows(&aspects, width, self.extent, self.column_gap);
                let height = rects.iter().map(|r| r.y1).fold(0.0, f64::max);
                for (&child, rect) in self.nodes.iter().zip(&rects) {
                    if let Ok(current) = taffy.style(child) {
                        let mut style = current.clone();
                        style.position = taffy::Position::Absolute;
                        style.inset = taffy::Rect {
                            left: length(crate::num::f32_of_f64(rect.x0)),
                            top: length(crate::num::f32_of_f64(rect.y0)),
                            right: auto(),
                            bottom: auto(),
                        };
                        style.size = taffy::Size {
                            width: length(crate::num::f32_of_f64(rect.width())),
                            height: length(crate::num::f32_of_f64(rect.height())),
                        };
                        if *current != style {
                            changed |= taffy.set_style(child, style).is_ok();
                        }
                    }
                }
                if let Ok(current) = taffy.style(node) {
                    let target = length(crate::num::f32_of_f64(height));
                    if current.size.height != target {
                        let mut style = current.clone();
                        style.size.height = target;
                        changed |= taffy.set_style(node, style).is_ok();
                    }
                }
            }
        }
        changed
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::from_origin_size(
            (
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
            ),
            (f64::from(l.size.width), f64::from(l.size.height)),
        );
        let origin = self.rect.origin();
        for (card, &child) in self.cards.iter_mut().zip(&self.nodes) {
            card.apply_layout(taffy, child, origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for card in &mut self.cards {
            card.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        for (card, &node) in self.cards.iter_mut().zip(&self.nodes) {
            card.handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if *ctx.handled {
                break;
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.cards.iter().map(AsRef::as_ref).collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for card in &self.cards {
            visitor(card.as_ref());
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.cards
            .iter_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    fn mark_drawn(&mut self) {
        for card in &mut self.cards {
            card.mark_drawn();
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "MediaGrid"
    }
}

#[cfg(test)]
mod tests;
