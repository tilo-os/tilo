//! The overview column: the articles of the chosen place, one
//! [`ListRowFace`] each, under the refresh progress while feeds refresh;
//! or the empty state when there is nothing to list.
//!
//! The rows are painted, and a [`ScrollRegion`] scrolls them: it owns the
//! offset, the wheel, keyboard and touchpad gestures, the edge fades and the
//! overlay scroll bar that fades when idle. The empty state is the retained [`EmptyState`],
//! so its one action takes part in focus and activation like any button.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use ui::a11y::{AccessNode, AccessState, Role};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::Instant;
use ui::tokens::TextRole;
use ui::widget::{
    AppModel, EmptyState, Interaction, ListRowFace, ModelPath, ProgressBar, RowAction,
    ScrollRegion, TaffyTreeExt, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

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
    /// Scrolls the painted rows.
    pub(crate) scroller: ScrollRegion,
    /// The theme of the last paint, for the scroll bar's fade.
    pub(crate) theme: Theme,
    /// Scroll the open article's row into view on the next paint.
    pub(crate) reveal: bool,
    /// The rows' full height, with their insets.
    pub(crate) content: f64,
    pub(crate) rows: Vec<RowHit>,
    progress: ProgressBar,
}

impl Default for ListState {
    fn default() -> Self {
        Self {
            rect: Rect::ZERO,
            viewport: Rect::ZERO,
            scroller: ScrollRegion::vertical().with_edge_fade(),
            theme: Theme::dark(),
            reveal: true,
            content: 0.0,
            rows: Vec::new(),
            progress: ProgressBar::new(0.0).with_label("Refreshing feeds"),
        }
    }
}

impl ListState {
    /// How far the rows are scrolled.
    pub(crate) fn scroll(&self) -> f64 {
        self.scroller.offset().y
    }

    /// Back to the top of the list.
    pub(crate) fn scroll_to_top(&mut self) {
        self.scroller.set_offset(Vec2::ZERO);
    }
}

/// What the list says when it has no rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EmptyList {
    title: &'static str,
    icon: Icon,
    /// The empty library offers its one action.
    add_feed: bool,
}

impl EmptyList {
    fn widget(self) -> EmptyState {
        let state = EmptyState::new(self.title).with_icon(self.icon);
        if self.add_feed {
            state
                .with_action("Add feed")
                .with_action_id("feeds.empty.add")
        } else {
            state
        }
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

    /// What the list says while the chosen place holds no articles: an
    /// empty library offers its one action, an empty place says what would
    /// fill it.
    pub(crate) fn empty_list(&self) -> Option<EmptyList> {
        if !self.store.articles_for(self.filter).is_empty() {
            return None;
        }
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
        Some(EmptyList {
            title,
            icon,
            add_feed: library_empty,
        })
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
        let resized = self.list.rect.size() != area.size();
        self.list.rect = area;
        if area.width() <= 0.0 || area.height() <= 0.0 {
            return;
        }
        let inset = f64::from(theme.tokens.spacing.s2);
        let top = if self.refresh.visible() {
            self.paint_refresh(scene, t, text, theme, presence, area)
        } else {
            area.y0
        };
        let viewport = Rect::new(area.x0, top, area.x1, area.y1);
        self.list.viewport = viewport;
        self.list.scroller.set_viewport(viewport, theme);
        self.list.theme = theme.clone();

        let ids: Vec<Id> = self
            .store
            .articles_for(self.filter)
            .iter()
            .map(|a| a.id)
            .collect();
        let row_h = ListRowFace::new("Ag")
            .two_line_title()
            .secondary("Ag")
            .meta("Ag")
            .height(text, theme);
        self.list.content = if ids.is_empty() {
            0.0
        } else {
            row_h * ids.len() as f64 + inset * 2.0
        };
        self.list
            .scroller
            .set_content_size(viewport.width(), self.list.content);
        if ids.is_empty() {
            return;
        }
        if (std::mem::take(&mut self.list.reveal) || resized)
            && let Some(index) = self
                .selected
                .and_then(|sel| ids.iter().position(|id| *id == sel))
        {
            // The row with the list's inset above and below it.
            let row_top = row_h * index as f64;
            self.list.scroller.scroll_into_view(Rect::new(
                0.0,
                row_top,
                viewport.width(),
                row_top + row_h + inset * 2.0,
            ));
        }
        let scroll = self.list.scroll();
        // Row text starts on the column's content edge, the column title's,
        // and the lift bleeds past it; while the list overflows, the region
        // keeps the trailing lane free for the scroll bar.
        let edge = ui::chrome::column_gutter(theme) - ui::chrome::row_bleed(theme);
        let x0 = viewport.x0 + edge;
        let x1 = if self.list.scroller.overflows_vertically() {
            self.list.scroller.content_rect().x1
        } else {
            viewport.x1 - edge
        };

        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &viewport);
        let first = ((scroll - inset) / row_h).floor().max(0.0) as usize;
        let mut y = viewport.y0 + inset - scroll + row_h * first as f64;
        for (i, id) in ids.iter().enumerate().skip(first) {
            if y > viewport.y1 {
                break;
            }
            let rect = Rect::new(x0, y, x1, y + row_h);
            y += row_h;
            self.paint_row(
                &mut PaintCtx::new(scene, t, text, theme),
                rect,
                *id,
                ids.get(i + 1).copied(),
            );
        }
        scene.pop_layer();
        // Rows fade into the column where more can scroll in, under the bar.
        self.list.scroller.paint(scene, t, theme, |_, _| {});
    }

    /// One article's row at `rect`; `next` is the article below it.
    fn paint_row(&mut self, ctx: &mut PaintCtx<'_>, rect: Rect, id: Id, next: Option<Id>) {
        let Some(article) = self.store.article(id) else {
            return;
        };
        let viewport = self.list.viewport;
        let time = relative_time(article.published, self.now);
        let hovered = (rect.y0..rect.y1).contains(&self.pointer.y)
            && viewport.contains(self.pointer)
            && self.menu.is_none()
            && !self.modal_open;
        let star = [RowAction {
            icon: Icon::Star,
            label: star_label(article),
        }];
        let face = ListRowFace::new(&article.title)
            .two_line_title()
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
        let face = if article.starred {
            face.mark(Icon::Star)
        } else {
            face
        };
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
        // The column title's edge.
        let pad_x = ui::chrome::column_gutter(theme);
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

    /// Route `event` to the list. Returns whether the list used it.
    pub(crate) fn list_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        // The scroll view takes the wheel, touchpad gestures and its bar.
        let theme = self.list.theme.clone();
        if self.list.scroller.handle_event(event, ctx, &theme) {
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
        if !self.list.viewport.contains(self.pointer) {
            return false;
        }
        let hit = self
            .list
            .rows
            .iter()
            // A row answers across the list's whole width, its side insets
            // included.
            .find(|row| (row.rect.y0..row.rect.y1).contains(&self.pointer.y))
            .copied();
        let Some(row) = hit else {
            return false;
        };
        if row.star.is_some_and(|r| r.contains(self.pointer)) {
            self.toggle_star(row.id, &mut ctx.state);
        } else {
            self.select_article(row.id, &mut ctx.state);
        }
        true
    }

    pub(crate) fn list_accessibility(&self) -> AccessNode {
        let children: Vec<AccessNode> = self
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

/// The overview column widget: paints the list from the shared core, or
/// holds the empty state while there is nothing to list.
pub(crate) struct ListPane {
    core: Shared,
    rect: Rect,
    /// The empty state on show, and what it says.
    empty: Option<(EmptyList, EmptyState)>,
    empty_node: Option<NodeId>,
    /// The empty state came or went: the tree must be rebuilt.
    structure_dirty: bool,
}

impl ListPane {
    pub(crate) fn new(core: &Shared) -> Self {
        let mut pane = Self {
            core: std::rc::Rc::clone(core),
            rect: Rect::ZERO,
            empty: None,
            empty_node: None,
            structure_dirty: false,
        };
        pane.follow();
        pane
    }

    /// Show the empty state the library calls for.
    fn follow(&mut self) {
        let Ok(core) = self.core.try_borrow() else {
            return;
        };
        let wanted = core.empty_list();
        drop(core);
        if wanted != self.empty.as_ref().map(|(shown, _)| *shown) {
            self.empty = wanted.map(|empty| (empty, empty.widget()));
            self.structure_dirty = true;
        }
    }
}

impl Widget for ListPane {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.follow();
        if let Some((_, empty)) = self.empty.as_mut() {
            empty.sync(state, path);
        }
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.follow();
        self.empty
            .as_mut()
            .is_some_and(|(_, empty)| empty.update(state, path))
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.empty_node = self
            .empty
            .as_mut()
            .map(|(_, empty)| empty.build_layout(taffy, theme, node_map));
        let style = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        };
        let node = match self.empty_node {
            Some(child) => taffy.parent_node(style, &[child]),
            None => taffy.leaf_node(style),
        };
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        if let (Some((_, empty)), Some(child)) = (self.empty.as_mut(), self.empty_node) {
            empty.apply_layout(taffy, child, self.rect.origin());
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.empty
            .as_ref()
            .map(|(_, empty)| empty as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.empty
            .as_mut()
            .map(|(_, empty)| empty as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let viewport = {
            let mut core = self.core.borrow_mut();
            core.paint_list(scene, transform, text, theme, presence, self.rect);
            core.list.viewport
        };
        if let Some((_, empty)) = self.empty.as_mut() {
            // Centred in the rows' viewport, below any refresh progress.
            empty.apply_layout_external(viewport);
            empty.draw(scene, transform, text, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.core.borrow_mut().pointer =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if let (Some((_, empty)), Some(node)) = (self.empty.as_mut(), self.empty_node) {
            empty.handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if empty.take_action() {
                self.core.borrow_mut().open_add_subscription();
            }
            if *ctx.handled {
                return;
            }
        }
        if self.core.borrow_mut().list_event(event, ctx) {
            *ctx.handled = true;
        }
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let list = self.core.borrow().list_accessibility();
        Some(match &self.empty {
            Some((_, empty)) => list.with_children(empty.accessibility().into_iter().collect()),
            None => list,
        })
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        self.core.borrow().list_tooltip(point)
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if let Some((_, empty)) = &self.empty {
            return empty.cursor_shape(point);
        }
        let core = self.core.borrow();
        core.list
            .rows
            .iter()
            .any(|row| row.star.is_some_and(|r| r.contains(point)))
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn needs_animation_frame(&self) -> bool {
        self.core
            .try_borrow()
            .is_ok_and(|core| core.list.scroller.needs_animation_frame(&core.list.theme))
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.core
            .try_borrow()
            .ok()
            .and_then(|core| core.list.scroller.next_wake_deadline(&core.list.theme))
    }

    // The rows are painted every frame, as a leaf's are.
    fn needs_redraw(&self) -> bool {
        true
    }

    fn is_layout_dirty(&self) -> bool {
        true
    }

    fn layout_needs_measure(&self) -> bool {
        true
    }

    fn mark_drawn(&mut self) {
        if let Some((_, empty)) = self.empty.as_mut() {
            empty.mark_drawn();
        }
    }

    fn name(&self) -> &'static str {
        "FeedsArticleList"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
