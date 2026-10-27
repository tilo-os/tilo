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

pub(crate) const EMPTY_LIBRARY_TITLE: &str = "No feeds yet";

/// One article row as last painted.
#[derive(Clone, Debug)]
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
    /// What the frame being drawn paints, and what the last frame the host
    /// presented painted ([`Core::list_damage`]).
    shown_drawing: Option<ListShown>,
    shown_presented: Option<ListShown>,
}

/// What one frame of the list paints, to tell the frame on screen apart
/// from the next: a scroll that changed nothing else is offered to the
/// host as a copy of the rows that stay on screen.
#[derive(Clone, Debug, PartialEq)]
struct ListShown {
    /// What the rows are laid out and painted with besides the articles and
    /// the offset; `None` while the refresh progress paints above them,
    /// which repaints the list.
    frame: Option<ListFrame>,
    /// The index of the first painted row, and what each painted row paints
    /// from, its hover aside.
    first: usize,
    rows: Vec<u64>,
    /// The hovered row (its index, whether its star is under the pointer)
    /// and where it paints its lift and actions: they follow the pointer,
    /// not the rows.
    hovered: Option<(usize, bool, Rect)>,
    offset: f64,
}

#[derive(Clone, Debug, PartialEq)]
struct ListFrame {
    area: Rect,
    viewport: Rect,
    x0: f64,
    x1: f64,
    row_h: f64,
    inset: f64,
    count: usize,
    theme: Theme,
}

impl ListShown {
    /// Whether every row both frames paint paints the same, its hover
    /// aside.
    fn rows_agree(&self, other: &Self) -> bool {
        let start = self.first.max(other.first);
        let end = self
            .first
            .saturating_add(self.rows.len())
            .min(other.first.saturating_add(other.rows.len()));
        (start..end).all(|index| {
            let mine = self.rows.get(index.saturating_sub(self.first));
            mine.is_some() && mine == other.rows.get(index.saturating_sub(other.first))
        })
    }
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
            shown_drawing: None,
            shown_presented: None,
        }
    }
}

impl ListState {
    /// How far the rows are scrolled.
    pub(crate) fn scroll(&self) -> f64 {
        self.scroller.offset().y
    }

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
                .feed(&article.feed_id)
                .map_or("", |f| f.item().title.as_str()),
        }
    }

    /// What the list says while the chosen place holds no articles: an
    /// empty library offers its one action, an empty place says what would
    /// fill it.
    pub(crate) fn empty_list(&self) -> Option<EmptyList> {
        if !self.store.articles_for(&self.filter).is_empty() {
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
        self.list.shown_drawing = None;
        let resized = self.list.rect.size() != area.size();
        self.list.rect = area;
        if area.width() <= 0.0 || area.height() <= 0.0 {
            return;
        }
        let inset = f64::from(theme.tokens.spacing.s2);
        let settled = !self.refresh.visible();
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
            .articles_for(&self.filter)
            .iter()
            .map(|a| a.id.clone())
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
                .as_ref()
                .and_then(|sel| ids.iter().position(|id| id == sel))
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

        let frame = ListFrame {
            area,
            viewport,
            x0,
            x1,
            row_h,
            inset,
            count: ids.len(),
            theme: theme.clone(),
        };
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &viewport);
        let shown = self.paint_rows(
            &mut PaintCtx::new(scene, t, text, theme),
            &ids,
            frame,
            settled,
        );
        scene.pop_layer();
        self.list.shown_drawing = Some(shown);
        // Rows fade into the column where more can scroll in, under the bar.
        self.list.scroller.paint(scene, t, theme, |_, _| {});
    }

    /// The rows of `ids` in view, laid out by `frame`, and what they
    /// painted ([`ListShown`]); `settled` while nothing else paints above
    /// them.
    fn paint_rows(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        ids: &[Id],
        frame: ListFrame,
        settled: bool,
    ) -> ListShown {
        let scroll = self.list.scroll();
        let (viewport, row_h) = (frame.viewport, frame.row_h);
        let first = ((scroll - frame.inset) / row_h).floor().max(0.0) as usize;
        let mut y = viewport.y0 + frame.inset - scroll + row_h * first as f64;
        let (x0, x1) = (frame.x0, frame.x1);
        let mut shown = ListShown {
            frame: settled.then_some(frame),
            first,
            rows: Vec::new(),
            hovered: None,
            offset: scroll,
        };
        for (i, id) in ids.iter().enumerate().skip(first) {
            if y > viewport.y1 {
                break;
            }
            let rect = Rect::new(x0, y, x1, y + row_h);
            y += row_h;
            let (key, hovered, star_hot) = self.paint_row(ctx, rect, id, ids.get(i + 1));
            shown.rows.push(key);
            if hovered {
                // The lift bleeds past the row's text edges.
                shown.hovered = Some((
                    i,
                    star_hot,
                    Rect::new(viewport.x0, rect.y0, viewport.x1, rect.y1)
                        .inflate(4.0, 4.0)
                        .intersect(viewport),
                ));
            }
        }
        shown
    }

    /// One article's row at `rect`; `next` is the article below it.
    /// Returns what the row paints from, its hover aside, whether it is
    /// hovered and whether its star is under the pointer.
    fn paint_row(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        id: &Id,
        next: Option<&Id>,
    ) -> (u64, bool, bool) {
        use std::hash::{Hash, Hasher};
        let Some(article) = self.store.article(id).map(|article| article.item()) else {
            return (0, false, false);
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
            .selected(self.selected.as_ref() == Some(id))
            .actions(&star)
            .interaction(Interaction {
                hovered,
                ..Interaction::default()
            });
        let star_rect = face.action_rects(rect, ctx.theme).first().copied();
        let star_hot = star_rect.is_some_and(|r| r.contains(self.pointer));
        let face = face.hovered_action(star_hot.then_some(0));
        let face = if article.starred {
            face.mark(Icon::Star)
        } else {
            face
        };
        let next_selected = next.map(|next| self.selected.as_ref() == Some(next));
        let face = match next_selected {
            Some(selected) => face.separator(selected),
            None => face,
        };
        face.paint(ctx, rect);
        let mut key = std::collections::hash_map::DefaultHasher::new();
        (
            id,
            &article.title,
            self.row_secondary(article),
            &time,
            article.read,
            article.starred,
            self.selected.as_ref() == Some(id),
            next_selected,
        )
            .hash(&mut key);
        self.list.rows.push(RowHit {
            rect: rect.intersect(viewport),
            id: id.clone(),
            star: star_rect,
        });
        (key.finish(), hovered, hovered && star_hot)
    }

    /// The list's damage since the frame on screen, from what the frame
    /// just drawn painted: a scroll that changed nothing else offers the
    /// host a copy of the rows that stay on screen, repainting the hovered
    /// row where it is and was; a frame that changed only the hover
    /// repaints those rows; anything else repaints `rect`.
    fn list_damage<T: ?Sized>(&self, owner: &T, rect: Rect, regions: &mut Vec<Rect>) {
        let (Some(drawing), Some(presented)) = (
            self.list.shown_drawing.as_ref(),
            self.list.shown_presented.as_ref(),
        ) else {
            regions.push(rect);
            return;
        };
        let same = drawing.frame.is_some()
            && drawing.frame == presented.frame
            && drawing.rows_agree(presented);
        if !same {
            regions.push(rect);
            return;
        }
        let hover = |shown: &ListShown| shown.hovered.map(|(_, _, at)| at);
        let hovers = hover(drawing).into_iter().chain(hover(presented));
        // The same row hovered the same way: its lift moved with it.
        let same_hover = drawing.hovered.map(|(row, star, _)| (row, star))
            == presented.hovered.map(|(row, star, _)| (row, star));
        if drawing.offset.to_bits() != presented.offset.to_bits() {
            if self.list.scroller.offer_copy(owner, regions) {
                if !same_hover {
                    regions.extend(hovers);
                }
            } else {
                regions.push(rect);
            }
            return;
        }
        if drawing.hovered != presented.hovered {
            regions.extend(hovers);
        }
        self.list.scroller.damage_changed_bars(regions);
    }

    /// The frame just drawn is on screen.
    fn list_presented(&mut self) {
        self.list.shown_presented = self.list.shown_drawing.take();
        self.list.scroller.mark_presented();
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
            .cloned();
        let Some(row) = hit else {
            return false;
        };
        if row.star.is_some_and(|r| r.contains(self.pointer)) {
            self.toggle_star(&row.id, &mut ctx.state);
        } else {
            self.select_article(&row.id, &mut ctx.state);
        }
        true
    }

    pub(crate) fn list_accessibility(&self) -> AccessNode {
        let children: Vec<AccessNode> = self
            .list
            .rows
            .iter()
            .filter_map(|row| {
                let article = self.store.article(&row.id)?.item();
                let time = relative_time(article.published, self.now);
                let face = ListRowFace::new(&article.title)
                    .secondary(self.row_secondary(article))
                    .meta(&time)
                    .selected(self.selected.as_ref() == Some(&row.id));
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
        let article = self.store.article(&row.id)?.item();
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
        // A glide after a flick moves the rows; the frame copies them as it
        // does a wheel's scroll ([`Core::list_damage`]).
        if let Ok(mut core) = self.core.try_borrow_mut() {
            core.list.scroller.advance();
        }
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
            size: taffy::Size::from_percent(1.0, 1.0),
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
            self.core.borrow_mut().pointer = ctx.logical(position);
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

    // The rows are painted every frame, as a leaf's are; what changed is
    // told after the draw, from what it painted (`collect_damage`).
    fn needs_redraw(&self) -> bool {
        true
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        match self.core.try_borrow() {
            Ok(core) if self.empty.is_none() => core.list_damage(self, self.rect, regions),
            _ => regions.push(self.rect),
        }
    }

    fn is_layout_dirty(&self) -> bool {
        true
    }

    fn layout_needs_measure(&self) -> bool {
        true
    }

    fn mark_drawn(&mut self) {
        if let Ok(mut core) = self.core.try_borrow_mut() {
            core.list_presented();
        }
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

#[cfg(test)]
#[path = "list_pane/copy_tests.rs"]
mod copy_tests;
