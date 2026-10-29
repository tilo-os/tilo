//! Setup over everything: the first-use ceremony
//! ([`tilo_onboarding_host::view::OnboardingView`]) on a layer covering the
//! outputs, above the desktop and every window, taking the keyboard until
//! the person finished it. Its actions are the setup app's, through its
//! door; what setup needs of the session it reads through
//! [`tilo_onboarding_host::host::SessionHost`].

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Mutex, PoisonError};
use std::task::Waker;

use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_compositor::{HostKey, HostLayer, SurfaceHost};
use tilo_onboarding_host::view::OnboardingView;
use tilo_surface_host::{Placement, WidgetHost};
use tilo_surface_host::{Published, through_door};
use ui::device_local_door::SharedDoor;
use ui::host_operation::ResourceOperationStart;

/// What this session lends setup. One per process, as setup reads it
/// through a process-wide hook; each session started points it at itself.
struct Lent {
    waker: Mutex<Option<Waker>>,
    person: Mutex<String>,
    browser: Mutex<Option<PathBuf>>,
}

static LENT: Lent = Lent {
    waker: Mutex::new(None),
    person: Mutex::new(String::new()),
    browser: Mutex::new(None),
};

impl tilo_onboarding_host::host::SessionHost for Lent {
    fn loop_waker(&self) -> Waker {
        self.waker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| Waker::noop().clone())
    }

    fn browser_continuity(&self, projection: Option<PathBuf>) {
        *self.browser.lock().unwrap_or_else(PoisonError::into_inner) = projection;
    }

    fn wake(&self) {
        self.loop_waker().wake();
    }

    fn person(&self) -> String {
        self.person
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Point what setup reads of its session at this one: its loop and its
/// person.
pub(crate) fn lend(waker: Waker, person: &str) {
    *LENT.waker.lock().unwrap_or_else(PoisonError::into_inner) = Some(waker);
    person.clone_into(&mut LENT.person.lock().unwrap_or_else(PoisonError::into_inner));
    tilo_onboarding_host::host::install(&LENT);
}

/// The verified browser projection a carried-over setup brought, which the
/// browser starts with.
pub(crate) fn browser_continuity() -> Option<PathBuf> {
    LENT.browser
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// The setup view on its layer, while the person has not finished it.
pub(crate) struct Setup {
    widgets: WidgetHost,
    door: SharedDoor,
    finished: Rc<dyn Fn() -> bool>,
}

impl Setup {
    pub(crate) fn new(view: OnboardingView, scale: f64) -> Self {
        let door = view.device_local_door();
        let finished = view.finished_signal();
        let placement = Placement {
            origin: (0, 0),
            size: (1, 1),
            scale,
        };
        let widgets = WidgetHost::new(Box::new(view), placement, false)
            .covering_outputs()
            .with_keyboard(Rc::new(Cell::new(true)));
        Self {
            widgets,
            door,
            finished,
        }
    }

    /// Whether setup is still up.
    pub(crate) fn up(&self) -> bool {
        !(self.finished)()
    }

    /// Run what its door holds now, and publish what setup offers: the
    /// view does both on each pass of its own.
    fn serve(&mut self) {
        if self.up() && (self.door.has_queued() || self.door.revision() == 0) {
            self.widgets.relayout();
        }
    }
}

impl Published for Setup {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        self.serve();
        self.door.resources()
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        let door = self.door.clone();
        through_door(&door, address, arguments, || self.serve())
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        self.widgets
            .host_mut()
            .a11y_nodes()
            .into_iter()
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .collect()
    }

    fn offered(&self) -> bool {
        self.up()
    }

    fn exclusive(&self) -> bool {
        true
    }
}

impl SurfaceHost for Setup {
    fn layers(&self) -> Vec<HostLayer> {
        if self.up() {
            self.widgets.layers()
        } else {
            Vec::new()
        }
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        // Setup withholds the desktop: the pointer reaches nothing under it.
        self.up() && {
            let (ox, oy) = self.widgets.placement().origin;
            let (w, h) = self.widgets.placement().size;
            x >= f64::from(ox)
                && y >= f64::from(oy)
                && x < f64::from(ox) + f64::from(w)
                && y < f64::from(oy) + f64::from(h)
        }
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        self.widgets.pointer_motion(x, y);
    }

    fn pointer_leave(&mut self) {
        self.widgets.pointer_leave();
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.widgets.pointer_button(button, pressed);
    }

    fn pointer_axis(&mut self, horizontal: f64, vertical: f64) {
        self.widgets.pointer_axis(horizontal, vertical);
    }

    fn wants_keyboard(&self) -> bool {
        self.up()
    }

    fn key(&mut self, key: &HostKey) {
        self.widgets.key(key);
    }

    fn prepare_frame(&mut self) {
        if self.up() {
            self.widgets.prepare_frame();
        }
    }

    fn outputs_changed(&mut self, outputs: &[(i32, i32, i32, i32)]) {
        self.widgets.outputs_changed(outputs);
    }
}
