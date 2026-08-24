//! [`RemarkHost`] — the "say something about this" surface, and the plumbing
//! every app that offers it needs.
//!
//! [`RemarkCompose`] is the panel. This is everything around it: where the
//! destinations came from, where the panel is placed, which events are its and
//! which belong to the app underneath, and where a finished [`Remark`] waits to
//! be collected.
//!
//! ## Why this is in the toolkit
//!
//! The gesture is not an app feature — anything that can answer
//! [`Referent`](qos_reference::Referent) has it. But adopting it meant copying
//! about a hundred and twenty lines: a throwaway taffy pass, because the panel
//! is anchored in window coordinates rather than being a node of the app's
//! tree; a placement rule; an event pump that gives the panel the keyboard
//! while leaving the pointer shared; and an outbox. The word processor and
//! Photos each hold that copy. Files was written with the panel and *without*
//! the copy — it opened the surface and drew it, and no event ever reached it,
//! because the part that routes events was the part that had to be re-typed.
//!
//! That is the argument for hoisting it: the duplicated half is the half that
//! makes the panel work, and a widget whose behaviour lives in its callers is a
//! widget that silently does nothing in the caller that forgets a piece.
//!
//! ## Who owns what
//!
//! The host owns the panel's *lifecycle*. The app still owns its own subject
//! (it answers [`Referent`]), its own chrome (the context menu the panel sits
//! beside) and its own feedback (the toast that says where the remark went).
//! Nothing here draws anything the app did not ask it to draw.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};

use qos_reference::{Destination, Referent, Remark, ResourceRef};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;
use crate::widget::remark_compose::{
    BESIDE_GAP, DestinationOption, RemarkCompose, placeholder_text, send_label,
};

/// The compose surface and everything an app needs to run it.
#[derive(Default)]
pub struct RemarkHost {
    compose: RemarkCompose,
    /// Where a remark from this app can go. Supplied by the host that knows the
    /// room — see
    /// [`Widget::set_remark_destinations`](crate::widget::Widget::set_remark_destinations).
    /// Empty until it is, so an app never invents a roster of its own.
    destinations: Vec<DestinationOption>,
    /// A finished remark waiting to be collected by
    /// [`Widget::take_remark`](crate::widget::Widget::take_remark).
    outbox: Option<Remark>,
    /// Whether the remark in the outbox has been announced to the app yet.
    ///
    /// Separate from the outbox because two different parties want it and only
    /// one of them may consume it: the app wants to say where the message went
    /// (a toast, a log line), and the host wants to put it on the wire. An app
    /// that took it to render its own confirmation would leave the host nothing
    /// to send — a message that looks delivered and was never delivered, which
    /// is the worst failure this surface has.
    fresh: bool,
}

impl RemarkHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take the room's roster as the places a remark can be sent.
    pub fn set_destinations(&mut self, destinations: Vec<DestinationOption>) {
        self.destinations = destinations;
    }

    /// Whether the room has told this app anywhere to send yet.
    ///
    /// An app checks this before offering the gesture: a compose surface with
    /// an empty selector is a box that cannot send, and offering one is worse
    /// than not offering it.
    pub fn has_destinations(&self) -> bool {
        !self.destinations.is_empty()
    }

    /// Open the surface for whatever `referent` has selected, beside
    /// `neighbour` (normally the context menu) and inside `bounds`.
    ///
    /// Returns `false`, having opened nothing, when there is nowhere to send or
    /// nothing selected — a compose box over no subject would be asking for a
    /// sentence about nothing.
    pub fn open_beside(
        &mut self,
        referent: &dyn Referent,
        doc: &AutoCommit,
        neighbour: Rect,
        bounds: Rect,
    ) -> bool {
        let Some(reference) = referent.selection_reference(doc) else {
            return false;
        };
        self.open_reference_beside(reference, neighbour, bounds)
    }

    /// Open the surface for an explicit `reference`, beside `neighbour`.
    ///
    /// The address is passed in rather than resolved from a selection, for an
    /// app that already knows exactly which object the gesture was made on.
    pub fn open_reference_beside(
        &mut self,
        reference: ResourceRef,
        neighbour: Rect,
        bounds: Rect,
    ) -> bool {
        if self.destinations.is_empty() {
            return false;
        }
        self.compose.open_for(
            reference,
            self.destinations.clone(),
            0,
            Point::new(neighbour.x1 + BESIDE_GAP, neighbour.y0),
            &placeholder_text(),
            &send_label(),
        );
        // Measure, then place, then measure again: the panel's size is only
        // known once it has been laid out, and its size is what decides where
        // it fits.
        self.layout();
        self.compose.place_beside(neighbour, bounds);
        self.layout();
        true
    }

    pub fn is_open(&self) -> bool {
        self.compose.is_open()
    }

    pub fn close(&mut self) {
        self.compose.close();
    }

    /// The panel's rect, as laid out. Zero-area while it is closed.
    pub fn rect(&self) -> Rect {
        self.compose.get_rect()
    }

    /// What the open panel is about.
    pub fn reference(&self) -> Option<&ResourceRef> {
        self.compose.reference()
    }

    /// The destination the selector is showing, and therefore the one a remark
    /// sent now would go to.
    pub fn destination(&self) -> Option<&Destination> {
        self.compose.destination()
    }

    /// The text typed so far.
    pub fn text(&self) -> &str {
        self.compose.text()
    }

    /// Look at the remark just composed, once, without taking it.
    ///
    /// For the app's own feedback. Answers on the first call after a send and
    /// `None` thereafter, so a confirmation is shown once however many times
    /// the app is asked to handle an event before the host's next drain.
    pub fn just_sent(&mut self) -> Option<&Remark> {
        if std::mem::take(&mut self.fresh) {
            self.outbox.as_ref()
        } else {
            None
        }
    }

    /// Collect a finished remark, once.
    ///
    /// The counterpart to [`set_destinations`](Self::set_destinations): the
    /// room says where a remark may go, and this is how the finished one gets
    /// back out. `None` on every frame but the one after a send.
    pub fn take_remark(&mut self) -> Option<Remark> {
        self.outbox.take()
    }

    /// Draw the panel, if it is open.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.compose.is_open() {
            return;
        }
        self.layout();
        self.compose
            .draw(scene, transform, text_renderer, theme, presence);
        self.compose
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    /// Lay the panel out where it is anchored.
    ///
    /// Public because an app that places the panel by a rule of its own —
    /// Photos keeps the photograph clear rather than following the general
    /// order — has to measure between its own two placement steps.
    pub fn layout(&mut self) {
        if self.compose.is_open() {
            let _ = self.layout_pass();
        }
    }

    /// Move the panel, for an app placing it by a rule of its own.
    pub fn move_to(&mut self, at: Point) {
        self.compose.move_to(at);
    }

    /// Resolve the panel's rects in a throwaway taffy tree: it is anchored in
    /// window coordinates rather than being a node of the app's tree.
    /// Deliberately the same pass for painting and for hit-testing, so a
    /// control can never be drawn somewhere other than where it is clickable.
    fn layout_pass(&mut self) -> (TaffyTree<()>, NodeId) {
        // The panel's own metrics come from its constants, and spacing tokens
        // do not vary by palette, so one theme here keeps draw and hit-test
        // measuring with the same ruler under every theme.
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = self.compose.build_layout(&mut taffy, &theme, &mut nm);
        let _ = taffy.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(2000.0),
                height: taffy::AvailableSpace::Definite(1400.0),
            },
        );
        self.compose.apply_layout(&taffy, node, Point::ZERO);
        (taffy, node)
    }

    /// While the surface is open, route input to it. Returns whether the event
    /// was consumed.
    ///
    /// The panel owns the keyboard — it is a field that just appeared under the
    /// hands, and a keystroke meant for the message must never land in the app
    /// instead. The pointer stays shared: a press inside the panel is the
    /// panel's, a press on `neighbour` (the menu beside it) still works that
    /// menu, and a press anywhere else puts the compose away the way clicking
    /// off any composer does.
    ///
    /// `pointer` is the app's current pointer position, in the same logical
    /// space as [`rect`](Self::rect). `neighbour` is the rect of the chrome the
    /// panel was opened beside, so a click on it is not read as a click away;
    /// pass `None` when there is none.
    #[allow(clippy::too_many_arguments)]
    pub fn pump(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        mods: &Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        pointer: Point,
        neighbour: Option<Rect>,
    ) -> bool {
        if !self.compose.is_open() {
            return false;
        }
        match event {
            WindowEvent::CursorMoved { .. } => {
                // Hover feedback only. A move is never reported as consumed, or
                // the rest of the app stops tracking the pointer while the
                // panel is up — and the send button needs the hover it carries.
                let mut hover = false;
                self.forward(event, scale, mods, doc, path, &mut hover);
                false
            }
            WindowEvent::KeyboardInput { .. } => {
                self.forward(event, scale, mods, doc, path, handled);
                *handled = true;
                true
            }
            WindowEvent::MouseInput {
                button: MouseButton::Left,
                state: press,
                ..
            } => {
                // The expanded destination list paints its rows outside the
                // panel's own rect, so while it is down every press is the
                // panel's — including the one that dismisses it.
                let mine = self.compose.get_rect().contains(pointer)
                    || self.compose.destination_list_is_open();
                if mine {
                    self.forward(event, scale, mods, doc, path, handled);
                    *handled = true;
                    return true;
                }
                if *press == ElementState::Pressed
                    && !neighbour.is_some_and(|n| n.contains(pointer))
                {
                    self.compose.close();
                }
                false
            }
            // A wheel over the panel is the panel's, or the app scrolls out
            // from under a composer somebody is typing into.
            WindowEvent::MouseWheel { .. } if self.compose.get_rect().contains(pointer) => {
                *handled = true;
                true
            }
            _ => false,
        }
    }

    /// Hand one event to the open panel and collect anything it finished.
    fn forward(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        mods: &Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) {
        let (taffy, node) = self.layout_pass();
        let mut ft: Option<NodeId> = None;
        self.compose.handle_event(
            event, scale, mods, doc, path, handled, &mut ft, node, &taffy,
        );
        if let Some(remark) = self.compose.take_sent() {
            self.outbox = Some(remark);
            self.fresh = true;
        }
    }
}
