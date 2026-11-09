//! The session's surface apps on their layers above the windows: where each goes on the outputs,
//! and when it shows.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_compositor::{HostKey, HostLayer, OutputInfo, SurfaceHost};
use tilo_desktop::topbar::TOPBAR_HEIGHT_PX;
use tilo_surface_host::{AppHost, Placement, Published};
use ui::host_operation::ResourceOperationStart;
use ui::kurbo::{Point, Rect};

/// The bar's height, whole logical pixels.
fn bar_height() -> i32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the bar's height is a few dozen pixels"
    )]
    let height = TOPBAR_HEIGHT_PX.ceil() as i32;
    height
}

/// A rectangle in logical pixels as a placement at `scale` .
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

/// A rect on the desk another surface holds while it is up (the banners' stack, the readout's
/// band), shared with the panels that must keep clear of it and released once it no longer stands.
#[derive(Clone, Default)]
pub(crate) struct Reserved {
    /// The rect, and when it stops being held (`None`: until released).
    held: Rc<Cell<Option<Hold>>>,
}

/// A rect held while a surface is up, and when the hold ends.
#[derive(Clone, Copy)]
struct Hold {
    /// The rect held.
    rect: Rect,
    /// When it stops being held (`None`: until released).
    until: Option<ui::time::Instant>,
}

impl Reserved {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Hold `rect` until released.
    fn hold(&self, rect: Rect) {
        self.held.set(Some(Hold { rect, until: None }));
    }

    /// Hold `rect` for `hold` from now.
    fn hold_for(&self, rect: Rect, hold: ui::time::Duration) {
        self.held.set(Some(Hold {
            rect,
            until: Some(until(hold)),
        }));
    }

    /// Release it.
    fn release(&self) {
        self.held.set(None);
    }

    /// The rect held now, if any.
    fn now(&self) -> Option<Rect> {
        let now = ui::time::now();
        self.held
            .get()
            .filter(|hold| hold.until.is_none_or(|until| until > now))
            .map(|hold| hold.rect)
    }

    fn set(&self, rect: Option<Rect>) {
        match rect {
            Some(rect) => self.hold(rect),
            None => self.release(),
        }
    }
}

/// When a hold of `hold` from now ends.
fn until(hold: ui::time::Duration) -> ui::time::Instant {
    ui::time::now()
        .checked_add(hold)
        .unwrap_or_else(ui::time::now)
}

/// The notification banners over the first output, under the bar: the stack pins itself into its
/// top-right corner, clear of the windows' bars.
pub(crate) struct Banners {
    host: AppHost,
    link: tilo_notification_banners::Link,
    /// The desk whose windows the stack keeps clear of.
    desk: tilo_desktop_surface::DesktopHandle,
    /// The outputs the compositor last said it has: the first is the one in use, the rest repeat
    /// what is about the whole machine.
    outputs: Vec<tilo_compositor::HostedOutput>,
    /// The rect the stack covers, which the floating layer keeps clear of while a banner shows.
    stack: Reserved,
}

impl Banners {
    pub(crate) fn new(
        app: ui::App,
        link: tilo_notification_banners::Link,
        desk: tilo_desktop_surface::DesktopHandle,
        scale: f64,
        stack: Reserved,
    ) -> Self {
        Self {
            host: AppHost::new(app, placement(0, 0, 1, 1, scale)).named("Notifications"),
            link,
            desk,
            outputs: Vec::new(),
            stack,
        }
    }

    /// Say where the stack stands, so the floating layer's cards keep clear of it while a banner
    /// shows.
    fn publish_stack(&self) {
        let stack = self.link.shown().stack;
        self.stack.set((!stack.is_zero_area()).then_some(stack));
    }

    /// Put the stack on the first output, under the bar, starting where the windows' content does;
    /// the outputs besides the one in use repeat what is about the whole machine. Placed every
    /// frame, as the popover is: the banners are mounted before any output is lit, and they move
    /// with the layout from then on.
    fn place(&mut self) {
        let Some(first) = self.outputs.first().copied() else {
            return;
        };
        let (x, y, width, height) = first.rect;
        let scale = first.scale;
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
                .map(|output| tilo_notification_banners::OtherDisplay {
                    display: local(output.rect),
                    viewport: under_bar(output.rect),
                })
                .collect(),
            ..tilo_notification_banners::Placement::default()
        });
        self.publish_stack();
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
        look: &mut tilo_surface_host::Look<'_>,
    ) -> Result<ResourceOperationStart, DoorError> {
        self.host.invoke(address, arguments, look)
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        self.host.accessible_labels()
    }

    /// The controls a banner on screen shows by name, where they stand on the desk, and the press
    /// of one: a banner's buttons answer the control socket as any window's do.
    fn controls(&mut self) -> Vec<(String, (f64, f64, f64, f64))> {
        Published::controls(&mut self.host)
    }

    fn press(&mut self, name: &str) -> Result<(), String> {
        Published::press(&mut self.host, name)
    }

    fn press_action(
        &mut self,
        name: &str,
        action: ui::window_targets::TargetAction,
    ) -> Result<(), String> {
        Published::press_action(&mut self.host, name, action)
    }

    fn a11y_tree(&mut self) -> Option<ui::accesskit::TreeUpdate> {
        self.host.a11y_tree()
    }

    fn a11y_name(&self) -> Option<String> {
        Published::a11y_name(&self.host)
    }

    fn a11y_action(&mut self, target: ui::accesskit::NodeId, action: &ui::a11y::Action) -> bool {
        Published::a11y_action(&mut self.host, target, action)
    }

    fn wants_keyboard(&self) -> bool {
        SurfaceHost::wants_keyboard(self)
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
        SurfaceHost::wants_keyboard(&self.host)
    }

    fn key(&mut self, key: &HostKey) {
        self.host.key(key);
    }

    fn prepare_frame(&mut self) -> bool {
        self.place();
        let drew = self.host.prepare_frame();
        self.publish_stack();
        drew
    }

    fn next_deadline(&self) -> Option<std::time::Instant> {
        self.host.next_deadline()
    }

    fn set_frame_interval(&mut self, interval: std::time::Duration) {
        self.host.set_frame_interval(interval);
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

    fn outputs_changed(&mut self, outputs: &[tilo_compositor::HostedOutput]) {
        outputs.clone_into(&mut self.outputs);
        self.place();
    }
}

/// The floating layer over the first output: every activity's card, docked in a corner column or
/// dragged loose, clear of the bar, the banners and the readout. The session says what to show and
/// carries out what the person did; the layer draws it.
pub(crate) struct ActivityLayer {
    host: AppHost,
    link: tilo_activities::Link,
    /// The session's reading of every app's activity, and its rules about what shows.
    reader: Rc<crate::chrome::Activities>,
    /// The cards the apps draw themselves: where the layer puts each of their windows this pass.
    cards: crate::cards::Cards,
    /// The corner docked cards gather in, on this device.
    corner: ui::floating_layer::Corner,
    /// The outputs the compositor last said it has: the cards stand on the first.
    outputs: Vec<tilo_compositor::HostedOutput>,
    /// What the cards keep clear of: the banners' stack, the readout's band.
    banners: Reserved,
    readout: Reserved,
    /// The feed published last, so an unchanged screen is not a frame.
    published: Option<tilo_activities::Feed>,
}

impl ActivityLayer {
    pub(crate) fn new(
        app: ui::App,
        link: tilo_activities::Link,
        reader: Rc<crate::chrome::Activities>,
        cards: crate::cards::Cards,
        banners: Reserved,
        readout: Reserved,
        scale: f64,
    ) -> Self {
        Self {
            host: AppHost::new(app, placement(0, 0, 1, 1, scale)).named("Activities"),
            link,
            reader,
            cards,
            corner: ui::floating_layer::Corner::default(),
            outputs: Vec::new(),
            banners,
            readout,
            published: None,
        }
    }

    /// Put the layer on the first output, under the bar, with what the person
    /// shows now, and take back what the last frame asked. Placed every frame,
    /// as the banners are: the layer is mounted before any output is lit.
    fn place(&mut self) {
        let Some(first) = self.outputs.first().copied() else {
            return;
        };
        let (x, y, width, height) = first.rect;
        let scale = first.scale;
        let where_ = placement(x, y, width, height, scale);
        if self.host.placement() != where_ {
            self.host.set_placement(where_);
        }
        // The layer's own coordinates start at the display's corner, as the banners' do.
        let local = |rect: Rect| {
            Rect::new(
                rect.x0 - f64::from(x),
                rect.y0 - f64::from(y),
                rect.x1 - f64::from(x),
                rect.y1 - f64::from(y),
            )
        };
        let bar = f64::from(bar_height());
        let work = Rect::new(0.0, bar, f64::from(width), f64::from(height));
        let placement = ui::floating_layer::Placement {
            work,
            bar: Rect::new(0.0, 0.0, f64::from(width), bar),
            banners: self.banners.now(),
            readout: self.readout.now().map(local),
        };
        let cards = self.reader.cards();
        let expanded = self.reader.expanded();
        // Where the layer puts the cards their apps draw, this pass: the app's gesture is a
        // request, and this is the sign the desk places its window by. The layer lays them out with
        // the same arithmetic it draws with.
        let layer = ui::floating_layer::place(&placement, self.corner, &cards, expanded);
        self.cards.begin_slots();
        for placed in &layer.cards {
            if self.cards.size(&placed.app, &placed.summary.id).is_none() {
                continue;
            }
            let at = Rect::new(
                placed.rect.x0 + f64::from(x),
                placed.rect.y0 + f64::from(y),
                placed.rect.x1 + f64::from(x),
                placed.rect.y1 + f64::from(y),
            );
            self.cards.slot_at(&placed.app, &placed.summary.id, at);
        }
        let feed = tilo_activities::Feed {
            placement,
            corner: self.corner,
            cards,
            expanded,
        };
        if self.published.as_ref() != Some(&feed) {
            self.link.publish(feed.clone());
            self.published = Some(feed);
        }
        self.answer();
    }

    /// Carry out what the person did on the cards.
    fn answer(&self) {
        for ask in self.link.take_asks() {
            use tilo_activities::Ask;
            match ask {
                Ask::Control {
                    app,
                    activity,
                    control,
                } => self.reader.press(&app, &activity, &control),
                Ask::Close { app, activity } => self.reader.close(&app, &activity),
                Ask::Collapse { app, activity } => self.reader.collapse(&app, &activity),
                Ask::Moved { app, activity, at } => self.reader.moved(&app, &activity, at),
                Ask::Seek {
                    app,
                    activity,
                    fraction,
                } => self.reader.seek(&app, &activity, fraction),
            }
        }
    }
}

impl Published for ActivityLayer {
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
        look: &mut tilo_surface_host::Look<'_>,
    ) -> Result<ResourceOperationStart, DoorError> {
        self.host.invoke(address, arguments, look)
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        self.host.accessible_labels()
    }

    /// The controls a card on screen shows by name, where they stand on the desk, and the press of
    /// one: a card's buttons answer the control socket as any window's do.
    fn controls(&mut self) -> Vec<(String, (f64, f64, f64, f64))> {
        Published::controls(&mut self.host)
    }

    fn press(&mut self, name: &str) -> Result<(), String> {
        Published::press(&mut self.host, name)
    }

    fn press_action(
        &mut self,
        name: &str,
        action: ui::window_targets::TargetAction,
    ) -> Result<(), String> {
        Published::press_action(&mut self.host, name, action)
    }

    fn a11y_tree(&mut self) -> Option<ui::accesskit::TreeUpdate> {
        self.host.a11y_tree()
    }

    fn a11y_name(&self) -> Option<String> {
        Published::a11y_name(&self.host)
    }

    fn a11y_action(&mut self, target: ui::accesskit::NodeId, action: &ui::a11y::Action) -> bool {
        Published::a11y_action(&mut self.host, target, action)
    }

    fn wants_keyboard(&self) -> bool {
        SurfaceHost::wants_keyboard(self)
    }
}

impl SurfaceHost for ActivityLayer {
    fn layers(&self) -> Vec<HostLayer> {
        self.host.layers()
    }

    /// Only the cards take the pointer: a press beside one reaches the window under it, as it does
    /// anywhere else on the desk.
    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        let (ox, oy) = self.host.placement().origin;
        let point = Point::new(x - f64::from(ox), y - f64::from(oy));
        self.link
            .shown()
            .cards
            .iter()
            .any(|(_, _, rect)| rect.contains(point))
            && self.host.accepts_pointer(x, y)
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
        SurfaceHost::wants_keyboard(&self.host)
    }

    fn key(&mut self, key: &HostKey) {
        self.host.key(key);
    }

    fn prepare_frame(&mut self) -> bool {
        self.place();
        let drew = self.host.prepare_frame();
        self.answer();
        drew
    }

    fn next_deadline(&self) -> Option<std::time::Instant> {
        self.host.next_deadline()
    }

    fn set_frame_interval(&mut self, interval: std::time::Duration) {
        self.host.set_frame_interval(interval);
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

    fn outputs_changed(&mut self, outputs: &[tilo_compositor::HostedOutput]) {
        outputs.clone_into(&mut self.outputs);
        self.place();
    }
}

/// Where each display stands and where the person is typing, as the session last saw the
/// compositor: what the readout answers with when a key raises one.
#[derive(Clone, Debug, Default)]
pub(crate) struct ReadoutPlace {
    /// The focused text field's caret, in desk coordinates.
    caret: Option<Rect>,
    /// Each display the compositor shows, where it stands and the scale it draws at.
    displays: Vec<crate::displays::Placed>,
}

/// The session's hold of [`ReadoutPlace`], shared with the readout.
pub(crate) type ReadoutPlaces = Rc<RefCell<ReadoutPlace>>;

impl ReadoutPlace {
    /// Take the outputs the compositor shows and the caret of the field the person types into, as
    /// they stand now.
    pub(crate) fn observe(&mut self, outputs: &[OutputInfo], caret: Option<Rect>) {
        self.displays = crate::displays::placed(outputs)
            .into_iter()
            .filter(|display| display.on)
            .collect();
        self.caret = caret;
    }

    /// The caret the person types at, when there is one.
    pub(crate) fn caret(&self) -> Option<Rect> {
        self.caret
    }

    /// Where the display `display` stands, or the first one when none is named or it is not shown.
    fn output(&self, display: Option<&str>) -> Option<((i32, i32, i32, i32), f64)> {
        let named = display.and_then(|display| {
            self.displays
                .iter()
                .find(|shown| shown.connector == display)
        });
        named
            .or_else(|| self.displays.first())
            .map(|shown| (shown.rect, shown.scale))
    }
}

/// The hardware keys' readout, on the display whose key changed it: it never takes the pointer or
/// the keyboard, and shows over the lock too.
pub(crate) struct Readout {
    host: tilo_surface_host::WidgetHost,
    /// Where each display stands and where the person types.
    places: ReadoutPlaces,
    /// The display the readout up now answers for, by connector.
    display: Option<String>,
    /// The band it stands in, which the floating layer keeps clear of while it is up.
    band: Reserved,
}

impl Readout {
    pub(crate) fn new(scale: f64, places: ReadoutPlaces, band: Reserved) -> Self {
        Self {
            host: tilo_surface_host::WidgetHost::new(
                Box::new(tilo_desktop::osd::OsdSlab::new()),
                Placement {
                    origin: (0, 0),
                    // Placed on the outputs before anything is drawn; the readout's pill is as wide
                    // as what it reads.
                    size: (
                        1,
                        u32::try_from(tilo_desktop::osd::surface_height()).unwrap_or(1),
                    ),
                    scale,
                },
                false,
            )
            .named("Readout"),
            places,
            display: None,
            band,
        }
    }

    /// Draw in `theme` : the pill's ground is the theme's floating surface, so a light style is
    /// light, and its words follow the person's settings.
    pub(crate) fn set_theme(&mut self, theme: ui::Theme) {
        self.host.set_theme(theme);
    }

    /// Stand on the display `display` answers for, in the band the person is not typing in.
    fn stand(&mut self, display: Option<&str>) {
        self.display = display.map(str::to_owned);
        let Some(output) = self.places.borrow().output(display) else {
            return;
        };
        let caret = self.places.borrow().caret();
        let (output, scale) = output;
        let side = tilo_desktop::osd::clear_of_caret(caret, output);
        let (x, y, w, h) = tilo_desktop::osd::place(output, side);
        self.host.set_placement(placement(x, y, w, h, scale));
        // The layer keeps clear of the readout while it stands.
        self.band.hold_for(
            Rect::new(
                f64::from(x),
                f64::from(y),
                f64::from(x) + f64::from(w),
                f64::from(y) + f64::from(h),
            ),
            tilo_desktop::osd::OSD_HOLD,
        );
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
        look: &mut tilo_surface_host::Look<'_>,
    ) -> Result<ResourceOperationStart, DoorError> {
        // A readout stands on the display whose key changed it, and keeps clear of the line the
        // person is typing on.
        if address.resource == tilo_desktop::osd::OSD_RESOURCE && address.action == "show" {
            let display = arguments
                .get("display")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            self.stand(display.as_deref());
        }
        Published::invoke(&mut self.host, address, arguments, look)
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        Published::accessible_labels(&mut self.host)
    }

    /// A key pressed on the lock screen still answers there: the readout is the session's own word
    /// about what changed, not an app's.
    fn serves_while_locked(&self) -> bool {
        true
    }

    fn locked_resources(&mut self) -> Vec<ResourceSpec> {
        self.resources()
    }

    fn a11y_tree(&mut self) -> Option<ui::accesskit::TreeUpdate> {
        Published::a11y_tree(&mut self.host)
    }

    fn a11y_name(&self) -> Option<String> {
        Published::a11y_name(&self.host)
    }

    fn a11y_action(&mut self, target: ui::accesskit::NodeId, action: &ui::a11y::Action) -> bool {
        Published::a11y_action(&mut self.host, target, action)
    }

    fn wants_keyboard(&self) -> bool {
        SurfaceHost::wants_keyboard(self)
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

    fn set_frame_interval(&mut self, interval: std::time::Duration) {
        self.host.set_frame_interval(interval);
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

    fn outputs_changed(&mut self, _outputs: &[tilo_compositor::HostedOutput]) {
        // Where the displays stand comes with the session's own reading of them (connectors, not
        // just rectangles): the readout stands on the display it answers for again.
        let display = self.display.clone();
        self.stand(display.as_deref());
    }

    /// A hardware key's answer shows over the lock screen, where it was pressed.
    fn above_lock(&self) -> bool {
        true
    }
}
