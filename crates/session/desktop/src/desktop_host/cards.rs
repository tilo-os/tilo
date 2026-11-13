//! The cards the desk's apps present over it: the place the desk gives each
//! one, and the input and reading that follow from that place.
//!
//! A host with a compositor hands an app's card a window of its own. The desk
//! has one window, so it draws the card itself, at the place its floating
//! column gives it, and hands the app the events and the reading that follow
//! from there. The app draws the card either way: the desk only decides where
//! it stands and carries what the app paints there.

use ui::floating_layer::{Card, Corner, Placement};
use ui::floating_surface::{
    FloatingSurfaceHostMessage, FloatingSurfaceRequest, protocol_rect_array,
};
use ui::live_activity::{Kind, Summary};
use ui::presence::PresenceCtx;
use ui::widget::{EventCtx, FloatingSurfaceWidgetExt as _, Widget};
use ui::{TextRenderer, Theme};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Size};
use winit::event::WindowEvent;

use super::DeskLayer;

/// One card an app presents over the desk.
struct Shown {
    /// The presentation's id, as the app names it.
    id: String,
    /// What the card says it is, for the column's order.
    title: String,
    /// The size the app asked the card's body to be.
    size: Size,
    /// Where the app's content stood in its own window when it asked: what
    /// turns the app's pointer coordinates into the desk's.
    local: Point,
    /// The frame around the body that the app does not draw: its shadow
    /// margin.
    margins: Size,
    /// The card's body and its frame, in the desk's coordinates.
    body: Rect,
    window: Rect,
    /// The body the app was last told it stands at, so it is told once.
    told: Option<Rect>,
}

/// What the desk shows over its windows, and the asks it has not placed yet.
#[derive(Default)]
pub struct Cards {
    shown: Vec<Shown>,
    /// The requests that arrived before the desk knew its area: the frame
    /// after the first layout places them.
    asked: Vec<FloatingSurfaceRequest>,
}

impl Shown {
    /// The card `request` asks the desk to stand.
    fn of(request: &FloatingSurfaceRequest) -> Self {
        let (size, margins) = ask(request);
        let local = match &request.source {
            ui::floating_surface::FloatingSource::AppElement { region, .. }
            | ui::floating_surface::FloatingSource::AppRegion { region, .. } => region.origin(),
            _ => Point::ZERO,
        };
        Self {
            id: request.id.clone(),
            title: request.title.clone(),
            size,
            local,
            margins,
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

/// The body `request` asks for, and the frame it carries around that body.
fn ask(request: &FloatingSurfaceRequest) -> (Size, Size) {
    let body = request.preferred_size;
    let frame = request.presentation_rect.map_or(body, |rect| rect.size());
    (
        body,
        Size::new(
            ((frame.width - body.width) / 2.0).max(0.0),
            ((frame.height - body.height) / 2.0).max(0.0),
        ),
    )
}

/// Stand each card `cards` holds at the place the desk's floating column
/// gives it, and tell the app that draws a card where it went. Says whether
/// the desk told an app of a moved card.
fn stand(cards: &mut Cards, surface: &mut dyn Widget, work: Rect, bar: Rect) -> bool {
    let mut told = false;
    let column: Vec<Card> = cards.shown.iter().map(Shown::card).collect();
    let placement = Placement {
        work,
        bar,
        banners: None,
        readout: None,
        caret: None,
    };
    let layer = ui::floating_layer::place(&placement, Corner::default(), &column, false);
    for shown in &mut cards.shown {
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

impl DeskLayer {
    /// The area the windows stand in, under the bar: a card the desk places
    /// stands inside it.
    #[must_use]
    pub fn area_rect(&self) -> Rect {
        self.area.get_rect()
    }

    /// Stand each card where the desk's floating column puts it, tell the app
    /// what it did, and drop the cards the app no longer presents. Once a
    /// frame, after the surface updated and before it draws. Says whether the
    /// desk's cards changed, so a frame that moved one is drawn.
    pub fn follow_cards(&mut self) -> bool {
        let work = self.area_rect();
        if work.width() <= 1.0 || work.height() <= 1.0 {
            return false;
        }
        let bar = self.band.get_rect();
        let Some(surface) = self.area.children_mut().pop() else {
            return false;
        };
        surface.set_floating_surface_bounds(work);
        while let Some(request) = surface.take_floating_surface_request() {
            self.cards.asked.push(request);
        }
        let active = surface.active_floating_surface_ids();
        let mut moved = !self.cards.asked.is_empty();
        // A card the app no longer presents goes back to the app, which draws
        // the activity in its own window again.
        let mut dropped = Vec::new();
        let shown_before = self.cards.shown.len();
        self.cards.shown.retain(|shown| {
            let keeps = active.iter().any(|id| id == &shown.id);
            if !keeps {
                dropped.push(shown.id.clone());
            }
            keeps
        });
        for id in dropped {
            surface.on_floating_surface_host_message(&FloatingSurfaceHostMessage::Suspended { id });
        }
        moved |= self.cards.shown.len() != shown_before;
        for request in std::mem::take(&mut self.cards.asked) {
            if !active.iter().any(|id| id == &request.id) {
                continue;
            }
            self.cards.shown.push(Shown::of(&request));
        }
        stand(&mut self.cards, surface, work, bar) || moved
    }

    /// Draw the cards the desk stands over its windows, by the apps that own
    /// them, once the surface has drawn.
    pub fn draw_cards(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let Some(surface) = self.area.children_mut().pop() else {
            return;
        };
        for shown in &self.cards.shown {
            surface.draw_floating_surface(&shown.id, scene, transform, text, theme, presence);
        }
    }

    /// The card under `at`, in the desk's coordinates: what takes the press
    /// before the windows under it do.
    fn card_under(&self, at: Point) -> Option<String> {
        self.cards
            .shown
            .iter()
            .rev()
            .find(|shown| shown.window.contains(at))
            .map(|shown| shown.id.clone())
    }

    /// Deliver one input event aimed at the card `id`, in the desk's
    /// coordinates, to the app that draws it. Answers whether the card took
    /// it.
    pub fn card_event(&mut self, id: &str, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        let path = ctx.path.clone();
        // The desk's third layer is the area, and the area's top layer is the
        // surface: the card's input is forwarded with the node it draws at.
        let Some(area) = ctx
            .taffy
            .children(ctx.node_id)
            .ok()
            .and_then(|nodes| nodes.get(2).copied())
        else {
            return false;
        };
        let Some(node) = ctx
            .taffy
            .children(area)
            .ok()
            .and_then(|nodes| nodes.last().copied())
        else {
            return false;
        };
        let Some(surface) = self.area.children_mut().pop() else {
            return false;
        };
        surface.floating_surface_event(id, event, &mut ctx.reborrow(&path, node))
    }

    /// The desktop's points of the cards `at` stands on, if any: a press over
    /// a card is the card's, not the window's under it.
    #[must_use]
    pub fn card_at(&self, at: Point) -> Option<String> {
        self.card_under(at)
    }

    /// The cards as assistive technology reads them: each the reading and the
    /// controls its app draws, where the desk stands it.
    #[must_use]
    pub fn cards_accessibility(&self) -> Vec<ui::a11y::AccessNode> {
        let Some(surface) = self.area.children().pop() else {
            return Vec::new();
        };
        self.cards
            .shown
            .iter()
            .filter_map(|shown| surface.floating_surface_accessibility(&shown.id))
            .collect()
    }
}

impl Cards {
    /// Press one of the controls the cards publish, named the way the card's
    /// reading names it: the card whose app takes `target` does.
    pub fn press(
        &self,
        surface: &mut dyn Widget,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ui::automerge::ObjId,
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
    use ui::widget::TaffyTreeExt;

    /// A surface with one card up, which keeps what the desk told it.
    struct CardSurface {
        told: Rc<RefCell<Vec<FloatingSurfaceHostMessage>>>,
    }

    impl ui::widget::FloatingSurfaceProvider for CardSurface {
        fn on_floating_surface_host_message(&mut self, message: &FloatingSurfaceHostMessage) {
            self.told.borrow_mut().push(message.clone());
        }
    }

    impl Widget for CardSurface {
        ui::provides!(dyn ui::widget::FloatingSurfaceProvider);

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
            role: ui::floating_surface::FloatingRole::ActiveCall,
            source: ui::floating_surface::FloatingSource::AppRegion {
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
    fn the_desk_stands_a_card_in_its_column_and_tells_the_app_where_it_went() {
        let told = Rc::new(RefCell::new(Vec::new()));
        let mut surface = CardSurface {
            told: Rc::clone(&told),
        };
        let work = Rect::new(0.0, 48.0, 1280.0, 800.0);
        let bar = Rect::new(0.0, 0.0, 1280.0, 48.0);
        let mut cards = Cards {
            shown: vec![Shown::of(&request())],
            asked: Vec::new(),
        };
        assert!(
            stand(&mut cards, &mut surface, work, bar),
            "the desk tells the app its card moved"
        );
        let body = cards.shown[0].body;
        assert_eq!(
            body.size(),
            Size::new(320.0, 236.0),
            "the card stands at the size its app asked for"
        );
        assert!(
            body.x1 <= work.x1 && body.y1 <= work.y1 && body.x0 >= work.x0 && body.y0 >= work.y0,
            "the card stands inside the desk's area: {body:?} in {work:?}"
        );
        assert!(
            body.y1 <= work.y1 - 15.0 && body.y0 > bar.y1,
            "the card rests off the work area's edge and clear of the bar: {body:?}"
        );
        let messages = told.borrow();
        assert!(
            matches!(
                &messages[..],
                [
                    FloatingSurfaceHostMessage::PrepareSurface {
                        content_rect,
                        coordinate_offset,
                        ..
                    },
                    FloatingSurfaceHostMessage::Adopted { rect, .. }
                ] if *content_rect == protocol_rect_array(body)
                    && *rect == protocol_rect_array(body)
                    && *coordinate_offset
                        == [body.x0 - 40.0, body.y0 - 60.0]
            ),
            "the app is told the card's body and where its own content stood: {messages:?}"
        );
        drop(messages);
        // Told once: a desk that repeats the same rect every frame would
        // repaint the card's window and, on a page, every frame.
        assert!(
            !stand(&mut cards, &mut surface, work, bar),
            "a card that has not moved is not told again"
        );
        assert_eq!(told.borrow().len(), 2);
    }
}
