//! The cards an app stands over a shell that has one window: where the
//! floating column puts each one, what the app that draws it is told, and
//! which card the pointer, wheel and keys are for.
//!
//! A host with a compositor gives each card a window of its own. A shell with
//! one window (a page, a nested desktop, an app's own page) stands the card
//! itself, and every such shell does it through [`ShellCards`]. A card then
//! stands, draws and takes input in the same place whichever host shows it.

use vello::kurbo::{Point, Rect, Size};

use crate::floating_layer::{Card, Corner, Placement};
use crate::floating_surface::FloatingSurfaceWidgetExt as _;
use crate::floating_surface::{
    FloatingSource, FloatingSurfaceHostMessage, FloatingSurfaceRequest, protocol_rect_array,
};
use crate::live_activity::{Kind, Summary};
use crate::widget::{EventCtx, Widget};
use crate::winit::event::{ElementState, WindowEvent};

/// One card an app presents over the shell.
#[derive(Debug)]
struct PresentedCard {
    /// The presentation's id, as the app names it.
    id: String,
    /// What the card says it is, for the column's order.
    title: String,
    /// The size the app asked the card's body to be.
    size: Size,
    /// Where the app's content stood in its own window when it asked: what
    /// turns the app's pointer coordinates into the shell's.
    local: Point,
    /// The frame around the body that the app does not draw: its shadow
    /// margin.
    margins: Size,
    /// The card's body and its frame, in the shell's coordinates.
    body: Rect,
    window: Rect,
    /// The body the app was last told it stands at, so it is told once.
    told: Option<Rect>,
}

impl PresentedCard {
    /// The card `request` asks the shell to stand.
    fn of(request: &FloatingSurfaceRequest) -> Self {
        let body = request.preferred_size;
        let frame = request.presentation_rect.map_or(body, |rect| rect.size());
        let local = match &request.source {
            FloatingSource::AppElement { region, .. }
            | FloatingSource::AppRegion { region, .. } => region.origin(),
            _ => Point::ZERO,
        };
        Self {
            id: request.id.clone(),
            title: request.title.clone(),
            size: body,
            local,
            margins: Size::new(
                ((frame.width - body.width) / 2.0).max(0.0),
                ((frame.height - body.height) / 2.0).max(0.0),
            ),
            body: Rect::ZERO,
            window: Rect::ZERO,
            told: None,
        }
    }

    /// This card as the floating column reads it: an activity its app draws,
    /// at the size it asked for.
    fn card(&self) -> Card {
        let mut card = Card::new(
            self.id.clone(),
            Summary::new(Kind::Call, self.title.clone(), String::new()).with_id(self.id.clone()),
        );
        card.rich = true;
        card.size = Some(self.size);
        card
    }
}

/// The cards a one-window shell stands over its content, the requests it has
/// not placed yet, and the card the keys go to.
#[derive(Debug, Default)]
pub struct ShellCards {
    shown: Vec<PresentedCard>,
    /// Requests that arrived before the shell knew its area: the first frame
    /// with an area places them.
    asked: Vec<FloatingSurfaceRequest>,
    /// The card the person last pressed: it takes the keys a window's
    /// controls would, until the person presses something else.
    focus: Option<String>,
}

impl ShellCards {
    /// Take the cards `surface` asks for, drop the ones it no longer presents,
    /// stand each where the floating column puts it inside `work` (clear of
    /// `bar`), and tell the app what changed. Once a frame, after the surface
    /// updated and before it draws or takes input. Says whether a card came,
    /// went or moved, so the frame that did it is drawn.
    pub fn follow(&mut self, surface: &mut dyn Widget, work: Rect, bar: Rect) -> bool {
        if work.width() <= 1.0 || work.height() <= 1.0 {
            return false;
        }
        surface.set_floating_surface_bounds(work);
        while let Some(request) = surface.take_floating_surface_request() {
            self.asked.push(request);
        }
        let active = surface.active_floating_surface_ids();
        let mut changed = !self.asked.is_empty();
        // A card the app no longer presents goes back to the app, which draws
        // the activity in its own window again.
        let before = self.shown.len();
        let mut dropped = Vec::new();
        self.shown.retain(|shown| {
            let keeps = active.contains(&shown.id);
            if !keeps {
                dropped.push(shown.id.clone());
            }
            keeps
        });
        changed |= self.shown.len() != before;
        if self
            .focus
            .as_ref()
            .is_some_and(|focus| dropped.contains(focus))
        {
            self.focus = None;
        }
        for id in dropped {
            surface.on_floating_surface_host_message(&FloatingSurfaceHostMessage::Suspended { id });
        }
        for request in std::mem::take(&mut self.asked) {
            if active.contains(&request.id) && !self.shown.iter().any(|card| card.id == request.id)
            {
                self.shown.push(PresentedCard::of(&request));
            }
        }
        self.stand(surface, work, bar) || changed
    }

    /// Stand each card at the place the floating column gives it and tell the
    /// app of a card that moved. Says whether it told any.
    fn stand(&mut self, surface: &mut dyn Widget, work: Rect, bar: Rect) -> bool {
        if self.shown.is_empty() {
            return false;
        }
        let column: Vec<Card> = self.shown.iter().map(PresentedCard::card).collect();
        let placement = Placement {
            work,
            bar,
            banners: None,
            readout: None,
            caret: None,
        };
        let layer = crate::floating_layer::place(&placement, Corner::default(), &column, false);
        let mut told = false;
        for shown in &mut self.shown {
            let Some(placed) = layer.cards.iter().find(|card| card.app == shown.id) else {
                continue;
            };
            shown.body = placed.rect;
            shown.window = shown
                .body
                .inflate(shown.margins.width, shown.margins.height);
            if shown.told == Some(shown.body) {
                continue;
            }
            shown.told = Some(shown.body);
            told = true;
            surface.on_floating_surface_host_message(&FloatingSurfaceHostMessage::PrepareSurface {
                id: shown.id.clone(),
                token: shown.id.clone(),
                rect: protocol_rect_array(shown.window),
                content_rect: protocol_rect_array(shown.body),
                transfer_covered: false,
                coordinate_offset: [shown.body.x0 - shown.local.x, shown.body.y0 - shown.local.y],
            });
            surface.on_floating_surface_host_message(&FloatingSurfaceHostMessage::Adopted {
                id: shown.id.clone(),
                rect: protocol_rect_array(shown.body),
            });
        }
        told
    }

    /// Whether no card stands.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shown.is_empty()
    }

    /// Each card that stands, with its frame in the shell's coordinates, in
    /// the order they draw.
    pub fn windows(&self) -> impl Iterator<Item = (&str, Rect)> {
        self.shown
            .iter()
            .map(|shown| (shown.id.as_str(), shown.window))
    }

    /// The card under `at`, in the shell's coordinates: the topmost one, which
    /// takes the press before the windows under it.
    #[must_use]
    pub fn card_at(&self, at: Point) -> Option<&str> {
        self.shown
            .iter()
            .rev()
            .find(|shown| shown.window.contains(at))
            .map(|shown| shown.id.as_str())
    }

    /// The card `event` is for, with the pointer at `pointer`: the card under
    /// the pointer for a pointer event, the card last pressed for a key. A
    /// press away from every card takes the keys back for the shell.
    pub fn target(&mut self, event: &WindowEvent, pointer: Option<Point>) -> Option<String> {
        let id = match event {
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => self.focus.clone(),
            WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::Touch(_) => pointer.and_then(|at| self.card_at(at).map(str::to_owned)),
            _ => None,
        };
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        ) {
            self.focus.clone_from(&id);
        }
        id
    }

    /// Hand `event` to the card it is for (see [`Self::target`]), through the
    /// app in `surface` that draws it. Answers whether the card took it: the
    /// windows under a card do not see what it took.
    pub fn route(
        &mut self,
        surface: &mut dyn Widget,
        event: &WindowEvent,
        pointer: Option<Point>,
        ctx: &mut EventCtx<'_>,
    ) -> bool {
        let Some(id) = self.target(event, pointer) else {
            return false;
        };
        surface.floating_surface_event(&id, event, ctx)
    }

    /// Draw each card by the app in `surface` that owns it, over what the
    /// shell drew.
    pub fn draw(&self, surface: &mut dyn Widget, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        for shown in &self.shown {
            surface.draw_floating_surface(
                &shown.id,
                &mut crate::draw::DrawCtx::new(scene, transform, text, theme, env.clone()),
            );
        }
    }

    /// The cards as assistive technology reads them: each card's id, the
    /// frame the shell stands it in, and the reading and controls its app
    /// draws there.
    #[must_use]
    pub fn accessibility(
        &self,
        surface: &dyn Widget,
    ) -> Vec<(&str, Rect, crate::a11y::AccessNode)> {
        self.shown
            .iter()
            .filter_map(|shown| {
                surface
                    .floating_surface_accessibility(&shown.id)
                    .map(|node| (shown.id.as_str(), shown.window, node))
            })
            .collect()
    }

    /// Press one of the controls the cards publish, named the way the card's
    /// reading names it: the card whose app takes `target` does.
    pub fn press(
        &self,
        surface: &mut dyn Widget,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> bool {
        self.shown.iter().any(|shown| {
            surface.perform_floating_surface_accessibility_action(
                &shown.id, target, action, state, path,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use taffy::{NodeId, Style, TaffyTree};

    use super::*;
    use crate::Theme;
    use crate::widget::TaffyTreeExt as _;

    /// A surface with one card up, which keeps what the shell told it.
    struct CardSurface {
        told: Rc<RefCell<Vec<FloatingSurfaceHostMessage>>>,
        /// Whether the card is still to be asked for.
        to_ask: bool,
        /// Whether the app still presents the card.
        up: bool,
    }

    impl crate::floating_surface::FloatingSurfaceProvider for CardSurface {
        fn on_floating_surface_host_message(&mut self, message: &FloatingSurfaceHostMessage) {
            self.told.borrow_mut().push(message.clone());
        }

        fn take_floating_surface_request(&mut self) -> Option<FloatingSurfaceRequest> {
            std::mem::take(&mut self.to_ask).then(request)
        }

        fn active_floating_surface_ids(&self) -> Vec<String> {
            if self.up {
                vec![request().id]
            } else {
                Vec::new()
            }
        }
    }

    impl Widget for CardSurface {
        crate::provides!(dyn crate::floating_surface::FloatingSurfaceProvider);

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.widget_leaf(Style::default(), map, self)
        }

        fn name(&self) -> &'static str {
            "CardSurface"
        }
    }

    fn request() -> FloatingSurfaceRequest {
        FloatingSurfaceRequest {
            id: "chat-call:general".into(),
            title: "Just you".into(),
            role: crate::floating_surface::FloatingRole::ActiveCall,
            source: FloatingSource::AppRegion {
                app_id: "chat".into(),
                region: Rect::new(40.0, 60.0, 640.0, 460.0),
            },
            preferred_size: Size::new(320.0, 236.0),
            aspect_ratio: None,
            initial_rect: None,
            presentation_rect: Some(Rect::new(0.0, 0.0, 336.0, 252.0)),
            tracking_source: None,
        }
    }

    #[test]
    fn a_card_stands_in_the_column_and_its_app_is_told_once_where_it_went() {
        let told = Rc::new(RefCell::new(Vec::new()));
        let mut surface = CardSurface {
            told: Rc::clone(&told),
            to_ask: true,
            up: true,
        };
        let work = Rect::new(0.0, 48.0, 1280.0, 800.0);
        let bar = Rect::new(0.0, 0.0, 1280.0, 48.0);
        let mut cards = ShellCards::default();
        assert!(cards.follow(&mut surface, work, bar), "the card came");
        let (_, window) = cards.windows().next().expect("the card stands");
        let body = window.inflate(-8.0, -8.0);
        assert_eq!(
            body.size(),
            Size::new(320.0, 236.0),
            "the size its app asked for"
        );
        assert!(
            work.contains(body.origin()) && body.x1 <= work.x1,
            "the card stands inside the area: {body:?}"
        );
        assert!(
            body.y0 > bar.y1 && body.y1 <= work.y1 - 15.0,
            "the card rests inside the area, off its edge and clear of the bar: {body:?}"
        );
        assert!(
            matches!(
                &told.borrow()[..],
                [
                    FloatingSurfaceHostMessage::PrepareSurface { content_rect, coordinate_offset, .. },
                    FloatingSurfaceHostMessage::Adopted { rect, .. }
                ] if *content_rect == protocol_rect_array(body)
                    && *rect == protocol_rect_array(body)
                    && *coordinate_offset == [body.x0 - 40.0, body.y0 - 60.0]
            ),
            "the app is told the body and where its own content stood: {:?}",
            told.borrow()
        );
        // A shell that repeated the same rect every frame would repaint the
        // card, and on a page every frame.
        assert!(
            !cards.follow(&mut surface, work, bar),
            "an unmoved card is not told again"
        );
        assert_eq!(told.borrow().len(), 2);
    }

    #[test]
    fn keys_follow_the_pressed_card_and_a_card_the_app_drops_is_handed_back() {
        use crate::synthetic_input::{mouse_press, named_key};

        let told = Rc::new(RefCell::new(Vec::new()));
        let mut surface = CardSurface {
            told: Rc::clone(&told),
            to_ask: true,
            up: true,
        };
        let work = Rect::new(0.0, 0.0, 1024.0, 640.0);
        let mut cards = ShellCards::default();
        cards.follow(&mut surface, work, Rect::ZERO);
        let (_, window) = cards.windows().next().expect("the card stands");
        let key = named_key(crate::winit::keyboard::NamedKey::Enter);
        assert_eq!(
            cards.target(&key, None),
            None,
            "no card has the keys before a press"
        );
        assert_eq!(
            cards
                .target(&mouse_press(), Some(window.center()))
                .as_deref(),
            Some("chat-call:general"),
            "a press on the card is the card's"
        );
        assert_eq!(
            cards.target(&key, None).as_deref(),
            Some("chat-call:general")
        );
        assert_eq!(
            cards.target(&mouse_press(), Some(Point::new(1.0, 1.0))),
            None
        );
        assert_eq!(
            cards.target(&key, None),
            None,
            "a press elsewhere takes the keys back"
        );

        surface.up = false;
        assert!(
            cards.follow(&mut surface, work, Rect::ZERO),
            "the card went"
        );
        assert!(cards.is_empty());
        assert!(
            matches!(told.borrow().last(), Some(FloatingSurfaceHostMessage::Suspended { id }) if id == "chat-call:general"),
            "the app draws the activity in its own window again: {:?}",
            told.borrow()
        );
    }
}
