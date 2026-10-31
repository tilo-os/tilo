//! What a session is started with: the device it runs on, the person it acts
//! for, the settings it follows, and the services, sockets and desks it is
//! lent. Everything the loop can do without — a panel, a lock, a fabric — is
//! `None` here, and a session without it keeps that part of the desk dark.

use std::rc::Rc;

use tilo_quick_settings::device::{DeviceHost, Wake};
use tilo_quick_settings::host::PanelHost;
use ui::application::ComponentRunnerInputs;

use crate::rotation::Accelerometer;
use crate::{
    AppActions, KillSwitch, Launches, OnboardingView, SearchOptions, Service, SessionAction,
    SessionConfig,
};
use crate::{
    capture, controllers, desktop_services, engine, fabric, mount, portal_link, surfaces, sync,
    unlock,
};
use tilo_desktop_surface::DesktopHandle;

/// What a session starts with.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is an independent part of what a session is lent"
)]
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
    /// The tilolang shell package's bar (`apps/tl/shell`) is shown as a layer over the
    /// desktop. Off until it covers every display and steps aside for a fullscreen window as
    /// the desktop's own bar does.
    pub shell_bar: bool,
    /// What quick settings reads of the device and carries out on it;
    /// without it, the bar's status cluster opens nothing.
    pub quick_settings: Option<QuickSettings>,
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
    /// for: the app, room and process it launched itself. The apps it starts
    /// record their launch here; a session whose apps run in its own process
    /// (a test) records one itself.
    pub launches: std::sync::Arc<Launches>,
    /// The drives the person plugs in, which this session announces and acts
    /// on when they open or eject one.
    pub drives: Option<Rc<mount::drives::Drives>>,
    /// Setup to run over everything before the desktop is used: the first
    /// use of this device, or the recovery of its identity.
    pub setup: Option<OnboardingView>,
    /// Carries out what the person chose in the power menu (log out,
    /// suspend, restart, shut down, switch user); the session locks the
    /// screen itself. Without one, the choice is only logged.
    pub session_actions: fn(SessionAction) -> Result<(), String>,
    /// Whether the lock screen may offer suspend, restart and shut down:
    /// a session that owns the machine, not one running inside a window.
    pub lock_power: bool,
    /// Whose session this is, as the system knows them: the user a
    /// password unlocks the lock screen for.
    pub user: String,
    /// Whether a password unlocks the session; without one nothing does.
    pub verifier: std::sync::Arc<dyn unlock::PasswordVerifier>,
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
    /// It runs in a window of a desktop it did not start: that desktop owns
    /// idle, blanking, locking and suspend, so this one runs none of them.
    pub hosted: bool,
    /// The machine's power: the policy the person's settings resolved, the
    /// service over logind that acts on it and what holds it awake. Without
    /// one, this session runs no idle, lid, key or battery policy — it is a
    /// session in a window of another desktop, or a test's.
    pub power: Option<tilo_session_power::PowerLink>,
    /// Where this device keeps what outlives a session: the desk as it was
    /// left, and the pictures on it. Without one, the session starts on a
    /// fresh desk and keeps nothing (tests and screenshots).
    pub state: Option<std::path::PathBuf>,
    /// Whether the session takes part in the collaboration plane: who is
    /// here in each workspace's room, over Iroh. Off, this desktop has no
    /// collaborators but its own person.
    pub collab: bool,
    /// A collaboration plane to take part in instead of starting one: a
    /// session a test hands the presence a room would have carried
    /// ([`sync::Client::untethered`]), which says nothing on a network.
    pub sync: Option<Rc<sync::Client>>,
    /// The person's device fabric, when this session runs on one: what was
    /// copied here is shared with their other devices through it, and what
    /// they copied there joins the clipboard here.
    pub fabric: Option<tilo_device_fabric::link::SharedLink>,
    pub wake_seams: mount::wake::Seams,
    /// The person's VPN profile kill switches, when this session runs on a
    /// machine with the helper: a profile set to connect and not connected
    /// blocks traffic while its switch is on. Without one, nothing does.
    pub kill_switch: Option<KillSwitch>,
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
    /// The runs of what people ask this session's agents to do. Without one
    /// the session keeps its own, which its broker delivers to the agents
    /// admitted here; one given is a run channel a test plays the agent's
    /// side of.
    pub agent_runs: Option<std::sync::Arc<tilo_agent_runs::AgentRuns>>,
}

/// Where a session serves its apps the resources its services publish.
#[derive(Clone, Debug, Default)]
pub enum ActionPlane {
    /// Nowhere: the apps reach no service.
    #[default]
    Off,
    /// On the socket the launcher hands an app for its session
    /// (`launcher::services::collab_socket`), which is also what tells the
    /// launcher to bind one.
    Launcher,
    /// On `path`: a session started for a test, which leaves the machine's
    /// own socket alone.
    At(std::path::PathBuf),
}

/// Where quick settings reads the device from.
pub enum QuickSettings {
    /// The device host of this machine (or of a stand-in one), made with
    /// the session loop's wake: its readings land on the loop, what the
    /// panel asks of the desktop comes back to the session.
    Device(Box<dyn FnOnce(Wake) -> DeviceHost>),
    /// A host lent as it is: a screenshot's stand-in readings.
    Lent(Rc<dyn PanelHost>),
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

/// Who is here: this session's membership of each workspace's room, over the
/// collaboration plane. `None` when the session takes no part in it, or the
/// device's endpoint did not bind: a desktop with no one else on it.
pub(crate) fn start_collaboration(
    options: &Options,
    media: fabric::MediaContinuity,
    engine: Option<std::sync::Arc<engine::Engine>>,
    link: std::sync::Arc<crate::channels::ChannelLink>,
) -> Option<Rc<sync::Client>> {
    if let Some(sync) = &options.sync {
        return Some(Rc::clone(sync));
    }
    if !options.collab {
        return None;
    }
    let identity = sync::device_identity();
    let own = sync::OwnPresence {
        label: options.person.clone(),
        user: identity
            .as_ref()
            .map(|identity| hex::encode(identity.device_cert.user_public_key)),
        ..sync::OwnPresence::default()
    };
    match sync::Client::start(
        identity,
        engine,
        own,
        sync::configured_hosts(),
        media,
        options.wake.handle(),
        link,
    ) {
        Ok(sync) => Some(Rc::new(sync)),
        Err(error) => {
            tracing::warn!(%error, "the collaboration plane");
            None
        }
    }
}

/// This device's rooms, holding what every workspace of the desk shows and
/// what a join takes in, kept beside the desk. `None` when this computer has
/// no identity to hold them with, or keeps no state to keep them in.
pub(crate) fn open_engine(
    options: &Options,
    handle: &DesktopHandle,
) -> Option<std::sync::Arc<engine::Engine>> {
    let workspaces: Vec<_> = handle.read(|tree| {
        tree.workspaces
            .iter()
            .filter(|workspace| !workspace.room().is_nil())
            .map(|workspace| (workspace.id, workspace.room()))
            .collect()
    });
    match engine::Engine::for_this_device(options.state.as_deref(), workspaces) {
        Ok(engine) => Some(std::sync::Arc::new(engine)),
        Err(error) => {
            tracing::debug!(%error, "this session holds no rooms");
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
            shell_bar: false,
            sync: None,
            quick_settings: None,
            controllers: None,
            config: SessionConfig::default(),
            accelerometer: Accelerometer::default(),
            services: Vec::new(),
            search: SearchOptions::default(),
            apps: None,
            launch: None,
            setup: None,
            session_actions: no_session_actions,
            lock_power: false,
            user: String::new(),
            verifier: std::sync::Arc::new(unlock::Unavailable),
            wake: tilo_service::LoopWake::default(),
            captures: None,
            projector: None,
            pictures: None,
            desktop: None,
            questions: None,
            portal: None,
            surfaces: Vec::new(),
            hosted: false,
            power: None,
            state: None,
            collab: true,
            fabric: None,
            wake_seams: mount::wake::Seams::default(),
            kill_switch: None,
            action_plane: ActionPlane::default(),
            agent_door: None,
            agent_runs: None,
            launches: std::sync::Arc::default(),
            drives: None,
        }
    }
}
