//! What stands over everything before the session is used: the first-use ceremony
//! ([`Ceremony`]) on a layer covering the outputs, above the desktop and every window, taking the
//! keyboard until the person finished it, or the wait for the clock to admit this device's
//! certificate, which goes away by itself, or what a login on Tilo's own computer shows when its
//! device's daemon did not start it.
//!
//! The ceremony is the session's; setup's app (`apps/tl/onboarding`) draws it. The session
//! publishes it as the resource `setup`, and runs this build's copy of the app's view over its
//! reading in a [`PartView`] covering the outputs: setup is trusted, so no package a person
//! installed or bound draws it. What the view asks it asks through setup's actions, which a script
//! or an agent here calls the same way. When setup needs a file, the session puts the question to
//! the person in the shared file picker over the view. What setup needs of the session it reads
//! through [`tilo_onboarding_session::services::SessionServices`].

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Mutex, PoisonError};
use std::task::Waker;

use crate::platform::surfaces::{Placement, WidgetSurface};
use crate::shell::part_view::{PartSurface, PartView, Spec};
use tilo_actions::{ActionAddress, ActionOutput, ApiError, ResourceSpec};
use tilo_session_core::surface::{Scroll, ShellSurface, SurfaceLayer};
use tilo_session_core::surface::{SurfaceIme, SurfaceKey, SurfaceTextInput};

use tilo_onboarding_session::ceremony::{self, Ceremony, OnboardingEffect};
use tilo_session_core::surfaces::Published;
use tilo_ui_base::host_operation::ResourceOperationStart;
use ui::widget::{FilePicker, FilePickerOutcome};

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
    Ceremony(Box<Ceremony>),
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
    /// The ceremony, and setup's app drawing it.
    Ceremony(Box<Drawn>),
    /// The wait for the clock, which has no actions of its own.
    WaitForClock {
        widgets: WidgetSurface,
        wait: ClockWait,
    },
    /// The login the daemon did not start, which has no actions beyond its two buttons.
    NotStarted { widgets: WidgetSurface },
}

/// Setup's part, as the overlay stack and assistive technology name it.
const PART: &str = "setup";

/// The ceremony, drawn by setup's app over its reading, with the file picker standing over the
/// view while setup asks the person for a file.
pub(crate) struct Drawn {
    ceremony: Ceremony,
    part: PartView,
    /// The shared file picker, while setup asks for a file.
    picker: Option<WidgetSurface>,
    /// The reading the view has now.
    reading: String,
    /// The ceremony's epoch the view was opened at: when the ceremony forgets a secret, the view
    /// is opened again, so no field still shows what was typed into it.
    epoch: u64,
    person: String,
    waker: Waker,
    /// The outputs the layer covers, as the compositor last told it.
    outputs: Vec<tilo_session_core::surface::SurfaceOutput>,
    scale: f64,
    /// The view could not be opened: the layer shows the session's own failure in its place.
    broken: bool,
}

impl Drawn {
    fn new(mut ceremony: Ceremony, scale: f64, waker: Waker) -> Self {
        ceremony.step();
        let mut drawn = Self {
            ceremony,
            part: PartView::new(PART, scale, Rc::new(|| true)),
            picker: None,
            reading: String::new(),
            epoch: 0,
            person: LENT
                .person
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            waker,
            outputs: Vec::new(),
            scale,
            broken: false,
        };
        drawn.open();
        drawn
    }

    /// Run setup's view over the ceremony's reading now. A view that does not come up leaves
    /// setup [`broken`](Self::broken), and the layer says the session did not start.
    fn open(&mut self) {
        if let Err(why) = self.try_open() {
            tracing::error!(%why, "setup's view does not run");
            self.broken = true;
        }
    }

    fn try_open(&mut self) -> Result<(), String> {
        let page = self.ceremony.page();
        self.epoch = page.epoch;
        self.reading = page.reading(true).to_string();
        let compiled = tilolang_tilo::onboarding::compile()?;
        let dir = crate::parts::shipped_package(&compiled, "setup")
            .ok_or("this build's setup could not be written to run")?;
        let spec = Spec {
            app: tilolang_tilo::onboarding::SETUP_APP.to_owned(),
            view: tilolang_tilo::onboarding::SETUP_VIEW.to_owned(),
            dir,
            surface: PartSurface {
                id: ceremony::RESOURCE.to_owned(),
                kind: ceremony::KIND.to_owned(),
                app: tilolang_tilo::platform::SESSION_APP.to_owned(),
                title: "Setup".to_owned(),
                text: String::new(),
            },
        };
        let waker = self.waker.clone();
        self.part.open(
            &spec,
            &self.person,
            std::sync::Arc::new(move || waker.wake_by_ref()),
            &self.reading,
        )?;
        if !self.outputs.is_empty() {
            self.part.outputs_changed(&self.outputs);
        }
        Ok(())
    }

    /// Take what the view asked, move the ceremony on, and show where it is.
    fn pass(&mut self) {
        let published = [ceremony::resource()];
        let mut asked = Vec::new();
        self.part.serve(|ask| {
            let arguments: serde_json::Map<String, serde_json::Value> = ask
                .arguments
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect();
            let effect = tilo_resource::effect::decode::<OnboardingEffect>(
                &published,
                &ask.resource,
                &ask.action,
                &arguments,
            )
            .map_err(|error| error.to_string())?;
            asked.push(effect);
            Ok(())
        });
        let moved = !asked.is_empty();
        for effect in asked {
            if let Err(why) = self.ceremony.act(effect) {
                tracing::warn!(why, "setup refused what its view asked");
            }
        }
        self.settle(moved);
    }

    /// Move the ceremony on when something reached it, answer the file it asks for, and hand the
    /// view the reading it shows now.
    fn settle(&mut self, moved: bool) {
        let due = self
            .ceremony
            .next_deadline()
            .is_some_and(|deadline| deadline <= std::time::Instant::now());
        if moved || due || self.ceremony.needs_step() {
            self.ceremony.step();
        }
        self.follow_file_ask();
        let page = self.ceremony.page();
        if page.epoch != self.epoch {
            self.open();
            return;
        }
        let reading = page.reading(true).to_string();
        if reading != self.reading {
            self.part.publish(&reading);
            self.reading = reading;
        }
    }

    /// Put setup's file question to the person in the shared picker, and hand the ceremony what
    /// they answered.
    fn follow_file_ask(&mut self) {
        let Some(ask) = self.ceremony.file_ask() else {
            self.picker = None;
            return;
        };
        let picker = self.picker.get_or_insert_with(|| {
            let mut widgets = WidgetSurface::new(
                Box::new(ask.picker()),
                Placement {
                    origin: (0, 0),
                    size: (1, 1),
                    scale: self.scale,
                },
                false,
            )
            .named("Setup")
            .with_keyboard(Rc::new(|| true));
            if let Some(placement) = picker_placement(&self.outputs, self.scale) {
                widgets.set_placement(placement);
            }
            widgets
        });
        let mut outcome = None;
        picker.host_mut().change_root(|root| {
            root.as_any_mut()
                .and_then(|any| any.downcast_mut::<FilePicker>())
                .is_some_and(|picker| {
                    let moved = picker.poll_lookups();
                    outcome = picker.take_outcome();
                    moved
                })
        });
        let Some(outcome) = outcome else {
            return;
        };
        let files = match outcome {
            FilePickerOutcome::Selected(paths) => paths
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            FilePickerOutcome::Cancelled => Vec::new(),
        };
        self.picker = None;
        self.ceremony.files_answered(files);
        self.ceremony.step();
    }

    /// What takes the pointer and the keys: the picker while it stands, else the view.
    fn front(&self) -> &dyn ShellSurface {
        match &self.picker {
            Some(picker) => picker,
            None => &self.part,
        }
    }

    fn front_mut(&mut self) -> &mut dyn ShellSurface {
        match &mut self.picker {
            Some(picker) => picker,
            None => &mut self.part,
        }
    }

    fn published_mut(&mut self) -> &mut dyn Published {
        match &mut self.picker {
            Some(picker) => picker,
            None => &mut self.part,
        }
    }

    fn outputs_changed(&mut self, outputs: &[tilo_session_core::surface::SurfaceOutput]) {
        self.outputs = outputs.to_vec();
        self.part.outputs_changed(outputs);
        if let (Some(picker), Some(placement)) =
            (self.picker.as_mut(), picker_placement(outputs, self.scale))
        {
            picker.set_placement(placement);
        }
    }
}

/// Where the file picker stands over `outputs`: centred, most of what a person sees, never wider
/// or taller than it.
fn picker_placement(
    outputs: &[tilo_session_core::surface::SurfaceOutput],
    scale: f64,
) -> Option<Placement> {
    let (x0, y0, x1, y1) = outputs
        .iter()
        .map(|output| {
            let (x, y, w, h) = output.rect;
            (x, y, x.saturating_add(w), y.saturating_add(h))
        })
        .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))?;
    let (width, height) = (
        f64::from(x1.saturating_sub(x0)),
        f64::from(y1.saturating_sub(y0)),
    );
    let w = (width * 0.78).clamp(420.0, 900.0).min(width);
    let h = (height * 0.78).clamp(320.0, 640.0).min(height);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a rectangle on the desktop, in whole pixels"
    )]
    Some(Placement {
        origin: (
            x0.saturating_add(((width - w) / 2.0) as i32),
            y0.saturating_add(((height - h) / 2.0) as i32),
        ),
        size: ((w as u32).max(1), (h as u32).max(1)),
        scale,
    })
}

/// The setup view on its layer, while the person has not finished it.
pub(crate) struct Setup {
    surface: Surface,
    /// The outputs this layer covers, as the compositor last told a view: replayed to a view that
    /// replaces another, which is only told when the outputs themselves change.
    outputs: Vec<tilo_session_core::surface::SurfaceOutput>,
    scale: f64,
    waker: Waker,
    /// Where the person's choice on the session's failure goes: Log out or Restart.
    commands: Rc<RefCell<Vec<SessionCommand>>>,
}

impl Setup {
    /// Show `surface` over the outputs, waking `waiter` when the wait for the clock or setup has
    /// something new to answer. What the person chooses on a login the daemon did not start goes
    /// to `commands`, as the same choice from the power menu does.
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
            BeforeShell::Ceremony(ceremony) => {
                Surface::Ceremony(Box::new(Drawn::new(*ceremony, scale, waiter.clone())))
            }
            BeforeShell::WaitForClock(wait) => {
                let wait = wait.listening(waiter.clone());
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
        let mut setup = Self {
            surface,
            outputs: Vec::new(),
            scale,
            waker: waiter,
            commands: Rc::clone(commands),
        };
        setup.give_up_if_broken();
        setup
    }

    /// When setup's view could not come up, say so as a login the daemon did not start does:
    /// nothing done here would be kept, and the way on is to log out or restart.
    fn give_up_if_broken(&mut self) {
        if !matches!(&self.surface, Surface::Ceremony(drawn) if drawn.broken) {
            return;
        }
        let placement = Placement {
            origin: (0, 0),
            size: (1, 1),
            scale: self.scale,
        };
        let mut widgets = covering(Box::new(not_started(&self.commands)), placement);
        if !self.outputs.is_empty() {
            widgets.outputs_changed(&self.outputs);
        }
        self.surface = Surface::NotStarted { widgets };
    }

    /// Whether the layer is still over the shell.
    pub(crate) fn up(&self) -> bool {
        match &self.surface {
            Surface::Ceremony(drawn) => !drawn.ceremony.finished(),
            Surface::WaitForClock { wait, .. } => !wait.done(),
            Surface::NotStarted { .. } => true,
        }
    }

    /// Advance what the layer shows: the wait for the clock finishes itself once the identity
    /// verifies, and gives the layer to the setup the device still needs, or back to the session;
    /// setup takes what was asked of it.
    pub(crate) fn step(&mut self) {
        match &mut self.surface {
            Surface::Ceremony(drawn) => drawn.pass(),
            Surface::WaitForClock { wait, .. } => match wait.pump() {
                ClockWaitStep::Waiting => {}
                ClockWaitStep::Verified(None) => wait.finish(),
                ClockWaitStep::Verified(Some(ceremony)) | ClockWaitStep::Recovery(ceremony) => {
                    self.hand_over(ceremony);
                }
            },
            Surface::NotStarted { .. } => {}
        }
        self.give_up_if_broken();
    }

    /// Show `ceremony` in place of what the layer shows now, over the same outputs.
    fn hand_over(&mut self, ceremony: Ceremony) {
        let mut drawn = Drawn::new(ceremony, self.scale, self.waker.clone());
        if !self.outputs.is_empty() {
            drawn.outputs_changed(&self.outputs);
        }
        self.surface = Surface::Ceremony(Box::new(drawn));
        self.give_up_if_broken();
    }

    /// What takes the pointer and the keys now.
    fn front(&self) -> &dyn ShellSurface {
        match &self.surface {
            Surface::Ceremony(drawn) => drawn.front(),
            Surface::WaitForClock { widgets, .. } | Surface::NotStarted { widgets } => widgets,
        }
    }

    fn front_mut(&mut self) -> &mut dyn ShellSurface {
        match &mut self.surface {
            Surface::Ceremony(drawn) => drawn.front_mut(),
            Surface::WaitForClock { widgets, .. } | Surface::NotStarted { widgets } => widgets,
        }
    }

    fn published_mut(&mut self) -> &mut dyn Published {
        match &mut self.surface {
            Surface::Ceremony(drawn) => {
                drawn.pass();
                drawn.published_mut()
            }
            Surface::WaitForClock { widgets, .. } | Surface::NotStarted { widgets } => widgets,
        }
    }
}

/// What a login the daemon did not start shows: that nothing here would be kept, logging out to
/// sign in again, and restarting to reach Repair Mode from the boot menu.
fn not_started(commands: &Rc<RefCell<Vec<SessionCommand>>>) -> ui::widget::Container {
    let log_out = Rc::clone(commands);
    let restart = Rc::clone(commands);
    let face = ui::widget::arrival_failure(
        t("session-not-started-title"),
        t("session-not-started-body"),
    )
    .with_action(
        ui::widget::Button::new("session-not-started-log-out", &t("session-power-log-out"))
            .on_click(move || log_out.borrow_mut().push(SessionCommand::LogOut)),
    )
    .with_secondary_action(
        ui::widget::Button::new("session-not-started-restart", &t("session-power-restart"))
            .role(ui::widget::ButtonRole::Plain)
            .on_click(move || {
                restart
                    .borrow_mut()
                    .push(SessionCommand::Power(Effect::Restart));
            }),
    );
    on_ground(Box::new(ui::widget::WindowFailure::new(face)))
}

/// `content` on the person's wallpaper, covering every output: what stands before the session is
/// opaque, so nothing of the desktop under it shows through.
pub(crate) fn on_ground(content: Box<dyn tilo_ui_base::Widget>) -> ui::widget::Container {
    ui::widget::Container::new()
        .filling()
        .with_no_padding()
        .with_child(Box::new(
            ui::widget::Stack::new()
                .push(Box::new(ui::widget::Backdrop::new()))
                .push(content),
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
        match &self.surface {
            Surface::Ceremony(_) if self.up() => vec![ceremony::resource()],
            Surface::Ceremony(_) | Surface::WaitForClock { .. } | Surface::NotStarted { .. } => {
                Vec::new()
            }
        }
    }

    /// Setup's actions, from whoever calls them: the same the view's controls ask for.
    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        _look: &mut tilo_session_core::surfaces::Look<'_>,
    ) -> Result<ResourceOperationStart, ApiError> {
        let up = self.up();
        let Surface::Ceremony(drawn) = &mut self.surface else {
            return Err(ApiError::Unknown(address.clone()));
        };
        if !up {
            return Err(ApiError::Unknown(address.clone()));
        }
        let effect = tilo_resource::effect::decode::<OnboardingEffect>(
            &[ceremony::resource()],
            &address.resource,
            &address.action,
            arguments,
        )?;
        drawn
            .ceremony
            .act(effect)
            .map_err(ApiError::InvalidArguments)?;
        drawn.settle(true);
        self.give_up_if_broken();
        Ok(ResourceOperationStart::Completed(ActionOutput::default()))
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        self.published_mut().accessible_labels()
    }

    /// Setup's controls by name, so a tool that goes through the first login — `tilo-device-step`'s
    /// `--identity device` — presses the page the person sees, never a guessed point.
    fn controls(&mut self) -> Vec<ui::window_targets::WindowControl> {
        self.published_mut().controls()
    }

    fn press(&mut self, name: &str) -> Result<(), String> {
        self.published_mut().press(name)?;
        if let Surface::Ceremony(drawn) = &mut self.surface {
            // What the press asked is setup's to take now, as a pass of the session would.
            drawn.part.prepare_frame();
            drawn.pass();
        }
        self.give_up_if_broken();
        Ok(())
    }

    fn a11y_tree(&mut self) -> Option<ui::accesskit::TreeUpdate> {
        if !self.up() {
            return None;
        }
        self.published_mut().a11y_tree()
    }

    fn a11y_name(&self) -> Option<String> {
        Some("Setup".to_owned())
    }

    fn a11y_action(
        &mut self,
        target: ui::accesskit::NodeId,
        action: &tilo_ui_base::a11y::Action,
    ) -> bool {
        self.up() && self.published_mut().a11y_action(target, action)
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
        if !self.up() {
            return Vec::new();
        }
        match &self.surface {
            Surface::Ceremony(drawn) => {
                let mut layers = drawn.part.layers();
                if let Some(picker) = &drawn.picker {
                    layers.extend(picker.layers());
                }
                layers
            }
            Surface::WaitForClock { widgets, .. } | Surface::NotStarted { widgets } => {
                widgets.layers()
            }
        }
    }

    fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
        // What stands before the session withholds it: the pointer reaches nothing under the
        // layer, which covers every output.
        self.up()
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        self.front_mut().pointer_motion(x, y);
    }

    fn pointer_leave(&mut self) {
        self.front_mut().pointer_leave();
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.front_mut().pointer_button(button, pressed);
    }

    fn pointer_axis(&mut self, scroll: Scroll) {
        self.front_mut().pointer_axis(scroll);
    }

    fn pointer_modifiers(&mut self, held: tilo_session_core::surface::KeyModifiers) {
        self.front_mut().pointer_modifiers(held);
    }

    fn takes_fingers(&self, _x: f64, _y: f64) -> bool {
        false
    }

    fn wants_keyboard(&self) -> bool {
        self.up()
    }

    fn key(&mut self, key: &SurfaceKey) {
        self.front_mut().key(key);
    }

    fn text_input(&self) -> Option<SurfaceTextInput> {
        self.front().text_input()
    }

    fn ime(&mut self, event: SurfaceIme) {
        self.front_mut().ime(event);
    }

    fn prepare_frame(&mut self) -> bool {
        self.give_up_if_broken();
        if !self.up() {
            return false;
        }
        match &mut self.surface {
            Surface::Ceremony(drawn) => {
                let drew = drawn.part.prepare_frame();
                drawn.pass();
                let drew = drawn.part.prepare_frame() || drew;
                drawn
                    .picker
                    .as_mut()
                    .is_some_and(ShellSurface::prepare_frame)
                    || drew
            }
            Surface::WaitForClock { widgets, .. } | Surface::NotStarted { widgets } => {
                widgets.prepare_frame()
            }
        }
    }

    /// A layer that is not over the shell asks for no frames.
    fn next_deadline(&self) -> Option<std::time::Instant> {
        if !self.up() {
            return None;
        }
        match &self.surface {
            Surface::Ceremony(drawn) => [
                drawn.part.next_deadline(),
                drawn.ceremony.next_deadline(),
                drawn.picker.as_ref().and_then(ShellSurface::next_deadline),
            ]
            .into_iter()
            .flatten()
            .min(),
            Surface::WaitForClock { widgets, .. } | Surface::NotStarted { widgets } => {
                widgets.next_deadline()
            }
        }
    }

    fn set_frame_interval(&mut self, interval: std::time::Duration) {
        match &mut self.surface {
            Surface::Ceremony(drawn) => {
                drawn.part.set_frame_interval(interval);
                if let Some(picker) = drawn.picker.as_mut() {
                    picker.set_frame_interval(interval);
                }
            }
            Surface::WaitForClock { widgets, .. } | Surface::NotStarted { widgets } => {
                widgets.set_frame_interval(interval);
            }
        }
    }

    fn cursor(&self) -> tilo_session_core::surface::CursorIcon {
        self.front().cursor()
    }

    fn backdrops(&self) -> Vec<tilo_session_core::surface::BackdropRequest> {
        match &self.surface {
            Surface::Ceremony(drawn) => drawn.part.backdrops(),
            Surface::WaitForClock { widgets, .. } | Surface::NotStarted { widgets } => {
                widgets.backdrops()
            }
        }
    }

    fn set_backdrops(&mut self, images: Vec<Option<tilo_session_core::surface::BackdropImage>>) {
        match &mut self.surface {
            Surface::Ceremony(drawn) => drawn.part.set_backdrops(images),
            Surface::WaitForClock { widgets, .. } | Surface::NotStarted { widgets } => {
                widgets.set_backdrops(images);
            }
        }
    }

    fn outputs_changed(&mut self, outputs: &[tilo_session_core::surface::SurfaceOutput]) {
        self.outputs = outputs.to_vec();
        match &mut self.surface {
            Surface::Ceremony(drawn) => drawn.outputs_changed(outputs),
            Surface::WaitForClock { widgets, .. } | Surface::NotStarted { widgets } => {
                widgets.outputs_changed(outputs);
            }
        }
    }
}
