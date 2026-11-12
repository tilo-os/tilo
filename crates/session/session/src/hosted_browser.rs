//! A session in a workspace's VM: the device, the workspace and the window its host handed
//! it (`docs/hosted-browser.md`, "What the session in the VM is handed").
//!
//! The session in a workspace's VM is started at the first browser action, so it
//! holds nothing of its own: the relay placement record on its standard input is the workspace it
//! joins, the record whose Browser window it opens, and the device certificate and key it joins
//! as. It is a device of the workspace like any other — the relay lists it, and its window is
//! published, subscribed and acted on as a window on anyone's computer is — and it does nothing
//! else a person's session does on start: no onboarding, no Lounge, no other app.
//!
//! The session reports what it became on its standard output, one [`Report`] per line: [`READY`]
//! once the identity it was handed is the one it runs as, [`UP`] with the window's provider once
//! the window publishes, and [`USED`] whenever the page is used. The host that started it counts
//! its idle limit from that, and hands the provider to the relay, which routes page actions to it.
//!
//! The window also holds departures nobody asked for — a page posting on its own, a download it
//! started — and nobody stands in front of it to allow them. The session reports each one the
//! window publishes ([`SEND_HELD`], by the id it is published with) to the relay's device, which
//! puts it to the person and answers with their word: their allow runs that one request as their
//! own, and a denial runs [`KEEP_HELD`] instead, leaving the page as it was filled.
//!
//! [`READY`]: tilo_actions::remote::browser::READY
//! [`UP`]: tilo_actions::remote::browser::UP
//! [`USED`]: tilo_actions::remote::browser::USED
//! [`SEND_HELD`]: tilo_actions::remote::browser::SEND_HELD
//! [`KEEP_HELD`]: tilo_actions::remote::browser::KEEP_HELD

use std::collections::HashSet;
use std::io::{self, BufRead as _, Write as _};
use std::time::{Duration, Instant};

use tilo_action_broker::collab_app_broker::{AppBrokerState, ExecutionStatus};
use tilo_action_broker::services::HostedWorkspaces;
use tilo_actions::remote::browser::{MARK, Report, SEND_HELD};
use tilo_actions::remote::{
    ActionHost as _, ActionHostError, Caller, CataloguePage, CatalogueQuery, ConfirmedInvocation,
    InvokeArgument, InvokeRequest, RemoteActionTarget,
};
use tilo_actions::selection::logical_resource_id;
use tilo_relay_protocol::hosted::Material;

use crate::Shared;

/// The app whose window a session holds.
pub const BROWSER: &str = "so.tilo.browser";
/// The resource the window's page actions are actions of.
const PAGE_RESOURCE: &str = "browser.page";
/// The page action that drops a held request the person did not allow.
const KEEP_HELD: &str = tilo_actions::remote::browser::KEEP_HELD;
/// The most held requests one page of the window's catalogue is read for:
/// the window itself bounds what it holds at once, so one page names them all.
const HELD_PAGE: u32 = 64;
/// How long the invitation the relay answers the join with stays redeemable.
const ENTER_INVITATION_SECONDS: u64 = 600;
/// How often the page's use is reported while it is in use: one an action or a viewer is what the
/// host counts its idle limit from, and a page being read for an hour is in use the whole time.
const USED_EVERY: Duration = Duration::from_secs(60);
/// The colour the record of the person a session serves carries, as a device that joined
/// without their root gets it.
const ENVIRONMENT_AVATAR: [u8; 3] = [88, 101, 242];

/// What the session reports on its standard output: the host relays every line.
pub type Reports = Box<dyn FnMut(Report) + Send>;

/// The session in a workspace's VM: the identity it was handed, the join it asked of its relay,
/// and the window it holds.
pub struct HostedBrowser {
    /// The relay's placement record: what this session joins, opens and is.
    material: Material,
    reports: Reports,
    /// What wakes the session's loop when the relay answers or the page is used.
    waking: Option<tilo_service::Wake>,
    arrived: std::sync::mpsc::Receiver<Arrived>,
    sender: std::sync::mpsc::Sender<Arrived>,
    /// The session's broker, while it serves one: what the window publishes,
    /// and what the requests it holds are answered through.
    broker: Option<std::sync::Arc<AppBrokerState>>,
    /// Where the workspace's host is asked about a page: the relay's device,
    /// which puts the person's answer to them.
    hosted: Option<std::sync::Arc<dyn HostedWorkspaces>>,
    /// Who sits here, as this session's own actions name them.
    person: String,
    /// The held requests the relay has been told about, by the id the window
    /// published them with: each is put to the person once.
    held: HashSet<u32>,
    join: Join,
    window: Window,
    /// When the page's use was last reported.
    said_used: Option<Instant>,
}

/// What reached the loop from the tasks that watch the relay and the desk.
enum Arrived {
    /// The relay answered the join: the invitation text to redeem, or why it did not.
    Invitation(Result<String, String>),
    /// An action ran on the window's page.
    PageAction,
    /// What the window holds for the person now, as it published it.
    Held(Vec<Held>),
    /// What the person answered about a held request: their word for it, or
    /// why they did not give one, and the invocation it answers.
    Answered {
        request: Box<InvokeRequest>,
        answer: Box<Result<ConfirmedInvocation, ActionHostError>>,
    },
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
    /// The relay has not been asked yet.
    Waiting,
    /// The relay was asked, and what it answers is on its way: the invitation is redeemed by the
    /// session's own join, which settles the workspace on the desk.
    Asking,
}

/// The window the session holds.
enum Window {
    /// Nothing was opened: the desk does not hold the workspace yet.
    Unopened,
    /// The launch was made, and no window published for it yet.
    Opening,
    /// The window publishes under this provider, which page actions name: its tab, one number.
    Open(u64),
    /// Browser did not open, and why is in the log: nothing asks again.
    Failed,
}

impl HostedBrowser {
    /// The session, joining as the device `material`, the relay's placement record
    /// names: its identity is taken here, before anything reads this device of itself, and `ready`
    /// is reported. `reports` is where the session says what it became.
    ///
    /// # Errors
    ///
    /// The material is not the relay's, or the identity it names is not one this device may take.
    pub fn take(material: &str, reports: Reports) -> Result<Self, String> {
        let material: Material = serde_json::from_str(material.trim())
            .map_err(|error| format!("the material is not the relay's: {error}"))?;
        take_identity(&material)?;
        let (sender, arrived) = std::sync::mpsc::channel();
        let mut hosted = Self {
            material,
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
        };
        hosted.say(Report::Ready);
        Ok(hosted)
    }

    /// Wake the session's loop with `wake` when the relay answers the join or the page is used.
    pub(crate) fn waking(&mut self, wake: tilo_service::Wake) {
        self.waking = Some(wake);
    }

    /// Follow what the session's broker admits: an action on the window's page is the page in use,
    /// and the page track's viewers say so too ([`Self::follow`]). `hosted` is where the workspace's
    /// host is asked about a departure the window holds, and `person` who this seat acts as.
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

    /// What the session does each pass: redeem the invitation the relay answered with, open the
    /// record's window once the desk holds the workspace, and report what became of it.
    pub(crate) fn follow(&mut self, shared: &Shared) {
        self.take_arrived(shared);
        self.join();
        self.open(shared);
        if let Window::Open(provider) = &self.window
            && shared.pages.borrow().viewed().contains(provider)
        {
            self.used();
        }
        self.publish(shared);
    }

    /// Redeem what the relay answered the join with: the invitation text through the session's own
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
                    tracing::warn!(%reason, "the relay did not let this session into the workspace");
                }
                Arrived::PageAction => self.used(),
                Arrived::Held(held) => self.report(held),
                Arrived::Answered {
                    mut request,
                    answer,
                } => match *answer {
                    // The person allowed that one request: it runs as their
                    // own, carrying their word, and leaves once.
                    Ok(confirmed) => {
                        confirmed.apply(&mut request);
                        self.run(&request);
                    }
                    // A denial keeps the page as it was filled.
                    // ignored-failure: it drops the held request, as it says.
                    Err(_) => self.run(&keep(*request)),
                },
            }
        }
    }

    /// Ask its relay to enter the device into the workspace, once: as the device the
    /// material certifies, it answers with an invitation, which the session redeems.
    fn join(&mut self) {
        if !matches!(self.join, Join::Waiting) {
            return;
        }
        self.join = Join::Asking;
        let Ok(runtime) = tilo_rt::try_app() else {
            tracing::warn!("the workspace is not joined: no task to ask the relay on");
            return;
        };
        let material = self.material.clone();
        let arrived = self.sender.clone();
        let waking = self.waking.clone();
        runtime.spawn(async move {
            let answer = invitation(&material).await;
            if arrived.send(Arrived::Invitation(answer)).is_err() {
                return;
            }
            if let Some(wake) = &waking {
                wake();
            }
        });
    }

    /// Open the record's Browser window, once the desk holds the workspace the material names.
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

    /// Report the window's provider once it publishes: the window instance the desk shows the app
    /// in, which is what every page action on it names.
    fn publish(&mut self, shared: &Shared) {
        if !matches!(self.window, Window::Opening) {
            return;
        }
        let Some(provider) = tab_of(shared, &self.material.workspace, BROWSER) else {
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

    /// Ask the workspace's host about each departure the window holds that
    /// nobody has been asked about yet: it puts them to the person, and their
    /// answer comes back as [`Arrived::Answered`].
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
            host.confirm(&caller, &request).then(move |answer| {
                let answered = Arrived::Answered {
                    request: Box::new(request),
                    answer: Box::new(
                        answer.unwrap_or(Err(ActionHostError::confirmation_required())),
                    ),
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

    /// Run `request` on the window as the person at this seat: the page action
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

    /// Who this seat is, as the session's own actions name it: the person in
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
    if std::env::var_os(MARK).is_none() {
        return Ok(None);
    }
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|error| format!("the material was not read from the session's input: {error}"))?;
    if line.trim().is_empty() {
        return Err("the session was handed no material".to_owned());
    }
    HostedBrowser::take(&line, Box::new(|report| say_on_stdout(&report))).map(Some)
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
/// relay issued for it, where every part of the session that reads this device of itself reads it.
fn take_identity(material: &Material) -> Result<(), String> {
    let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
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
    tilo_user_identity::persist::write_signing_key(&paths.device_privkey, &key)
        .map_err(|error| format!("this device's key was not written: {error}"))?;
    tilo_user_identity::install_device_cert(&paths, &material.device, tilo_clock::unix_now_ns())
        .map_err(|error| format!("this device's certificate was not taken: {error}"))?;
    // The person the workspace belongs to, as a record this device holds of them: the relay hands
    // over their public key, and every part of the session that presents this device's person in a
    // workspace reads that person from this login's identity records. The record carries no root:
    // what signs here is the device certificate the relay issued.
    let directory = runtime_identity::identity_dir_from_env()
        .map_err(|error| format!("this device's identity directory is not known: {error}"))?;
    let record = runtime_identity::public_member_record(
        runtime_identity::UNNAMED_DISPLAY_NAME,
        ENVIRONMENT_AVATAR,
        material.user_public_key,
    )
    .map_err(|error| format!("the person's record was not written: {error}"))?;
    let user = hex::encode(material.user_public_key);
    let id = format!("identity-{}", user.get(..55).unwrap_or(&user));
    if let Err(error) = runtime_identity::import_identity(&directory, &id, &record)
        && error.kind() != io::ErrorKind::AlreadyExists
    {
        return Err(format!("the person's record was not taken: {error}"));
    }
    Ok(())
}

/// Ask its relay to enter the session's device into the workspace, the way a member
/// device asks to enter a workspace its relay hosts, and answer the invitation text it redeems.
async fn invitation(material: &Material) -> Result<String, String> {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .map_err(|error| format!("the session reached no network: {error}"))?;
    let joined = async {
        let client = tilo_relay_session::RelayClient::connect(
            endpoint.clone(),
            material.relay.clone(),
            material.user_public_key,
            material.device.clone(),
            material.key()?,
            tilo_user_identity::RevocationList::new(),
        )
        .await
        .map_err(|error| format!("the session could not reach its relay: {error}"))?;
        tilo_relay_session::workspaces::enter_invitation(
            &client,
            &material.workspace,
            ENTER_INVITATION_SECONDS,
        )
        .await
    }
    .await;
    endpoint.close().await;
    joined
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

/// Whether the desk holds `workspace`.
fn held(shared: &Shared, workspace: &str) -> bool {
    shared.handle.read(|tree| {
        tree.workspaces
            .iter()
            .any(|held| held.id().to_string() == workspace)
    })
}

/// The tab `app`'s window takes in `workspace`, once the desk holds one: the window's instance,
/// which is the provider its actions are published under.
fn tab_of(shared: &Shared, workspace: &str, app: &str) -> Option<u64> {
    shared.handle.read(|tree| {
        tree.workspaces
            .iter()
            .find(|held| held.id().to_string() == workspace)
            .and_then(|held| {
                held.strips
                    .iter()
                    .flat_map(|strip| strip.window_groups.iter())
                    .flat_map(|group| group.tabs.iter())
                    .find(|tab| {
                        tilo_desktop_model::apps::canonical_app_id(&tab.tile)
                            .is_some_and(|shown| shown == app)
                    })
            })
            .map(|tab| tab.id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// What a relay hands a session: a device of the workspace's person that
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
            workspace_id: 7,
            record: "browser-1".into(),
            device,
            secret,
            relay: iroh::EndpointAddr::new(iroh::SecretKey::from_bytes(&[7; 32]).public()),
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
        take_identity(&material()).expect("the session took the identity it was handed");
        let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
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

    /// A session in a workspace's VM, holding the window `provider` and reporting into `said`.
    fn holding(provider: u64, said: &Arc<Mutex<Vec<String>>>) -> HostedBrowser {
        let reports = Arc::clone(said);
        let (sender, arrived) = std::sync::mpsc::channel();
        HostedBrowser {
            material: Material {
                workspace: "11111111-1111-4111-8111-111111111111".into(),
                workspace_id: 1,
                record: "uuid:5c07c075-0000-4000-8000-0000000000aa".into(),
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
                relay: iroh::EndpointAddr::new(iroh::SecretKey::from_bytes(&[7; 32]).public()),
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
            arrived,
            sender,
            broker: None,
            hosted: None,
            person: "person".into(),
            held: HashSet::new(),
            join: Join::Asking,
            window: Window::Open(provider),
            said_used: None,
        }
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
        let action = tilo_actions::door::survey(&[page]).into_iter().next()?;
        Some(tilo_actions::remote::CatalogueRow::for_action(
            "42", BROWSER, action,
        ))
    }

    /// The holds the window publishes are read off its catalogue: each one's
    /// id, and what leaves. Nothing else the page publishes is a hold.
    #[test]
    fn the_window_reports_the_departures_it_publishes() {
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

    /// The invocation reported for a hold names the window, the action and
    /// the id the window published it with: what the host asks the person
    /// about and their answer runs. Kept instead, it names the same request
    /// and needs nobody's word.
    #[test]
    fn a_reported_hold_names_the_one_request_it_answers() {
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
