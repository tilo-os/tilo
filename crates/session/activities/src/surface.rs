//! The layer's surface: the activity cards over the desk, their controls,
//! dragging one loose, and the group assistive technology reads.
//!
//! The surface shows the cards the session publishes ([`Link`]) and puts what
//! the person does to them as [`Ask`]s. The frame, the layout, the presses
//! and the accessibility tree are
//! [`tilo_desktop_surface::floating_layer`]'s; this is the widget the session
//! mounts on its own layer, so what it draws is the session's to place and
//! what it asks is the session's to carry out.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_desktop_surface::floating_layer::{Ask, FloatingLayer};
use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow, ResourceSurface, SurfaceCx};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Point, Rect};
use ui::widget::{EventCtx, Widget};
use ui::winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};

use crate::link::Link;
use crate::resource::{Activities, ActivityEffect};

/// The activity cards on screen, holding the conversation with the session
/// that places them.
pub struct ActivitiesSurface {
    state: Rc<RefCell<State>>,
}

struct State {
    link: Link,
    layer: FloatingLayer,
    /// The surface's own rectangle, in its own coordinates.
    rect: Rect,
    /// The generation of the feed shown.
    shown: u64,
    /// Where the pointer last was, in the surface's coordinates: a release
    /// comes without a position of its own.
    pointer: Point,
    /// The frame just drawn is the damage the host presents.
    drawn: bool,
}

impl ActivitiesSurface {
    /// The layer over the feed a session publishes on `link`; without one
    /// (a test, a capture of its own) it shows nothing until one arrives.
    pub fn new(link: Option<Link>) -> Self {
        Self {
            state: Rc::new(RefCell::new(State {
                link: link.unwrap_or_default(),
                layer: FloatingLayer::default(),
                rect: Rect::ZERO,
                shown: 0,
                pointer: Point::ZERO,
                drawn: false,
            })),
        }
    }

    /// The link the layer and its session talk over.
    #[must_use]
    pub fn link(&self) -> Link {
        self.state.borrow().link.clone()
    }
}

impl State {
    /// Show the feed published since the last frame.
    fn catch_up(&mut self) {
        let Some((generation, feed)) = self.link.feed_after(self.shown) else {
            return;
        };
        self.shown = generation;
        self.layer.set(feed.placement, feed.corner, feed.cards);
        self.layer.set_expanded(feed.expanded);
    }

    /// Say what the layer shows, so the session's hit testing and
    /// accessibility read this frame.
    fn hand_over(&self) {
        let shown = self.layer.shown();
        self.link.show(shown);
    }

    fn needs_redraw(&self) -> bool {
        self.drawn || self.layer.needs_redraw() || self.link.generation() != self.shown
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.catch_up();
        self.layer.paint(scene, transform, text_renderer, theme);
        self.drawn = true;
        self.hand_over();
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.catch_up();
        let scale = ctx.scale_factor;
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Point::new(position.x / scale, position.y / scale);
                self.layer.pointer_motion(self.pointer);
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let pressed = *state == ElementState::Pressed;
                let asks = self.layer.pointer_button(self.pointer, pressed);
                if !asks.is_empty() {
                    *ctx.handled = true;
                    for ask in asks {
                        self.link.ask(ask);
                    }
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if let Some(ask) = self.layer.key(logical_key) {
                    *ctx.handled = true;
                    self.link.ask(ask);
                }
            }
            _ => {}
        }
        self.hand_over();
    }

    fn mark_drawn(&mut self) {
        self.drawn = false;
    }

    fn answers(&self, effect: ActivityEffect) {
        let ask = match effect {
            ActivityEffect::Control {
                app,
                activity,
                control,
            } => Ask::Control {
                app,
                activity,
                control,
            },
            ActivityEffect::Close { app, activity } => Ask::Close { app, activity },
            ActivityEffect::Collapse { app, activity } => Ask::Collapse { app, activity },
        };
        self.link.ask(ask);
    }
}

impl Widget for ActivitiesSurface {
    fn name(&self) -> &'static str {
        "Activities"
    }

    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        let style = ui::taffy::Style {
            size: ui::taffy::Size::from_percent(1.0, 1.0),
            ..ui::taffy::Style::default()
        };
        let node = taffy
            .new_leaf(style)
            .unwrap_or_else(|_| ui::taffy::NodeId::from(0_u64));
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(
        &mut self,
        taffy: &ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        origin: Point,
    ) {
        self.state.borrow_mut().rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn get_rect(&self) -> Rect {
        self.state.borrow().rect
    }

    fn sync(&mut self, _state: &ui::widget::AppModel, _path: &ui::widget::ModelPath) {
        self.state.borrow_mut().catch_up();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.state
            .borrow_mut()
            .draw(scene, transform, text_renderer, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.state.borrow_mut().handle_event(event, ctx);
    }

    fn needs_redraw(&self) -> bool {
        self.state.borrow().needs_redraw()
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        let state = self.state.borrow();
        if !state.drawn {
            return;
        }
        let damage = state.layer.damage().intersect(state.rect);
        if !damage.is_zero_area() {
            regions.push(damage);
        }
    }

    fn mark_drawn(&mut self) {
        self.state.borrow_mut().mark_drawn();
    }

    fn wants_pointer_capture(&self) -> bool {
        self.state.borrow().layer.shown().dragging
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.state.borrow().layer.accessibility()
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        _action: &ui::a11y::Action,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ui::widget::ModelPath,
    ) -> bool {
        let state = self.state.borrow_mut();
        let ask = state.layer.activate(target);
        if let Some(ask) = ask {
            state.link.ask(ask);
            state.hand_over();
            return true;
        }
        false
    }

    fn test_rects(&self) -> Vec<(String, Rect)> {
        self.state.borrow().layer.test_rects()
    }
}

impl ResourceSurface<Activities, ActivityEffect> for ActivitiesSurface {
    fn present(&mut self, _state: &Rc<Activities>) {}

    /// A capture, or the app's own window, shows the fixed desktop's
    /// activities: no session lent it any.
    fn stage(&mut self, scenario: Option<&str>) {
        self.link().publish(crate::fixture::feed(scenario));
    }

    fn attach(&mut self, _cx: SurfaceCx<Activities>) {}

    fn effect(
        &mut self,
        effect: ActivityEffect,
        _cx: &mut EffectCx<Activities, Self>,
    ) -> EffectAnswer {
        self.state.borrow_mut().answers(effect);
        Ok(Effected::done())
    }

    /// The cards on screen, each closed through [`ActivityEffect::Close`]:
    /// what an agent reads to find the activity to act on.
    fn live_rows(&self) -> Vec<LiveRow> {
        let state = self.state.borrow();
        state
            .layer
            .layout()
            .cards
            .iter()
            .map(|card| {
                LiveRow::new(
                    format!("activities.card/{}:{}", card.app, card.summary.id),
                    "activities.card",
                    card.summary.title.clone(),
                )
                .effect(&ActivityEffect::Close {
                    app: card.app.clone(),
                    activity: card.summary.id.clone(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::floating_layer::{Card, Corner, Placement};
    use ui::live_activity::{Control, ControlKind, Kind, Summary};

    fn timer(id: &str, title: &str) -> Card {
        let summary = Summary::new(Kind::Timer, title, "1:30")
            .with_id(id)
            .with_countdown(90)
            .with_control(Control::new("pause", "Pause", ControlKind::Primary))
            .with_control(Control::new("close", "End", ControlKind::Close));
        Card::new("so.tilo.clocks", summary)
    }

    fn work(width: f64) -> Rect {
        Rect::new(0.0, 40.0, width, 800.0)
    }

    /// The layer showing `cards` on a display `width` wide, as the session
    /// hands them to it.
    fn hosted(width: f64, cards: Vec<Card>) -> ActivitiesSurface {
        let surface = ActivitiesSurface::new(Some(Link::new()));
        surface.state.borrow_mut().rect = work(width);
        surface.link().publish(crate::link::Feed {
            placement: Placement {
                work: work(width),
                bar: Rect::new(0.0, 0.0, width, 40.0),
                ..Placement::default()
            },
            corner: Corner::BottomRight,
            cards,
            expanded: false,
        });
        surface.state.borrow_mut().catch_up();
        surface
    }

    #[test]
    fn the_sessions_cards_land_where_the_layout_puts_them() {
        let surface = hosted(1280.0, vec![timer("tea", "Tea")]);
        let state = surface.state.borrow();
        assert!(state.needs_redraw(), "a new feed is a frame to paint");
        let cards = &state.layer.layout().cards;
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].summary.title, "Tea");
        let card = cards[0].rect;
        assert!(
            work(1280.0).contains_rect(card.inflate(
                -ui::floating_layer::EDGE_INSET,
                -ui::floating_layer::EDGE_INSET
            )),
            "docked inside the work area: {card:?}"
        );
        assert_eq!(
            state.layer.shown().cards.len(),
            1,
            "said back to the session"
        );
    }

    #[test]
    fn a_card_dragged_loose_is_the_sessions_to_keep() {
        let surface = hosted(1280.0, vec![timer("tea", "Tea")]);
        let card = surface.state.borrow().layer.layout().cards[0].rect;
        let start = card.center();
        surface.state.borrow_mut().layer.pointer_button(start, true);
        surface
            .state
            .borrow_mut()
            .layer
            .pointer_motion(Point::new(start.x - 200.0, start.y - 120.0));
        let asks = surface
            .state
            .borrow_mut()
            .layer
            .pointer_button(Point::new(start.x - 200.0, start.y - 120.0), false);
        assert!(
            matches!(
                asks.as_slice(),
                [Ask::Moved {
                    app,
                    activity,
                    at: Some(_)
                }] if app == "so.tilo.clocks" && activity == "tea"
            ),
            "{asks:?}"
        );
    }

    #[test]
    fn a_control_named_by_assistive_technology_reaches_its_app() {
        let surface = hosted(1280.0, vec![timer("tea", "Tea")]);
        let link = surface.link();
        {
            let state = surface.state.borrow();
            let ask = state
                .layer
                .activate("activity:so.tilo.clocks:tea:pause")
                .expect("the card offers pause");
            state.link.ask(ask);
        }
        assert_eq!(
            link.take_asks(),
            [Ask::Control {
                app: "so.tilo.clocks".into(),
                activity: "tea".into(),
                control: "pause".into(),
            }]
        );
    }

    #[test]
    fn an_action_becomes_an_ask_for_the_session() {
        let surface = hosted(1280.0, vec![timer("tea", "Tea")]);
        surface.state.borrow_mut().answers(ActivityEffect::Close {
            app: "so.tilo.clocks".into(),
            activity: "tea".into(),
        });
        assert_eq!(
            surface.link().take_asks(),
            [Ask::Close {
                app: "so.tilo.clocks".into(),
                activity: "tea".into(),
            }]
        );
    }
}
