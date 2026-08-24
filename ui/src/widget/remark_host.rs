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
//! The host owns the panel's *lifecycle*. Whoever embeds this supplies the
//! subject (something that answers [`Referent`]), the rect to sit beside, and
//! the bounds to stay inside; nothing here draws anything it was not asked to.
//!
//! ## The host should be the compositor, not the app
//!
//! This type is embedded by three applications today and that is the wrong
//! home for it. Two defects follow from it directly, and they are the same
//! defect seen from two sides:
//!
//! * **It is clipped.** A Wayland client cannot draw outside its own surface,
//!   so a panel an app paints stops at that app's window edge. "Beside" is the
//!   entire design of this surface — [`BESIDE_GAP`], the right-then-below-then-
//!   left rule in [`RemarkCompose::place_beside`] — and beside a context menu
//!   near a window edge is *outside the window*. On a strip of tiled windows
//!   that is most of the time, so the panel is sliced off mid-word with the
//!   next application visible past the cut.
//! * **It is inert unless every app remembers to route to it.** The panel is
//!   not a node of the app's layout tree — it is anchored in window
//!   coordinates and laid out in a tree of its own — so an app that draws it
//!   without also pumping events at it gets a surface that paints perfectly
//!   and cannot be focused, typed into, or clicked. That is not hypothetical:
//!   it is exactly the state Files shipped in.
//!
//! Both dissolve if the compositor owns it. It is chrome by this project's own
//! rule — the compositor draws window decorations, not the app — and a surface
//! that reads the room's roster and carries a resource address *between*
//! applications is chrome by the same reasoning. The compositor can draw above
//! neighbouring windows, it already holds the roster, and it is on the room's
//! side of the boundary, so a composed remark reaches the room without
//! crossing back out through a client.
//!
//! The split that leaves is the honest one: the **app** supplies the subject
//! and where its menu is, and the **compositor** draws it, places it, owns the
//! keyboard, and sends it. This type is the compositor's half of that, and
//! nothing in it assumes otherwise — it is `ui::Widget` machinery, and the
//! shell's own tree is built out of `ui::Widget`s.

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

/// An app asking for the compose surface, rather than drawing one.
///
/// The whole of what crosses from an application to the compositor when
/// somebody says "say something about this": **which object**, and **what to
/// sit beside**. Nothing about how the panel looks, who is in the room, or
/// where on a screen it should end up — those are the compositor's answers,
/// and an app supplying them would be supplying answers it does not have.
///
/// The address is the point. `reference` is the object's own id, resolved by
/// the app that owns it, so a sentence composed about it still means the same
/// thing on another machine and in a session opened tomorrow. A description of
/// where it sat on somebody's screen would not.
#[derive(Clone, Debug, PartialEq)]
pub struct RemarkRequest {
    /// The address of the thing being talked about.
    pub reference: ResourceRef,
    /// The chrome to sit beside — normally the context menu the same gesture
    /// opened — **in the app's own coordinate space**.
    ///
    /// The app reports where its own menu is and stops there. Turning that
    /// into a place on a desktop needs to know where this window sits, which
    /// display it is on, and what is next to it; the app knows none of the
    /// three. That asymmetry is the whole reason this surface moved. "Beside
    /// the menu" is a position on the desktop, and an app can only ever name
    /// one inside itself.
    pub beside: Rect,
}

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
    /// Whether something outside this process draws the panel.
    ///
    /// Set by the host when the application is running under a compositor that
    /// draws this surface itself. Opening then produces a [`RemarkRequest`] and
    /// no panel of this app's own: two panels for one gesture — one clipped at
    /// the window edge and one not — is worse than either.
    delegated: bool,
    /// A request waiting to be collected by
    /// [`Widget::take_remark_request`](crate::widget::Widget::take_remark_request).
    asked: Option<RemarkRequest>,
}

impl RemarkHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take the room's roster as the places a remark can be sent.
    pub fn set_destinations(&mut self, destinations: Vec<DestinationOption>) {
        self.destinations = destinations;
    }

    /// Hand the surface to whatever is drawing this app's chrome.
    ///
    /// From here on the gesture produces a [`RemarkRequest`] and nothing is
    /// drawn in this process. Called by the host when the application is
    /// running under a compositor that hosts the panel — which is the only
    /// arrangement in which "beside the menu" can be honoured, because beside
    /// a menu near a window's edge is outside the window.
    ///
    /// It also settles the destinations question. A delegated app is never
    /// given a roster and never needs one: whoever draws the panel fills its
    /// selector from the room it already holds. So [`has_destinations`] stops
    /// being the gate on the gesture — see [`can_open`].
    ///
    /// [`has_destinations`]: Self::has_destinations
    /// [`can_open`]: Self::can_open
    pub fn delegate(&mut self) {
        self.delegated = true;
    }

    /// Whether the panel is drawn somewhere other than this process.
    pub fn is_delegated(&self) -> bool {
        self.delegated
    }

    /// Whether the gesture can go anywhere at all.
    ///
    /// Two different questions with one answer, because they are the same
    /// question asked of the two homes this surface has: an app that draws the
    /// panel itself needs somewhere to send, and an app that asks for one needs
    /// something to ask. Apps call this rather than
    /// [`has_destinations`](Self::has_destinations) so a delegated app is not
    /// gated on a roster it is deliberately never given.
    pub fn can_open(&self) -> bool {
        self.delegated || !self.destinations.is_empty()
    }

    /// Collect a request to open the surface, once.
    ///
    /// The counterpart to [`take_remark`](Self::take_remark) on the way *in*:
    /// that one carries a finished sentence out of an app that composed it,
    /// this one carries a question out of an app that cannot.
    pub fn take_request(&mut self) -> Option<RemarkRequest> {
        self.asked.take()
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
        if self.delegated {
            // Asked for, not drawn. `bounds` is dropped on purpose: it is this
            // window's rect, and the panel is about to be placed on a desktop
            // this window is only one tile of. Clamping into the app's own
            // bounds here is exactly the defect being moved away from.
            self.asked = Some(RemarkRequest {
                reference,
                beside: neighbour,
            });
            return true;
        }
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

    /// Close the panel if nothing has been typed into it. Returns whether it
    /// closed.
    ///
    /// For the chrome the panel opened beside. The two go up on one gesture,
    /// so an *empty* panel is still part of that gesture and leaves when the
    /// gesture is dismissed — a panel left standing over a menu that has gone
    /// is an overlay attached to nothing.
    ///
    /// The moment somebody has typed into it that stops being true. It is no
    /// longer chrome; it is their unsent sentence, and a click aimed at
    /// something else must not throw it away. So a written panel stays, and
    /// stays deliberately: the person can send it or dismiss it themselves,
    /// and neither outcome is one the machine chose for them.
    pub fn close_if_unwritten(&mut self) -> bool {
        if !self.compose.is_open() || !self.compose.text().trim().is_empty() {
            return false;
        }
        self.compose.close();
        true
    }

    /// The panel's rect, as laid out. Zero-area while it is closed.
    pub fn rect(&self) -> Rect {
        self.compose.get_rect()
    }

    /// The panel itself, while it is open, so a host can put it in the tree it
    /// publishes to a test harness.
    ///
    /// The panel is not a node of anybody's layout tree — it is anchored in
    /// window coordinates and laid out in a tree of its own — so without this
    /// it is invisible to a registry walk, and a harness driving the gesture
    /// has to aim at a pixel. It carries `test_id` `remark_compose`; the point
    /// of the compositor drawing it is that the name now resolves.
    ///
    /// Read-only on purpose. Events reach the panel through [`Self::pump`],
    /// which decides what is the panel's and what belongs to the app
    /// underneath; a host that also routed to it as an ordinary child would
    /// deliver every keystroke twice.
    pub fn compose_widget(&self) -> Option<&dyn Widget> {
        self.compose.is_open().then(|| &self.compose as &dyn Widget)
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
                    // Click-away dismisses an empty panel, and only an empty
                    // one. The rule across every way this surface can go is
                    // the same: it closes on an act aimed *at* it — Escape, or
                    // Send — or because there was nothing in it to lose. A
                    // click somewhere else is not aimed at somebody's unsent
                    // sentence, and a composer that throws one away because
                    // the pointer went elsewhere is the defect, not the
                    // tidiness.
                    self.close_if_unwritten();
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
