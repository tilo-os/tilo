//! The cards a page's apps stand over it: where the floating column puts each,
//! what the app that draws it is told, and the pointer, hover and keys that
//! are the card's rather than the window's under it.
//!
//! A native desktop with a compositor hands an app's card a window of its own
//! (`tilo_desktop::desktop_host::cards`). A page has one window, so its frame
//! stands the card itself, draws it through the app, and routes what lands on
//! it — the same arithmetic, so a page and a desktop put a card in the same
//! place and a press reaches the same control.

use super::*;

/// One card the root presents over it, as the frame stands it.
pub(super) struct StandingCard {
    /// The presentation's id, as the app names it.
    id: String,
    /// What the card says it is, for the floating column's order.
    title: String,
    /// The size the app asked the card's body to be.
    size: vello::kurbo::Size,
    /// The frame around the body that the app does not draw: its shadow margin.
    margins: vello::kurbo::Size,
    /// Where the app's content stood in its own window when it asked: what
    /// turns the app's pointer coordinates into the frame's.
    local: vello::kurbo::Point,
    /// The card's body and its frame, in the frame's coordinates.
    body: vello::kurbo::Rect,
    window: vello::kurbo::Rect,
    /// The body the app was last told it stands at, so it is told once.
    told: Option<vello::kurbo::Rect>,
}

impl StandingCard {
    fn of(request: &crate::floating_surface::FloatingSurfaceRequest) -> Self {
        let body = request.preferred_size;
        let frame = request.presentation_rect.map_or(body, |rect| rect.size());
        let margins = vello::kurbo::Size::new(
            ((frame.width - body.width) / 2.0).max(0.0),
            ((frame.height - body.height) / 2.0).max(0.0),
        );
        let local = match &request.source {
            crate::floating_surface::FloatingSource::AppElement { region, .. }
            | crate::floating_surface::FloatingSource::AppRegion { region, .. } => region.origin(),
            _ => vello::kurbo::Point::ZERO,
        };
        Self {
            id: request.id.clone(),
            title: request.title.clone(),
            size: body,
            margins,
            local,
            body: vello::kurbo::Rect::ZERO,
            window: vello::kurbo::Rect::ZERO,
            told: None,
        }
    }

    fn column_card(&self) -> crate::floating_layer::Card {
        let mut card = crate::floating_layer::Card::new(
            self.id.clone(),
            crate::live_activity::Summary::new(
                crate::live_activity::Kind::Call,
                self.title.clone(),
                String::new(),
            )
            .with_id(self.id.clone()),
        );
        card.rich = true;
        card.size = Some(self.size);
        card
    }
}

impl FrameHarness {
    /// The ids of the cards this frame stands, in the order they draw.
    pub(super) fn standing_ids(&self) -> Vec<String> {
        self.cards.iter().map(|card| card.id.clone()).collect()
    }

    /// Stand the cards the root presents where the floating column gives
    /// them, and tell their apps where they went — once a frame, before it
    /// draws and routes input. The same arithmetic the native desktop's cards
    /// use, so a page stands, draws and routes a card where a desktop does.
    pub(super) fn follow_cards(&mut self) {
        let work = vello::kurbo::Rect::new(
            0.0,
            0.0,
            f64::from(self.viewport.0),
            f64::from(self.viewport.1),
        );
        if work.width() <= 1.0 || work.height() <= 1.0 {
            return;
        }
        while let Some(request) =
            crate::widget::FloatingSurfaceWidgetExt::take_floating_surface_request(
                self.root.as_mut(),
            )
        {
            self.cards_asked.push(request);
        }
        let active = crate::widget::FloatingSurfaceWidgetExt::active_floating_surface_ids(
            self.root.as_ref(),
        );
        self.cards.retain(|card| active.contains(&card.id));
        for request in std::mem::take(&mut self.cards_asked) {
            if active.contains(&request.id) && !self.cards.iter().any(|card| card.id == request.id)
            {
                self.cards.push(StandingCard::of(&request));
            }
        }
        if self.cards.is_empty() {
            return;
        }
        let column: Vec<crate::floating_layer::Card> =
            self.cards.iter().map(StandingCard::column_card).collect();
        let placement = crate::floating_layer::Placement {
            work,
            bar: vello::kurbo::Rect::new(0.0, 0.0, work.width(), 0.0),
            banners: None,
            readout: None,
            caret: None,
        };
        let layer = crate::floating_layer::place(
            &placement,
            crate::floating_layer::Corner::default(),
            &column,
            false,
        );
        let mut messages = Vec::new();
        for card in &mut self.cards {
            let Some(placed) = layer.cards.iter().find(|placed| placed.app == card.id) else {
                continue;
            };
            card.body = placed.rect;
            card.window = card.body.inflate(card.margins.width, card.margins.height);
            if card.told == Some(card.body) {
                continue;
            }
            card.told = Some(card.body);
            messages.push(
                crate::floating_surface::FloatingSurfaceHostMessage::PrepareSurface {
                    id: card.id.clone(),
                    token: card.id.clone(),
                    rect: crate::floating_surface::protocol_rect_array(card.window),
                    content_rect: crate::floating_surface::protocol_rect_array(card.body),
                    transfer_covered: false,
                    coordinate_offset: [card.body.x0 - card.local.x, card.body.y0 - card.local.y],
                },
            );
            messages.push(
                crate::floating_surface::FloatingSurfaceHostMessage::Adopted {
                    id: card.id.clone(),
                    rect: crate::floating_surface::protocol_rect_array(card.body),
                },
            );
        }
        for message in messages {
            crate::widget::FloatingSurfaceWidgetExt::on_floating_surface_host_message(
                self.root.as_mut(),
                &message,
            );
        }
        crate::widget::FloatingSurfaceWidgetExt::set_floating_surface_bounds(
            self.root.as_mut(),
            work,
        );
    }

    /// The card the shell stands over its windows under `at`: the topmost of
    /// what [`crate::widget::FloatingSurfaceWidgetExt::floating_surface_presentations`]
    /// names, in the coordinates the shell placed it at.
    pub(super) fn card_at(&self, at: vello::kurbo::Point) -> Option<String> {
        self.cards
            .iter()
            .rev()
            .find(|card| card.window.contains(at))
            .map(|card| card.id.clone())
    }

    /// Deliver `event` to the card the person is on — the one under the
    /// pointer, or the one holding the press for keys — so a card's controls
    /// answer a press, hover and typing as a window's do. Answers whether a
    /// card took it: the windows under the card do not see the event.
    pub(super) fn dispatch_to_card(
        &mut self,
        event: &WindowEvent,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
    ) -> bool {
        let pressed = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        );
        let under = match event {
            WindowEvent::CursorMoved { position, .. } => {
                let at = Point::new(position.x / self.input_scale, position.y / self.input_scale);
                // A press that follows without a move of its own is where the
                // pointer last was.
                self.pointer.at = Some(at);
                Some(at)
            }
            WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::Touch(_) => self.pointer.at,
            _ => None,
        };
        let id = match event {
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => self.card_focus.clone(),
            _ => under.and_then(|at| self.card_at(at)),
        };
        if pressed && id.is_none() {
            // A press anywhere else takes the keyboard back from the card.
            self.card_focus = None;
            return false;
        }
        let Some(id) = id else {
            return false;
        };
        if pressed {
            self.card_focus = Some(id.clone());
        }
        crate::widget::FloatingSurfaceWidgetExt::floating_surface_event(
            self.root.as_mut(),
            &id,
            event,
            &mut EventCtx {
                scale_factor: self.input_scale,
                modifiers: &self.modifiers,
                state: crate::widget::Edits::lend(&mut self.state.doc),
                path: &self.state.root,
                handled,
                focus_target,
                node_id: self.child_node,
                taffy: &self.taffy,
            },
        )
    }

    /// The cards the shell stands over its windows, as a reader finds them:
    /// each the reading and controls its app draws, at the rectangle the shell
    /// placed it. The shell draws a card with its app's own paint, so its
    /// controls own no retained widget and the tree names them here.
    pub(super) fn floating_surface_inspection(&self) -> Vec<InspectionNode> {
        let mut out = Vec::new();
        for standing in &self.cards {
            let Some(semantics) =
                crate::widget::FloatingSurfaceWidgetExt::floating_surface_accessibility(
                    self.root.as_ref(),
                    &standing.id,
                )
            else {
                continue;
            };
            let window = standing.window;
            out.push(InspectionNode {
                path: format!("floating:{}", standing.id),
                widget: "FloatingSurface".into(),
                test_id: None,
                focusable: false,
                rect: [window.x0, window.y0, window.width(), window.height()],
                child_clip: None,
                page_title: None,
                layout: None,
                paint_key: None,
                semantics: Some(semantics),
                children: Vec::new(),
            });
        }
        out
    }
}
