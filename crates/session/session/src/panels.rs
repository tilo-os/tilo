//! The session's surface apps on their layers above the windows: where
//! each goes on the outputs, and when it shows.

use std::cell::Cell;
use std::rc::Rc;

use tilo_compositor::{HostKey, HostLayer, SurfaceHost};
use tilo_desktop::topbar::TOPBAR_HEIGHT_PX;
use tilo_surface_host::{AppHost, Placement};
use ui::device_local_door::SharedDoor;
use ui::kurbo::Rect;

/// A surface app whose actions the session publishes: through its door,
/// as any catalogue on this device reaches it.
pub(crate) trait Served {
    /// The surface's door.
    fn door(&self) -> SharedDoor;
    /// Run what its door holds now, and publish what it offers.
    fn serve(&mut self);
    /// Whether it offers its actions now.
    fn offered(&self) -> bool {
        true
    }
}

/// The width of a popover panel (quick settings), logical pixels.
const POPOVER_WIDTH: i32 = 380;
/// The gap between a popover and the edges of its output.
const MARGIN: i32 = 8;

/// The bar's height, whole logical pixels.
fn bar_height() -> i32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the bar's height is a few dozen pixels"
    )]
    let height = TOPBAR_HEIGHT_PX.ceil() as i32;
    height
}

/// The first output's rectangle `(x, y, w, h)`, the one the bar's status
/// cluster is on.
fn first(outputs: &[(i32, i32, i32, i32)]) -> Option<(i32, i32, i32, i32)> {
    outputs.first().copied()
}

/// A rectangle in logical pixels as a placement at `scale`.
fn placement(x: i32, y: i32, w: i32, h: i32, scale: f64) -> Placement {
    Placement {
        origin: (x, y),
        size: (
            u32::try_from(w.max(1)).unwrap_or(1),
            u32::try_from(h.max(1)).unwrap_or(1),
        ),
        scale,
    }
}

/// An app the bar opens below its right end, shown while `open` holds.
pub(crate) struct Popover {
    host: AppHost,
    open: Rc<Cell<bool>>,
}

impl Popover {
    pub(crate) fn new(app: ui::App, open: Rc<Cell<bool>>, scale: f64) -> Self {
        Self {
            host: AppHost::new(app, placement(0, 0, POPOVER_WIDTH, 1, scale)),
            open,
        }
    }
}

impl Served for Popover {
    fn door(&self) -> SharedDoor {
        self.host.door()
    }

    fn serve(&mut self) {
        self.host.serve_door();
    }
}

impl SurfaceHost for Popover {
    fn layers(&self) -> Vec<HostLayer> {
        if self.open.get() {
            self.host.layers()
        } else {
            Vec::new()
        }
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        self.open.get() && self.host.accepts_pointer(x, y)
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
        self.open.get()
    }

    fn key(&mut self, key: &HostKey) {
        self.host.key(key);
    }

    fn prepare_frame(&mut self) {
        if self.open.get() {
            self.host.prepare_frame();
        }
    }

    fn outputs_changed(&mut self, outputs: &[(i32, i32, i32, i32)]) {
        let Some((x, y, w, h)) = first(outputs) else {
            return;
        };
        let top = y.saturating_add(bar_height());
        let scale = self.host.placement().scale;
        self.host.set_placement(placement(
            x.saturating_add(w)
                .saturating_sub(POPOVER_WIDTH)
                .saturating_sub(MARGIN),
            top,
            POPOVER_WIDTH,
            h.saturating_sub(bar_height()).saturating_sub(MARGIN),
            scale,
        ));
    }
}

/// The notification banners over the first output, under the bar: the
/// stack pins itself into its top-right corner.
pub(crate) struct Banners {
    host: AppHost,
    link: tilo_notification_banners::Link,
}

impl Banners {
    pub(crate) fn new(app: ui::App, link: tilo_notification_banners::Link, scale: f64) -> Self {
        Self {
            host: AppHost::new(app, placement(0, 0, 1, 1, scale)),
            link,
        }
    }
}

impl Served for Banners {
    fn door(&self) -> SharedDoor {
        self.host.door()
    }

    fn serve(&mut self) {
        self.host.serve_door();
    }
}

impl SurfaceHost for Banners {
    fn layers(&self) -> Vec<HostLayer> {
        self.host.layers()
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        self.host.accepts_pointer(x, y)
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
        self.host.wants_keyboard()
    }

    fn key(&mut self, key: &HostKey) {
        self.host.key(key);
    }

    fn prepare_frame(&mut self) {
        self.host.prepare_frame();
    }

    fn outputs_changed(&mut self, outputs: &[(i32, i32, i32, i32)]) {
        let Some((x, y, w, h)) = first(outputs) else {
            return;
        };
        let scale = self.host.placement().scale;
        self.host.set_placement(placement(x, y, w, h, scale));
        self.link.place(tilo_notification_banners::Placement {
            viewport: Rect::new(0.0, f64::from(bar_height()), f64::from(w), f64::from(h)),
            ..tilo_notification_banners::Placement::default()
        });
    }
}

/// A hosted surface the session keeps a hold of after handing it to the
/// compositor: the chrome, which the session reads back.
pub(crate) struct Held<T>(pub(crate) Rc<std::cell::RefCell<T>>);

impl<T: SurfaceHost> SurfaceHost for Held<T> {
    fn layers(&self) -> Vec<HostLayer> {
        self.0.borrow().layers()
    }

    fn below_windows(&self) -> bool {
        self.0.borrow().below_windows()
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        self.0.borrow().accepts_pointer(x, y)
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        self.0.borrow_mut().pointer_motion(x, y);
    }

    fn pointer_leave(&mut self) {
        self.0.borrow_mut().pointer_leave();
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.0.borrow_mut().pointer_button(button, pressed);
    }

    fn pointer_axis(&mut self, horizontal: f64, vertical: f64) {
        self.0.borrow_mut().pointer_axis(horizontal, vertical);
    }

    fn wants_keyboard(&self) -> bool {
        self.0.borrow().wants_keyboard()
    }

    fn key(&mut self, key: &HostKey) {
        self.0.borrow_mut().key(key);
    }

    fn prepare_frame(&mut self) {
        self.0.borrow_mut().prepare_frame();
    }

    fn outputs_changed(&mut self, outputs: &[(i32, i32, i32, i32)]) {
        self.0.borrow_mut().outputs_changed(outputs);
    }
}
