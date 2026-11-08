//! What a session is started with: the device it runs on, the person it acts
//! for, the settings it follows, and the services, sockets and desks it is
//! lent. Everything the loop can do without — a panel, a lock, a fabric — is
//! `None` here, and a session without it keeps that part of the desk dark.

use std::rc::Rc;

use ui::application::ComponentRunnerInputs;

use crate::BeforeDesk;
use crate::displays::Backlights;
use crate::rotation::Accelerometer;
use crate::{AppActions, Launches, SearchOptions, Service, SessionAction, SessionConfig, Vpn};
use crate::{
    capture, controllers, desktop_services, engine, fabric, mount, portal_link, surfaces, sync,
    unlock,
};
use tilo_desktop_surface::DesktopHandle;

/// What a session starts with.
pub struct Options {
    /// Seeds the desktop's ids, so two devices never mint the same.
    pub salt: u16,
    /// Physical pixels per logical pixel the desktop draws at.
    pub scale: f64,
    /// This device, by id.
    pub device: String,
    /// The person at it, by the id the desktop's authors name them by. The
    /// session reads it from this device's own identity as it starts, so a
    /// caller's value answers only for a computer that has no identity yet.
    pub person: String,
    /// The theme and fonts surface apps draw with.
    pub presentation: fn() -> ComponentRunnerInputs,
    /// Whether the tilolang shell package (`apps/tl/shell`) runs: its whole screen under the
    /// windows, one part per display, each on its own display's rectangle and stepping aside
    /// where a fullscreen window owns one — its bar, dock and the person's arrangement as
    /// their preset shows it, every window in the well its place leaves it. `false`, the
    /// default, until the desktop's own chrome moves onto it.
    pub shell: bool,
    /// The controllers in the room, which drive the desktop; without one, a
    /// controller does nothing here.
    pub controllers: Option<controllers::Source>,
    /// The person's settings: shortcuts, input, gestures.
    pub config: SessionConfig,
    /// The built-in panel turns with the way up this reads the device.
    pub accelerometer: Accelerometer,
    /// The system services the session hosts: what they publish is the
    /// session's, beside the desktop's.
    pub services: Vec<Box<dyn Service>>,
    /// The machine's brightness controls, which the displays service reads and
    /// sets: the panel's backlight and every external monitor that speaks
    /// DDC/CI. Without one, no display of this session can be dimmed (a test,
    /// or a session in a window of another desktop).
    pub backlights: Option<Box<dyn Backlights>>,
    /// What the desktop's search reaches beyond the desktop.
    pub search: SearchOptions,
    /// The action host: what open apps publish and run, and where what is
    /// chosen in search runs by address. Without one, search finds the
    /// desktop's own and starts nothing.
    pub apps: Option<Rc<dyn AppActions>>,
    /// The apps this session starts: what the desktop asks for, by name, and
    /// what a search row or a file opens.
    pub launch: Option<Rc<mount::apps::Apps>>,
    /// The apps this session started, which its broker admits a connection
    /// for: the app, workspace and process it launched itself. The apps it
    /// starts record their launch here; a session whose apps run in its own
    /// process (a test) records one itself.
    pub launches: std::sync::Arc<Launches>,
    /// The drives the person plugs in, which this session announces and acts
    /// on when they open or eject one.
    pub drives: Option<Rc<mount::drives::Drives>>,
    /// What stands over everything before the desktop is used: the setup the
    /// person goes through once (the first use of this device, or the
    /// recovery of its identity), or the wait for the clock to admit this
    /// device's certificate.
    pub setup: Option<BeforeDesk>,
    /// Carries out what the person chose in the power menu (log out,
    /// suspend, restart, shut down, switch user); the session locks the
    /// screen itself. Without one, the choice is only logged.
    pub session_actions: fn(SessionAction) -> Result<(), String>,
    /// Whose session this is, as the system knows them: the user a
    /// password unlocks the lock screen for.
    pub user: String,
    /// Whether a password unlocks the session; without one nothing does.
    pub verifier: std::sync::Arc<dyn unlock::PasswordVerifier>,
    /// The login's lock record, when a session wrapper started this session:
    /// the lock a crashed session left in force comes back with it, and the
    /// state this one ends in is published for the next.
    pub recovery: Option<crate::recovery::Recovery>,
    /// What the services were handed to wake the session's loop when
    /// something lands: the session binds it to its loop once started.
    pub wake: tilo_service::LoopWake,
    /// The screenshots the portal asks for, which the session takes.
    pub captures: Option<capture::Captures>,
    /// Where the wireless display service answers: its control socket.
    /// `None`: the service's own, which the session's first request
    /// activates.
    pub projector: Option<std::path::PathBuf>,
    /// The folder the capture studio keeps its Screenshots folder in;
    /// `None`: the person's Pictures folder, else their home.
    pub pictures: Option<std::path::PathBuf>,
    /// The desktop services the session runs beside: what apps sent their
    /// notifications daemon, the questions they ask, and the portal they
    /// serve. The session feeds it what it knows of itself: which apps run
    /// and how, when the screen locks, which apps exited and what its
    /// workspaces grant.
    pub desktop: Option<Rc<desktop_services::DesktopServices>>,
    /// The questions the session's sheets answer, when nothing runs beside
    /// it to ask them: a session with desk services asks theirs.
    pub questions: Option<tilo_service::Questions>,
    /// The portal's link to the loop: what shares the session's screens,
    /// drives its seat and passes its selection.
    pub portal: Option<portal_link::SessionLink>,
    /// The surface apps the session runs as its own processes, each a client
    /// of its compositor.
    pub surfaces: Vec<surfaces::Surface>,
    /// Which machine this session draws on (see [`Machine`]).
    pub machine: Machine,
    /// The machine's power: the policy the person's settings resolved, the
    /// service over logind that acts on it and what holds it awake. Without
    /// one, this session runs no idle, lid, key or battery policy — it is a
    /// session in a window of another desktop, or a test's.
    pub power: Option<tilo_session_power::PowerLink>,
    /// Where this device keeps what outlives a session: the desk as it was
    /// left, and the pictures on it. Without one, the session starts on a
    /// fresh desk and keeps nothing (tests and screenshots).
    pub state: Option<std::path::PathBuf>,
    /// The desk the shell this session replaced kept on this device
    /// ([`crate::shell_state_path`]), which the first start that has no desk
    /// of its own brings over. `None`: there is none to look for (tests and
    /// screenshots).
    pub old_desk: Option<std::path::PathBuf>,
    /// Whether the session takes part in the collaboration plane: who is
    /// here in each workspace, over Iroh. Off, this desktop has no
    /// collaborators but its own person.
    pub collab: bool,
    /// A collaboration plane to take part in instead of starting one: a
    /// session a test hands the presence a workspace would have carried
    /// ([`sync::Client::untethered`]), which says nothing on a network.
    pub sync: Option<Rc<sync::Client>>,
    /// The person's device fabric, when this session runs on one: what was
    /// copied here is shared with their other devices through it, and what
    /// they copied there joins the clipboard here.
    pub fabric: Option<tilo_device_fabric::link::SharedLink>,
    pub wake_seams: mount::wake::Seams,
    /// The device VPN, when this session runs on a machine with the helper:
    /// a VPN profile set to connect and not connected blocks traffic while
    /// its kill switch is on, and this device routes through another of the
    /// person's devices while they choose one. Without one, nothing does.
    pub vpn: Option<Vpn>,
    /// Whether the apps this session runs read and invoke what its services
    /// publish, and on which socket. Off, the apps it starts reach no
    /// service at all.
    pub action_plane: ActionPlane,
    /// Where the session's agent door listens
    /// ([`agent_door::path`]): the socket a program of this login asks on to
    /// work in a workspace as an agent, and the person admits it. `None`:
    /// this session serves no agents (a test, or a session in a window of
    /// another desktop).
    pub agent_door: Option<std::path::PathBuf>,
    /// The programs the person admits, as they start the session, to act as
    /// them at the agent door (`--admit-repl PATH`): their decision, made
    /// ahead of time, so a REPL they start is let in without the sheet. Only
    /// a connection whose executable is one of these and that asks to act as
    /// the person is; any other is asked about as before.
    pub admit_repl: Vec<std::path::PathBuf>,
    /// Where the development test-control socket opens
    /// ([`crate::test_control`]), which `tilo-control` drives the session
    /// over: `tilo-session --test-control PATH`, honoured by development
    /// builds only. `None`: there is none.
    pub test_control: Option<std::path::PathBuf>,
    /// Where the device admin's control socket opens
    /// ([`crate::test_control::admin_path`]), which `tilo-control` reaches
    /// as the account whose desktop this is: `tilo-session --admin-control
    /// PATH`, else the seat's own `admin-control.sock` for a session that
    /// owns the machine, and `headless-owner-control.sock` for a headless
    /// one. `None`: there is none.
    pub admin_control: Option<std::path::PathBuf>,
}

/// Which machine a session draws on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Machine {
    /// This one: the session owns idle, blanking, locking and suspend, the
    /// device's controllers and services, and the session actions it carries
    /// out reach this machine.
    #[default]
    Owned,
    /// A desktop this session did not start, in whose window it runs: that
    /// desktop owns idle, blanking, locking and suspend, so this one runs none
    /// of them and no session action of its reaches the machine under the
    /// window.
    Window,
}

impl Machine {
    /// Whether this session owns the machine it draws on.
    #[must_use]
    pub const fn is_owned(self) -> bool {
        matches!(self, Self::Owned)
    }
}

/// Where a session serves its apps the resources its services publish.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ActionPlane {
    /// Nowhere: the apps reach no service.
    #[default]
    Off,
    /// On the socket the launcher hands an app for its session
    /// (`launcher::services::collab_socket`), which is also what tells the
    /// launcher to bind one.
    Launcher,
    /// On a socket of the session's own, in its private directory under the
    /// runtime directory (`tilo_paths::login::session_dir`): the session that
    /// draws in a window of another desktop serves its apps there, and leaves
    /// that desktop's own socket to its desktop.
    Own,
    /// On `path`: a session started for a test, which leaves the machine's
    /// own socket alone.
    At(std::path::PathBuf),
}

impl ActionPlane {
    /// What a session on `machine` serves its apps on: the socket the
    /// launcher hands them when it owns the machine, and a socket of its own
    /// when it runs in a window of another desktop, whose socket is not its
    /// to take. Every surface app it starts reads its resources there — its
    /// lock screen above all, which unlocks the session through them.
    #[must_use]
    pub const fn for_machine(machine: Machine) -> Self {
        if machine.is_owned() {
            Self::Launcher
        } else {
            Self::Own
        }
    }
}

/// The default for [`Options::session_actions`]: a session that owns no
/// machine (a test, a screenshot, a session in a window) carries out
/// nothing.
#[allow(
    clippy::unnecessary_wraps,
    reason = "the shape every session action runner has"
)]
pub fn no_session_actions(action: SessionAction) -> Result<(), String> {
    tracing::info!(?action, "this session carries out no session actions");
    Ok(())
}

/// Who is here: this session's membership of each workspace, over the
/// collaboration plane. `None` when the session takes no part in it, or the
/// device's endpoint did not bind: a desktop with no one else on it.
///
/// `beside` is what else this device answers on its endpoint — its exit for
/// the person's other devices. A caller that hands its own client
/// ([`Options::sync`]) has already said what that endpoint serves.
///
/// `documents` is where what admission takes in from another device goes, so
/// the apps this session runs show what everyone wrote.
///
/// `remote` is the agent door toward the person's other devices: answered on
/// the endpoint over the devices the person's own store holds, and dialing
/// theirs from it ([`crate::remote_door`]).
pub(crate) fn start_collaboration(
    options: &Options,
    continuity: fabric::Continuity,
    engine: Option<std::sync::Arc<engine::Engine>>,
    (beside, remote): (
        tilo_network_actor::local_host::Protocols,
        &std::sync::Arc<crate::remote_door::Remote>,
    ),
    documents: Option<sync::PeerDocuments>,
) -> Option<Rc<sync::Client>> {
    if let Some(sync) = &options.sync {
        return Some(Rc::clone(sync));
    }
    if !options.collab {
        return None;
    }
    let identity = sync::device_identity();
    // The person's own store is answered on the session's endpoint, so it is
    // opened before that endpoint binds and handed over with the rest.
    let personal = identity.as_ref().and_then(|identity| {
        match crate::personal::Personal::open(options, identity) {
            Ok(personal) => Some(personal),
            Err(error) => {
                tracing::warn!(%error, "this session keeps no store of the person's own");
                None
            }
        }
    });
    let mut beside = beside;
    // Which devices are the person's is their own store's to say, for the
    // agents that connect from one of them and for those this one dials.
    let devices = personal.as_ref().map(|personal| {
        std::sync::Arc::new(tilo_runtime::remote_door::PersonalDevices::new(
            personal.sync().clone(),
        )) as std::sync::Arc<dyn tilo_runtime::remote_door::PersonsDevices>
    });
    if let Some(personal) = &personal {
        beside.push(personal.protocol());
    }
    if let Some(devices) = &devices {
        beside.push(remote.protocol(std::sync::Arc::clone(devices)));
    }
    let own = sync::OwnPresence {
        label: options.person.clone(),
        user: identity
            .as_ref()
            .map(|identity| hex::encode(identity.device_cert.user_public_key)),
        ..sync::OwnPresence::default()
    };
    match sync::Client::start(
        sync::Entry {
            identity,
            own,
            hosts: sync::configured_hosts(),
            continuity,
            wake: options.wake.handle(),
        },
        engine,
        options.questions.clone(),
        beside,
        documents,
    ) {
        Ok(sync) => {
            if let Some(personal) = personal {
                sync.attach_personal(personal);
            }
            if let (Some(devices), Some(endpoint)) = (devices, sync.endpoint()) {
                remote.attach(tilo_runtime::remote_door::Doors::new(
                    endpoint,
                    devices,
                    tilo_rt::app().handle().clone(),
                ));
            }
            Some(Rc::new(sync))
        }
        Err(error) => {
            tracing::warn!(%error, "the collaboration plane");
            None
        }
    }
}

/// This device's workspaces, holding what the desk shows of them and what a
/// join takes in, kept beside the desk. `None` when this computer has
/// no identity to hold them with, or keeps no state to keep them in.
pub(crate) fn open_engine(
    options: &Options,
    handle: &DesktopHandle,
) -> Option<std::sync::Arc<engine::Engine>> {
    let workspaces: Vec<_> = handle.read(|tree| {
        tree.workspaces
            .iter()
            .filter(|workspace| !workspace.id().is_nil())
            .map(|workspace| (workspace.slot, workspace.id()))
            .collect()
    });
    match engine::Engine::for_this_device(options.state.as_deref(), workspaces) {
        Ok(engine) => Some(std::sync::Arc::new(engine)),
        Err(error) => {
            tracing::warn!(%error, "this session holds no workspaces");
            None
        }
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            salt: 1,
            scale: 1.0,
            device: "this-device".into(),
            person: "person".into(),
            presentation: ComponentRunnerInputs::from_native_process,
            shell: false,
            sync: None,
            controllers: None,
            config: SessionConfig::default(),
            accelerometer: Accelerometer::default(),
            services: Vec::new(),
            backlights: None,
            search: SearchOptions::default(),
            apps: None,
            launch: None,
            setup: None,
            session_actions: no_session_actions,
            user: String::new(),
            verifier: std::sync::Arc::new(unlock::Unavailable),
            recovery: None,
            wake: tilo_service::LoopWake::default(),
            captures: None,
            projector: None,
            pictures: None,
            desktop: None,
            questions: None,
            portal: None,
            surfaces: Vec::new(),
            machine: Machine::default(),
            power: None,
            state: None,
            old_desk: None,
            collab: true,
            fabric: None,
            wake_seams: mount::wake::Seams::default(),
            vpn: None,
            action_plane: ActionPlane::default(),
            agent_door: None,
            admit_repl: Vec::new(),
            test_control: None,
            admin_control: None,
            launches: std::sync::Arc::default(),
            drives: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ActionPlane, Machine};

    /// A session in a window of another desktop serves its apps on a socket
    /// of its own, never on the one the desktop it draws in serves: that one
    /// is the other session's, and the apps of this one — its lock screen
    /// above all — read their resources on this one.
    #[test]
    fn a_session_serves_its_apps_where_they_reach_it() {
        assert_eq!(
            ActionPlane::for_machine(Machine::Owned),
            ActionPlane::Launcher
        );
        assert_eq!(ActionPlane::for_machine(Machine::Window), ActionPlane::Own);
    }
}
