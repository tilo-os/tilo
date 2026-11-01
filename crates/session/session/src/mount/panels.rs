//! The panels the session stands up beside the desk: quick settings over the
//! device's own backend, the setup a person goes through once, and the keys'
//! readout with the keys that answer wherever the keyboard is.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use tilo_compositor::Compositor;
use tilo_desktop::overlays::Overlays;

use crate::controls::SessionControls;
use crate::keys::{DeskKeys, GuideKeys, Invocations, PressedChords, Presses, SessionKeys};
use crate::panels::{Held, Popover, Readout};
use crate::setup::Setup;
use crate::{DeviceHost, Doors, OnboardingView, Options, PanelHost, QuickSettings, SessionCommand};

/// Quick settings as a popover under the bar, lent `panel`'s readings
/// (which `readings` then holds too).
pub(crate) fn mount_quick_settings(
    compositor: &mut Compositor,
    doors: &mut Doors,
    panel: Rc<dyn PanelHost>,
    options: &Options,
    readings: &crate::panels::Readings,
    overlays: &Overlays,
) -> bool {
    let panel = Rc::new(crate::panels::Tap {
        inner: panel,
        feed: Rc::clone(readings),
    });
    match tilo_quick_settings::prepare(panel, &options.device, &options.person, 1)
        .and_then(|component| ui::App::from_component(component, (options.presentation)()))
    {
        Ok(app) => {
            let surface = Rc::new(RefCell::new(Popover::new(
                app,
                overlays.clone(),
                options.scale,
            )));
            compositor.add_host(Box::new(Held(Rc::clone(&surface))));
            doors.add(surface);
            true
        }
        Err(error) => {
            tracing::warn!(%error, "quick settings not mounted");
            false
        }
    }
}

/// A sheet lent `seat`, hosted over the desktop and its panels with its
/// actions behind the door while it is up; the seat, when it mounted.
/// Setup, when the person has not been through it: above everything,
/// first to the keyboard, and while it is up the only surface offering
/// actions.
pub(crate) fn mount_setup(
    compositor: &mut Compositor,
    doors: &mut Doors,
    view: Option<OnboardingView>,
    person: &str,
    scale: f64,
) -> Option<Rc<RefCell<Setup>>> {
    crate::setup::lend(compositor.waker(), person);
    view.map(|view| {
        let setup = Rc::new(RefCell::new(Setup::new(view, scale)));
        compositor.add_host(Box::new(Held(Rc::clone(&setup))));
        doors.add(Rc::clone(&setup) as tilo_surface_host::Shared);
        setup
    })
}

/// What the session's keys answer with besides the shortcut table: the
/// controller the person last drove the desktop with, whether the keys
/// being routed are that controller's own, the display sheet Super+P puts
/// up, and the surfaces over the desk.
pub(crate) struct KeyContext {
    pub(crate) controller: Rc<Cell<Option<ui::ControllerFamily>>>,
    pub(crate) controller_keys: Rc<Cell<bool>>,
    pub(crate) displays: Option<tilo_sheets::Presentation>,
    /// Whether a desk widget has its card open, which Escape folds.
    pub(crate) fold: tilo_desktop::desk_widgets::DeskFold,
    /// What stands over the desk, which Escape closes from the top.
    pub(crate) overlays: Overlays,
}

/// The hardware keys' readout above the apps, and the session's own
/// resources and the keys it answers wherever the keyboard is: above every
/// surface, so offered every press first.
pub(crate) fn mount_controls(
    compositor: &mut Compositor,
    doors: &mut Doors,
    shortcuts: Rc<RefCell<ui::shortcut_table::ShortcutConfig>>,
    portal: Option<tilo_portal::global_shortcuts::Shortcuts>,
    guide: GuideKeys,
    KeyContext {
        controller,
        controller_keys,
        displays,
        fold,
        overlays,
    }: KeyContext,
    scale: f64,
) -> (Rc<RefCell<Vec<SessionCommand>>>, Invocations, PressedChords) {
    let readout = Rc::new(RefCell::new(Readout::new(scale)));
    compositor.add_host(Box::new(Held(Rc::clone(&readout))));
    doors.add(readout);
    let commands = Rc::new(RefCell::new(Vec::new()));
    doors.add(Rc::new(RefCell::new(SessionControls::new(Rc::clone(
        &commands,
    )))));
    let invocations = Invocations::default();
    let triggered = PressedChords::default();
    // The press that falls outside a surface the session placed closes it,
    // which only the input path knows the place of.
    crate::press_outside::mount(compositor, &overlays);
    compositor.add_host(Box::new(SessionKeys::new(
        shortcuts,
        Presses {
            invocations: Rc::clone(&invocations),
            triggered: Rc::clone(&triggered),
        },
        portal,
        guide,
        DeskKeys::new(fold, overlays),
        displays,
        controller,
        controller_keys,
    )));
    (commands, invocations, triggered)
}

/// The device's own backend, when the session runs one, and the quick
/// settings panel over it.
pub(crate) fn mount_device(
    compositor: &mut Compositor,
    doors: &mut Doors,
    options: &mut Options,
    readings: &crate::panels::Readings,
    overlays: &Overlays,
) -> (Option<DeviceHost>, bool) {
    let (device, panel) = match options.quick_settings.take() {
        Some(QuickSettings::Device(make)) => {
            let waker = compositor.waker();
            let device = make(Arc::new(move || waker.wake_by_ref()));
            let panel = device.panel();
            (Some(device), Some(panel))
        }
        Some(QuickSettings::Lent(panel)) => (None, Some(panel)),
        None => (None, None),
    };
    let quick_settings = panel.is_some_and(|panel| {
        mount_quick_settings(compositor, doors, panel, options, readings, overlays)
    });
    (device, quick_settings)
}
