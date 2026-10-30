//! The session's surface apps on their layers above the windows: where
//! each goes on the outputs, and when it shows.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_compositor::{HostKey, HostLayer, KeyClaim, SurfaceHost};
use tilo_desktop::topbar::TOPBAR_HEIGHT_PX;
use tilo_surface_host::{AppHost, Placement, Published};
use ui::host_operation::ResourceOperationStart;
use ui::kurbo::Rect;

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

/// Quick settings' host, with its readings kept where the session reads
/// them too (a readout shows the level a key just changed).
pub(crate) struct Tap {
    pub(crate) inner: Rc<dyn tilo_quick_settings::host::PanelHost>,
    pub(crate) feed: Readings,
}

/// The readings quick settings was lent, once it attached.
pub(crate) type Readings = Rc<std::cell::RefCell<Option<tilo_quick_settings::host::Feed>>>;

impl tilo_quick_settings::host::PanelHost for Tap {
    fn attach(&self, feed: tilo_quick_settings::host::Feed) {
        *self.feed.borrow_mut() = Some(feed.clone());
        self.inner.attach(feed);
    }

    fn showing(&self, showing: &tilo_quick_settings::host::Showing) {
        self.inner.showing(showing);
    }

    fn run(&self, effect: tilo_quick_settings::QsEffect) -> Result<(), String> {
        self.inner.run(effect)
    }

    fn laid_out(&self, height: f64) {
        self.inner.laid_out(height);
    }
}

/// An app the bar opens below its right end, shown while `open` holds; a
/// press anywhere else closes it.
pub(crate) struct Popover {
    host: AppHost,
    open: Rc<Cell<bool>>,
    /// When a press outside last closed it.
    dismissed: Rc<Cell<Option<Instant>>>,
}

impl Popover {
    pub(crate) fn new(
        app: ui::App,
        open: Rc<Cell<bool>>,
        dismissed: Rc<Cell<Option<Instant>>>,
        scale: f64,
    ) -> Self {
        Self {
            host: AppHost::new(app, placement(0, 0, POPOVER_WIDTH, 1, scale)),
            open,
            dismissed,
        }
    }
}

impl Published for Popover {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        self.host.resources()
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        self.host.invoke(address, arguments)
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        if self.open.get() {
            self.host.accessible_labels()
        } else {
            Vec::new()
        }
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

    fn pressed_elsewhere(&mut self) {
        if self.open.replace(false) {
            self.dismissed.set(Some(Instant::now()));
        }
    }

    fn wants_keyboard(&self) -> bool {
        self.open.get()
    }

    fn key(&mut self, key: &HostKey) {
        self.host.key(key);
    }

    fn prepare_frame(&mut self) {
        // Kept current while hidden too: what it offers follows the
        // readings it was lent, open or not. Nothing is drawn unless
        // something changed.
        self.host.prepare_frame();
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

impl Published for Banners {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        self.host.resources()
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

/// The hardware keys' readout, bottom-centre on the first output: it never
/// takes the pointer or the keyboard.
pub(crate) struct Readout {
    host: tilo_surface_host::WidgetHost,
}

impl Readout {
    pub(crate) fn new(scale: f64) -> Self {
        let (w, h) = tilo_desktop::osd::OSD_SIZE;
        Self {
            host: tilo_surface_host::WidgetHost::new(
                Box::new(tilo_desktop::osd::OsdSlab::new()),
                Placement {
                    origin: (0, 0),
                    size: (w, h),
                    scale,
                },
                false,
            ),
        }
    }
}

impl Published for Readout {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        Published::resources(&mut self.host)
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        Published::invoke(&mut self.host, address, arguments)
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        Published::accessible_labels(&mut self.host)
    }
}

impl SurfaceHost for Readout {
    fn layers(&self) -> Vec<HostLayer> {
        self.host.layers()
    }

    fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
        false
    }

    fn pointer_motion(&mut self, _x: f64, _y: f64) {}

    fn pointer_button(&mut self, _button: u32, _pressed: bool) {}

    fn prepare_frame(&mut self) {
        self.host.prepare_frame();
    }

    fn outputs_changed(&mut self, outputs: &[(i32, i32, i32, i32)]) {
        let Some((x, y, w, h)) = first(outputs) else {
            return;
        };
        let (width, height) = tilo_desktop::osd::OSD_SIZE;
        let (width, height) = (
            i32::try_from(width).unwrap_or(i32::MAX),
            i32::try_from(height).unwrap_or(i32::MAX),
        );
        let scale = self.host.placement().scale;
        // A sixth of the way up from the bottom edge, where a glance finds
        // it and no bar sits.
        self.host.set_placement(placement(
            x.saturating_add(w.saturating_sub(width) / 2),
            y.saturating_add(h)
                .saturating_sub(height)
                .saturating_sub(h / 6),
            width,
            height,
            scale,
        ));
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

    fn pressed_elsewhere(&mut self) {
        self.0.borrow_mut().pressed_elsewhere();
    }

    fn claim_key(&mut self, key: &HostKey, locked: bool) -> KeyClaim {
        self.0.borrow_mut().claim_key(key, locked)
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
