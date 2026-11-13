//! `Board`: cards in lanes, moved between them by dragging (a kanban board,
//! a pipeline, a hand of tasks by status).
//!
//! Each lane is a column with a header, where it has one, and its cards, side by side. The
//! cards are any widgets (usually a pressable [`Card`](super::Card)), so a
//! card opens, takes focus and is read out as itself. Dragging one shows a
//! ghost under the pointer and the insertion line where it would land, over
//! the lane it would land in; releasing it reports one [`BoardDrop`]: the
//! card, the lane and the card it goes before. The board changes nothing
//! itself: the owner turns the drop into one action on its model, and the
//! board shows the card where the model then puts it.
//!
//! A press that never travels past [`DRAG_START_THRESHOLD_PX`] is the card's
//! own click. Once a drag starts, the card under it is told the pointer left,
//! so releasing ends the drag without also opening the card.
//!
//! On a touchscreen a still hold lifts a card and a drag after it moves it;
//! a drag that begins without the hold scrolls what the board is in.
//!
//! A card is moved without a pointer through what it is an object of (a
//! status in its page, the action the drop stands for), never only by
//! dragging.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::color_compat::ColorCompat as _;
use crate::dnd::DRAG_START_THRESHOLD_PX;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::Rhythm;
use crate::widget::DrawChild;
use crate::widget::{CardWidth, Column, Container, Element, EventCtx, Row, Scrollable, Widget};

/// Where a dragged card was dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardDrop {
    /// The card's key.
    pub card: String,
    pub lane: String,
    /// The card it goes before; `None` for the end of the lane.
    pub before: Option<String>,
}

/// One lane's keys, in the order its widgets sit.
struct LaneKeys {
    key: String,
    cards: Vec<String>,
    /// Whether the lane heads itself: its column starts with the heading, over the view that
    /// scrolls its cards. A lane nobody named heads nothing, and its cards start at its top.
    heading: bool,
}

/// A drag from press to release.
struct Drag {
    card: String,
    /// The pressed card's rect and where in it the pointer took it.
    rect: Rect,
    grab: Point,
    start: Point,
    active: bool,
    /// The lane and the card it would go before, while over a lane.
    target: Option<(usize, Option<String>)>,
}

/// See the [module docs](self).
#[path = "board_component.rs"]
mod component;
pub use component::{BoardCardProps, BoardLaneProps, BoardProps};

pub struct Board {
    row: Row,
    lanes: Vec<LaneKeys>,
    drag: Option<Drag>,
    cursor: Point,
    dirty: bool,
    on_drop: Option<Box<dyn FnMut(BoardDrop)>>,
    /// What each drop asks the host for, where the app bound one.
    drops: Option<crate::widget::ActionEmitter<BoardDrop>>,
    /// What the board is, for assistive technology.
    pub(crate) name: Option<String>,
    /// The key the board was built under.
    id: String,
    /// The theme the lanes were last sized in: a lane's width follows the pane the board is
    /// given, which is known only once it is laid out (`Board::size_lanes`).
    theme: Option<Theme>,
    /// The width the lanes were last given.
    sized: Option<f32>,
    /// The width the pane showing the board was last laid out at: what a lane is judged
    /// compact against ([`Board::snap_points`]).
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
            // The lanes stand the height of the board, so each one's cards scroll under its
            // header instead of the whole board growing past the pane. They keep a gutter at
            // the board's sides too: a board scrolled to either end stands its first or last
            // lane a gutter in from the pane's edge, the air it began with before the first.
            row: Row::new()
                .no_wrap()
                .with_align_items(taffy::AlignItems::Stretch)
                .with_padding_rhythm(Some(Rhythm::Group), None),
            lanes: Vec::new(),
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

    /// Add a lane keyed `key`, headed by `header` where it has one, holding `cards` (each
    /// with its key) in order, and `foot` standing at the lane's end under them: what adds to
    /// the lane, always in reach while the cards scroll above it.
    ///
    /// A lane with no heading starts its cards at its top, where a list's first row stands. The
    /// board's owner gives a lane without a name of its own the heading row of the lanes beside
    /// it — an empty one — where they have headings, so every lane's cards line up.
    #[must_use]
    pub fn lane(
        mut self,
        key: impl Into<String>,
        header: Option<Element>,
        cards: Vec<(String, Element)>,
        foot: Option<Element>,
    ) -> Self {
        self.push_lane(key, header, cards, foot);
        self
    }

    /// [`Self::lane`] on a board already built: the lane stands at the board's end.
    pub(crate) fn push_lane(
        &mut self,
        key: impl Into<String>,
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
        // The cards fill the lane under its header and scroll there when they
        // outgrow it, so the header (and what it offers) always stays in
        // reach and a lane holding more cards than the pane shows scrolls
        // instead of losing them.
        let scrolling = Scrollable::new_vertical()
            .fit_content_height()
            .with_child(Box::new(cards_column));
        // A lane takes the board's height where the board has one (a pane's),
        // and its content's where it does not.
        let heading = header.is_some();
        let mut column = Column::new().with_no_padding().with_rhythm(Rhythm::Related);
        if let Some(header) = header {
            column = column.push(header);
        }
        let column = column.push(Box::new(scrolling));
        // What adds to the lane stands at its foot, under the cards, whichever of them shows.
        let column = match foot {
            Some(foot) => column.push(foot),
            None => column,
        };
        // The lane's width is a card's comfortable measure, set at layout
        // from the theme (`Board::size_lanes`).
        let lane = Container::new()
            .with_no_padding()
            .filling()
            .with_width(0.0)
            .with_child(Box::new(column));
        self.row = std::mem::take(&mut self.row).push(Box::new(lane));
        self.lanes.push(LaneKeys {
            key: key.into(),
            cards: keys,
            heading,
        });
    }

    /// Whether the board shows exactly `lanes`: each lane by its key and its cards by theirs,
    /// in order. A board's props given in place name the lanes and cards it shows and carry
    /// none of their views, so props naming other lanes or cards are another board.
    pub(crate) fn holds(&self, lanes: &[(String, Vec<String>)]) -> bool {
        self.lanes.len() == lanes.len()
            && self
                .lanes
                .iter()
                .zip(lanes)
                .all(|(held, (key, cards))| held.key == *key && held.cards == *cards)
    }

    /// The lanes the board shows now, where it stands: what the board is and what the person is
    /// doing with it — a drag in flight — stay.
    pub(crate) fn set_lanes(
        &mut self,
        lanes: Vec<(
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
        self.lanes.clear();
        for (key, header, cards, foot) in lanes {
            self.push_lane(key, header, cards, foot);
        }
        self.dirty = true;
    }

    /// Answer a drop with `on_drop` where it stands.
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

    /// Report a dropped card: through the app's bound action where it bound
    /// one, else to its closure.
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

    /// Whether a drag is going on: the ghost and insertion line show.
    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.drag.as_ref().is_some_and(|drag| drag.active)
    }

    /// Each lane's rect, in lane order.
    pub(crate) fn lane_rects(&self) -> Vec<Rect> {
        self.row
            .children()
            .iter()
            .map(|lane| lane.get_rect())
            .collect()
    }

    /// Each card shown in lane `lane`, with its key, in order: the lane's
    /// header stands above them, and its cards scroll under it.
    fn shown_cards(&self, lane: usize) -> Vec<(String, Rect)> {
        let Some(keys) = self.lanes.get(lane) else {
            return Vec::new();
        };
        let children = self.row.children();
        let Some(container) = children.get(lane) else {
            return Vec::new();
        };
        let band = container.get_rect();
        let lane_children = container.children();
        let Some(column) = lane_children.first() else {
            return Vec::new();
        };
        // The column holds the lane's heading where it has one, then the view that scrolls its
        // cards, whose only child holds them.
        let column_children = column.children();
        let Some(scrolling) = column_children.get(usize::from(keys.heading)) else {
            return Vec::new();
        };
        let scrolled = scrolling.children();
        let Some(cards) = scrolled.first() else {
            return Vec::new();
        };
        cards
            .children()
            .iter()
            .zip(&keys.cards)
            .map(|(card, key)| (key.clone(), card.get_rect()))
            .filter(|(_, card)| card.intersect(band).area() > 0.0)
            .collect()
    }

    /// Each card shown in lane `lane`, in order.
    #[cfg(test)]
    pub(crate) fn card_rects(&self, lane: usize) -> Vec<Rect> {
        self.shown_cards(lane)
            .into_iter()
            .map(|(_, rect)| rect)
            .collect()
    }

    /// The card under `point`: its lane, key and rect.
    fn card_at(&self, point: Point) -> Option<(String, Rect)> {
        (0..self.lanes.len()).find_map(|lane| {
            self.shown_cards(lane)
                .into_iter()
                .find(|(_, rect)| rect.contains(point))
        })
    }

    /// Where a card dragged to `point` lands: the lane whose band holds the
    /// pointer, before the first card whose middle is below it.
    fn target_at(&self, point: Point, dragged: &str) -> Option<(usize, Option<String>)> {
        let lane = self
            .lane_rects()
            .iter()
            .position(|rect| point.x >= rect.x0 && point.x < rect.x1)?;
        let before = self
            .shown_cards(lane)
            .into_iter()
            .filter(|(key, _)| key.as_str() != dragged)
            .find(|(_, rect)| point.y < rect.center().y)
            .map(|(key, _)| key);
        Some((lane, before))
    }

    /// The width a card reads best in a lane (`metrics.board_lane_width`), never below the
    /// measure a card's own text needs at this theme's type size.
    fn comfortable_lane(theme: &Theme) -> f64 {
        f64::from(theme.tokens.metrics.board_lane_width).max(CardWidth::Regular.min_width(theme))
    }

    /// The width a lane takes on a board `width` wide: a lane's comfortable measure, the same
    /// on every board and at every window width, so lanes stand left-aligned with the room a
    /// wider pane leaves open beside them. Only a pane too narrow for one lane and the board's
    /// gutters narrows it, and then to the pane, so a narrow board shows one whole lane per
    /// page rather than one lane and a sliver of the next.
    pub(crate) fn lane_width(theme: &Theme, width: f64) -> f32 {
        let gutter = f64::from(theme.tokens.spacing.rhythm.get(Rhythm::Group));
        let least = f64::from(CardWidth::Regular.min_width(theme));
        let comfortable = Self::comfortable_lane(theme);
        let room = (width - 2.0 * gutter).max(0.0);
        let lane = if room >= comfortable {
            comfortable
        } else {
            room.max(least)
        };
        #[allow(clippy::cast_possible_truncation, reason = "a lane width fits f32")]
        {
            lane as f32
        }
    }

    /// Give every lane's widget this width, which the lane builds itself from.
    fn set_lane_widths(&mut self, width: f32) {
        for lane in self.row.children_mut() {
            if let Some(lane) = lane
                .as_any_mut()
                .and_then(|lane| lane.downcast_mut::<Container>())
            {
                lane.set_width(width);
            }
        }
    }

    /// Give every lane this width, as its widget builds itself and as it already stands.
    fn size_lanes(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        theme: &Theme,
        width: f64,
    ) -> bool {
        let width = Self::lane_width(theme, width);
        if self.sized.is_some_and(|sized| (sized - width).abs() < 0.05) {
            return false;
        }
        self.sized = Some(width);
        self.set_lane_widths(width);
        for &lane in &taffy.children(node).unwrap_or_default() {
            let Ok(mut style) = taffy.style(lane).cloned() else {
                continue;
            };
            style.size.width = taffy::Dimension::length(width);
            let _ = taffy.set_style(lane, style);
        }
        true
    }

    /// Where the insertion line goes for `target`, across its lane.
    fn line_rect(&self, (lane, before): &(usize, Option<String>), theme: &Theme) -> Option<Rect> {
        let band = *self.lane_rects().get(*lane)?;
        let shown = self.shown_cards(*lane);
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
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        if let Some(drops) = &mut self.drops {
            let _ = drops.install_custody(domain, attachment);
        }
        for child in Widget::children_mut(self) {
            crate::widget::controls::install_control_custody(child, domain, attachment);
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

    /// The board answers for the drop it publishes itself (its
    /// [`ActionControls`](crate::widget::controls::ActionControls) reaches
    /// both that action and every row's); everything else the rows answer as
    /// before.
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

    /// A contact on a card holds to lift it; anywhere else it is ordinary content.
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

    /// A lane a page at a time where the pane is too narrow to show two lanes side by side: the
    /// board offers each lane's leading edge, less the air it keeps before the first, so the
    /// view showing it settles a lane at a time. A pane wide enough for its lanes shows them
    /// side by side and offers none: it scrolls freely.
    fn snap_points(&self, axis: crate::widget::ScrollAxis, out: &mut Vec<f32>) {
        if !matches!(axis, crate::widget::ScrollAxis::Horizontal) {
            return;
        }
        let (Some(theme), Some(pane), Some(lane)) = (&self.theme, self.pane, self.sized) else {
            return;
        };
        if f64::from(lane) * 2.0 <= pane {
            return;
        }
        let gutter = f64::from(theme.tokens.spacing.rhythm.get(Rhythm::Group));
        let leading = Widget::get_rect(self).x0;
        out.extend(
            Widget::children(&self.row)
                .iter()
                .map(|lane| crate::num::f32_of_f64(Widget::get_rect(*lane).x0 - leading - gutter)),
        );
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = Some(theme.clone());
        // The lanes build at a lane's comfortable measure, or at the width the pane the board
        // is given settles (`adjust_layout_style`).
        #[allow(clippy::cast_possible_truncation, reason = "a lane width fits f32")]
        let width = self.sized.unwrap_or(Self::comfortable_lane(theme) as f32);
        self.set_lane_widths(width);
        Widget::build_layout(&mut self.row, taffy, theme, node_map)
    }

    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        _text: &mut crate::text_render::TextRenderer,
    ) -> bool {
        let Some(theme) = self.theme.clone() else {
            return false;
        };
        let Ok(layout) = taffy.layout(node) else {
            return false;
        };
        let mut width = f64::from(layout.size.width);
        // A board in a pane that scrolls sideways is as wide as its lanes; the room it shares
        // them out over is the pane it shows through, its parent.
        if let Some(parent) = taffy.parent(node)
            && let Ok(pane) = taffy.layout(parent)
            && pane.size.width > 0.0
        {
            width = width.min(f64::from(pane.size.width));
        }
        self.pane = Some(width);
        self.size_lanes(taffy, node, &theme, width)
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
            // A secondary press ends a pick-up without a drop: a hold lifted where it began
            // is one, and opens the card's menu.
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
                    if let Some((lane, before)) = drag.target
                        && let Some(keys) = self.lanes.get(lane)
                    {
                        let drop = BoardDrop {
                            card: drag.card,
                            lane: keys.key.clone(),
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

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        DrawChild::draw_child(
            &mut self.row,
            scene,
            transform,
            text_renderer,
            theme,
            presence,
        );
        self.dirty = false;
        let Some(drag) = self.drag.as_ref().filter(|drag| drag.active) else {
            return;
        };
        let colors = &theme.tokens.colors.intent;
        let radius = f64::from(theme.tokens.radii.control);
        let line_px = f64::from(theme.tokens.metrics.drag_line_px);
        if let Some(target) = &drag.target {
            if let Some(band) = self.lane_rects().get(target.0) {
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
        clippy::unwrap_used,
        clippy::expect_used,
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
            .lane(
                "todo",
                Some(Box::new(Text::new("Todo")) as Element),
                vec![("a".into(), card()), ("b".into(), card())],
                None,
            )
            .lane(
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

    /// A lane as tall as the pane it is in shows every card it has room for under its heading,
    /// and the ones past that scroll in the view under it rather than being lost.
    #[test]
    fn a_lane_shows_every_card_it_has_room_for() {
        let cards: Vec<(String, Element)> = (0..8)
            .map(|index| (format!("card-{index}"), card()))
            .collect();
        let board = Board::new().lane("todo", Some(Box::new(Text::new("Todo"))), cards, None);
        let host = harness(board);
        let root = host.root::<Board>();
        let shown = root.card_rects(0);
        assert_eq!(
            shown.len(),
            8,
            "eight 60 tall cards stand in a 600 tall pane under their heading, and are all shown"
        );
        let lane = root.lane_rects()[0];
        for (index, rect) in shown.iter().enumerate() {
            assert!(
                rect.y0 >= lane.y0 && rect.y1 <= lane.y1,
                "card {index} stands in its lane: {rect:?} in {lane:?}"
            );
            assert!(
                rect.y0 > lane.y0,
                "card {index} stands below the lane's heading"
            );
        }
        assert!(
            shown.windows(2).all(|pair| pair[0].y1 <= pair[1].y0),
            "the cards are in order, one under the other"
        );
    }

    /// The lanes stand left-aligned at their comfortable measure: the first a gutter in from
    /// the leading edge, each the sibling gap from the last, and the room a wider pane leaves
    /// open beside them at the trailing edge.
    #[test]
    fn a_board_keeps_a_gutter_at_both_ends_of_its_lanes() {
        let theme = Theme::dark();
        let lane = f64::from(Board::lane_width(&theme, 1280.0));
        let gutter = f64::from(theme.tokens.spacing.rhythm.get(Rhythm::Group));
        let gap = f64::from(theme.tokens.spacing.rhythm.get(Rhythm::Sibling));
        let pane = 1280u32;
        let mut lanes = Board::new();
        for key in ["todo", "doing", "done"] {
            let header = Some(Box::new(Text::new(key)) as Element);
            lanes = lanes.lane(key, header, vec![(format!("{key}-card"), card())], None);
        }
        let host =
            crate::headless::FrameHarness::for_root(crate::State::new(), lanes, (pane, 600), theme);
        let root = host.root::<Board>();
        let rects = root.lane_rects();
        assert_eq!(rects.len(), 3, "three lanes");
        let board = root.get_rect();
        assert!(
            (rects[0].x0 - board.x0 - gutter).abs() < 1.0,
            "the first lane stands a gutter in from the leading edge: {rects:?} in {board:?}"
        );
        for (index, rect) in rects.iter().enumerate() {
            assert!(
                (rect.width() - lane).abs() < 1.0,
                "lane {index} keeps its comfortable measure: {rect:?}"
            );
            if let Some(next) = rects.get(index + 1) {
                assert!(
                    (next.x0 - rect.x1 - gap).abs() < 1.0,
                    "lane {index} is a sibling gap from the next: {rects:?}"
                );
            }
        }
        assert!(
            board.x1 - rects[2].x1 > gutter,
            "a wider pane leaves its extra room open beside the lanes: {rects:?} in {board:?}"
        );
    }

    #[test]
    fn dragging_a_card_into_another_lane_reports_one_drop_before_the_card_under_it() {
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
                lane: "done".into(),
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
                lane: "todo".into(),
                before: None,
            }]
        );
    }
}
