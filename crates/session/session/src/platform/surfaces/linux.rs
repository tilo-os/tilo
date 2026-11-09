//! The Linux desktop's layers: the compositor shows each widget tree the session places on a
//! surface of its own, and the session's door reaches what it publishes.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_compositor::{Compositor, HostKey, HostLayer, KeyClaim, SurfaceHost};
use tilo_session_core::door::{Doors, Published};
use tilo_surface_host::WidgetHost;
use ui::widget::Widget;

use super::{Heard, Layer, Placement, Surfaces};

/// What a Linux session places layers on: its compositor, and the door its surfaces publish
/// through.
pub struct Hosted<'a> {
    compositor: &'a mut Compositor,
    doors: &'a mut Doors,
    theme: ui::Theme,
}

impl<'a> Hosted<'a> {
    /// The layers of the session that owns `compositor` and `doors`, drawn in `theme`.
    pub fn new(compositor: &'a mut Compositor, doors: &'a mut Doors, theme: ui::Theme) -> Self {
        Self {
            compositor,
            doors,
            theme,
        }
    }
}

impl Surfaces for Hosted<'_> {
    fn place(
        &mut self,
        name: &str,
        tree: Box<dyn Widget>,
        placement: Placement,
        heard: Heard,
    ) -> Rc<dyn Layer> {
        let mut host = WidgetHost::new(tree, placement, false).named(name);
        host.set_theme(self.theme.clone());
        let host = Rc::new(RefCell::new(
            host.covering_outputs()
                .with_keyboard(Rc::clone(&heard.wants_keyboard)),
        ));
        self.compositor.add_host(Box::new(Held(
            Rc::clone(&host),
            Some(heard.pressed_elsewhere),
        )));
        host
    }

    fn publish(&mut self, published: Rc<RefCell<dyn Published>>) {
        self.doors.add(published);
    }
}

impl Layer for RefCell<WidgetHost> {
    fn relayout(&self) {
        self.borrow_mut().relayout();
    }

    fn set_theme(&self, theme: ui::Theme) {
        self.borrow_mut().set_theme(theme);
    }

    fn revision(&self) -> u64 {
        WidgetHost::revision(&self.borrow())
    }

    fn a11y_tree(&self) -> Option<ui::accesskit::TreeUpdate> {
        Published::a11y_tree(&mut *self.borrow_mut())
    }

    fn a11y_action(&self, target: ui::accesskit::NodeId, action: &ui::a11y::Action) -> bool {
        Published::a11y_action(&mut *self.borrow_mut(), target, action)
    }

    fn accessible_labels(&self) -> Vec<String> {
        Published::accessible_labels(&mut *self.borrow_mut())
    }

    fn controls(&self) -> Vec<(String, (f64, f64, f64, f64))> {
        Published::controls(&mut *self.borrow_mut())
    }

    fn press(&self, name: &str) -> Result<(), String> {
        Published::press(&mut *self.borrow_mut(), name)
    }
}

/// A hosted surface the session keeps a hold of after handing it to the compositor: the chrome,
/// which the session reads back, and when the seam placed it for a role, what that role hears.
pub struct Held<T>(
    Rc<RefCell<T>>,
    /// What the role closes when the pointer is pressed elsewhere: it runs before the tree is
    /// borrowed, so the role may ask the layer it holds to draw again.
    Option<Rc<dyn Fn()>>,
);

impl<T> Held<T> {
    /// The surface `tree` shows, with nothing of a role to hear, as the compositor drives it.
    pub const fn new(tree: Rc<RefCell<T>>) -> Self {
        Self(tree, None)
    }
}

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

    fn drawn_over_lock(&self) -> bool {
        self.0.borrow().drawn_over_lock()
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
        if let Some(dismissed) = &self.1 {
            dismissed();
        }
        self.0.borrow_mut().pressed_elsewhere(x, y);
    }

    fn claim_key(&mut self, key: &HostKey, locked: bool, inhibited: bool) -> KeyClaim {
        self.0.borrow_mut().claim_key(key, locked, inhibited)
    }

    fn wants_keyboard(&self) -> bool {
        SurfaceHost::wants_keyboard(&*self.0.borrow())
    }

    fn key(&mut self, key: &HostKey) {
        self.0.borrow_mut().key(key);
    }

    fn prepare_frame(&mut self) -> bool {
        self.0.borrow_mut().prepare_frame()
    }

    fn advance(&mut self) -> bool {
        self.0.borrow_mut().advance()
    }

    fn next_deadline(&self) -> Option<std::time::Instant> {
        self.0.borrow().next_deadline()
    }

    fn set_frame_interval(&mut self, interval: std::time::Duration) {
        self.0.borrow_mut().set_frame_interval(interval);
    }

    fn outputs_changed(&mut self, outputs: &[tilo_compositor::HostedOutput]) {
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
