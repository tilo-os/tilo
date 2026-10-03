//! The overview column: the articles of the chosen place, one
//! [`ListRowFace`] each, under the refresh progress while feeds refresh;
//! or the empty state when there is nothing to list.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use ui::a11y::{AccessNode, AccessState, Role};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    EmptyState, EmptyStateContent, Interaction, ListRowFace, PressOutcome, ProgressBar, RowAction,
    ScrollMetrics, Scrollbar, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

use crate::app::{Core, Shared};
use crate::model::{Article, Filter, Id, relative_time};

/// The empty library's title.
pub(crate) const EMPTY_LIBRARY_TITLE: &str = "No feeds yet";

/// One article row as last painted.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RowHit {
    pub(crate) rect: Rect,
    pub(crate) id: Id,
    /// The star action, while the row shows its actions.
    pub(crate) star: Option<Rect>,
}

/// What the list painted last frame, for hit-testing and scrolling.
pub(crate) struct ListState {
    pub(crate) rect: Rect,
    /// The rows' viewport, below the refresh progress.
    pub(crate) viewport: Rect,
    pub(crate) scroll: f64,
    /// Scroll the open article's row into view on the next paint.
    pub(crate) reveal: bool,
    pub(crate) content: f64,
    /// One row's height: a wheel line scrolls by it.
    line: f64,
    bar: Scrollbar,
    /// How far the thumb sits inside the scroll gutter.
    bar_inset: f64,
    pub(crate) rows: Vec<RowHit>,
    /// The empty state's action.
    pub(crate) empty_action: Option<Rect>,
    progress: ProgressBar,
}

impl Default for ListState {
    fn default() -> Self {
        Self {
            rect: Rect::ZERO,
            viewport: Rect::ZERO,
            scroll: 0.0,
            reveal: true,
            content: 0.0,
            line: 0.0,
            bar: Scrollbar::vertical(),
            bar_inset: 0.0,
            rows: Vec::new(),
            empty_action: None,
            progress: ProgressBar::new(0.0).with_label("Refreshing feeds"),
        }
    }
}

impl ListState {
    fn metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(self.content, self.viewport.height(), self.scroll)
    }

    fn track(&self, gutter: f64) -> Rect {
        Rect::new(
            self.viewport.x1 - gutter + self.bar_inset,
            self.viewport.y0 + self.bar_inset,
            self.viewport.x1 - self.bar_inset,
            self.viewport.y1 - self.bar_inset,
        )
    }

    fn max_scroll(&self) -> f64 {
        (self.content - self.viewport.height()).max(0.0)
    }
}

fn star_label(article: &Article) -> &'static str {
    if article.starred {
        "Remove star"
    } else {
        "Star article"
    }
}

impl Core {
    /// The row's one secondary fact: the feed, unless the list is one feed's,
    /// where it is the article's summary.
    fn row_secondary<'a>(&'a self, article: &'a Article) -> &'a str {
        match self.filter {
            Filter::Feed(_) => article.summary.as_str(),
            _ => self
                .store
                .feed(article.feed_id)
                .map_or("", |f| f.title.as_str()),
        }
    }

    pub(crate) fn paint_list(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        area: Rect,
    ) {
        self.list.rows.clear();
        self.list.empty_action = None;
        let resized = self.list.rect.size() != area.size();
        self.list.rect = area;
        if area.width() <= 0.0 || area.height() <= 0.0 {
            return;
        }
        let spacing = &theme.tokens.spacing;
        let inset = f64::from(spacing.s2);
        let top = if self.refresh.visible() {
            self.paint_refresh(scene, t, text, theme, presence, area)
        } else {
            area.y0
        };
        let viewport = Rect::new(area.x0, top, area.x1, area.y1);
        self.list.viewport = viewport;

        let ids: Vec<Id> = self
            .store
            .articles_for(self.filter)
            .iter()
            .map(|a| a.id)
            .collect();
        if ids.is_empty() {
            self.list.content = 0.0;
            self.paint_list_empty(scene, t, text, theme, viewport);
            return;
        }

        let gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
        // The thumb stands half a step inside its gutter, as a scroll view's does.
        self.list.bar_inset = f64::from(spacing.s1) / 2.0;
        let row_h = ListRowFace::new("Ag")
            .secondary("Ag")
            .meta("Ag")
            .height(text, theme);
        self.list.line = row_h;
        self.list.content = row_h * ids.len() as f64 + inset * 2.0;
        if (std::mem::take(&mut self.list.reveal) || resized)
            && let Some(index) = self
                .selected
                .and_then(|sel| ids.iter().position(|id| *id == sel))
        {
            let row_top = inset + row_h * index as f64;
            let row_bottom = row_top + row_h;
            if row_top - inset < self.list.scroll {
                self.list.scroll = row_top - inset;
            } else if row_bottom + inset > self.list.scroll + viewport.height() {
                self.list.scroll = row_bottom + inset - viewport.height();
            }
        }
        self.list.scroll = self.list.scroll.clamp(0.0, self.list.max_scroll());
        let overflows = self.list.metrics().overflows();
        let x1 = viewport.x1 - inset.max(if overflows { gutter } else { 0.0 });

        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &viewport);
        let mut y = viewport.y0 + inset - self.list.scroll;
        let first = ((self.list.scroll - inset) / row_h).floor().max(0.0) as usize;
        y += row_h * first as f64;
        for (i, id) in ids.iter().enumerate().skip(first) {
            if y > viewport.y1 {
                break;
            }
            let rect = Rect::new(viewport.x0 + inset, y, x1, y + row_h);
            y += row_h;
            self.paint_row(
                &mut PaintCtx::new(scene, t, text, theme),
                rect,
                *id,
                ids.get(i + 1).copied(),
            );
        }
        scene.pop_layer();

        if overflows {
            self.list.bar.draw(
                scene,
                t,
                self.list.track(gutter),
                self.list.metrics(),
                theme,
                false,
            );
        }
    }

    /// One article's row at `rect`; `next` is the article below it.
    fn paint_row(&mut self, ctx: &mut PaintCtx<'_>, rect: Rect, id: Id, next: Option<Id>) {
        let Some(article) = self.store.article(id) else {
            return;
        };
        let viewport = self.list.viewport;
        let time = relative_time(article.published, self.now);
        let hovered = rect.contains(self.pointer)
            && viewport.contains(self.pointer)
            && self.menu.is_none()
            && !self.modal_open;
        let star = [RowAction {
            icon: Icon::Star,
            label: star_label(article),
        }];
        let face = ListRowFace::new(&article.title)
            .secondary(self.row_secondary(article))
            .meta(&time)
            .emphasized(!article.read)
            .selected(self.selected == Some(id))
            .actions(&star)
            .interaction(Interaction {
                hovered,
                ..Interaction::default()
            });
        let star_rect = face.action_rects(rect, ctx.theme).first().copied();
        let face = face.hovered_action(star_rect.filter(|r| r.contains(self.pointer)).map(|_| 0));
        let face = match next {
            Some(next) => face.separator(self.selected == Some(next)),
            None => face,
        };
        face.paint(ctx, rect);
        self.list.rows.push(RowHit {
            rect: rect.intersect(viewport),
            id,
            star: star_rect,
        });
    }

    /// The refresh progress under the header: one sentence over the bar.
    /// Returns where the rows start.
    fn paint_refresh(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        area: Rect,
    ) -> f64 {
        let spacing = &theme.tokens.spacing;
        let caption = theme.text(TextRole::Caption);
        let line = f64::from(text.measure_style("Ag", None, &caption).1);
        let pad_x = f64::from(spacing.s4);
        let band_y = area.y0 + f64::from(spacing.s3);
        text.draw_style(
            scene,
            &self.refresh.status(),
            theme.text_dim,
            t * Affine::translate((area.x0 + pad_x, band_y)),
            Some(((area.width() - pad_x * 2.0).max(0.0)) as f32),
            &caption,
        );
        let bar_y = band_y + line + f64::from(spacing.s2);
        let bar = Rect::new(
            area.x0 + pad_x,
            bar_y,
            area.x1 - pad_x,
            bar_y + f64::from(spacing.s2),
        );
        let fraction = if self.refresh.in_progress() {
            self.refresh.fraction()
        } else {
            1.0
        };
        self.list.progress.set_progress(fraction as f32);
        self.list.progress.apply_layout_external(bar);
        self.list.progress.draw(scene, t, text, theme, presence);
        let top = bar.y1 + f64::from(spacing.s3);
        ui::surface::separator(
            scene,
            t,
            area.x0,
            area.x1,
            top - 0.5,
            0.0,
            theme.tokens.colors.surfaces.content,
        );
        top
    }

    /// The list's empty state: an empty library offers its one action; an
    /// empty place says what would fill it.
    fn paint_list_empty(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let library_empty = self.store.feeds.is_empty();
        let (title, icon) = if library_empty {
            (EMPTY_LIBRARY_TITLE, Icon::Inbox)
        } else {
            match self.filter {
                Filter::Unread => ("No unread articles", Icon::Check),
                Filter::Starred => ("No starred articles", Icon::Star),
                Filter::Tag(_) => ("No articles with this tag", Icon::Inbox),
                _ => ("No articles yet", Icon::FileText),
            }
        };
        let content = EmptyStateContent {
            icon: Some(icon),
            action_label: library_empty.then_some("Add feed"),
            ..EmptyStateContent::new(title)
        };
        self.list.empty_action = EmptyState::paint(
            &mut PaintCtx::new(scene, t, text, theme),
            area,
            &content,
            self.pointer,
        );
    }

    /// Route `event` to the list. Returns whether the list used it.
    pub(crate) fn list_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        gutter: f64,
    ) -> bool {
        let viewport = self.list.viewport;
        let inside = self.list.rect.contains(self.pointer);
        match event {
            WindowEvent::CursorMoved { .. } => {
                let track = self.list.track(gutter);
                let metrics = self.list.metrics();
                if let Some(offset) = self.list.bar.drag_to(track, metrics, self.pointer) {
                    self.list.scroll = offset;
                    return true;
                }
                self.list.bar.update_hover(track, metrics, self.pointer);
                false
            }
            WindowEvent::MouseWheel { delta, .. } if inside => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * self.list.line,
                    MouseScrollDelta::PixelDelta(p) => p.y / ctx.scale_factor,
                };
                self.list.scroll = (self.list.scroll - dy).clamp(0.0, self.list.max_scroll());
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if inside => {
                if self
                    .list
                    .empty_action
                    .is_some_and(|r| r.contains(self.pointer))
                {
                    self.open_add_subscription();
                    return true;
                }
                let metrics = self.list.metrics();
                if metrics.overflows() && viewport.contains(self.pointer) {
                    match self
                        .list
                        .bar
                        .press(self.list.track(gutter), metrics, self.pointer)
                    {
                        PressOutcome::Thumb => return true,
                        PressOutcome::Page(offset) => {
                            self.list.scroll = offset;
                            return true;
                        }
                        PressOutcome::Miss => {}
                    }
                }
                let hit = self
                    .list
                    .rows
                    .iter()
                    .find(|row| row.rect.contains(self.pointer))
                    .copied();
                if let Some(row) = hit {
                    if row.star.is_some_and(|r| r.contains(self.pointer)) {
                        self.toggle_star(row.id, ctx.state);
                    } else {
                        self.select_article(row.id, ctx.state);
                    }
                }
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.list.bar.release(),
            _ => false,
        }
    }

    pub(crate) fn list_accessibility(&self) -> AccessNode {
        let mut children: Vec<AccessNode> = self
            .list
            .rows
            .iter()
            .filter_map(|row| {
                let article = self.store.article(row.id)?;
                let time = relative_time(article.published, self.now);
                let face = ListRowFace::new(&article.title)
                    .secondary(self.row_secondary(article))
                    .meta(&time)
                    .selected(self.selected == Some(row.id));
                let node = face
                    .accessibility(row.rect, format!("feeds.article:{}", row.id))
                    .with_children(vec![
                        AccessNode::new(Role::ToggleButton)
                            .with_name(format!("Star {}", article.title))
                            .with_state(AccessState::new().focusable(true).checked(article.starred))
                            .with_bounds(row.star.unwrap_or(row.rect))
                            .with_action_target(format!("feeds.star:{}", row.id)),
                    ]);
                Some(node)
            })
            .collect();
        if let Some(rect) = self.list.empty_action {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name("Add feed")
                    .with_state(AccessState::new().focusable(true))
                    .with_bounds(rect)
                    .with_action_target("feeds.empty.add"),
            );
        }
        AccessNode::new(Role::List)
            .with_name(self.filter_title())
            .with_bounds(self.list.rect)
            .with_children(children)
    }

    fn list_tooltip(&self, point: Point) -> Option<(Rect, String)> {
        let row = self.list.rows.iter().find(|row| row.rect.contains(point))?;
        let article = self.store.article(row.id)?;
        if let Some(star) = row.star.filter(|r| r.contains(point)) {
            return Some((star, star_label(article).to_string()));
        }
        None
    }
}

/// The overview column widget: paints the list from the shared core.
pub(crate) struct ListPane {
    core: Shared,
    rect: Rect,
    gutter: f64,
}

impl ListPane {
    pub(crate) fn new(core: &Shared) -> Self {
        Self {
            core: std::rc::Rc::clone(core),
            rect: Rect::ZERO,
            gutter: 0.0,
        }
    }
}

impl Widget for ListPane {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
        crate::reader_pane::fill_leaf(taffy, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::reader_pane::leaf_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.core
            .borrow_mut()
            .paint_list(scene, transform, text, theme, presence, self.rect);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.core.borrow_mut().pointer =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if self.core.borrow_mut().list_event(event, ctx, self.gutter) {
            *ctx.handled = true;
        }
    }

    fn accessibility(&self) -> Option<AccessNode> {
        Some(self.core.borrow().list_accessibility())
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        self.core.borrow().list_tooltip(point)
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        let core = self.core.borrow();
        let over_star = core
            .list
            .rows
            .iter()
            .any(|row| row.star.is_some_and(|r| r.contains(point)));
        let over_action = core.list.empty_action.is_some_and(|r| r.contains(point));
        (over_star || over_action).then_some(winit::window::CursorIcon::Pointer)
    }

    fn wants_pointer_capture(&self) -> bool {
        self.core.borrow().list.bar.is_dragging()
    }

    fn name(&self) -> &'static str {
        "FeedsArticleList"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
