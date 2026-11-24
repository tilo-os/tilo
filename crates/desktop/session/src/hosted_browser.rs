//! A session in a workspace's VM: the device, the workspace and the window its host handed
//! it (`docs/hosted-browser.md`, "What the session in the VM is handed").
//!
//! The session in a workspace's VM is started at the first browser action, so it
//! holds nothing of its own: the server placement record on its standard input is the workspace it
//! joins, the record whose Browser window it opens, and the device certificate and key it joins
//! as. It is a device of the workspace like any other — the server lists it, and its window is
//! published, subscribed and acted on as a window on anyone's computer is — and it does nothing
//! else a person's session does on start: no onboarding, no Lounge, no other app.
//!
//! The session reports what it became on its standard output, one [`Report`] per line: [`READY`]
//! once the identity it was handed is the one it runs as, [`UP`] with the window's provider once
//! the window publishes, and [`USED`] whenever the page is used. The host that started it counts
//! its idle limit from that, and hands the provider to the server, which routes page actions to it.
//!
//! The window also holds departures nobody asked for — a page posting on its own, a download it
//! started — and nobody stands in front of it to allow them. The session reports each one the
//! window publishes ([`SEND_HELD`], by the id it is published with) to the server's device, which
//! puts it to the person and answers with their word: their allow runs that one request as their
//! own, and a denial runs [`KEEP_HELD`] instead, leaving the page as it was filled.
//!
//! A session started for a workspace's Linux apps ([`tilo_actions::remote::linux_apps`]) is the
//! same device in the same workspace, run as the VM's tenant: it opens no Browser window, and reads
//! every later line of its standard input as one app to start ([`AppLaunch`], a desktop file of the
//! guest's). Each app's windows open on its shell in the workspace, and the session shares them with
//! the workspace's members the moment they map, so they see and drive them on their own sessions. It
//! reports [`USED`] while it holds any window, so its host keeps it while an app is open.
//!
//! [`READY`]: tilo_actions::remote::browser::READY
//! [`UP`]: tilo_actions::remote::browser::UP
//! [`USED`]: tilo_actions::remote::browser::USED
//! [`SEND_HELD`]: tilo_actions::remote::browser::SEND_HELD
//! [`KEEP_HELD`]: tilo_actions::remote::browser::KEEP_HELD

use std::collections::HashSet;
use std::io::{self, BufRead as _, Write as _};
use std::time::{Duration, Instant};

use tilo_actions::remote::browser::{MARK, Report, SEND_HELD};
use tilo_actions::remote::environment_device;
use tilo_actions::remote::linux_apps::{self, AppLaunch};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, CataloguePage, CatalogueQuery, InvokeArgument,
    InvokeRequest, ReceiptStatus, RemoteActionTarget,
};
use tilo_actions::selection::logical_resource_id;
use tilo_server_protocol::hosted::Material;
use tilo_workspace_service::apps::collab_app_broker::{AppBrokerState, ExecutionStatus};
use tilo_workspace_service::apps::services::HostedWorkspaces;

use crate::Shared;
use crate::app_wire::AppWire;
use crate::item_lending::ItemLending;
use runtime_collab::BrowserServiceSessionReply as ItemReply;

/// The app whose window a session holds.
pub use ui::apps::BROWSER;
/// The resource the window's page actions are actions of.
const PAGE_RESOURCE: &str = "browser.page";
/// The page action that drops a held request the person did not allow.
const KEEP_HELD: &str = tilo_actions::remote::browser::KEEP_HELD;
/// The most held requests one page of the window's catalogue is read for:
/// the window itself bounds what it holds at once, so one page names them all.
const HELD_PAGE: u32 = 64;
/// How long the invitation the server answers the join with stays redeemable.
const ENTER_INVITATION_SECONDS: u64 = 600;
/// How far behind the host this session's clock may read and still wait for it
/// to reach the certificate it was handed: a guest resumed from a template's
/// snapshot is behind by the template's age and the host sets its clock as it
/// wakes it, so what is left is a fraction of a second. A minute covers a clock
/// the host could not set, which is refused rather than waited out.
const CLOCK_WAIT_NS: u64 = 60 * 1_000_000_000;
/// How often the page's use is reported while it is in use: one an action or a viewer is what the
/// host counts its idle limit from, and a page being read for an hour is in use the whole time.
const USED_EVERY: Duration = Duration::from_secs(60);
/// How often a call the person is asked about is looked at again, where the host cannot tell
/// this session when the invocation moved.
const FOLLOW_EVERY: Duration = Duration::from_millis(250);
/// The colour the record of the person a session serves carries, as a device that joined
/// without their root gets it.
const ENVIRONMENT_AVATAR: [u8; 3] = [88, 101, 242];

/// What the session reports on its standard output: the host relays every line.
pub type Reports = Box<dyn FnMut(Report) + Send>;

/// What a session in a workspace's VM holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holds {
    /// The workspace's Browser window, on the record it was handed.
    Browser,
    /// The windows of the workspace's Linux apps, started as it is told.
    Apps,
}

impl Holds {
    /// What this process was started to hold, by the mark its host set: `None` for a session a
    /// person starts.
    #[must_use]
    pub fn marked() -> Option<Self> {
        let set = |mark: &str| std::env::var_os(mark).is_some_and(|value| value == "1");
        if set(linux_apps::MARK) {
            Some(Self::Apps)
        } else if set(MARK) {
            Some(Self::Browser)
        } else {
            None
        }
    }
}

/// Whether this session shares its windows with its workspace the moment they open: a session
/// in a workspace's VM runs for the workspace's members, and nobody sits at it.
#[must_use]
pub fn shares_by_default() -> bool {
    Holds::marked().is_some()
}

/// The session in a workspace's VM: the identity it was handed, the join it asked of its server,
/// and the window it holds.
pub struct HostedBrowser {
    /// The server's placement record: what this session joins, opens and is.
    material: Material,
    /// What it holds: Browser's window, or the Linux apps it is told to start.
    holds: Holds,
    /// The Linux apps it was told to start and has not started yet.
    launches: Vec<AppLaunch>,
    /// Woken when a line telling it to start an app arrives: the reader of its input has no other
    /// way to reach the loop.
    told_wake: std::sync::Arc<std::sync::Mutex<Option<tilo_service::Wake>>>,
    reports: Reports,
    /// What wakes the session's loop when the server answers or the page is used.
    waking: Option<tilo_service::Wake>,
    arrived: std::sync::mpsc::Receiver<Arrived>,
    sender: std::sync::mpsc::Sender<Arrived>,
    /// The session's broker, while it serves one: what the window publishes,
    /// and what the requests it holds are answered through.
    broker: Option<std::sync::Arc<AppBrokerState>>,
    /// Where the workspace's host is asked about a page: the server's device,
    /// which puts the person's answer to them.
    hosted: Option<std::sync::Arc<dyn HostedWorkspaces>>,
    /// Who sits here, as this session's own actions name them.
    person: String,
    /// The held requests the server has been told about, by the id the window
    /// published them with: each is put to the person once.
    held: HashSet<u32>,
    join: Join,
    window: Window,
    /// When the page's use was last reported.
    said_used: Option<Instant>,
    /// Whether the session's action service and its window shares ask the workspace's host yet: for the
    /// media keys of this device's own streams, and to write its shares.
    asks_host: bool,
    /// Whether this session's window shares are written by the workspace's host yet.
    governed: bool,
    /// What the session's account-credentials service answers Browser's asks for vault items
    /// with, once the session was handed it.
    items: Option<std::sync::Arc<ItemLending>>,
    /// Whether that service asks the workspace's server for those items yet.
    lends_items: bool,
}

/// How this session asks the workspace's host what its device may not hold or write itself
/// ([`tilo_actions::remote::environment_device`]).
#[derive(Clone)]
struct HostAsks {
    hosted: std::sync::Arc<dyn HostedWorkspaces>,
    caller: Caller,
    /// The workspace, by its UUID: the one this session was placed in, and the only one it asks
    /// about.
    workspace: String,
}

impl HostAsks {
    /// Ask the host for `action` with `argument`: what it answered, or `None` when it refused or
    /// did not answer.
    async fn ask(&self, action: &str, argument: InvokeArgument) -> Option<serde_json::Value> {
        match self.ask_told(action, argument).await {
            Ok(answer) => answer,
            Err(unanswered) => {
                match *unanswered {
                    Unanswered::Unreached => {}
                    Unanswered::Refused(error) => {
                        tracing::warn!(%error, action, "the workspace's host refused this session");
                    }
                    Unanswered::Status(status) => {
                        tracing::warn!(
                            ?status,
                            action,
                            "the workspace's host did not answer this session"
                        );
                    }
                }
                None
            }
        }
    }

    /// Ask as [`Self::ask`] does, and where there is no answer say why: a refusal's reason is
    /// handed back to whoever asked and not only written down.
    async fn ask_told(
        &self,
        action: &str,
        argument: InvokeArgument,
    ) -> Result<Option<serde_json::Value>, Box<Unanswered>> {
        let Some(host) = self.hosted.host_actions(&self.workspace) else {
            return Err(Box::new(Unanswered::Unreached));
        };
        let request = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: environment_device::target(action),
            argument,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let receipt = match host.invoke(&self.caller, request).await {
            Some(Ok(receipt)) => receipt,
            Some(Err(error)) => return Err(Box::new(Unanswered::Refused(error))),
            None => return Err(Box::new(Unanswered::Unreached)),
        };
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value),
            status => Err(Box::new(Unanswered::Status(status))),
        }
    }

    /// Ask the server to use the vault item `used` names for the page action it names, as
    /// the device it placed, and open what it answers with `key`, this device's own: the
    /// text, that its person is asked first, or the refusal in the server's own words.
    async fn use_item(
        &self,
        key: &ed25519_dalek::SigningKey,
        used: &environment_device::ItemUse,
    ) -> ItemReply {
        let argument = match serde_json::to_value(used) {
            Ok(serde_json::Value::Object(named)) => InvokeArgument::Json(named),
            _ => {
                return ItemReply::item_refused(
                    used.invocation,
                    "This session could not name the item to its workspace's server, so nothing \
                     was filled.",
                );
            }
        };
        let answered = self.ask_told(environment_device::USE_ITEM, argument).await;
        item_answer(&self.workspace, key, used, answered)
    }
}

/// Why the workspace's server gave this session no answer.
enum Unanswered {
    /// The server was not reached.
    Unreached,
    /// It refused before it started anything.
    Refused(ActionCallError),
    /// It answered that the action did not complete.
    Status(ReceiptStatus),
}

impl Unanswered {
    /// Why, in the server's own words where it gave any: a refusal's reason is carried as it
    /// was given.
    fn reason(&self) -> String {
        match self {
            Self::Unreached => {
                "This session cannot reach its workspace's server, so nothing was filled."
                    .to_owned()
            }
            Self::Refused(error) | Self::Status(ReceiptStatus::Rejected { error }) => match error {
                ActionCallError::Refused { reason } => reason.clone(),
                other => other.to_string(),
            },
            Self::Status(ReceiptStatus::Failed { error }) => error.message.clone(),
            Self::Status(ReceiptStatus::Cancelled { message }) => {
                message.clone().unwrap_or_else(|| {
                    "The workspace's server stopped the ask for the item, so nothing was filled."
                        .to_owned()
                })
            }
            Self::Status(_) => {
                "The workspace's server has not decided about the item, so nothing was filled."
                    .to_owned()
            }
        }
    }
}

/// What a box that does not open here is refused with: it names nothing of what it holds.
const NOT_SEALED_FOR_THIS: &str = "What the workspace's server answered was not sealed for \
                                   this fill of this page on this device, so nothing was filled.";

/// What Browser is answered for `used`, from what the workspace's server answered this
/// device: a text it sealed to `key` for this workspace, this page action and this site is
/// opened, a question's words and a refusal's reason are carried, and a box sealed for
/// anything else opens for nothing.
///
/// The text is held by the answer alone, which wipes it and prints nothing. No failure here
/// says anything of it.
fn item_answer(
    workspace: &str,
    key: &ed25519_dalek::SigningKey,
    used: &environment_device::ItemUse,
    answered: Result<Option<serde_json::Value>, Box<Unanswered>>,
) -> ItemReply {
    let invocation = used.invocation;
    let refused = |reason: &str| ItemReply::item_refused(invocation, reason);
    let answer = match answered {
        Ok(Some(answer)) => answer,
        Ok(None) => {
            return refused(
                "The workspace's server answered nothing for the item, so nothing was filled.",
            );
        }
        Err(unanswered) => return refused(&unanswered.reason()),
    };
    match serde_json::from_value::<environment_device::ItemUsed>(answer) {
        Ok(environment_device::ItemUsed::Asked { words }) => {
            ItemReply::item_asked(invocation, &words)
        }
        Ok(environment_device::ItemUsed::Released { sealed, into }) => {
            let Some(into) = runtime_collab::ItemField::from_word(&into) else {
                return refused(
                    "The workspace's server named a kind of field this session does not know, \
                     so nothing was filled.",
                );
            };
            let Ok(sealed) = hex::decode(sealed) else {
                return refused(NOT_SEALED_FOR_THIS);
            };
            let bound = key_group::ReleasedFor {
                workspace,
                invocation: *invocation.0.as_bytes(),
                origin: &used.origin,
            };
            match key_group::open_released_text(key, &bound, &sealed) {
                Ok(text) => ItemReply::ItemText {
                    invocation,
                    text,
                    into,
                },
                Err(_) => refused(NOT_SEALED_FOR_THIS),
            }
        }
        Err(_) => refused(
            "The workspace's server answered something that is no item's use, so nothing was \
             filled.",
        ),
    }
}

/// The action service's lender: the media keys of this device's own streams, as the workspace's host lends
/// them, opened with this device's key.
struct HostLends {
    asking: HostAsks,
    key: ed25519_dalek::SigningKey,
}

impl crate::live::MediaKeysLender for HostLends {
    fn lend(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<key_group::SenderMediaKeys>> + Send + '_>,
    > {
        Box::pin(async move {
            if workspace.to_string() != self.asking.workspace {
                return None;
            }
            let answer = self
                .asking
                .ask(environment_device::MEDIA_KEYS, InvokeArgument::None)
                .await?;
            let lent: environment_device::MediaKeys = serde_json::from_value(answer).ok()?;
            let sealed = hex::decode(lent.sealed).ok()?;
            match key_group::SenderMediaKeys::open_with(
                &self.key,
                self.key.verifying_key().to_bytes(),
                &sealed,
            ) {
                Ok(keys) => Some(keys),
                Err(error) => {
                    tracing::warn!(%error, "the media keys the host lent do not open here");
                    None
                }
            }
        })
    }
}

/// The window shares' governor: the workspace's host writes each share of this device's windows.
struct HostGoverns(HostAsks);

impl crate::window_share::ShareGovernor for HostGoverns {
    fn share(&self, workspace: &str, app_id: &str, tab: u64, shared: bool) {
        if workspace != self.0.workspace {
            return;
        }
        let Ok(runtime) = tilo_rt::try_app() else {
            tracing::warn!(
                tab,
                "a window's share is not written: no task to ask the host on"
            );
            return;
        };
        let action = if shared {
            environment_device::SHARE_WINDOW
        } else {
            environment_device::END_WINDOW_SHARE
        };
        let window = environment_device::WindowShare {
            app: app_id.to_owned(),
            tab,
        };
        let argument = match serde_json::to_value(window) {
            Ok(serde_json::Value::Object(map)) => InvokeArgument::Json(map),
            _ => return,
        };
        let asking = self.0.clone();
        runtime.spawn(async move {
            if asking.ask(action, argument).await.is_some() {
                tracing::info!(tab, shared, "the host wrote this window's share");
            }
        });
    }
}

/// What reached the loop from the tasks that watch the server and the shell.
enum Arrived {
    /// The server answered the join: the invitation text to redeem, or why it did not.
    Invitation(Result<String, String>),
    /// An action ran on the window's page.
    PageAction,
    /// What the window holds for the person now, as it published it.
    Held(Vec<Held>),
    /// Its host told it to start a Linux app.
    Launch(AppLaunch),
    /// What the workspace's host answered the call that would release the
    /// request the page waits with: the receipt as it stood, and the call.
    Answered {
        request: Box<InvokeRequest>,
        answer: Box<Result<tilo_actions::remote::InvocationReceipt, ActionCallError>>,
    },
    /// That call is over without running: the person did not allow it, or
    /// nobody answered, so the page stays as it was filled.
    Kept(Box<InvokeRequest>),
}

/// One departure the window holds that no page action asked for.
struct Held {
    /// The id the window answers it by.
    id: u32,
    /// What leaves, as the window words it.
    describing: String,
    /// The page it leaves from, what it does there, the method it leaves with
    /// and where it goes, by the names the window states them under: what the
    /// person's question carries, so they see what leaves and not only a name
    /// for it.
    facts: Vec<(String, String)>,
}

/// How far the join has come.
enum Join {
    /// The server has not been asked yet.
    Waiting,
    /// The server was asked, and what it answers is on its way: the invitation is redeemed by the
    /// session's own join, which settles the workspace on the shell.
    Asking,
}

/// The window the session holds.
enum Window {
    /// Nothing was opened: the shell does not hold the workspace yet.
    Unopened,
    /// The launch was made, and no window published for it yet.
    Opening,
    /// The window publishes under this provider, which page actions name: the id the
    /// session's broker lists it by, one number.
    Open(u64),
    /// Browser did not open, and why is in the log: nothing asks again.
    Failed,
}

impl HostedBrowser {
    /// The session, joining as the device `material`, the server's placement record
    /// names: its identity is taken here, before anything reads this device of itself, and `ready`
    /// is reported. `reports` is where the session says what it became.
    ///
    /// # Errors
    ///
    /// The material is not the server's, or the identity it names is not one this device may take.
    pub fn take(material: &str, reports: Reports) -> Result<Self, String> {
        Self::take_holding(material, Holds::Browser, reports)
    }

    /// The session, as [`Self::take`], holding what `holds` says.
    ///
    /// # Errors
    ///
    /// As [`Self::take`].
    pub fn take_holding(material: &str, holds: Holds, reports: Reports) -> Result<Self, String> {
        let material: Material = serde_json::from_str(material.trim())
            .map_err(|error| format!("the material is not the server's: {error}"))?;
        take_identity(&material)?;
        let (sender, arrived) = std::sync::mpsc::channel();
        let mut hosted = Self {
            material,
            holds,
            launches: Vec::new(),
            told_wake: std::sync::Arc::default(),
            reports,
            waking: None,
            arrived,
            sender,
            broker: None,
            hosted: None,
            person: String::new(),
            held: HashSet::new(),
            join: Join::Waiting,
            window: Window::Unopened,
            said_used: None,
            asks_host: false,
            governed: false,
            items: None,
            lends_items: false,
        };
        hosted.say(Report::Ready);
        Ok(hosted)
    }

    /// Wake the session's loop with `wake` when the server answers the join or the page is used.
    pub(crate) fn waking(&mut self, wake: tilo_service::Wake) {
        *self
            .told_wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(wake.clone());
        self.waking = Some(wake);
    }

    /// Take what the session's account-credentials service answers Browser's asks for vault
    /// items with. A session that holds a workspace's Browser says so from here on, and has
    /// the items used once its server can be asked.
    pub(crate) fn lending(&mut self, items: Option<std::sync::Arc<ItemLending>>) {
        if self.holds == Holds::Browser
            && let Some(items) = &items
        {
            items.hold(&self.material.workspace);
        }
        self.items = items;
    }

    /// Take from the session's app wire what the window it holds needs of it, as `person`'s
    /// device: what its broker admits ([`Self::watching`]), and then what answers Browser's
    /// asks for vault items ([`Self::lending`]).
    pub(crate) fn wired(&mut self, app_wire: Option<&AppWire>, person: &str) {
        self.watching(
            app_wire.and_then(AppWire::agent_broker).as_ref(),
            app_wire.and_then(AppWire::hosted_workspaces),
            person,
        );
        self.lending(app_wire.map(AppWire::item_lending));
    }

    /// Read what its host tells it on `input` from now on, a line at a time, each a Linux app
    /// to start: on a thread of its own, since the input blocks, waking the loop for each.
    fn read_launches(&self, input: impl std::io::BufRead + Send + 'static) {
        let arrived = self.sender.clone();
        let wake = std::sync::Arc::clone(&self.told_wake);
        let spawned = std::thread::Builder::new()
            .name("hosted-apps-input".into())
            .spawn(move || {
                for line in input.lines() {
                    let Ok(line) = line else {
                        return;
                    };
                    let Some(launch) = AppLaunch::parse(&line) else {
                        tracing::warn!(%line, "the host told this session something it does not start");
                        continue;
                    };
                    if arrived.send(Arrived::Launch(launch)).is_err() {
                        return;
                    }
                    let wake = wake
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    if let Some(wake) = wake {
                        wake();
                    }
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "this session reads no Linux apps to start");
        }
    }

    /// Follow what the session's broker admits: an action on the window's page is the page in use,
    /// and the page track's viewers say so too ([`Self::follow`]). `hosted` is where the workspace's
    /// host is asked about a departure the window holds, and `person` who this device acts as.
    /// A session that waits for its window is woken once the broker lists it
    /// ([`Self::wake_when_listed`]).
    pub(crate) fn watching(
        &mut self,
        broker: Option<&std::sync::Arc<AppBrokerState>>,
        hosted: Option<std::sync::Arc<dyn HostedWorkspaces>>,
        person: &str,
    ) {
        self.broker = broker.cloned();
        self.hosted = hosted;
        self.person = person.into();
        let Some(broker) = broker.cloned() else {
            return;
        };
        let Ok(runtime) = tilo_rt::try_app() else {
            tracing::warn!("a hosted Browser's page is not watched: no task to watch it on");
            return;
        };
        self.wake_when_listed(&broker);
        let mut activity = broker.subscribe_execution_activity();
        let arrived = self.sender.clone();
        let waking = self.waking.clone();
        let app = BROWSER.to_owned();
        let workspace = self.material.workspace.clone();
        runtime.spawn(async move {
            while let Ok(admitted) = activity.recv().await {
                // An execution is reported as it starts and as it ends: the page is in use once it
                // is admitted, whatever became of it.
                if admitted.route.app_id != app
                    || admitted.route.cluster_id != workspace
                    || admitted.status != ExecutionStatus::Accepted
                {
                    continue;
                }
                if arrived.send(Arrived::PageAction).is_err() {
                    return;
                }
                if let Some(wake) = &waking {
                    wake();
                }
            }
        });
    }

    /// Wake the session's loop once `broker` lists the window this session holds, so `up` is
    /// said by the pass that wake makes ([`Self::publish`]) and not by the next one made for
    /// something else. Only a session that holds Browser waits for a window. The wake is all
    /// this does: it reads the listing [`Self::publish`] reads, hands the session nothing and
    /// reports nothing, and it ends with its one wake.
    fn wake_when_listed(&self, broker: &std::sync::Arc<AppBrokerState>) {
        if self.holds != Holds::Browser {
            return;
        }
        let (Some(wake), Ok(tasks)) = (self.waking.clone(), tilo_rt::try_app()) else {
            return;
        };
        // Followed from here, before the task has its first turn: whatever an app publishes
        // from now on is seen.
        let mut changes = broker.surface_changes();
        // The task keeps no broker alive: a session that ends takes its broker with it.
        let listing = std::sync::Arc::downgrade(broker);
        let workspace = self.material.workspace.clone();
        let shown = self.material.record.clone();
        tasks.spawn(async move {
            loop {
                match listing
                    .upgrade()
                    .map(|broker| listed(&broker, &workspace, &shown))
                {
                    None => return,
                    Some(Some(_)) => break,
                    Some(None) => {}
                }
                if changes.changed().await.is_err() {
                    return;
                }
            }
            wake();
        });
    }

    /// What the session does each pass: redeem the invitation the server answered with, open the
    /// record's window once the shell holds the workspace, and report what became of it.
    pub(crate) fn follow(&mut self, shared: &Shared) {
        self.take_arrived(shared);
        self.join(shared);
        self.ask_host(shared);
        if self.holds == Holds::Apps {
            self.follow_apps(shared);
            return;
        }
        self.open(shared);
        if let Window::Open(provider) = &self.window
            && shared.pages.borrow().viewed().contains(provider)
        {
            self.used();
        }
        self.publish();
    }

    /// Once the workspace's host can be asked, have the session ask it for what this device may not
    /// hold or write itself: the action service is lent the media keys of this device's own streams
    /// ([`HostLends`]), and the window shares are written by the host ([`HostGoverns`]).
    fn ask_host(&mut self, shared: &Shared) {
        if self.asks_host {
            return;
        }
        let Some(hosted) = self.hosted.clone() else {
            return;
        };
        let asking = HostAsks {
            hosted,
            caller: self.caller(),
            workspace: self.material.workspace.clone(),
        };
        if !self.lends_items {
            // The pages of this window are filled from vault items the server keeps: the
            // session's account-credentials service asks for each as this device from here on,
            // and opens the answer with this device's key.
            self.lends_items = true;
            if self.holds == Holds::Browser
                && let Some(items) = &self.items
            {
                match self.material.key() {
                    Ok(key) => {
                        let asking = asking.clone();
                        items.lent_by(std::sync::Arc::new(
                            move |used: environment_device::ItemUse| -> crate::item_lending::Lending {
                                let (asking, key) = (asking.clone(), key.clone());
                                Box::pin(async move { asking.use_item(&key, &used).await })
                            },
                        ));
                    }
                    Err(error) => {
                        tracing::warn!(%error, "no vault item is used for this window's pages");
                    }
                }
            }
        }
        if !self.governed {
            // Before any window is shared: a share the session wrote itself would be dropped.
            self.governed = true;
            shared
                .window_shares
                .borrow_mut()
                .governed_by(std::rc::Rc::new(HostGoverns(asking.clone())));
        }
        let Some(sync) = shared.sync.clone() else {
            return;
        };
        let key = match self.material.key() {
            Ok(key) => key,
            Err(error) => {
                tracing::warn!(%error, "this session's own streams cannot be lent keys");
                self.asks_host = true;
                return;
            }
        };
        let lender = std::sync::Arc::new(HostLends { asking, key });
        if !sync.lend_media_keys(lender) {
            // No action service yet: the next pass hands it over.
            return;
        }
        self.asks_host = true;
    }

    /// What an apps session does each pass: say it is up once the shell holds the workspace, start
    /// each app it was told to, and say it is used while any window of theirs is open.
    fn follow_apps(&mut self, shared: &Shared) {
        if !held(shared, &self.material.workspace) {
            return;
        }
        if matches!(self.window, Window::Unopened) {
            self.window = Window::Open(0);
            self.say(Report::Up {
                provider: linux_apps::UP_PROVIDER.to_owned(),
            });
        }
        let Some(apps) = &shared.launch else {
            return;
        };
        let workspace = apps.workspace_named(&self.material.workspace);
        for launch in std::mem::take(&mut self.launches) {
            // What runs is what the guest's desktop file says: the same launch as an app a
            // person opens from their own computer's applications.
            let start = tilo_app_launch::Launch::Host {
                desktop_id: launch.desktop_id.clone(),
                open: None,
            };
            match apps.start(&start, &workspace) {
                Ok(()) => tracing::info!(app = %launch.desktop_id, "a Linux app started"),
                Err(error) => {
                    tracing::warn!(%error, app = %launch.desktop_id, "a Linux app did not start");
                }
            }
        }
        if holds_windows(shared, &self.material.workspace) {
            self.used();
        }
    }

    /// Redeem what the server answered the join with: the invitation text through the session's own
    /// join, the path a pasted invitation takes.
    fn take_arrived(&mut self, shared: &Shared) {
        let arrived: Vec<Arrived> = self.arrived.try_iter().collect();
        for arrived in arrived {
            match arrived {
                Arrived::Invitation(Ok(text)) => {
                    if let Err(reason) =
                        crate::platform::directory::take(&shared.directory_store, &text)
                    {
                        tracing::warn!(%reason, "the workspace was not joined");
                    }
                }
                Arrived::Invitation(Err(reason)) => {
                    tracing::warn!(%reason, "the server did not let this session into the workspace");
                }
                Arrived::PageAction => self.used(),
                Arrived::Launch(launch) => self.launches.push(launch),
                Arrived::Held(held) => self.report(held),
                Arrived::Answered { request, answer } => match *answer {
                    // It ran: the window released what it was waiting with, as
                    // the person allowed.
                    Ok(receipt) => match receipt.status {
                        ReceiptStatus::Completed { .. } => {}
                        // Kept as its asked record, or still with the window:
                        // followed until it settles.
                        ReceiptStatus::Accepted
                        | ReceiptStatus::Rejected {
                            error: ActionCallError::ConfirmationRequired(_),
                        } => self.follow_ask(*request),
                        ReceiptStatus::Rejected { .. }
                        | ReceiptStatus::Failed { .. }
                        | ReceiptStatus::Cancelled { .. } => self.kept(*request),
                    },
                    // Nothing answered it: the page stays as it was filled,
                    // and the engine drops the request at its own deadline.
                    Err(error) => {
                        tilo_failure::ignored!(
                            "a call nobody answered releases nothing, and the engine drops the \
                             request the page waits with",
                            tracing::warn!(
                                %error,
                                "the workspace's host answered nothing about a page's request"
                            )
                        );
                        self.kept(*request);
                    }
                },
                Arrived::Kept(request) => self.kept(*request),
            }
        }
    }

    /// Ask its server to enter the device into the workspace, once: as the device the
    /// material certifies, it answers with an invitation, which the session redeems.
    ///
    /// The server is reached at the address the material names, which is where this VM reaches
    /// it; the invitation names the server where it sees itself, its own loopback. Once the server
    /// answered there, the session's endpoint dials it there too, so the join and everything
    /// after it reach the server this session did.
    fn join(&mut self, shared: &Shared) {
        if !matches!(self.join, Join::Waiting) {
            return;
        }
        self.join = Join::Asking;
        let Ok(runtime) = tilo_rt::try_app() else {
            tracing::warn!("the workspace is not joined: no task to ask the server on");
            return;
        };
        let material = self.material.clone();
        let arrived = self.sender.clone();
        let waking = self.waking.clone();
        let reached = shared.sync.as_ref().map(|sync| sync.reached());
        runtime.spawn(async move {
            let answer = invitation(&material).await;
            if answer.is_ok()
                && let Some(reached) = &reached
            {
                reached.note(&material.server);
            }
            if arrived.send(Arrived::Invitation(answer)).is_err() {
                return;
            }
            if let Some(wake) = &waking {
                wake();
            }
        });
    }

    /// Open the record's Browser window, once the shell holds the workspace the material names.
    fn open(&mut self, shared: &Shared) {
        if !matches!(self.window, Window::Unopened) {
            return;
        }
        if !held(shared, &self.material.workspace) {
            return;
        }
        let Some(apps) = &shared.launch else {
            return;
        };
        // The window is opened for the agent that works it, not for the
        // person: it browses as any visitor would, on a fresh profile of its
        // own, so an agent's page actions take no authority over the person's
        // sign-ins — and what acts on the page still asks them.
        let launch = tilo_app_launch::Launch::Record {
            app: BROWSER.to_owned(),
            record: self.material.record.clone(),
            visitor: true,
        };
        let workspace = apps.workspace_named(&self.material.workspace);
        match apps.start(&launch, &workspace) {
            Ok(()) => self.window = Window::Opening,
            Err(error) => {
                tracing::warn!(%error, record = %self.material.record, "the session's Browser did not open");
                self.window = Window::Failed;
            }
        }
    }

    /// The provider the session's broker lists this session's window under: the
    /// app the session started, showing what the server opened it on ([`listed`]).
    fn window_provider(&self) -> Option<u64> {
        listed(
            self.broker.as_ref()?,
            &self.material.workspace,
            &self.material.record,
        )
    }

    /// Report the window's provider once it publishes: the window instance the shell shows the app
    /// in, which is what every page action on it names. It is the one the session's broker lists
    /// the window under, so a caller naming that id reaches the window's own actions.
    fn publish(&mut self) {
        if !matches!(self.window, Window::Opening) {
            return;
        }
        let Some(provider) = self.window_provider() else {
            return;
        };
        self.say(Report::Up {
            provider: provider.to_string(),
        });
        self.window = Window::Open(provider);
        self.watch_holds(provider);
    }

    /// Read what the window holds for the person, from now on: each departure
    /// nothing asked for is reported once ([`Self::report`]), as the window
    /// publishes it and whenever what it publishes changes — never on a
    /// timer, and never a second time for one the person is deciding on.
    fn watch_holds(&self, provider: u64) {
        let (Some(broker), Ok(runtime)) = (self.broker.clone(), tilo_rt::try_app()) else {
            return;
        };
        let arrived = self.sender.clone();
        let waking = self.waking.clone();
        let caller = self.caller();
        let provider = provider.to_string();
        runtime.spawn(async move {
            let query = CatalogueQuery {
                provider: Some(provider),
                max_results: HELD_PAGE,
                ..CatalogueQuery::default()
            };
            let mut changes = broker.surface_changes();
            loop {
                let mut held = Vec::new();
                if let Some(Ok(page)) = broker.action_query(&caller, &query).await {
                    held = published_holds(&page);
                }
                if arrived.send(Arrived::Held(held)).is_err() {
                    return;
                }
                if let Some(wake) = &waking {
                    wake();
                }
                // The next thing the app publishes: a departure the page just
                // started, or one the person's answer took away.
                if changes.changed().await.is_err() {
                    return;
                }
            }
        });
    }

    /// Call the workspace's host about each departure the window keeps that
    /// nobody has been asked about yet, as the person at this device: the host
    /// keeps each call as its own asked record where the person must give
    /// their yes, and its answer comes back as [`Arrived::Answered`].
    fn report(&mut self, held: Vec<Held>) {
        let Some(window) = (match &self.window {
            Window::Open(provider) => Some(*provider),
            _ => None,
        }) else {
            return;
        };
        let Some(host) = self
            .hosted
            .as_ref()
            .and_then(|hosted| hosted.host_actions(&self.material.workspace))
        else {
            return;
        };
        let caller = self.caller();
        for held in held {
            if !self.held.insert(held.id) {
                continue;
            }
            let request = answer(held.id, window, &held.describing, &held.facts, SEND_HELD);
            let arrived = self.sender.clone();
            let waking = self.waking.clone();
            host.invoke(&caller, request.clone()).then(move |answer| {
                let answered = Arrived::Answered {
                    request: Box::new(request),
                    answer: Box::new(answer.unwrap_or(Err(ActionCallError::Unavailable {
                        reason: "the workspace's host answered nothing".into(),
                    }))),
                };
                if arrived.send(answered).is_err() {
                    return;
                }
                if let Some(wake) = &waking {
                    wake();
                }
            });
        }
    }

    /// Follow the call that would release `request`'s request until it
    /// settles: the person's approve runs it where the window keeps what it
    /// releases, and their decline, or nobody's answer, is told to the loop as
    /// [`Arrived::Kept`]. Nothing blocks: the host tells this when the
    /// invocation moved, or the wait between looks keeps a thread busy.
    fn follow_ask(&self, request: InvokeRequest) {
        let Some(host) = self
            .hosted
            .as_ref()
            .and_then(|hosted| hosted.host_actions(&self.material.workspace))
        else {
            return self.kept(request);
        };
        let Ok(runtime) = tilo_rt::try_app() else {
            return self.kept(request);
        };
        let caller = self.caller();
        let arrived = self.sender.clone();
        let waking = self.waking.clone();
        let invocation = request.invocation;
        runtime.spawn(async move {
            loop {
                let settled = match host.observe(&caller, &invocation) {
                    Some(state) => match state.update {
                        tilo_actions::InvocationUpdate::Succeeded { .. } => return,
                        tilo_actions::InvocationUpdate::Failed { .. }
                        | tilo_actions::InvocationUpdate::Cancelled { .. } => true,
                        tilo_actions::InvocationUpdate::Accepted { .. }
                        | tilo_actions::InvocationUpdate::Progress { .. }
                        | tilo_actions::InvocationUpdate::Item { .. } => false,
                    },
                    // The host no longer has it, so it will not run.
                    None => true,
                };
                if settled {
                    if arrived.send(Arrived::Kept(Box::new(request))).is_ok()
                        && let Some(wake) = &waking
                    {
                        wake();
                    }
                    return;
                }
                match host.next_change(&caller, &invocation) {
                    Some(change) => {
                        let _ = change.await;
                    }
                    None => tokio::time::sleep(FOLLOW_EVERY).await,
                }
            }
        });
    }

    /// Keep the page as it was filled, dropping the request the call would
    /// have released.
    fn kept(&self, request: InvokeRequest) {
        tilo_failure::ignored!(
            "a denial drops the request, and keeps the page as it was filled",
            self.run(&keep(request))
        );
    }

    /// Run `request` on the window as the person at this device: the page action
    /// that releases what they allowed, or drops what they did not.
    fn run(&self, request: &InvokeRequest) {
        let (Some(broker), Ok(runtime)) = (self.broker.clone(), tilo_rt::try_app()) else {
            return;
        };
        let caller = self.caller();
        let answer = broker.action_invoke(&caller, None, None, request);
        // The app answers the invocation in its own time: the page stays as
        // it was until then, and nothing of this pass waits for it.
        runtime.spawn(async move {
            let _ran = answer.await;
        });
    }

    /// Who this device acts as, as the session's own actions name it: the person in
    /// the workspace the session holds a window of.
    fn caller(&self) -> Caller {
        Caller::local_person(self.person.clone()).in_workspace(self.material.workspace.clone())
    }

    /// Report the page in use, no more than once a minute while it is.
    fn used(&mut self) {
        let now = Instant::now();
        if self
            .said_used
            .is_some_and(|said| now.duration_since(said) < USED_EVERY)
        {
            return;
        }
        self.said_used = Some(now);
        self.say(Report::Used);
    }

    /// Say `report` on the session's standard output.
    fn say(&mut self, report: Report) {
        (self.reports)(report);
    }
}

impl crate::Shared {
    /// The session follows its join and its window each pass: this is all it does of
    /// what a person's session does, so what it opened and what it reports is followed here.
    pub(crate) fn follow_hosted_browser(&self) {
        if let Some(hosted) = &mut *self.hosted_browser.borrow_mut() {
            hosted.follow(self);
        }
    }
}

/// The session its host started, when its host says so: what it is
/// started with is the material as the first line of its standard input, and nothing else.
///
/// # Errors
///
/// The material was not read, or names an identity this device may not take.
pub fn handed() -> Result<Option<HostedBrowser>, String> {
    let Some(holds) = Holds::marked() else {
        return Ok(None);
    };
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|error| format!("the material was not read from the session's input: {error}"))?;
    if line.trim().is_empty() {
        return Err("the session was handed no material".to_owned());
    }
    let hosted =
        HostedBrowser::take_holding(&line, holds, Box::new(|report| say_on_stdout(&report)))?;
    // An apps session is told each app to start as a later line of the same input.
    if holds == Holds::Apps {
        hosted.read_launches(std::io::BufReader::new(std::io::stdin()));
    }
    Ok(Some(hosted))
}

/// Every report, one line on the session's standard output: what the host that started the
/// session reads, and the log is its stderr only.
fn say_on_stdout(report: &Report) {
    println!("{}", report.line());
    if let Err(error) = std::io::stdout().flush() {
        tracing::warn!(%error, "a hosted Browser's report was not written");
    }
}

/// Take `material`'s device as this session's own: the key it signs with, and the certificate the
/// server issued for it, where every part of the session that reads this device of itself reads it.
fn take_identity(material: &Material) -> Result<(), String> {
    let paths = tilo_identity::provisioning::collaboration_identity_paths();
    paths
        .ensure_directories()
        .map_err(|error| format!("this device's identity has nowhere to live: {error}"))?;
    // Only this session's user reads any of it: a workspace's own agent has
    // sudo in this VM and may copy what it finds, so what it finds is one key
    // for one record, in directories nobody else may enter.
    for directory in [
        paths.user_pubkey.parent(),
        paths.device_privkey.parent(),
        paths.device_cert.parent(),
    ]
    .into_iter()
    .flatten()
    {
        tilo_files::native::private::restrict_directory(directory)
            .map_err(|error| format!("this device's identity is not private: {error}"))?;
    }
    let key = material.key()?;
    // [`write_signing_key`] is the one way to write a key only its owner may
    // read (0600), rather than the device key's group-readable mode.
    tilo_device_identity::persist::write_signing_key(&paths.device_privkey, &key)
        .map_err(|error| format!("this device's key was not written: {error}"))?;
    // A guest resumed from its template's snapshot wakes with the clock that
    // snapshot was taken at, behind the host's: the certificate the server issued
    // this moment is there "not yet valid". The host sets the guest's clock as
    // it wakes it; should it still read a little behind, this session waits for
    // the clock to reach its certificate rather than ending without an identity.
    loop {
        let refused = match tilo_device_identity::install_device_cert(
            &paths,
            &material.device,
            tilo_clock::unix_now_ns(),
        ) {
            Ok(()) => break,
            Err(tilo_device_identity::PersistError::Io(error)) => error,
            Err(error) => {
                return Err(format!("this device's certificate was not taken: {error}"));
            }
        };
        let waiting = tilo_user_identity::CertificateClockError::in_io(&refused)
            .and_then(tilo_user_identity::CertificateClockError::valid_at_ns)
            .filter(|at| at.saturating_sub(tilo_clock::unix_now_ns()) <= CLOCK_WAIT_NS);
        let Some(at) = waiting else {
            return Err(format!(
                "this device's certificate was not taken: {refused}"
            ));
        };
        if tilo_device_identity::wait_for_wall_clock(at).is_err() {
            return Err(format!(
                "this device's certificate was not taken: {refused}"
            ));
        }
    }
    // The person the workspace belongs to, as a record this device holds of them: the server hands
    // over their public key, and every part of the session that presents this device's person in a
    // workspace reads that person from this login's identity records. The record carries no root:
    // what signs here is the device certificate the server issued.
    let directory = tilo_identity::store::identity_dir_from_env()
        .map_err(|error| format!("this device's identity directory is not known: {error}"))?;
    let record = tilo_identity::store::public_member_record(
        tilo_identity::store::UNNAMED_DISPLAY_NAME,
        ENVIRONMENT_AVATAR,
        material.user_public_key,
    )
    .map_err(|error| format!("the person's record was not written: {error}"))?;
    let user = hex::encode(material.user_public_key);
    let id = format!("identity-{}", user.get(..55).unwrap_or(&user));
    if let Err(error) = tilo_identity::store::import_identity(&directory, &id, &record)
        && error.kind() != io::ErrorKind::AlreadyExists
    {
        return Err(format!("the person's record was not taken: {error}"));
    }
    Ok(())
}

/// Ask its server to enter the session's device into the workspace, the way a member
/// device asks to enter a workspace its server hosts, and answer the invitation text it redeems.
async fn invitation(material: &Material) -> Result<String, String> {
    use tilo_actions::InvocationUpdate;
    use tilo_actions::remote::{InvokeArgument, InvokeRequest};
    use tilo_server_protocol::workspaces::{
        ENTER_WORKSPACE, EnteredWorkspace, HOST_ACTIONS_PROVIDER, HOST_WORKSPACES_RESOURCE,
    };
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .map_err(|error| format!("the session reached no network: {error}"))?;
    let joined = async {
        // The host answers this device's ask on its actions link: entering is
        // one of the host's own actions, as it is for any device asking.
        let actions = std::sync::Arc::new(tilo_peer_state::actions::ActionService::new(
            material.key()?,
            std::sync::Arc::new(tilo_peer_state::actions::NoActions),
        ));
        let remote = std::sync::Arc::new(actions.remote(
            &endpoint,
            &material.server,
            &tilo_server_protocol::WorkspaceId::nil().to_string(),
        ));
        let arguments = serde_json::Map::from_iter([
            ("workspace".to_owned(), material.workspace.clone().into()),
            (
                "expires_in_seconds".to_owned(),
                ENTER_INVITATION_SECONDS.into(),
            ),
        ]);
        let request = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: HOST_ACTIONS_PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(
                    HOST_WORKSPACES_RESOURCE.to_owned(),
                    ENTER_WORKSPACE,
                ),
            },
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        match remote
            .invoke_until_settled(request, std::time::Duration::from_secs(60))
            .await
        {
            Some(Ok(InvocationUpdate::Succeeded { output, .. })) => {
                let entered: EnteredWorkspace =
                    serde_json::from_value(output.value.unwrap_or_default())
                        .map_err(|error| format!("the server answered no invitation: {error}"))?;
                let signed = entered
                    .invitation
                    .decode()
                    .map_err(|error| format!("the server answered no invitation: {error}"))?
                    .signed_invitation;
                Ok(tilo_workspace_join::encode_invitation_text(&signed))
            }
            Some(Ok(InvocationUpdate::Failed { error, .. })) => Err(error.message),
            Some(Ok(_)) => Err("the server did not answer the entry".into()),
            Some(Err(error)) => Err(format!("the server refused to enter it: {error}")),
            None => Err("the session could not reach its server".into()),
        }
    }
    .await;
    endpoint.close().await;
    joined
}

/// The provider `broker` lists Browser's window under in `workspace`, the one that shows
/// `shown` when one does: what a session reports its window as, and what it waits for.
/// `None` while the broker lists no such window yet.
fn listed(broker: &AppBrokerState, workspace: &str, shown: &str) -> Option<u64> {
    broker.window_provider(BROWSER, workspace, Some(shown))
}

/// The departures `page` reports the window holding: each action the window
/// publishes to let one leave, named by the id it is bound to and worded as
/// what leaves. A window that holds nothing reports none.
fn published_holds(page: &CataloguePage) -> Vec<Held> {
    page.rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .filter(|action| {
            logical_resource_id(&action.address.resource) == PAGE_RESOURCE
                && action.address.action == SEND_HELD
        })
        .filter_map(|action| {
            let stated = |name: &str| {
                action
                    .params
                    .iter()
                    .find(|param| param.name == name)
                    .and_then(|param| param.choices.first())
                    .map(ToString::to_string)
            };
            Some(Held {
                id: stated("id")?.parse().ok()?,
                describing: action.action_label.clone(),
                // Every departure fact the window states, in the names it
                // states them under: what the person's question carries.
                facts: action
                    .params
                    .iter()
                    .filter(|param| param.name != "id")
                    .filter_map(|param| {
                        let value = param.choices.first()?.to_string();
                        (!value.is_empty()).then(|| (param.name.to_string(), value))
                    })
                    .collect(),
            })
        })
        .collect()
}

/// The invocation of page action `action` on the window `provider` naming the
/// held request `id`, worded for the person as `describing`: what the host
/// puts to them, and what their answer is good for running. The departure's
/// `facts` travel in its context, so the question names the page it leaves
/// from and what it sends there.
fn answer(
    id: u32,
    provider: u64,
    describing: &str,
    facts: &[(String, String)],
    action: &str,
) -> InvokeRequest {
    InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: provider.to_string(),
            address: tilo_actions::ActionAddress::new(PAGE_RESOURCE, action),
        },
        argument: InvokeArgument::Json(serde_json::Map::from_iter([(
            "id".to_owned(),
            id.to_string().into(),
        )])),
        context: Some(tilo_actions::ActionContext {
            purpose: Some(describing.to_owned()),
            facets: facts
                .iter()
                .map(|(key, value)| {
                    tilo_actions::remote::browser::departure_facet(key, value.clone())
                })
                .collect(),
            ..tilo_actions::ActionContext::default()
        }),
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// [`answer`]'s request as the action that keeps the page as it is: what the
/// session runs when the person did not allow the departure, or nobody
/// answered. It names the same held request and carries no confirmation.
fn keep(mut request: InvokeRequest) -> InvokeRequest {
    request.invocation = tilo_actions::InvocationId::new();
    request.target.address.action = String::from(KEEP_HELD);
    request.confirmation = None;
    request
}

/// Whether the shell holds `workspace`.
fn held(shared: &Shared, workspace: &str) -> bool {
    shared.handle.read(|tree| {
        tree.workspaces
            .iter()
            .any(|held| held.id().to_string() == workspace)
    })
}

/// Whether `workspace` holds any window of a program of this computer's own: a Linux app the
/// session started, which keeps the session in use while it is open.
fn holds_windows(shared: &Shared, workspace: &str) -> bool {
    shared.handle.read(|tree| {
        tree.workspaces
            .iter()
            .filter(|held| held.id().to_string() == workspace)
            .flat_map(|held| held.strips.iter())
            .flat_map(|strip| strip.window_groups.iter())
            .flat_map(|group| group.tabs.iter())
            .any(|tab| matches!(tab.tile, tilo_desktop_model::TileContent::Foreign { .. }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tilo_server_protocol::WorkspaceId;

    /// What a server hands a session: a device of the workspace's person that
    /// the host certified with its stand-in.
    fn material() -> Material {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let user = root.verifying_key().to_bytes();
        let now = tilo_clock::unix_now_ns();
        let stand_in_key = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
        let mut stand_in = tilo_user_identity::prepare_stand_in_cert(
            user,
            stand_in_key.verifying_key().to_bytes(),
            "the person's host",
            now,
            now.saturating_add(24 * 60 * 60 * 1_000_000_000),
        );
        tilo_user_identity::sign_device_cert(&mut stand_in, &root).unwrap();
        let secret = [3_u8; 32];
        let mut device = tilo_user_identity::DeviceCert {
            user_public_key: user,
            device_public_key: ed25519_dalek::SigningKey::from_bytes(&secret)
                .verifying_key()
                .to_bytes(),
            device_label: "Browser".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: now,
            expires_at_ns: Some(now.saturating_add(2 * 60 * 60 * 1_000_000_000)),
            signature: [0; 64],
            issuer: Some(Box::new(stand_in)),
        };
        tilo_user_identity::sign_device_cert(&mut device, &stand_in_key).unwrap();
        Material {
            workspace: "11111111-1111-4111-8111-111111111111".into(),
            workspace_id: WorkspaceId::from_u128(7),
            record: "browser-1".into(),
            device,
            secret,
            server: iroh::EndpointAddr::new(iroh::SecretKey::from_bytes(&[7; 32]).public()),
            user_public_key: user,
            account: "acct-1".into(),
        }
    }

    /// The session takes the identity it was handed, and nothing but this
    /// session's user may read any of it: the workspace's own agent has sudo in
    /// this VM, and what it can copy is one key for one record, for two hours.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_sessions_key_is_written_for_its_user_alone() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let identity = dir.path().join("identity");
        let config = dir.path().join("config");
        std::fs::create_dir_all(&config).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_IDENTITY_ROOT", &identity)
            .set("XDG_CONFIG_HOME", &config);
        let handed = material();
        take_identity(&handed).expect("the session took the identity it was handed");
        // Taken again, as a session restarted in the same VM takes it.
        take_identity(&handed).expect("the session took the identity it was handed again");
        let paths = tilo_identity::provisioning::collaboration_identity_paths();
        assert_eq!(
            tilo_device_identity::persist::read_device_cert(&paths.device_cert)
                .unwrap()
                .device_public_key,
            handed.device.device_public_key,
            "the device keeps the certificate the server issued it"
        );
        let mode =
            |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode(&paths.device_privkey),
            0o600,
            "the browser device's key is its user's alone"
        );
        assert_eq!(
            mode(&paths.user_pubkey),
            0o644,
            "the person's key is public"
        );
        for directory in [
            identity.join("device-private"),
            identity.join("user-identity"),
            identity.join("device"),
        ] {
            assert_eq!(
                mode(&directory),
                0o700,
                "{} is readable by this user alone",
                directory.display()
            );
        }
    }

    /// What the window of the session [`holding`] makes shows.
    const SHOWN: &str = "uuid:5c07c075-0000-4000-8000-0000000000aa";

    /// A session in a workspace's VM, holding the window `provider` and reporting into `said`.
    fn holding(provider: u64, said: &Arc<Mutex<Vec<String>>>) -> HostedBrowser {
        let reports = Arc::clone(said);
        let (sender, arrived) = std::sync::mpsc::channel();
        HostedBrowser {
            material: Material {
                workspace: "11111111-1111-4111-8111-111111111111".into(),
                workspace_id: WorkspaceId::from_u128(1),
                record: SHOWN.into(),
                device: tilo_user_identity::DeviceCert {
                    user_public_key: [1; 32],
                    device_public_key: [2; 32],
                    device_label: "Browser".into(),
                    capabilities: tilo_user_identity::DeviceCapabilities::member(),
                    issued_at_ns: 1,
                    expires_at_ns: None,
                    signature: [0; 64],
                    issuer: None,
                },
                secret: [0; 32],
                server: iroh::EndpointAddr::new(iroh::SecretKey::from_bytes(&[7; 32]).public()),
                user_public_key: [1; 32],
                account: "acct".into(),
            },
            reports: Box::new(move |report| {
                reports
                    .lock()
                    .expect("the reports are this test's")
                    .push(report.line());
            }),
            waking: None,
            holds: Holds::Browser,
            launches: Vec::new(),
            told_wake: Arc::default(),
            arrived,
            sender,
            broker: None,
            hosted: None,
            person: "person".into(),
            held: HashSet::new(),
            join: Join::Asking,
            window: Window::Open(provider),
            said_used: None,
            asks_host: false,
            governed: false,
            items: None,
            lends_items: false,
        }
    }

    /// A vault item's reference, made up.
    const ITEM: &str = "vault.item:0123456789abcdef0123456789abcdef";
    /// Where the made-up item is kept for.
    const ORIGIN: &str = "https://login.example.test";
    /// What the made-up item keeps: nobody's password, and it says so.
    const MADE_UP_TEXT: &str = "made-up item text, nobody's password";

    /// The workspace's server is stood in for by what it answers this device: a text sealed
    /// to it is opened for the fill, the site and the workspace it was sealed for and for no
    /// other, a question's words and a refusal's reason are carried, and nothing that is said
    /// holds the text.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one named scenario walked through in order"
    )]
    fn what_the_server_answers_an_items_use_is_opened_here_or_its_reason_is_carried() {
        use environment_device::{ItemUse, ItemUsed};
        let handed = material();
        let key = handed.key().unwrap();
        let device = key.verifying_key().to_bytes();
        let used = ItemUse {
            invocation: tilo_actions::InvocationId::new(),
            item: ITEM.into(),
            origin: ORIGIN.into(),
        };
        let sealed_for = |workspace: &str,
                          invocation: tilo_actions::InvocationId,
                          device: [u8; 32],
                          origin: &str,
                          into: &str|
         -> Result<Option<serde_json::Value>, Box<Unanswered>> {
            let bound = key_group::ReleasedFor {
                workspace,
                invocation: *invocation.0.as_bytes(),
                origin,
            };
            let sealed = key_group::seal_released_text(device, &bound, MADE_UP_TEXT).unwrap();
            Ok(Some(
                serde_json::to_value(ItemUsed::Released {
                    sealed: hex::encode(sealed),
                    into: into.to_owned(),
                })
                .unwrap(),
            ))
        };
        let here = handed.workspace.as_str();

        // Sealed to this device for this fill of this page: opened, with its kind of field.
        let reply = item_answer(
            here,
            &key,
            &used,
            sealed_for(here, used.invocation, device, ORIGIN, "password"),
        );
        assert!(matches!(
            &reply,
            ItemReply::ItemText { invocation, text, into: runtime_collab::ItemField::Password }
                if *invocation == used.invocation && text.as_str() == MADE_UP_TEXT
        ));
        assert!(!format!("{reply:?}").contains(MADE_UP_TEXT));

        // Sealed for another site, another page action, another device or another workspace,
        // or for a kind of field nobody knows: nothing is released, and the refusal holds
        // nothing of the text.
        let another = ed25519_dalek::SigningKey::from_bytes(&[9; 32])
            .verifying_key()
            .to_bytes();
        for answered in [
            sealed_for(
                here,
                used.invocation,
                device,
                "https://other.example.test",
                "password",
            ),
            sealed_for(
                here,
                tilo_actions::InvocationId::new(),
                device,
                ORIGIN,
                "password",
            ),
            sealed_for(here, used.invocation, another, ORIGIN, "password"),
            sealed_for(
                "22222222-2222-4222-8222-222222222222",
                used.invocation,
                device,
                ORIGIN,
                "password",
            ),
            sealed_for(here, used.invocation, device, ORIGIN, "elsewhere"),
            Ok(Some(
                serde_json::to_value(ItemUsed::Released {
                    sealed: "not hex".into(),
                    into: "password".into(),
                })
                .unwrap(),
            )),
        ] {
            let reply = item_answer(here, &key, &used, answered);
            assert!(matches!(
                &reply,
                ItemReply::ItemRefused { invocation, reason }
                    if *invocation == used.invocation
                        && reason.contains("nothing was filled")
                        && !reason.contains(MADE_UP_TEXT)
            ));
        }

        // A refusal comes back in the server's own words, however it was given.
        let no_fill = "No fill this server handed this window waits for that item";
        let said_no = "Ada said no, so the item is not used.";
        let late = "Nobody answered in time, so the item is not used.";
        for (answered, reason) in [
            (
                Err(Box::new(Unanswered::Refused(ActionCallError::Refused {
                    reason: no_fill.to_owned(),
                }))),
                no_fill,
            ),
            (
                Err(Box::new(Unanswered::Status(ReceiptStatus::Rejected {
                    error: ActionCallError::Refused {
                        reason: said_no.to_owned(),
                    },
                }))),
                said_no,
            ),
            (
                Err(Box::new(Unanswered::Status(ReceiptStatus::Rejected {
                    error: ActionCallError::Refused {
                        reason: late.to_owned(),
                    },
                }))),
                late,
            ),
            (
                Err(Box::new(Unanswered::Status(ReceiptStatus::Rejected {
                    error: ActionCallError::NotPermitted,
                }))),
                "may not invoke this action",
            ),
            (
                Err(Box::new(Unanswered::Status(ReceiptStatus::Cancelled {
                    message: None,
                }))),
                "stopped the ask",
            ),
            (Err(Box::new(Unanswered::Unreached)), "cannot reach"),
            (Ok(None), "answered nothing"),
            (
                Ok(Some(serde_json::json!({"neither": true}))),
                "no item's use",
            ),
        ] {
            let reply = item_answer(here, &key, &used, answered);
            assert!(
                matches!(
                    &reply,
                    ItemReply::ItemRefused { invocation, reason: said }
                        if *invocation == used.invocation && said.contains(reason)
                ),
                "a refusal lost its reason: {reason}"
            );
        }

        // That its person is asked first comes back in the words whoever asked is told.
        let words = "Ada is asked whether the assistant may sign in.";
        let reply = item_answer(
            here,
            &key,
            &used,
            Ok(Some(
                serde_json::to_value(ItemUsed::Asked {
                    words: words.to_owned(),
                })
                .unwrap(),
            )),
        );
        assert!(matches!(
            &reply,
            ItemReply::ItemAsked { invocation, words: said }
                if *invocation == used.invocation && said == words
        ));
    }

    /// An apps session reads each later line of its input as one app to start, wakes its loop
    /// for each, and starts nothing for a line that names no desktop file.
    #[test]
    fn an_apps_session_is_told_each_app_on_its_input() {
        let said: Arc<Mutex<Vec<String>>> = Arc::default();
        let mut hosted = holding(0, &said);
        hosted.holds = Holds::Apps;
        // Each wake comes after the app it is for arrived: the loop it wakes finds it.
        let (woke, wakes) = std::sync::mpsc::channel();
        hosted.waking(Arc::new(move || {
            let _ = woke.send(());
        }));
        let input = format!(
            "{}\nsh -c id\n{}\n",
            AppLaunch::new("xterm").unwrap().line(),
            AppLaunch::new("org.gnome.TextEditor").unwrap().line()
        );
        hosted.read_launches(std::io::Cursor::new(input.into_bytes()));
        let mut told = Vec::new();
        for _ in 0..2 {
            wakes
                .recv_timeout(Duration::from_secs(10))
                .expect("the loop is woken once for each app");
            loop {
                match hosted.arrived.try_recv() {
                    Ok(Arrived::Launch(launch)) => break told.push(launch.desktop_id),
                    Ok(_) => {}
                    Err(error) => panic!("no app had arrived when the loop woke: {error}"),
                }
            }
        }
        assert_eq!(told, ["xterm", "org.gnome.TextEditor"]);
    }

    /// A wake that counts itself in `woken` and stirs the thread that made it, which waits in
    /// [`woken_within`].
    fn counted(woken: &Arc<AtomicUsize>) -> tilo_service::Wake {
        let (woken, waiting) = (Arc::clone(woken), std::thread::current());
        Arc::new(move || {
            woken.fetch_add(1, Ordering::SeqCst);
            waiting.unpark();
        })
    }

    /// How often the loop was woken, once it was or once `limit` is over.
    fn woken_within(woken: &AtomicUsize, limit: Duration) -> usize {
        let began = Instant::now();
        loop {
            let times = woken.load(Ordering::SeqCst);
            let left = limit.saturating_sub(began.elapsed());
            if times > 0 || left.is_zero() {
                return times;
            }
            std::thread::park_timeout(left);
        }
    }

    /// Let every task a publication to the broker woke have its turn: the app's tasks are
    /// polled in the order they were woken, so one started now ends after them.
    fn settled() {
        let tasks = tilo_rt::try_app().expect("the app's tasks");
        tasks
            .block_on(tasks.spawn(async {}))
            .expect("this thread may wait")
            .expect("the task ended");
    }

    /// What `app` publishes for one window in `workspace`, as connection `peer_id`: `live`
    /// while the app is open.
    fn window_surface(
        peer_id: u64,
        app: &str,
        workspace: &str,
        live: bool,
    ) -> tilo_workspace_service::apps::collab_app_broker::PublishedSurface {
        tilo_workspace_service::apps::collab_app_broker::PublishedSurface {
            peer_id,
            app_id: app.to_owned(),
            route: tilo_workspace_service::apps::route::CrdtRoute::app(app, workspace)
                .on_resource(SHOWN),
            actions: Vec::new(),
            live,
        }
    }

    /// A session that waits for its Browser window is woken when its broker lists the
    /// window, so `up` is said on the pass that wake makes and not on the next one made for
    /// another reason. The wake is only a wake: it says nothing and hands the session
    /// nothing, `up` names the provider the broker lists, and what is not this session's
    /// window (another app's, Browser's in another workspace, a Browser that closed) wakes
    /// nobody.
    #[test]
    fn a_session_waiting_for_its_window_is_woken_when_its_broker_lists_it() {
        let said: Arc<Mutex<Vec<String>>> = Arc::default();
        let reports = || said.lock().expect("this test's reports").clone();
        let mut hosted = holding(0, &said);
        hosted.window = Window::Opening;
        let woken: Arc<AtomicUsize> = Arc::default();
        hosted.waking(counted(&woken));
        let broker = AppBrokerState::test_broker_with_idle_directory_queue();
        hosted.watching(Some(broker.state()), None, "person");
        let here = hosted.material.workspace.clone();

        // The broker lists no window yet: the pass finds none, and nothing is said.
        hosted.publish();
        assert!(
            reports().is_empty(),
            "no window is up before the broker lists one"
        );

        // Nothing that is not this session's window wakes it.
        broker.publish_surface_for_test(window_surface(4, "so.tilo.notes", &here, true));
        broker.publish_surface_for_test(window_surface(
            5,
            BROWSER,
            "22222222-2222-4222-8222-222222222222",
            true,
        ));
        broker.publish_surface_for_test(window_surface(6, BROWSER, &here, false));
        settled();
        assert_eq!(
            hosted.window_provider(),
            None,
            "none of them is this session's window"
        );
        assert_eq!(
            woken.load(Ordering::SeqCst),
            0,
            "what is not this session's window wakes nobody"
        );

        // The Browser the session started publishes: the broker lists its window, and the
        // loop is woken for it.
        broker.publish_surface_for_test(window_surface(7, BROWSER, &here, true));
        assert_eq!(
            hosted.window_provider(),
            Some(7),
            "the broker lists the window"
        );
        assert_eq!(
            woken_within(&woken, Duration::from_secs(10)),
            1,
            "the loop is woken when its broker lists the window"
        );
        assert!(reports().is_empty(), "the wake itself says nothing");
        assert!(
            hosted.arrived.try_recv().is_err(),
            "and it hands the session nothing"
        );

        // The pass that wake makes says the window is up, under the provider the broker lists.
        hosted.publish();
        let up = Report::Up {
            provider: "7".to_owned(),
        };
        assert_eq!(
            reports(),
            [up.line()],
            "the window is up under the provider its broker lists it by"
        );
        assert!(matches!(hosted.window, Window::Open(7)));
    }

    /// A session that holds a workspace's Linux apps waits for no window of Browser: one the
    /// broker lists in its workspace wakes nobody.
    #[test]
    fn an_apps_session_is_woken_for_no_window_of_browser() {
        let said: Arc<Mutex<Vec<String>>> = Arc::default();
        let mut hosted = holding(0, &said);
        hosted.holds = Holds::Apps;
        let woken: Arc<AtomicUsize> = Arc::default();
        hosted.waking(counted(&woken));
        let broker = AppBrokerState::test_broker_with_idle_directory_queue();
        hosted.watching(Some(broker.state()), None, "person");
        let here = hosted.material.workspace.clone();
        broker.publish_surface_for_test(window_surface(7, BROWSER, &here, true));
        settled();
        assert_eq!(
            woken.load(Ordering::SeqCst),
            0,
            "an apps session waits for no window of Browser"
        );
    }

    #[test]
    fn the_page_in_use_is_said_once_a_minute_while_it_is() {
        let said: Arc<Mutex<Vec<String>>> = Arc::default();
        let mut hosted = holding(42, &said);
        let reports =
            |said: &Arc<Mutex<Vec<String>>>| said.lock().expect("this test's reports").clone();
        hosted.used();
        assert_eq!(reports(&said), vec!["used".to_owned()], "the page was used");
        hosted.used();
        assert_eq!(
            reports(&said),
            vec!["used".to_owned()],
            "a page in use is said once a minute, not once an action"
        );
        // A minute later it is said again: a page being read for an hour is in use the whole time.
        hosted.said_used = Instant::now().checked_sub(USED_EVERY + Duration::from_secs(1));
        hosted.used();
        assert_eq!(
            reports(&said),
            vec!["used".to_owned(), "used".to_owned()],
            "the page is still in use"
        );
    }

    /// A page's catalogue row for the action `action`, `label`led, with the
    /// held request `held` bound to it when it answers one.
    fn page_row(
        action: &str,
        label: &str,
        held: Option<u32>,
    ) -> Option<tilo_actions::remote::CatalogueRow> {
        let mut page = tilo_actions::ResourceSpec::new(PAGE_RESOURCE, PAGE_RESOURCE, "Page");
        let mut spec = tilo_actions::ActionSpec::immediate_encoded(
            action,
            label,
            tilo_actions::ExecutionLocus::ClientLocal,
            tilo_actions::EncodedAction {
                type_id: action.to_owned(),
                payload: Vec::new(),
            },
        );
        if let Some(held) = held {
            spec = spec
                .with_bound(vec![
                    ("id".to_owned(), serde_json::Value::String(held.to_string())),
                    ("page".to_owned(), "https://orders.example".into()),
                    ("kind".to_owned(), "submit".into()),
                    ("method".to_owned(), "POST".into()),
                    ("destination".to_owned(), "https://orders.example".into()),
                    ("picture".to_owned(), "tilo.content:5f2b".into()),
                ])
                .stating_bound("id", "The held request's id")
                .stating_bound("page", "The page it leaves from")
                .stating_bound("kind", "What the page does")
                .stating_bound("method", "The method it leaves with")
                .stating_bound("destination", "Where it goes")
                .stating_bound("picture", "The filled page, as content");
        }
        page.actions.push(spec);
        let action = tilo_actions::api::survey(&[page]).into_iter().next()?;
        Some(tilo_actions::remote::CatalogueRow::for_action(
            "42", BROWSER, action,
        ))
    }

    tilo_test_env::scenario! {
        fn a_departure_the_window_holds_is_reported_and_answered_by_its_own_number;

        phase the_window_reports_the_departures_it_publishes;
        // The holds the window publishes are read off its catalogue: each one's
        // id, and what leaves. Nothing else the page publishes is a hold.
        {
            let page = CataloguePage {
                rows: [
                    page_row(
                        SEND_HELD,
                        "Let it leave: POST https://orders.example",
                        Some(4),
                    ),
                    page_row("back", "Back", None),
                ]
                .into_iter()
                .flatten()
                .collect(),
                ..CataloguePage::default()
            };
            let held = published_holds(&page);
            assert_eq!(held.len(), 1, "one departure waits on the person");
            assert_eq!(held[0].id, 4);
            assert!(
                held[0].describing.contains("orders.example"),
                "the person is told what leaves: {}",
                held[0].describing
            );
            assert_eq!(
                held[0].facts,
                [
                    ("page".to_owned(), "https://orders.example".to_owned()),
                    ("kind".to_owned(), "submit".to_owned()),
                    ("method".to_owned(), "POST".to_owned()),
                    (
                        "destination".to_owned(),
                        "https://orders.example".to_owned()
                    ),
                    ("picture".to_owned(), "tilo.content:5f2b".to_owned()),
                ],
                "and every fact the window states about it"
            );
        }

        phase two_departures_published_side_by_side_are_each_answered_by_their_own_number;
        // The window numbers what it holds itself, one number to a hold: a
        // request and a download the engine gave the same number are published
        // side by side under two. Each is read with its own number and is
        // answered by it, so the person's allow of one never reaches the other.
        {
            let page = CataloguePage {
                rows: [
                    page_row(
                        SEND_HELD,
                        "Let it leave: POST https://orders.example",
                        Some(1),
                    ),
                    page_row(
                        SEND_HELD,
                        "Let it leave: download from https://files.example (return.pdf)",
                        Some(2),
                    ),
                ]
                .into_iter()
                .flatten()
                .collect(),
                ..CataloguePage::default()
            };
            let held = published_holds(&page);
            assert_eq!(
                held.iter().map(|held| held.id).collect::<Vec<_>>(),
                [1, 2],
                "each departure is read under the number the window gave it"
            );
            assert!(held[1].describing.contains("return.pdf"));
            for held in &held {
                let request = answer(held.id, 42, &held.describing, &held.facts, SEND_HELD);
                assert_eq!(
                    request.argument,
                    InvokeArgument::Json(serde_json::Map::from_iter([(
                        "id".to_owned(),
                        held.id.to_string().into()
                    )])),
                    "and answered by that number and no other"
                );
                assert_eq!(
                    request.context.and_then(|context| context.purpose),
                    Some(held.describing.clone())
                );
            }
        }

        phase a_reported_hold_names_the_one_request_it_answers;
        // The invocation reported for a hold names the window, the action and
        // the id the window published it with: what the host asks the person
        // about and their answer runs. Kept instead, it names the same request
        // and needs nobody's word.
        {
            let request = answer(
                7,
                42,
                "POST https://orders.example",
                &[("method".to_owned(), "POST".to_owned())],
                SEND_HELD,
            );
            assert_eq!(request.target.provider, "42");
            assert_eq!(request.target.address.resource, PAGE_RESOURCE);
            assert_eq!(request.target.address.action, SEND_HELD);
            assert_eq!(
                request.argument,
                InvokeArgument::Json(serde_json::Map::from_iter([("id".to_owned(), "7".into())]))
            );
            assert_eq!(
                request
                    .context
                    .as_ref()
                    .and_then(|context| context.purpose.clone()),
                Some("POST https://orders.example".to_owned()),
                "the person is told what leaves"
            );
            assert_eq!(
                tilo_actions::remote::browser::departure_facts(request.context.as_ref()),
                [("method".to_owned(), "POST".to_owned())],
                "and what the window stated about the page it leaves from"
            );
            let kept = keep(request);
            assert_eq!(kept.target.address.action, KEEP_HELD);
            assert_eq!(kept.confirmation, None);
        }
    }
}
