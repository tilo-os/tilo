//! A session in a workspace's VM: one of the VM's daemon's shells, holding the workspace's
//! Browser window or its Linux apps' windows (`docs/hosted-browser.md`).
//!
//! A hosted workspace's VM runs one device daemon, logged in as a device of the workspace, and the
//! daemon starts the VM's sessions as its clients the first time the machine that runs the VM
//! launches something in one ([`tilo_actions::remote::environment_window`]). The session is told
//! what it holds by its environment ([`Holds::marked`], and the workspace it serves), and holds
//! nothing of its own: no identity, no join and no material. Everything it asks goes to its daemon
//! through the device's dispatch, as anything a person's session asks does:
//!
//! - it asks the daemon what was launched for it ([`wire::NEXT_LAUNCH`]), and opens each launch:
//!   Browser on a record, as a visitor, or a Linux app by its desktop file;
//! - it tells the daemon once its window is up, with the provider the workspace's dispatch lists
//!   Browser's window under, which every page action names, and that it is in use at most once a
//!   minute while somebody views the page or an app's window is open ([`wire::REPORT`]);
//! - what the VM holds no keys for it asks of the workspace's host through the daemon, which asks
//!   as the VM's device and opens what the host sealed to it: a vault item's text for one fill of
//!   a page ([`wire::USE_ITEM`]), and each share of its windows ([`HostGoverns`]).
//!
//! It does nothing else a person's session does on start: no onboarding, no Lounge, no other app.

use std::time::{Duration, Instant};

use tilo_actions::remote::environment_window::{
    self as wire, Holds, ItemLent, Launch, Report, Session, Share,
};
use tilo_actions::remote::linux_apps::{self, AppLaunch};
use tilo_actions::remote::{
    ActionProvider, Caller, CataloguePage, CatalogueQuery, InvokeArgument, InvokeRequest,
    ReceiptStatus, RemoteActionTarget, environment_device::ItemUse,
};

use tilo_actions::InvocationUpdate;

use crate::Shared;
use crate::app_wire::AppWire;

/// The app whose window a session holds.
pub use ui::apps::BROWSER;

/// How often the page's use is told while it is in use: one a viewer is what the daemon counts its
/// idle limit from, and a page being read for an hour is in use the whole time.
const USED_EVERY: Duration = Duration::from_secs(60);
/// How long the daemon is asked to hold an ask for the next launch before it answers none.
const NEXT_WAIT_MS: u32 = 25_000;
/// How long a call the daemon accepted is looked at again, where it cannot say when it moved.
const LOOK_EVERY: Duration = Duration::from_millis(250);
/// How long an ask the daemon could not answer waits before it is asked again.
const ASK_AGAIN: Duration = Duration::from_secs(1);
/// How long the window Browser opened is looked for in the workspace's listing.
const UP_PATIENCE: Duration = Duration::from_secs(60);

/// The device's dispatch, as this session reaches its daemon through it.
type Dispatch = std::sync::Arc<dyn ActionProvider + Send + Sync>;

/// What this process was started to hold, by the mark its daemon set: `None` for a session a
/// person starts.
fn marked() -> Option<Holds> {
    let set = |mark: &str| std::env::var_os(mark).is_some_and(|value| value == "1");
    [Holds::Apps, Holds::Browser]
        .into_iter()
        .find(|holds| set(holds.mark()))
}

/// Whether this session shares its windows with its workspace the moment they open: a session
/// in a workspace's VM runs for the workspace's members, and nobody sits at it.
#[must_use]
pub fn shares_by_default() -> bool {
    marked().is_some()
}

/// The session in a workspace's VM: what it holds, the launches it was told and the window it
/// opened.
pub struct HostedBrowser {
    holds: Holds,
    /// The workspace it serves, by its UUID: the one its daemon named, and the only one it opens
    /// anything in.
    workspace: String,
    /// Who sits here, as this session's own actions name them.
    person: String,
    /// Where it asks its daemon, once the session has the device's dispatch.
    dispatch: Option<Dispatch>,
    /// What wakes the session's loop when a launch arrives or the window is found.
    waking: Option<tilo_service::Wake>,
    arrived: std::sync::mpsc::Receiver<Arrived>,
    sender: std::sync::mpsc::Sender<Arrived>,
    /// Whether it asks its daemon for launches yet.
    asking: bool,
    /// Whether its window shares are written by the workspace's host yet.
    shares_through_host: bool,
    /// Whether Browser's vault items are asked of the host yet.
    lends_items: bool,
    /// The records Browser was launched on and has not opened yet.
    records: Vec<String>,
    /// The Linux apps it was told to start and has not started yet.
    launches: Vec<AppLaunch>,
    window: Window,
    /// When the page's use was last told.
    said_used: Option<Instant>,
}

/// What reached the loop from the tasks that ask the daemon.
enum Arrived {
    /// What was launched for this session.
    Launches(Vec<Launch>),
    /// The provider the workspace's dispatch lists the window under, or `None` when it listed none
    /// in time.
    Listed(Option<u64>),
}

/// The window the session holds.
#[derive(Debug, PartialEq, Eq)]
enum Window {
    /// Nothing was opened yet.
    Unopened,
    /// Browser was started on this record, and the window it opens is looked for.
    Opening(String),
    /// The window is listed under this provider, which page actions name. An apps session holds
    /// no one window, and is open from its first pass with the workspace held.
    Open(Option<u64>),
    /// Browser did not open, or its window was never listed, and why is in the log.
    Failed,
}

/// Writes this session's window shares through the workspace's host: it writes each share of this
/// device's windows, asked through the daemon.
struct HostShares {
    dispatch: Dispatch,
    caller: Caller,
}

impl crate::window_share::ShareSource for HostShares {
    fn share(&self, workspace: &str, app_id: &str, tab: u64, shared: bool) {
        if Some(workspace) != self.caller.workspace() {
            return;
        }
        let window = Share {
            app: app_id.to_owned(),
            tab,
            shared,
        };
        let request = request(wire::target(wire::SHARE_WINDOW), wire::argument(&window));
        let (dispatch, caller) = (std::sync::Arc::clone(&self.dispatch), self.caller.clone());
        let Ok(tasks) = tilo_rt::try_app() else {
            tracing::warn!(
                tab,
                "a window's share is not written: no task to ask the host on"
            );
            return;
        };
        tasks.spawn(async move {
            match settled(dispatch.as_ref(), &caller, request).await {
                Ok(_) => tracing::info!(tab, shared, "the host wrote this window's share"),
                Err(error) => {
                    tracing::warn!(%error, tab, shared, "the host did not write this window's share");
                }
            }
        });
    }
}

/// What Browser is answered for `used`: what the daemon opened of the host's answer, or why
/// nothing was released.
async fn item_reply(
    dispatch: Dispatch,
    caller: Caller,
    used: ItemUse,
) -> runtime_collab::BrowserServiceSessionReply {
    use runtime_collab::BrowserServiceSessionReply as Reply;
    let invocation = used.invocation;
    let asked = request(wire::target(wire::USE_ITEM), wire::argument(&used));
    let lent = settled(dispatch.as_ref(), &caller, asked)
        .await
        .and_then(|answer| {
            serde_json::from_value::<ItemLent>(answer).map_err(|error| error.to_string())
        });
    match lent {
        Ok(ItemLent::Text { text, into }) => {
            let text = zeroize::Zeroizing::new(text);
            match runtime_collab::ItemField::from_word(&into) {
                Some(into) => Reply::ItemText {
                    invocation,
                    text,
                    into,
                },
                None => Reply::item_refused(
                    invocation,
                    "The workspace's server named a kind of field this session does not know, \
                     so nothing was filled.",
                ),
            }
        }
        Ok(ItemLent::Asked { words }) => Reply::item_asked(invocation, &words),
        Ok(ItemLent::Refused { reason }) => Reply::item_refused(invocation, &reason),
        Err(error) => {
            tracing::warn!(%error, "the daemon did not answer a fill's ask for its item");
            Reply::item_refused(
                invocation,
                "This session's machine did not answer for the item, so nothing was filled.",
            )
        }
    }
}

impl HostedBrowser {
    /// The session holding what `holds` names in `workspace`.
    #[must_use]
    pub fn new(holds: Holds, workspace: &str) -> Self {
        let (sender, arrived) = std::sync::mpsc::channel();
        Self {
            holds,
            workspace: workspace.to_owned(),
            person: String::new(),
            dispatch: None,
            waking: None,
            arrived,
            sender,
            asking: false,
            shares_through_host: false,
            lends_items: false,
            records: Vec::new(),
            launches: Vec::new(),
            window: Window::Unopened,
            said_used: None,
        }
    }

    /// Ask the daemon through `dispatch` as `person` from now on: what the session's app wire
    /// hands it ([`Self::wired`]), or a test's own provider in the daemon's place.
    #[must_use]
    pub fn asking(mut self, dispatch: Dispatch, person: &str) -> Self {
        self.dispatch = Some(dispatch);
        person.clone_into(&mut self.person);
        self
    }

    /// Wake the session's loop with `wake` when a launch arrives or the window is listed.
    pub(crate) fn waking(&mut self, wake: tilo_service::Wake) {
        self.waking = Some(wake);
    }

    /// Take the device's dispatch from the session's app wire, as `person`'s device: where it asks
    /// its daemon. A dispatch it was handed already stays.
    pub(crate) fn wired(&mut self, app_wire: Option<&AppWire>, person: &str) {
        // Browser's asks for vault items are this session's to answer for its workspace, each
        // asked of the host through the daemon.
        if self.holds == Holds::Browser
            && let Some(app_wire) = app_wire
        {
            let items = app_wire.item_lending();
            items.hold(&self.workspace);
            let wired = app_wire
                .dispatch
                .clone()
                .map(|dispatch| dispatch as Dispatch);
            self.lend_items(&items, wired, person);
        }
        if self.dispatch.is_some() {
            return;
        }
        let Some(dispatch) = app_wire.and_then(|wire| wire.dispatch.clone()) else {
            tracing::warn!("a hosted session has no dispatch: its daemon handed it no link");
            return;
        };
        self.dispatch = Some(dispatch);
        person.clone_into(&mut self.person);
    }

    /// Have Browser's vault items asked of the host through `dispatch` (or the one this session
    /// was handed) as `person`, once.
    fn lend_items(
        &mut self,
        items: &crate::item_lending::ItemLending,
        wired: Option<Dispatch>,
        person: &str,
    ) {
        if self.lends_items {
            return;
        }
        let (dispatch, person) = match (&self.dispatch, wired) {
            (Some(handed), _) => (std::sync::Arc::clone(handed), self.person.clone()),
            (None, Some(wired)) => (wired, person.to_owned()),
            (None, None) => return,
        };
        self.lends_items = true;
        let caller = Caller::local_person(person).in_workspace(self.workspace.clone());
        items.lent_by(std::sync::Arc::new(
            move |used: ItemUse| -> crate::item_lending::Lending {
                Box::pin(item_reply(
                    std::sync::Arc::clone(&dispatch),
                    caller.clone(),
                    used,
                ))
            },
        ));
    }

    /// Who this device acts as: the person, in the workspace the session serves.
    fn caller(&self) -> Caller {
        Caller::local_person(self.person.clone()).in_workspace(self.workspace.clone())
    }

    /// What the session does each pass: take what arrived, ask the daemon for launches, and once
    /// the shell holds the workspace open each one and tell what became of it.
    pub(crate) fn follow(&mut self, shared: &Shared) {
        self.take_arrived();
        self.ask_for_launches();
        self.share_through_host(shared);
        if !held(shared, &self.workspace) {
            return;
        }
        match self.holds {
            Holds::Browser => {
                self.open_records(shared);
                if let Window::Open(Some(provider)) = &self.window
                    && shared.pages.borrow().viewed().contains(provider)
                {
                    self.used();
                }
            }
            Holds::Apps => self.follow_apps(shared),
        }
    }

    fn take_arrived(&mut self) {
        let arrived: Vec<Arrived> = self.arrived.try_iter().collect();
        for arrived in arrived {
            match arrived {
                Arrived::Launches(launches) => {
                    for launch in launches {
                        if launch.holds != self.holds || launch.workspace != self.workspace {
                            tracing::warn!(
                                ?launch,
                                "the daemon launched something this session does not hold"
                            );
                            continue;
                        }
                        self.records.extend(launch.record);
                        self.launches.extend(launch.app);
                    }
                }
                Arrived::Listed(Some(provider)) => {
                    if matches!(self.window, Window::Opening(_)) {
                        self.window = Window::Open(Some(provider));
                        self.tell(Report::Up {
                            holds: Holds::Browser,
                            provider: provider.to_string(),
                        });
                    }
                }
                Arrived::Listed(None) => {
                    if let Window::Opening(record) = &self.window {
                        tracing::warn!(%record, "the window Browser opened was never listed");
                        self.window = Window::Failed;
                    }
                }
            }
        }
    }

    /// Ask the daemon what was launched for this session, from now on: one ask at a time, each
    /// held by the daemon until something is launched or its wait is over, on a task of its own.
    fn ask_for_launches(&mut self) {
        if self.asking {
            return;
        }
        let (Some(dispatch), Ok(tasks)) = (self.dispatch.clone(), tilo_rt::try_app()) else {
            return;
        };
        self.asking = true;
        let caller = self.caller();
        let session = Session { holds: self.holds };
        let arrived = self.sender.clone();
        let waking = self.waking.clone();
        tasks.spawn(async move {
            loop {
                let mut asked = request(wire::target(wire::NEXT_LAUNCH), wire::argument(&session));
                asked.wait_ms = Some(NEXT_WAIT_MS);
                let launches = match settled(dispatch.as_ref(), &caller, asked).await {
                    Ok(value) => serde_json::from_value::<Vec<Launch>>(value).unwrap_or_else(|error| {
                        tracing::warn!(%error, "the daemon answered no launches");
                        Vec::new()
                    }),
                    Err(error) => {
                        tracing::warn!(%error, "the daemon did not answer this session's ask for launches");
                        tokio::time::sleep(ASK_AGAIN).await;
                        Vec::new()
                    }
                };
                if launches.is_empty() {
                    // The daemon holds an ask until something is launched: one answered empty at
                    // once is asked again after a look, never in a loop that takes the thread.
                    tokio::time::sleep(LOOK_EVERY).await;
                    continue;
                }
                if arrived.send(Arrived::Launches(launches)).is_err() {
                    return;
                }
                if let Some(wake) = &waking {
                    wake();
                }
            }
        });
    }

    /// Have the workspace's host write this session's window shares, before any window is shared:
    /// a share the session wrote itself would be dropped.
    fn share_through_host(&mut self, shared: &Shared) {
        if self.shares_through_host {
            return;
        }
        let Some(dispatch) = self.dispatch.clone() else {
            return;
        };
        self.shares_through_host = true;
        shared
            .window_shares
            .borrow_mut()
            .shares_from(std::rc::Rc::new(HostShares {
                dispatch,
                caller: self.caller(),
            }));
    }

    /// Open Browser on each record it was launched on, as the visitor an agent works it as, and
    /// look for the first window in the workspace's listing.
    fn open_records(&mut self, shared: &Shared) {
        let Some(apps) = &shared.launch else {
            return;
        };
        let workspace = apps.workspace_named(&self.workspace);
        for record in std::mem::take(&mut self.records) {
            // The window is opened for the agent that works it, not for the person: it browses as
            // any visitor would, on a fresh profile of its own, so an agent's page actions take no
            // authority over the person's sign-ins, and what acts on the page still asks them.
            let launch = tilo_app_launch::Launch::Record {
                app: BROWSER.to_owned(),
                record: record.clone(),
                visitor: true,
            };
            match apps.start(&launch, &workspace) {
                Ok(()) => {
                    if matches!(self.window, Window::Unopened | Window::Failed) {
                        self.look_for_window(&record);
                        self.window = Window::Opening(record);
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, %record, "the session's Browser did not open");
                    if matches!(self.window, Window::Unopened) {
                        self.window = Window::Failed;
                    }
                }
            }
        }
    }

    /// Look for the window Browser opens on `record` in the workspace's listing, until the
    /// dispatch lists it or the patience is over: what is told as [`Arrived::Listed`].
    fn look_for_window(&self, record: &str) {
        let (Some(dispatch), Ok(tasks)) = (self.dispatch.clone(), tilo_rt::try_app()) else {
            return;
        };
        let caller = self.caller();
        let (workspace, record) = (self.workspace.clone(), record.to_owned());
        let arrived = self.sender.clone();
        let waking = self.waking.clone();
        tasks.spawn(async move {
            let began = Instant::now();
            let query = CatalogueQuery {
                app_id: Some(BROWSER.to_owned()),
                max_results: 0,
                ..CatalogueQuery::default()
            };
            let listed = loop {
                if let Some(Ok(page)) = dispatch.query(&caller, query.clone()).await
                    && let Some(provider) = window_of(&page, &workspace, &record)
                {
                    break Some(provider);
                }
                if began.elapsed() >= UP_PATIENCE {
                    break None;
                }
                tokio::time::sleep(LOOK_EVERY).await;
            };
            if arrived.send(Arrived::Listed(listed)).is_ok()
                && let Some(wake) = &waking
            {
                wake();
            }
        });
    }

    /// What an apps session does each pass: say it is up, start each app it was told to, and say
    /// it is used while any window of theirs is open.
    fn follow_apps(&mut self, shared: &Shared) {
        if self.window == Window::Unopened {
            self.window = Window::Open(None);
            self.tell(Report::Up {
                holds: Holds::Apps,
                provider: linux_apps::UP_PROVIDER.to_owned(),
            });
        }
        let Some(apps) = &shared.launch else {
            return;
        };
        let workspace = apps.workspace_named(&self.workspace);
        for launch in std::mem::take(&mut self.launches) {
            // What runs is what the guest's desktop file says: the same launch as an app a person
            // opens from their own computer's applications.
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
        if holds_windows(shared, &self.workspace) {
            self.used();
        }
    }

    /// Tell the daemon the session is in use, no more than once a minute while it is.
    fn used(&mut self) {
        let now = Instant::now();
        if self
            .said_used
            .is_some_and(|said| now.duration_since(said) < USED_EVERY)
        {
            return;
        }
        self.said_used = Some(now);
        self.tell(Report::Used { holds: self.holds });
    }

    /// Tell the daemon `report`, on a task: nothing of the pass waits for its answer.
    fn tell(&self, report: Report) {
        let (Some(dispatch), Ok(tasks)) = (self.dispatch.clone(), tilo_rt::try_app()) else {
            tracing::warn!(
                ?report,
                "the daemon is not told: this session has no dispatch"
            );
            return;
        };
        let caller = self.caller();
        tasks.spawn(async move {
            let told = request(wire::target(wire::REPORT), wire::argument(&report));
            if let Err(error) = settled(dispatch.as_ref(), &caller, told).await {
                tracing::warn!(%error, ?report, "the daemon was not told");
            }
        });
    }
}

impl crate::Shared {
    /// The session follows its launches and its window each pass: this is all it does of what a
    /// person's session does.
    pub(crate) fn follow_hosted_browser(&self) {
        if let Some(hosted) = &mut *self.hosted_browser.borrow_mut() {
            hosted.follow(self);
        }
    }
}

/// The session its daemon started, when its daemon says so: what it holds by the mark, and the
/// workspace it serves.
///
/// # Errors
///
/// It was marked, and named no workspace.
pub fn handed() -> Result<Option<HostedBrowser>, String> {
    let Some(holds) = marked() else {
        return Ok(None);
    };
    let workspace = std::env::var(wire::WORKSPACE)
        .ok()
        .filter(|workspace| !workspace.is_empty())
        .ok_or_else(|| {
            format!(
                "the session's daemon named no workspace in {}",
                wire::WORKSPACE
            )
        })?;
    Ok(Some(HostedBrowser::new(holds, &workspace)))
}

/// An invocation of `target` with `argument`.
fn request(target: RemoteActionTarget, argument: InvokeArgument) -> InvokeRequest {
    let mut request = InvokeRequest::plain(target, serde_json::Map::new());
    request.argument = argument;
    request
}

/// Invoke `request` through `dispatch` as `caller` and follow it until it ended: the value it
/// answered, or why it did not.
async fn settled(
    dispatch: &(dyn ActionProvider + Send + Sync),
    caller: &Caller,
    request: InvokeRequest,
) -> Result<serde_json::Value, String> {
    let receipt = dispatch
        .invoke(caller, request)
        .await
        .ok_or_else(|| "the daemon answered nothing".to_owned())?
        .map_err(|error| error.to_string())?;
    match receipt.status {
        ReceiptStatus::Completed { output } => return Ok(output.value.unwrap_or_default()),
        ReceiptStatus::Accepted => {}
        ReceiptStatus::Rejected { error } => return Err(error.to_string()),
        ReceiptStatus::Failed { error } => return Err(error.message),
        ReceiptStatus::Cancelled { message } => {
            return Err(message.unwrap_or_else(|| "it was cancelled".to_owned()));
        }
    }
    loop {
        match dispatch
            .observe(caller, &receipt.invocation)
            .map(|state| state.update)
        {
            Some(InvocationUpdate::Succeeded { output, .. }) => {
                return Ok(output.value.unwrap_or_default());
            }
            Some(InvocationUpdate::Failed { error, .. }) => return Err(error.message),
            Some(InvocationUpdate::Cancelled { .. }) => return Err("it was cancelled".to_owned()),
            None => return Err("the daemon no longer has the call".to_owned()),
            Some(_) => {}
        }
        match dispatch.next_change(caller, &receipt.invocation) {
            Some(change) => {
                let _moved = change.await;
            }
            None => tokio::time::sleep(LOOK_EVERY).await,
        }
    }
}

/// The provider `page` lists Browser's window under in `workspace`, the one that shows `record`
/// when one does: what a session tells its window as. `None` while it lists no such window.
fn window_of(page: &CataloguePage, workspace: &str, record: &str) -> Option<u64> {
    page.providers
        .iter()
        .filter(|listed| listed.app_id == BROWSER && listed.workspace == workspace)
        .filter_map(|listed| {
            Some((
                listed.document_id == record,
                listed.provider.parse::<u64>().ok()?,
            ))
        })
        .max()
        .map(|(_, provider)| provider)
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
    use std::sync::{Arc, Mutex};
    use tilo_actions::remote::{CatalogueProvider, TestProvider};

    const WORKSPACE: &str = "11111111-1111-4111-8111-111111111111";
    const SHOWN: &str = "uuid:5c07c075-0000-4000-8000-0000000000aa";

    fn listed(provider: &str, workspace: &str, document: &str) -> CatalogueProvider {
        CatalogueProvider {
            provider: provider.into(),
            app_id: BROWSER.into(),
            workspace: workspace.into(),
            document_id: document.into(),
            browsable: false,
        }
    }

    /// The window a session tells is Browser's in its workspace, the one showing its record when
    /// there are several: never another workspace's, and never a provider that is no number.
    #[test]
    fn the_window_told_is_browsers_in_the_workspace_showing_the_record() {
        let page = |providers: Vec<CatalogueProvider>| CataloguePage {
            providers,
            ..CataloguePage::default()
        };
        assert_eq!(window_of(&page(Vec::new()), WORKSPACE, SHOWN), None);
        assert_eq!(
            window_of(
                &page(vec![
                    listed("5", "22222222-2222-4222-8222-222222222222", SHOWN),
                    listed("so.tilo.browser", WORKSPACE, SHOWN),
                ]),
                WORKSPACE,
                SHOWN
            ),
            None,
            "another workspace's window, or a provider that is no window, is not this one"
        );
        assert_eq!(
            window_of(
                &page(vec![
                    listed("9", WORKSPACE, "other"),
                    listed("7", WORKSPACE, SHOWN)
                ]),
                WORKSPACE,
                SHOWN
            ),
            Some(7),
            "the window showing the record"
        );
        assert_eq!(
            window_of(
                &page(vec![listed("9", WORKSPACE, "other")]),
                WORKSPACE,
                SHOWN
            ),
            Some(9),
            "and Browser's window when none shows it yet"
        );
    }

    /// The session asks its daemon what was launched for it and takes each launch it holds, and
    /// tells its daemon it is in use once a minute, not once an action.
    #[test]
    fn the_session_takes_its_launches_and_says_it_is_used_once_a_minute() {
        let answered = Arc::new(Mutex::new(false));
        let answering = Arc::new(TestProvider::new().completing({
            let answered = Arc::clone(&answered);
            move |request| {
                let value = match request.target.address.action.as_str() {
                    wire::NEXT_LAUNCH
                        if !std::mem::replace(&mut *answered.lock().unwrap(), true) =>
                    {
                        serde_json::to_value(vec![
                            Launch {
                                workspace: WORKSPACE.into(),
                                holds: Holds::Browser,
                                record: Some(SHOWN.into()),
                                app: None,
                            },
                            Launch {
                                workspace: "22222222-2222-4222-8222-222222222222".into(),
                                holds: Holds::Browser,
                                record: Some("elsewhere".into()),
                                app: None,
                            },
                        ])
                        .unwrap()
                    }
                    _ => serde_json::json!([]),
                };
                tilo_actions::ActionOutput {
                    value: Some(value),
                    ..tilo_actions::ActionOutput::default()
                }
            }
        }));
        let daemon: Dispatch = Arc::clone(&answering) as Dispatch;
        let mut hosted = HostedBrowser::new(Holds::Browser, WORKSPACE).asking(daemon, "person");
        let (woke, wakes) = std::sync::mpsc::channel();
        hosted.waking(Arc::new(move || {
            let _ = woke.send(());
        }));
        let _tasks = tilo_rt::try_app().expect("the app's tasks");
        hosted.ask_for_launches();
        wakes
            .recv_timeout(tilo_test_env::hang_bound())
            .expect("the loop is woken for what was launched");
        hosted.take_arrived();
        assert_eq!(
            hosted.records,
            [SHOWN.to_owned()],
            "the launch in its workspace is taken, and one in another is not"
        );

        hosted.used();
        hosted.used();
        hosted.said_used = Instant::now().checked_sub(USED_EVERY + Duration::from_secs(1));
        hosted.used();
        let told = || {
            answering
                .asked()
                .iter()
                .filter_map(|request| match &request.argument {
                    InvokeArgument::Json(named)
                        if request.target.address.action == wire::REPORT =>
                    {
                        serde_json::from_value::<Report>(named.clone().into()).ok()
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let began = Instant::now();
        while told().len() < 2 && began.elapsed() < tilo_test_env::hang_bound() {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            told(),
            [
                Report::Used {
                    holds: Holds::Browser
                },
                Report::Used {
                    holds: Holds::Browser
                }
            ],
            "in use is told once a minute while it is"
        );
        assert!(
            answering
                .asked()
                .iter()
                .all(|request| request.target.provider == wire::SESSIONS),
            "everything is asked of the provider the daemon answers its sessions under"
        );
    }
}
