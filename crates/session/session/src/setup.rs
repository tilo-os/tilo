//! What stands over everything before the desk is used: the first-use ceremony
//! ([`tilo_onboarding_host::view::OnboardingView`]) on a layer covering the outputs, above the
//! desktop and every window, taking the keyboard until the person finished it, or the wait for the
//! clock to admit this device's certificate, which goes away by itself. Its actions are the setup
//! app's, through its door; what setup needs of the session it reads through
//! [`tilo_onboarding_host::host::SessionHost`].

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Mutex, PoisonError};
use std::task::Waker;

use crate::platform::surfaces::{Placement, WidgetHost};
use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_session_core::surface::HostKey;
use tilo_session_core::surface::{HostLayer, SurfaceHost};

use tilo_onboarding_host::view::OnboardingView;
use tilo_session_core::door::{Published, through_door};
use ui::device_local_door::SharedDoor;
use ui::host_operation::ResourceOperationStart;

use crate::clock_wait::{ClockWait, ClockWaitStep};

/// What this session lends setup. One per process, as setup reads it through a process-wide hook;
/// each session started points it at itself.
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

/// Point what setup reads of its session at this one: its loop and its person.
pub(crate) fn lend(waker: Waker, person: &str) {
    *LENT.waker.lock().unwrap_or_else(PoisonError::into_inner) = Some(waker);
    person.clone_into(&mut LENT.person.lock().unwrap_or_else(PoisonError::into_inner));
    tilo_onboarding_host::host::install(&LENT);
}

/// The verified browser projection a carried-over setup brought, which the browser starts with.
pub(crate) fn browser_continuity() -> Option<PathBuf> {
    LENT.browser
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// What this session shows before its desk is used.
pub enum BeforeDesk {
    /// Setup the person goes through once: the first use of this device, or the recovery of its
    /// identity.
    Ceremony(Box<OnboardingView>),
    /// The wait for the clock to admit this device's certificate. It goes away by itself, and what
    /// a verified identity opens takes its place.
    WaitForClock(ClockWait),
}

/// What the layer shows, while it shows anything.
enum Surface {
    /// The setup app's own view: its door publishes its actions, and its own signal says when the
    /// person finished it.
    Ceremony {
        widgets: WidgetHost,
        door: SharedDoor,
        finished: Rc<dyn Fn() -> bool>,
    },
    /// The wait for the clock, which has no actions of its own.
    WaitForClock {
        widgets: WidgetHost,
        wait: ClockWait,
    },
}

/// The setup view on its layer, while the person has not finished it.
pub(crate) struct Setup {
    surface: Surface,
    /// The outputs this layer covers, as the compositor last told a view: replayed to a view that
    /// replaces another, which is only told when the outputs themselves change.
    outputs: Vec<tilo_session_core::surface::HostedOutput>,
}

impl Setup {
    /// Show `surface` over the outputs, waking `waiter` when the wait for the clock has something
    /// new to answer.
    pub(crate) fn new(surface: BeforeDesk, scale: f64, waiter: Waker) -> Self {
        let placement = Placement {
            origin: (0, 0),
            size: (1, 1),
            scale,
        };
        let surface = match surface {
            BeforeDesk::Ceremony(view) => ceremony(*view, placement),
            BeforeDesk::WaitForClock(wait) => {
                let wait = wait.listening(waiter);
                let screen = wait.screen();
                Surface::WaitForClock {
                    widgets: covering(Box::new(screen), placement),
                    wait,
                }
            }
        };
        Self {
            surface,
            outputs: Vec::new(),
        }
    }

    /// Whether the layer is still over the desk.
    pub(crate) fn up(&self) -> bool {
        match &self.surface {
            Surface::Ceremony { finished, .. } => !(finished)(),
            Surface::WaitForClock { wait, .. } => !wait.done(),
        }
    }

    /// Advance what the layer shows: the wait for the clock finishes itself once the identity
    /// verifies, and gives the layer to the setup the device still needs, or back to the desk.
    pub(crate) fn step(&mut self) {
        let Surface::WaitForClock { wait, .. } = &mut self.surface else {
            return;
        };
        match wait.pump() {
            ClockWaitStep::Waiting => {}
            ClockWaitStep::Verified(None) => wait.finish(),
            ClockWaitStep::Verified(Some(view)) | ClockWaitStep::Recovery(view) => {
                self.hand_over(view);
            }
        }
    }

    /// Show `view` in place of what the layer shows now, on the same rectangle and over the same
    /// outputs.
    fn hand_over(&mut self, view: OnboardingView) {
        let placement = self.widgets().placement();
        let mut replacement = ceremony(view, placement);
        if !self.outputs.is_empty()
            && let Surface::Ceremony { widgets, .. } = &mut replacement
        {
            widgets.outputs_changed(&self.outputs);
        }
        self.surface = replacement;
    }

    /// The widget tree on the layer now.
    fn widgets(&self) -> &WidgetHost {
        match &self.surface {
            Surface::Ceremony { widgets, .. } | Surface::WaitForClock { widgets, .. } => widgets,
        }
    }

    fn widgets_mut(&mut self) -> &mut WidgetHost {
        match &mut self.surface {
            Surface::Ceremony { widgets, .. } | Surface::WaitForClock { widgets, .. } => widgets,
        }
    }

    /// Run what its door holds now, and publish what setup offers: the view does both on each pass
    /// of its own. The wait for the clock has no door and nothing of its own to publish.
    fn serve(&mut self) {
        if let Surface::Ceremony {
            widgets,
            door,
            finished,
        } = &mut self.surface
            && !(finished)()
            && (door.has_queued() || door.revision() == 0)
        {
            widgets.relayout();
        }
    }
}

/// The setup app's view on the layer over every output, taking the keyboard.
fn ceremony(view: OnboardingView, placement: Placement) -> Surface {
    let door = view.device_local_door();
    let finished = view.finished_signal();
    Surface::Ceremony {
        widgets: covering(Box::new(view), placement),
        door,
        finished,
    }
}

/// A tree covering every output and taking the keyboard: what stands in front of the desk withholds
/// it.
fn covering(root: Box<dyn ui::Widget>, placement: Placement) -> WidgetHost {
    WidgetHost::new(root, placement, false)
        .named("Setup")
        .covering_outputs()
        .with_keyboard(Rc::new(|| true))
}

impl Published for Setup {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        self.serve();
        match &self.surface {
            Surface::Ceremony { door, .. } => door.resources(),
            Surface::WaitForClock { .. } => Vec::new(),
        }
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        _look: &mut tilo_session_core::door::Look<'_>,
    ) -> Result<ResourceOperationStart, DoorError> {
        let Surface::Ceremony { door, .. } = &self.surface else {
            return Err(DoorError::Unknown(address.clone()));
        };
        let door = door.clone();
        through_door(&door, address, arguments, || self.serve())
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        self.widgets_mut()
            .host_mut()
            .a11y_nodes()
            .into_iter()
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .collect()
    }

    /// Setup's controls by name, so a tool that goes through the first login — `tilo-vmd` 's
    /// `--identity device` — presses the page the person sees, never a guessed point.
    fn controls(&mut self) -> Vec<(String, (f64, f64, f64, f64))> {
        self.serve();
        Published::controls(self.widgets_mut())
    }

    fn press(&mut self, name: &str) -> Result<(), String> {
        self.serve();
        Published::press(self.widgets_mut(), name)
    }

    fn a11y_tree(&mut self) -> Option<ui::accesskit::TreeUpdate> {
        if !self.up() {
            return None;
        }
        self.serve();
        Published::a11y_tree(self.widgets_mut())
    }

    fn a11y_name(&self) -> Option<String> {
        Published::a11y_name(self.widgets())
    }

    fn a11y_action(&mut self, target: ui::accesskit::NodeId, action: &ui::a11y::Action) -> bool {
        self.up() && Published::a11y_action(self.widgets_mut(), target, action)
    }

    fn wants_keyboard(&self) -> bool {
        self.up()
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
            self.widgets().layers()
        } else {
            Vec::new()
        }
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        // What stands before the desk withholds it: the pointer reaches nothing under the layer.
        self.up() && {
            let (ox, oy) = self.widgets().placement().origin;
            let (w, h) = self.widgets().placement().size;
            x >= f64::from(ox)
                && y >= f64::from(oy)
                && x < f64::from(ox) + f64::from(w)
                && y < f64::from(oy) + f64::from(h)
        }
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        self.widgets_mut().pointer_motion(x, y);
    }

    fn pointer_leave(&mut self) {
        self.widgets_mut().pointer_leave();
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.widgets_mut().pointer_button(button, pressed);
    }

    fn pointer_axis(&mut self, horizontal: f64, vertical: f64) {
        self.widgets_mut().pointer_axis(horizontal, vertical);
    }

    fn wants_keyboard(&self) -> bool {
        self.up()
    }

    fn key(&mut self, key: &HostKey) {
        self.widgets_mut().key(key);
    }

    fn prepare_frame(&mut self) -> bool {
        self.up() && self.widgets_mut().prepare_frame()
    }

    /// A layer that is not over the desk asks for no frames.
    fn next_deadline(&self) -> Option<std::time::Instant> {
        self.up().then(|| self.widgets().next_deadline()).flatten()
    }

    fn set_frame_interval(&mut self, interval: std::time::Duration) {
        self.widgets_mut().set_frame_interval(interval);
    }

    fn cursor(&self) -> tilo_session_core::surface::CursorIcon {
        self.widgets().cursor()
    }

    fn backdrops(&self) -> Vec<tilo_session_core::surface::BackdropRequest> {
        self.widgets().backdrops()
    }

    fn set_backdrops(&mut self, images: Vec<Option<tilo_session_core::surface::BackdropImage>>) {
        self.widgets_mut().set_backdrops(images);
    }

    fn outputs_changed(&mut self, outputs: &[tilo_session_core::surface::HostedOutput]) {
        self.outputs = outputs.to_vec();
        self.widgets_mut().outputs_changed(outputs);
    }
}
