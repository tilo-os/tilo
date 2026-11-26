//! The walk of the published tree: from the workspace root to the resource an address names,
//! over whatever provider serves it.
//!
//! The root is the workspace. Its children are what publishes there: each app (running or
//! installed), the workspace's own resources (`workspace`), the host's services, the
//! workspace's folder (`files`) and its invocations (`invocations`), each with the provider
//! that serves it. Below a child, a name is resolved against that provider's live directory,
//! level by level, as deep as its resources nest. Every caller resolves addresses here: the MCP
//! tools, the REST API and the command line, each over its own [`ActionProvider`].

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tilo_lock::MutexRecover as _;

use super::level::read_on;
use super::{
    Address, Level, Node, Segment, TypeNames, app_meant, is_program_form, level_of, path_name,
    pick, rank,
};
use crate::ActionDescriptor;
use crate::InvocationId;
use crate::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationState,
};

/// Resource rows one directory level reads at most, across its pages.
pub const LEVEL_PAGES: usize = 4;
/// Times a name not found is looked for again, after pauses that double from 5 ms to 200 ms:
/// about seven tenths of a second in all.
const LOOKS_AGAIN: u32 = 8;
/// The moment those looks come to. A level that was read on past its bound is looked through
/// again only within it: one that takes longer to read whole is not read whole eight times.
const LOOKS_MOMENT: Duration = Duration::from_millis(700);
/// Rows asked per page.
pub const PAGE_ROWS: u32 = 128;
/// Containers at an app's top level whose contents the app's root also
/// offers: a notebook's pages are the app's own.
pub const ROOT_PEEK: usize = 3;
/// Containers a name that is not where it was looked for reads on, one by
/// one, before it is called missing.
pub const DEEP_READS: usize = 16;
/// Longest name asked of an app whole; a longer one is a key or an id.
const NAME_BYTES: usize = 256;
/// Resources a listing's first page holds: what a level read near is read to.
pub const LISTED_NEAR: usize = 40;
/// The longest pause between two reads of a directory level that keeps
/// changing while it is read.
pub const RESTART_PAUSE_MAX: Duration = Duration::from_millis(200);
/// Pages of installed apps read at most: a page holds 256.
const INSTALLED_PAGES: usize = 4;
/// How long a level that keeps moving on, or that its app cannot read now, is read again.
pub const LEVEL_WAIT: Duration = Duration::from_secs(5);
/// How long starting an app is followed, over as many waits as it takes.
pub const START_WAIT: Duration = Duration::from_secs(60);
/// How long one wait for an invocation to settle lasts when its caller names none.
pub const SETTLE_WAIT: Duration = Duration::from_secs(5);
/// Paths and apps a tree keeps resolved at most: a caller naming ever new paths does not grow
/// them without end.
pub const RESOLVED_PATHS: usize = 256;
/// The resolved paths a tree keeps for its listings, at most: a page of any listing reads the
/// one its cursor names, and a caller of thousands of anything holds only these.
const LISTING_RESOLUTIONS: usize = 32;
/// The app that shows the workspace's folder: the folder is every workspace's own, served
/// by the framework rather than installed into it.
pub const FILES_APP: &str = "so.tilo.file-explorer";
/// How long a catalogue page is waited for, and a workspace that refused it as not serving
/// yet: a server that restarted serves one again within a minute, and a call that gave up
/// sooner sent its caller after evidence it never had.
pub const ANSWER_WAIT: Duration = Duration::from_secs(60);

/// How a tree asks its provider for a catalogue page, when not by the provider's own
/// `query`: a bridge on a shared runtime asks off that runtime's thread.
pub type Asker<P> = Box<
    dyn Fn(
            &P,
            &Caller,
            CatalogueQuery,
        ) -> crate::remote::Pending<Result<CataloguePage, ActionCallError>>
        + Send
        + Sync,
>;

/// `future`'s output, or none once `wait` passed first.
pub async fn within<F: Future>(pause: &dyn Pause, wait: Duration, future: F) -> Option<F::Output> {
    Within {
        future: Box::pin(future),
        timer: pause.pause(wait),
    }
    .await
}

struct Within<F> {
    future: Pin<Box<F>>,
    timer: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl<F: Future> Future for Within<F> {
    type Output = Option<F::Output>;

    fn poll(
        mut self: Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if let std::task::Poll::Ready(output) = self.future.as_mut().poll(context) {
            return std::task::Poll::Ready(Some(output));
        }
        self.timer.as_mut().poll(context).map(|()| None)
    }
}

/// How a tree waits: a pause between two reads of a directory that keeps moving, and the
/// clock its deadlines are measured on. The walk itself runs on no executor of its own, so
/// it builds wherever the provider does (a browser page included).
pub trait Pause: Send + Sync {
    /// Resolves once `how_long` has passed.
    fn pause(&self, how_long: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>>;
    /// Where this clock stands, from an origin of its own: only differences are read.
    fn now(&self) -> Duration;
}

/// The directory levels one call read, by the resource each holds.
pub type Levels = HashMap<String, Level>;

/// A path resolved against the live directory, kept for the pages that list on from it.
pub type ListingResolution = Arc<Resolved>;

/// Why an address did not resolve, or a call on it did not start. Each says what happened in
/// the words a caller is answered with ([`fmt::Display`]); a caller that tells a refusal apart
/// reads the variant.
#[derive(Clone, Debug)]
pub enum Unresolved {
    /// The provider refused a query asked on the way.
    Asked(ActionCallError),
    /// The provider dropped a query without answering it.
    Stopped,
    /// A directory kept moving on while it was read.
    KeptChanging,
    /// An app cannot read its directory right now.
    CannotListNow,
    /// What a query named could not be listed, and why.
    Unavailable(String),
    /// No resource under `at` goes by `name`; `has` are the addresses of those it lists.
    NoSuch {
        name: String,
        at: String,
        has: Vec<String>,
    },
    /// The app `name` is installed but runs nowhere; `opens` when the caller may start it.
    NotRunning { name: String, opens: bool },
    /// The caller may not start the app `name`.
    MayNotOpen { name: String },
    /// No running app is named `name`, and the installed ones could not be read.
    InstalledUnread {
        name: String,
        reason: Box<Unresolved>,
    },
    /// Starting `app_id` did not end in its running: how it ended instead, and the code its
    /// start failed with (empty when it did not fail).
    NotStarted {
        app_id: String,
        why: String,
        code: String,
    },
    /// A name the walk could not place, said whole (one several resources or apps carry, a
    /// window of another app, an action no resource offers).
    Said(String),
}

impl Unresolved {
    /// Whether this is no refusal but what the address names, said: an app that is
    /// installed and not running. A listing answers it as its page.
    #[must_use]
    pub const fn is_note(&self) -> bool {
        matches!(self, Self::NotRunning { .. })
    }

    /// Whether this says the app that was asked has gone — its provider stopped answering, or
    /// answered that what it served is unavailable — so what a session kept of it is stale. A
    /// decision about a name, such as no resource by it, is not.
    #[must_use]
    pub const fn app_gone(&self) -> bool {
        matches!(
            self,
            Self::Stopped | Self::Asked(ActionCallError::Unavailable { .. })
        )
    }

    /// What this says, without the code it starts with.
    #[must_use]
    pub fn words(&self) -> String {
        let said = self.to_string();
        match said.split_once(": ") {
            Some((code, rest))
                if !code.is_empty()
                    && code
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte == b'_') =>
            {
                rest.to_owned()
            }
            _ => said,
        }
    }
}

impl From<String> for Unresolved {
    fn from(said: String) -> Self {
        Self::Said(said)
    }
}

impl From<&str> for Unresolved {
    fn from(said: &str) -> Self {
        Self::Said(said.to_owned())
    }
}

impl From<ActionCallError> for Unresolved {
    fn from(refused: ActionCallError) -> Self {
        Self::Asked(refused)
    }
}

impl fmt::Display for Unresolved {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Asked(refused) => out.write_str(&refused.said()),
            Self::Stopped => out.write_str("the desktop's actions stopped answering"),
            Self::KeptChanging => {
                out.write_str("the app's directory kept changing while it was read; try again")
            }
            Self::CannotListNow => {
                out.write_str("the app cannot list what it has right now; try again")
            }
            Self::Unavailable(reason) | Self::Said(reason) => out.write_str(reason),
            Self::NoSuch { name, at, has } if has.is_empty() => {
                write!(out, "no {name} in {at}; it lists nothing here")
            }
            Self::NoSuch { name, at, has } => {
                write!(out, "no {name} in {at}; it has: {}", list_names(has))
            }
            Self::NotRunning { name, opens: true } => write!(
                out,
                "not_found: {name} is installed but not running; start it with tilo_do {}#open, or run one of its actions and it starts first",
                path_name(name)
            ),
            Self::NotRunning { name, opens: false } => write!(
                out,
                "not_found: {name} is installed but not running, and {}",
                may_not_open(name)
            ),
            Self::MayNotOpen { name } => out.write_str(&may_not_open(name)),
            Self::InstalledUnread { name, reason } => write!(
                out,
                "no running app is named {name}, and the installed apps could not be read: {}",
                reason.words()
            ),
            // A start refused for a reason the same call is asked again for (the workspace
            // not serving the caller yet, or not answering) keeps that code before its words,
            // so a caller reading them is told to ask again rather than that nothing is there.
            Self::NotStarted { app_id, why, code } if ActionCallError::ask_again_code(code) => {
                write!(out, "{code}: {app_id} did not start: {why}")
            }
            Self::NotStarted { app_id, why, .. } => write!(out, "{app_id} did not start: {why}"),
        }
    }
}

impl std::error::Error for Unresolved {}

/// Why an installed app this caller may not open stays closed, and who changes that.
fn may_not_open(name: &str) -> String {
    format!(
        "this agent may not open {name}: the person decides which apps an agent may use in this workspace"
    )
}

/// How many names an error lists before it stops, the rest counted.
#[must_use]
pub fn list_names(names: &[String]) -> String {
    let mut shown: Vec<&str> = names
        .iter()
        .take(super::NAMES_LISTED)
        .map(String::as_str)
        .collect();
    let more = names.len().saturating_sub(super::NAMES_LISTED);
    let tail = format!("… +{more}");
    if more > 0 {
        shown.push(&tail);
    }
    shown.join(", ")
}

/// [`list_names`], or `none` for no names: an empty list reads as a sentence cut short.
#[must_use]
pub fn list_or_none(names: &[String]) -> String {
    if names.is_empty() {
        "none".to_owned()
    } else {
        list_names(names)
    }
}

/// The pause before the `read`th read of a directory level that keeps changing: none before
/// the second, then doubling from 5 ms to [`RESTART_PAUSE_MAX`].
#[must_use]
pub fn restart_pause(read: u32) -> Duration {
    let Some(doublings) = read.checked_sub(2) else {
        return Duration::ZERO;
    };
    Duration::from_millis(5)
        .checked_mul(1_u32.checked_shl(doublings).unwrap_or(u32::MAX))
        .map_or(RESTART_PAUSE_MAX, |pause| pause.min(RESTART_PAUSE_MAX))
}

/// Whether `id` is a chain's resource: the head of the items an app keeps as one list.
#[must_use]
pub fn is_chain(id: &str) -> bool {
    id.strip_prefix("chain:")
        .is_some_and(|document| !document.is_empty())
}

/// The child of the workspace root a path starts with, as it was written (`browser@4711`). A
/// device's settings section (`devices/<id>/settings/power`) is the settings app's, as a person's
/// (`settings/appearance`) is; any other resource a device keeps of its own names its app right
/// after the device (`devices/<id>/<app>/…`), as a root each device keeps its own does. No app is
/// called `devices`.
#[must_use]
pub fn app_of(parsed: &Address) -> String {
    if device_section(parsed) {
        return tilo_app_id::apps::SETTINGS.to_owned();
    }
    if is_device_resource(parsed)
        && let Some(app) = parsed.segments.get(2)
    {
        return app.written();
    }
    parsed
        .segments
        .first()
        .map(Segment::written)
        .unwrap_or_default()
}

/// Whether `parsed` is a resource a device keeps of its own (`devices/<id>/…`).
#[must_use]
pub fn is_device_resource(parsed: &Address) -> bool {
    parsed
        .segments
        .first()
        .is_some_and(|first| first.written() == "devices")
}

/// Whether `parsed` names a device's settings section, which its app finds by its whole address.
fn device_section(parsed: &Address) -> bool {
    parsed
        .segments
        .first()
        .is_some_and(|first| first.written() == "devices")
        && crate::remote::settings_resources::is_section(&parsed.resource())
}

/// The segments of a path below the child of the root it starts with.
#[must_use]
pub fn below_app(parsed: &Address) -> &[Segment] {
    parsed.segments.get(1..).unwrap_or_default()
}

/// Whether a segment of `parsed` below its root child names a type the way a program writes
/// one (`tasks.Task`): only then are a program's type names asked for.
#[must_use]
pub fn names_a_type(parsed: &Address) -> bool {
    below_app(parsed)
        .iter()
        .any(|segment| is_program_form(&segment.written()))
}

/// Whether `name` names Files, every workspace's own folder: its name, the last part of its id,
/// or its id, in any case and however its words are parted (`file_explorer`).
#[must_use]
pub fn names_files(name: &str) -> bool {
    let name = super::folded(name);
    ["files", "file-explorer", FILES_APP]
        .iter()
        .any(|files| name == super::folded(files))
}

/// The app a name beginning with a kind names (`tasks.tracker` is the Tasks
/// app's tracker), when the name is one kind and no app: a kind is the app's
/// own name and one more word, and an item's kind names its collection after
/// that (`tasks.issues.issues`, `tasks.issues.links`), where a package's id is
/// a domain and its names.
#[must_use]
pub fn type_address(name: &str) -> Option<&str> {
    let (app, kind) = name.split_once('.')?;
    (!app.is_empty() && !kind.is_empty() && !kind.starts_with('.') && !kind.ends_with('.'))
        .then_some(app)
}

/// The running app `name` names among `running`: its id whole, or the last part of it.
///
/// # Errors
/// No running app is named so, or several are.
pub fn app_named(name: &str, running: &[&str]) -> Result<String, String> {
    if running.contains(&name) {
        return Ok(name.to_owned());
    }
    let mut by_tail = running
        .iter()
        .filter(|id| {
            id.rsplit('.')
                .next()
                .is_some_and(|tail| tail.eq_ignore_ascii_case(name))
        })
        .collect::<Vec<_>>();
    by_tail.dedup();
    match by_tail.as_slice() {
        [one] => Ok((**one).to_owned()),
        [] => Err(not_running(name, running)),
        many => Err(format!(
            "{name} names several running apps: {}; name one",
            many.iter().map(|id| **id).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// Why `app_id` cannot be addressed: what is running instead, under the
/// name to use for each, and how to start an app that is not.
#[must_use]
pub fn not_running(app_id: &str, running: &[&str]) -> String {
    let mut ids = running.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let listed = if ids.is_empty() {
        "no app is running".to_owned()
    } else {
        format!(
            "running: {}",
            ids.iter()
                .map(|id| match id.rsplit_once('.') {
                    Some((_, tail)) => format!("{id} ({tail})"),
                    None => (*id).to_owned(),
                })
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "{app_id} is not running ({listed}); name a running app by its id, or start one with tilo_do <app>#open"
    )
}

/// Whether `provider` is a window of an app, or a record a device serves whoever has it open
/// (a host serving its workspace's records): the app, live, as opposed to a service of the
/// host's that answers for an app id.
#[must_use]
pub fn is_window(provider: &str) -> bool {
    provider.parse::<u64>().is_ok() || crate::remote::is_resource_provider(provider)
}

/// The published tree of one workspace as one caller walks it, over `P`.
///
/// It keeps what it read for the calls after: the directory levels of each provider, the
/// app each name means and the paths its listings resolved, each bounded. What it kept is
/// checked by the provider when a call reaches it, so a stale one costs a refusal, never a
/// wrong write; a refusal drops what was kept of that app ([`Self::forget_app`]).
pub struct Tree<P> {
    provider: P,
    caller: Caller,
    workspace: String,
    pause: Arc<dyn Pause>,
    asker: Option<Asker<P>>,
    /// The directory levels read, by the provider that serves them and the resource they hold.
    dirs: Mutex<HashMap<String, Levels>>,
    /// Where each name was found: the provider a call found or started the app on, the app an
    /// installed name means, or a decision no grant can change — never what this caller may
    /// open, which the workspace decides on every call.
    apps: Mutex<HashMap<String, Looked>>,
    /// The paths listings resolved, by the path as written and the directory revision they
    /// were resolved at.
    listings: Mutex<HashMap<(String, String), ListingResolution>>,
}

impl<P> Tree<P> {
    /// The tree of `workspace` as `caller` walks it over `provider`, pausing with `pause`.
    pub fn new(provider: P, caller: Caller, workspace: String, pause: Arc<dyn Pause>) -> Self {
        Self {
            provider,
            caller,
            workspace,
            pause,
            asker: None,
            dirs: Mutex::default(),
            apps: Mutex::default(),
            listings: Mutex::default(),
        }
    }

    /// The tree asking its catalogue pages with `asker` rather than the provider's `query`.
    #[must_use]
    pub fn asking(mut self, asker: Asker<P>) -> Self {
        self.asker = Some(asker);
        self
    }

    /// The provider every call of this tree goes to.
    pub const fn provider(&self) -> &P {
        &self.provider
    }

    /// Who walks it.
    pub const fn caller(&self) -> &Caller {
        &self.caller
    }

    /// The workspace it is the tree of.
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    /// How it waits.
    pub fn pause(&self) -> &Arc<dyn Pause> {
        &self.pause
    }

    /// The resolution `path` was listed by at the directory revision `revision`, when this
    /// tree listed one: a later page reads by it instead of resolving the path again.
    pub fn listing_resolution(&self, path: &str, revision: &str) -> Option<ListingResolution> {
        if revision.is_empty() {
            return None;
        }
        self.listings
            .lock_recover()
            .get(&(path.to_owned(), revision.to_owned()))
            .cloned()
    }

    /// Keep the resolution `path` was listed by at `revision`, for the page that reads on
    /// from it. The newest [`LISTING_RESOLUTIONS`] are kept; what was listed before those is
    /// resolved again when it is listed again.
    pub fn remember_listing_resolution(
        &self,
        path: &str,
        revision: &str,
        resolved: &ListingResolution,
    ) {
        if revision.is_empty() {
            return;
        }
        let mut kept = self.listings.lock_recover();
        if kept.len() >= LISTING_RESOLUTIONS {
            kept.clear();
        }
        kept.insert((path.to_owned(), revision.to_owned()), Arc::clone(resolved));
    }
}

impl<P: ActionProvider + Sync + Clone + Send + 'static> Tree<P> {
    /// The provider's word on `invocation`, read off a runtime's worker
    /// ([`observed_off_the_worker`](crate::remote::observed_off_the_worker)): a provider that
    /// waits on a device must not wait on the worker the answer needs.
    pub(super) async fn observed(&self, invocation: &InvocationId) -> Option<InvocationState> {
        crate::remote::observed_off_the_worker(
            self.provider.clone(),
            self.caller.clone(),
            *invocation,
        )
        .await
    }

    /// One catalogue page asked of the provider as this tree's caller, waited for until it
    /// answers, for at most [`ANSWER_WAIT`]. A workspace that just started opens the documents
    /// a listing reads before it answers, and an ask given up sooner left the caller waiting
    /// for an answer it was already sent. A workspace a server is not serving yet refuses the
    /// ask and takes it once it does: its session moving is the signal to ask again.
    ///
    /// # Errors
    /// The provider refused it, did not answer in time, or dropped it unanswered.
    pub async fn ask(&self, query: CatalogueQuery) -> Result<CataloguePage, Unresolved> {
        let until = self.pause.now().saturating_add(ANSWER_WAIT);
        let left = || until.saturating_sub(self.pause.now());
        loop {
            let asked = match &self.asker {
                Some(asker) => asker(&self.provider, &self.caller, query.clone()),
                None => self.provider.query(&self.caller, query.clone()),
            };
            // No answer is not an answer that nothing is there: the caller hears it went
            // unanswered, never that the path names nothing.
            let answer = match within(&*self.pause, left(), asked).await {
                None => return Err(Unresolved::Asked(ActionCallError::Unanswered)),
                Some(None) => return Err(Unresolved::Stopped),
                Some(Some(answer)) => answer,
            };
            if matches!(answer, Err(ActionCallError::NotReady))
                && let Some(moved) = self.provider.session_state_changed(&self.caller)
                && within(&*self.pause, left(), moved).await.is_some()
            {
                continue;
            }
            return answer.map_err(Unresolved::Asked);
        }
    }

    /// Every publisher this caller may browse in its workspace, each by its provider id with
    /// what it is: an application's window (a numeric peer) or one of the host's own services
    /// (`tilo.desktop`), by the app id it answers for. Host services first, then windows
    /// oldest first, so the newest window of an app is its last entry. With them, the
    /// revision the provider answered at.
    ///
    /// # Errors
    /// The provider could not be asked.
    pub async fn providers(&self) -> Result<(String, Vec<(String, Value)>), Unresolved> {
        let page = self
            .ask(CatalogueQuery {
                max_results: 0,
                ..CatalogueQuery::default()
            })
            .await?;
        let mut listed = page
            .providers
            .into_iter()
            .filter(|provider| provider.browsable && provider.workspace == self.workspace)
            .map(|provider| {
                let peer = provider.provider.parse::<u64>().ok();
                let mut entry = serde_json::json!({
                    "provider": provider.provider,
                    "app_id": provider.app_id,
                    "document_id": provider.document_id,
                });
                if let (Some(peer), Some(fields)) = (peer, entry.as_object_mut()) {
                    fields.insert("peer_id".to_owned(), serde_json::json!(peer));
                    fields.insert(
                        "uri".to_owned(),
                        serde_json::json!(format!("tilo://apps/{peer}")),
                    );
                }
                (provider.provider, entry)
            })
            .collect::<Vec<_>>();
        listed.sort_by_key(|(provider, _)| (provider.parse::<u64>().ok(), provider.clone()));
        Ok((page.revision, listed))
    }
}

/// What an app name names.
#[derive(Clone)]
pub enum AppRef {
    /// A running publisher of the app: its newest window, the window the
    /// path named (`browser@4711`, `pinned`), or the host's own service
    /// answering for it (`desktop`).
    Running {
        app_id: String,
        provider: String,
        pinned: bool,
    },
    /// `opens` when this caller may start it.
    Installed { app_id: String, opens: bool },
}

impl AppRef {
    /// The app's id, as the workspace names it.
    pub fn app_id(&self) -> &str {
        match self {
            Self::Running { app_id, .. } | Self::Installed { app_id, .. } => app_id,
        }
    }
}

/// What looking a name up came to, as this tree keeps it.
///
/// Only where an app was found or started is kept, and a decision about a name that is no
/// permission answer (no app by it, or several). What this caller may open is never kept: the
/// workspace decides that on every call, so a grant withdrawn since the last call is refused by
/// the next one, and one given since is not refused from here.
#[derive(Clone)]
enum Looked {
    /// The app runs on this provider: a call found it there, or a start this session made put it
    /// there.
    Found(AppRef),
    /// The name means this installed app. Where the app is, never what this caller may do with
    /// it: every call that starts it is admitted by the workspace as it makes it.
    Named(String),
    /// A decision about the name that no grant can change.
    Refused(Unresolved),
}

/// The one resource an already-read level names exactly as `name`: its id
/// first, then its label. `None` for a name several resources carry, or one
/// none of them does.
fn named_among(level: &Level, name: &str) -> Option<Node> {
    let children = level.whole_children();
    if let [only] = children
        .iter()
        .filter(|node| node.names_id(name))
        .collect::<Vec<_>>()
        .as_slice()
    {
        return Some((*only).clone());
    }
    match children
        .iter()
        .filter(|node| node.answers_to(name) == Some(0))
        .collect::<Vec<_>>()
        .as_slice()
    {
        [only] => Some((*only).clone()),
        _ => None,
    }
}

/// An app installed for the workspace, as the workspace lists it.
pub struct InstalledApp {
    pub app_id: String,
    pub label: String,
    /// Whether this caller may open it: the workspace names an app it may
    /// not open without the action.
    pub opens: bool,
    /// What its signed package publishes (`surface.toml`), as the workspace's
    /// listing carries it: the types an agent reads before the app opens.
    pub surface: Option<String>,
}

impl InstalledApp {
    /// The app as naming it answers: its newest window `window` while it
    /// runs, else the app to open.
    pub fn named(&self, window: Option<String>) -> AppRef {
        let app_id = self.app_id.clone();
        match window {
            Some(provider) => AppRef::Running {
                app_id,
                provider,
                pinned: false,
            },
            None => AppRef::Installed {
                app_id,
                opens: self.opens,
            },
        }
    }
}

/// A path resolved against the live directory.
pub struct Resolved {
    pub app_id: String,
    pub provider: String,
    /// Written back as a path: the app and the names down to the target.
    pub path: String,
    /// The target resource; `None` for the app itself.
    pub node: Option<Node>,
    /// Actions on the target (the app's root actions for the app itself).
    pub actions: Vec<ActionDescriptor>,
    /// Its children (the app's resources for the app itself).
    pub children: Vec<Node>,
    /// The nodes this resolution passed through, root first.
    pub chain: Vec<Node>,
    /// The directory levels the resolution read, which what the call does
    /// next (finding the action that reads the target) reads again from
    /// here rather than asking the app twice.
    pub levels: Levels,
    /// The directory revision the target was found at.
    pub revision: String,
}

/// Whether `page` says of itself that it is not all there is and cannot be gone on from: it
/// stopped at a limit, is not the end and offers no cursor. An app answers so when its
/// directory cannot be read now and when a page of it broke off.
pub fn cannot_be_read_now(page: &crate::remote::CataloguePage) -> bool {
    page.budget_exhausted && !page.exhausted && page.next_cursor.is_empty()
}

impl<P: ActionProvider + Sync + Clone + Send + 'static> Tree<P> {
    /// One directory level: the resources under `parent` and their actions,
    /// over as many pages as a bounded read takes.
    pub async fn level(
        &self,
        provider: &str,
        parent: &str,
        levels: &mut Levels,
    ) -> Result<Level, Unresolved> {
        if let Some(read) = levels.get(parent) {
            return Ok(read.clone());
        }
        let read = self
            .read_level(provider, parent, None, usize::MAX)
            .await?
            .ok_or("the app's directory kept changing while it was read; try again")?;
        levels.insert(parent.to_owned(), read.clone());
        Ok(read)
    }

    /// One directory level, read as far as one page of a listing holds:
    /// enough to name a resource and what it holds on its first page, and
    /// extended by whoever pages past it ([`Self::level_reaching`],
    /// [`Self::level_holding`]). A listing reads one page of a directory of
    /// thousands per call, not a fixed stretch of it.
    pub async fn level_near(
        &self,
        provider: &str,
        parent: &str,
        levels: &mut Levels,
    ) -> Result<Level, Unresolved> {
        if let Some(read) = levels.get(parent) {
            return Ok(read.clone());
        }
        let read = self
            .read_level(provider, parent, None, LISTED_NEAR)
            .await?
            .ok_or("the app's directory kept changing while it was read; try again")?;
        levels.insert(parent.to_owned(), read.clone());
        Ok(read)
    }

    /// The directory levels this session read in `provider`, as the last call
    /// left them: what a call resolving a path there starts from.
    pub fn levels_kept(&self, provider: &str) -> Levels {
        self.dirs
            .lock_recover()
            .get(provider)
            .cloned()
            .unwrap_or_default()
    }

    /// Keep what a call resolving its path in `provider` read, for the calls
    /// after it: a write on another of the app's resources is resolved from
    /// what the workspace already answered.
    pub fn keep_levels(&self, provider: &str, levels: &Levels) {
        let mut dirs = self.dirs.lock_recover();
        if dirs.len() >= RESOLVED_PATHS && !dirs.contains_key(provider) {
            dirs.clear();
        }
        dirs.insert(provider.to_owned(), levels.clone());
    }

    /// Forget the app `app_id` the session found, after the workspace refused a
    /// call as one the app does not offer: the app or its directory moved
    /// since, so the next call looks both up again. What the workspace decided
    /// about other names — a name several apps carry, or none — is dropped with
    /// it: the catalogue moved, and the calls that follow read it once more.
    pub fn forget_app(&self, app_id: &str) {
        self.apps.lock_recover().retain(|_, looked| match looked {
            Looked::Found(app) => app.app_id() != app_id,
            // The binding is where the app was looked up: an app that moved is looked up again.
            Looked::Named(known) => known != app_id,
            Looked::Refused(_) => false,
        });
        self.dirs.lock_recover().clear();
    }

    /// The app `name` means: a running one by its id or last part, else an
    /// installed one the workspace offers to open.
    ///
    /// Where a name was found is kept: every call in an app names it again, and
    /// looking it up asks the workspace which apps run and which are installed
    /// — two catalogue reads, for each call. So a running app is answered from
    /// here after the first call, as a name that means an installed app is, and
    /// so is a start this session made ([`Self::saw_running`]) and a decision
    /// about a name no grant can change. What this caller may open is not kept:
    /// an app that is not known to run here is looked up on every call, so a
    /// grant given or withdrawn is decided by the call it reaches. A call the
    /// app refuses drops what was kept ([`McpServer::forget_app`]), so an app
    /// that moved is looked up again.
    pub async fn app_ref(&self, name: &str) -> Result<AppRef, Unresolved> {
        let known = {
            let apps = self.apps.lock_recover();
            match apps.get(name) {
                Some(Looked::Found(app)) => Some(Ok(app.clone())),
                Some(Looked::Refused(why)) => Some(Err(why.clone())),
                // Where the name's app runs, if a call in this session found it running.
                Some(Looked::Named(app_id)) => apps.get(app_id).and_then(|found| match found {
                    Looked::Found(app) => Some(Ok(app.clone())),
                    _ => None,
                }),
                None => None,
            }
        };
        if let Some(known) = known {
            return known;
        }
        let looked;
        let answer = match self.app_ref_live(name).await {
            // Where the app runs, and nothing this caller may do with it.
            Ok(app @ AppRef::Running { .. }) => {
                looked = Looked::Found(app.clone());
                Ok(app)
            }
            // Where the name's app is installed. What this caller may open is not kept: every
            // call that starts the app is admitted by the workspace as it makes it.
            Ok(app @ AppRef::Installed { .. }) => {
                looked = Looked::Named(app.app_id().to_owned());
                Ok(app)
            }
            // A decision about the name that no grant can change: several apps carry it, or no
            // app answers to it. Kept, as a running app is. What could not be read now — a
            // catalogue that did not answer, a directory that kept moving — is asked again next
            // time.
            Err(why @ Unresolved::Said(_)) => {
                looked = Looked::Refused(why.clone());
                Err(why)
            }
            Err(other) => return Err(other),
        };
        let mut apps = self.apps.lock_recover();
        if apps.len() >= RESOLVED_PATHS && !apps.contains_key(name) {
            apps.clear();
        }
        apps.insert(name.to_owned(), looked);
        answer
    }

    /// Keep `app_id` as running on `provider`: what a start this session made, or a call an app
    /// answered at, proves. Every name this tree knew the app by is kept so too, so the call
    /// after a start invokes the app instead of starting it again.
    pub fn saw_running(&self, app_id: &str, provider: &str) {
        let running = AppRef::Running {
            app_id: app_id.to_owned(),
            provider: provider.to_owned(),
            pinned: false,
        };
        let mut apps = self.apps.lock_recover();
        for looked in apps.values_mut() {
            let known = match looked {
                // A name that pinned one window of the app keeps naming that window: only the
                // app found by its own names is what a start or a call says runs. A refusal
                // names no app that runs.
                Looked::Found(AppRef::Running { pinned: true, .. }) | Looked::Refused(_) => false,
                Looked::Found(app) => app.app_id() == app_id,
                Looked::Named(known) => known == app_id,
            };
            if known {
                *looked = Looked::Found(running.clone());
            }
        }
        if matches!(
            apps.get(app_id),
            Some(Looked::Found(AppRef::Running { pinned: true, .. }))
        ) {
            return;
        }
        if apps.len() >= RESOLVED_PATHS && !apps.contains_key(app_id) {
            apps.clear();
        }
        apps.insert(app_id.to_owned(), Looked::Found(running));
    }

    /// [`Self::app_ref`] as the live directory answers it, asked of the
    /// workspace: the app a running one is by its id or last part, else an
    /// installed one the workspace offers to open.
    pub async fn app_ref_live(&self, name: &str) -> Result<AppRef, Unresolved> {
        // Files is every workspace's own folder, served by the framework
        // rather than installed into it: `files#open` reaches it wherever the
        // workspace is, and puts a file into its tree.
        if names_files(name) {
            return Ok(AppRef::Installed {
                app_id: FILES_APP.to_owned(),
                opens: true,
            });
        }
        let (name, instance) = match name.rsplit_once('@') {
            Some((app, instance)) if !instance.trim().is_empty() => {
                (app.trim(), Some(instance.trim()))
            }
            _ => (name, None),
        };
        let (_, running) = self.providers().await?;
        if let Some(instance) = instance {
            return pinned_window(&running, name, instance);
        }
        // A record a device serves whoever has it open (a host serving its
        // workspace's records) is the app, live, as a window of it is.
        let is_window = |provider: &str| {
            provider.parse::<u64>().is_ok() || crate::remote::is_resource_provider(provider)
        };
        let ids_where = |window: bool| {
            running
                .iter()
                .filter(|(provider, _)| is_window(provider) == window)
                .filter_map(|(_, app)| app["app_id"].as_str())
                .collect::<Vec<_>>()
        };
        let newest = |app_id: &str, window: bool| {
            running
                .iter()
                .rev()
                .filter(|(provider, _)| is_window(provider) == window)
                .find(|(_, app)| app["app_id"].as_str() == Some(app_id))
                .map(|(provider, _)| provider.clone())
        };
        let windows = ids_where(true);
        match app_named(name, &windows) {
            Ok(app_id) => {
                let provider =
                    newest(&app_id, true).ok_or_else(|| not_running(&app_id, &windows))?;
                return Ok(AppRef::Running {
                    app_id,
                    provider,
                    pinned: false,
                });
            }
            Err(error) if error.contains("names several") => return Err(error.into()),
            Err(_) => {}
        }
        // The host's own services (the desktop's layout, the workspace's
        // activity) answer for app ids no app installs: the app itself,
        // when it is installed, is what its name opens.
        let installed = self.installed().await;
        // The name the listing shows an app by (`Files`) is that app.
        if let Some(app) = installed
            .as_ref()
            .ok()
            .and_then(|apps| listed_as(apps, name))
        {
            return Ok(app.named(newest(&app.app_id, true)));
        }
        let services = ids_where(false);
        let service = app_named(name, &services).ok().filter(|app_id| {
            installed
                .as_ref()
                .is_ok_and(|installed| !installed.iter().any(|app| app.app_id == *app_id))
                || installed.is_err()
        });
        if let Some(app_id) = service
            && let Some(provider) = newest(&app_id, false)
        {
            return Ok(AppRef::Running {
                app_id,
                provider,
                pinned: false,
            });
        }
        // An installed list that could not be read is said, never taken for
        // an empty one: the agent would conclude the app does not exist.
        let installed = installed.map_err(|reason| Unresolved::InstalledUnread {
            name: name.to_owned(),
            reason: Box::new(reason),
        })?;
        let ids = installed
            .iter()
            .map(|app| app.app_id.as_str())
            .collect::<Vec<_>>();
        match app_named(name, &ids) {
            Ok(app_id) => {
                let opens = installed
                    .iter()
                    .any(|app| app.app_id == app_id && app.opens);
                Ok(AppRef::Installed { app_id, opens })
            }
            Err(error) if error.contains("names several") => Err(error.into()),
            // No app is called exactly that: the name is read once more as a model writes one.
            Err(_) => freely_named(name, &running, &installed),
        }
    }

    /// The surface the app `app_id` publishes, as the workspace's catalogue carries it on the
    /// row the app's provider publishes: for an app that is no installed application, such as
    /// the workspace's own actions. `None` when no row names one.
    pub async fn surface_of(&self, app_id: &str) -> Option<String> {
        let page = self
            .ask(CatalogueQuery {
                app_id: Some(app_id.to_owned()),
                max_results: 8,
                ..CatalogueQuery::default()
            })
            .await
            .ok()?;
        page.rows.into_iter().find_map(|row| row.surface)
    }

    /// The apps installed for this workspace, with their labels and whether
    /// this caller may open them. The workspace answers them by their kind,
    /// whole, wherever they fall among its other rows (governance, shortcuts),
    /// so a workspace with many of those still lists every app.
    pub async fn installed(&self) -> Result<Vec<InstalledApp>, Unresolved> {
        let provider = crate::remote::workspace_provider(&self.workspace);
        let mut apps: Vec<InstalledApp> = Vec::new();
        let mut cursor = Vec::new();
        let mut revision = None;
        for _ in 0..INSTALLED_PAGES {
            let page = self
                .ask(CatalogueQuery {
                    provider: Some(provider.clone()),
                    resource_name: Some(crate::remote::APPLICATION_RESOURCE_KIND.to_owned()),
                    max_results: crate::remote::MAX_CATALOGUE_PAGE,
                    cursor: std::mem::take(&mut cursor),
                    revision: revision.take(),
                    ..CatalogueQuery::default()
                })
                .await?;
            for row in page.rows {
                let Some(app_id) = row
                    .resource
                    .id
                    .strip_prefix(crate::remote::APPLICATION_RESOURCE_PREFIX)
                    .filter(|app_id| !app_id.starts_with("host:"))
                else {
                    continue;
                };
                // The workspace offers an action on an app (open, or close
                // while it runs) only to a caller that may use it, and names
                // one it may not without any: a running app shows only its
                // close, and is still this caller's to open.
                let opens = row.action.is_some();
                match apps.iter_mut().find(|app| app.app_id == app_id) {
                    Some(app) => {
                        app.opens |= opens;
                        if row.surface.is_some() {
                            app.surface.clone_from(&row.surface);
                        }
                    }
                    None => apps.push(InstalledApp {
                        app_id: app_id.to_owned(),
                        label: row.resource.label.clone(),
                        opens,
                        surface: row.surface.clone(),
                    }),
                }
            }
            if page.exhausted || page.next_cursor.is_empty() {
                break;
            }
            cursor = page.next_cursor;
            revision = Some(page.revision);
        }
        Ok(apps)
    }

    /// The resources under `parent` that `name` names (by label or kind),
    /// asked of the app whole: one note among a notebook's thousands costs
    /// the rows naming it, not a page of every note. `None` when the app
    /// cannot say (a host from before parents were named answers about its
    /// top level), and the level is listed instead.
    pub async fn named(
        &self,
        provider: &str,
        parent: &str,
        name: &str,
    ) -> Result<Option<Level>, Unresolved> {
        let name = name.trim();
        if name.is_empty() || name.len() > NAME_BYTES {
            return Ok(None);
        }
        let Ok(page) = self
            .ask(CatalogueQuery {
                provider: Some(provider.to_owned()),
                parent_resource: (!parent.is_empty()).then(|| parent.to_owned()),
                resource_name: Some(name.to_owned()),
                max_results: PAGE_ROWS,
                ..CatalogueQuery::default()
            })
            .await
        else {
            return Ok(None);
        };
        if !page.exhausted
            || page.stale_cursor
            || page.rows.iter().any(|row| row.parent_resource != parent)
        {
            return Ok(None);
        }
        let mut named = level_of(&page.rows, parent);
        named.revision = page.revision;
        Ok(Some(named))
    }

    /// What `name` may mean at the app's level, asked by name: the app's top
    /// level and, inside each of its containers, what `name` names there,
    /// less what another of them owns; `None` when one cannot be asked.
    pub async fn named_at_app(
        &self,
        provider: &str,
        name: &str,
        levels: &mut Levels,
    ) -> Result<Option<Level>, Unresolved> {
        // What this call already holds of the app's own level answers the name
        // where it names one resource exactly: the session keeps the directory
        // it read, so a call on another of the app's resources costs itself.
        if let Some(root) = levels.get("").cloned()
            && let Some(only) = named_among(&root, name)
        {
            return Ok(Some(Level {
                own: Vec::new(),
                children: vec![only],
                revision: root.revision,
                pages: Arc::default(),
            }));
        }
        // A name the app's own level carries is asked of the app by name
        // first: a chain it keeps is answered from the head part publishing
        // it, never a page of the listing built from every item the app
        // shows. A name no one resource there answers is listed as before.
        if let Some(among) = self.named(provider, "", name).await?
            && let [only] = among.children.as_slice()
        {
            return Ok(Some(Level {
                own: Vec::new(),
                children: vec![only.clone()],
                revision: among.revision,
                pages: Arc::default(),
            }));
        }
        let root = self.level(provider, "", levels).await?;
        let mut revision = root.revision.clone();
        // A name the app's own level carries exactly is answered there: its id
        // first, then its label. The containers hold the resource's items,
        // and asking each of them for a name it is not (the resource's own
        // name, most often) lists every item it holds.
        let by_id: Vec<&Node> = root
            .children
            .iter()
            .filter(|node| node.names_id(name))
            .collect();
        let by_label: Vec<&Node> = root
            .children
            .iter()
            .filter(|node| node.answers_to(name) == Some(0))
            .collect();
        for named in [by_id, by_label] {
            if let [only] = named.as_slice() {
                return Ok(Some(Level {
                    own: Vec::new(),
                    children: vec![(**only).clone()],
                    revision,
                    pages: Arc::default(),
                }));
            }
        }
        let mut pool = root.children.clone();
        for container in root
            .children
            .iter()
            .filter(|node| node.container)
            .take(ROOT_PEEK)
        {
            let Some(inside) = self.named(provider, &container.id, name).await? else {
                return Ok(None);
            };
            revision = inside.revision;
            for child in inside.children {
                if !pool.iter().any(|seen| seen.id == child.id) {
                    pool.push(child);
                }
            }
        }
        let all = pool.clone();
        pool.retain(|node| !all.iter().any(|owner| owner.beside(node)));
        Ok(Some(Level {
            own: Vec::new(),
            children: pool,
            revision,
            pages: Arc::default(),
        }))
    }

    /// The resource `segment` names under `parent` (the app itself for
    /// `None`), and the resources it was named among. A name only one
    /// resource there carries is asked of the app by name; anything else (a
    /// key, the start of a name, a name several carry, one that is not
    /// there) is decided on the whole level, as listing it shows. With them
    /// comes the directory revision it was found at. `at` is the address the
    /// level is listed at, which a name not found is answered with.
    pub async fn step(
        &self,
        provider: &str,
        at: &str,
        parent: Option<&Node>,
        segment: &str,
        levels: &mut Levels,
        forms: Option<&dyn TypeNames>,
    ) -> Result<(Node, Level), Unresolved> {
        let asked = match parent {
            Some(owner) if !levels.contains_key(&owner.id) => {
                self.named(provider, &owner.id, segment).await?
            }
            None => self.named_at_app(provider, segment, levels).await?,
            Some(_) => None,
        };
        if let Some(among) = asked {
            // What the app answered is the resource the segment names: one
            // whose id it is, before any label, else one its label names
            // exactly. Either costs the answer, not a read of the level.
            let mut named = among.children.iter().filter(|node| node.names_id(segment));
            if let (Some(only), None) = (named.next(), named.next()) {
                let only = only.clone();
                return Ok((only, among));
            }
            let mut named = among
                .children
                .iter()
                .filter(|node| node.answers_to(segment) == Some(0));
            if let (Some(only), None) = (named.next(), named.next()) {
                let only = only.clone();
                return Ok((only, among));
            }
        }
        // An app's directory is rebuilt whenever its document changes, and for an instant it
        // lists less than it has. A name not found is looked for again for a moment, the
        // levels read anew, before the app is said not to have it.
        let began = self.pause().now();
        let mut looks: u32 = 0;
        loop {
            // A level is read as far as its bound, and a directory that holds more goes on
            // past it. A name the pages read first do not carry is looked for on the pages
            // after them, to the directory's end, before it is looked for anywhere else: what
            // a resource holds is found under it wherever it falls in the directory, and
            // only a name no page carries is not there.
            let (picked, here) = if let Some(owner) = parent {
                self.level_holding(provider, &owner.id, levels, |level| {
                    Ok(pick(segment, level.whole_children(), forms)?.cloned())
                })
                .await?
            } else {
                let here = self.app_children(provider, levels).await?;
                (pick(segment, &here.children, forms)?.cloned(), here)
            };
            let found = match picked {
                Some(node) => Some(node),
                None => self.deep(provider, segment, parent, levels, forms).await?,
            };
            if let Some(found) = found {
                return Ok((found, here));
            }
            if looks >= LOOKS_AGAIN
                || (read_on(&here) && self.pause().now().saturating_sub(began) >= LOOKS_MOMENT)
            {
                // Each name as the address that takes it, not the word alone:
                // a caller reading where it went wrong writes the next call
                // from what it is told, and a word it cannot place is no
                // address at all.
                let addresses: Vec<_> = (here.children.iter())
                    .map(|node| format!("{at}/{}", node.name(&here.children)))
                    .collect();
                return Err(Unresolved::NoSuch {
                    name: segment.to_owned(),
                    at: at.to_owned(),
                    has: addresses,
                });
            }
            looks = looks.saturating_add(1);
            self.pause()
                .pause(restart_pause(looks.saturating_add(1)))
                .await;
            levels.clear();
        }
    }

    /// A resource `name` names somewhere below `within` (or anywhere in the
    /// app), found among the nodes this call has read and then by reading
    /// the containers it has not: a note's body is published beside the
    /// note rather than inside it, and its key says whose it is.
    pub async fn deep(
        &self,
        provider: &str,
        name: &str,
        within: Option<&Node>,
        levels: &mut Levels,
        forms: Option<&dyn TypeNames>,
    ) -> Result<Option<Node>, Unresolved> {
        for _ in 0..=DEEP_READS {
            let pool: Vec<Node> = levels
                .values()
                .flat_map(|level| level.whole_children().iter())
                .filter(|node| {
                    within.is_none_or(|owner| {
                        node.key.contains(owner.token()) || Self::below(node, owner, levels)
                    }) && rank(name, node, forms).is_some()
                })
                .cloned()
                .collect();
            if let Some(found) = pick(name, &pool, forms)? {
                return Ok(Some(found.clone()));
            }
            let unread = levels
                .values()
                .flat_map(|level| level.children.iter())
                .find(|node| node.container && !levels.contains_key(&node.id))
                .map(|node| node.id.clone());
            let Some(unread) = unread else {
                return Ok(None);
            };
            self.level(provider, &unread, levels).await?;
        }
        Ok(None)
    }

    /// Whether `node`'s parent chain, as far as this call has read it,
    /// reaches `owner`.
    fn below(node: &Node, owner: &Node, levels: &Levels) -> bool {
        let mut parent = node.parent.clone();
        for _ in 0..8 {
            if parent.is_empty() {
                return false;
            }
            if parent == owner.id {
                return true;
            }
            let Some(next) = levels
                .values()
                .flat_map(|level| level.children.iter())
                .find(|candidate| candidate.id == parent)
            else {
                return false;
            };
            parent.clone_from(&next.parent);
        }
        false
    }

    /// Resolve a whole path. A segment written the way a program types a type (`tasks.Task`)
    /// is read as that type first: it names the resource whose published kind the listing
    /// prints that name for. Only a segment written that way is asked of the task's programs,
    /// so a path of titles (`notes/Todo`) does not start a task's host to read a name already
    /// said; a form no app declares falls back to the plain reading, where it may be a title.
    ///
    /// With the resolution come the forms it was read by (`None` when it was read plainly), so
    /// a caller that shows what it found names the type the same way without asking again.
    pub async fn resolve(
        &self,
        parsed: &Address,
        forms: Option<&dyn TypeNames>,
    ) -> Result<(Resolved, bool), Unresolved> {
        if let Some(forms) = forms.filter(|_| names_a_type(parsed)) {
            match self.resolve_with(parsed, Some(forms), usize::MAX).await {
                // A path of segments resolves to a resource only when it walked every one: an
                // empty chain is a name no resource answered.
                Ok(found) if !found.chain.is_empty() => return Ok((found, true)),
                Ok(_) => {}
                Err(note) if note.is_note() => {}
                Err(other) => return Err(other),
            }
        }
        self.resolve_with(parsed, None, usize::MAX)
            .await
            .map(|found| (found, false))
    }

    /// [`Self::resolve`] reading the names a path segment may go by as `forms` says they are
    /// written: a segment names a type by the name a listing prints for it (`tasks.Task`),
    /// which `forms` reads back as its kind. `reaching` is how many resources of an app's own
    /// page to read its containers for: a listing reads the page's end and one past it, and
    /// a read takes every one (`usize::MAX`).
    pub async fn resolve_with(
        &self,
        parsed: &Address,
        forms: Option<&dyn TypeNames>,
        reaching: usize,
    ) -> Result<Resolved, Unresolved> {
        let mut levels = Levels::new();
        let reference = match self.app_ref(&app_of(parsed)).await {
            Ok(reference) => reference,
            Err(why) => match self.app_of_type(&app_of(parsed)).await {
                Some((app_id, provider, _)) => {
                    return self
                        .resolve_in(parsed, app_id, provider, true, forms, &mut levels, reaching)
                        .await;
                }
                None => return Err(why),
            },
        };
        // An app this tree found running is kept for the calls after this one, which is only
        // right while it still answers: one that stopped since is looked up again, so the call
        // after this refusal asks the workspace where the app is instead of failing here again.
        let about = reference.app_id().to_owned();
        let resolved = match reference {
            AppRef::Running {
                app_id,
                provider,
                pinned: true,
            } => {
                self.resolve_in(parsed, app_id, provider, true, forms, &mut levels, reaching)
                    .await
            }
            AppRef::Running {
                app_id, provider, ..
            } => {
                self.resolve_published(parsed, app_id, provider, true, forms, &mut levels, reaching)
                    .await
            }
            // Reading what an installed app holds starts it without a window, as an action on
            // it does: a path names the app's resources, not one of its processes.
            AppRef::Installed {
                app_id,
                opens: true,
            } => {
                let provider = self.started_provider(&app_id, None).await?;
                self.resolve_published(parsed, app_id, provider, true, forms, &mut levels, reaching)
                    .await
            }
            AppRef::Installed { opens: false, .. } => Err(Unresolved::NotRunning {
                name: app_of(parsed),
                opens: false,
            }),
        };
        if let Err(why) = &resolved
            && why.app_gone()
        {
            self.forget_app(&about);
        }
        resolved
    }

    /// [`Self::resolve_in`] the newest instance `newest` of `app_id`, or, when
    /// it has published nothing for the path, the app's older instances in
    /// turn. A path names the app, not one of its processes: an instance
    /// lists as running a moment before it publishes its directory, and must
    /// not hide an older one that has (an agent saw an app's actions, and
    /// the next call found none).
    pub async fn resolve_published(
        &self,
        parsed: &Address,
        app_id: String,
        newest: String,
        list: bool,
        forms: Option<&dyn TypeNames>,
        levels: &mut Levels,
        reaching: usize,
    ) -> Result<Resolved, Unresolved> {
        let published = |found: &Resolved| {
            !found.chain.is_empty() || !found.actions.is_empty() || !found.children.is_empty()
        };
        let first = self
            .resolve_in(
                parsed,
                app_id.clone(),
                newest.clone(),
                list,
                forms,
                levels,
                reaching,
            )
            .await;
        if first.as_ref().is_ok_and(published) {
            return first;
        }
        // The levels here are the newest instance's: an older one keeps its
        // directory, and reading it under this one's name would name the wrong
        // resources.
        levels.clear();
        // Only then are the running apps asked again, for the older instances.
        let older: Vec<String> = self
            .providers()
            .await?
            .1
            .iter()
            .rev()
            .filter(|(_, app)| app["app_id"].as_str() == Some(app_id.as_str()))
            .map(|(peer, _)| peer.clone())
            .filter(|peer| *peer != newest)
            .collect();
        for provider in older {
            let resolved = self
                .resolve_in(
                    parsed,
                    app_id.clone(),
                    provider,
                    list,
                    forms,
                    levels,
                    reaching,
                )
                .await;
            if resolved.as_ref().is_ok_and(published) {
                return resolved;
            }
        }
        first
    }

    /// Resolve the segments of `parsed` in the running app `app_id`, whose
    /// instance is `provider`. With `list`, what the target holds and the
    /// actions on it are read too; without, a resource on the path is only
    /// found (an action named on it is resolved by its address). `forms` is how
    /// a path segment may name a type the way a program types it
    /// (`tasks.Task`), as a listing names it. `reaching` is how far what an
    /// app's own page merges from its containers is read: a listing's end and
    /// one resource past it, or every one ([`Self::app_children_listed`]).
    pub async fn resolve_in(
        &self,
        parsed: &Address,
        app_id: String,
        provider: String,
        list: bool,
        forms: Option<&dyn TypeNames>,
        levels: &mut Levels,
        reaching: usize,
    ) -> Result<Resolved, Unresolved> {
        let app = app_of(parsed).to_lowercase();
        // What an action answered is a path the next call takes as it is:
        // `tasks.tracker/issues/7` names one resource whole, where its parts
        // named one at a time would name every kind that shares a word.
        if type_address(&app_of(parsed)).is_some() || device_section(parsed) {
            let address = parsed.resource();
            if let Some(found) = self
                .by_item_address(&app_id, &provider, &address, levels)
                .await?
            {
                return Ok(found);
            }
        }
        let mut chain: Vec<Node> = Vec::new();
        // What each resource on the path was named among, and the revision
        // the last of them was found at.
        let mut among: Vec<Vec<Node>> = Vec::new();
        let mut revision = String::new();
        // The address each level is listed at, as the app's own listing writes
        // it: what a name not found there is answered with.
        let mut at = app.clone();
        for segment in below_app(parsed) {
            let (found, siblings) = self
                .step(
                    &provider,
                    &at,
                    chain.last(),
                    &segment.written(),
                    levels,
                    forms,
                )
                .await?;
            at = format!("{at}/{}", found.name(&siblings.children));
            chain.push(found);
            among.push(siblings.children);
            revision = siblings.revision;
        }
        let mut here = match chain.last() {
            Some(_) if !list => Level::default(),
            Some(node) => self.level_near(&provider, &node.id, levels).await?,
            None => {
                // What the app's containers hold (a notebook's notes) is read for a listing.
                // An action named on the app is found on the app's own level first
                // ([`Self::pick_action_in`]): `notes#create-page` is the notebook's, and
                // reading every note to find it would grow with the notebook.
                let mut root = if list {
                    self.app_children_listed(&provider, levels, reaching)
                        .await?
                } else {
                    self.level(&provider, "", levels).await?
                };
                as_the_service(&provider, &mut root);
                root
            }
        };
        let mut written = app;
        if let Some(owner) = chain.last() {
            // Published beside the resource, not inside it, but its key says
            // whose it is: listed as one of its children.
            let token = owner.token().to_owned();
            let related: Vec<Node> = levels
                .values()
                .flat_map(|level| level.children.iter())
                .filter(|node| {
                    token.len() >= 8
                        && node.id != owner.id
                        && node.key.contains(&token)
                        && !here.children.iter().any(|child| child.id == node.id)
                })
                .cloned()
                .collect();
            here.children.extend(related);
        }
        let mut previous: Option<Node> = None;
        for (node, siblings) in chain.iter().zip(&among) {
            // Named against what it sits among and the resource it sits
            // under, whose title it may share.
            let mut named = siblings.clone();
            named.extend(previous.clone());
            written = format!("{written}/{}", node.name(&named));
            previous = Some(node.clone());
        }
        Ok(Resolved {
            app_id,
            provider,
            path: written,
            node: chain.last().cloned(),
            // A directory that hoists a resource's actions into its parent's
            // listing lists them there, not under the resource.
            actions: if here.own.is_empty() {
                chain
                    .last()
                    .map(|node| node.actions.clone())
                    .unwrap_or_default()
            } else {
                here.own
            },
            children: here.children,
            chain,
            levels: levels.clone(),
            revision,
        })
    }
}

/// Why `name` names no app: how a running app is addressed, and the apps
/// running and installed.
fn no_app_named(name: &str, running: &[(String, Value)], installed: &[InstalledApp]) -> String {
    let running_ids = running
        .iter()
        .filter_map(|(_, app)| app["app_id"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let installed_ids = installed
        .iter()
        .map(|app| app.app_id.clone())
        .collect::<Vec<_>>();
    format!(
        "unknown_app: no app {name}; name a running app by its id or last part, e.g. {}. Running {}; installed {}",
        running_ids.first().map_or("<app_id>", String::as_str),
        list_or_none(&app_names(&running_ids)),
        list_or_none(&app_names(&installed_ids)),
    )
}

/// The one installed app the listing shows by the name `name` (`Files` for
/// `file-explorer`), any case.
pub fn listed_as<'a>(installed: &'a [InstalledApp], name: &str) -> Option<&'a InstalledApp> {
    let mut named = installed
        .iter()
        .filter(|app| app.label.eq_ignore_ascii_case(name));
    named.next().filter(|_| named.next().is_none())
}

/// The app `name` means where no app is called exactly that: the name read once more as a
/// model writes one (`word_processor`, `WordProcessor`, `Word`), among the installed apps as
/// this caller is listed them and the apps of the providers it may use, and no others. What it
/// finds is opened and acted in as the listing says this caller may. A word that still names
/// none (`host` for the Browser, `so.tilo.browser`) is a guess, so the running apps are named
/// as they are to be addressed.
fn freely_named(
    name: &str,
    running: &[(String, Value)],
    installed: &[InstalledApp],
) -> Result<AppRef, Unresolved> {
    let listed: Vec<(&str, &str)> = installed
        .iter()
        .map(|app| (app.app_id.as_str(), app.label.as_str()))
        .chain(
            running
                .iter()
                .filter_map(|(_, app)| Some((app["app_id"].as_str()?, ""))),
        )
        .collect();
    let Some(app_id) = app_meant(name, &listed)? else {
        return Err(no_app_named(name, running, installed).into());
    };
    // Its newest window, or else whatever answers for it without one.
    let newest = |window: bool| {
        running
            .iter()
            .rev()
            .filter(|(provider, _)| {
                (provider.parse::<u64>().is_ok() || crate::remote::is_resource_provider(provider))
                    == window
            })
            .find(|(_, app)| app["app_id"].as_str() == Some(app_id.as_str()))
            .map(|(provider, _)| provider.clone())
    };
    if let Some(app) = installed.iter().find(|app| app.app_id == app_id) {
        return Ok(app.named(newest(true)));
    }
    let provider = newest(true).or_else(|| newest(false));
    match provider {
        Some(provider) => Ok(AppRef::Running {
            app_id,
            provider,
            pinned: false,
        }),
        None => Err(no_app_named(name, running, installed).into()),
    }
}

pub fn app_names(ids: &[String]) -> Vec<String> {
    let mut names: Vec<String> = ids
        .iter()
        .map(|id| id.rsplit('.').next().unwrap_or(id).to_lowercase())
        .collect();
    names.sort();
    names.dedup();
    names
}

impl<P: ActionProvider + Sync + Clone + Send + 'static> Tree<P> {
    /// The app a kind names (`tasks.tracker` is the Tasks app's tracker), as
    /// the app id and the provider serving it: a kind begins with the app it
    /// belongs to. `None` when the name is no kind, or its app is not here or
    /// is not the agent's to open.
    ///
    /// An app that is installed but not running is started without a window,
    /// as an action on it is: a cold link addresses a resource of the app the
    /// device never showed, and the app that publishes it serves its kind once
    /// it runs.
    pub async fn app_of_type(&self, name: &str) -> Option<(String, String, bool)> {
        let app = type_address(name)?;
        let (app_id, provider, pinned) = match self.app_ref(app).await.ok()? {
            AppRef::Running {
                app_id,
                provider,
                pinned,
            } => (app_id, provider, pinned),
            AppRef::Installed {
                app_id,
                opens: true,
            } => {
                let provider = self.started_provider(&app_id, None).await.ok()?;
                (app_id, provider, false)
            }
            AppRef::Installed { opens: false, .. } => return None,
        };
        if pinned {
            return Some((app_id, provider, pinned));
        }
        // What the kind names, never one of the app's other documents that happens to be first:
        // a kind's directory lists a resource of it (`tasks.issues` is the issues' chain, and
        // `tasks.tracker` the tracker's own document). The app's documents are asked in turn,
        // newest first, and the first that lists the kind answers.
        let kind = name
            .strip_prefix(app)
            .and_then(|rest| rest.strip_prefix('.'))?
            .to_owned();
        let naming = |node: &Node| {
            node.resource_type == kind || node.resource_type.starts_with(&format!("{kind}."))
        };
        let mut candidates = vec![provider.clone()];
        if let Ok((_, running)) = self.providers().await {
            candidates.extend(
                running
                    .iter()
                    .rev()
                    .filter(|(_, app)| app["app_id"].as_str() == Some(app_id.as_str()))
                    .map(|(provider, _)| provider.clone()),
            );
        }
        candidates.dedup();
        for provider in candidates {
            let mut levels = Levels::new();
            let Ok(level) = self.level(&provider, "", &mut levels).await else {
                continue;
            };
            if level.children.iter().any(naming) {
                return Some((app_id, provider, false));
            }
        }
        Some((app_id, provider, false))
    }
}

/// The one window `instance` (a provider an `open` or `tilo_ls` named) among `running`, which
/// must be of the app `name`: the agent means that window, never another of the app's.
fn pinned_window(
    running: &[(String, Value)],
    name: &str,
    instance: &str,
) -> Result<AppRef, Unresolved> {
    let (provider, entry) = running
        .iter()
        .find(|(provider, _)| provider == instance)
        .ok_or_else(|| format!("no running window {instance}; tilo_ls lists the running apps"))?;
    let app_id = entry["app_id"]
        .as_str()
        .ok_or_else(|| format!("window {instance} names no app"))?
        .to_owned();
    if app_named(name, &[app_id.as_str()]).is_err() {
        return Err(format!(
            "window {instance} is {}, not {name}",
            app_names(std::slice::from_ref(&app_id)).join("")
        )
        .into());
    }
    Ok(AppRef::Running {
        app_id,
        provider: provider.clone(),
        pinned: true,
    })
}

/// The top level `root` of the host service `provider`, as the service itself
/// answers it.
pub fn as_the_service(provider: &str, root: &mut Level) {
    // A host service (the desktop's layout, on `desktop:session`)
    // publishes everything on its one resource: its actions are
    // the service's own, `desktop#<action>`. A resource's one
    // container (Tasks' tracker) holds what the app offers more.
    if provider.parse::<u64>().is_err()
        && root.own.is_empty()
        && let [only] = root.children.as_slice()
        && !only.container
    {
        root.own = only.actions.clone();
    }
    // A host service's one top resource holding more (a workspace's
    // governance, holding its servers and apps) is the service
    // itself: its actions are the service's own (`workspace#member.list`),
    // and what it holds is listed as the service's.
    if provider.parse::<u64>().is_err()
        && !crate::remote::is_resource_provider(provider)
        && root.own.is_empty()
        && let [only] = root
            .children
            .iter()
            .filter(|node| node.parent.is_empty())
            .collect::<Vec<_>>()
            .as_slice()
        && only.container
    {
        let only = (*only).clone();
        root.own = only.actions;
        root.children.retain(|node| node.id != only.id);
    }
}
impl From<Unresolved> for String {
    fn from(unresolved: Unresolved) -> Self {
        unresolved.to_string()
    }
}
