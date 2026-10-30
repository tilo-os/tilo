//! The sheets the session shows over the desktop (the power menu, the
//! questions it asks for an app or a service, workspace settings), each an
//! app of [`tilo_sheets`] on a layer of its own over the first output, up
//! while its seat's presentation says so. The seat is how the session and
//! the services behind it talk to the sheet: what it shows in, what the
//! person chose out. Its actions are published through its door while it
//! is up.

use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_compositor::{HostKey, HostLayer, SurfaceHost};
use tilo_sheets::Seat;
use tilo_surface_host::Published;
use tilo_surface_host::{AppHost, Placement};
use ui::application::{ApplicationDefinition, ComponentRunnerInputs, prepare_private_surface};
use ui::host_operation::ResourceOperationStart;

/// One sheet's app on its layer.
pub(crate) struct Sheet<S: Seat> {
    host: AppHost,
    seat: S,
    /// The lock screen: drawn while the session is locked, and only then.
    lock_screen: bool,
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
        })
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
        self.host.accessible_labels()
    }

    fn offered(&self) -> bool {
        self.open()
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
        self.open()
    }

    fn key(&mut self, key: &HostKey) {
        self.host.key(key);
    }

    fn prepare_frame(&mut self) -> bool {
        self.drawn() && self.host.prepare_frame()
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
        let Some(&(x, y, w, h)) = outputs.first() else {
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
