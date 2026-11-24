//! The panels the session stands up beside the shell: the setup a person goes through once, and the
//! keys that answer wherever the keyboard is.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use tilo_compositor::Compositor;
use tilo_session_core::desktop::overlays::Overlays;

use crate::controls::ControlsReading;
use tilo_sheets::shortcut_guide::State as GuideState;

use crate::keys::{ControllerKeys, Invocations, PressedChords, Presses, SessionKeys, ShellKeys};
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
    /// What stands over the shell, which Escape closes from the top.
    pub(crate) overlays: Overlays,
    /// The desktop's handle where the shell draws its own launcher, whose rows the keys walk here.
    pub(crate) launcher: Option<tilo_session_core::desktop::state::DesktopHandle>,
    /// The surfaces the shell draws over its windows, which Escape puts away from the top.
    pub(crate) surfaces: Option<crate::device_lists::Surfaces>,
}

/// The session's own resources and the keys it answers wherever the keyboard is: above every
/// surface, so offered every press first. What mounting the session's controls hands the session
/// back: the commands the key surface raises and the state its own resources read, the invocations
/// it answers and the chords it pressed through.
pub(crate) type Controls = (
    Rc<RefCell<Vec<SessionCommand>>>,
    Rc<RefCell<ControlsReading>>,
    Invocations,
    PressedChords,
);

/// The desktop's one shortcut table, shared: one reader matches a press against it, another shows
/// its chord, and a person's rebinding reaches all of them.
pub(crate) type Shortcuts = Rc<RefCell<ui::shortcut_table::ShortcutConfig>>;

pub(crate) fn mount_controls(
    compositor: &mut Compositor,
    shortcuts: Shortcuts,
    portal: Option<tilo_portal::global_shortcuts::Shortcuts>,
    guide: Rc<GuideState>,
    KeyContext {
        controller,
        controller_keys,
        sheets,
        overlays,
        launcher,
        surfaces,
    }: KeyContext,
) -> Controls {
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
            ShellKeys::new(overlays),
            sheets,
            ControllerKeys {
                last: controller,
                own: controller_keys,
            },
        )
        .with_launcher(launcher)
        .with_surfaces(surfaces),
        portal,
        Rc::clone(&commands),
    ))));
    (commands, controls_reading, invocations, triggered)
}
