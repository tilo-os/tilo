//! The Linux desktop's layers: the compositor shows each widget tree the session places on a
//! surface of its own, and the session's door reaches what it publishes.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_compositor::{
    BackdropImage, Compositor, HostKey, HostLayer, HostTextInput, HostedOutput, SurfaceHost,
};
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
        self.compositor.add_host(Box::new(Above {
            host: Rc::clone(&host),
            dismissed: Rc::clone(&heard.pressed_elsewhere),
        }));
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

/// A placed layer as the compositor drives it.
struct Above {
    host: Rc<RefCell<WidgetHost>>,
    /// What the role does when the pointer is pressed elsewhere.
    dismissed: Rc<dyn Fn()>,
}

impl SurfaceHost for Above {
    fn layers(&self) -> Vec<HostLayer> {
        SurfaceHost::layers(&*self.host.borrow())
    }

    fn withholds_captures(&self) -> bool {
        SurfaceHost::withholds_captures(&*self.host.borrow())
    }

    fn below_windows(&self) -> bool {
        SurfaceHost::below_windows(&*self.host.borrow())
    }

    fn window_pictures(&self) -> Vec<tilo_compositor::WindowPicture> {
        SurfaceHost::window_pictures(&*self.host.borrow())
    }

    fn backdrops(&self) -> Vec<tilo_compositor::BackdropRequest> {
        SurfaceHost::backdrops(&*self.host.borrow())
    }

    fn set_backdrops(&mut self, images: Vec<Option<BackdropImage>>) {
        SurfaceHost::set_backdrops(&mut *self.host.borrow_mut(), images);
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        SurfaceHost::accepts_pointer(&*self.host.borrow(), x, y)
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        SurfaceHost::pointer_motion(&mut *self.host.borrow_mut(), x, y);
    }

    fn pointer_leave(&mut self) {
        SurfaceHost::pointer_leave(&mut *self.host.borrow_mut());
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        SurfaceHost::pointer_button(&mut *self.host.borrow_mut(), button, pressed);
    }

    fn pointer_axis(&mut self, horizontal: f64, vertical: f64) {
        SurfaceHost::pointer_axis(&mut *self.host.borrow_mut(), horizontal, vertical);
    }

    fn pressed_elsewhere(&mut self, _x: f64, _y: f64) {
        (self.dismissed)();
    }

    fn wants_keyboard(&self) -> bool {
        SurfaceHost::wants_keyboard(&*self.host.borrow())
    }

    fn key(&mut self, key: &HostKey) {
        SurfaceHost::key(&mut *self.host.borrow_mut(), key);
    }

    fn text_input(&self) -> Option<HostTextInput> {
        SurfaceHost::text_input(&*self.host.borrow())
    }

    fn prepare_frame(&mut self) -> bool {
        SurfaceHost::prepare_frame(&mut *self.host.borrow_mut())
    }

    fn next_deadline(&self) -> Option<std::time::Instant> {
        SurfaceHost::next_deadline(&*self.host.borrow())
    }

    fn set_frame_interval(&mut self, interval: std::time::Duration) {
        SurfaceHost::set_frame_interval(&mut *self.host.borrow_mut(), interval);
    }

    fn outputs_changed(&mut self, outputs: &[HostedOutput]) {
        SurfaceHost::outputs_changed(&mut *self.host.borrow_mut(), outputs);
    }
}
