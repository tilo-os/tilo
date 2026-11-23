//! A hosted workspace's Browser: a window that runs in the workspace's own
//! VM, reached through this host.
//!
//! The relay runs no apps. Browser's page actions exist only on a live
//! Browser window, so a workspace hosted here opens Browser by starting a
//! headless Tilo session in the workspace's VM, the one its Linux programs
//! run in (`docs/hosted-browser.md`). That session is a device of the
//! workspace — a fresh key this host certifies for this one workspace — and it
//! opens the Browser window on the record the open made, so the window is
//! live and published like any other.
//!
//! This module is what the server does about it:
//!
//! - **Place** ([`HostedBrowsers::place`]): after `app.so.tilo.browser#open`
//!   made a record, certify a device for the session scoped to the
//!   workspace, list it as a member device, ask the hosted-VM service to
//!   start it ([`BrowserSessions::start`]) with that material, and
//!   remember the window it answered. The open answers once the window is
//!   live; a second open attaches to the same session.
//! - **Route** ([`HostedBrowsers::route`]): an action addressed to that
//!   window reaches this host (an agent's calls go to the workspace's
//!   host device) and is handed to the device holding the window, stamped
//!   with the caller, so it runs there as the caller's: History records the
//!   agent's run for the person it acts for. The server runs none of it. A
//!   listing of the window is handed over the same way, and the workspace's
//!   own listing names the window among its publishers
//!   ([`HostedBrowsers::listed`]), so a caller finds it as it finds any
//!   app's window. What the window answers such a caller reaches it as this
//!   host's, the device it asked ([`HostedBrowsers::answers_for_host`]).
//! - **Ask** ([`HostedBrowsers::ask`]): a page action asks the person first,
//!   and nobody stands in front of a window in a VM to ask. The window refuses
//!   such an action for want of their answer; this host puts the question to
//!   their approving devices itself, as it does for its own agents' actions,
//!   and answers its caller that the invocation waits. Allowed, the same
//!   invocation goes to the window again carrying their confirmation —
//!   signed by this host's own device key, for that one invocation — which
//!   the window that certified it accepts for its own person, running it
//!   without asking again; nothing else is a confirmation there. Denied or
//!   unanswered, it ends refused. A caller needs nothing beyond asking and
//!   following what it asked for. The window's refusal states the departure
//!   the action's run reached — what is about to leave, where, and the
//!   picture of the filled page — so the question names what leaves rather
//!   than a label for it, the same as for a departure the page started on its
//!   own. A key its person let operate web pages
//!   ([`DeviceCapabilities::browser_pages`]) is not asked about the page
//!   actions that lets it take — [`OPERATING_ACTIONS`], never a submission —
//!   and this host answers for them where their answer would have: the
//!   window runs it once, as theirs, and counts it. A departure the page
//!   started on its own — a script posting, a download — waits on nobody's
//!   invocation, so the session holding the window reports it here
//!   ([`Self::held_reported`]) and the person is asked about it just the
//!   same: their allow answers the session with the approval that releases
//!   that one request, and a denial keeps the page as it is.
//! - **Meter** ([`BrowserMeter`]): every page action is admitted for the
//!   workspace's account before it is handed over, and counted once the
//!   window answers that it ran. A refused one is answered with the host's
//!   typed [`Refusal`] — the VM path's refusal, not a second one — and one
//!   the person did not allow is never counted.
//!
//! - **Stop** ([`HostedBrowsers::stopped`]): the environment's device is
//!   refused on every protocol first — its entry in the server's register says it
//!   stopped, before anything is delisted, so a delisting that fails leaves it
//!   refused rather than open — then the session in the VM is ended, its
//!   device delisted, and every connection it had is cut. The register says so
//!   on disk too, so a server that restarts keeps it refused until the
//!   certificate it was given expires.
//!
//! What starts and stops sessions is the hosted-VM service's
//! ([`BrowserSessions`]): this host places, routes and counts, and stops an
//! environment when its session here ends ([`RECONNECT`] after the last one
//! went, unless the device is back by then): the signal is the device's own
//! sessions to this server, so a session that dies is heard wherever it ran,
//! and the certificate's expiry bounds whatever is missed.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod environment_device;
mod turn_fills;

pub use turn_fills::FILL_PAGE_ITEM;

use crate::WorkspaceId;
use crate::certification::CertificationProtocol;
use ed25519_dalek::SigningKey;
use iroh::EndpointAddr;
use tilo_actions::remote::linux_apps::{self, AppLaunch};
use tilo_actions::remote::server_account::{NextStep, NextStepKind, Reason, Refusal};
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionCallError, Caller, Completer,
    Confirmation, ConfirmedInvocation, InvocationReceipt, InvocationState, InvokeRequest, Pending,
    ReceiptStatus, RemoteActionMessage, argument_digest, pending, provider_workspace,
};
use tilo_actions::{ActionFailure, InvocationUpdate};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::certify::{ConfirmationAnswer, PageDeparture, PendingConfirmation};
use tilo_user_identity::{
    Answered, DeviceCapabilities, DeviceCert, HostApproval, Parties, sign_device_cert,
};
use tilo_workspace_service::agents::windows::{
    Ends, Releasing, TurnFill, Windows, opened_resource, opens_browser_at, page_action,
};

// A page action handed to a window for a turn this server carries out itself.
mod turn_pages;

/// The app a hosted workspace opens Browser through.
pub use tilo_app_id::apps::BROWSER as APP;
/// What people read Browser's name as.
pub const TITLE: &str = "Browser";

/// Browser as a host that places its windows lists it: a workspace opens
/// `app.so.tilo.browser`, and the record it makes is opened by the session in
/// the workspace's VM. This host mounts nothing of the app.
#[must_use]
pub fn placed_app() -> tilo_agent_session::PlacedApp {
    tilo_agent_session::PlacedApp {
        app_id: APP,
        title: TITLE,
        needs: None,
    }
}

/// A workspace's Linux apps as a host that places them lists them: a
/// workspace opens `app.so.tilo.linux` naming the app's desktop file, and the
/// app starts in the workspace's VM, in the apps session there
/// ([`tilo_actions::remote::linux_apps`]). This host mounts nothing of it.
#[must_use]
pub fn placed_linux_apps() -> tilo_agent_session::PlacedApp {
    tilo_agent_session::PlacedApp {
        app_id: linux_apps::APP,
        title: linux_apps::TITLE,
        needs: Some(tilo_agent_session::PlacedArgument {
            name: linux_apps::APP_ARGUMENT,
            description: "the app to open, by its desktop file's name, such as \
                          org.gnome.TextEditor",
        }),
    }
}

/// The label the workspace shows a Linux apps session's device under.
pub const APPS_DEVICE_LABEL: &str = "Linux apps";

/// Make this host the person's stand-in approver, from their root:
/// a browser session joins a workspace as a device of theirs, and a stand-in
/// is what certifies one. A host its person's owner runs holds their root
/// (a deployment's root file); a stand-in this host already keeps live is
/// left as it is.
///
/// # Errors
///
/// `state_root` is not writable, or the root would not sign the certificate.
pub fn keep_stand_in(
    state_root: &std::path::Path,
    root: &ed25519_dalek::SigningKey,
) -> Result<(), String> {
    crate::web::keep_stand_in(state_root, root)
}
/// The page actions a key holding [`DeviceCapabilities::browser_pages`]
/// runs without asking: navigating, filling and choosing in a page, clicking
/// and pressing what it found, scrolling and pointing. Submitting the page,
/// uploading the person's files into it, filling in a password, and whatever
/// else Browser holds for the person are none of them.
pub const OPERATING_ACTIONS: [&str; 9] = [
    "navigate",
    "fill-page",
    "select-page",
    "click-page",
    "press-page",
    "scroll-page",
    "drag-page",
    "pointer-path",
    "pointer-page",
];
/// The page action that keeps the page as it is, dropping the held request.
pub use tilo_actions::remote::browser::KEEP_HELD;
/// The page action that lets a held request leave, by the id the window
/// publishes it with: what a hold nothing called for is answered with.
pub use tilo_actions::remote::browser::SEND_HELD;
/// How long a device certified for a session lasts: as long as the session
/// may run.
pub const DEVICE_LIFETIME: Duration = Duration::from_secs(2 * 60 * 60);
/// How long an environment device whose last connection here ended has to
/// come back before its environment stops. A session in a VM that dies is
/// gone; one whose connection dropped is back in a moment, and its window
/// lives on.
pub const RECONNECT: Duration = Duration::from_secs(30);
/// How long the person's answer to one page action stays good for running
/// it: their confirmation of that invocation, not a lasting grant.
pub const APPROVAL_LIFETIME: u64 = 5 * 60 * 1_000_000_000;
/// The waits before a host asks again for a workspace it could not name yet
/// ([`HostedBrowsers::ready_workspace_name`]).
const NOT_READY_WAITS: [Duration; 4] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
];

/// How long a page action's question waits for the person: as long as the
/// window holds what it asks about (`apps/tl/browser`, whose window shows a page the engine draws: a
/// request wait ten minutes at most), less a margin, so an answer always
/// finds the request still held. Every device of the person is asked, and
/// told again by a notification when no Tilo of theirs is open.
pub const ANSWER_WAIT: Duration = Duration::from_secs(9 * 60);

/// The phase of a page action that waits for the person's answer.
pub const WAITING_FOR_PERSON: &str = "waiting_for_person";
/// What a caller following such a page action is told while it waits.
pub const WAITING_MESSAGE: &str =
    "waiting for the person to approve it in Tilo, on any of their devices; it runs once they do";
/// What a caller is told when the person did not answer while the window
/// could hold what the action reached.
pub const UNANSWERED: &str = "the person didn't answer in 9 minutes, so nothing was sent and \
     the page stays as it was filled; run the action again to ask them again";
/// `answer_wait` in nanoseconds.
fn answer_wait_ns(answer_wait: Duration) -> u64 {
    u64::try_from(answer_wait.as_nanos()).unwrap_or(u64::MAX)
}
/// The label the workspace shows the session's device under.
pub const DEVICE_LABEL: &str = "Browser";
/// The address the sessions in the sessions reach this host at, written
/// `KEY@HOST:PORT` ([`tilo_server_protocol::hosted::endpoint`]): this host's
/// device key and its **public** address.
///
/// A browser session's network reaches the public internet and nothing
/// else — no cluster-internal, private or link-local address answers it — so
/// a host that places sessions names the address they reach it at, and
/// never its own local one.
pub const SERVER_ENV: &str = "TILO_HOSTED_BROWSER_SERVER";

/// Where the sessions in this host's browser sessions reach it: the
/// address [`SERVER_ENV`] names, which the host must set to serve them.
///
/// # Errors
///
/// Nothing is named, or what is named is not `KEY@HOST:PORT`.
pub fn server_address() -> Result<EndpointAddr, String> {
    let named = std::env::var(SERVER_ENV).map_err(|_| {
        format!(
            "a host that places Browser sets {SERVER_ENV} to the address its \
             sessions reach it at (KEY@HOST:PORT): their network reaches no private one"
        )
    })?;
    tilo_server_protocol::hosted::endpoint(&named)
}

/// What a host hands a browser session when it gives it a workspace: the
/// device it certified for it, and where to be that device. The session in
/// the session reads the same type, so the two ends of the hand-off
/// cannot disagree about what it holds.
pub use tilo_server_protocol::hosted::Material;

/// A browser session that is up, and the window it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// The device of the workspace the session in the VM joined as.
    pub device: [u8; 32],
    /// The provider token the window publishes under (`browser@42`): what a
    /// page action names in place of the app.
    pub provider: String,
}

/// The service that starts and stops a workspace's browser sessions.
///
/// The workspace's devices that run Linux implement this
/// ([`crate::linux_devices::LinuxDevices`]): the one that takes the turn starts
/// the session in the workspace's VM with the [`Material`], and it answers once
/// the session reports its window live.
pub trait BrowserSessions: Send + Sync {
    /// Start the workspace's browser session with `material`, and answer its
    /// window once it is live.
    ///
    /// # Errors
    ///
    /// The words to refuse the open with: no host had room, or the host
    /// would not start it.
    fn start(&self, material: &Material) -> Result<Window, String>;

    /// The workspace's browser session stopped, or must: its device is
    /// forgotten and its window no longer exists.
    fn stop(&self, workspace: &str);

    /// Start `launch` in the workspace's Linux apps session, starting that
    /// session with `material` when none runs, and answer once it is up and
    /// was told to start the app. The app's windows join the shell as they
    /// map.
    ///
    /// # Errors
    ///
    /// The words to refuse the open with, and the plans' typed refusal when
    /// the account may not run Linux.
    fn start_app(&self, material: &Material, launch: &AppLaunch) -> Result<Window, AppRefusal> {
        let _ = (material, launch);
        Err(AppRefusal::Words(
            "this host starts no Linux apps session".to_owned(),
        ))
    }

    /// The workspace's Linux apps session stopped, or must.
    fn stop_apps(&self, workspace: &str) {
        let _ = workspace;
    }

    /// Ask the hosts that run sessions as `actions`'s device, from `endpoint`:
    /// this host's own, once it runs, holding `workspaces` read on `runtime`.
    /// Sessions that need no host to ask ignore it.
    fn ask_from(
        &self,
        actions: tilo_peer_state::actions::ActionService,
        endpoint: iroh::Endpoint,
        workspaces: crate::workspaces::Workspaces,
        runtime: tokio::runtime::Handle,
    ) {
        let _ = (actions, endpoint, workspaces, runtime);
    }
}

/// Why a workspace's Linux app did not open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppRefusal {
    /// The account's plan does not let it run Linux: the plans' own typed
    /// refusal, which says what would let it through.
    Refused(Refusal),
    /// Anything else, in words for whoever opened it.
    Words(String),
}

impl std::fmt::Display for AppRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refusal) => f.write_str(&refusal.message),
            Self::Words(words) => f.write_str(words),
        }
    }
}

/// What a host counts page actions against: its plans and the account's
/// member access.
pub trait BrowserMeter: Send + Sync {
    /// Whether `account` may run one more page action on a Browser this host
    /// placed.
    ///
    /// # Errors
    ///
    /// The typed refusal the action is answered with, which is the VM path's
    /// ([`Refusal`]).
    fn admit(&self, account: &str) -> Result<(), Refusal>;

    /// One page action ran for `account`, in the workspace it is booked to.
    fn record(&self, workspace: &str, account: &str);
}

/// A host whose plans file sells nothing: it counts nothing and refuses
/// nothing, as it does for VMs.
#[derive(Clone, Copy, Debug, Default)]
pub struct Unmetered;

impl BrowserMeter for Unmetered {
    fn admit(&self, _: &str) -> Result<(), Refusal> {
        Ok(())
    }

    fn record(&self, _: &str, _: &str) {}
}

/// What placing needs of the host itself, once it runs: the certificate it
/// makes browser sessions' devices with, where it is reached, the workspaces
/// it serves, and the register that confines each session's device to the one
/// record it was placed for.
pub struct Authority {
    pub user_public_key: [u8; 32],
    /// The person's stand-in approver for this host
    /// ([`tilo_user_identity::prepare_stand_in_cert`]): what a session's
    /// device is certified with, as a member device of the workspace's person
    /// for as long as the session may run. A host the person did not give
    /// one holds no certificate to make a session a device, and placing
    /// one is refused.
    pub cert: DeviceCert,
    /// That stand-in's own signing key.
    pub key: SigningKey,
    /// Where the session joins the workspace.
    pub server: EndpointAddr,
    pub workspaces: Option<crate::workspaces::Workspaces>,
    /// The server's environments' devices, confined to the record each was
    /// placed for ([`crate::environment`]): what every protocol of the workspace
    /// asks before it lets one in, or hands it a change.
    pub environments: crate::environment::EnvironmentRegistry,
    /// The runtime the workspaces run on: listing a device writes membership
    /// there.
    pub runtime: tokio::runtime::Handle,
    /// Where the person is asked about an action their agent made: the
    /// channel their approving devices answer on.
    pub certification: crate::certification::CertificationProtocol,
    /// Where a page action the person allowed is handed to the window that
    /// refused it, as the caller asked it: the host's own delivery.
    pub hand: crate::app_serving::Hand,
}

/// One workspace's window, while its session runs.
struct Open {
    /// The workspace, by its UUID, as its VM is named.
    workspace: String,
    window: Window,
    /// The record the window was opened on.
    record: String,
    /// The device the workspace lists for it, forgotten when it stops.
    device: DeviceCert,
    /// The devices whose frames this host handed the window: what the window
    /// answers them reaches them as this host's, the device they asked.
    callers: std::collections::HashSet<[u8; 32]>,
}

impl Open {
    /// `caller`'s frames are handed to the window from now on, and its
    /// answers reach `caller` as this host's. Bounded as what is handed is:
    /// a host that has handed the window more callers than that starts over.
    fn answers(&mut self, caller: [u8; 32]) {
        if self.callers.len() >= HANDED && !self.callers.contains(&caller) {
            self.callers.clear();
        }
        self.callers.insert(caller);
    }
}

/// A page action handed to a window, as the host handed it there: whose it
/// is, which window has it, and how far its answer has come.
struct Handed {
    /// The caller, as the window hears it and the person's question names it.
    caller: Caller,
    window: [u8; 32],
    /// The workspace, by its UUID: whose account it is counted on.
    workspace: String,
    /// The action as the caller asked it: what the person's question shows,
    /// and what runs once they allow it.
    request: InvokeRequest,
    stage: Stage,
    /// Callers asking how it went, waiting for it to move, by the number
    /// that finds them again once their wait is over.
    waiting: Vec<(u64, Completer<RemoteActionMessage>)>,
}

/// How far a handed page action has come.
enum Stage {
    /// With the window, which answers the caller itself — as it answers a
    /// caller asking after it.
    Running,
    /// The window refused it for want of the person's word: this host asks
    /// them, and the caller waits for their answer.
    Asking,
    /// The person allowed it: with the window again, carrying their answer.
    Allowed,
    /// It is over: the update a caller asking after it is answered with.
    Over(InvocationUpdate),
}

impl Stage {
    /// The state an invocation in this stage is answered with when a caller
    /// asks how it went.
    fn state(&self, invocation: tilo_actions::InvocationId) -> InvocationUpdate {
        match self {
            Self::Running | Self::Allowed => InvocationUpdate::Accepted { invocation },
            // Said to the caller as what it waits on, so an agent following
            // it tells whoever it works for to look at their device rather
            // than guess why nothing happens.
            Self::Asking => InvocationUpdate::Progress {
                invocation,
                sequence: 0,
                phase: WAITING_FOR_PERSON.to_owned(),
                completed: None,
                total: None,
                message: Some(WAITING_MESSAGE.to_owned()),
            },
            Self::Over(update) => update.clone(),
        }
    }
}

/// How many handed page actions are remembered while their windows answer:
/// beyond it, the oldest are forgotten and only go uncounted.
const HANDED: usize = 8192;

/// The Browser this host places for its workspaces, and the page actions it
/// hands to the windows.
pub struct HostedBrowsers {
    sessions: Box<dyn BrowserSessions>,
    meter: Box<dyn BrowserMeter>,
    /// Who pays for the workspaces' VMs, as this host knows it.
    account: String,
    /// The windows up now, by workspace.
    open: Mutex<BTreeMap<WorkspaceId, Open>>,
    /// Held while a window is placed: what makes two opens one session.
    placing: Mutex<()>,
    /// The Linux apps sessions up now, by workspace: each holds the windows
    /// of the apps it started, which it shares with the workspace itself.
    apps: Mutex<BTreeMap<WorkspaceId, Open>>,
    authority: Mutex<Option<Authority>>,
    /// The page actions handed to a window and not yet answered, by
    /// invocation: what a count is owed for once it ran, and what asks first.
    handed: Arc<Mutex<HashMap<tilo_actions::InvocationId, Handed>>>,
    /// The held requests the person is being asked about, by workspace and
    /// the id the window published them with: a session that reports one
    /// again is told it is asked already rather than asked twice.
    asking: Arc<Mutex<std::collections::HashSet<(WorkspaceId, u32)>>>,
    /// The page actions this server handed to windows for the turns it carries out itself
    /// ([`turn_pages`]).
    turn_pages: turn_pages::TurnPages,
    /// The number naming the next caller waiting for an invocation to move.
    next_waiter: AtomicU64,
    /// How long an environment device that disconnected has to come back
    /// before its environment stops ([`RECONNECT`]).
    reconnect: Duration,
    /// How long a page action's question waits for the person
    /// ([`ANSWER_WAIT`]).
    answer_wait: Duration,
    /// The fills of a page's field this server handed to windows for the turns it carries
    /// out itself ([`turn_fills`]).
    turn_fills: turn_fills::TurnFills,
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "each method holds one guard for one answer; placing holds it so two opens make one session"
)]
impl HostedBrowsers {
    /// A host that places Browser through `sessions`, counting page
    /// actions of `account` in `meter`.
    pub fn new(
        sessions: Box<dyn BrowserSessions>,
        meter: Box<dyn BrowserMeter>,
        account: String,
    ) -> Self {
        Self {
            sessions,
            meter,
            account,
            open: Mutex::default(),
            placing: Mutex::default(),
            apps: Mutex::default(),
            authority: Mutex::default(),
            handed: Arc::default(),
            asking: Arc::default(),
            turn_pages: turn_pages::TurnPages::default(),
            next_waiter: AtomicU64::new(0),
            reconnect: RECONNECT,
            answer_wait: ANSWER_WAIT,
            turn_fills: turn_fills::TurnFills::default(),
        }
    }

    /// Wait `reconnect` for a disconnected environment device to come back
    /// before its environment stops ([`RECONNECT`]).
    #[must_use]
    pub const fn with_reconnect(mut self, reconnect: Duration) -> Self {
        self.reconnect = reconnect;
        self
    }

    /// Wait `answer_wait` for the person's answer to a page action's question
    /// before it is refused ([`ANSWER_WAIT`]).
    #[must_use]
    pub const fn with_answer_wait(mut self, answer_wait: Duration) -> Self {
        self.answer_wait = answer_wait;
        self
    }

    /// The Browser this host's environment names, when it serves Browser
    /// at all: where they start and stop (the workspaces' devices that run
    /// Linux, [`crate::linux_devices::LinuxDevices`]), the log their page
    /// actions are counted in ([`crate::browser_actions::BrowserMeter`]), and
    /// the address the sessions reach this host at.
    ///
    /// # Errors
    ///
    /// Sessions are named but cannot be started, or their page actions
    /// cannot be counted: a host that runs browsers without counting them
    /// would serve them free of charge, so the start fails rather than run
    /// half of it.
    pub fn from_env() -> Result<Option<Self>, String> {
        let Some(sessions) = crate::linux_devices::LinuxDevices::from_env() else {
            return Ok(None);
        };
        let meter = crate::browser_actions::BrowserMeter::from_env()?.ok_or_else(|| {
            format!(
                "{} names who pays for Browser sessions, so {} must name the log their \
                 page actions are counted in",
                tilo_server_account::ACCOUNT_ENV,
                tilo_server_account::browser_actions::ACTIONS_LOG_ENV
            )
        })?;
        // The address their sessions reach this host at is part of serving
        // them: a host that cannot say it fails here, not at the first open.
        server_address()?;
        let account = sessions.account().to_owned();
        Ok(Some(Self::new(
            Box::new(sessions),
            Box::new(meter),
            account,
        )))
    }

    /// Name the host's identity and workspaces, once it runs: needed to
    /// certify a session's device and to list it in its workspace. It is also
    /// where this host hears that one of its environment's sessions ended
    /// ([`Self::session_ended`]).
    pub fn serve_from(self: &Arc<Self>, authority: Authority) {
        let browsers = Arc::downgrade(self);
        authority
            .environments
            .when_ended(Arc::new(move |scope, device| {
                if let Some(browsers) = browsers.upgrade() {
                    browsers.session_ended(scope.workspace, device);
                }
            }));
        *self.authority.lock_recover() = Some(authority);
    }

    /// Ask the VM hosts that start sessions as `actions`'s device, from
    /// `endpoint`: this host's own. Until then every open is refused, as
    /// the host is not running yet.
    pub fn ask_from(
        &self,
        actions: tilo_peer_state::actions::ActionService,
        endpoint: iroh::Endpoint,
        workspaces: crate::workspaces::Workspaces,
        runtime: tokio::runtime::Handle,
    ) {
        self.sessions
            .ask_from(actions, endpoint, workspaces, runtime);
    }

    /// The runtime this host serves on, when it runs: where what it follows between messages — an
    /// open that answers with the run it started — is polled.
    pub(crate) fn runtime(&self) -> Option<tokio::runtime::Handle> {
        self.authority
            .lock_recover()
            .as_ref()
            .map(|authority| authority.runtime.clone())
    }

    /// The window `workspace_id`'s Browser is in, when one is up.
    #[must_use]
    pub fn window(&self, workspace_id: WorkspaceId) -> Option<Window> {
        self.open
            .lock_recover()
            .get(&workspace_id)
            .map(|open| open.window.clone())
    }

    /// `answer`, this host's own answer to `query` in `workspace_id`, with
    /// the Browser window among the publishers it lists while one is up: the
    /// window runs in the workspace's VM, not on this host, yet it is the
    /// workspace's like any app's window, so a caller asking what runs there
    /// finds it, and reads and acts on it through this host. Only a listing
    /// naming no provider lists publishers; any other answer is as it was.
    #[must_use]
    pub fn listed(
        &self,
        workspace_id: WorkspaceId,
        query: &tilo_actions::remote::CatalogueQuery,
        mut answer: RemoteActionMessage,
    ) -> RemoteActionMessage {
        if query.provider.is_some() {
            return answer;
        }
        let RemoteActionMessage::CatalogueResponse {
            result: Ok(page), ..
        } = &mut answer
        else {
            return answer;
        };
        let open = self.open.lock_recover();
        let Some(up) = open.get(&workspace_id) else {
            return answer;
        };
        if page
            .providers
            .iter()
            .any(|listed| listed.provider == up.window.provider)
        {
            return answer;
        }
        // Named by the workspace it was opened in: a caller holding what runs
        // in its own workspace to that workspace keeps it. The host's own rows
        // name no workspace (they are the caller's by the host serving them),
        // so theirs is not the name to copy.
        page.providers
            .push(tilo_actions::remote::CatalogueProvider {
                provider: up.window.provider.clone(),
                app_id: APP.to_owned(),
                workspace: up.workspace.clone(),
                document_id: up.record.clone(),
                browsable: true,
            });
        answer
    }

    /// Where `workspace_id`'s page action goes: to the device holding its
    /// window, once the meter admits it for the workspace's account; answered
    /// here with the host's typed refusal otherwise. `None` for an action
    /// that is not a page action of that window, which this host serves
    /// itself.
    ///
    /// A caller asking how one of those went is answered here too
    /// ([`Self::observe`]): by the window while it runs there, and by this
    /// host while the person is being asked and once it is over.
    #[must_use]
    pub fn route(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        message: &RemoteActionMessage,
    ) -> Option<crate::app_serving::Routing> {
        self.route_as(workspace_id, caller, None, message)
    }

    /// [`Self::route`], with the certificate the server's handshake verified
    /// for the caller's device: a key whose person let it operate web pages
    /// ([`DeviceCapabilities::browser_pages`]) has its page actions of
    /// [`OPERATING_ACTIONS`] answered for them here, as their own answer
    /// answers a question. The window still decides: the caller's key reaches
    /// no workspace it was not admitted to, and Browser still holds every
    /// submission for the person.
    #[must_use]
    pub fn route_certified(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        certificate: &DeviceCert,
        message: &RemoteActionMessage,
    ) -> Option<crate::app_serving::Routing> {
        self.route_as(workspace_id, caller, Some(certificate), message)
    }

    /// Where a message that invokes nothing goes: what a window reports of an action it holds,
    /// what a window says of a page action this server handed it for a turn of its own, which
    /// is taken here since nobody on a connection asked for it, or a caller asking after an
    /// invocation.
    fn answered_or_asked_after(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        message: &RemoteActionMessage,
    ) -> Option<crate::app_serving::Routing> {
        if let RemoteActionMessage::Confirm {
            correlation,
            request,
        } = message
        {
            return self.held_reported(workspace_id, caller, *correlation, request);
        }
        if let Some(taken) = self.page_answered(workspace_id, caller, message) {
            return Some(taken);
        }
        self.asked_after(caller, message)
    }

    fn route_as(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        message: &RemoteActionMessage,
    ) -> Option<crate::app_serving::Routing> {
        if let RemoteActionMessage::Invoke(request) = message
            && let Some(answered) = self.environment_asked(workspace_id, caller, request)
        {
            return Some(answered);
        }
        // What a window says of a fill this server handed it for one of its own turns is
        // taken here: nobody on a connection asked for it.
        if let Some(taken) = self.fill_answered(workspace_id, caller, message) {
            return Some(taken);
        }
        // What the window offers is read from the window: a listing of its
        // provider is handed to the device holding it, as its page actions
        // are, and answered there as the caller's.
        if let RemoteActionMessage::CatalogueRequest { query, .. } = message {
            let mut open = self.open.lock_recover();
            let up = open.get_mut(&workspace_id)?;
            if query.provider.as_deref() != Some(up.window.provider.as_str()) {
                return None;
            }
            up.answers(*caller.device_public_key()?);
            return Some(crate::app_serving::Routing::Device(up.window.device));
        }
        let RemoteActionMessage::Invoke(request) = message else {
            return self.answered_or_asked_after(workspace_id, caller, message);
        };
        let (provider, window) = {
            let open = self.open.lock_recover();
            let up = open.get(&workspace_id)?;
            (up.window.provider.clone(), up.window.device)
        };
        if !page_action(&request.target, &provider) {
            return None;
        }
        let device = *caller.device_public_key()?;
        let permitted = match &request.confirmation {
            None => true,
            Some(Confirmation::Approved(token)) => {
                self.approved(token, workspace_id, device, request)
            }
            Some(_) => false,
        };
        if !permitted {
            return answered_with(message, ActionCallError::NotPermitted);
        }
        // A page action the person is already being asked about waits for
        // their answer: a caller that asks again is told so, not handed over
        // twice.
        if request.confirmation.is_none() && self.waits_for_the_person(request, device) {
            return Some(waiting(request.invocation));
        }
        // A key whose person let it operate web pages needs no question about
        // one: this host answers for them with the same approval their answer
        // would have made, so the window runs it once and answers the caller
        // itself.
        let mut operated = None;
        if request.confirmation.is_none()
            && certificate.is_some_and(|certificate| operates(certificate, request))
            && let (Some(&person), Some((_, key))) = (caller.user_public_key(), self.certifying())
        {
            let asked = Asked {
                person,
                window,
                device,
                request: request.clone(),
                departure: None,
                until_ns: tilo_clock::unix_now_ns()
                    .saturating_add(answer_wait_ns(self.answer_wait)),
            };
            let mut run = request.clone();
            match asked.confirmed(&Ok(ConfirmationAnswer::AllowOnce), &key) {
                Ok(confirmed) => {
                    confirmed.apply(&mut run);
                    operated = Some(run);
                }
                Err(error) => return answered_with(message, error),
            }
        }
        // A page action handed to the window without this host's answer is one
        // the window holds for the person: what the server says here is why it
        // could not answer for them, which a key that may operate web pages is
        // answered for.
        if operated.is_none()
            && request.confirmation.is_none()
            && OPERATING_ACTIONS.contains(&request.target.address.action.as_str())
        {
            note_unanswered_handover(workspace_id, request, certificate);
        }
        let request = operated.as_ref().unwrap_or(request);
        match self.meter.admit(&self.account) {
            Ok(()) => {
                if let Some(up) = self.open.lock_recover().get_mut(&workspace_id) {
                    up.answers(device);
                }
                self.hand_over(workspace_id, caller.clone(), window, request);
                Some(match operated {
                    // The window is handed the action with the person's
                    // answer this host made for it, not the frame the caller
                    // sent, which carries none.
                    Some(operated) => crate::app_serving::Routing::Replaced {
                        to: window,
                        message: Box::new(RemoteActionMessage::Invoke(operated)),
                    },
                    None => crate::app_serving::Routing::Device(window),
                })
            }
            Err(refusal) => answered_with(
                message,
                ActionCallError::Refused {
                    reason: refusal.to_reason(),
                },
            ),
        }
    }

    /// A departure a window holds that no page action is waiting on — the
    /// page posted on its own, or started a download — is put to the person
    /// here: the session holding the window reports it
    /// ([`RemoteActionMessage::Confirm`] naming the window's [`SEND_HELD`]),
    /// this host asks the person once, and their answer is the approval that
    /// releases that one request, made for the invocation the report names.
    /// A denial, or a question nobody answers, is the answer that keeps the
    /// page as it is: the session runs [`KEEP_HELD`] with it.
    ///
    /// Only the environment's own device reports a hold of its own window,
    /// and a hold it reports again while the person is being asked is told
    /// so, never asked about twice. `None` for a message that is not this
    /// host's: it is served, or refused, as it was.
    fn held_reported(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        correlation: u64,
        request: &InvokeRequest,
    ) -> Option<crate::app_serving::Routing> {
        let (provider, window) = {
            let open = self.open.lock_recover();
            let up = open.get(&workspace_id)?;
            (up.window.provider.clone(), up.window.device)
        };
        if *caller.device_public_key()? != window
            || !page_action(&request.target, &provider)
            || request.target.address.action != SEND_HELD
        {
            return None;
        }
        let refused = |error: ActionCallError| {
            Some(crate::app_serving::Routing::Answer(Pending::ready(
                RemoteActionMessage::ConfirmResponse {
                    correlation,
                    result: Err(error),
                },
            )))
        };
        if request.confirmation.is_some() {
            return refused(ActionCallError::NotPermitted);
        }
        let Some(held) = held_of(&request.argument) else {
            return refused(ActionCallError::InvalidRequest {
                reason: "a held request is named by the id it was published with".to_owned(),
            });
        };
        let (Some(&person), Some(device)) = (caller.user_public_key(), caller.device_public_key())
        else {
            return refused(ActionCallError::NotPermitted);
        };
        let (Some((certification, key)), Some(_)) = (self.certifying(), self.hand()) else {
            return refused(ActionCallError::Unavailable {
                reason: "this host asks nobody about a page".into(),
            });
        };
        if !self.asking.lock_recover().insert((workspace_id, held)) {
            return refused(ActionCallError::Unavailable {
                reason: "the person is already being asked about that request".into(),
            });
        }
        let asked = Asked {
            person,
            window,
            device: *device,
            request: request.clone(),
            // The departure the session reported is what the person is asked
            // about: the window stated it beside the hold it published.
            departure: departure_of(request.context.as_ref()),
            until_ns: tilo_clock::unix_now_ns().saturating_add(answer_wait_ns(self.answer_wait)),
        };
        let question = asked.question(caller);
        let (answer, answered) = pending();
        let asking = Arc::clone(&self.asking);
        let taking = self.submission(
            workspace_id,
            format!("browser-hold/{}/{held}", hex::encode(window)),
        );
        tokio::runtime::Handle::current().spawn(async move {
            let (result, taken) = match taking.await {
                Ok(Taking::Answered(answer)) => (answer.map(Box::new), None),
                Ok(Taking::Taken(taken)) => {
                    let answered_by_person = certification.confirm_for(person, question).await;
                    let result = taken
                        .handing(asked.confirmed(&answered_by_person, &key))
                        .await
                        .map(Box::new);
                    (result, Some(taken))
                }
                Err(error) => (Err(error), None),
            };
            asking.lock_recover().remove(&(workspace_id, held));
            answer.complete(RemoteActionMessage::ConfirmResponse {
                correlation,
                result,
            });
            if let Some(taken) = taken {
                taken.release().await;
            }
        });
        Some(crate::app_serving::Routing::Answer(answered))
    }

    /// The window's answer to a page action this host handed it: it ran
    /// (`Completed`), so it is counted — once, when it ran — and its caller is
    /// answered with the page's receipt; the window asks the person first
    /// (`ConfirmationRequired`), so this host puts the question to them and
    /// answers the caller that its invocation waits, rather than refused.
    /// Anything else ran nothing and is counted not at all.
    pub fn relayed(
        &self,
        workspace_id: WorkspaceId,
        from: [u8; 32],
        message: &RemoteActionMessage,
    ) -> Option<RemoteActionMessage> {
        let RemoteActionMessage::Receipt(receipt) = message else {
            return None;
        };
        if !self.open.lock_recover().contains_key(&workspace_id) {
            return None;
        }
        let mut handed = self.handed.lock_recover();
        let entry = handed.get_mut(&receipt.invocation)?;
        if entry.window != from {
            return None;
        }
        match &receipt.status {
            ReceiptStatus::Completed { .. } => {
                // The action ran where the window says it did, and nowhere
                // else: a count is owed once.
                let allowed = matches!(entry.stage, Stage::Allowed);
                let ran = allowed || matches!(entry.stage, Stage::Running);
                let workspace = entry.workspace.clone();
                answered(entry, &receipt.status);
                let update = entry.stage.state(receipt.invocation);
                drop(handed);
                if ran {
                    self.counted(&workspace);
                }
                // The caller's own invocation was answered with "accepted,
                // it waits": what ran is told to it as the state it follows,
                // not as a second receipt for the same invocation.
                allowed.then(|| state_message(receipt.invocation, update, None))
            }
            ReceiptStatus::Rejected {
                error: ActionCallError::ConfirmationRequired { departure },
            } if !matches!(entry.stage, Stage::Over(_)) => {
                entry.stage = Stage::Asking;
                let caller = entry.caller.clone();
                let request = entry.request.clone();
                let departure =
                    departure_of_facts(&tilo_actions::remote::browser::stated_facts(departure));
                drop(handed);
                self.ask(workspace_id, &caller, &request, departure);
                Some(waiting_receipt(request.invocation))
            }
            // Running, or to be followed: the answer is still to come, and
            // what it settles is counted then.
            ReceiptStatus::Accepted => None,
            ReceiptStatus::Rejected { .. }
            | ReceiptStatus::Failed { .. }
            | ReceiptStatus::Cancelled { .. } => {
                answered(entry, &receipt.status);
                None
            }
        }
    }

    /// Count one page action a window of `workspace` says it carried out.
    fn counted(&self, workspace: &str) {
        self.meter.record(workspace, &self.account);
    }

    /// Ask the person `caller` acts for whether `request` — a page action of
    /// `workspace_id`'s window — may run, once. Their answer runs it as the
    /// caller asked it, carrying the approval the window accepts; a denial, or
    /// a question nobody answers, ends the invocation refused and nothing
    /// runs. `departure` is what the window said it holds for them, when it
    /// said so: the question names what leaves a page rather than a label.
    fn ask(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
        departure: Option<PageDeparture>,
    ) {
        let Some(window) = self.window_of(workspace_id, request) else {
            return;
        };
        let (Some(&person), Some(&device)) = (caller.user_public_key(), caller.device_public_key())
        else {
            return;
        };
        let (Some((certification, key)), Some(hand)) = (self.certifying(), self.hand()) else {
            return;
        };
        let asked = Asked {
            person,
            window,
            device,
            request: request.clone(),
            departure,
            until_ns: tilo_clock::unix_now_ns().saturating_add(answer_wait_ns(self.answer_wait)),
        };
        let question = asked.question(caller);
        tracing::info!(
            workspace = %workspace_id,
            action = %request.target.address.action,
            person = %hex::encode(person),
            "asking the person about a page action"
        );
        let invocation = request.invocation;
        let mut run = request.clone();
        let handed = Arc::clone(&self.handed);
        let taking = self.submission(workspace_id, format!("browser-hold/{}", invocation.0));
        tokio::runtime::Handle::current().spawn(async move {
            let (confirmed, taken) = match taking.await {
                // The person answered it already: their answer goes to the
                // window as it did then, and the window, which keeps what
                // each invocation did, carries it out at most once.
                Ok(Taking::Answered(answer)) => (answer, None),
                Ok(Taking::Taken(taken)) => {
                    let answered = certification.confirm_for(person, question).await;
                    (
                        taken.handing(asked.confirmed(&answered, &key)).await,
                        Some(taken),
                    )
                }
                Err(error) => (Err(error), None),
            };
            {
                let mut handed = handed.lock_recover();
                // A caller that stopped waiting while the person decided
                // has nothing done for it.
                if let Some(entry) = handed.get_mut(&invocation) {
                    match confirmed {
                        Ok(confirmed) => {
                            confirmed.apply(&mut run);
                            entry.stage = Stage::Allowed;
                            drop(handed);
                            // The invocation goes once more, as the caller
                            // asked it, with the person's answer: the window
                            // that refused it accepts this one, and answers
                            // the caller itself.
                            hand.hand(
                                workspace_id,
                                (device, person),
                                window,
                                RemoteActionMessage::Invoke(run),
                            );
                        }
                        Err(error) => over(entry, update_of_error(invocation, &error)),
                    }
                }
            }
            if let Some(taken) = taken {
                taken.release().await;
            }
        });
    }

    /// Whether `request` of `device`'s is one this host is already asking the
    /// person about, or has handed over with their answer.
    fn waits_for_the_person(&self, request: &InvokeRequest, device: [u8; 32]) -> bool {
        self.handed
            .lock_recover()
            .get(&request.invocation)
            .is_some_and(|entry| {
                entry.caller.device_public_key() == Some(&device)
                    && matches!(entry.stage, Stage::Asking | Stage::Allowed)
            })
    }

    /// Answer a caller asking how one of a window's invocations went
    /// ([`RemoteActionMessage::Observe`]) or stopping one
    /// ([`RemoteActionMessage::Cancel`]).
    fn asked_after(
        &self,
        caller: &Caller,
        message: &RemoteActionMessage,
    ) -> Option<crate::app_serving::Routing> {
        match message {
            RemoteActionMessage::Observe {
                correlation,
                invocation,
                wait_ms,
                ..
            } => self.observe(caller, *invocation, *wait_ms, *correlation),
            RemoteActionMessage::Cancel { invocation } => self.cancel(caller, *invocation),
            _ => None,
        }
    }

    /// How `invocation` of `caller`'s went: its state now, or — for a caller
    /// that waits — as soon as it moves or its wait is over. `None` for one
    /// this host does not hold, which is answered as it always was.
    fn observe(
        &self,
        caller: &Caller,
        invocation: tilo_actions::InvocationId,
        wait_ms: Option<u32>,
        correlation: u64,
    ) -> Option<crate::app_serving::Routing> {
        let device = *caller.device_public_key()?;
        let mut handed = self.handed.lock_recover();
        let entry = handed.get_mut(&invocation)?;
        if entry.caller.device_public_key() != Some(&device) {
            return None;
        }
        match &entry.stage {
            // The window holds it, and answers whoever asks after it.
            Stage::Running => return Some(crate::app_serving::Routing::Device(entry.window)),
            Stage::Over(update) => {
                return Some(state_routing(invocation, update.clone(), correlation));
            }
            Stage::Asking | Stage::Allowed => {}
        }
        let wait = Duration::from_millis(u64::from(wait_ms.unwrap_or_default()));
        if wait.is_zero() {
            return Some(state_routing(
                invocation,
                entry.stage.state(invocation),
                correlation,
            ));
        }
        let token = self.next_waiter.fetch_add(1, Ordering::Relaxed);
        let (completer, answer) = pending();
        entry.waiting.push((token, completer));
        let handed = Arc::clone(&self.handed);
        tokio::runtime::Handle::current().spawn(async move {
            tokio::time::sleep(wait).await;
            let found = {
                let mut handed = handed.lock_recover();
                handed.get_mut(&invocation).and_then(|entry| {
                    let at = entry
                        .waiting
                        .iter()
                        .position(|(found, _)| *found == token)?;
                    let (_, waiting) = entry.waiting.remove(at);
                    Some((waiting, entry.stage.state(invocation)))
                })
            };
            if let Some((waiting, state)) = found {
                waiting.complete(state_message(invocation, state, Some(correlation)));
            }
        });
        Some(crate::app_serving::Routing::Answer(answer))
    }

    /// Stop `invocation` of `caller`'s: while the person is asked it never
    /// runs, and while the window has it the window stops it.
    fn cancel(
        &self,
        caller: &Caller,
        invocation: tilo_actions::InvocationId,
    ) -> Option<crate::app_serving::Routing> {
        let device = *caller.device_public_key()?;
        let mut handed = self.handed.lock_recover();
        let entry = handed.get_mut(&invocation)?;
        if entry.caller.device_public_key() != Some(&device) {
            return None;
        }
        let stopped = match &entry.stage {
            Stage::Running | Stage::Allowed => {
                return Some(crate::app_serving::Routing::Device(entry.window));
            }
            Stage::Asking => Ok(()),
            Stage::Over(_) => Err(ActionCallError::NotCancellable {
                reason: "it is over".to_owned(),
            }),
        };
        if stopped.is_ok() {
            handed.remove(&invocation);
        }
        Some(crate::app_serving::Routing::Answer(Pending::ready(
            RemoteActionMessage::CancelResponse {
                invocation,
                result: stopped,
            },
        )))
    }

    /// The window `workspace_id`'s page action `request` is about: `None` for
    /// anything that is not a page action of a window up here.
    fn window_of(&self, workspace_id: WorkspaceId, request: &InvokeRequest) -> Option<[u8; 32]> {
        let open = self.open.lock_recover();
        let up = open.get(&workspace_id)?;
        page_action(&request.target, &up.window.provider).then_some(up.window.device)
    }

    /// Where the person is asked about a page action, and the key their answer
    /// is approved with: this host's own, once it runs.
    fn certifying(&self) -> Option<(CertificationProtocol, SigningKey)> {
        let authority = self.authority.lock_recover();
        let authority = authority.as_ref()?;
        Some((authority.certification.clone(), authority.key.clone()))
    }

    /// Where a frame this host hands over after its own answer goes: the
    /// server's own delivery to the device holding the window.
    fn hand(&self) -> Option<crate::app_serving::Hand> {
        let authority = self.authority.lock_recover();
        Some(authority.as_ref()?.hand.clone())
    }

    /// Whether `token` is this host's answer for exactly this page action:
    /// the answer of the host that certified `workspace_id`'s window, for the
    /// person it serves, naming this caller, this invocation, this action and
    /// these arguments, and still good.
    fn approved(
        &self,
        token: &str,
        workspace_id: WorkspaceId,
        caller: [u8; 32],
        request: &InvokeRequest,
    ) -> bool {
        let Some(approval) = HostApproval::decode(token) else {
            return false;
        };
        let open = self.open.lock_recover();
        let Some(up) = open.get(&workspace_id) else {
            return false;
        };
        approval.stands_for(&up.device, tilo_clock::unix_now_ns())
            && approval.caller_device_public_key == caller
            && approval.invocation == *request.invocation.0.as_bytes()
            && approval.provider == request.target.provider
            && approval.resource == request.target.address.resource
            && approval.action == request.target.address.action
            && approval.argument == argument_digest(request.argument.to_slot().as_deref())
    }

    /// Whether what device `from` sends `to` in `workspace_id` is the
    /// Browser window answering a caller whose frames this host handed it:
    /// `to` asked this host, so it hears the answer from this host's device.
    #[must_use]
    pub fn answers_for_host(
        &self,
        workspace_id: WorkspaceId,
        from: [u8; 32],
        to: [u8; 32],
    ) -> bool {
        self.open
            .lock_recover()
            .get(&workspace_id)
            .is_some_and(|up| up.window.device == from && up.callers.contains(&to))
    }

    /// Whether `workspace_id` has a page action with a window, whose answer
    /// this host counts.
    #[must_use]
    pub fn tracks(&self, workspace_id: WorkspaceId) -> bool {
        self.open.lock_recover().contains_key(&workspace_id)
            && !self.handed.lock_recover().is_empty()
    }

    /// Remember that `invocation` of `caller`'s is with the window: what its
    /// answer is counted by, and what its caller is told about.
    fn hand_over(
        &self,
        workspace_id: WorkspaceId,
        caller: Caller,
        window: [u8; 32],
        request: &InvokeRequest,
    ) {
        let workspace = self
            .open
            .lock_recover()
            .get(&workspace_id)
            .map(|up| up.workspace.clone())
            .unwrap_or_default();
        let mut handed = self.handed.lock_recover();
        // One invocation is one run: what is already known about it — what
        // the window answers a caller that asks again, and how far the person
        // has come with it — stays.
        if handed.contains_key(&request.invocation) {
            return;
        }
        if handed.len() >= HANDED {
            handed.clear();
        }
        handed.insert(
            request.invocation,
            Handed {
                caller,
                window,
                workspace,
                request: request.clone(),
                stage: Stage::Running,
                waiting: Vec::new(),
            },
        );
    }

    /// The UUID of the workspace this host carries as `workspace_id`, when it
    /// serves one: what a page action and a session's device name it by.
    #[must_use]
    pub async fn workspace_name(&self, workspace_id: WorkspaceId) -> Option<String> {
        self.workspaces()?.workspace_identity(workspace_id).await
    }

    /// The workspaces this server serves, once it serves them.
    fn workspaces(&self) -> Option<crate::workspaces::Workspaces> {
        self.authority.lock_recover().as_ref()?.workspaces.clone()
    }

    /// Take the page submission `lease` names in `workspace_id`'s own state
    /// as this server's device, for as long as the person is given to answer
    /// and their answer to be handed over: of the workspace's holders, the
    /// one that took it asks the person and hands over their answer, and no
    /// other does, after a takeover or across a partition alike. A
    /// submission the person already answered — through this holder or
    /// another, before a restart or a takeover — is answered as they did,
    /// from what the workspace kept, and never asked about again.
    fn submission(
        &self,
        workspace_id: WorkspaceId,
        lease: String,
    ) -> impl Future<Output = Result<Taking, ActionCallError>> + Send + 'static {
        let workspaces = self.workspaces();
        let ttl = self.answer_wait.saturating_add(SUBMISSION_HANDOVER);
        async move {
            let unheld = || ActionCallError::Unavailable {
                reason: "this server holds no copy of the workspace to ask in".into(),
            };
            let workspaces = workspaces.ok_or_else(unheld)?;
            let workspace = workspaces
                .workspace_identity(workspace_id)
                .await
                .ok_or_else(unheld)?;
            if let Some(answer) = kept_answer(&workspaces, &workspace, &lease).await {
                return Ok(Taking::Answered(answer));
            }
            let device = workspaces.serving_certificate().clone();
            let claimed = workspaces
                .author_own_workspace_state(&workspace, &device, |peer| {
                    peer.workspace_mut().take_lease(
                        &device,
                        &lease,
                        ttl,
                        tilo_peer_state::unranked(),
                        std::time::SystemTime::now(),
                    )
                })
                .await
                .map_err(|error| ActionCallError::Unavailable {
                    reason: error.to_string(),
                })?;
            if claimed.is_none() {
                return Err(ActionCallError::Unavailable {
                    reason: ANOTHER_HOLDER_ASKS.into(),
                });
            }
            let taken = Submission {
                workspaces,
                workspace,
                lease,
            };
            // The merged state may hold an answer given before this holder
            // took the submission: it is the answer, asked never again.
            if let Some(answer) =
                kept_answer(&taken.workspaces, &taken.workspace, &taken.lease).await
            {
                taken.release().await;
                return Ok(Taking::Answered(answer));
            }
            Ok(Taking::Taken(taken))
        }
    }

    /// [`Self::workspace_name`], asked again a few times with a growing
    /// wait while this host is not ready to name it yet (it is still starting,
    /// or the workspace is still being bound here): an open that comes a moment
    /// early is placed, not refused. Waits on the runtime, never on a thread,
    /// so only a caller that can wait its own answer asks it.
    #[must_use]
    pub async fn ready_workspace_name(&self, workspace_id: WorkspaceId) -> Option<String> {
        for wait in NOT_READY_WAITS {
            if let Some(name) = self.workspace_name(workspace_id).await {
                return Some(name);
            }
            tokio::time::sleep(wait).await;
        }
        self.workspace_name(workspace_id).await
    }

    /// Place `workspace_id`'s Browser — the workspace whose UUID is
    /// `workspace` — on the record `record`, and answer the window once it is
    /// live. A Browser already up is the same window: a second open attaches
    /// to it.
    ///
    /// # Errors
    ///
    /// The words to refuse the open with: this host is not running, no device
    /// could be certified, or the session would not start.
    pub fn place(
        &self,
        workspace: &str,
        workspace_id: WorkspaceId,
        record: &str,
    ) -> Result<Window, String> {
        // One placement at a time, so two opens make one session; the windows
        // already up stay readable meanwhile, since a session's start takes
        // as long as its VM's wake and every frame of the workspace asks
        // about them.
        let _placing = self.placing.lock_recover();
        if let Some(up) = self.open.lock_recover().get(&workspace_id) {
            return Ok(up.window.clone());
        }
        tracing::info!(%workspace, record, "placing the workspace's Browser in its VM");
        let (device, material) = {
            let authority = self.authority.lock_recover();
            let authority = authority
                .as_ref()
                .ok_or("Browser is still starting. Try again in a minute.")?;
            let (device, secret) = certified(&authority.cert, &authority.key, DEVICE_LABEL)?;
            let material = Material {
                workspace: workspace.to_owned(),
                workspace_id,
                record: record.to_owned(),
                device: device.clone(),
                secret,
                server: authority.server.clone(),
                user_public_key: authority.user_public_key,
                account: self.account.clone(),
            };
            (device, material)
        };
        // Confine the device to that record before it can ask for anything
        // and before the workspace lists it: the register is the server's own
        // state, written down here, so a server that restarts still confines
        // it. The device itself is asked nothing.
        let until_ns = device
            .expires_at_ns
            .unwrap_or_else(|| tilo_clock::unix_now_ns().saturating_add(lifetime_ns()));
        if let Err(error) = self.confine(
            &device,
            crate::environment::EnvironmentScope::for_resource(workspace_id, APP, record),
            until_ns,
        ) {
            tracing::warn!(%error, "the Browser's device could not be confined");
            return Err("Browser couldn't start. Try again in a minute.".to_owned());
        }
        let window = match self.sessions.start(&material) {
            Ok(window) => window,
            Err(error) => {
                // The device never left this host, so nothing holds it: it is
                // forgotten rather than left a member of the workspace.
                self.forget(&device);
                return Err(error);
            }
        };
        self.list(workspace_id, &device, true);
        tracing::info!(
            %workspace,
            provider = %window.provider,
            device = %hex::encode(window.device),
            "the workspace's Browser window is up in its VM"
        );
        self.open.lock_recover().insert(
            workspace_id,
            Open {
                workspace: workspace.to_owned(),
                window: window.clone(),
                record: record.to_owned(),
                device,
                callers: std::collections::HashSet::new(),
            },
        );
        Ok(window)
    }

    /// Start `launch` in `workspace_id`'s Linux apps session — the workspace
    /// whose UUID is `workspace`, opened on the record `record` — starting
    /// that session when none runs: a device certified for it, confined to
    /// that record and the workspace's own document (the shell its windows
    /// stand on), and listed as a member device, as a Browser session is. A
    /// session already up is told to start the app.
    ///
    /// # Errors
    ///
    /// What to refuse the open with: this host is not running, no device
    /// could be certified, the account may not run Linux, or the session
    /// would not start.
    pub fn place_app(
        &self,
        workspace: &str,
        workspace_id: WorkspaceId,
        record: &str,
        launch: &AppLaunch,
    ) -> Result<Window, AppRefusal> {
        let words = AppRefusal::Words;
        let mut apps = self.apps.lock_recover();
        let (device, material) = {
            let authority = self.authority.lock_recover();
            let authority = authority.as_ref().ok_or_else(|| {
                words("Linux apps are still starting. Try again in a minute.".to_owned())
            })?;
            // A session already up keeps its device: the material only
            // starts a session, and the VM host hands the app to the one
            // running.
            let (device, secret) = match apps.get(&workspace_id) {
                Some(up) => (up.device.clone(), [0; 32]),
                None => certified(&authority.cert, &authority.key, APPS_DEVICE_LABEL).map_err(
                    |error| {
                        tracing::warn!(%error, "the Linux apps device could not be certified");
                        words("The app couldn't start. Try again in a minute.".to_owned())
                    },
                )?,
            };
            let material = Material {
                workspace: workspace.to_owned(),
                workspace_id,
                record: record.to_owned(),
                device: device.clone(),
                secret,
                server: authority.server.clone(),
                user_public_key: authority.user_public_key,
                account: self.account.clone(),
            };
            (device, material)
        };
        let fresh = !apps.contains_key(&workspace_id);
        if fresh {
            let until_ns = device
                .expires_at_ns
                .unwrap_or_else(|| tilo_clock::unix_now_ns().saturating_add(lifetime_ns()));
            self.confine(
                &device,
                crate::environment::EnvironmentScope::for_resource(
                    workspace_id,
                    linux_apps::APP,
                    record,
                ),
                until_ns,
            )
            .map_err(|error| {
                tracing::warn!(%error, "the Linux app's device could not be confined");
                words("The app couldn't start. Try again in a minute.".to_owned())
            })?;
        }
        let window = match self.sessions.start_app(&material, launch) {
            Ok(window) => window,
            Err(refusal) => {
                if fresh {
                    self.forget(&device);
                }
                return Err(refusal);
            }
        };
        if fresh {
            self.list(workspace_id, &device, true);
            apps.insert(
                workspace_id,
                Open {
                    workspace: workspace.to_owned(),
                    window: window.clone(),
                    record: record.to_owned(),
                    device,
                    callers: std::collections::HashSet::new(),
                },
            );
        }
        Ok(window)
    }

    /// The workspace's browser session stopped: refuse its device on every
    /// protocol, end the session in the VM, forget the device and its window, and
    /// cut every connection the device has here.
    ///
    /// The refusal comes first and is written down, so a stop leaves the
    /// device refused even when what follows fails, and keeps it refused until
    /// the certificate it was given expires.
    pub fn stopped(&self, workspace_id: WorkspaceId) {
        let stopped = self.open.lock_recover().remove(&workspace_id);
        let stopped_apps = self.apps.lock_recover().remove(&workspace_id);
        let devices = {
            let authority = self.authority.lock_recover();
            authority.as_ref().map_or_else(Vec::new, |authority| {
                authority.environments.placed_in(workspace_id)
            })
        };
        self.refuse(
            devices
                .iter()
                .map(|(device, _, until_ns)| (*device, *until_ns)),
        );
        if let Some(stopped) = &stopped {
            self.sessions.stop(&stopped.workspace);
            self.list(workspace_id, &stopped.device, false);
        }
        if let Some(stopped) = &stopped_apps {
            self.sessions.stop_apps(&stopped.workspace);
            self.list(workspace_id, &stopped.device, false);
        }
        // Every connection the devices had here audits on this: the protocols
        // that carried them refuse them, and their sessions end.
        crate::authority_epoch::bump();
    }

    /// One environment's device stopped — its session ended and did not come
    /// back: refuse it on every protocol, forget its window, end what is left of
    /// the session in the VM and delist it. A device placed for the same
    /// workspace since is another environment, and is not this stop's.
    ///
    /// The refusal comes first and is written down, so what follows may fail:
    /// the device stays refused until its certificate expires.
    fn stopped_device(&self, workspace_id: WorkspaceId, device: [u8; 32], until_ns: u64) {
        let stopped = {
            let mut open = self.open.lock_recover();
            match open.get(&workspace_id) {
                Some(up) if up.window.device == device => open.remove(&workspace_id),
                Some(_) | None => None,
            }
        };
        let stopped_apps = {
            let mut apps = self.apps.lock_recover();
            match apps.get(&workspace_id) {
                Some(up) if up.window.device == device => apps.remove(&workspace_id),
                Some(_) | None => None,
            }
        };
        if let Some(stopped) = stopped.as_ref().or(stopped_apps.as_ref()) {
            tracing::info!(
                workspace = %stopped.workspace,
                device = %hex::encode(device),
                "a session in the workspace's VM stopped: its device's last connection here ended {}s ago",
                self.reconnect.as_secs()
            );
        }
        self.refuse(std::iter::once((device, until_ns)));
        if let Some(stopped) = &stopped {
            self.sessions.stop(&stopped.workspace);
            self.list(workspace_id, &stopped.device, false);
        }
        if let Some(stopped) = &stopped_apps {
            self.sessions.stop_apps(&stopped.workspace);
            self.list(workspace_id, &stopped.device, false);
        }
        crate::authority_epoch::bump();
    }

    /// Refuse `devices`, on every protocol, until the certificate each was given
    /// expires. A refusal that cannot be written down is still refused here,
    /// and a restart confines it no wider than its scope.
    fn refuse(&self, devices: impl Iterator<Item = ([u8; 32], u64)>) {
        let authority = self.authority.lock_recover();
        let Some(authority) = authority.as_ref() else {
            return;
        };
        for (device, until_ns) in devices {
            if let Err(error) = authority.environments.stop(device, until_ns) {
                tracing::error!(
                    %error,
                    device = %hex::encode(device),
                    "a stopped browser environment's device is refused here but was not kept"
                );
            }
        }
    }

    /// One of the workspace's environment devices ended its last connection
    /// here and has not come back: its environment stops, and with it the
    /// window that session held.
    ///
    /// A session whose connection dropped is back in a moment and its window
    /// lives on, so the device is given [`RECONNECT`] first, and what decides
    /// is whether a connection of it is here then.
    fn session_ended(self: &Arc<Self>, workspace_id: WorkspaceId, device: [u8; 32]) {
        let (runtime, environments) = {
            let authority = self.authority.lock_recover();
            let Some(authority) = authority.as_ref() else {
                return;
            };
            (authority.runtime.clone(), authority.environments.clone())
        };
        let browsers = Arc::clone(self);
        runtime.spawn(async move {
            tokio::time::sleep(browsers.reconnect).await;
            if environments.connected(&device)
                || crate::environment::environment_of(&environments, &device).is_none()
            {
                return;
            }
            let held = |open: &Mutex<BTreeMap<WorkspaceId, Open>>| {
                open.lock_recover()
                    .get(&workspace_id)
                    .filter(|up| up.window.device == device)
                    .and_then(|up| up.device.expires_at_ns)
            };
            let until_ns = held(&browsers.open)
                .or_else(|| held(&browsers.apps))
                .unwrap_or_else(|| tilo_clock::unix_now_ns().saturating_add(lifetime_ns()));
            // Stopping asks VM hosts and writes workspace membership: never on
            // a runtime thread.
            let _ = tokio::task::spawn_blocking(move || {
                browsers.stopped_device(workspace_id, device, until_ns);
            })
            .await;
        });
    }

    /// Register what a session's device reaches, before it is handed to the
    /// session and listed in its workspace.
    fn confine(
        &self,
        device: &DeviceCert,
        scope: crate::environment::EnvironmentScope,
        until_ns: u64,
    ) -> Result<(), String> {
        let authority = self.authority.lock_recover();
        let Some(authority) = authority.as_ref() else {
            return Err("The workspace isn't ready yet.".to_owned());
        };
        authority
            .environments
            .place_certified(device, scope, until_ns)
    }

    /// Forget a device that never left this host: one whose session could not
    /// be started.
    fn forget(&self, device: &DeviceCert) {
        let authority = self.authority.lock_recover();
        let Some(authority) = authority.as_ref() else {
            return;
        };
        authority.environments.forget(&device.device_public_key);
    }

    /// List (or forget) the session's device in the workspace it serves,
    /// as a member device of the person the workspace belongs to.
    fn list(&self, workspace_id: WorkspaceId, device: &DeviceCert, listed: bool) {
        let authority = self.authority.lock_recover();
        let Some(authority) = authority.as_ref() else {
            return;
        };
        let Some(workspaces) = &authority.workspaces else {
            return;
        };
        let Some(listed) = waited_on_its_own_thread(
            &authority.runtime,
            workspaces.list_device(workspace_id, device.device_public_key, listed),
        ) else {
            return;
        };
        if let Err(error) = listed {
            tracing::warn!(
                %error,
                device = %hex::encode(device.device_public_key),
                "a browser session's device was not listed in its workspace"
            );
        }
    }
}

/// `work`, run to its end on `runtime` from a thread of the runtime's own: a
/// session's device is listed from the blocking thread that placed it, which
/// carries the runtime's context, where waiting on the runtime is refused.
/// `None` when that thread panicked.
fn waited_on_its_own_thread<T: Send>(
    runtime: &tokio::runtime::Handle,
    work: impl Future<Output = T> + Send,
) -> Option<T> {
    std::thread::scope(|scope| scope.spawn(|| runtime.block_on(work)).join().ok())
}

/// [`DEVICE_LIFETIME`] in nanoseconds: how long a session's certificate lasts.
fn lifetime_ns() -> u64 {
    u64::try_from(DEVICE_LIFETIME.as_nanos()).unwrap_or(u64::MAX)
}

/// A fresh device for a session, certified by the host's stand-in as a
/// member device of the workspace's person, for as long as the session
/// may run.
fn certified(
    stand_in: &DeviceCert,
    key: &SigningKey,
    label: &str,
) -> Result<(DeviceCert, [u8; 32]), String> {
    if !stand_in.capabilities.stand_in {
        return Err(
            "this host holds no stand-in certificate for the workspace's person, so it cannot \
             make a browser session a device"
                .to_owned(),
        );
    }
    let mut secret = [0_u8; 32];
    getrandom::fill(&mut secret).map_err(|error| error.to_string())?;
    let device = SigningKey::from_bytes(&secret);
    let now = tilo_clock::unix_now_ns();
    let mut minted = DeviceCert {
        user_public_key: stand_in.user_public_key,
        device_public_key: device.verifying_key().to_bytes(),
        device_label: label.to_owned(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: now,
        expires_at_ns: Some(now.saturating_add(lifetime_ns())),
        signature: [0; 64],
        issuer: Some(Box::new(stand_in.clone())),
    };
    sign_device_cert(&mut minted, key).map_err(|error| error.to_string())?;
    Ok((minted, secret))
}

/// The held request `argument` names (`{"id": "4"}`), as the window publishes
/// each hold's answer action with the id it holds.
#[must_use]
pub fn held_of(argument: &tilo_actions::remote::InvokeArgument) -> Option<u32> {
    let tilo_actions::remote::InvokeArgument::Json(arguments) = argument else {
        return None;
    };
    match arguments.get("id")? {
        serde_json::Value::String(id) => id.parse().ok(),
        serde_json::Value::Number(id) => id.as_u64().and_then(|id| id.try_into().ok()),
        _ => None,
    }
}

/// What the server says of a page action handed to the window with nobody's
/// answer: whether the caller's certificate came with it, and whether that
/// certificate is an agent's that may operate web pages, which is what would
/// have had this host answer for the person instead of asking them.
fn note_unanswered_handover(
    workspace_id: WorkspaceId,
    request: &InvokeRequest,
    certificate: Option<&DeviceCert>,
) {
    let (agent, browser_pages) = certificate.map_or((false, false), |certificate| {
        (
            certificate.capabilities.agent.is_some(),
            certificate.capabilities.browser_pages,
        )
    });
    tracing::info!(
        workspace = %workspace_id,
        action = %request.target.address.action,
        certificate = certificate.is_some(),
        agent,
        browser_pages,
        "a page action is handed to the window with nobody's answer, so its person is asked"
    );
}

/// Whether `certificate` is a key its person let operate web pages in
/// Browser, asking `request`, which is one of the actions that lets it take
/// without asking them ([`OPERATING_ACTIONS`]). The key reaches only the
/// workspaces it was admitted to, so what it may operate is what it is
/// already in.
#[must_use]
pub fn operates(certificate: &DeviceCert, request: &InvokeRequest) -> bool {
    certificate.capabilities.agent.is_some()
        && certificate.capabilities.browser_pages
        && OPERATING_ACTIONS.contains(&request.target.address.action.as_str())
}

/// How long past the person's wait a page submission stays taken: time to
/// hand their answer to the window.
const SUBMISSION_HANDOVER: Duration = Duration::from_secs(60);

/// Why a holder that did not take a page submission asks nothing about it.
pub const ANOTHER_HOLDER_ASKS: &str = "another holder of the workspace is asking the person";

/// What came of taking a page submission ([`HostedBrowsers::submission`]).
enum Taking {
    /// This server's device took it: it asks the person.
    Taken(Submission),
    /// The person already answered it; the answer the workspace kept.
    Answered(Result<ConfirmedInvocation, ActionCallError>),
}

/// The person's answer to the page submission `lease`, as a holder of
/// `workspace` kept it once it was handed over.
async fn kept_answer(
    workspaces: &crate::workspaces::Workspaces,
    workspace: &str,
    lease: &str,
) -> Option<Result<ConfirmedInvocation, ActionCallError>> {
    let done = workspaces.state(workspace).await?.workspace().done(lease)?;
    match serde_json::from_value(done.answer) {
        Ok(answer) => Some(answer),
        Err(error) => {
            tracing::warn!(%lease, %error, "a kept answer is not one this build reads");
            None
        }
    }
}

/// A page submission this server's device took in its workspace's state
/// ([`HostedBrowsers::submission`]).
struct Submission {
    workspaces: crate::workspaces::Workspaces,
    workspace: String,
    lease: String,
}

impl Submission {
    /// `confirmed`, the person's answer, as it may be handed over: only
    /// while this server still holds the submission. A holder that lost it
    /// while the person decided — another one took it over, or won it once
    /// a partition healed — hands over nothing, so the page acts once. The
    /// person's answer, an approval or a denial, is kept in the workspace
    /// before it is handed over, so the same submission arriving later at
    /// any holder is answered with it ([`Taking::Answered`]).
    async fn handing(
        &self,
        confirmed: Result<ConfirmedInvocation, ActionCallError>,
    ) -> Result<ConfirmedInvocation, ActionCallError> {
        let ours = self.workspaces.serving_certificate().device_public_key;
        let held = self
            .workspaces
            .state(&self.workspace)
            .await
            .and_then(|state| {
                state
                    .workspace()
                    .lease(&self.lease, std::time::SystemTime::now())
                    .holder
            })
            .is_some_and(|holder| holder.device == ours);
        if !held {
            return Err(ActionCallError::Unavailable {
                reason: ANOTHER_HOLDER_ASKS.into(),
            });
        }
        // A question nobody answered is no answer: the next arrival asks.
        if matches!(confirmed, Ok(_) | Err(ActionCallError::NotPermitted)) {
            self.keep(&confirmed).await;
        }
        confirmed
    }

    /// Keep `answer` as the person's answer to this submission.
    async fn keep(&self, answer: &Result<ConfirmedInvocation, ActionCallError>) {
        let device = self.workspaces.serving_certificate().clone();
        let kept = match serde_json::to_value(answer) {
            Ok(answer) => self
                .workspaces
                .author_own_workspace_state(&self.workspace, &device, |peer| {
                    peer.workspace_mut().keep_done(
                        &device,
                        &self.lease,
                        answer,
                        std::time::SystemTime::now(),
                    )
                })
                .await
                .map(drop)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error.to_string()),
        };
        if let Err(error) = kept {
            tracing::warn!(lease = %self.lease, %error, "the person's answer was not kept");
        }
    }

    /// Let the submission go once its answer was handed over.
    async fn release(self) {
        let device = self.workspaces.serving_certificate().clone();
        if let Err(error) = self
            .workspaces
            .author_own_workspace_state(&self.workspace, &device, |peer| {
                peer.workspace_mut().release_lease(
                    &device,
                    &self.lease,
                    std::time::SystemTime::now(),
                )
            })
            .await
        {
            tracing::warn!(lease = %self.lease, %error, "a page submission was not let go");
        }
    }
}

/// One page action the person is asked about, and what their answer is
/// turned into for the window that refused it.
struct Asked {
    /// The person asked: their approving devices answer it.
    person: [u8; 32],
    /// The window's device, which their answer names as its target.
    window: [u8; 32],
    /// The caller's device, which it names as the one that asked.
    device: [u8; 32],
    /// The action as the caller asked it: what the question shows, and what
    /// their answer is good for running.
    request: InvokeRequest,
    /// The page departure the question asks about, when there is one: what
    /// leaves a page nobody stands at, and the filled page it leaves from.
    departure: Option<PageDeparture>,
    /// When the question stops waiting for an answer.
    until_ns: u64,
}

impl Asked {
    /// The question this puts to the person, which they answer once.
    fn question(&self, caller: &Caller) -> PendingConfirmation {
        question(
            caller.workspace().unwrap_or_default(),
            caller,
            &self.request,
            self.departure.clone(),
            self.until_ns,
        )
    }

    /// `answer` as this invocation's confirmation, or the refusal it amounts
    /// to: the approval `key` signs, for exactly this action and these
    /// arguments, naming this caller and this window.
    fn confirmed(
        &self,
        answer: &Result<ConfirmationAnswer, String>,
        key: &SigningKey,
    ) -> Result<ConfirmedInvocation, ActionCallError> {
        match answer {
            // The question shows no draft, so nothing in it is changed.
            Ok(ConfirmationAnswer::AllowOnce) => {
                let now = tilo_clock::unix_now_ns();
                let mut approval = HostApproval::unsigned(
                    Parties {
                        person: self.person,
                        host: key.verifying_key().to_bytes(),
                        caller: self.device,
                        target: self.window,
                    },
                    Answered {
                        invocation: *self.request.invocation.0.as_bytes(),
                        provider: self.request.target.provider.clone(),
                        resource: self.request.target.address.resource.clone(),
                        action: self.request.target.address.action.clone(),
                        argument: argument_digest(self.request.argument.to_slot().as_deref()),
                    },
                    (now, now.saturating_add(APPROVAL_LIFETIME)),
                );
                approval
                    .sign(key)
                    .map(|()| {
                        ConfirmedInvocation::unchanged(Confirmation::Approved(approval.encode()))
                    })
                    .map_err(|error| ActionCallError::Unavailable {
                        reason: format!("the host could not answer for the person: {error}"),
                    })
            }
            Ok(
                ConfirmationAnswer::Deny
                | ConfirmationAnswer::Reply(_)
                | ConfirmationAnswer::Secret(_)
                | ConfirmationAnswer::AllowAlways,
            ) => Err(ActionCallError::NotPermitted),
            Err(error) => {
                tracing::warn!(
                    person = %hex::encode(self.person),
                    action = %self.request.target.address.action,
                    %error,
                    "the person was asked about a page action and no answer came"
                );
                Err(ActionCallError::Unavailable {
                    reason: UNANSWERED.to_owned(),
                })
            }
        }
    }
}

/// `invocation` accepted, and to be followed: what a caller is answered with
/// while the person is asked about it.
fn waiting_receipt(invocation: tilo_actions::InvocationId) -> RemoteActionMessage {
    RemoteActionMessage::Receipt(InvocationReceipt {
        invocation,
        status: ReceiptStatus::Accepted,
    })
}

/// [`waiting_receipt`], as where a caller's frame goes.
fn waiting(invocation: tilo_actions::InvocationId) -> crate::app_serving::Routing {
    crate::app_serving::Routing::Answer(Pending::ready(waiting_receipt(invocation)))
}

/// `update` as the answer a caller asking after `invocation` receives.
fn state_message(
    invocation: tilo_actions::InvocationId,
    update: InvocationUpdate,
    correlation: Option<u64>,
) -> RemoteActionMessage {
    RemoteActionMessage::Update {
        invocation,
        state: Some(InvocationState::new(invocation, update)),
        correlation,
    }
}

/// [`state_message`], as where a caller's frame goes.
fn state_routing(
    invocation: tilo_actions::InvocationId,
    update: InvocationUpdate,
    correlation: u64,
) -> crate::app_serving::Routing {
    crate::app_serving::Routing::Answer(Pending::ready(state_message(
        invocation,
        update,
        Some(correlation),
    )))
}

/// `message` — a request of a window's — answered at once with `error`.
fn answered_with(
    message: &RemoteActionMessage,
    error: ActionCallError,
) -> Option<crate::app_serving::Routing> {
    Some(crate::app_serving::Routing::Answer(Pending::ready(
        tilo_actions::remote::refuse(message, error)?,
    )))
}

/// Note that a handed page action is over, as `status` says: what a caller
/// asking after it is answered with, and what the callers waiting on it are
/// answered with now.
fn answered(entry: &mut Handed, status: &ReceiptStatus) {
    over(entry, update_of(entry.request.invocation, status));
}

/// Note that a handed page action is over with `update`, and answer the
/// callers waiting on it.
fn over(entry: &mut Handed, update: InvocationUpdate) {
    entry.stage = Stage::Over(update.clone());
    let state = state_message(entry.request.invocation, update, None);
    for (_, waiting) in entry.waiting.drain(..) {
        waiting.complete(state.clone());
    }
}

/// How an invocation ends, as `status` says: the update its callers read.
fn update_of(invocation: tilo_actions::InvocationId, status: &ReceiptStatus) -> InvocationUpdate {
    match status {
        ReceiptStatus::Accepted => InvocationUpdate::Accepted { invocation },
        ReceiptStatus::Completed { output } => InvocationUpdate::Succeeded {
            invocation,
            output: output.clone(),
        },
        ReceiptStatus::Failed { error } => InvocationUpdate::Failed {
            invocation,
            error: error.clone(),
        },
        ReceiptStatus::Rejected { error } => update_of_error(invocation, error),
        ReceiptStatus::Cancelled { message } => InvocationUpdate::Cancelled {
            invocation,
            message: message.clone(),
        },
    }
}

/// `error` as the failure a caller reads: its typed code and what it says.
fn update_of_error(
    invocation: tilo_actions::InvocationId,
    error: &ActionCallError,
) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation,
        error: ActionFailure {
            code: error.code().to_owned(),
            message: error.to_string(),
            details: None,
            retryable: false,
        },
    }
}

/// The question `request` puts to the person, as their approving devices
/// draw it: the agent that asked, by the name it goes by, the action as the
/// caller described it, the workspace it acts in, and — when what it asks
/// about is a page about to send something (`departure`, which the window
/// stated) — the page it leaves from, what it sends there, and the filled page
/// as a picture they can see.
fn question(
    workspace: &str,
    caller: &tilo_actions::remote::Caller,
    request: &InvokeRequest,
    departure: Option<PageDeparture>,
    until_ns: u64,
) -> PendingConfirmation {
    let mut id = [0_u8; 32];
    let _ = getrandom::fill(&mut id);
    let address = &request.target.address;
    let context = request.context.as_ref();
    let agent_id = caller.acting().agent_id().unwrap_or_default().to_owned();
    // A question about a page is put by the page itself when no agent named
    // itself, and its detail names the site it leaves from rather than the
    // record it is kept under: a person reads the address they are on.
    let about_a_page = departure.is_some();
    let agent_label = caller
        .agent_name()
        .map(str::to_owned)
        .or_else(|| about_a_page.then(|| "This page".to_owned()))
        .unwrap_or_else(|| address.action.clone());
    let resource_label = departure.as_ref().map_or_else(
        || {
            context
                .and_then(|context| context.resource.as_ref())
                .map_or_else(
                    || address.resource.clone(),
                    |resource| resource.label.clone(),
                )
        },
        |page| {
            if page.page.is_empty() {
                page.destination.clone()
            } else {
                page.page.clone()
            }
        },
    );
    PendingConfirmation {
        id,
        agent_id,
        agent_label,
        action_label: departure.as_ref().map_or_else(
            || {
                context
                    .and_then(|context| context.purpose.clone())
                    .unwrap_or_else(|| address.action.clone())
            },
            PageDeparture::doing,
        ),
        resource_label,
        risk: context
            .and_then(|context| {
                context
                    .facets
                    .iter()
                    .find(|facet| facet.key == tilo_actions::RISK_FACET)
            })
            .map_or_else(|| "destructive".to_owned(), |facet| facet.value.clone()),
        // A page about to send something is acting outside Tilo: the sheet
        // says so instead of guessing "it deletes something" from a page it
        // never inspected.
        kind: if about_a_page {
            "outside".to_owned()
        } else {
            String::new()
        },
        workspace: workspace.to_owned(),
        expires_at_ns: until_ns,
        page: departure,
        ..PendingConfirmation::default()
    }
}

/// The page departure `context` reports ([`DEPARTURE_FACET`]), when it reports
/// one: a window that holds a page's own departure states what it is about to
/// send and the picture of the filled page, and the session holding it passes
/// them on with the question it asks.
///
/// [`DEPARTURE_FACET`]: tilo_actions::remote::browser::DEPARTURE_FACET
fn departure_of(context: Option<&tilo_actions::ActionContext>) -> Option<PageDeparture> {
    departure_of_facts(&tilo_actions::remote::browser::departure_facts(context))
}

/// [`departure_of`], from the facts a window stated: the same ones it publishes
/// a hold it started itself with, and the ones it answers a page action's run
/// with when what that reached waits for the person.
fn departure_of_facts(facts: &[(String, String)]) -> Option<PageDeparture> {
    let stated = |name: &str| {
        facts
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .unwrap_or_default()
    };
    let departure = PageDeparture {
        page: stated("page"),
        kind: stated("kind"),
        method: stated("method"),
        destination: stated("destination"),
        picture: Some(stated("picture")).filter(|picture| !picture.is_empty()),
    };
    // A window that reported nothing about its page is no departure: it asks
    // about an action, not about something leaving a page.
    (departure != PageDeparture::default()).then_some(departure)
}

/// [`Reason::NoPaymentMethod`] for an account whose period's free actions are
/// used up, with the one step that would let the next action through.
#[must_use]
pub fn no_payment_method(free: u64) -> Refusal {
    Refusal {
        reason: Reason::NoPaymentMethod,
        message: format!(
            "This period's {free} free Browser actions are used up. A card continues them."
        ),
        next: vec![NextStep {
            kind: NextStepKind::AddPaymentMethod,
            url: None,
        }],
    }
}

/// What a host that places no Browser refuses an open of it with: words that
/// say why, since a Browser window cannot run here.
#[must_use]
pub fn unplaced() -> String {
    "Browser doesn't open here. Open it on your own device instead.".to_owned()
}

/// What a host that places no Linux apps refuses an open of one with.
#[must_use]
pub fn unplaced_linux_apps() -> String {
    "Linux apps don't open here. Open the app on your own device instead.".to_owned()
}

/// What a host knows of one of a caller's invocations: how it follows an open that answers with
/// the run it started rather than with the record the run made.
pub type ObserveInvocation = dyn Fn(&Caller, &tilo_actions::InvocationId) -> Option<tilo_actions::remote::InvocationState>
    + Send
    + Sync;

/// The answer to an open of Browser that ran in `workspace_id`: `message`
/// with the window placed, or the refusal in its place.
///
/// The open answered once its record was made; this places the session on
/// that record, and the open answers once the window is live. An open answered
/// at admission — a run the caller follows — places the window once the run
/// says what record it made, so an agent's open still runs where its record
/// is.
#[must_use]
pub async fn placed(
    browsers: &Arc<HostedBrowsers>,
    workspace_id: WorkspaceId,
    invocation: tilo_actions::InvocationId,
    message: RemoteActionMessage,
    observe: Option<Arc<ObserveInvocation>>,
    caller: &Caller,
) -> RemoteActionMessage {
    let Some(record) = opened_resource(&message) else {
        if accepted(&message) {
            when_record(
                browsers,
                workspace_id,
                invocation,
                observe,
                caller.clone(),
                move |browsers, workspace, record| {
                    let placed = browsers.place(&workspace, workspace_id, &record);
                    match placed {
                        Ok(_) => tracing::info!(
                            %workspace_id,
                            "the workspace's Browser window is up in its VM"
                        ),
                        Err(reason) => tracing::warn!(
                            %workspace_id,
                            %reason,
                            "the workspace's Browser did not open"
                        ),
                    }
                },
            );
            return message;
        }
        // An open answered without naming the record it made asks for no
        // window: nothing would run in the workspace's VM, and the caller
        // would read a record nothing publishes.
        tracing::warn!(
            %workspace_id,
            answer = ?message,
            "an open of Browser answered without the record it made"
        );
        return message;
    };
    place_record(browsers, workspace_id, invocation, &record, message).await
}

/// Whether `message` is the receipt of an invocation that runs on: its record is on its way with
/// the run's result.
fn accepted(message: &RemoteActionMessage) -> bool {
    matches!(
        message,
        RemoteActionMessage::Receipt(tilo_actions::remote::InvocationReceipt {
            status: tilo_actions::remote::ReceiptStatus::Accepted,
            ..
        })
    )
}

/// Follow `invocation`, the open `caller` started, until the run says what record it made, and
/// hand that record to `place` then. The caller's own answer is unchanged: it follows its run as
/// it does any other.
fn when_record(
    browsers: &Arc<HostedBrowsers>,
    workspace_id: WorkspaceId,
    invocation: tilo_actions::InvocationId,
    observe: Option<Arc<ObserveInvocation>>,
    caller: Caller,
    place: impl FnOnce(&Arc<HostedBrowsers>, String, String) + Send + 'static,
) {
    let (Some(observe), Some(runtime)) = (observe, browsers.runtime()) else {
        tracing::warn!(
            %workspace_id,
            "an open answered at admission cannot be followed, so its window was not placed"
        );
        return;
    };
    let browsers = Arc::clone(browsers);
    runtime.spawn(async move {
        let Some(until) = Instant::now().checked_add(FOLLOW_OPEN) else {
            return;
        };
        loop {
            if let Some(state) = observe(&caller, &invocation) {
                match state.update {
                    tilo_actions::outcome::InvocationUpdate::Succeeded { output, .. } => {
                        let record = output
                            .value
                            .as_ref()
                            .and_then(|value| value.get("record"))
                            .and_then(|record| record.as_str())
                            .map(str::to_owned);
                        let Some(record) = record else {
                            tracing::warn!(
                                %workspace_id,
                                "an open finished without the record it made, so its window was \
                                 not placed"
                            );
                            return;
                        };
                        // The host names the workspace once it is bound here, and placing
                        // asks its VM host and waits for the session: the first waits on
                        // the runtime, the second never on one of its threads.
                        let Some(workspace) = browsers.ready_workspace_name(workspace_id).await
                        else {
                            tracing::warn!(
                                %workspace_id,
                                "this host serves no such workspace, so its window was not placed"
                            );
                            return;
                        };
                        let placed = tokio::task::spawn_blocking(move || {
                            place(&browsers, workspace, record);
                        });
                        if let Err(error) = placed.await {
                            tracing::warn!(%error, "placing a window that opened ended");
                        }
                        return;
                    }
                    tilo_actions::outcome::InvocationUpdate::Failed { .. }
                    | tilo_actions::outcome::InvocationUpdate::Cancelled { .. } => return,
                    _ => {}
                }
            }
            if Instant::now() >= until {
                tracing::warn!(
                    %workspace_id,
                    "an open did not settle in time, so its window was not placed"
                );
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
}

/// Place the workspace's Browser on `record`, the record `invocation`'s open made, or answer with
/// the refusal in its place.
async fn place_record(
    browsers: &Arc<HostedBrowsers>,
    workspace_id: WorkspaceId,
    invocation: tilo_actions::InvocationId,
    record: &str,
    message: RemoteActionMessage,
) -> RemoteActionMessage {
    let Some(workspace) = browsers.ready_workspace_name(workspace_id).await else {
        return refused(
            invocation,
            "The workspace isn't ready yet. Open it again in a minute.".to_owned(),
        );
    };
    let placed = Arc::clone(browsers);
    let record = record.to_owned();
    let placement =
        tokio::task::spawn_blocking(move || placed.place(&workspace, workspace_id, &record))
            .await
            .unwrap_or_else(|error| Err(format!("the placement ended: {error}")));
    match placement {
        Ok(_) => message,
        Err(reason) => {
            tracing::warn!(%workspace_id, %reason, "the workspace's Browser did not open");
            refused(invocation, reason)
        }
    }
}

/// How long an open that answers at admission is followed for the record it made: a record an app
/// makes takes a moment, and a run that has not made one in this long is not waited on.
const FOLLOW_OPEN: Duration = Duration::from_secs(5 * 60);

/// The answer to `invocation` refused with `reason`.
#[must_use]
pub fn refused(invocation: tilo_actions::InvocationId, reason: String) -> RemoteActionMessage {
    RemoteActionMessage::Receipt(tilo_actions::remote::InvocationReceipt {
        invocation,
        status: tilo_actions::remote::ReceiptStatus::Rejected {
            error: ActionCallError::Refused { reason },
        },
    })
}

/// The windows this server places, as a turn it carries out itself reaches them.
impl Windows for HostedBrowsers {
    fn window_provider(&self, workspace_id: WorkspaceId) -> Option<String> {
        self.window(workspace_id).map(|window| window.provider)
    }

    fn page_for_turn(
        &self,
        workspace_id: WorkspaceId,
        person: [u8; 32],
        request: InvokeRequest,
    ) -> Ends<'_, Result<InvocationUpdate, ActionCallError>> {
        Box::pin(Self::page_for_turn(self, workspace_id, person, request))
    }

    fn fill_for_turn(
        &self,
        workspace_id: WorkspaceId,
        fill: TurnFill,
        releasing: Releasing,
    ) -> Ends<'_, Result<InvocationUpdate, ActionCallError>> {
        Box::pin(Self::fill_for_turn(self, workspace_id, fill, releasing))
    }

    fn placed(
        self: Arc<Self>,
        workspace_id: WorkspaceId,
        invocation: tilo_actions::InvocationId,
        made: RemoteActionMessage,
        caller: &Caller,
    ) -> Ends<'_, RemoteActionMessage> {
        Box::pin(async move { placed(&self, workspace_id, invocation, made, None, caller).await })
    }
}

/// Whether `request` opens Browser (`app.so.tilo.browser#open`): the open the
/// action service places a session after.
#[must_use]
pub fn opens_browser(request: &InvokeRequest) -> bool {
    opens_browser_at(&request.target)
}

/// The Linux app `request` opens (`app.so.tilo.linux#open`, naming its
/// desktop file in [`linux_apps::APP_ARGUMENT`]), when it opens one: `None`
/// for any other request, and the words to refuse it with when it names no
/// app it could start.
#[must_use]
pub fn opens_linux_app(request: &InvokeRequest) -> Option<Result<AppLaunch, String>> {
    let opens = provider_workspace(&request.target.provider).is_some()
        && request.target.address.resource
            == format!("{APPLICATION_RESOURCE_PREFIX}{}", linux_apps::APP)
        && request.target.address.action == APPLICATION_OPEN_ACTION;
    if !opens {
        return None;
    }
    let named = match &request.argument {
        tilo_actions::remote::InvokeArgument::Json(map) => map
            .get(linux_apps::APP_ARGUMENT)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    };
    Some(AppLaunch::new(&named))
}

/// The answer to an open of a Linux app that ran in `workspace_id`:
/// `message` with the app started in the workspace's VM, or the refusal in
/// its place — the plans' own typed refusal when the account may not run
/// Linux, so whoever opened it reads what would let it through.
#[must_use]
pub async fn placed_linux_app(
    browsers: &Arc<HostedBrowsers>,
    workspace_id: WorkspaceId,
    invocation: tilo_actions::InvocationId,
    launch: &AppLaunch,
    message: RemoteActionMessage,
    observe: Option<Arc<ObserveInvocation>>,
    caller: &Caller,
) -> RemoteActionMessage {
    let Some(record) = opened_resource(&message) else {
        if accepted(&message) {
            let launch = launch.clone();
            when_record(
                browsers,
                workspace_id,
                invocation,
                observe,
                caller.clone(),
                move |browsers, workspace, record| {
                    let placed = browsers.place_app(&workspace, workspace_id, &record, &launch);
                    match placed {
                        Ok(_) => tracing::info!(
                            %workspace_id,
                            app = %launch.desktop_id,
                            "the workspace's Linux app is up in its VM"
                        ),
                        Err(AppRefusal::Refused(refusal)) => tracing::warn!(
                            %workspace_id,
                            reason = %refusal.message,
                            "the workspace's Linux app was refused"
                        ),
                        Err(AppRefusal::Words(reason)) => tracing::warn!(
                            %workspace_id,
                            %reason,
                            "the workspace's Linux app did not open"
                        ),
                    }
                },
            );
        }
        return message;
    };
    let Some(workspace) = browsers.ready_workspace_name(workspace_id).await else {
        return refused(
            invocation,
            "The app couldn't start. Try again in a minute.".to_owned(),
        );
    };
    let placed = Arc::clone(browsers);
    let launch = launch.clone();
    let placement = tokio::task::spawn_blocking(move || {
        placed.place_app(&workspace, workspace_id, &record, &launch)
    })
    .await
    .unwrap_or_else(|error| Err(AppRefusal::Words(format!("the placement ended: {error}"))));
    match placement {
        Ok(_) => message,
        Err(AppRefusal::Refused(refusal)) => refused_by_plan(invocation, &refusal),
        Err(AppRefusal::Words(reason)) => {
            tracing::warn!(%workspace_id, %reason, "the workspace's Linux app did not open");
            refused(
                invocation,
                "The app couldn't start. Try again in a minute.".to_owned(),
            )
        }
    }
}

/// The answer to `invocation` refused by the account's plan: the plans' typed
/// refusal, its words and the next step that lets it through.
#[must_use]
pub fn refused_by_plan(
    invocation: tilo_actions::InvocationId,
    refusal: &Refusal,
) -> RemoteActionMessage {
    RemoteActionMessage::Receipt(tilo_actions::remote::InvocationReceipt {
        invocation,
        status: tilo_actions::remote::ReceiptStatus::Rejected {
            error: ActionCallError::Refused {
                reason: refusal.to_reason(),
            },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::ActionAddress;
    use tilo_actions::ActionOutput;
    use tilo_actions::remote::RemoteActionTarget;
    use tilo_workspace_service::agents::windows::PAGE_RESOURCE;

    fn page(provider: &str, resource: &str) -> RemoteActionTarget {
        RemoteActionTarget {
            provider: provider.to_owned(),
            address: ActionAddress::new(resource, "read-page"),
        }
    }

    tilo_test_env::scenario! {
        fn the_host_recognises_the_page_actions_and_opens_addressed_to_it;

        phase a_windows_page_action_is_the_one_addressed_to_that_window;
        {
            assert!(page_action(
                &page("browser@42", PAGE_RESOURCE),
                "browser@42"
            ));
            assert!(
                !page_action(&page("browser@42", PAGE_RESOURCE), "browser@7"),
                "another window's page is another device's action"
            );
            assert!(
                !page_action(&page("browser@42", "browser.tab"), "browser@42"),
                "the record's own actions are the host's, not the window's"
            );
        }

        phase an_open_of_browser_is_the_one_that_places_a_session;
        {
            use tilo_actions::remote::{APPLICATION_OPEN_ACTION, workspace_provider};
            let open = |resource: &str, action: &str| InvokeRequest {
                invocation: tilo_actions::InvocationId::new(),
                on_behalf_of: None,
                target: RemoteActionTarget {
                    provider: workspace_provider("ws"),
                    address: ActionAddress::new(resource, action),
                },
                argument: tilo_actions::remote::InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            };
            assert!(opens_browser(&open(
                "app.so.tilo.browser",
                APPLICATION_OPEN_ACTION
            )));
            assert!(!opens_browser(&open(
                "app.so.tilo.notes",
                APPLICATION_OPEN_ACTION
            )));
        }

        phase an_open_of_a_linux_app_names_its_desktop_file;
        // An open of the workspace's Linux apps names the app by its desktop
        // file; one that names none, or something that is not one, is refused
        // in words before anything is placed, and an open of another app is
        // not one at all.
        {
            use tilo_actions::remote::{APPLICATION_OPEN_ACTION, workspace_provider};
            let open = |resource: &str, app: Option<&str>| InvokeRequest {
                invocation: tilo_actions::InvocationId::new(),
                on_behalf_of: None,
                target: RemoteActionTarget {
                    provider: workspace_provider("ws"),
                    address: ActionAddress::new(resource, APPLICATION_OPEN_ACTION),
                },
                argument: app.map_or(tilo_actions::remote::InvokeArgument::None, |app| {
                    tilo_actions::remote::InvokeArgument::Json(serde_json::Map::from_iter([(
                        linux_apps::APP_ARGUMENT.to_owned(),
                        app.into(),
                    )]))
                }),
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            };
            assert_eq!(
                opens_linux_app(&open("app.so.tilo.linux", Some("org.gnome.TextEditor"))),
                Some(Ok(AppLaunch::new("org.gnome.TextEditor").unwrap()))
            );
            for named in [None, Some(""), Some("xterm -e sh"), Some("../xterm")] {
                assert!(
                    matches!(
                        opens_linux_app(&open("app.so.tilo.linux", named)),
                        Some(Err(_))
                    ),
                    "{named:?}"
                );
            }
            assert_eq!(
                opens_linux_app(&open("app.so.tilo.browser", Some("xterm"))),
                None
            );
            assert!(!opens_browser(&open("app.so.tilo.linux", Some("xterm"))));
            // phase: a refusal by the plans travels typed, so the caller reads
            // what lets it through
            let refusal = no_payment_method(0);
            let RemoteActionMessage::Receipt(receipt) =
                refused_by_plan(tilo_actions::InvocationId::new(), &refusal)
            else {
                panic!("a refusal is a receipt");
            };
            let tilo_actions::remote::ReceiptStatus::Rejected {
                error: ActionCallError::Refused { reason },
            } = receipt.status
            else {
                panic!("refused");
            };
            assert_eq!(Refusal::from_reason(&reason), Some(refusal));
        }

        phase an_open_that_made_a_record_answers_it_by_its_id;
        {
            let made = RemoteActionMessage::Receipt(tilo_actions::remote::InvocationReceipt {
                invocation: tilo_actions::InvocationId::new(),
                status: tilo_actions::remote::ReceiptStatus::Completed {
                    output: ActionOutput {
                        value: Some(
                            serde_json::json!({"provider": "record:so.tilo.browser:abc", "record": "abc"}),
                        ),
                        resources: Vec::new(),
                    },
                },
            });
            assert_eq!(opened_resource(&made).as_deref(), Some("abc"));
            let refused = refused(tilo_actions::InvocationId::new(), unplaced());
            assert_eq!(opened_resource(&refused), None);
        }

        phase a_host_that_places_nothing_says_why_instead_of_naming_an_unknown_app;
        {
            let words = unplaced();
            assert!(words.contains("Browser doesn't open here"), "{words}");
            assert!(words.contains("your own device"), "{words}");
            assert!(!words.contains("no app"), "{words}");
        }
    }

    #[test]
    fn a_host_serves_browsers_only_when_it_names_where_they_start_where_it_is_reached_and_what_it_counts()
     {
        use crate::browser_actions::BrowserMeter;
        use tilo_server_account::{ACCOUNT_ENV, ACTIONS_LOG_ENV, PLANS_ENV, SOCKET_ENV};

        let mut env = tilo_test_env::ScopedEnv::new();
        env.remove(SOCKET_ENV)
            .remove(ACCOUNT_ENV)
            .remove(ACTIONS_LOG_ENV)
            .remove(PLANS_ENV)
            .remove(SERVER_ENV);
        assert!(
            HostedBrowsers::from_env().unwrap().is_none(),
            "a host that pays for no machines starts no Browser"
        );

        // An account alone is half a host: without a log it would serve
        // Browser free of charge.
        let dir = tempfile::tempdir().unwrap();
        env.set(ACCOUNT_ENV, "acct-1");
        let error = HostedBrowsers::from_env().err().unwrap();
        assert!(error.contains(ACTIONS_LOG_ENV), "{error}");
        assert!(BrowserMeter::from_env().unwrap().is_none());

        // With the log but no address, its sessions would be told to reach
        // it where they cannot.
        env.set(ACTIONS_LOG_ENV, dir.path().join("actions.jsonl"))
            .set(PLANS_ENV, dir.path().join("plans.json"));
        let error = HostedBrowsers::from_env().err().unwrap();
        assert!(error.contains(SERVER_ENV), "{error}");

        // All of them: this host places its workspaces' Browser.
        let key = hex::encode(
            ed25519_dalek::SigningKey::from_bytes(&[7; 32])
                .verifying_key()
                .to_bytes(),
        );
        env.set(SERVER_ENV, format!("{key}@127.0.0.1:7704"));
        assert!(HostedBrowsers::from_env().unwrap().is_some());
    }
}
