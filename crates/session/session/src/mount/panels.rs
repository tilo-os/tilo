//! The panels the session stands up beside the desk: the setup a person goes through once, and the
//! keys' readout with the keys that answer wherever the keyboard is.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use tilo_compositor::Compositor;
use tilo_desktop::overlays::Overlays;

use crate::controls::ControlsReading;
use crate::keys::{
    ControllerKeys, DeskKeys, GuideKeys, Invocations, PressedChords, Presses, SessionKeys,
};
use crate::panels::{ActivityLayer, Held, Readout, ReadoutPlace, ReadoutPlaces, Reserved};
use crate::setup::Setup;
use crate::{BeforeDesk, SessionCommand};
use tilo_surface_host::Doors;

/// What stands before the desk, when it stands there at all: above everything, first to the
/// keyboard, and while it is up the only surface offering actions.
pub(crate) fn mount_setup(
    compositor: &mut Compositor,
    doors: &mut Doors,
    surface: Option<BeforeDesk>,
    person: &str,
    scale: f64,
) -> Option<Rc<RefCell<Setup>>> {
    let waiter = compositor.waker();
    crate::setup::lend(waiter.clone(), person);
    surface.map(|surface| {
        let setup = Rc::new(RefCell::new(Setup::new(surface, scale, waiter)));
        compositor.add_host(Box::new(Held(Rc::clone(&setup))));
        doors.add(Rc::clone(&setup) as tilo_surface_host::Shared);
        setup
    })
}

/// What the session's keys answer with besides the shortcut table: the controller the person last
/// drove the desktop with, whether the keys being routed are that controller's own, the display
/// sheet Super+P puts up, and the surfaces over the desk.
pub(crate) struct KeyContext {
    pub(crate) controller: Rc<Cell<Option<ui::ControllerFamily>>>,
    pub(crate) controller_keys: Rc<Cell<bool>>,
    pub(crate) displays: Option<tilo_sheets::Presentation>,
    /// Whether a desk widget has its card open, which Escape folds.
    pub(crate) fold: tilo_desktop::desk_widgets::DeskFold,
    /// What stands over the desk, which Escape closes from the top.
    pub(crate) overlays: Overlays,
    /// The desk's own overlays: the surface up, and Alt+Tab's walk.
    pub(crate) desk: Rc<crate::overlays::Overlays>,
    /// When each display's bar shows over the window covering it, which Super asks for.
    pub(crate) reveals: Rc<RefCell<tilo_desktop::topbar::Reveals>>,
    /// What the keys ask the panel's own app, when this device draws it.
    pub(crate) panel: Option<Rc<RefCell<Vec<&'static str>>>>,
}

/// The hardware keys' readout above the apps, and the session's own resources and the keys it
/// answers wherever the keyboard is: above every surface, so offered every press first. What
/// mounting the session's controls hands the session back: the commands the key surface raises and
/// the state its own resources read, the invocations it answers, the chords it pressed through,
/// where the readout stands and the readout itself.
pub(crate) type Controls = (
    Rc<RefCell<Vec<SessionCommand>>>,
    Rc<RefCell<ControlsReading>>,
    Invocations,
    PressedChords,
    ReadoutPlaces,
    Rc<RefCell<Readout>>,
);

/// The desktop's one shortcut table, shared: one reader matches a press against it, another shows
/// its chord, and a person's rebinding reaches all of them.
pub(crate) type Shortcuts = Rc<RefCell<ui::shortcut_table::ShortcutConfig>>;

pub(crate) fn mount_controls(
    compositor: &mut Compositor,
    doors: &mut Doors,
    shortcuts: Shortcuts,
    portal: Option<tilo_portal::global_shortcuts::Shortcuts>,
    guide: GuideKeys,
    KeyContext {
        controller,
        controller_keys,
        displays,
        fold,
        overlays,
        desk,
        reveals,
        panel,
    }: KeyContext,
    scale: f64,
    band: Reserved,
) -> Controls {
    // Where each display stands and where the person types, filled in every pass: what the readout
    // answers with.
    let places: ReadoutPlaces = Rc::new(RefCell::new(ReadoutPlace::default()));
    let readout = Rc::new(RefCell::new(Readout::new(scale, Rc::clone(&places), band)));
    compositor.add_host(Box::new(Held(Rc::clone(&readout))));
    let shared: tilo_surface_host::Shared = readout.clone();
    doors.add(shared);
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
    compositor.add_host(Box::new(
        SessionKeys::new(
            shortcuts,
            Presses {
                invocations: Rc::clone(&invocations),
                triggered: Rc::clone(&triggered),
            },
            portal,
            guide,
            DeskKeys::new(fold, overlays)
                .with_switcher(desk)
                .with_panel(panel),
            displays,
            ControllerKeys {
                last: controller,
                own: controller_keys,
            },
            reveals,
        )
        .with_commands(Rc::clone(&commands)),
    ));
    (
        commands,
        controls_reading,
        invocations,
        triggered,
        places,
        readout,
    )
}

/// The floating layer over the desk: every app's activity as a card, which the session's own
/// reading of the board fills and what the person does on it reaches the apps through.
/// Under the shell nothing of it is mounted: the old shell draws the timer's own floating panel, so
/// a layer of ours would draw a second card over it.
pub(crate) fn mount_activities(
    compositor: &mut Compositor,
    doors: &mut Doors,
    reader: &Rc<crate::chrome::Activities>,
    cards: &crate::cards::Cards,
    banners: Reserved,
    readout: Reserved,
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
                banners,
                readout,
                options.scale,
            )));
            compositor.add_host(Box::new(Held(Rc::clone(&surface))));
            doors.add(surface);
            Some(link)
        }
        Err(error) => {
            tracing::warn!(%error, "activity layer not mounted");
            None
        }
    }
}
