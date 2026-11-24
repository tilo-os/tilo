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
//! - **Place** ([`HostedBrowsers::place`]): the workspace's open of Browser
//!   (`app.so.tilo.browser#open`) reaches this host as a provider on its
//!   device's one dispatch ([`HostedBrowsers::provide_in`]); it certifies a
//!   device for the session scoped to the workspace, lists it as a member
//!   device, asks the hosted-VM service to start it
//!   ([`BrowserSessions::start`]) with that material, and remembers the window
//!   it answered. The open answers once the window is live; a second open
//!   attaches to the same session.
//! - **Route** ([`HostedBrowsers::route`]): an action addressed to that
//!   window reaches this host (an agent's calls go to the workspace's
//!   host device) through the same registry, which answers the window's
//!   provider, and is handed to the device holding the window, stamped
//!   with the caller, so it runs there as the caller's: History records the
//!   agent's run for the person it acts for. The server runs none of it. A
//!   listing of the window is handed over the same way, and a listing naming
//!   no provider names the window among its publishers
//!   ([`ActionProvider::query`]), so a caller finds it as it finds any app's
//!   window. What the window answers such a caller reaches it as this host's,
//!   the device it asked ([`HostedBrowsers::answers_for_host`]).
//! - **Ask**: a page action asks the person first, and nobody stands in front
//!   of a window in a VM to ask. The window refuses such an action for want
//!   of their answer, stating what leaves and where
//!   ([`tilo_actions::remote::browser::departure_asked`]); that refusal
//!   travels to whoever carried the call, and the dispatch there keeps it as
//!   the call's own asked invocation record. Their approve admits the same
//!   invocation again, carrying their answer, and this host hands it to the
//!   window ([`Confirmation::Asked`]); their decline ends it, and the page
//!   stays as it was. A departure the page started on its own — a script
//!   posting, a download — is the same call, invoked by the session that runs
//!   the window: nobody stands at the page, so its own session carries it
//!   and the host relays it. A key its person let operate web pages
//!   ([`DeviceCapabilities::browser_pages`]) is not asked about the page
//!   actions that lets it take — [`OPERATING_ACTIONS`], never a submission —
//!   and this host answers for them where their answer would have: the
//!   window runs it once, as theirs, and counts it.
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
use ed25519_dalek::SigningKey;
use iroh::EndpointAddr;
use tilo_actions::remote::asked::{AskedDispatch, AskedRecords, AskedStore, RecordRef};
use tilo_actions::remote::linux_apps::{self, AppLaunch};
use tilo_actions::remote::server_account::{NextStep, NextStepKind, Reason, Refusal};
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionCallError, ActionProvider, Caller,
    CataloguePage, CatalogueProvider, CatalogueQuery, Completer, Confirmation, InvocationReceipt,
    InvocationState, InvokeRequest, Pending, ReceiptStatus, RemoteActionMessage,
    pending, provider_workspace, workspace_provider,
};
use tilo_actions::{
    ActionFailure, ActionKind, ActionSpec, EncodedAction, ExecutionMode, InvocationUpdate,
    OperationKind, ResourceSpec,
};
use tilo_lock::MutexRecover as _;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
use tilo_workspace_service::agents::windows::{
    Ends, Releasing, TurnFill, Windows, opened_resource, opens_browser_at, page_action,
};
use tilo_workspace_service::workspaces::providers::{Providers, Reach};

// A page action handed to a window for a turn this server carries out itself.
mod turn_pages;

/// The app a hosted workspace opens Browser through.
pub use tilo_app_id::apps::BROWSER as APP;
/// What people read Browser's name as.
pub const TITLE: &str = "Browser";

/// The provider a workspace's Browser calls reach on this host: the app this host places, whose
/// `app.so.tilo.browser#open` starts its window here.
pub const PROVIDER: &str = APP;

/// The record a workspace's Browser window opens on, in the workspace's Browser app: one per
/// workspace, so opening Browser again attaches to the window already holding it.
const RECORD: &str = "browser";

/// The label the workspace shows a Linux apps session's device under.
pub const APPS_DEVICE_LABEL: &str = "Linux apps";

/// What this host publishes: Browser's open, the action a workspace starts Browser by
/// (`app.so.tilo.browser#open`), which places its window in the workspace's VM here. A caller
/// naming the address reaches this host whichever provider token it asks under.
fn published() -> Vec<ResourceSpec> {
    let mut spec = ResourceSpec::new(
        format!("{APPLICATION_RESOURCE_PREFIX}{APP}"),
        tilo_actions::remote::APPLICATION_RESOURCE_KIND,
        TITLE,
    );
    spec.app_id = Some(APP.to_owned());
    let type_id = format!("{PROVIDER}/{APPLICATION_OPEN_ACTION}");
    let encoded = type_id.clone();
    let mut action = ActionSpec::typed_operation(
        APPLICATION_OPEN_ACTION,
        format!("Open {TITLE}"),
        "",
        OperationKind::Effect,
        type_id,
        Vec::new(),
        move |_| EncodedAction {
            type_id: encoded.clone(),
            payload: Vec::new(),
        },
    );
    let contract = &mut action.type_mut().contract;
    contract.kind = ActionKind::Outside;
    // Starting the session takes as long as its VM takes to wake: the call is answered once the
    // window is up, and followed meanwhile as any background action is.
    contract.execution = ExecutionMode::Background { cancellable: false };
    spec.actions.push(action);
    vec![spec]
}

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
/// The waits before a host asks again for a workspace it could not name yet
/// ([`HostedBrowsers::ready_workspace_name`]).
const NOT_READY_WAITS: [Duration; 4] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
];

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

pub use crate::browser_sessions::{AppRefusal, BrowserSessions, Sessions, Window};

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
    /// It waits for its person, as its asked record: their approve admits the
    /// same call again, and their decline ends it.
    Asked(RecordRef),
    /// It is over: the update a caller asking after it is answered with.
    Over(InvocationUpdate),
}

impl Stage {
    /// The state an invocation in this stage is answered with when a caller
    /// asks how it went.
    fn state(&self, invocation: tilo_actions::InvocationId) -> InvocationUpdate {
        match self {
            Self::Running => InvocationUpdate::Accepted { invocation },
            Self::Asked(record) => record.waiting(invocation),
            Self::Over(update) => update.clone(),
        }
    }
}

/// The name this host's dispatch keeps the asked records it writes by, across its restarts.
const ASKED_BY: &str = "browser";

/// The dispatch a window's page action comes back to, when it waits for its person: this
/// host, referred to weakly so the store keeps nothing of it alive.
struct BrowsersAsks(std::sync::Weak<HostedBrowsers>);

impl AskedDispatch for BrowsersAsks {
    fn context(&self, invocation: tilo_actions::InvocationId) -> serde_json::Value {
        self.0
            .upgrade()
            .map_or(serde_json::Value::Null, |browsers| {
                browsers.ask_context(invocation)
            })
    }

    fn readmit(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        context: &serde_json::Value,
    ) -> Result<(), ActionCallError> {
        self.0
            .upgrade()
            .ok_or_else(not_waiting)?
            .readmit(caller, request, context)
    }

    fn declined(
        &self,
        _caller: &Caller,
        invocation: tilo_actions::InvocationId,
    ) -> Result<(), ActionCallError> {
        self.0
            .upgrade()
            .ok_or_else(not_waiting)?
            .declined_by_person(invocation)
    }
}

/// What a host that no longer holds the call answers: it waits no more.
fn not_waiting() -> ActionCallError {
    ActionCallError::Refused {
        reason: "the call no longer waits for this answer".to_owned(),
    }
}

/// How many handed page actions are remembered while their windows answer:
/// beyond it, the oldest are forgotten and only go uncounted.
const HANDED: usize = 8192;

/// The Browser this host places for its workspaces, and the page actions it
/// hands to the windows.
pub struct HostedBrowsers {
    sessions: Arc<dyn BrowserSessions>,
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
    /// invocation: what a count is owed for once it ran.
    handed: Arc<Mutex<HashMap<tilo_actions::InvocationId, Handed>>>,
    /// Where a page action that waits for its person is kept, once the host named the store
    /// ([`Self::keep_asked_in`]): the call's own asked record, answered with
    /// `<record>#resolve`.
    asked: Mutex<Option<Arc<dyn AskedRecords>>>,
    /// The dispatch a person's answer to one of those records comes back to, kept while the
    /// host runs.
    asked_dispatch: Mutex<Option<Arc<dyn AskedDispatch>>>,
    /// The page actions this server handed to windows for the turns it carries out itself
    /// ([`turn_pages`]).
    turn_pages: turn_pages::TurnPages,
    /// The number naming the next caller waiting for an invocation to move.
    next_waiter: AtomicU64,
    /// How long an environment device that disconnected has to come back
    /// before its environment stops ([`RECONNECT`]).
    reconnect: Duration,
    /// The fills of a page's field this server handed to windows for the turns it carries
    /// out itself ([`turn_fills`]).
    turn_fills: turn_fills::TurnFills,
    /// What this host publishes at fixed addresses: Browser's open, which places its window here.
    resources: Vec<ResourceSpec>,
    /// Where this host answers as a provider, once its device registers it
    /// ([`Self::provide_in`]): the registry that admits its calls, what it registered, and itself,
    /// weakly, for what it follows on the runtime.
    publishing: Mutex<Option<RegisteredAs>>,
}

/// Where a host that places Browser answers as a provider: the registry its calls are admitted
/// on, what it registered there, and itself, all by weak reference, as each of them refers to
/// this host.
#[derive(Clone)]
struct RegisteredAs {
    providers: tilo_workspace_service::workspaces::providers::WeakProviders,
    answers: std::sync::Weak<dyn ActionProvider + Send + Sync>,
    this: std::sync::Weak<HostedBrowsers>,
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "each method holds one guard for one answer; placing holds it so two opens make one session"
)]
impl HostedBrowsers {
    /// A host that places Browser through `sessions`, counting page
    /// actions of `account` in `meter`.
    pub fn new(
        sessions: Arc<dyn BrowserSessions>,
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
            asked: Mutex::default(),
            asked_dispatch: Mutex::default(),
            turn_pages: turn_pages::TurnPages::default(),
            next_waiter: AtomicU64::new(0),
            reconnect: RECONNECT,
            turn_fills: turn_fills::TurnFills::default(),
            resources: published(),
            publishing: Mutex::default(),
        }
    }

    /// Wait `reconnect` for a disconnected environment device to come back
    /// before its environment stops ([`RECONNECT`]).
    #[must_use]
    pub const fn with_reconnect(mut self, reconnect: Duration) -> Self {
        self.reconnect = reconnect;
        self
    }

    /// The Browser this host serves through `sessions`, when the daemon hands it any (the
    /// workspaces' devices that run Linux, on a host that pays for their machines): the log
    /// their page actions are counted in ([`crate::browser_actions::BrowserMeter`]) and the
    /// address the sessions reach this host at come from its environment.
    ///
    /// # Errors
    ///
    /// Sessions are handed but their page actions cannot be counted, or the address they
    /// reach this host at is not named: a host that runs browsers without counting them would
    /// serve them free of charge, so the start fails rather than run half of it.
    pub fn serving(sessions: Option<Sessions>) -> Result<Option<Self>, String> {
        let Some(Sessions { sessions, account }) = sessions else {
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
        Ok(Some(Self::new(sessions, Box::new(meter), account)))
    }

    /// Answer this device's workspaces' calls as the provider this host is: `app.so.tilo.browser#open`
    /// places its window here, and the page actions of a window it placed reach it as every other
    /// action reaches its handler, admitted the same way. A host that places no window registers
    /// nothing, so an open is answered by its device's own refusal.
    pub fn provide_in(self: &Arc<Self>, providers: &Providers) {
        let answers: Arc<dyn ActionProvider + Send + Sync> = Arc::<Self>::clone(self);
        providers.provide(PROVIDER, APP, Reach::Devices, Arc::clone(&answers));
        *self.publishing.lock_recover() = Some(RegisteredAs {
            providers: providers.downgrade(),
            answers: Arc::downgrade(&answers),
            this: Arc::downgrade(self),
        });
    }

    /// This host as the registry holds it, for what it follows on the runtime.
    fn me(&self) -> Option<Arc<Self>> {
        self.publishing.lock_recover().as_ref()?.this.upgrade()
    }

    /// Answer the page actions of the window published under `provider`: the provider is named by
    /// the window's own device, so a caller naming it reaches this host, which hands the action to
    /// the device.
    fn publish_window(&self, provider: &str) {
        let Some(RegisteredAs {
            providers, answers, ..
        }) = self.publishing.lock_recover().clone()
        else {
            return;
        };
        let (Some(providers), Some(answers)) = (providers.upgrade(), answers.upgrade()) else {
            return;
        };
        if let Err(error) = providers.serve(provider, APP, Reach::Devices, answers) {
            tracing::warn!(%error, "a Browser window's provider is served on this device already");
        }
    }

    /// Answer no more calls under `provider`: the window it named stopped.
    fn unpublish_window(&self, provider: &str) {
        let Some(RegisteredAs {
            providers, answers, ..
        }) = self.publishing.lock_recover().clone()
        else {
            return;
        };
        let (Some(providers), Some(answers)) = (providers.upgrade(), answers.upgrade()) else {
            return;
        };
        providers.withdraw(provider, &answers);
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
        let (provider, window, window_device) = {
            let open = self.open.lock_recover();
            let up = open.get(&workspace_id)?;
            (
                up.window.provider.clone(),
                up.window.device,
                up.device.device_public_key,
            )
        };
        if !page_action(&request.target, &provider) {
            return None;
        }
        let device = *caller.device_public_key()?;
        // The request a window holds is its own session's to release or drop: another device
        // answering for its page would be answered as though it were the window.
        if matches!(
            request.target.address.action.as_str(),
            tilo_actions::remote::browser::SEND_HELD | tilo_actions::remote::browser::KEEP_HELD
        ) && device != window_device
        {
            return answered_with(message, ActionCallError::NotPermitted);
        }
        let permitted = match &request.confirmation {
            None => true,
            // The person approved the asked record this call was kept as: the
            // dispatch admitted the same call again carrying their answer, and
            // the window runs it with it. Admission drops an answer from
            // anything a caller sent, so what reaches here is the dispatch's.
            Some(Confirmation::Asked(_)) => true,
            Some(_) => false,
        };
        if !permitted {
            return answered_with(message, ActionCallError::NotPermitted);
        }
        // A key whose person let it operate web pages needs no question about
        // one: this host answers for them, handing the key's own call to the
        // window it placed with the confirmation their answer would have
        // made. The window takes that word from this host's link alone
        // ([`tilo_actions::remote::browser::host_answered`]), so it runs the call
        // once and answers the caller itself.
        let operated = (request.confirmation.is_none()
            && certificate.is_some_and(|certificate| operates(certificate, request)))
        .then(|| InvokeRequest {
            confirmation: Some(Confirmation::Confirmed),
            ..request.clone()
        });
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

    /// Keep every page action a window of this host holds for its person as its own asked
    /// record in `store`, answered through this host: their approve hands the same call to the
    /// window again carrying their answer, and their decline keeps the page as it was.
    pub fn keep_asked_in(self: &Arc<Self>, store: &Arc<AskedStore>) {
        let dispatch: Arc<dyn AskedDispatch> = Arc::new(BrowsersAsks(Arc::downgrade(self)));
        let records = store.for_dispatch(ASKED_BY, Arc::downgrade(&dispatch));
        *self.asked.lock_recover() = Some(records);
        *self.asked_dispatch.lock_recover() = Some(dispatch);
    }

    /// What admitting an asked call again needs, kept with its record: the workspace whose
    /// window holds it.
    fn ask_context(&self, invocation: tilo_actions::InvocationId) -> serde_json::Value {
        let handed = self.handed.lock_recover();
        handed.get(&invocation).map_or(
            serde_json::Value::Null,
            |entry| serde_json::json!({ "workspace": entry.workspace }),
        )
    }

    /// Admit the call `caller` asked for again on their approve of its record, carrying their
    /// answer: the window it was handed to runs it once, as it runs a page action the host
    /// answered for the person.
    fn readmit(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        context: &serde_json::Value,
    ) -> Result<(), ActionCallError> {
        let workspace_id = context
            .get("workspace")
            .and_then(serde_json::Value::as_str)
            .and_then(WorkspaceId::parse)
            .ok_or_else(not_waiting)?;
        let device = *caller
            .device_public_key()
            .ok_or(ActionCallError::NotPermitted)?;
        let window = self
            .open
            .lock_recover()
            .get(&workspace_id)
            .map(|up| up.window.device)
            .ok_or_else(not_waiting)?;
        let hand = self.hand().ok_or_else(|| ActionCallError::Unavailable {
            reason: "this host reaches no window".to_owned(),
        })?;
        {
            let mut handed = self.handed.lock_recover();
            let Some(entry) = handed.get_mut(&request.invocation) else {
                return Err(not_waiting());
            };
            if entry.window != window {
                return Err(not_waiting());
            }
            entry.request = request.clone();
            entry.stage = Stage::Running;
            let accepted = InvocationUpdate::Accepted {
                invocation: request.invocation,
            };
            for (_, waiter) in entry.waiting.drain(..) {
                waiter.complete(state_message(request.invocation, accepted.clone(), None));
            }
        }
        // The window it is handed to runs it once, counted as any page action is; so that its
        // answer reaches this caller, this host follows the answer and answers the invocation
        // with it (nothing on a connection asked for this run).
        let browsers = self.me().ok_or_else(not_waiting)?;
        let runtime = self.runtime().ok_or_else(not_waiting)?;
        let answer = hand.frame(
            workspace_id,
            (device, caller.user_public_key().copied().unwrap_or([0; 32])),
            window,
            RemoteActionMessage::Invoke(request),
        );
        runtime.spawn(async move {
            if let Some(Some(reply)) = answer.await {
                let _ = browsers.relayed(workspace_id, window, &reply);
            }
        });
        Ok(())
    }

    /// End `invocation`, which waits in its asked record, as declined by the person: the page
    /// stays as it was, and a caller following it reads that.
    fn declined_by_person(
        &self,
        invocation: tilo_actions::InvocationId,
    ) -> Result<(), ActionCallError> {
        let mut handed = self.handed.lock_recover();
        let Some(entry) = handed.get_mut(&invocation) else {
            return Err(not_waiting());
        };
        let update = InvocationUpdate::Cancelled {
            invocation,
            message: Some(tilo_actions::remote::asked::DECLINED.to_owned()),
        };
        over(entry, update);
        Ok(())
    }

    /// The window's answer to a page action this host handed it: it ran
    /// (`Completed`), so it is counted — once, when it ran — and its caller is
    /// answered with the page's receipt; the window asks the person first
    /// (`ConfirmationRequired`), stating what leaves and where, and that
    /// refusal travels to the caller as it is: whoever carried the call keeps
    /// it as its asked record, and the person's approve admits it again.
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
        let known = handed.get(&receipt.invocation)?;
        if known.window != from {
            return None;
        }
        match &receipt.status {
            ReceiptStatus::Completed { .. } => {
                // The action ran where the window says it did, and nowhere
                // else: a count is owed once. Releasing or dropping a request the
                // page already holds is no page action of the person's allowance.
                let entry = handed.get_mut(&receipt.invocation)?;
                let ran = matches!(entry.stage, Stage::Running);
                let counted = !matches!(
                    entry.request.target.address.action.as_str(),
                    tilo_actions::remote::browser::SEND_HELD
                        | tilo_actions::remote::browser::KEEP_HELD
                );
                let workspace = entry.workspace.clone();
                answered(entry, &receipt.status);
                drop(handed);
                if ran && counted {
                    self.counted(&workspace);
                }
                None
            }
            // It waits for the person: the call is kept as its own asked record, which they
            // answer with `<record>#resolve`. Their approve admits the same call again, and the
            // window's own answer to that run is counted like any other.
            ReceiptStatus::Rejected {
                error: ActionCallError::ConfirmationRequired(facts),
            } => {
                let entry = handed.get(&receipt.invocation)?;
                let (workspace, caller, request) = (
                    entry.workspace.clone(),
                    entry.caller.clone(),
                    entry.request.clone(),
                );
                let records = self.asked.lock_recover().clone();
                drop(handed);
                let Some(records) = records else {
                    // No store here keeps it: the caller is answered the refusal it always was.
                    self.handed.lock_recover().remove(&receipt.invocation);
                    return None;
                };
                let Ok(record) = records.write_asked(&workspace, &caller, &request, &facts) else {
                    self.handed.lock_recover().remove(&receipt.invocation);
                    return None;
                };
                let mut handed = self.handed.lock_recover();
                let entry = handed.get_mut(&receipt.invocation)?;
                entry.stage = Stage::Asked(record.clone());
                let waiting =
                    InvocationState::new(receipt.invocation, record.waiting(receipt.invocation));
                for (_, waiter) in entry.waiting.drain(..) {
                    waiter.complete(state_message(
                        receipt.invocation,
                        waiting.update.clone(),
                        None,
                    ));
                }
                Some(RemoteActionMessage::Receipt(waiting.receipt()))
            }
            // Running, or to be followed: the answer is still to come, and
            // what it settles is counted then.
            ReceiptStatus::Accepted => None,
            ReceiptStatus::Rejected { .. }
            | ReceiptStatus::Failed { .. }
            | ReceiptStatus::Cancelled { .. } => {
                let entry = handed.get_mut(&receipt.invocation)?;
                answered(entry, &receipt.status);
                None
            }
        }
    }

    /// Count one page action a window of `workspace` says it carried out.
    fn counted(&self, workspace: &str) {
        self.meter.record(workspace, &self.account);
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
            // It waits for its person: the state it is answered with, and a wait for it.
            Stage::Asked(_) => {}
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
            Stage::Running => {
                return Some(crate::app_serving::Routing::Device(entry.window));
            }
            Stage::Over(_) => Err(ActionCallError::NotCancellable {
                reason: "it is over".to_owned(),
            }),
            // The person answers it, in its record: the call has not reached the window.
            Stage::Asked(_) => Err(ActionCallError::NotCancellable {
                reason: "it waits for its person's answer".to_owned(),
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

    /// Where a frame this host hands over after its own answer goes: the
    /// server's own delivery to the device holding the window.
    fn hand(&self) -> Option<crate::app_serving::Hand> {
        let authority = self.authority.lock_recover();
        Some(authority.as_ref()?.hand.clone())
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
        // The workspace's roster has the device before the session dials
        // with it, so its first link is admitted.
        self.list(workspace_id, &device, true);
        let window = match self.sessions.start(&material) {
            Ok(window) => window,
            Err(error) => {
                // The device never left this host: it is forgotten rather
                // than left a member of the workspace.
                self.forget(&device);
                self.list(workspace_id, &device, false);
                return Err(error);
            }
        };
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
        // A page action names the window's own provider: from now on this host answers it, and
        // hands the action to the device holding the window.
        self.publish_window(&window.provider);
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
            self.list(workspace_id, &device, true);
        }
        let window = match self.sessions.start_app(&material, launch) {
            Ok(window) => window,
            Err(refusal) => {
                if fresh {
                    self.forget(&device);
                    self.list(workspace_id, &device, false);
                }
                return Err(refusal);
            }
        };
        if fresh {
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
        if let Some(stopped) = &stopped {
            // The window's provider is gone with it: a call naming it reaches no page action now.
            self.unpublish_window(&stopped.window.provider);
        }
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
        if let Some(stopped) = &stopped {
            self.unpublish_window(&stopped.window.provider);
        }
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

    /// Register what a session's device reaches, before it is listed in its
    /// workspace and handed to the session.
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
            workspaces.list_device(workspace_id, device, listed),
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

/// What this host answers before it runs: a window can be neither placed nor reached yet.
fn still_starting() -> ActionCallError {
    ActionCallError::Unavailable {
        reason: "Browser is still starting. Try again in a minute.".to_owned(),
    }
}

/// `message`, a window's answer to a call, as the call's receipt.
fn receipt_answer(
    message: Option<RemoteActionMessage>,
) -> Result<InvocationReceipt, ActionCallError> {
    match message {
        Some(RemoteActionMessage::Receipt(receipt)) => Ok(receipt),
        Some(_) | None => Err(ActionCallError::Unavailable {
            reason: "the Browser window did not answer the call".to_owned(),
        }),
    }
}

/// Whether `request` opens Browser here: `app.so.tilo.browser#open` addressed to the workspace it
/// opens in, or to the app in that workspace.
fn opens_browser_here(request: &InvokeRequest) -> bool {
    opens_browser_at(&request.target)
        || (request.target.address.resource == format!("{APPLICATION_RESOURCE_PREFIX}{APP}")
            && request.target.address.action == APPLICATION_OPEN_ACTION
            && tilo_actions::remote::application_provider(&request.target.provider).is_some())
}

/// The workspace an open of Browser names: the one its provider token names, whichever token it
/// is addressed under.
fn opened_workspace(target: &tilo_actions::remote::RemoteActionTarget) -> Option<&str> {
    provider_workspace(&target.provider).or_else(|| {
        tilo_actions::remote::application_provider(&target.provider).map(|(workspace, _)| workspace)
    })
}

impl HostedBrowsers {
    /// `request`, which this host's device admitted for `caller`: an open of Browser places the
    /// workspace's window here, and a page action of a window it placed is handed to the device
    /// holding that window and answered with what it said.
    async fn carry_out(
        self: &Arc<Self>,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Result<InvocationReceipt, ActionCallError> {
        if opens_browser_here(&request) {
            return self.open_browser(caller, request).await;
        }
        let Some(workspace_id) = caller.workspace().and_then(WorkspaceId::parse) else {
            return Err(ActionCallError::UnknownAction);
        };
        let Some(window) = self.window(workspace_id) else {
            return Err(ActionCallError::UnknownAction);
        };
        if !page_action(&request.target, &window.provider) {
            return Err(ActionCallError::UnknownAction);
        }
        self.carry_page_action(workspace_id, caller, request).await
    }

    /// `request`, an open of Browser in the workspace its provider token names: place the
    /// workspace's window on its record here, and answer the record it was opened on.
    async fn open_browser(
        self: &Arc<Self>,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Result<InvocationReceipt, ActionCallError> {
        let Some(workspace) = opened_workspace(&request.target) else {
            return Err(ActionCallError::UnknownAction);
        };
        let Some(workspace_id) = WorkspaceId::parse(workspace) else {
            return Err(ActionCallError::UnknownAction);
        };
        // A window is placed in the workspace the open names, and nowhere else: an open naming
        // one this caller is not in is answered as one naming nothing.
        if caller.workspace() != Some(workspace) {
            return Err(ActionCallError::UnknownAction);
        }
        let Some(name) = self.ready_workspace_name(workspace_id).await else {
            return Err(ActionCallError::Refused {
                reason: "The workspace isn't ready yet. Open it again in a minute.".to_owned(),
            });
        };
        let placed = Arc::clone(self);
        let record = RECORD.to_owned();
        let opening = record.clone();
        let placement =
            tokio::task::spawn_blocking(move || placed.place(&name, workspace_id, &opening))
                .await
                .unwrap_or_else(|error| Err(format!("the placement ended: {error}")));
        match placement {
            Ok(_) => Ok(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput {
                        value: Some(serde_json::json!({ "record": record })),
                        ..tilo_actions::ActionOutput::default()
                    },
                },
            }),
            Err(reason) => {
                tracing::warn!(%workspace_id, %reason, "the workspace's Browser did not open");
                Err(ActionCallError::Refused { reason })
            }
        }
    }

    /// A page action of the window up in `workspace_id`, as `caller` asked it: routed as the host
    /// routes one — counted, answered for the person where their key may operate the page, or
    /// kept as the call's asked record — and handed to the device holding the window.
    async fn carry_page_action(
        self: &Arc<Self>,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Result<InvocationReceipt, ActionCallError> {
        let device = *caller
            .device_public_key()
            .ok_or(ActionCallError::NotPermitted)?;
        let certificate = self.certificate(workspace_id, device).await;
        let message = RemoteActionMessage::Invoke(request.clone());
        let routed = match &certificate {
            Some(certificate) => self.route_certified(workspace_id, caller, certificate, &message),
            None => self.route(workspace_id, caller, &message),
        };
        let Some(routed) = routed else {
            return Err(ActionCallError::UnknownAction);
        };
        let (window, message) = match routed {
            // The host answers this one itself: its typed refusal, or how an invocation it holds
            // went.
            crate::app_serving::Routing::Answer(answer) => return receipt_answer(answer.await),
            crate::app_serving::Routing::Device(window) => (window, message),
            crate::app_serving::Routing::Replaced { to, message } => (to, *message),
        };
        let Some(hand) = self.hand() else {
            return Err(still_starting());
        };
        let from = (device, caller.user_public_key().copied().unwrap_or([0; 32]));
        let Some(Some(answer)) = hand.frame(workspace_id, from, window, message).await else {
            return Err(ActionCallError::Unavailable {
                reason: "this host reaches no Browser window".to_owned(),
            });
        };
        // What the window answered is read as the host reads it — counted once it ran, and kept
        // as the call's asked record where it waits for the person — and answered to the caller.
        let answered = self
            .relayed(workspace_id, window, &answer)
            .unwrap_or(answer);
        receipt_answer(Some(answered))
    }

    /// The certificate this host holds for `device` in `workspace_id`, when it holds one: what a
    /// caller's page actions are decided by beyond who it is.
    async fn certificate(&self, workspace_id: WorkspaceId, device: [u8; 32]) -> Option<DeviceCert> {
        self.workspaces()?
            .state(&workspace_id.to_string())
            .await?
            .certificates()
            .into_iter()
            .find(|certificate| certificate.device_public_key == device)
    }
}

/// The provider this host is on its device's dispatch: the open of Browser it places, and the page
/// actions of the windows it placed.
impl ActionProvider for HostedBrowsers {
    /// The app this host places, and the provider each window it placed publishes under.
    fn serves(&self, provider: &str) -> bool {
        provider == PROVIDER
            || self
                .open
                .lock_recover()
                .values()
                .any(|up| up.window.provider == provider)
    }

    fn published(&self) -> Vec<ResourceSpec> {
        self.resources.clone()
    }

    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let Some(workspace_id) = caller.workspace().and_then(WorkspaceId::parse) else {
            return Pending::ready(Ok(CataloguePage::default()));
        };
        if query.provider.as_deref() == Some(PROVIDER) {
            return Pending::ready(Ok(CataloguePage::published(
                PROVIDER,
                &self.resources,
                &query,
            )));
        }
        if let Some(provider) = query.provider.as_deref() {
            // What a window offers is read from the window: handed to the device holding it, and
            // answered there as the caller's.
            let Some(window) = self.window(workspace_id) else {
                return Pending::ready(Err(ActionCallError::UnknownAction));
            };
            if window.provider != provider {
                return Pending::ready(Err(ActionCallError::UnknownAction));
            }
            let Some(device) = caller.device_public_key().copied() else {
                return Pending::ready(Err(ActionCallError::NotPermitted));
            };
            let (Some(hand), Some(runtime)) = (self.hand(), self.runtime()) else {
                return Pending::ready(Err(still_starting()));
            };
            if let Some(up) = self.open.lock_recover().get_mut(&workspace_id) {
                up.answers(device);
            }
            let from = (device, caller.user_public_key().copied().unwrap_or([0; 32]));
            let message = RemoteActionMessage::CatalogueRequest {
                correlation: 0,
                query: query.clone(),
            };
            let (completer, answer) = pending();
            runtime.spawn(async move {
                completer.complete(
                    match hand.frame(workspace_id, from, window.device, message).await {
                        Some(Some(RemoteActionMessage::CatalogueResponse { result, .. })) => result,
                        _ => Err(ActionCallError::Unavailable {
                            reason: "the Browser window did not answer its listing".to_owned(),
                        }),
                    },
                );
            });
            return answer;
        }
        // A listing naming no provider: the workspace's own open of Browser, which this host
        // answers, and the window it placed while one is up, beside it.
        let mut page = CataloguePage::default();
        page.providers.push(CatalogueProvider {
            provider: workspace_provider(&workspace_id.to_string()),
            app_id: String::new(),
            workspace: String::new(),
            document_id: String::new(),
            browsable: false,
        });
        if let Some(up) = self.open.lock_recover().get(&workspace_id) {
            page.providers.push(CatalogueProvider {
                provider: up.window.provider.clone(),
                app_id: APP.to_owned(),
                workspace: up.workspace.clone(),
                document_id: up.record.clone(),
                browsable: true,
            });
        }
        Pending::ready(Ok(page))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let (Some(browsers), Some(runtime)) = (self.me(), self.runtime()) else {
            return Pending::ready(Err(still_starting()));
        };
        let caller = caller.clone();
        let (completer, answer) = pending();
        runtime.spawn(async move {
            completer.complete(browsers.carry_out(&caller, request).await);
        });
        answer
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        let device = *caller.device_public_key()?;
        let handed = self.handed.lock_recover();
        let entry = handed.get(id)?;
        (entry.caller.device_public_key() == Some(&device))
            .then(|| InvocationState::new(*id, entry.stage.state(*id)))
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        // The window runs it where it is: there is nothing here to stop.
        Pending::ready(Err(ActionCallError::NotCancellable {
            reason: "the Browser window holds it".to_owned(),
        }))
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
    tilo_workspace_service::agents::windows::UNPLACED.to_owned()
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
    fn a_host_serves_browsers_only_when_it_is_handed_sessions_is_reached_and_counts_them() {
        use crate::browser_actions::BrowserMeter;
        use tilo_server_account::{ACTIONS_LOG_ENV, PLANS_ENV};

        struct NoMachines;
        impl BrowserSessions for NoMachines {
            fn start(&self, _: &Material) -> Result<Window, String> {
                Err("no machines".into())
            }
            fn stop(&self, _: &str) {}
        }
        let sessions = || {
            Some(Sessions {
                sessions: Arc::new(NoMachines),
                account: "acct-1".into(),
            })
        };

        let mut env = tilo_test_env::ScopedEnv::new();
        env.remove(ACTIONS_LOG_ENV)
            .remove(PLANS_ENV)
            .remove(SERVER_ENV);
        assert!(
            HostedBrowsers::serving(None).unwrap().is_none(),
            "a host the daemon hands no sessions starts no Browser"
        );

        // Sessions without a log would be served free of charge.
        let dir = tempfile::tempdir().unwrap();
        let error = HostedBrowsers::serving(sessions()).err().unwrap();
        assert!(error.contains(ACTIONS_LOG_ENV), "{error}");
        assert!(BrowserMeter::from_env().unwrap().is_none());

        // With the log but no address, its sessions would be told to reach
        // it where they cannot.
        env.set(ACTIONS_LOG_ENV, dir.path().join("actions.jsonl"))
            .set(PLANS_ENV, dir.path().join("plans.json"));
        let error = HostedBrowsers::serving(sessions()).err().unwrap();
        assert!(error.contains(SERVER_ENV), "{error}");

        // All of them: this host places its workspaces' Browser.
        let key = hex::encode(
            ed25519_dalek::SigningKey::from_bytes(&[7; 32])
                .verifying_key()
                .to_bytes(),
        );
        env.set(SERVER_ENV, format!("{key}@127.0.0.1:7704"));
        assert!(HostedBrowsers::serving(sessions()).unwrap().is_some());
    }
}
