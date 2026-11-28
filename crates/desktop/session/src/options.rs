//! What a session is started with: the device it runs on, the person it acts for, the settings it
//! follows, and the services, sockets and sessions it is lent. Everything the loop can do without — a
//! panel, a lock, a fabric — is `None` here, and a session without it keeps that part of the shell
//! dark.

use std::rc::Rc;
use std::sync::Arc;

use ui::application::ComponentRunnerInputs;

use crate::BeforeShell;
use crate::displays::Backlights;
use crate::rotation::Accelerometer;
use crate::{AppActions, Launches, SearchOptions, Service, SessionAction, SessionConfig, Vpn};
use crate::{capture, controllers, desktop_services, portal_link, surfaces, sync, unlock};

/// What a session starts with.
pub struct Options {
    /// Seeds the desktop's ids, so two devices never mint the same.
    pub salt: u16,
    /// Physical pixels per logical pixel the desktop draws at.
    pub scale: f64,
    /// This device, by id.
    pub device: String,
    /// The person at it, by the id the desktop's authors name them by. The session reads it from
    /// this device's own identity as it starts, so a caller's value answers only for a computer
    /// that has no identity yet.
    pub person: String,
    /// The theme and fonts surface apps draw with.
    pub presentation: fn() -> ComponentRunnerInputs,
    /// The controllers in the room, which drive the desktop; without one, a controller does nothing
    /// here.
    pub controllers: Option<controllers::Source>,
    /// The person's settings: shortcuts, input, gestures.
    pub config: SessionConfig,
    /// The built-in panel turns with the way up this reads the device.
    pub accelerometer: Accelerometer,
    /// The system services the session hosts: what they publish is the session's, beside the
    /// desktop's.
    pub services: Vec<Box<dyn Service>>,
    /// The machine's brightness controls, which the displays service reads and
    /// sets: the panel's backlight and every external monitor that speaks
    /// DDC/CI. Without one, no display of this session can be dimmed (a test,
    /// or a session in a window of another desktop).
    pub backlights: Option<Box<dyn Backlights>>,
    /// What the desktop's search reaches beyond the desktop.
    pub search: SearchOptions,
    /// The apps this device lists — installed here, the machine's own and those a person can get —
    /// as search, the app sheets and the tool service read them.
    pub lists: crate::lists::Lists,
    /// The records' content, as this session reaches it: the socket the device's daemon serves
    /// its content store on. `None` reaches the one the launcher named the daemon's.
    pub content_socket: Option<std::path::PathBuf>,
    /// The action host: what open apps publish and run, and where what is chosen in search runs by
    /// address. Without one, search finds the desktop's own and starts nothing.
    pub apps: Option<Rc<dyn AppActions>>,
    /// The apps this session starts: what the desktop asks for, by name, and what a search row or a
    /// file opens.
    pub launch: Option<Rc<crate::apps::Apps>>,
    /// The session program a workspace's render service runs as: this process's own when none is
    /// named, which is a render service in its own mode (`tilo-session __render-service`). A
    /// session a test runs inside its own process names the session binary it was built from, so
    /// the apps it starts draw their frames there.
    pub render_service: Option<std::path::PathBuf>,
    /// The apps this session started, which its broker admits a connection for: the app, workspace
    /// and process it launched itself. The apps it starts record their launch here; a session whose
    /// apps run in its own process (a test) records one itself.
    pub launches: std::sync::Arc<Launches>,
    /// How the Windows compatibility service the Windows Apps manager reaches is put together:
    /// the computer's own Windows stack, or what a test stands in for it.
    pub windows: compat_service::Assembly,
    /// Where the service publishes the device's Windows programs as it starts: the session's
    /// system service serves them from here.
    pub windows_manager: compat_service::ManagerOut,
    /// The drives the person plugs in, which this session announces and acts on when they open or
    /// eject one.
    pub drives: Option<Rc<crate::drives::Drives>>,
    /// What stands over everything before the desktop is used: the setup the person goes through
    /// once (the first use of this device, or the recovery of its identity), or the wait for the
    /// clock to admit this device's certificate.
    pub setup: Option<BeforeShell>,
    /// The identity backup a desktop already in use opens, as setup shows it when a computer is
    /// first used: the person writes their identity to a file they choose, sealed with a
    /// passphrase. `host_desktop` is whether this session is Tilo installed on an existing Linux
    /// host. A function returning `Err` says why this computer has nothing to back up.
    #[cfg(target_os = "linux")]
    pub identity_backup: fn(bool) -> Result<tilo_onboarding_session::ceremony::Ceremony, String>,
    /// Whether this session is Tilo installed as a desktop session on an existing Linux host.
    pub host_desktop: bool,
    /// What this session holds of a workspace, when a workspace VM's daemon started it for one:
    /// its Browser window or its Linux apps' windows, opened as the daemon launches them
    /// ([`crate::hosted_browser::HostedBrowser`]). A session a person starts has none, and does
    /// nothing a hosted Browser's session is spared.
    #[cfg(target_os = "linux")]
    pub hosted_browser: Option<crate::hosted_browser::HostedBrowser>,
    /// Carries out what the person chose in the power menu (log out,
    /// suspend, restart, shut down, switch user); the session locks the
    /// screen itself. Without one, the choice is only logged.
    pub session_actions: fn(SessionAction) -> Result<(), String>,
    /// Whose session this is, as the system knows them: the user a password unlocks the lock screen
    /// for.
    pub user: String,
    /// The ids the person went by before this device had an identity — the login account among
    /// them — under which a shell they kept, and the workspaces it holds, are still theirs.
    pub earlier: Vec<String>,
    /// Whether a password unlocks the session; without one nothing does.
    pub verifier: std::sync::Arc<dyn unlock::PasswordVerifier>,
    /// The login's lock record, when a session wrapper started this session: the lock a crashed
    /// session left in force comes back with it, and the state this one ends in is published for
    /// the next.
    pub recovery: Option<crate::recovery::Recovery>,
    /// How the session ends, asked once as it does: what its apps are left as. Every session but
    /// the machine's own desktop ends its apps; that one asks whether its wrapper restarts it in
    /// place ([`crate::restart::teardown`]).
    #[cfg(target_os = "linux")]
    pub teardown: Box<dyn Fn() -> tilo_app_launch::Teardown>,
    /// What the services were handed to wake the session's loop when something lands: the session
    /// binds it to its loop once started.
    pub wake: tilo_service::LoopWake,
    /// The screenshots the portal asks for, which the session takes.
    pub captures: Option<capture::Captures>,
    /// Where the wireless display service answers: its control socket. `None` : the service's own,
    /// which the session's first request activates.
    pub projector: Option<std::path::PathBuf>,
    /// The folder the capture studio keeps its Screenshots folder in;
    /// `None`: the person's Pictures folder, else their home.
    pub pictures: Option<std::path::PathBuf>,
    /// The desktop services the session runs beside: what apps sent their notifications daemon, the
    /// questions they ask, and the portal they serve. The session feeds it what it knows of itself:
    /// which apps run and how, when the screen locks, which apps exited and what its workspaces
    /// grant.
    pub desktop: Option<Rc<desktop_services::DesktopServices>>,
    /// Where programs ask the person, and the session's side of it, when nothing runs beside the
    /// session to supply them: a session with shell services takes theirs.
    pub asks: Option<(tilo_service::AskSender, tilo_service::AskReceiver)>,
    /// The portal's link to the loop: what shares the session's screens,
    /// drives its seat and passes its selection.
    pub portal: Option<portal_link::SessionLink>,
    /// The surface apps the session runs as its own processes, each a client of its compositor.
    pub surfaces: Vec<surfaces::Surface>,
    /// Which machine this session draws on (see [`Machine`]).
    pub machine: Machine,
    /// The machine's power: the policy the person's settings resolved, the service over logind that
    /// acts on it and what holds it awake. Without one, this session runs no idle, lid, key or
    /// battery policy — it is a session in a window of another desktop, or a test's.
    pub power: Option<tilo_session_power::PowerLink>,
    /// This device's state directory, where the session's own services keep their files. Without
    /// one, they keep nothing (tests and screenshots).
    pub state: Option<std::path::PathBuf>,
    /// The device's records, where the session keeps what outlives it: the shell as it was left,
    /// the pictures on it, the notification centre. The session fills it in from its daemon's
    /// link when the caller hands none; without either, it starts on a fresh shell and keeps
    /// nothing.
    pub records: Option<tilo_session_client::device_records::DeviceRecords>,
    /// The one stream of changes of the workspaces' records on this device, which the roles that
    /// follow records (the activity, the content index, the person's mentions) read again on.
    /// Without one, the session follows no workspace's records (most tests).
    pub record_changes: Option<crate::platform::records::Changes>,
    /// Whether the session takes part in the peer network: who is here in each workspace,
    /// over Iroh. Off, this desktop has no members but its own person; the workspaces it makes
    /// are still founded on its own workspace service, so what it lays out comes back after a
    /// restart.
    pub collab: bool,
    /// A peer network to take part in instead of starting one: a session a test hands the
    /// presence a workspace would have carried ([`sync::Client::untethered`]), which says nothing
    /// on a network.
    pub sync: Option<Rc<sync::Client>>,
    /// The device VPN, when this session runs on a machine with the helper: a VPN profile set to
    /// connect and not connected blocks traffic while its kill switch is on, and this device routes
    /// through another of the person's devices while they choose one. Without one, nothing does.
    pub vpn: Option<Vpn>,
    /// Whether the apps this session runs read and invoke what its services publish, and on which
    /// socket. Off, the apps it starts reach no service at all.
    pub app_socket: AppSocket,
    /// Where the session's MCP server listens ([`mcp_server::path`]): the socket a program of this
    /// login asks on to work in a workspace as an agent, and the person admits it. `None` : this
    /// session serves no agents (a test).
    pub mcp_server: Option<std::path::PathBuf>,
    /// Where the person's own programs on this computer reach Tilo's API, as them: its login's
    /// `api.sock`. The device's daemon serves it; the session takes each connection there and
    /// hands it over on its shell link ([`crate::app_socket::serve_api`]). `None`: no socket.
    pub api_socket: Option<std::path::PathBuf>,
    /// The agent key the person starts the session with (`--agent-key FILE`): the session keeps
    /// a secret key in FILE (made there when FILE is empty or absent), and the person's root
    /// certifies an agent's grant for it, listed with their devices. A program presenting the key
    /// at the MCP server ([`tilo_mcp::session_api::AGENT_KEY_ENV`]) is admitted as that
    /// grant's agent without asking them: what a session started for an agent, with no one to
    /// answer the API, lets the agent in by. Revoking the grant, as a device or from the agent's
    /// card, ends it.
    pub agent_key: Option<std::path::PathBuf>,
    /// Where the development test-control socket opens ([`crate::test_control`]), which
    /// `tilo-control` drives the session over: `tilo-session --test-control PATH` , honoured by
    /// development builds only. `None` : there is none.
    pub test_control: Option<std::path::PathBuf>,
    /// Where the device admin's control socket opens ([`crate::test_control::admin_path`]), which
    /// `tilo-control` reaches as the account whose desktop this is:
    /// `tilo-session --admin-control PATH` , else this device's own `admin-control.sock` for a session
    /// that owns the machine, and `headless-owner-control.sock` for a headless one. `None` : there
    /// is none.
    pub admin_control: Option<std::path::PathBuf>,
    /// The shell's end of the link the device's daemon made for this session, when the session
    /// runs in a test's own process (`tilo_runtime::daemon::Running::link_shell`): a session the
    /// daemon started redeems the one-use secret it was given (`TILO_SHELL_TOKEN`) for a link of
    /// its own instead. `None`: that one.
    #[cfg(unix)]
    pub shell_link: Option<std::os::fd::OwnedFd>,
    /// Why the device's daemon answered this session no link, when it said so: what the screen in
    /// front of a login it did not start shows as the cause. `None`: no refusal to show.
    #[cfg(unix)]
    pub shell_link_refusal: Option<String>,
    /// The person's settings, installed as this process's by whoever starts the session before it
    /// reads any of them: the desktop services and the session's own config read them before the
    /// session is built, and a watch on the settings follows only a source installed when it is
    /// made. `None`: the session installs them first thing as it starts.
    #[cfg(target_os = "linux")]
    pub settings: Option<crate::SessionSettings>,
    /// What keeps the daemon that made [`Self::shell_link`] running, ended only after the session
    /// is: a test's daemon in its own process, with the profile it serves. `None` for a session
    /// the daemon started.
    #[cfg(unix)]
    pub shell_daemon: Option<Box<dyn std::any::Any>>,
    /// How the connectivity service shares a saved network's password with the person's other
    /// devices: the session attaches the device's daemon to it once its link is up.
    #[cfg(target_os = "linux")]
    pub wifi_sharing: crate::wifi_continuity::WifiSharing,
}

/// Which machine a session draws on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Machine {
    /// This one: the session owns idle, blanking, locking and suspend, the device's controllers and
    /// services, and the session actions it carries out reach this machine.
    #[default]
    Owned,
    /// A desktop this session did not start, in whose window it runs: that desktop owns idle,
    /// blanking, locking and suspend, so this one runs none of them and no session action of its
    /// reaches the machine under the window.
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
pub enum AppSocket {
    /// Nowhere: the apps reach no service.
    #[default]
    Off,
    /// On the socket the launcher hands an app for its session
    /// (`launcher::services::collab_socket`), in the session's runtime directory: every app the
    /// session starts, its windows and its agents reach it there, whatever machine it draws on.
    Launcher,
    /// On `path` : a session started for a test, which leaves the machine's own socket alone.
    At(std::path::PathBuf),
}

/// The default for [`Options::session_actions`]: a session that owns no machine (a test, a
/// screenshot, a session in a window) carries out nothing.
#[allow(
    clippy::unnecessary_wraps,
    reason = "the shape every session action runner has"
)]
pub fn no_session_actions(action: SessionAction) -> Result<(), String> {
    tracing::info!(?action, "this session carries out no session actions");
    Ok(())
}

/// Who is here: this session's membership of each workspace, over the device's daemon, which
/// holds the device's key and its one endpoint. A session the daemon did not start is a client of
/// no peer network: the daemon owns the key, and two endpoints under one key push each other off
/// the network, so none is bound here. The workspaces' live sessions the daemon gives the session
/// carry who is here and what they say, and the daemon says this device's one word
/// ([`crate::live::Live`]).
/// A caller that hands its own client ([`Options::sync`]) is taken as it is.
pub(crate) fn start_collaboration(
    options: &Options,
    live: Rc<std::cell::RefCell<Option<crate::live::Live>>>,
) -> Option<Rc<sync::Client>> {
    if let Some(sync) = &options.sync {
        return Some(Rc::clone(sync));
    }
    if !options.collab {
        return None;
    }
    let identity = sync::device_identity()?;
    let own = sync::OwnPresence {
        // Publish a name only when the session has one; viewers supply the unnamed label.
        label: crate::platform::person::session_name(
            ::collaboration::local_identity::load().map(|identity| identity.display_name),
            &options.person,
        )
        .unwrap_or_default(),
        user: Some(hex::encode(identity.device_cert.user_public_key)),
        ..sync::OwnPresence::default()
    };
    let publisher = iroh::EndpointId::from_bytes(&identity.device_cert.device_public_key).ok()?;
    let saying = live.borrow().as_ref().map(|live| {
        tilo_session_client::Saying::Word(
            Arc::new(live.telling()) as Arc<dyn tilo_session_client::TellWord>
        )
    });
    // Nothing this session says leaves on the peer network itself: what it says of the person
    // goes to the daemon's word, and the rest of the network is the daemon's.
    let (outgoing, _commands): (
        tokio::sync::mpsc::UnboundedSender<tilo_session_client::Cmd>,
        _,
    ) = tokio::sync::mpsc::unbounded_channel();
    let (_heard, arrived) = std::sync::mpsc::channel();
    let sync = Rc::new(sync::Client::over(
        sync::Transport {
            outgoing: Arc::new(outgoing),
            arrived,
            saying,
        },
        sync::Device {
            publisher,
            endpoint: iroh::EndpointAddr::new(publisher),
            person: Some(identity.device_cert.user_public_key),
            name: identity.device_cert.device_label,
        },
        own,
        live,
    ));
    Some(sync)
}

/// How the person's workspaces stand, as the device's daemon publishes them, and the actions that
/// change them: `None` when the session reaches no daemon, or this computer has no identity.
pub(crate) fn open_view(
    link: &tilo_app_host::services::DeviceDaemon,
    options: &Options,
) -> Option<std::sync::Arc<tilo_session_client::workspace_view::WorkspaceView>> {
    crate::device_shell::open(link.clone().ok()?, options.wake.handle())
}

/// Where the device's workspace service keeps its workspaces under this session's state: what
/// the agents tab and the workspaces' records read of the models it runs. `None` for a session
/// that keeps no state.
pub(crate) fn service_root(options: &Options) -> Option<std::path::PathBuf> {
    options
        .state
        .as_deref()
        .map(|state| state.join("tilo").join("workspace-service"))
}

impl Default for Options {
    fn default() -> Self {
        Self {
            salt: 1,
            scale: 1.0,
            device: "this-device".into(),
            person: "person".into(),
            presentation: ComponentRunnerInputs::from_native_process,
            sync: None,
            controllers: None,
            config: SessionConfig::default(),
            accelerometer: Accelerometer::default(),
            services: Vec::new(),
            backlights: None,
            search: SearchOptions::default(),
            lists: crate::lists::Lists::default(),
            content_socket: None,
            apps: None,
            launch: None,
            render_service: None,
            setup: None,
            #[cfg(target_os = "linux")]
            identity_backup: crate::startup::portable_backup,
            host_desktop: false,
            #[cfg(target_os = "linux")]
            hosted_browser: None,
            session_actions: no_session_actions,
            user: String::new(),
            earlier: Vec::new(),
            verifier: std::sync::Arc::new(unlock::Unavailable),
            recovery: None,
            #[cfg(target_os = "linux")]
            teardown: Box::new(|| tilo_app_launch::Teardown::Logout),
            wake: tilo_service::LoopWake::default(),
            captures: None,
            projector: None,
            pictures: None,
            desktop: None,
            asks: None,
            portal: None,
            surfaces: Vec::new(),
            machine: Machine::default(),
            power: None,
            state: None,
            records: None,
            record_changes: None,
            collab: true,
            vpn: None,
            app_socket: AppSocket::default(),
            mcp_server: None,
            api_socket: None,
            agent_key: None,
            test_control: None,
            admin_control: None,
            launches: std::sync::Arc::default(),
            windows: compat_service::Assembly::system(),
            windows_manager: compat_service::ManagerOut::new(),
            drives: None,
            #[cfg(unix)]
            shell_link: None,
            #[cfg(unix)]
            shell_link_refusal: None,
            #[cfg(target_os = "linux")]
            settings: None,
            #[cfg(unix)]
            shell_daemon: None,
            #[cfg(target_os = "linux")]
            wifi_sharing: crate::wifi_continuity::WifiSharing::default(),
        }
    }
}

/// The person's record of the answers they gave about this device's camera, screen and the rest,
/// in their own workspace, followed at the device's daemon as the device `cert` and changed through
/// its typed actions there, calling `wake` each time it moved. The devices each answer is on are
/// read from the record of the person's devices the session follows as `devices`.
pub(crate) fn answers_record(
    daemon: &tilo_session_client::daemon_documents::DaemonShell,
    cert: tilo_user_identity::DeviceCert,
    devices: crate::followed_resource::SharedFollow<
        tilo_workspace_service::workspaces::person_workspace::devices::Devices,
    >,
    wake: tilo_service::Wake,
) -> tilo_peer_state::AnswersRecord {
    use tilo_workspace_service::workspaces::person_workspace::device_answers::{
        DEVICE_ANSWERS, DeviceAnswers,
    };
    let person = cert.user_public_key;
    let record = crate::followed_resource::FollowedResource::<DeviceAnswers>::follow(
        daemon,
        (
            &tilo_server_protocol::servers::person_workspace_id(&person),
            DEVICE_ANSWERS.0,
            DEVICE_ANSWERS.1,
        ),
        tilo_member_types::PersonId::from_root(person).as_str(),
        None,
        wake,
    )
    .shared();
    let kept = record.clone();
    let answering = record.clone();
    let taking_back = record.clone();
    tilo_peer_state::AnswersRecord {
        cert,
        kept: std::sync::Arc::new(move || kept.get().kept()),
        changes: record.changes(),
        devices: std::sync::Arc::new(move || {
            devices
                .get()
                .certified_at(&person, tilo_clock::unix_now_ns())
        }),
        answer: std::sync::Arc::new(move |answer| {
            answering.invoke_unwaited(&DeviceAnswers::answering(&answer), warn_unkept);
        }),
        take_back: std::sync::Arc::new(move |device, app, cap| {
            taking_back.invoke_unwaited(&DeviceAnswers::taking_back(device, app, cap), warn_unkept);
        }),
    }
}

/// Say an answer about this device that the person's record did not take.
fn warn_unkept(went: Result<(), String>) {
    if let Err(error) = went {
        tracing::warn!(%error, "an answer about this device was not kept");
    }
}
