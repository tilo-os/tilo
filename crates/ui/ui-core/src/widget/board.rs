//! `Board`: cards in lists, moved between them by dragging (a kanban board, a pipeline, a hand of tasks
//! by status).

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::color_compat::ColorCompat as _;
use crate::dnd::DRAG_START_THRESHOLD_PX;

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::Rhythm;
use crate::widget::DrawChild;
use crate::widget::virtual_list::ListModel as _;
use crate::widget::{CardWidth, Column, Container, Element, EventCtx, Row, Scrollable, Widget};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardDrop {
    pub card: String,
    pub list: String,
    pub before: Option<String>,
}

/// One list's keys, in the order its widgets sit.
struct ListKeys {
    key: String,
    cards: Vec<String>,
    heading: bool,
    /// The list's cards are a list that builds only the cards in view
    /// ([`Board::long_list`]), not a column holding every one.
    listed: bool,
}

/// The cards of a list that builds them as they come into view: each card's key, and the
/// list over them.
type ListedCards = super::VirtualList<super::VecListModel<String, String>>;

/// How tall a card is taken to be until it is laid out: a title row and a row of details.
fn card_estimate() -> f32 {
    crate::tokens::Metrics::BASE.panel_row_height * 2.0
}

struct Drag {
    card: String,
    rect: Rect,
    grab: Point,
    start: Point,
    active: bool,
    /// The list and the card it would go before, while over a list.
    target: Option<(usize, Option<String>)>,
}

#[path = "board_component.rs"]
mod component;
pub use component::{BoardCardProps, BoardListProps, BoardProps};

pub struct Board {
    row: Row,
    lists: Vec<ListKeys>,
    drag: Option<Drag>,
    cursor: Point,
    dirty: bool,
    on_drop: Option<Box<dyn FnMut(BoardDrop)>>,
    /// What each drop asks the host for, where the app bound one.
    drops: Option<crate::widget::ActionEmitter<BoardDrop>>,
    pub(crate) name: Option<String>,
    id: String,
    /// The theme the lists were last sized in: a list's width follows the pane the board is
    /// given, which is known only once it is laid out (`Board::size_lists`).
    theme: Option<Theme>,
    sized: Option<f32>,
    pane: Option<f64>,
}

impl Default for Board {
    fn default() -> Self {
        Self::new()
    }
}

impl Board {
    #[must_use]
    pub fn new() -> Self {
        Self {
            // The lists stand the height of the board, so each one's cards scroll under its
            // header instead of the whole board growing past the pane. They keep a gutter at
            // the board's sides too: a board scrolled to either end stands its first or last
            // list a gutter in from the pane's edge, the air it began with before the first.
            row: Row::new()
                .no_wrap()
                .with_align_items(taffy::AlignItems::Stretch)
                .with_padding_rhythm(Some(Rhythm::Group), None),
            lists: Vec::new(),
            drag: None,
            cursor: Point::ZERO,
            dirty: false,
            on_drop: None,
            drops: None,
            name: None,
            id: String::new(),
            theme: None,
            sized: None,
            pane: None,
        }
    }

    /// Add a list keyed `key`, headed by `header` where it has one, holding `cards` (each
    /// with its key) in order, and `foot` at the list's end under them: what adds to
    /// the list, always in reach while the cards scroll above it.
    ///
    /// A list with no heading starts its cards at its top, where a list's first row stands. The
    /// board's owner gives a list without a name of its own the heading row of the lists beside
    /// it — an empty one — where they have headings, so every list's cards line up.
    #[must_use]
    pub fn list(
        mut self,
        key: impl Into<String>,
        header: Option<Element>,
        cards: Vec<(String, Element)>,
        foot: Option<Element>,
    ) -> Self {
        self.push_list(key.into(), header, cards, foot);
        self
    }

    /// Add a list keyed `key` whose cards, named by `cards` in order, are built by `build` only
    /// as they come into view: a list of thousands of cards builds and lays out the few its pane
    /// shows, and the rest as the list scrolls to them. Otherwise as [`Self::list`].
    #[must_use]
    pub fn long_list(
        mut self,
        key: String,
        header: Option<Element>,
        cards: Vec<String>,
        build: Box<dyn Fn(&str) -> Element>,
        foot: Option<Element>,
    ) -> Self {
        // Keys name cards once; a key given twice keeps its first place.
        let mut seen = std::collections::HashSet::new();
        let cards: Vec<String> = cards
            .into_iter()
            .filter(|card| seen.insert(card.clone()))
            .collect();
        let model = super::VecListModel::new(cards.clone(), Clone::clone)
            .unwrap_or_else(|_| super::VecListModel::empty());
        let list = ListedCards::new(model, card_estimate(), move |_, _, key| {
            // The gap a column of cards keeps between them, under each card.
            Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_rhythm(Rhythm::Sibling)
                    .push(build(key))
                    .push(Box::new(super::Spacer::token(
                        crate::tokens::SpacingStep::S0,
                    ))),
            ) as Element
        })
        .flush()
        .reading()
        .fitting_rows();
        self.push_list_body(key, header, Box::new(list), cards, true, foot);
        self
    }

    pub(crate) fn push_list(
        &mut self,
        key: String,
        header: Option<Element>,
        cards: Vec<(String, Element)>,
        foot: Option<Element>,
    ) {
        let mut cards_column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(Rhythm::Sibling);
        let mut keys = Vec::with_capacity(cards.len());
        for (card, widget) in cards {
            keys.push(card);
            cards_column = cards_column.push(widget);
        }
        self.push_list_body(key, header, Box::new(cards_column), keys, false, foot);
    }

    fn push_list_body(
        &mut self,
        key: String,
        header: Option<Element>,
        body: Element,
        keys: Vec<String>,
        listed: bool,
        foot: Option<Element>,
    ) {
        // The cards fill the list under its header and scroll there when they
        // outgrow it, so the header (and what it offers) always stays in
        // reach and a list holding more cards than the pane shows scrolls
        // instead of losing them.
        let scrolling = Scrollable::new_vertical()
            .fit_content_height()
            .with_child(body);
        let heading = header.is_some();
        let mut stack = Column::new().with_no_padding().with_rhythm(Rhythm::Related);
        if let Some(header) = header {
            stack = stack.push(header);
        }
        let stack = stack.push(Box::new(scrolling));
        let stack = match foot {
            Some(foot) => stack.push(foot),
            None => stack,
        };
        let list = Container::new()
            .with_no_padding()
            .filling()
            .with_width(0.0)
            .with_child(Box::new(stack));
        self.row = std::mem::take(&mut self.row).push(Box::new(list));
        self.lists.push(ListKeys {
            key,
            cards: keys,
            heading,
            listed,
        });
    }

    pub(crate) fn set_lists(
        &mut self,
        lists: Vec<(
            String,
            Option<Element>,
            Vec<(String, Element)>,
            Option<Element>,
        )>,
    ) {
        let mut row = Row::new()
            .no_wrap()
            .with_align_items(taffy::AlignItems::Stretch)
            .with_padding_rhythm(Some(Rhythm::Group), None);
        if !self.id.is_empty() {
            row = row.with_id(self.id.clone());
        }
        self.row = row;
        self.lists.clear();
        for (key, header, cards, foot) in lists {
            self.push_list(key, header, cards, foot);
        }
        self.dirty = true;
    }

    pub(crate) fn set_on_drop(&mut self, on_drop: Box<dyn FnMut(BoardDrop)>) {
        self.on_drop = Some(on_drop);
    }

    /// Called with each drop.
    #[must_use]
    pub fn on_drop(mut self, on_drop: impl FnMut(BoardDrop) + 'static) -> Self {
        self.on_drop = Some(Box::new(on_drop));
        self
    }

    /// Each drop asks the host for `binding`: the app's own action, so the
    /// move is confirmed, undone and permitted like every other.
    #[must_use]
    pub fn on_drop_binding(mut self, binding: tilo_actions::ActionBinding<BoardDrop>) -> Self {
        self.drops = Some(crate::widget::ActionEmitter::new(binding));
        self
    }

    fn drop_card(&mut self, drop: BoardDrop) {
        if let Some(drops) = &mut self.drops {
            if let Err(error) = drops.emit(drop) {
                tracing::warn!(%error, "a drop refused its card");
            }
        } else if let Some(on_drop) = &mut self.on_drop {
            on_drop(drop);
        }
    }

    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        let id = id.into();
        self.row = std::mem::take(&mut self.row).with_id(id.clone());
        self.id = id;
        self
    }

    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.drag.as_ref().is_some_and(|drag| drag.active)
    }

    /// Each list's rect, in list order.
    pub(crate) fn list_rects(&self) -> Vec<Rect> {
        self.row
            .children()
            .iter()
            .map(|list| list.get_rect())
            .collect()
    }

    /// Each card shown in list `list`, with its key, in order: the list's
    /// header stands above them, and its cards scroll under it.
    fn shown_cards(&self, list: usize) -> Vec<(String, Rect)> {
        let Some(keys) = self.lists.get(list) else {
            return Vec::new();
        };
        let children = self.row.children();
        let Some(container) = children.get(list) else {
            return Vec::new();
        };
        let band = container.get_rect();
        let list_children = container.children();
        let Some(stack) = list_children.first() else {
            return Vec::new();
        };
        // The stack holds the list's heading where it has one, then the view that scrolls its
        // cards, whose only child holds them.
        let stack_children = stack.children();
        let Some(scrolling) = stack_children.get(usize::from(keys.heading)) else {
            return Vec::new();
        };
        let scrolled = scrolling.children();
        let Some(cards) = scrolled.first() else {
            return Vec::new();
        };
        if keys.listed {
            let Some(list) = cards
                .as_any()
                .and_then(|any| any.downcast_ref::<ListedCards>())
            else {
                return Vec::new();
            };
            return list
                .mounted_rows()
                .filter_map(|index| {
                    let key = list.model().key_at(index)?;
                    let rect = list.row_rect(&key)?;
                    Some((key, rect))
                })
                .filter(|(_, card)| card.intersect(band).area() > 0.0)
                .collect();
        }
        cards
            .children()
            .iter()
            .zip(&keys.cards)
            .map(|(card, key)| (key.clone(), card.get_rect()))
            .filter(|(_, card)| card.intersect(band).area() > 0.0)
            .collect()
    }

    /// Each card shown in list `list`, in order.
    #[cfg(test)]
    pub(crate) fn card_rects(&self, list: usize) -> Vec<Rect> {
        self.shown_cards(list)
            .into_iter()
            .map(|(_, rect)| rect)
            .collect()
    }

    fn card_at(&self, point: Point) -> Option<(String, Rect)> {
        (0..self.lists.len()).find_map(|list| {
            self.shown_cards(list)
                .into_iter()
                .find(|(_, rect)| rect.contains(point))
        })
    }

    fn target_at(&self, point: Point, dragged: &str) -> Option<(usize, Option<String>)> {
        let list = self
            .list_rects()
            .iter()
            .position(|rect| point.x >= rect.x0 && point.x < rect.x1)?;
        let before = self
            .shown_cards(list)
            .into_iter()
            .filter(|(key, _)| key.as_str() != dragged)
            .find(|(_, rect)| point.y < rect.center().y)
            .map(|(key, _)| key);
        Some((list, before))
    }

    /// The width a card reads best in a list (`metrics.board_list_width`), never below the
    /// measure a card's own text needs at this theme's type size.
    fn comfortable_list(theme: &Theme) -> f64 {
        f64::from(theme.tokens.metrics.board_list_width).max(CardWidth::Regular.min_width(theme))
    }

    /// The width each of `lists` lists takes on a board `width` wide: a comfortable measure,
    /// the same on every board and at every window width, so lists stand left-aligned with the
    /// width a wider pane leaves open beside them. Only a pane too narrow for one list and the
    /// board's gutters narrows it, and then to the pane, so a narrow board shows one whole list
    /// per page rather than one list and a sliver of the next.
    ///
    /// Lists that fall a little short of fitting side by side narrow, never below the measure a
    /// card needs, so every list stands whole in the pane rather than the last one cut off at its
    /// edge; lists that do not fit even then keep their comfortable measure and scroll.
    pub(crate) fn lists_width(theme: &Theme, width: f64, lists: usize) -> f32 {
        let gutter = f64::from(theme.tokens.spacing.rhythm.get(Rhythm::Group));
        let gap = f64::from(theme.tokens.spacing.rhythm.get(Rhythm::Sibling));
        let least = f64::from(CardWidth::Regular.min_width(theme));
        let comfortable = Self::comfortable_list(theme);
        let spare = (width - 2.0 * gutter).max(0.0);
        #[allow(
            clippy::cast_precision_loss,
            reason = "a board holds a handful of lists"
        )]
        let count = lists.max(1) as f64;
        let fitted = (spare - gap * (count - 1.0)) / count;
        let list = if lists > 1 && fitted < comfortable && fitted >= least {
            fitted
        } else if spare >= comfortable {
            comfortable
        } else {
            spare.max(least)
        };
        #[allow(clippy::cast_possible_truncation, reason = "a list width fits f32")]
        {
            list as f32
        }
    }

    fn set_list_widths(&mut self, width: f32) {
        for list in self.row.children_mut() {
            if let Some(list) = list
                .as_any_mut()
                .and_then(|list| list.downcast_mut::<Container>())
            {
                list.set_width(width);
            }
        }
    }

    /// How wide each list is on a board `width` wide: a comfortable measure where the width is
    /// not yet known.
    fn list_width_at(&self, width: taffy::AvailableSpace) -> Option<f32> {
        let theme = self.theme.as_ref()?;
        #[allow(clippy::cast_possible_truncation, reason = "a list width fits f32")]
        Some(match width {
            taffy::AvailableSpace::Definite(width) => {
                Self::lists_width(theme, f64::from(width), Widget::children(&self.row).len())
            }
            _ => Self::comfortable_list(theme) as f32,
        })
    }

    fn line_rect(&self, (list, before): &(usize, Option<String>), theme: &Theme) -> Option<Rect> {
        let band = *self.list_rects().get(*list)?;
        let shown = self.shown_cards(*list);
        let gap = f64::from(theme.tokens.spacing.s2);
        let line = f64::from(theme.tokens.metrics.drag_line_px);
        let y = match before {
            Some(before) => shown.iter().find(|(key, _)| key == before)?.1.y0 - gap / 2.0,
            None => shown
                .last()
                .map_or(band.y0 + gap, |(_, row)| row.y1 + gap / 2.0),
        };
        Some(Rect::new(band.x0, y - line / 2.0, band.x1, y + line / 2.0))
    }

    /// The pointer left every card: a drag started, so the press that began
    /// it is not a click.
    fn cancel_child_press(&mut self, event_ctx: &mut EventCtx<'_>) {
        let away = WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(-1.0e6, -1.0e6),
        };
        Widget::handle_event(&mut self.row, &away, event_ctx);
    }

    fn pointer(&self, position: winit::dpi::PhysicalPosition<f64>, scale: f64) -> Point {
        let _ = self;
        Point::new(position.x / scale, position.y / scale)
    }
}

impl crate::widget::controls::ActionControls for Board {
    fn attach_controls(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_domain::DomainAttachment,
    ) {
        if let Some(drops) = &mut self.drops {
            let _ = drops.attach(domain, attachment);
        }
        for child in Widget::children_mut(self) {
            crate::widget::controls::attach_controls(child, domain, attachment);
        }
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        if let Some(invocation) = self
            .drops
            .as_mut()
            .and_then(crate::widget::ActionEmitter::take)
        {
            return Some(invocation);
        }
        Widget::children_mut(self)
            .into_iter()
            .find_map(crate::widget::controls::take_control_invocation)
    }

    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        let mut bindings = Vec::new();
        if let Some(drops) = &self.drops {
            let control = Widget::test_id(self).unwrap_or("board").to_owned();
            bindings.push(drops.metadata(control));
        }
        for child in Widget::children(self) {
            bindings.extend(crate::widget::controls::control_action_bindings(child));
        }
        bindings
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        if let Some(drops) = &mut self.drops {
            drops.refresh_availability(allowed);
        }
        for child in Widget::children_mut(self) {
            crate::widget::controls::refresh_control_action_availability(child, allowed);
        }
    }
}

impl crate::widget::Decorator for Board {
    type Inner = Row;

    fn inner(&self) -> &Row {
        &self.row
    }

    fn inner_mut(&mut self) -> &mut Row {
        &mut self.row
    }

    fn provide<'a>(&'a self, request: &mut crate::widget::capability::Request<'a, '_>) {
        if self.drops.is_some() && request.wants::<dyn crate::widget::controls::ActionControls>() {
            request.provide::<dyn crate::widget::controls::ActionControls>(self);
            return;
        }
        self.row.provide(request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut crate::widget::capability::RequestMut<'a, '_>) {
        if self.drops.is_some() && request.wants::<dyn crate::widget::controls::ActionControls>() {
            request.provide::<dyn crate::widget::controls::ActionControls>(self);
            return;
        }
        self.row.provide_mut(request);
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || Widget::needs_redraw(&self.row)
    }

    fn touch_claim(&self, point: Point) -> crate::interaction::TouchClaim {
        if self.card_at(point).is_some() {
            crate::interaction::TouchClaim::HoldDrag
        } else {
            Widget::touch_claim(&self.row, point)
        }
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        self.name
            .as_ref()
            .map(|name| AccessNode::new(Role::Group).with_optional_name(Some(name.clone())))
    }

    /// A list a page at a time where the pane is too narrow to show two lists side by side: the
    /// board offers each list's leading edge, less the air it keeps before the first, so the
    /// view showing it settles a list at a time. A pane wide enough for its lists shows them
    /// side by side and offers none: it scrolls freely.
    fn snap_points(&self, axis: crate::widget::ScrollAxis, out: &mut Vec<f32>) {
        if !matches!(axis, crate::widget::ScrollAxis::Horizontal) {
            return;
        }
        let (Some(theme), Some(pane), Some(list)) = (&self.theme, self.pane, self.sized) else {
            return;
        };
        if f64::from(list) * 2.0 <= pane {
            return;
        }
        let gutter = f64::from(theme.tokens.spacing.rhythm.get(Rhythm::Group));
        let leading = Widget::get_rect(self).x0;
        out.extend(
            Widget::children(&self.row)
                .iter()
                .map(|list| crate::num::f32_of_f64(Widget::get_rect(*list).x0 - leading - gutter)),
        );
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = Some(theme.clone());
        #[allow(clippy::cast_possible_truncation, reason = "a list width fits f32")]
        let width = self.sized.unwrap_or(Self::comfortable_list(theme) as f32);
        self.set_list_widths(width);
        let node = Widget::build_layout(&mut self.row, taffy, theme, node_map);
        let _ = taffy.set_restyled_for_width(node, true);
        node
    }

    /// The board is as wide as the pane it is in, so the solve hands it the pane's width; lists
    /// it cannot fit side by side stand past its edge, where the pane scrolls to them.
    fn size_before_solve(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        _text: &mut crate::text_render::TextRenderer,
    ) {
        if let Ok(current) = taffy.style(node)
            && current.size.width != taffy::Dimension::percent(1.0)
        {
            let mut style = current.clone();
            style.size.width = taffy::Dimension::percent(1.0);
            let _ = taffy.set_style(node, style);
        }
    }

    /// Each list as wide as [`Self::lists_width`] says for the pane's width.
    fn style_for_width(
        &mut self,
        styles: &mut taffy::Restyle<'_>,
        width: taffy::AvailableSpace,
        _text: &mut crate::text_render::TextRenderer,
    ) {
        let Some(list) = self.list_width_at(width) else {
            return;
        };
        let node = styles.node();
        for list_node in styles.children(node).to_vec() {
            let mut style = styles.style(list_node).clone();
            style.size.width = taffy::Dimension::length(list);
            // Lists past the board's edge keep their width: the pane scrolls to them.
            style.flex_shrink = 0.0;
            styles.set_style(list_node, style);
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let width = taffy.layout(node).map_or(0.0, |layout| layout.size.width);
        self.pane = Some(f64::from(width));
        if let Some(list) = self.list_width_at(taffy::AvailableSpace::Definite(width)) {
            self.sized = Some(list);
            self.set_list_widths(list);
        }
        Widget::apply_layout(&mut self.row, taffy, node, origin);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = self.pointer(*position, ctx.scale_factor);
                let cursor = self.cursor;
                let mut started = false;
                if let Some(drag) = self.drag.as_mut() {
                    if !drag.active && drag.start.distance(cursor) > DRAG_START_THRESHOLD_PX {
                        drag.active = true;
                        started = true;
                    }
                    if drag.active {
                        let card = drag.card.clone();
                        let target = self.target_at(cursor, &card);
                        if let Some(drag) = self.drag.as_mut() {
                            drag.target = target;
                        }
                        self.dirty = true;
                        if started {
                            self.cancel_child_press(ctx);
                        }
                        *ctx.handled = true;
                        return;
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some((card, rect)) = self.card_at(self.cursor) {
                    self.drag = Some(Drag {
                        card,
                        rect,
                        grab: Point::new(self.cursor.x - rect.x0, self.cursor.y - rect.y0),
                        start: self.cursor,
                        active: false,
                        target: None,
                    });
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } if self.drag.is_some() => {
                self.drag = None;
                self.dirty = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(drag) = self.drag.take()
                    && drag.active
                {
                    self.dirty = true;
                    if let Some((list, before)) = drag.target
                        && let Some(keys) = self.lists.get(list)
                    {
                        let drop = BoardDrop {
                            card: drag.card,
                            list: keys.key.clone(),
                            before,
                        };
                        self.drop_card(drop);
                    }
                    *ctx.handled = true;
                    return;
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key
                        == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
                    && self.is_dragging() =>
            {
                self.drag = None;
                self.dirty = true;
                *ctx.handled = true;
                return;
            }
            _ => {}
        }
        Widget::handle_event(&mut self.row, event, ctx);
    }

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        DrawChild::draw_child(&mut self.row, scene, transform, text_renderer, theme, &env);
        self.dirty = false;
        let Some(drag) = self.drag.as_ref().filter(|drag| drag.active) else {
            return;
        };
        let colors = &theme.tokens.colors.intent;
        let radius = f64::from(theme.tokens.radii.control);
        let line_px = f64::from(theme.tokens.metrics.drag_line_px);
        if let Some(target) = &drag.target {
            if let Some(band) = self.list_rects().get(target.0) {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.selection.with_alpha_factor(0.35),
                    None,
                    &RoundedRect::from_rect(*band, radius),
                );
            }
            if let Some(line) = self.line_rect(target, theme) {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.accent,
                    None,
                    &RoundedRect::from_rect(line, line_px / 2.0),
                );
            }
        }
        let origin = Point::new(self.cursor.x - drag.grab.x, self.cursor.y - drag.grab.y);
        let ghost = Rect::from_origin_size(origin, drag.rect.size());
        let shape = RoundedRect::from_rect(ghost, radius);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.selection.with_alpha_factor(0.6),
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(line_px),
            transform,
            colors.accent,
            None,
            &shape,
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "in a test these are the assertion"
    )]

    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use crate::widget::{Spacer, Text};

    fn card() -> Element {
        Box::new(
            Container::new()
                .with_size(200.0, 60.0)
                .with_child(Box::new(Spacer::fixed(1.0))),
        )
    }

    fn board(drops: &Rc<RefCell<Vec<BoardDrop>>>) -> Board {
        let sink = Rc::clone(drops);
        Board::new()
            .list(
                "todo",
                Some(Box::new(Text::new("Todo")) as Element),
                vec![("a".into(), card()), ("b".into(), card())],
                None,
            )
            .list(
                "done",
                Some(Box::new(Text::new("Done")) as Element),
                vec![("c".into(), card())],
                None,
            )
            .on_drop(move |drop| sink.borrow_mut().push(drop))
    }

    fn harness(board: Board) -> crate::headless::FrameHarness {
        crate::headless::FrameHarness::for_root(
            crate::State::new(),
            board,
            (900, 600),
            Theme::dark(),
        )
    }

    tilo_test_env::scenario! {
        /// The board widget: lists, gutters and drops.
        /// Each phase was a test of its own and builds its own fixture.
        fn a_board_lays_out_lists_and_reports_drops;

        phase a_list_shows_every_card_it_fits;
        // A list as tall as the pane it is in shows every card it fits under its heading,
        // and the ones past that scroll in the view under it rather than being lost.
        {
            let cards: Vec<(String, Element)> = (0..8)
                .map(|index| (format!("card-{index}"), card()))
                .collect();
            let board = Board::new().list("todo", Some(Box::new(Text::new("Todo"))), cards, None);
            let host = harness(board);
            let root = host.root::<Board>();
            let shown = root.card_rects(0);
            assert_eq!(
                shown.len(),
                8,
                "eight 60 tall cards stand in a 600 tall pane under their heading, and are all shown"
            );
            let list = root.list_rects()[0];
            for (index, rect) in shown.iter().enumerate() {
                assert!(
                    rect.y0 >= list.y0 && rect.y1 <= list.y1,
                    "card {index} stands in its list: {rect:?} in {list:?}"
                );
                assert!(
                    rect.y0 > list.y0,
                    "card {index} stands below the list's heading"
                );
            }
            assert!(
                shown.windows(2).all(|pair| pair[0].y1 <= pair[1].y0),
                "the cards are in order, one under the other"
            );
        }

        phase a_board_keeps_a_gutter_at_both_ends_of_its_lists;
        // The lists stand left-aligned at their comfortable measure: the first a gutter in from
        // the leading edge, each the sibling gap from the last, and the width a wider pane leaves
        // open beside them at the trailing edge.
        {
            let theme = Theme::dark();
            let list = f64::from(Board::lists_width(&theme, 1280.0, 1));
            let gutter = f64::from(theme.tokens.spacing.rhythm.get(Rhythm::Group));
            let gap = f64::from(theme.tokens.spacing.rhythm.get(Rhythm::Sibling));
            let pane = 1280u32;
            let mut lists = Board::new();
            for key in ["todo", "doing", "done"] {
                let header = Some(Box::new(Text::new(key)) as Element);
                lists = lists.list(key, header, vec![(format!("{key}-card"), card())], None);
            }
            let host =
                crate::headless::FrameHarness::for_root(crate::State::new(), lists, (pane, 600), theme);
            let root = host.root::<Board>();
            let rects = root.list_rects();
            assert_eq!(rects.len(), 3, "three lists");
            let board = root.get_rect();
            assert!(
                (rects[0].x0 - board.x0 - gutter).abs() < 1.0,
                "the first list stands a gutter in from the leading edge: {rects:?} in {board:?}"
            );
            for (index, rect) in rects.iter().enumerate() {
                assert!(
                    (rect.width() - list).abs() < 1.0,
                    "list {index} keeps its comfortable measure: {rect:?}"
                );
                if let Some(next) = rects.get(index + 1) {
                    assert!(
                        (next.x0 - rect.x1 - gap).abs() < 1.0,
                        "list {index} is a sibling gap from the next: {rects:?}"
                    );
                }
            }
            assert!(
                board.x1 - rects[2].x1 > gutter,
                "a wider pane leaves its extra area open beside the lists: {rects:?} in {board:?}"
            );
        }
    }

    /// A board whose lists build their cards as they come into view, counting every card built.
    fn listed(cards: usize, built: &Rc<std::cell::Cell<usize>>) -> Board {
        let mut board = Board::new();
        for list in ["todo", "done"] {
            let keys: Vec<String> = (0..cards).map(|index| format!("{list}-{index}")).collect();
            let counted = Rc::clone(built);
            board = board.long_list(
                list.to_owned(),
                Some(Box::new(Text::new(list)) as Element),
                keys,
                Box::new(move |_| {
                    counted.set(counted.get() + 1);
                    card()
                }),
                None,
            );
        }
        board
    }

    #[test]
    fn a_list_of_ten_thousand_cards_builds_the_few_it_shows_and_more_as_it_scrolls() {
        let built = Rc::new(std::cell::Cell::new(0));
        let mut host = harness(listed(10_000, &built));
        // Before its first layout a list holds the cards of a tall screen; then the nine or so
        // 60 tall cards its 600 tall pane shows, with a margin either side. However long it is.
        let tall_screen = (crate::virtual_viewport::UNPLACED_VIEWPORT / f64::from(card_estimate()))
            .ceil() as usize;
        let opened = built.get();
        assert!(
            opened <= 2 * (tall_screen + 3 * 10 + 16),
            "two lists of 10,000 cards built {opened} of them as the board opened"
        );
        let shown = host.root::<Board>().card_rects(0);
        assert!(
            (5..=12).contains(&shown.len()),
            "the list shows the cards its height fits: {}",
            shown.len()
        );
        let list = host.root::<Board>().list_rects()[0];
        host.send(&crate::synthetic_input::cursor_moved(
            list.center().x,
            list.center().y,
        ));
        for _ in 0..20 {
            let before = built.get();
            host.scroll(0.0, -10.0);
            let _ = host.poll_frame();
            assert!(
                built.get() - before <= 40,
                "a step of the list's scroll built {} cards",
                built.get() - before
            );
        }
        let shown = host.root::<Board>().shown_cards(0);
        let keys: Vec<&str> = shown.iter().map(|(key, _)| key.as_str()).collect();
        assert!(
            (5..=12).contains(&shown.len()),
            "scrolled, the list shows the cards its height fits: {keys:?}"
        );
        assert!(
            !keys.contains(&"todo-0"),
            "scrolled, the list shows cards further down: {keys:?}"
        );
    }

    #[test]
    fn a_card_dragged_between_long_lists_drops_before_the_card_under_it() {
        let built = Rc::new(std::cell::Cell::new(0));
        let drops = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&drops);
        let board = listed(3, &built).on_drop(move |drop| sink.borrow_mut().push(drop));
        let mut host = harness(board);
        let root = host.root::<Board>();
        let a = root.card_rects(0)[0];
        let c = root.card_rects(1)[0];
        host.drag_at((a.center().x, a.center().y), (c.center().x, c.y0 + 5.0));
        assert_eq!(
            drops.borrow().as_slice(),
            [BoardDrop {
                card: "todo-0".into(),
                list: "done".into(),
                before: Some("done-0".into()),
            }]
        );
    }

    #[test]
    fn dragging_a_card_into_another_list_reports_one_drop_before_the_card_under_it() {
        let drops = Rc::new(RefCell::new(Vec::new()));
        let mut host = harness(board(&drops));
        let root = host.root::<Board>();
        let a = root.card_rects(0)[0];
        let c = root.card_rects(1)[0];
        host.drag_at((a.center().x, a.center().y), (c.center().x, c.y0 + 5.0));
        assert_eq!(
            drops.borrow().as_slice(),
            [BoardDrop {
                card: "a".into(),
                list: "done".into(),
                before: Some("c".into()),
            }]
        );
        assert!(!host.root::<Board>().is_dragging());
    }

    #[test]
    fn dropping_below_every_card_lands_at_the_end_and_a_click_is_no_drop() {
        let drops = Rc::new(RefCell::new(Vec::new()));
        let mut host = harness(board(&drops));
        let root = host.root::<Board>();
        let a = root.card_rects(0)[0];
        let b = root.card_rects(0)[1];
        host.click_at(a.center().x, a.center().y);
        assert!(drops.borrow().is_empty(), "a click moves nothing");
        host.drag_at((a.center().x, a.center().y), (b.center().x, b.y1 + 40.0));
        assert_eq!(
            drops.borrow().as_slice(),
            [BoardDrop {
                card: "a".into(),
                list: "todo".into(),
                before: None,
            }]
        );
    }
}
