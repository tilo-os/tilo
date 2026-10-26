//! The pinned roster's code: the people overlay with its peek and message
//! box (`tilo_desktop::people_overlay`).
//!
//! Without `deferred-people-overlay` it is linked in. With it, the bundling
//! crate hands over a loader ([`defer_people_overlay`]) that fetches the
//! code as its own module the first time someone pins the roster (or opens
//! a page where it was left pinned), so a visit that never pins it does not
//! download it. Until the code is in, the roster stays in the bar's
//! popover; following, its banner and the popover's rows need none of it.

use tilo_desktop::desk_edges::DockEdge;
use tilo_desktop::people_overlay::{PeekPictures, PeopleHit, PeopleOverlay, PeopleOverlayModel};
use ui::kurbo::{Affine, Point};
use ui::time::Instant;
use ui::vello::Scene;
use ui::winit::event::WindowEvent;
use ui::winit::keyboard::{Key, ModifiersState};
use ui::{TextRenderer, Theme};

/// What the page asks of the pinned roster ([`PeopleOverlay`]'s own
/// methods), so that the code behind it can come in its own module.
pub trait PinnedRoster {
    fn show(&mut self, open: bool);
    fn is_visible(&self) -> bool;
    /// Whose peek is open.
    fn peeked(&self) -> Option<&str>;
    /// Whether the message box holds the keyboard.
    fn composing(&self) -> bool;
    fn contains(&self, point: Point) -> bool;
    fn wants_pointer_capture(&self) -> bool;
    fn needs_animation_frame(&self) -> bool;
    fn next_wake_deadline(&self) -> Option<Instant>;
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &PeopleOverlayModel<'_>,
        pictures: &mut dyn PeekPictures,
    );
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        handled: &mut bool,
    ) -> Option<PeopleHit>;
    /// `Some` when the key was the overlay's, with what it asks for.
    fn key(&mut self, key: &Key, modifiers: ModifiersState) -> Option<Option<PeopleHit>>;
    /// The message box's message went: it empties.
    fn sent(&mut self);
}

impl PinnedRoster for PeopleOverlay {
    fn show(&mut self, open: bool) {
        PeopleOverlay::show(self, open);
    }

    fn is_visible(&self) -> bool {
        PeopleOverlay::is_visible(self)
    }

    fn peeked(&self) -> Option<&str> {
        PeopleOverlay::peeked(self)
    }

    fn composing(&self) -> bool {
        PeopleOverlay::composing(self)
    }

    fn contains(&self, point: Point) -> bool {
        PeopleOverlay::contains(self, point)
    }

    fn wants_pointer_capture(&self) -> bool {
        PeopleOverlay::wants_pointer_capture(self)
    }

    fn needs_animation_frame(&self) -> bool {
        PeopleOverlay::needs_animation_frame(self)
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        PeopleOverlay::next_wake_deadline(self)
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &PeopleOverlayModel<'_>,
        pictures: &mut dyn PeekPictures,
    ) {
        PeopleOverlay::draw(self, scene, transform, text, theme, model, pictures);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        handled: &mut bool,
    ) -> Option<PeopleHit> {
        PeopleOverlay::handle_event(self, event, scale, handled)
    }

    fn key(&mut self, key: &Key, modifiers: ModifiersState) -> Option<Option<PeopleHit>> {
        PeopleOverlay::key(self, key, modifiers)
    }

    fn sent(&mut self) {
        PeopleOverlay::sent(self);
    }
}

/// Makes the pinned roster, waiting at an edge.
pub type MakePinnedRoster = fn(DockEdge) -> Box<dyn PinnedRoster>;

/// The people overlay as the pinned roster.
#[must_use]
pub fn builtin_pinned_roster(edge: DockEdge) -> Box<dyn PinnedRoster> {
    Box::new(PeopleOverlay::new(edge))
}

#[cfg(not(feature = "deferred-people-overlay"))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the deferred build has no code until it is fetched"
)]
pub(crate) fn code() -> Option<MakePinnedRoster> {
    Some(builtin_pinned_roster)
}

#[cfg(feature = "deferred-people-overlay")]
static CODE: ui::deferred::Deferred<MakePinnedRoster> = ui::deferred::Deferred::new();

/// The pinned roster's code, fetching it on the first ask (`None` until it
/// is in).
#[cfg(feature = "deferred-people-overlay")]
pub(crate) fn code() -> Option<MakePinnedRoster> {
    CODE.get()
}

/// Fetch the pinned roster's code with `load` the first time the roster is
/// pinned, instead of linking it in (the `deferred-people-overlay`
/// feature). `load` starts the fetch and then calls
/// [`install_people_overlay`] or [`people_overlay_load_failed`].
#[cfg(feature = "deferred-people-overlay")]
pub fn defer_people_overlay(load: fn()) {
    CODE.defer(load);
}

/// The pinned roster's code arrived.
#[cfg(feature = "deferred-people-overlay")]
pub fn install_people_overlay(make: MakePinnedRoster) {
    CODE.install(make);
}

/// Fetching the pinned roster's code failed: the next frame that wants it
/// fetches again.
#[cfg(feature = "deferred-people-overlay")]
pub fn people_overlay_load_failed() {
    CODE.load_failed();
}
