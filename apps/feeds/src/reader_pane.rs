//! The content column: the open article, set as a document.
//!
//! The article is content the reader draws: its paper (the system ground or
//! a fixed light, sepia or dark page), its title, byline, tags and blocks,
//! all in the reading face and scale the person chose. Every size is a text
//! role scaled by that reading scale, every colour comes from the paper's
//! palette or the theme, and every space from the spacing tokens.

use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use ui::a11y::{AccessNode, AccessState, Role};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::surface::{ContentEdge, content_edge_fade};
use ui::text_render::{FontChoice, TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::time::Instant;
use ui::tokens::TextRole;
use ui::widget::chip::ChipFace;
use ui::widget::{ChipLead, EmptyState, EmptyStateContent, ScrollRegion, TaffyTreeExt, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::app::{Core, Shared};
use crate::model::{BlockKind, ContentBlock, Filter, Id, ReadFont, TagKey};
use crate::reader_theme::ReaderPalette;

/// A leaf that fills its column, registered for the host's sweeps.
pub(crate) fn fill_leaf<W: Widget + 'static>(
    taffy: &mut TaffyTree<()>,
    node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    widget: &mut W,
) -> NodeId {
    let node = taffy.leaf_node(Style {
        size: taffy::Size {
            width: taffy::Dimension::percent(1.0),
            height: taffy::Dimension::percent(1.0),
        },
        ..Default::default()
    });
    node_map.insert(node, std::ptr::from_mut::<dyn Widget>(widget));
    node
}

/// What the reader painted last frame, for hit-testing and scrolling.
pub(crate) struct ReaderState {
    pub(crate) rect: Rect,
    /// Scrolls the painted article: offset, wheel, keyboard and the overlay
    /// scroll bar that fades when idle.
    pub(crate) scroller: ScrollRegion,
    /// The page's theme at the last paint, for the scroll bar's fade.
    pub(crate) theme: Theme,
    /// Links, one rect per line they cover, with their target.
    pub(crate) links: Vec<(Rect, usize, String)>,
    /// Tag chips and the tag each shows.
    pub(crate) tags: Vec<(Rect, TagKey)>,
    /// The article the scroll offset belongs to.
    shown: Option<Id>,
}

impl Default for ReaderState {
    fn default() -> Self {
        Self {
            rect: Rect::ZERO,
            scroller: ScrollRegion::vertical(),
            theme: Theme::dark(),
            links: Vec::new(),
            tags: Vec::new(),
            shown: None,
        }
    }
}

impl ReaderState {
    /// Back to the top of the article.
    pub(crate) fn scroll_to_top(&mut self) {
        self.scroller.set_offset(vello::kurbo::Vec2::ZERO);
    }
}

/// The article's styles: each a text role in the chosen face, grown by the
/// reading scale.
struct Styles {
    title: TextStyle,
    h1: TextStyle,
    h2: TextStyle,
    body: TextStyle,
    quote: TextStyle,
    code: TextStyle,
    byline: TextStyle,
}

impl Styles {
    fn new(theme: &Theme, font: ReadFont, scale: f32) -> Self {
        let face = match font {
            ReadFont::Sans => FontChoice::SansSerif,
            ReadFont::Serif => FontChoice::Serif,
            ReadFont::Mono => FontChoice::Monospace,
        };
        let role = |role: TextRole| {
            let mut style = theme.text(role).with_family(face.clone());
            style.size *= scale;
            style
        };
        let mut code = theme.text(TextRole::Code);
        code.size *= scale;
        Self {
            title: role(TextRole::LargeTitle).with_weight(TextWeight::Semibold),
            h1: role(TextRole::Title).with_weight(TextWeight::Semibold),
            h2: role(TextRole::Heading).with_weight(TextWeight::Semibold),
            body: role(TextRole::Reading),
            quote: role(TextRole::Reading).with_italic(true),
            code,
            byline: role(TextRole::Body),
        }
    }
}

/// The date an article was published, spelled out ("June 20, 2026").
fn published_date(published: i64) -> Option<String> {
    (published > 0)
        .then(|| chrono::DateTime::from_timestamp(published, 0))
        .flatten()
        .map(|dt| dt.format("%B %-d, %Y").to_string())
}

impl Core {
    #[allow(
        clippy::too_many_lines,
        reason = "one pass over the document: title, byline, tags, then each block in order"
    )]
    pub(crate) fn paint_reader(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        self.reader.links.clear();
        self.reader.tags.clear();
        self.reader.rect = area;
        if area.width() <= 0.0 || area.height() <= 0.0 {
            return;
        }
        self.reader.scroller.set_viewport(area, theme);
        if self.reader.shown != self.selected {
            self.reader.shown.clone_from(&self.selected);
            self.reader.scroll_to_top();
        }
        let palette = ReaderPalette::resolve(self.settings.theme, theme);
        if let Some(page) = palette.page {
            scene.fill(Fill::NonZero, t, page, None, &area);
        }
        let page_theme = palette.page_theme(theme);
        self.reader.theme = page_theme.clone();
        let Some(article) = self.open_article().map(|article| article.item().clone()) else {
            self.reader.scroller.set_content_size(area.width(), 0.0);
            // An empty library has one empty state, in the list.
            if !self.store.feeds.is_empty() {
                let content = EmptyStateContent {
                    icon: Some(Icon::FileText),
                    ..EmptyStateContent::new("No article selected")
                };
                EmptyState::paint(
                    &mut PaintCtx::new(scene, t, text, &page_theme),
                    area,
                    &content,
                    Point::new(f64::NAN, f64::NAN),
                );
            }
            return;
        };

        let spacing = &theme.tokens.spacing;
        let scale = f64::from(self.settings.font_scale);
        let styles = Styles::new(theme, self.settings.font, self.settings.font_scale);
        let gutter = f64::from(spacing.s8);
        // A readable measure: about seventy characters of the body face.
        let measure = f64::from(styles.body.size) * 34.0;
        let width = (area.width() - gutter * 2.0).min(measure).max(0.0);
        let x = area.x0 + (area.width() - width) / 2.0;
        let w = width as f32;
        let gap = f64::from(spacing.s4) * scale;
        let scroll = self.reader.scroller.offset().y;

        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &area);
        let top = area.y0 + gutter;
        let mut y = top - scroll;

        // Title.
        text.draw_style(
            scene,
            &article.title,
            palette.ink,
            t * Affine::translate((x, y)),
            Some(w),
            &styles.title,
        );
        y += f64::from(text.measure_style(&article.title, Some(w), &styles.title).1);
        y += f64::from(spacing.s3) * scale;

        // Byline: who wrote it, then when.
        for line in [
            Some(article.author.trim().to_string()).filter(|a| !a.is_empty()),
            published_date(article.published),
        ]
        .into_iter()
        .flatten()
        {
            text.draw_style(
                scene,
                &line,
                palette.ink_dim,
                t * Affine::translate((x, y)),
                Some(w),
                &styles.byline,
            );
            y += f64::from(text.measure_style(&line, Some(w), &styles.byline).1);
        }

        // Tags: chips that show the tag's articles.
        if !article.tags.is_empty() {
            y += f64::from(spacing.s3);
            let chip_h = ChipFace::height(&page_theme);
            let chip_gap = f64::from(spacing.s2);
            let mut cx = x;
            for tag in article.tags.iter().take(8) {
                let chip_w =
                    ChipFace::intrinsic_width(tag, ChipLead::None, false, text, &page_theme);
                if cx > x && cx + chip_w > x + width {
                    cx = x;
                    y += chip_h + chip_gap;
                }
                let rect = Rect::new(cx, y, cx + chip_w, y + chip_h);
                ChipFace::new(rect, tag)
                    .hovered(rect.contains(self.pointer))
                    .paint(scene, t, text, &page_theme);
                self.reader
                    .tags
                    .push((rect.intersect(area), crate::model::tag_key(tag)));
                cx += chip_w + chip_gap;
            }
            y += chip_h;
        }

        // The rule between the header and the body.
        y += f64::from(spacing.s6) * scale;
        ui::surface::separator(scene, t, x, x + width, y, 0.0, palette.ground);
        y += f64::from(spacing.s6) * scale;

        for (index, block) in article.blocks.0.iter().enumerate() {
            y = self.paint_block(
                PaintCtx::new(scene, t, text, theme),
                &BlockFrame {
                    theme,
                    palette: &palette,
                    styles: &styles,
                    x,
                    width,
                    gap,
                    area,
                },
                index,
                block,
                y,
            );
        }
        y += gutter;
        scene.pop_layer();
        let content = y + scroll - area.y0;
        self.reader.scroller.set_content_size(area.width(), content);
        // The page fades into its paper where more can scroll in.
        if scroll > 0.0 {
            content_edge_fade(scene, t, area, ContentEdge::Top, palette.ground);
        }
        if scroll + area.height() < content {
            content_edge_fade(scene, t, area, ContentEdge::Bottom, palette.ground);
        }
        self.reader.scroller.paint_bars(scene, t, &page_theme);
    }

    /// Paint one block at `y`; returns where the next one starts.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per block kind, each a few lines of the same shape"
    )]
    fn paint_block(
        &mut self,
        ctx: PaintCtx<'_>,
        frame: &BlockFrame<'_>,
        index: usize,
        block: &ContentBlock,
        y: f64,
    ) -> f64 {
        let PaintCtx {
            scene,
            transform: t,
            text,
            ..
        } = ctx;
        let BlockFrame {
            theme,
            palette,
            styles,
            x,
            width,
            gap,
            area,
        } = *frame;
        let spacing = &theme.tokens.spacing;
        let w = width as f32;
        let height = |text: &mut TextRenderer, s: &str, style: &TextStyle, w: f32| {
            f64::from(text.measure_style(s, Some(w), style).1)
        };
        match block.kind {
            BlockKind::Heading1 | BlockKind::Heading2 | BlockKind::Heading3 => {
                let style = if block.kind == BlockKind::Heading1 {
                    &styles.h1
                } else {
                    &styles.h2
                };
                let y = y + gap / 2.0;
                text.draw_style(
                    scene,
                    &block.text,
                    palette.ink,
                    t * Affine::translate((x, y)),
                    Some(w),
                    style,
                );
                y + height(text, &block.text, style, w) + gap / 2.0
            }
            BlockKind::Paragraph => {
                text.draw_style(
                    scene,
                    &block.text,
                    palette.ink,
                    t * Affine::translate((x, y)),
                    Some(w),
                    &styles.body,
                );
                y + height(text, &block.text, &styles.body, w) + gap
            }
            BlockKind::Bullet => {
                let indent = f64::from(spacing.s6);
                let line = height(text, "Ag", &styles.body, w);
                let dot = f64::from(spacing.s1) / 2.0;
                scene.fill(
                    Fill::NonZero,
                    t,
                    palette.ink_dim,
                    None,
                    &Circle::new(Point::new(x + indent / 2.0, y + line / 2.0), dot),
                );
                let tw = (width - indent).max(0.0) as f32;
                text.draw_style(
                    scene,
                    &block.text,
                    palette.ink,
                    t * Affine::translate((x + indent, y)),
                    Some(tw),
                    &styles.body,
                );
                y + height(text, &block.text, &styles.body, tw) + gap / 2.0
            }
            BlockKind::Quote => {
                let indent = f64::from(spacing.s4);
                let tw = (width - indent).max(0.0) as f32;
                let h = height(text, &block.text, &styles.quote, tw);
                let bar = Rect::new(x, y, x + f64::from(spacing.s1) / 2.0, y + h);
                scene.fill(Fill::NonZero, t, palette.rule, None, &bar);
                text.draw_style(
                    scene,
                    &block.text,
                    palette.ink_dim,
                    t * Affine::translate((x + indent, y)),
                    Some(tw),
                    &styles.quote,
                );
                y + h + gap
            }
            BlockKind::Code => {
                let pad = f64::from(spacing.s3);
                let tw = (width - pad * 2.0).max(0.0) as f32;
                let h = height(text, &block.text, &styles.code, tw);
                let well = Rect::new(x, y, x + width, y + h + pad * 2.0);
                scene.fill(
                    Fill::NonZero,
                    t,
                    palette.well,
                    None,
                    &RoundedRect::from_rect(well, f64::from(theme.tokens.radii.control)),
                );
                text.draw_style(
                    scene,
                    &block.text,
                    palette.ink,
                    t * Affine::translate((x + pad, y + pad)),
                    Some(tw),
                    &styles.code,
                );
                well.y1 + gap
            }
            BlockKind::Link => {
                // A link is primary ink with an underline, one per line.
                let lines = text.clamp_to_lines(&block.text, &styles.body, w, usize::MAX);
                let line_h = height(text, "Ag", &styles.body, w);
                let mut ly = y;
                for line in &lines {
                    let lw = f64::from(text.measure_style(line, None, &styles.body).0);
                    text.draw_style(
                        scene,
                        line,
                        palette.ink,
                        t * Affine::translate((x, ly)),
                        None,
                        &styles.body,
                    );
                    let base = ly + line_h - f64::from(spacing.s1) / 2.0;
                    let rule = f64::from(theme.tokens.metrics.chrome_rule_px);
                    scene.fill(
                        Fill::NonZero,
                        t,
                        palette.ink,
                        None,
                        &Rect::new(x, base, x + lw, base + rule),
                    );
                    if let Some(href) = &block.href {
                        let rect = Rect::new(x, ly, x + lw, ly + line_h);
                        if rect.y1 > area.y0 && rect.y0 < area.y1 {
                            self.reader.links.push((rect, index, href.clone()));
                        }
                    }
                    ly += line_h;
                }
                ly + gap
            }
            BlockKind::Divider => {
                let y = y + gap / 2.0;
                ui::surface::separator(scene, t, x, x + width, y, 0.0, palette.ground);
                y + gap
            }
        }
    }

    /// Route `event` to the reader. Returns whether the reader used it.
    pub(crate) fn reader_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        // The scroll view takes the wheel, touchpad gestures and its bar.
        let theme = self.reader.theme.clone();
        if self.reader.scroller.handle_event(event, ctx, &theme) {
            *ctx.handled = true;
            return true;
        }
        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        else {
            return false;
        };
        if !self.reader.rect.contains(self.pointer) {
            return false;
        }
        if let Some(key) = self
            .reader
            .tags
            .iter()
            .find(|(rect, _)| rect.contains(self.pointer))
            .map(|(_, key)| *key)
        {
            self.set_filter(Filter::Tag(key));
            return true;
        }
        if let Some(href) = self
            .reader
            .links
            .iter()
            .find(|(rect, _, _)| rect.contains(self.pointer))
            .map(|(_, _, href)| href.clone())
        {
            self.open_url(&href);
            return true;
        }
        false
    }

    pub(crate) fn reader_accessibility(&self) -> AccessNode {
        let Some(article) = self.open_article() else {
            return AccessNode::new(Role::Document)
                .with_name("No article selected")
                .with_bounds(self.reader.rect);
        };
        let mut children =
            vec![AccessNode::new(Role::Heading { level: 1 }).with_name(article.title.clone())];
        if !article.author.is_empty() {
            children.push(AccessNode::new(Role::StaticText).with_name(article.author.clone()));
        }
        if let Some(date) = published_date(article.published) {
            children.push(AccessNode::new(Role::StaticText).with_name(date));
        }
        children.extend(article.tags.iter().filter_map(|tag| {
            let key = crate::model::tag_key(tag);
            let rect = self
                .reader
                .tags
                .iter()
                .find_map(|(rect, found)| (*found == key).then_some(*rect))?;
            Some(
                AccessNode::new(Role::Link)
                    .with_name(format!("Tag {tag}"))
                    .with_description("Show articles with this tag")
                    .with_state(AccessState::new().focusable(true))
                    .with_bounds(rect)
                    .with_action_target(format!("feeds.reader-tag:{key}")),
            )
        }));
        children.extend(article.blocks.0.iter().enumerate().map(|(index, block)| {
            let role = match block.kind {
                BlockKind::Heading1 => Role::Heading { level: 2 },
                BlockKind::Heading2 => Role::Heading { level: 3 },
                BlockKind::Heading3 => Role::Heading { level: 4 },
                BlockKind::Link => Role::Link,
                _ => Role::Paragraph,
            };
            let mut node = AccessNode::new(role)
                .with_name(block.text.clone())
                .with_hashed_key((article.id, index));
            if role == Role::Link && block.href.is_some() {
                node = node.with_action_target(format!("feeds.reader-link:{index}"));
                if let Some((rect, _, _)) = self.reader.links.iter().find(|(_, i, _)| *i == index) {
                    node = node.with_bounds(*rect);
                }
            }
            node
        }));
        AccessNode::new(Role::Document)
            .with_name(article.title.clone())
            .with_state(AccessState::new().read_only(true))
            .with_bounds(self.reader.rect)
            .with_children(children)
    }
}

/// What every block of one article paints against.
#[derive(Clone, Copy)]
struct BlockFrame<'a> {
    theme: &'a Theme,
    palette: &'a ReaderPalette,
    styles: &'a Styles,
    x: f64,
    width: f64,
    gap: f64,
    area: Rect,
}

/// The content column widget: paints the open article from the shared core.
pub(crate) struct ReaderPane {
    core: Shared,
    rect: Rect,
}

impl ReaderPane {
    pub(crate) fn new(core: &Shared) -> Self {
        Self {
            core: std::rc::Rc::clone(core),
            rect: Rect::ZERO,
        }
    }
}

impl Widget for ReaderPane {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        fill_leaf(taffy, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.core
            .borrow_mut()
            .paint_reader(scene, transform, text, theme, self.rect);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.core.borrow_mut().pointer =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if self.core.borrow_mut().reader_event(event, ctx) {
            *ctx.handled = true;
        }
    }

    fn accessibility(&self) -> Option<AccessNode> {
        Some(self.core.borrow().reader_accessibility())
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        let core = self.core.borrow();
        if let Some((rect, _, href)) = core.reader.links.iter().find(|(r, _, _)| r.contains(point))
        {
            return Some((*rect, href.clone()));
        }
        core.reader
            .tags
            .iter()
            .find(|(r, _)| r.contains(point))
            .map(|(rect, _)| (*rect, "Show articles with this tag".to_string()))
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        let core = self.core.borrow();
        let over = core.reader.links.iter().any(|(r, _, _)| r.contains(point))
            || core.reader.tags.iter().any(|(r, _)| r.contains(point));
        over.then_some(winit::window::CursorIcon::Pointer)
    }

    fn needs_animation_frame(&self) -> bool {
        self.core.try_borrow().is_ok_and(|core| {
            core.reader
                .scroller
                .needs_animation_frame(&core.reader.theme)
        })
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.core
            .try_borrow()
            .ok()
            .and_then(|core| core.reader.scroller.next_wake_deadline(&core.reader.theme))
    }

    fn primary_vertical_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        self.core
            .try_borrow()
            .ok()
            .and_then(|core| core.reader.scroller.primary_vertical_scroll())
    }

    fn apply_primary_vertical_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        self.core
            .borrow_mut()
            .reader
            .scroller
            .apply_primary_vertical_scroll(action)
    }

    fn name(&self) -> &'static str {
        "FeedsReader"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
