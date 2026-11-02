//! The session's surface apps on their layers above the windows: where
//! each goes on the outputs, and when it shows.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_compositor::{HostKey, HostLayer, KeyClaim, OutputInfo, SurfaceHost};
use tilo_desktop::overlays::{Overlays, Slot};
use tilo_desktop::topbar::TOPBAR_HEIGHT_PX;
use tilo_surface_host::{AppHost, Placement, Published};
use ui::host_operation::ResourceOperationStart;
use ui::kurbo::Rect;

/// The bar's height, whole logical pixels.
fn bar_height() -> i32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the bar's height is a few dozen pixels"
    )]
    let height = TOPBAR_HEIGHT_PX.ceil() as i32;
    height
}

/// The rect the stack places a surface at, as a placement at `scale`.
fn placed(place: Rect, scale: f64) -> Placement {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a placed surface is at most a few thousand logical pixels"
    )]
    let (x, y, w, h) = (
        place.x0.round() as i32,
        place.y0.round() as i32,
        place.width().round() as i32,
        place.height().round() as i32,
    );
    placement(x, y, w, h, scale)
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

/// An app the bar opens under its right end, or beside the caret that asked
/// for it: it shows while the stack has its surface up, stands where the
/// stack places it, and a press anywhere else closes it — the session's
/// reader of presses outside the overlays does that, so nothing here
/// decides it.
pub(crate) struct Popover {
    host: AppHost,
    overlays: Overlays,
}

impl Popover {
    pub(crate) fn new(app: ui::App, overlays: Overlays, scale: f64) -> Self {
        let place = overlays.place(Slot::QuickSettings);
        let start = place.map_or_else(
            || placement(0, 0, 1, 1, scale),
            |place| placed(place, scale),
        );
        Self {
            host: AppHost::new(app, start),
            overlays,
        }
    }

    /// Whether the stack has this popover up.
    fn open(&self) -> bool {
        self.overlays.is_open(Slot::QuickSettings)
    }
}

impl Published for Popover {
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
        if self.open() {
            self.host.accessible_labels()
        } else {
            Vec::new()
        }
    }
}

impl SurfaceHost for Popover {
    fn layers(&self) -> Vec<HostLayer> {
        if self.open() {
            self.host.layers()
        } else {
            Vec::new()
        }
    }

    /// Whether what the panel shows is a secret — its Wi-Fi passphrase field
    /// with a value in it — and so never leaves the display in a capture.
    fn withholds_captures(&self) -> bool {
        self.open() && self.host.withholds_captures()
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        self.open() && self.host.accepts_pointer(x, y)
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
        self.overlays.target() == tilo_desktop::overlays::Target::Surface(Slot::QuickSettings)
    }

    fn key(&mut self, key: &HostKey) {
        self.host.key(key);
    }

    fn prepare_frame(&mut self) -> bool {
        // It stands where the stack places it: under the bar's trailing end,
        // or beside the caret that asked for it.
        if let Some(place) = self.overlays.place(Slot::QuickSettings) {
            let scale = self.host.placement().scale;
            let place = placed(place, scale);
            if self.host.placement() != place {
                self.host.set_placement(place);
            }
        }
        // Kept current while hidden too: what it offers follows the
        // readings it was lent, open or not. Nothing is drawn unless
        // something changed.
        self.host.prepare_frame()
    }

    fn next_deadline(&self) -> Option<std::time::Instant> {
        self.host.next_deadline()
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
}

/// The notification banners over the first output, under the bar: the
/// stack pins itself into its top-right corner, clear of the windows' bars.
pub(crate) struct Banners {
    host: AppHost,
    link: tilo_notification_banners::Link,
    /// The desk whose windows the stack keeps clear of.
    desk: tilo_desktop_surface::DesktopHandle,
    /// The outputs the compositor last said it has: the first is the one in
    /// use, the rest repeat what is about the whole machine.
    outputs: Vec<(i32, i32, i32, i32)>,
}

impl Banners {
    pub(crate) fn new(
        app: ui::App,
        link: tilo_notification_banners::Link,
        desk: tilo_desktop_surface::DesktopHandle,
        scale: f64,
    ) -> Self {
        Self {
            host: AppHost::new(app, placement(0, 0, 1, 1, scale)),
            link,
            desk,
            outputs: Vec::new(),
        }
    }

    /// Put the stack on the first output, under the bar, starting where the
    /// windows' content does; the outputs besides the one in use repeat what
    /// is about the whole machine. Placed every frame, as the popover is: the
    /// banners are mounted before any output is lit, and they move with the
    /// layout from then on.
    fn place(&mut self) {
        let Some(&(x, y, width, height)) = self.outputs.first() else {
            return;
        };
        let scale = self.host.placement().scale;
        let place = placement(x, y, width, height, scale);
        if self.host.placement() != place {
            self.host.set_placement(place);
        }
        // The banners' own coordinates start at the display's corner.
        let local = |(x0, y0, w, h): (i32, i32, i32, i32)| {
            let left = f64::from(x0) - f64::from(x);
            let top = f64::from(y0) - f64::from(y);
            Rect::new(left, top, left + f64::from(w), top + f64::from(h))
        };
        let under_bar = |output: (i32, i32, i32, i32)| {
            let mut display = local(output);
            display.y0 += f64::from(bar_height());
            display
        };
        // Banners start where the windows' content does, below their bars,
        // as the desk's own notification centre does.
        let clear_top = (self.desk.content_top() - f64::from(y)).max(0.0);
        self.link.place(tilo_notification_banners::Placement {
            viewport: under_bar((x, y, width, height)),
            clear_top,
            others: self
                .outputs
                .iter()
                .skip(1)
                .map(|&output| tilo_notification_banners::OtherDisplay {
                    display: local(output),
                    viewport: under_bar(output),
                })
                .collect(),
            ..tilo_notification_banners::Placement::default()
        });
    }
}

impl Published for Banners {
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

    fn prepare_frame(&mut self) -> bool {
        self.place();
        self.host.prepare_frame()
    }

    fn next_deadline(&self) -> Option<std::time::Instant> {
        self.host.next_deadline()
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
        outputs.clone_into(&mut self.outputs);
        self.place();
    }
}

/// Where each display stands and where the person is typing, as the session
/// last saw the compositor: what the readout answers with when a key raises
/// one.
#[derive(Clone, Debug, Default)]
pub(crate) struct ReadoutPlace {
    /// The focused text field's caret, in desk coordinates.
    caret: Option<Rect>,
    /// Each display's connector, and its rectangle.
    displays: Vec<(String, (i32, i32, i32, i32))>,
}

/// The session's hold of [`ReadoutPlace`], shared with the readout.
pub(crate) type ReadoutPlaces = Rc<RefCell<ReadoutPlace>>;

impl ReadoutPlace {
    /// Take the outputs the compositor shows and the caret of the field the
    /// person types into, as they stand now.
    pub(crate) fn observe(&mut self, outputs: &[OutputInfo], caret: Option<Rect>) {
        self.displays = outputs
            .iter()
            .filter(|output| output.enabled)
            .filter_map(|output| Some((output.name.clone(), output.rect?)))
            .collect();
        self.caret = caret;
    }

    /// The caret the person types at, when there is one.
    fn caret(&self) -> Option<Rect> {
        self.caret
    }

    /// Where the display `display` stands, or the first one when none is
    /// named or it is not shown.
    fn output(&self, display: Option<&str>) -> Option<(i32, i32, i32, i32)> {
        display
            .and_then(|display| {
                self.displays
                    .iter()
                    .find(|(connector, _)| connector == display)
                    .map(|(_, rect)| *rect)
            })
            .or_else(|| self.displays.first().map(|(_, rect)| *rect))
    }
}

/// The hardware keys' readout, on the display whose key changed it: it never
/// takes the pointer or the keyboard, and shows over the lock too.
pub(crate) struct Readout {
    host: tilo_surface_host::WidgetHost,
    /// Where each display stands and where the person types.
    places: ReadoutPlaces,
    /// The display the readout up now answers for, by connector.
    display: Option<String>,
}

impl Readout {
    pub(crate) fn new(scale: f64, places: ReadoutPlaces) -> Self {
        Self {
            host: tilo_surface_host::WidgetHost::new(
                Box::new(tilo_desktop::osd::OsdSlab::new()),
                Placement {
                    origin: (0, 0),
                    // Placed on the outputs before anything is drawn; the
                    // readout's pill is as wide as what it reads.
                    size: (
                        1,
                        u32::try_from(tilo_desktop::osd::surface_height()).unwrap_or(1),
                    ),
                    scale,
                },
                false,
            ),
            places,
            display: None,
        }
    }

    /// Draw in `theme`: the pill's ground is the theme's floating surface, so
    /// a light style is light, and its words follow the person's settings.
    pub(crate) fn set_theme(&mut self, theme: ui::Theme) {
        self.host.set_theme(theme);
    }

    /// Stand on the display `display` answers for, in the band the person is
    /// not typing in.
    fn stand(&mut self, display: Option<&str>) {
        self.display = display.map(str::to_owned);
        let Some(output) = self.places.borrow().output(display) else {
            return;
        };
        let caret = self.places.borrow().caret();
        let side = tilo_desktop::osd::clear_of_caret(caret, output);
        let scale = self.host.placement().scale;
        let (x, y, w, h) = tilo_desktop::osd::place(output, side);
        self.host.set_placement(placement(x, y, w, h, scale));
    }
}

impl Published for Readout {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        Published::resources(&mut self.host)
    }

    fn revision(&self) -> Option<u64> {
        Published::revision(&self.host)
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        // A readout stands on the display whose key changed it, and keeps
        // clear of the line the person is typing on.
        if address.resource == tilo_desktop::osd::OSD_RESOURCE && address.action == "show" {
            let display = arguments
                .get("display")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            self.stand(display.as_deref());
        }
        Published::invoke(&mut self.host, address, arguments)
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        Published::accessible_labels(&mut self.host)
    }

    /// A key pressed on the lock screen still answers there: the readout is
    /// the session's own word about what changed, not an app's.
    fn serves_while_locked(&self) -> bool {
        true
    }

    fn locked_resources(&mut self) -> Vec<ResourceSpec> {
        self.resources()
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

    fn prepare_frame(&mut self) -> bool {
        self.host.prepare_frame()
    }

    fn next_deadline(&self) -> Option<std::time::Instant> {
        self.host.next_deadline()
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

    fn outputs_changed(&mut self, _outputs: &[(i32, i32, i32, i32)]) {
        // Where the displays stand comes with the session's own reading of
        // them (connectors, not just rectangles): the readout stands on the
        // display it answers for again.
        let display = self.display.clone();
        self.stand(display.as_deref());
    }

    /// A hardware key's answer shows over the lock screen, where it was
    /// pressed.
    fn above_lock(&self) -> bool {
        true
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

    fn above_lock(&self) -> bool {
        self.0.borrow().above_lock()
    }

    fn withholds_captures(&self) -> bool {
        self.0.borrow().withholds_captures()
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

    fn pressed_elsewhere(&mut self, x: f64, y: f64) {
        self.0.borrow_mut().pressed_elsewhere(x, y);
    }

    fn claim_key(&mut self, key: &HostKey, locked: bool, inhibited: bool) -> KeyClaim {
        self.0.borrow_mut().claim_key(key, locked, inhibited)
    }

    fn wants_keyboard(&self) -> bool {
        self.0.borrow().wants_keyboard()
    }

    fn key(&mut self, key: &HostKey) {
        self.0.borrow_mut().key(key);
    }

    fn prepare_frame(&mut self) -> bool {
        self.0.borrow_mut().prepare_frame()
    }

    fn next_deadline(&self) -> Option<std::time::Instant> {
        self.0.borrow().next_deadline()
    }

    fn outputs_changed(&mut self, outputs: &[(i32, i32, i32, i32)]) {
        self.0.borrow_mut().outputs_changed(outputs);
    }

    fn window_pictures(&self) -> Vec<tilo_compositor::WindowPicture> {
        self.0.borrow().window_pictures()
    }

    fn backdrops(&self) -> Vec<tilo_compositor::BackdropRequest> {
        self.0.borrow().backdrops()
    }

    fn set_backdrops(&mut self, images: Vec<Option<tilo_compositor::BackdropImage>>) {
        self.0.borrow_mut().set_backdrops(images);
    }

    fn cursor(&self) -> tilo_compositor::CursorIcon {
        self.0.borrow().cursor()
    }

    fn drop_offered(&mut self, mimes: &[String], x: f64, y: f64) -> Option<String> {
        self.0.borrow_mut().drop_offered(mimes, x, y)
    }

    fn drop_left(&mut self) {
        self.0.borrow_mut().drop_left();
    }

    fn drop_data(&mut self, mime: &str, data: Vec<u8>) {
        self.0.borrow_mut().drop_data(mime, data);
    }

    fn take_drag(&mut self) -> Option<tilo_compositor::HostDrag> {
        self.0.borrow_mut().take_drag()
    }

    fn drag_ended(&mut self, dropped: bool) {
        self.0.borrow_mut().drag_ended(dropped);
    }
}
