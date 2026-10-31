//! The sheets the session shows over the desktop (the power menu, the
//! questions it asks for an app or a service, workspace settings), each an
//! app of [`tilo_sheets`] on a layer of its own covering a display, up
//! while its seat's presentation says so. The seat is how the session and
//! the services behind it talk to the sheet: what it shows in, what the
//! person chose out. Its actions are published through its door while it
//! is up.

use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_compositor::{HostKey, HostLayer, KeyClaim, SurfaceHost};
use tilo_sheets::Seat;
use tilo_surface_host::Published;
use tilo_surface_host::{AppHost, Placement};
use ui::application::{ApplicationDefinition, ComponentRunnerInputs, prepare_private_surface};
use ui::host_operation::ResourceOperationStart;

/// A display's rectangle, in logical pixels: the display a sheet covers.
pub(crate) type Place = (i32, i32, u32, u32);

/// One sheet's app on its layer.
pub(crate) struct Sheet<S: Seat> {
    host: AppHost,
    seat: S,
    /// The lock screen: drawn while the session is locked, and only then.
    lock_screen: bool,
    /// The displays as the compositor last said they are.
    outputs: Vec<(i32, i32, i32, i32)>,
    /// The app answers its actions and draws while the sheet is down too: a
    /// command surface, whose key takes a screenshot without the bar up.
    always_serving: bool,
    /// The sheet asks the person something about what they are typing into
    /// and leaves the keyboard where it was: it takes Enter and Escape, and
    /// every other key reaches the field under it.
    typing_through: bool,
}

impl<S: Seat> Sheet<S> {
    /// Mount `definition` lent `seat`, as `person` on `device`.
    pub(crate) fn mount(
        definition: &'static ApplicationDefinition,
        seat: S,
        (device, person): (&str, &str),
        presentation: ComponentRunnerInputs,
        scale: f64,
    ) -> Result<Self, ui::launch::LaunchError> {
        let lent = seat.clone();
        let component =
            prepare_private_surface(definition, device, person, "desktop", 0, |context| {
                context.lend(lent);
            })?;
        let app = ui::App::from_component(component, presentation)?;
        let placement = Placement {
            origin: (0, 0),
            size: (1, 1),
            scale,
        };
        Ok(Self {
            host: AppHost::new(app, placement),
            seat,
            lock_screen: false,
            outputs: Vec::new(),
            always_serving: false,
            typing_through: false,
        })
    }

    /// The app keeps running while the sheet is down, and its actions stay
    /// offered: what it takes is the whole point of the key that asks it.
    pub(crate) const fn always_serving(mut self) -> Self {
        self.always_serving = true;
        self
    }

    /// Ask about what the person is typing into without taking their typing
    /// from them: the sheet answers Enter and Escape, the field keeps every
    /// other key. A prompt that asks whether to listen has to leave the
    /// person's typing where it was.
    pub(crate) const fn typing_through(mut self) -> Self {
        self.typing_through = true;
        self
    }

    /// Show it as the lock screen: while the session is locked, over
    /// nothing else.
    pub(crate) const fn locking(mut self) -> Self {
        self.lock_screen = true;
        self
    }

    pub(crate) fn seat(&self) -> &S {
        &self.seat
    }

    fn open(&self) -> bool {
        self.seat.presentation().is_open()
    }

    fn drawn(&self) -> bool {
        self.seat.presentation().is_drawn()
    }

    /// The sheet covers the display the session placed it on, or the first
    /// one while it named none.
    fn follow_place(&mut self) {
        let Some((x, y, w, h)) = covering(self.seat.presentation().place(), &self.outputs) else {
            return;
        };
        let scale = self.host.placement().scale;
        self.host.set_placement(Placement {
            origin: (x, y),
            size: (
                u32::try_from(w.max(1)).unwrap_or(1),
                u32::try_from(h.max(1)).unwrap_or(1),
            ),
            scale,
        });
    }
}

/// The display among `outputs` a sheet covers, its rectangle as the
/// compositor reports it: the one the session `named` while it is still
/// there, else the first in reading order, left to right and top to bottom:
/// a display unplugged from under a sheet takes it to the one the person's
/// eye starts at rather than leaving it on nothing.
fn covering(
    named: Option<Place>,
    outputs: &[(i32, i32, i32, i32)],
) -> Option<(i32, i32, i32, i32)> {
    let named = named.and_then(|(x, y, w, h)| {
        let (w, h) = (i32::try_from(w).ok()?, i32::try_from(h).ok()?);
        Some((x, y, w, h))
    });
    named
        .filter(|place| outputs.contains(place))
        .or_else(|| outputs.iter().copied().min_by_key(|(x, y, _, _)| (*x, *y)))
}

impl<S: Seat> Published for Sheet<S> {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        self.host.resources()
    }

    fn revision(&self) -> Option<u64> {
        Published::revision(&self.host)
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        self.host.invoke(address, arguments)
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        // A sheet that is down shows nothing to read, even one whose
        // actions stay offered while it is.
        if !self.drawn() {
            return Vec::new();
        }
        self.host.accessible_labels()
    }

    fn offered(&self) -> bool {
        self.always_serving || self.open()
    }

    /// The lock screen up withholds everything else.
    fn exclusive(&self) -> bool {
        self.lock_screen
    }
}

impl<S: Seat> SurfaceHost for Sheet<S> {
    fn lock_screen(&self) -> bool {
        self.lock_screen
    }

    fn layers(&self) -> Vec<HostLayer> {
        if self.drawn() {
            self.host.layers()
        } else {
            Vec::new()
        }
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        // A sheet is modal: a press beside its card is still the sheet's,
        // which takes it as one outside.
        self.open() && {
            let placement = self.host.placement();
            let (ox, oy) = placement.origin;
            let (w, h) = placement.size;
            x >= f64::from(ox)
                && y >= f64::from(oy)
                && x < f64::from(ox) + f64::from(w)
                && y < f64::from(oy) + f64::from(h)
        }
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        self.host.pointer_motion(x, y);
    }

    fn pointer_leave(&mut self) {
        self.host.pointer_leave();
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.host.pointer_button(button, pressed);
    }

    fn pointer_axis(&mut self, horizontal: f64, vertical: f64) {
        self.host.pointer_axis(horizontal, vertical);
    }

    fn wants_keyboard(&self) -> bool {
        self.open() && !self.typing_through
    }

    fn claim_key(&mut self, key: &HostKey, _locked: bool) -> KeyClaim {
        // The card's own two keys: the primary answer on Enter, the way out
        // on Escape. Ordinary typing is the field's.
        let answers = self.typing_through
            && self.open()
            && matches!(
                key.unmodified_keysym,
                xkeysym::key::Return | xkeysym::key::Escape
            );
        if answers {
            KeyClaim::Take
        } else {
            KeyClaim::Pass
        }
    }

    fn key(&mut self, key: &HostKey) {
        self.host.key(key);
    }

    fn prepare_frame(&mut self) -> bool {
        self.follow_place();
        // A command surface answers the actions it is offered even with
        // nothing of it drawn.
        (self.drawn() || self.always_serving) && self.host.prepare_frame()
    }

    fn cursor(&self) -> tilo_compositor::CursorIcon {
        self.host.cursor()
    }

    fn backdrops(&self) -> Vec<tilo_compositor::BackdropRequest> {
        self.host.backdrops()
    }

    fn set_backdrops(&mut self, images: Vec<Option<tilo_compositor::BackdropImage>>) {
        self.host.set_backdrops(images);
    }

    fn outputs_changed(&mut self, outputs: &[(i32, i32, i32, i32)]) {
        self.outputs = outputs.to_vec();
        self.follow_place();
    }
}

#[cfg(test)]
mod tests {
    use super::{Place, covering};

    const RIGHT: Place = (1000, 0, 1000, 700);
    const LEFT_SHOWN: (i32, i32, i32, i32) = (0, 0, 1000, 700);
    const RIGHT_SHOWN: (i32, i32, i32, i32) = (1000, 0, 1000, 700);

    #[test]
    fn a_sheet_covers_the_display_it_was_given() {
        assert_eq!(
            covering(Some(RIGHT), &[LEFT_SHOWN, RIGHT_SHOWN]),
            Some(RIGHT_SHOWN)
        );
    }

    #[test]
    fn a_sheet_covers_the_first_display_when_the_one_it_was_given_is_gone() {
        // The focused display was unplugged, and the compositor lists the
        // one still there second: the sheet goes to the one on the left,
        // where the person's eye starts, not to whichever came first.
        let unplugged: Place = (2000, 0, 1000, 700);
        assert_eq!(
            covering(Some(unplugged), &[RIGHT_SHOWN, LEFT_SHOWN]),
            Some(LEFT_SHOWN)
        );
    }

    #[test]
    fn a_sheet_covers_the_first_display_in_reading_order_when_none_was_given() {
        assert_eq!(covering(None, &[RIGHT_SHOWN, LEFT_SHOWN]), Some(LEFT_SHOWN));
        assert_eq!(covering(None, &[]), None);
    }
}
