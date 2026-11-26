//! What stands over everything before the session is used: the first-use ceremony
//! ([`tilo_onboarding_session::view::OnboardingView`]) on a layer covering the outputs, above the
//! desktop and every window, taking the keyboard until the person finished it, or the wait for the
//! clock to admit this device's certificate, which goes away by itself, or what a login on Tilo's
//! own computer shows when its device's daemon did not start it. Its actions are the setup
//! app's, through its API; what setup needs of the session it reads through
//! [`tilo_onboarding_session::services::SessionServices`].

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Mutex, PoisonError};
use std::task::Waker;

use crate::platform::surfaces::{Placement, WidgetSurface};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_session_core::surface::SurfaceKey;
use tilo_session_core::surface::{Scroll, ShellSurface, SurfaceLayer};

use tilo_onboarding_session::view::OnboardingView;
use tilo_session_core::surfaces::{Published, through_api};
use ui::device_local_api::SharedApi;
use tilo_ui_base::host_operation::ResourceOperationStart;

use crate::clock_wait::{ClockWait, ClockWaitStep};
use crate::controls::SessionCommand;
use crate::i18n::t;
use crate::power_menu::Effect;

/// What this session lends setup. One per process, as setup reads it through a process-wide hook;
/// each session started points it at itself.
struct Lent {
    waker: Mutex<Option<Waker>>,
    person: Mutex<String>,
}

static LENT: Lent = Lent {
    waker: Mutex::new(None),
    person: Mutex::new(String::new()),
};

impl tilo_onboarding_session::services::SessionServices for Lent {
    fn loop_waker(&self) -> Waker {
        self.waker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| Waker::noop().clone())
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
    tilo_onboarding_session::services::install(&LENT);
}

/// What this session shows before its session is used.
pub enum BeforeShell {
    /// Setup the person goes through once: the first use of this device, or the recovery of its
    /// identity.
    Ceremony(Box<OnboardingView>),
    /// The wait for the clock to admit this device's certificate. It goes away by itself, and what
    /// a verified identity opens takes its place.
    WaitForClock(ClockWait),
    /// A login on a computer whose system daemon starts every person's shell, which it did not
    /// start: nothing done here would be kept, so the person logs out and in again, or restarts
    /// into Repair Mode. It stays until they do.
    NotStarted,
}

/// What the layer shows, while it shows anything.
enum Surface {
    /// The setup app's own view: its API publishes its actions, and its own signal says when the
    /// person finished it.
    Ceremony {
        widgets: WidgetSurface,
        api: SharedApi,
        finished: Rc<dyn Fn() -> bool>,
    },
    /// The wait for the clock, which has no actions of its own.
    WaitForClock {
        widgets: WidgetSurface,
        wait: ClockWait,
    },
    /// The login the daemon did not start, which has no actions beyond its two buttons.
    NotStarted { widgets: WidgetSurface },
}

/// The setup view on its layer, while the person has not finished it.
pub(crate) struct Setup {
    surface: Surface,
    /// The outputs this layer covers, as the compositor last told a view: replayed to a view that
    /// replaces another, which is only told when the outputs themselves change.
    outputs: Vec<tilo_session_core::surface::SurfaceOutput>,
}

impl Setup {
    /// Show `surface` over the outputs, waking `waiter` when the wait for the clock has something
    /// new to answer. What the person chooses on a login the daemon did not start goes to
    /// `commands`, as the same choice from the power menu does.
    pub(crate) fn new(
        surface: BeforeShell,
        scale: f64,
        waiter: Waker,
        commands: &Rc<RefCell<Vec<SessionCommand>>>,
    ) -> Self {
        let placement = Placement {
            origin: (0, 0),
            size: (1, 1),
            scale,
        };
        let surface = match surface {
            BeforeShell::Ceremony(view) => ceremony(*view, placement),
            BeforeShell::WaitForClock(wait) => {
                let wait = wait.listening(waiter);
                let screen = wait.screen();
                Surface::WaitForClock {
                    widgets: covering(Box::new(screen), placement),
                    wait,
                }
            }
            BeforeShell::NotStarted => Surface::NotStarted {
                widgets: covering(Box::new(not_started(commands)), placement),
            },
        };
        Self {
            surface,
            outputs: Vec::new(),
        }
    }

    /// Whether the layer is still over the shell.
    pub(crate) fn up(&self) -> bool {
        match &self.surface {
            Surface::Ceremony { finished, .. } => !(finished)(),
            Surface::WaitForClock { wait, .. } => !wait.done(),
            Surface::NotStarted { .. } => true,
        }
    }

    /// Advance what the layer shows: the wait for the clock finishes itself once the identity
    /// verifies, and gives the layer to the setup the device still needs, or back to the session.
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
    fn widgets(&self) -> &WidgetSurface {
        match &self.surface {
            Surface::Ceremony { widgets, .. }
            | Surface::WaitForClock { widgets, .. }
            | Surface::NotStarted { widgets } => widgets,
        }
    }

    fn widgets_mut(&mut self) -> &mut WidgetSurface {
        match &mut self.surface {
            Surface::Ceremony { widgets, .. }
            | Surface::WaitForClock { widgets, .. }
            | Surface::NotStarted { widgets } => widgets,
        }
    }

    /// Run what its API holds now, and publish what setup offers: the view does both on each pass
    /// of its own. The wait for the clock has no API and nothing of its own to publish.
    fn serve(&mut self) {
        if let Surface::Ceremony {
            widgets,
            api,
            finished,
        } = &mut self.surface
            && !(finished)()
            && (api.has_queued() || api.revision() == 0)
        {
            widgets.relayout();
        }
    }
}

/// The setup app's view on the layer over every output, taking the keyboard.
fn ceremony(view: OnboardingView, placement: Placement) -> Surface {
    let api = view.device_local_api();
    let finished = view.finished_signal();
    Surface::Ceremony {
        widgets: covering(Box::new(view), placement),
        api,
        finished,
    }
}

/// What a login the daemon did not start shows: that nothing here would be kept, logging out to
/// sign in again, and restarting to reach Repair Mode from the boot menu.
fn not_started(commands: &Rc<RefCell<Vec<SessionCommand>>>) -> ui::widget::Container {
    let log_out = Rc::clone(commands);
    let restart = Rc::clone(commands);
    ui::widget::Container::new()
        .filling()
        .with_background()
        .with_centered_max_width(560.0)
        .with_child(Box::new(
            ui::widget::arrival_failure(
                t("session-not-started-title"),
                t("session-not-started-body"),
            )
            .with_action(
                ui::widget::Button::new("session-not-started-log-out", &t("session-power-log-out"))
                    .on_click(move || log_out.borrow_mut().push(SessionCommand::LogOut)),
            )
            .with_secondary_action(
                ui::widget::Button::new("session-not-started-restart", &t("session-power-restart"))
                    .on_click(move || {
                        restart
                            .borrow_mut()
                            .push(SessionCommand::Power(Effect::Restart));
                    }),
            ),
        ))
}

/// A tree covering every output and taking the keyboard: what stands in front of the session withholds
/// it.
fn covering(root: Box<dyn tilo_ui_base::Widget>, placement: Placement) -> WidgetSurface {
    WidgetSurface::new(root, placement, false)
        .named("Setup")
        .covering_outputs()
        .with_keyboard(Rc::new(|| true))
}

impl Published for Setup {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        self.serve();
        match &self.surface {
            Surface::Ceremony { api, .. } => api.resources(),
            Surface::WaitForClock { .. } | Surface::NotStarted { .. } => Vec::new(),
        }
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        _look: &mut tilo_session_core::surfaces::Look<'_>,
    ) -> Result<ResourceOperationStart, ApiError> {
        let Surface::Ceremony { api, .. } = &self.surface else {
            return Err(ApiError::Unknown(address.clone()));
        };
        let api = api.clone();
        through_api(&api, address, arguments, || self.serve())
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        self.widgets_mut()
            .host_mut()
            .a11y_nodes()
            .into_iter()
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .collect()
    }

    /// Setup's controls by name, so a tool that goes through the first login — `tilo-device-step`'s
    /// `--identity device` — presses the page the person sees, never a guessed point.
    fn controls(&mut self) -> Vec<ui::window_targets::WindowControl> {
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

    fn a11y_action(&mut self, target: ui::accesskit::NodeId, action: &tilo_ui_base::a11y::Action) -> bool {
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

impl ShellSurface for Setup {
    fn layers(&self) -> Vec<SurfaceLayer> {
        if self.up() {
            self.widgets().layers()
        } else {
            Vec::new()
        }
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        // What stands before the session withholds it: the pointer reaches nothing under the layer.
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

    fn pointer_axis(&mut self, scroll: Scroll) {
        self.widgets_mut().pointer_axis(scroll);
    }

    fn pointer_modifiers(&mut self, held: tilo_session_core::surface::KeyModifiers) {
        self.widgets_mut().pointer_modifiers(held);
    }

    fn wants_keyboard(&self) -> bool {
        self.up()
    }

    fn key(&mut self, key: &SurfaceKey) {
        self.widgets_mut().key(key);
    }

    fn prepare_frame(&mut self) -> bool {
        self.up() && self.widgets_mut().prepare_frame()
    }

    /// A layer that is not over the shell asks for no frames.
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

    fn outputs_changed(&mut self, outputs: &[tilo_session_core::surface::SurfaceOutput]) {
        self.outputs = outputs.to_vec();
        self.widgets_mut().outputs_changed(outputs);
    }
}
