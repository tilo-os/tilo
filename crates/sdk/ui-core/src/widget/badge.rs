//! Badge widget — the non-interactive member of the chip family: a small
//! pill typically anchored over another element (avatar, icon, row). Three
//! contents:
//!
//! - [`BadgeContent::Dot`] — a small filled circle, no text.
//! - [`BadgeContent::Count`] — `0` renders nothing; values `>= 100` clamp to
//!   `"99+"` so the pill never grows past the visual budget reserved for it.
//! - [`BadgeContent::Text`] — a short status ("Owner", "Beta").
//!
//! A badge takes a [`ChipStyle`], never a colour. A count or a dot is
//! [`ChipStyle::Live`] by default (something new happened), filled in your
//! colour like a sidebar's unread count; a text badge is
//! [`ChipStyle::Neutral`]. A status reads as `ChipStyle::Tinted(Tone::…)`.
//! Canvas hosts paint the same pill with [`BadgeFace`].
//!
//! A "modified" mark (unsaved changes) is a dot in neutral primary ink
//! ([`Badge::modified`], [`BadgeFace::modified`]): it is not new and not
//! anyone's, so it takes no colour.
//!
//! Positioning is the caller's responsibility — `Badge` does not anchor
//! itself to another widget.

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::chip::{ChipLead, ChipStyle, PillPaint, paint_pill};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};

/// See module docs for content semantics.
#[derive(Clone, Debug)]
pub enum BadgeContent {
    Dot,
    /// Unsaved changes: a dot in neutral primary ink.
    Modified,
    Count(u32),
    Text(String),
}

/// The ink of a badge dot: the style's fill, or neutral primary ink for a
/// modified mark.
fn dot_colour(style: ChipStyle, modified: bool, theme: &Theme) -> vello::peniko::Color {
    if modified {
        theme.text_primary
    } else {
        style
            .colors(theme, theme.tokens.colors.surfaces.content, false, false)
            .fill
    }
}

/// Paint a badge dot centred in `rect`.
fn paint_dot(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    style: ChipStyle,
    modified: bool,
    theme: &Theme,
) {
    let radius = BadgeMetrics::of(theme).dot / 2.0;
    scene.fill(
        vello::peniko::Fill::NonZero,
        transform,
        dot_colour(style, modified, theme),
        None,
        &Circle::new(rect.center(), radius),
    );
}

/// The theme inputs a badge's paint reads, so a theme switch that keeps the
/// layout (dark to light, a new presence colour) repaints a cached badge.
type ThemeKey = (
    vello::peniko::Color,
    vello::peniko::Color,
    vello::peniko::Color,
    Option<vello::peniko::Color>,
    vello::peniko::Color,
);

fn theme_key(style: ChipStyle, theme: &Theme) -> ThemeKey {
    let colors = style.colors(theme, theme.tokens.colors.surfaces.content, false, false);
    (
        colors.fill,
        colors.ink,
        theme.text_primary,
        colors.edge,
        theme.tokens.colors.surfaces.content,
    )
}

/// A badge's geometry in one theme.
#[derive(Clone, Debug, PartialEq)]
struct BadgeMetrics {
    /// The caption line plus `spacing.s0` a side: a pill as tight as a
    /// badge can be and still hold its text at any text scale.
    height: f64,
    pad_x: f64,
    dot: f64,
    caption: crate::text_render::TextStyle,
}

impl BadgeMetrics {
    fn of(theme: &Theme) -> Self {
        let line = f64::from(theme.text(TextRole::Caption).line_height_px());
        let height = (line + 2.0 * f64::from(theme.tokens.spacing.s0)).ceil();
        Self {
            height,
            pad_x: f64::from(theme.tokens.spacing.s2).max((height / 3.0).round()),
            dot: f64::from(theme.tokens.spacing.s2),
            caption: theme.text(TextRole::Caption),
        }
    }

    fn width(&self, label: &str, text: &mut TextRenderer) -> f64 {
        if label.is_empty() {
            return self.dot;
        }
        let (w, _) = text.measure_style(label, None, &self.caption);
        (f64::from(w) + 2.0 * self.pad_x).ceil().max(self.height)
    }
}

/// An immediate-mode badge for hosts that own their row geometry (shell
/// overlays, painted lists): the [`Badge`] look without a layout node.
pub struct BadgeFace<'a> {
    rect: Rect,
    label: &'a str,
    style: ChipStyle,
    /// A dot rather than a pill.
    dot: bool,
    modified: bool,
}

impl<'a> BadgeFace<'a> {
    pub fn new(rect: Rect, label: &'a str) -> Self {
        Self {
            rect,
            label,
            style: ChipStyle::Neutral,
            dot: false,
            modified: false,
        }
    }

    /// A dot centred in `rect`: something new, in your colour (or the
    /// style's tone).
    #[must_use]
    pub fn dot(rect: Rect) -> Self {
        Self {
            rect,
            label: "",
            style: ChipStyle::Live,
            dot: true,
            modified: false,
        }
    }

    /// Unsaved changes: a dot in neutral primary ink, centred in `rect`.
    #[must_use]
    pub fn modified(rect: Rect) -> Self {
        Self {
            modified: true,
            ..Self::dot(rect)
        }
    }

    #[must_use]
    pub fn style(mut self, style: ChipStyle) -> Self {
        self.style = style;
        self
    }
    pub fn height(theme: &Theme) -> f64 {
        BadgeMetrics::of(theme).height
    }
    /// The width a badge needs for `label` in `theme`.
    pub fn intrinsic_width(label: &str, text: &mut TextRenderer, theme: &Theme) -> f64 {
        BadgeMetrics::of(theme).width(label, text)
    }
    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.dot {
            paint_dot(
                scene,
                transform,
                self.rect,
                self.style,
                self.modified,
                theme,
            );
            return;
        }
        if self.label.is_empty() {
            return;
        }
        // The pill is the badge's own height wherever the caller's rect is
        // taller, so its padding comes from its metrics, not the rect.
        let height = BadgeMetrics::of(theme).height.min(self.rect.height());
        let cy = self.rect.center().y;
        let rect = Rect::new(
            self.rect.x0,
            cy - height / 2.0,
            self.rect.x1,
            cy + height / 2.0,
        );
        paint_badge_pill(
            scene,
            transform,
            rect,
            self.label,
            self.style,
            text_renderer,
            theme,
        );
    }
}

/// A badge's pill over `rect`: `bare` paints the label alone, at
/// `opacity`, over a ground painted once.
fn badge_pill(
    rect: Rect,
    label: &str,
    style: ChipStyle,
    bare: bool,
    opacity: f32,
) -> PillPaint<'_> {
    PillPaint {
        rect,
        label,
        lead: ChipLead::None,
        style,
        selected: false,
        hovered: false,
        remove: None,
        menu: false,
        bare,
        opacity,
        role: TextRole::Caption,
        centred: true,
    }
}

fn paint_badge_pill(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    label: &str,
    style: ChipStyle,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    paint_pill(
        &PillPaint {
            rect,
            label,
            lead: ChipLead::None,
            style,
            selected: false,
            hovered: false,
            remove: None,
            menu: false,
            bare: false,
            opacity: 1.0,
            role: TextRole::Caption,
            centred: true,
        },
        scene,
        transform,
        text_renderer,
        theme,
    );
}

pub struct Badge {
    content: BadgeContent,
    style: ChipStyle,
    rect: Rect,
    cached: vello::Scene,
    dirty: bool,
    cache_rect: Rect,
    /// The geometry seen in `build_layout`, for the measured width.
    metrics: Option<BadgeMetrics>,
    /// The theme inputs `cached` was painted from.
    cache_theme: Option<ThemeKey>,
    /// The label shown, cross-fading from the one before when the count
    /// changes while the badge is on screen (`None`: a dot).
    label: crate::motion::CrossFade<Option<String>>,
    /// The content `label` shows, while it leaves: a badge whose count went
    /// to nothing is drawn going.
    leaving: Option<BadgeContent>,
    /// Whether the badge shows, appearing and leaving from its centre.
    presence: crate::motion::Presence,
    /// A frame with the badge in it has been presented: only a badge on
    /// screen moves when its count changes.
    on_screen: bool,
    /// The frame being drawn has the badge's place in it.
    drawing: bool,
    /// The last frame drew the badge on its way, so its resting look is
    /// still to be painted (and cached).
    moved_drawn: bool,
    /// The content changed since the last layout.
    layout_dirty: bool,
    /// The layout keeps room for a leaving badge until it has gone.
    laid_out_leaving: bool,
    /// The motion the person allows, as last drawn.
    motion: Option<crate::tokens::EffectiveMotion>,
    /// A view that builds the badge anew on each change: the one built in
    /// its place goes on from it (`motion::keep`), as if its count had
    /// been set.
    hand_on: crate::motion::HandOn<KeptBadge>,
}

/// What a badge leaves for the one a rebuild puts in its place.
struct KeptBadge {
    content: BadgeContent,
    label: crate::motion::CrossFade<Option<String>>,
    leaving: Option<BadgeContent>,
    presence: crate::motion::Presence,
    on_screen: bool,
}

impl Badge {
    fn with_content(content: BadgeContent, style: ChipStyle) -> Self {
        let shows = shows(&content);
        Self {
            label: crate::motion::CrossFade::new(label_of(&content)),
            content,
            style,
            rect: Rect::ZERO,
            cached: vello::Scene::new(),
            dirty: true,
            cache_rect: Rect::ZERO,
            metrics: None,
            cache_theme: None,
            leaving: None,
            presence: if shows {
                crate::motion::Presence::opened()
            } else {
                crate::motion::Presence::closed()
            },
            on_screen: false,
            drawing: false,
            moved_drawn: false,
            layout_dirty: false,
            laid_out_leaving: false,
            motion: None,
            hand_on: crate::motion::HandOn::default(),
        }
    }

    /// Go on from the badge a rebuild dropped, as that badge would have
    /// gone on had its content been set to this one's: a count changed on
    /// screen cross-fades, one from or to nothing appears or leaves.
    fn take_up(&mut self, kept: KeptBadge) {
        let content = std::mem::replace(&mut self.content, kept.content);
        self.label = kept.label;
        self.leaving = kept.leaving;
        self.presence = kept.presence;
        self.on_screen = kept.on_screen;
        self.set_content(content);
        self.dirty = true;
    }

    /// Show `n` new things. On screen, a changed count cross-fades the old
    /// number out and the new one in, and a badge coming from or going to
    /// nothing appears or leaves from its centre (`motion::Appear`); off
    /// screen, or after a key or an assistive action, it changes at once. It
    /// never pulses or rolls.
    pub fn set_count(&mut self, n: u32) {
        self.set_content(BadgeContent::Count(n));
    }

    /// Show `content` instead, as [`Self::set_count`] does.
    pub fn set_content(&mut self, content: BadgeContent) {
        let (was, will) = (shows(&self.content), shows(&content));
        let label = label_of(&content);
        let unchanged = was == will && (!will || *self.label.shown() == label) && {
            let modified = |c: &BadgeContent| matches!(c, BadgeContent::Modified);
            modified(&self.content) == modified(&content)
        };
        if unchanged {
            self.content = content;
            return;
        }
        let now = crate::motion::frame_now();
        let motion = self.motion.unwrap_or_else(|| {
            crate::tokens::EffectiveMotion::new(crate::tokens::Motion::default(), false)
        });
        // A change moves only on screen, and lands after a key or an action;
        // a count ticking while the person looks at it cross-fades.
        let source = if self.on_screen {
            crate::motion::recent_input(now).unwrap_or(crate::tokens::MotionSource::Pointer)
        } else {
            crate::tokens::MotionSource::Action
        };
        match (was, will) {
            (true, true) => self.label.show(label, motion, source, now),
            (false, true) => {
                self.leaving = None;
                self.label = crate::motion::CrossFade::new(label);
                self.presence
                    .show(true, crate::motion::Appear::SPEC, motion, source, now);
            }
            (true, false) => {
                self.leaving = Some(std::mem::replace(&mut self.content, content.clone()));
                self.presence
                    .show(false, crate::motion::Appear::SPEC, motion, source, now);
            }
            (false, false) => {}
        }
        if let Some(until) = self
            .presence
            .settles_at()
            .into_iter()
            .chain(self.label.settles_at())
            .max()
        {
            crate::motion::want_frames_until(until);
        }
        if !self.presence.is_visible(now) {
            self.leaving = None;
        }
        self.content = content;
        self.dirty = true;
        self.layout_dirty = true;
    }

    /// Whether the label or the badge itself is on its way at `now`.
    fn is_moving(&self, now: crate::time::Instant) -> bool {
        self.presence.is_moving(now) || self.label.is_moving(now)
    }

    /// What the badge is laid out and drawn as: its content, or what it
    /// showed while that leaves.
    fn drawn_content(&self) -> &BadgeContent {
        self.leaving.as_ref().unwrap_or(&self.content)
    }

    /// The badge on its way at `now`: scaled and faded about its centre as
    /// it comes or goes, its old label fading out over the new one. The
    /// pill stays put under a changing count; only the numbers cross-fade.
    fn paint_moving(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        now: crate::time::Instant,
    ) {
        let content = self.drawn_content();
        let motion = theme.motion();
        let pose =
            crate::motion::Appear::pose(self.presence.progress(now), self.rect.center(), motion);
        let modified = matches!(content, BadgeContent::Modified);
        let bounds = self.rect.inflate(1.0, 1.0);
        let style = self.style;
        let rect = self.rect;
        let incoming = self.label.incoming(now);
        let outgoing = self.label.outgoing(now);
        let shown = self.label.shown().clone();
        pose.paint(scene, transform, bounds, |scene, xform| {
            let pill = |label, bare, opacity| badge_pill(rect, label, style, bare, opacity);
            match (&shown, outgoing) {
                (None, _) => paint_dot(scene, xform, rect, style, modified, theme),
                (Some(label), None) if incoming >= 1.0 => {
                    paint_pill(&pill(label, false, 1.0), scene, xform, text_renderer, theme);
                }
                (shown, outgoing) => {
                    // The ground holds still; the labels trade places on it.
                    paint_pill(&pill("", false, 1.0), scene, xform, text_renderer, theme);
                    if let Some((Some(old), opacity)) = outgoing {
                        paint_pill(
                            &pill(old, true, opacity),
                            scene,
                            xform,
                            text_renderer,
                            theme,
                        );
                    }
                    if let Some(label) = shown {
                        paint_pill(
                            &pill(label, true, incoming),
                            scene,
                            xform,
                            text_renderer,
                            theme,
                        );
                    }
                }
            }
        });
    }

    /// Unsaved changes: a dot in neutral primary ink.
    pub fn modified() -> Self {
        Self::with_content(BadgeContent::Modified, ChipStyle::Neutral)
    }

    /// Something new, without a number.
    pub fn dot() -> Self {
        Self::with_content(BadgeContent::Dot, ChipStyle::Live)
    }

    /// How many new things there are.
    pub fn count(n: u32) -> Self {
        Self::with_content(BadgeContent::Count(n), ChipStyle::Live)
    }

    /// A short status ("Owner", "Beta").
    pub fn text(s: impl Into<String>) -> Self {
        Self::with_content(BadgeContent::Text(s.into()), ChipStyle::Neutral)
    }

    /// What the badge is: neutral, tinted with a tone, or live.
    #[must_use]
    pub fn style(mut self, style: ChipStyle) -> Self {
        self.style = style;
        self
    }

    /// `None` when no text should be drawn (the dot variant, or the
    /// zero-count "nothing to show" case). Centralising the clamp/"99+"
    /// logic here lets tests pin the contract without rendering.
    pub fn display_text(&self) -> Option<String> {
        label_of(&self.content)
    }
}

/// The text `content` shows; `None` for a dot or nothing.
fn label_of(content: &BadgeContent) -> Option<String> {
    match content {
        BadgeContent::Dot | BadgeContent::Modified | BadgeContent::Count(0) => None,
        BadgeContent::Count(n) if *n >= 100 => Some("99+".to_string()),
        BadgeContent::Count(n) => Some(n.to_string()),
        BadgeContent::Text(s) => Some(s.clone()),
    }
}

/// Whether `content` shows anything: a zero count is no badge.
fn shows(content: &BadgeContent) -> bool {
    !matches!(content, BadgeContent::Count(0))
}

impl Widget for Badge {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let metrics = BadgeMetrics::of(theme);
        self.hand_on.build(None);
        self.layout_dirty = false;
        self.laid_out_leaving = self.leaving.is_some();
        let (width, height) = match self.drawn_content() {
            BadgeContent::Dot | BadgeContent::Modified => (metrics.dot, metrics.dot),
            BadgeContent::Count(0) => (0.0, 0.0),
            // `auto`: measured in `measure`, pinned in `adjust_layout_style`.
            _ => (-1.0, metrics.height),
        };
        if self.metrics.as_ref() != Some(&metrics) {
            self.dirty = true;
        }
        self.metrics = Some(metrics);
        // Layout dimensions are bounded logical-pixel measurements; the
        // f64->f32 narrowing for taffy's `Dimension` cannot lose meaningful
        // precision at UI scale.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "bounded logical-pixel layout size; f32 precision is sufficient"
        )]
        let style = Style {
            size: Size {
                width: if width < 0.0 {
                    Dimension::auto()
                } else {
                    Dimension::length(width as f32)
                },
                height: Dimension::length(height as f32),
            },
            // Fixed target: never the item that gives in an overflowing row.
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    /// A text or count badge is as wide as its measured label.
    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        let (Some(label), Some(metrics)) = (label_of(self.drawn_content()), self.metrics.as_ref())
        else {
            return false;
        };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "bounded logical-pixel width; f32 precision is sufficient"
        )]
        let width = Dimension::length(metrics.width(&label, text) as f32);
        let Ok(style) = taffy.style(node) else {
            return false;
        };
        if style.size.width == width {
            return false;
        }
        let mut style = style.clone();
        style.size.width = width;
        taffy.set_style(node, style).is_ok()
    }

    fn measure(
        &mut self,
        known: Size<Option<f32>>,
        _available: Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> Size<f32> {
        let (Some(label), Some(metrics)) = (label_of(self.drawn_content()), self.metrics.as_ref())
        else {
            return Size::ZERO;
        };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "bounded logical-pixel size; f32 precision is sufficient"
        )]
        Size {
            width: known.width.unwrap_or(metrics.width(&label, text) as f32),
            height: known.height.unwrap_or(metrics.height as f32),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        // A badge has no name of its own: it goes on from the one that stood
        // where it stands, its box centred on the same line and starting,
        // ending or centred where that one's did (a count's width changes).
        let at = self.rect;
        let left = self.hand_on.place_by(at, |was| {
            let near = |a: f64, b: f64| (a - b).abs() < 0.5;
            near(was.center().y, at.center().y)
                && (near(was.x0, at.x0)
                    || near(was.x1, at.x1)
                    || near(was.center().x, at.center().x))
        });
        if let Some(kept) = left {
            self.take_up(kept);
        }
        // Exact equality is the point — this is change detection for the
        // paint cache, not a numerical comparison that needs a tolerance.
        #[allow(
            clippy::float_cmp,
            reason = "exact-change detection for the paint cache, not numerical comparison"
        )]
        let size_changed = self.rect.width() != self.cache_rect.width()
            || self.rect.height() != self.cache_rect.height();
        if size_changed {
            self.dirty = true;
        }
    }

    fn draw(
        &mut self,
        out_scene: &mut Scene,
        out_transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        let now = crate::motion::frame_now();
        self.motion = Some(theme.motion());
        if self.leaving.is_some() && !self.presence.is_visible(now) {
            self.leaving = None;
        }
        self.drawing = true;
        if !shows(self.drawn_content()) {
            return;
        }
        if self.is_moving(now) {
            // On its way: drawn fresh each frame, and cached once it rests.
            self.paint_moving(out_scene, out_transform, text_renderer, theme, now);
            self.moved_drawn = true;
            return;
        }
        let key = theme_key(self.style, theme);
        let needs_emit = self.dirty
            || std::mem::take(&mut self.moved_drawn)
            || self.cache_rect == Rect::ZERO
            || self.cache_theme != Some(key);
        if !needs_emit {
            let dx = self.rect.x0 - self.cache_rect.x0;
            let dy = self.rect.y0 - self.cache_rect.y0;
            out_scene.append(
                &self.cached,
                Some(out_transform.pre_translate((dx, dy).into())),
            );
            return;
        }
        // mem::take dance — `display_text` borrows &self while `cached` needs &mut self.
        let mut cached = std::mem::take(&mut self.cached);
        cached.reset();
        let transform = Affine::IDENTITY;
        match label_of(self.drawn_content()) {
            None => {
                let modified = matches!(self.drawn_content(), BadgeContent::Modified);
                paint_dot(
                    &mut cached,
                    transform,
                    self.rect,
                    self.style,
                    modified,
                    theme,
                );
            }
            Some(label) => {
                paint_badge_pill(
                    &mut cached,
                    transform,
                    self.rect,
                    &label,
                    self.style,
                    text_renderer,
                    theme,
                );
            }
        }
        out_scene.append(&cached, Some(out_transform));
        self.cached = cached;
        self.cache_rect = self.rect;
        self.cache_theme = Some(key);
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.moved_drawn || self.is_moving(crate::motion::frame_now())
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.on_screen = std::mem::take(&mut self.drawing);
    }

    /// Frames only while the count cross-fades or the badge comes or goes.
    fn needs_animation_frame(&self) -> bool {
        self.is_moving(crate::motion::frame_now())
    }

    fn is_layout_dirty(&self) -> bool {
        self.cache_rect == Rect::ZERO
            || self.layout_dirty
            // The room a leaving badge kept is given back once it has gone.
            || self.laid_out_leaving && !self.presence.is_visible(crate::motion::frame_now())
    }

    fn name(&self) -> &'static str {
        "Badge"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        // The dot variant and zero-count case have no text and so no value;
        // a modified mark says so.
        let value = match self.content {
            BadgeContent::Modified => Some("Modified".to_owned()),
            _ => self.display_text(),
        };
        Some(AccessNode::new(Role::StaticText).with_optional_value(value))
    }
}

/// A badge a rebuild drops leaves itself for the one put in its place.
impl Drop for Badge {
    fn drop(&mut self) {
        let kept = KeptBadge {
            content: std::mem::replace(&mut self.content, BadgeContent::Count(0)),
            label: std::mem::replace(&mut self.label, crate::motion::CrossFade::new(None)),
            leaving: self.leaving.take(),
            presence: self.presence,
            on_screen: self.on_screen,
        };
        self.hand_on.leave(kept);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::PresenceCtx;
    use crate::widget::Tone;
    use vello_encoding::DrawTag;

    /// COLOR is Vello's tag for a solid-colour fill — circles, rounded
    /// rects, and glyph runs all emit it, so a delta of zero proves nothing
    /// was painted at all.
    fn color_fill_count(badge: &mut Badge) -> usize {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        badge.rect = Rect::new(0.0, 0.0, 24.0, 16.0);
        badge.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count()
    }

    #[test]
    fn badge_paints_count_dot_and_text_variants() {
        let mut zero = Badge::count(0);
        assert_eq!(
            color_fill_count(&mut zero),
            0,
            "a zero count paints nothing"
        );

        assert_eq!(Badge::count(100).display_text().as_deref(), Some("99+"));
        assert_eq!(Badge::count(9_999).display_text().as_deref(), Some("99+"));
        assert_eq!(Badge::count(99).display_text().as_deref(), Some("99"));

        let mut dot = Badge::dot();
        assert_eq!(color_fill_count(&mut dot), 1, "a dot is one filled circle");

        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        BadgeFace::new(Rect::new(0.0, 0.0, 72.0, 20.0), "Owner")
            .style(ChipStyle::Tinted(Tone::Info))
            .paint(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        assert!(scene.encoding().draw_tags.contains(&DrawTag::COLOR));
    }

    #[test]
    fn a_text_badge_is_as_wide_as_its_label_at_every_text_scale() {
        for scale in [1.0, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let harness = crate::headless::FrameHarness::new_with_theme(
                crate::State::new(),
                Box::new(crate::widget::Row::new().push(Box::new(
                    Badge::text("Maintainer").style(ChipStyle::Tinted(Tone::Success)),
                ))),
                (600, 200),
                theme.clone(),
            );
            let rect = harness.inspection().root.children[0].rect;
            let mut text = TextRenderer::new();
            text.set_text_scale(scale);
            let (label, line) =
                text.measure_style("Maintainer", None, &theme.text(TextRole::Caption));
            assert!(rect[2] > f64::from(label), "{scale}x: {rect:?}");
            assert!(rect[3] >= f64::from(line), "{scale}x: {rect:?}");
        }
    }

    #[test]
    fn accessibility_reports_static_text_role_and_value() {
        use crate::a11y::Role;
        let b = Badge::count(3);
        let node = Widget::accessibility(&b).expect("badge is not presentational");
        assert_eq!(node.role, Role::StaticText);
        assert_eq!(node.value.as_deref(), Some("3"));
        assert!(!node.state.focusable, "a badge is not focusable");
        assert_eq!(
            Widget::accessibility(&Badge::count(250))
                .unwrap()
                .value
                .as_deref(),
            Some("99+")
        );
        assert_eq!(Widget::accessibility(&Badge::dot()).unwrap().value, None);
    }

    #[test]
    fn a_cached_badge_repaints_when_the_theme_changes() {
        let mut badge = Badge::count(3);
        let mut taffy = TaffyTree::<()>::new();
        let mut map = std::collections::HashMap::new();
        let dark = Theme::dark();
        let light = Theme::light();
        let node = badge.build_layout(&mut taffy, &dark, &mut map);
        let mut text = TextRenderer::new();
        taffy
            .compute_layout_with_measure(
                node,
                Size {
                    width: taffy::AvailableSpace::MaxContent,
                    height: taffy::AvailableSpace::MaxContent,
                },
                |known, available, _, _, _| badge_measure(known, available),
            )
            .unwrap();
        badge.apply_layout(&taffy, node, Point::ZERO);
        let presence = crate::presence::PresenceCtx::default();
        let mut first = Scene::new();
        badge.draw(&mut first, Affine::IDENTITY, &mut text, &dark, &presence);
        badge.mark_drawn();
        let mut second = Scene::new();
        badge.draw(&mut second, Affine::IDENTITY, &mut text, &light, &presence);
        assert_ne!(
            first.encoding().draw_data,
            second.encoding().draw_data,
            "a badge drawn dark is repainted for light"
        );
    }

    fn badge_measure(
        known: Size<Option<f32>>,
        _available: Size<taffy::AvailableSpace>,
    ) -> Size<f32> {
        Size {
            width: known.width.unwrap_or(20.0),
            height: known.height.unwrap_or(16.0),
        }
    }

    #[test]
    fn dots_and_the_modified_mark_paint_in_their_ink() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let rect = Rect::new(0.0, 0.0, 20.0, 20.0);
        let mut live = Scene::new();
        BadgeFace::dot(rect).paint(&mut live, Affine::IDENTITY, &mut text, &theme);
        let mut modified = Scene::new();
        BadgeFace::modified(rect).paint(&mut modified, Affine::IDENTITY, &mut text, &theme);
        assert!(!live.encoding().draw_tags.is_empty());
        assert_ne!(live.encoding().draw_data, modified.encoding().draw_data);
        assert_eq!(
            Widget::accessibility(&Badge::modified())
                .unwrap()
                .value
                .as_deref(),
            Some("Modified")
        );
        // A tall rect does not pad the pill more than its own metrics.
        let mut short = Scene::new();
        let mut tall = Scene::new();
        BadgeFace::new(Rect::new(0.0, 0.0, 60.0, BadgeFace::height(&theme)), "Beta").paint(
            &mut short,
            Affine::IDENTITY,
            &mut text,
            &theme,
        );
        BadgeFace::new(
            Rect::new(0.0, -20.0, 60.0, BadgeFace::height(&theme) + 20.0),
            "Beta",
        )
        .paint(&mut tall, Affine::IDENTITY, &mut text, &theme);
        assert_eq!(
            short.encoding().resources.glyph_runs.len(),
            tall.encoding().resources.glyph_runs.len()
        );
    }

    fn present(badge: &mut Badge, theme: &Theme) {
        badge.rect = Rect::new(0.0, 0.0, 24.0, 16.0);
        badge.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            theme,
            &PresenceCtx::default(),
        );
        badge.mark_drawn();
    }

    #[test]
    fn a_count_on_screen_cross_fades_and_lands_off_screen_or_after_a_key() {
        use crate::time::testing::with_monotonic_time;
        let ms = |t0: crate::time::Instant, ms: u64| t0 + crate::time::Duration::from_millis(ms);
        let theme = Theme::dark();
        // Never drawn: nobody sees it change.
        let mut unseen = Badge::count(3);
        unseen.set_count(4);
        assert!(!unseen.needs_animation_frame());
        assert_eq!(unseen.display_text().as_deref(), Some("4"));

        let mut badge = Badge::count(3);
        let t0 = crate::time::now();
        // Every read stands at a frozen instant: the fade is timed, and a
        // loaded host must not let it end between the change and the check.
        with_monotonic_time(t0, || {
            present(&mut badge, &theme);
            assert!(
                !badge.needs_redraw() && !badge.needs_animation_frame(),
                "at rest"
            );
            badge.set_count(4);
            assert_eq!(
                badge.display_text().as_deref(),
                Some("4"),
                "the value is new at once"
            );
            assert!(badge.needs_animation_frame(), "the numbers cross-fade");
        });
        with_monotonic_time(ms(t0, 12), || {
            let (old, opacity) = badge
                .label
                .outgoing(ms(t0, 12))
                .expect("the old number fades");
            assert_eq!(old.as_deref(), Some("3"));
            assert!(opacity > 0.0 && opacity < 1.0);
            assert!(badge.label.incoming(ms(t0, 12)) < 1.0);
            assert!(!badge.presence.is_moving(ms(t0, 12)), "it never pulses");
            present(&mut badge, &theme);
            assert!(badge.needs_redraw());
        });
        with_monotonic_time(ms(t0, 1_000), || {
            assert!(!badge.needs_animation_frame(), "settled: no frames");
            assert!(badge.needs_redraw(), "the resting look is painted once");
            present(&mut badge, &theme);
            assert!(!badge.needs_redraw(), "and then nothing");
            crate::motion::note_source(crate::tokens::MotionSource::Keyboard);
            badge.set_count(5);
            assert!(!badge.needs_animation_frame(), "after a key it lands");
        });
    }

    #[test]
    fn a_badge_appears_and_leaves_from_its_centre() {
        use crate::time::testing::with_monotonic_time;
        let ms = |t0: crate::time::Instant, ms: u64| t0 + crate::time::Duration::from_millis(ms);
        let theme = Theme::dark();
        let mut badge = Badge::count(0);
        let t0 = crate::time::now();
        with_monotonic_time(t0, || {
            present(&mut badge, &theme);
            badge.set_count(2);
        });
        let progress = badge.presence.progress(ms(t0, 40));
        assert!(progress > 0.0 && progress < 1.0, "appearing: {progress}");
        let pose = crate::motion::Appear::pose(progress, badge.rect.center(), theme.motion());
        assert!(
            pose.to_drawn(badge.rect.center())
                .distance(badge.rect.center())
                < 1e-9,
            "it grows from its centre"
        );
        with_monotonic_time(ms(t0, 1_000), || {
            present(&mut badge, &theme);
            badge.set_count(0);
        });
        assert!(badge.presence.is_moving(ms(t0, 1_040)), "it leaves");
        assert!(
            matches!(badge.drawn_content(), BadgeContent::Count(2)),
            "drawn, and laid out, as it was while it goes"
        );
        with_monotonic_time(ms(t0, 2_000), || present(&mut badge, &theme));
        assert!(badge.leaving.is_none(), "gone");
        assert!(badge.is_layout_dirty(), "its room is given back");
        // Reduced motion: only the fade.
        let calm = crate::motion::Appear::pose(
            0.5,
            badge.rect.center(),
            Theme::dark().with_reduce_motion(true).motion(),
        );
        assert_eq!(calm.transform, Affine::IDENTITY);
    }
}
