//! The panels the session stands up beside the shell: the setup a person goes through once, and the
//! keys' readout with the keys that answer wherever the keyboard is.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use tilo_compositor::Compositor;
use tilo_desktop::overlays::Overlays;

use crate::controls::ControlsReading;
use tilo_sheets::shortcut_guide::State as GuideState;

use crate::keys::{ControllerKeys, Invocations, PressedChords, Presses, SessionKeys, ShellKeys};
use crate::panels::{ActivityLayer, Readout, ReadoutPlaces, Reserved};
use crate::platform::keys::NativeKeys;
use crate::setup::Setup;
use crate::{BeforeShell, SessionCommand};
use tilo_session_core::surfaces::Surfaces;

/// What stands before the shell, when it stands there at all: above everything, first to the
/// keyboard, and while it is up the only surface offering actions.
pub(crate) fn mount_setup(
    compositor: &mut Compositor,
    surfaces: &mut Surfaces,
    surface: Option<BeforeShell>,
    person: &str,
    scale: f64,
) -> Option<Rc<RefCell<Setup>>> {
    let waiter = compositor.waker();
    crate::setup::lend(waiter.clone(), person);
    surface.map(|surface| {
        let setup = Rc::new(RefCell::new(Setup::new(surface, scale, waiter)));
        compositor.add_surface(setup.clone());
        surfaces.add(Rc::clone(&setup) as tilo_session_core::surfaces::Shared);
        setup
    })
}

/// What the session's keys answer with besides the shortcut table: the controller the person last
/// drove the desktop with, whether the keys being routed are that controller's own, the display
/// sheet Super+P puts up, and the surfaces over the shell.
pub(crate) struct KeyContext {
    pub(crate) controller: Rc<Cell<Option<ui::ControllerFamily>>>,
    pub(crate) controller_keys: Rc<Cell<bool>>,
    pub(crate) sheets: crate::keys::SheetKeys,
    /// Whether a Desktop widget has its card open, which Escape folds.
    pub(crate) fold: tilo_desktop::widgets::Fold,
    /// What stands over the shell, which Escape closes from the top.
    pub(crate) overlays: Overlays,
    /// When each display's bar shows over the window covering it, which Super asks for.
    pub(crate) reveals: Rc<RefCell<tilo_desktop::topbar::Reveals>>,
    /// What the keys ask the panel's own app, when this device draws it.
    pub(crate) panel: Option<Rc<RefCell<Vec<&'static str>>>>,
}

/// The hardware keys' readout above the apps, and the session's own resources and the keys it
/// answers wherever the keyboard is: above every surface, so offered every press first. What
/// mounting the session's controls hands the session back: the commands the key surface raises and
/// the state its own resources read, the invocations it answers, the chords it pressed through,
/// and the readout itself.
pub(crate) type Controls = (
    Rc<RefCell<Vec<SessionCommand>>>,
    Rc<RefCell<ControlsReading>>,
    Invocations,
    PressedChords,
    Rc<RefCell<Readout>>,
);

/// The desktop's one shortcut table, shared: one reader matches a press against it, another shows
/// its chord, and a person's rebinding reaches all of them.
pub(crate) type Shortcuts = Rc<RefCell<ui::shortcut_table::ShortcutConfig>>;

pub(crate) fn mount_controls(
    compositor: &mut Compositor,
    surfaces: &mut Surfaces,
    shortcuts: Shortcuts,
    portal: Option<tilo_portal::global_shortcuts::Shortcuts>,
    guide: Rc<GuideState>,
    KeyContext {
        controller,
        controller_keys,
        sheets,
        fold,
        overlays,
        reveals,
        panel,
    }: KeyContext,
    scale: f64,
    (band, places): (Reserved, &ReadoutPlaces),
) -> Controls {
    let readout = Rc::new(RefCell::new(Readout::new(scale, Rc::clone(places), band)));
    compositor.add_surface(readout.clone());
    let shared: tilo_session_core::surfaces::Shared = readout.clone();
    surfaces.add(shared);
    let commands = Rc::new(RefCell::new(Vec::new()));
    // The controls are a service of the session's, mounted beside the other services so an app
    // reads and invokes them as it does each of those: what the loop keeps of their state is read
    // here, before that mounts.
    let controls_reading = Rc::new(RefCell::new(ControlsReading::default()));
    let invocations = Invocations::default();
    let triggered = PressedChords::default();
    // The press that falls outside a surface the session placed closes it,
    // which only the input path knows the place of.
    crate::press_outside::mount(compositor, &overlays);
    compositor.add_surface(Rc::new(RefCell::new(NativeKeys::new(
        SessionKeys::new(
            shortcuts,
            Presses {
                invocations: Rc::clone(&invocations),
                triggered: Rc::clone(&triggered),
            },
            guide,
            ShellKeys::new(fold, overlays).with_panel(panel),
            sheets,
            ControllerKeys {
                last: controller,
                own: controller_keys,
            },
            reveals,
        ),
        portal,
        Rc::clone(&commands),
    ))));
    (commands, controls_reading, invocations, triggered, readout)
}

/// The floating layer over the shell: every app's activity as a card, which the session's own
/// reading of the board fills and what the person does on it reaches the apps through.
/// Under the shell nothing of it is mounted: the old shell draws the timer's own floating panel, so
/// a layer of ours would draw a second card over it.
pub(crate) fn mount_activities(
    compositor: &mut Compositor,
    surfaces: &mut Surfaces,
    reader: &Rc<crate::chrome::Activities>,
    cards: &crate::cards::Cards,
    (banners, readout): (Reserved, Reserved),
    (places, shell, overlays): (
        &ReadoutPlaces,
        &tilo_desktop_surface::DesktopHandle,
        &tilo_desktop::overlays::Overlays,
    ),
    options: &crate::Options,
) -> Option<tilo_activities::Link> {
    if options.shell {
        return None;
    }
    let link = tilo_activities::Link::new();
    match tilo_activities::prepare(link.clone(), &options.device, &options.person)
        .and_then(|component| ui::App::from_component(component, (options.presentation)()))
    {
        Ok(app) => {
            let surface = Rc::new(RefCell::new(ActivityLayer::new(
                app,
                link.clone(),
                Rc::clone(reader),
                cards.clone(),
                (banners, readout),
                Rc::clone(places),
                (shell.clone(), overlays.clone()),
                options.scale,
            )));
            compositor.add_surface(surface.clone());
            surfaces.add(surface);
            Some(link)
        }
        Err(error) => {
            tracing::warn!(%error, "activity layer not mounted");
            None
        }
    }
}
