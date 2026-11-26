//! The providers registered on a device, by the provider id a call names: the one registry every
//! caller of them reaches.
//!
//! An embedder registers a provider with the application whose grants decide its actions and how
//! far it reaches ([`Reach`]). A call on this device reaches it through the device's dispatch
//! (`tilo do -w`, an agent's `tilo_do`, a view's button, the session's own steps), which admits it
//! against the workspace's policy under that application, asks the person where the action's
//! contract says so, keeps the call's record and hands it to the provider's
//! [`run`](ActionProvider::run). A call from another device over the device action service
//! (`tilo-actions/1`) reaches a provider registered to reach devices ([`Providers::device_call`]),
//! admitted by the grants of the workspace it asks in for the asking device. A provider decides no
//! grant itself.
//!
//! A provider registers the resources it publishes at fixed addresses with it
//! ([`ActionProvider::published`]). An app's call naming one of those addresses reaches it whichever
//! app the call names ([`Providers::publishing`]): an app's code calls by address and never knows
//! whether an app or a provider answers, and the providers' packages are what that code compiles
//! against ([`Providers::packages`]).
//!
//! A record of a workspace placed on a host (a home server, a cloud host) is the host's to change:
//! a call on this device of a typed action of one of its records is sent to the host's actions
//! ([`Providers::place_on_hosts`]), and its observations and its cancellation follow it there.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueProvider, CatalogueQuery,
    InvocationReceipt, InvocationState, InvokeRequest, Pending, PendingPoll, ProvidedPackage,
    RemoteActionTarget,
};
use tilo_actions::{ResourceSpec, api::locate};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};

/// How far a registered provider reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// Every device of the workspace, this one included, by the workspace's grants for it.
    Devices,
    /// A caller on this device alone, confined to the workspace its connection was admitted to:
    /// what pairs a device, administers the person's identities or changes their settings.
    ThisDevice,
    /// What a turn does (the session's layout, its undo, its windows): offered to an agent while
    /// it works a turn, and admitted by that turn.
    Turns,
    /// An agent session's own (the programs it writes): taken only in an agent session, and listed
    /// to it when `listed` (an agent's tools are asked by its app, never by its model).
    AgentSessions {
        /// Whether its actions are listed in the agent's catalogue.
        listed: bool,
    },
}

impl Reach {
    /// Whether a call from another device reaches it.
    #[must_use]
    pub const fn devices(self) -> bool {
        matches!(self, Self::Devices)
    }

    /// Whether its actions are a session's own, which an agent takes only in one.
    #[must_use]
    pub const fn in_sessions(self) -> bool {
        matches!(self, Self::Turns | Self::AgentSessions { .. })
    }

    /// Whether its actions are listed in a catalogue.
    #[must_use]
    pub const fn listed(self) -> bool {
        !matches!(self, Self::AgentSessions { listed: false })
    }
}

/// One registered provider.
#[derive(Clone)]
pub struct Registered {
    /// The provider id a call names.
    pub provider: Arc<str>,
    /// The application whose grants decide its actions.
    pub app: Arc<str>,
    /// How far it reaches.
    pub reach: Reach,
    /// What answers its calls.
    pub answers: Arc<dyn ActionProvider + Send + Sync>,
    /// The resources it publishes at fixed addresses.
    pub published: Vec<ResourceSpec>,
    /// The device a host placed whose own calls this provider answers ([`Providers::serve_placed`]):
    /// a placed device reaches no provider but the one registered for it, beside the records its
    /// certificate names.
    pub placed_for: Option<[u8; 32]>,
    /// What answers its calls as the connection that carried each admitted it: the workspace it
    /// was admitted to and the device certificate its transport verified
    /// ([`Providers::answer_in_workspaces`]). `None` for a provider every call reaches through
    /// [`Self::answers`] alone.
    pub in_workspace: Option<Arc<dyn WorkspaceAnswers>>,
}

/// A call as the connection that carried it admitted it: the workspace it was admitted to, and the
/// device certificate its transport verified, beside its caller. A transport that named no
/// certificate leaves it `None`, and the workspace's own record of the caller's device decides
/// ([`super::Workspaces::callers_certificate`]).
#[derive(Clone, Copy)]
pub struct WorkspaceCall<'a> {
    /// The workspace the connection was admitted to, `None` for a call of the device itself.
    pub workspace: Option<collab_types::WorkspaceId>,
    pub caller: &'a Caller,
    /// The certificate the transport verified for the caller's device.
    pub certificate: Option<&'a tilo_user_identity::DeviceCert>,
}

/// What a provider answering in a workspace answers a call with.
pub enum Answered {
    /// Its output, once.
    Value(serde_json::Value),
    /// A result read as it moves: its first item now, each newer one as it comes, until it ends
    /// with its output or a refusal. The caller follows it as the invocation's items; letting go
    /// of it stops it.
    Live(tokio::sync::watch::Receiver<Live>),
}

/// Where a live result stands ([`Answered::Live`]).
#[derive(Clone, Debug, PartialEq)]
pub enum Live {
    /// Its newest item.
    Item(serde_json::Value),
    /// It ended: its output, or why it failed.
    Ended(Result<serde_json::Value, String>),
}

/// What an answer in a workspace is waited for as.
pub type Answering<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// What a provider registered to answer in the workspaces this device holds offers and answers:
/// each call with the workspace and the certificate of the connection that carried it.
pub trait WorkspaceAnswers: Send + Sync {
    /// The children of a workspace's own tree it answers (`vault` for
    /// `workspace.permissions:<id>/vault`): a call naming one reaches it, whichever provider
    /// the call names.
    fn children(&self) -> &'static [&'static str] {
        &[]
    }

    /// The rows of the children of `workspace`'s own tree it answers (its UUID,
    /// [`Self::children`]) that `call`'s caller is offered, listed in that tree.
    fn tree_rows<'a>(
        &'a self,
        _call: WorkspaceCall<'a>,
        _workspace: &'a str,
    ) -> Answering<'a, Vec<tilo_actions::remote::CatalogueRow>> {
        Box::pin(std::future::ready(Vec::new()))
    }

    /// The parameters the action `target` names is published with, where it answers it.
    fn params(&self, _target: &RemoteActionTarget) -> Option<Vec<tilo_actions::ActionParam>> {
        None
    }

    /// Whether the action `target` names answers a secret, where it answers it
    /// ([`tilo_actions::SECRET_ANSWER_TAG`]).
    fn answers_a_secret(&self, _target: &RemoteActionTarget) -> bool {
        false
    }

    /// Whether an agent's turn asks the action `target` names of it directly, decided by it for
    /// the turn rather than by a workspace's governance.
    fn turn_takes(&self, _target: &RemoteActionTarget) -> bool {
        false
    }

    /// Answer `request`, which an agent's turn asks of it directly ([`Self::turn_takes`]), as
    /// it decides for that turn.
    fn answer_turn<'a>(
        &'a self,
        call: WorkspaceCall<'a>,
        request: &'a InvokeRequest,
    ) -> Answering<'a, Result<Answered, ActionCallError>> {
        self.answer(call, request)
    }

    /// Whether another device of a workspace's roster asks the action `target` names of it,
    /// through the workspace's governance. One no turn takes and no roster device asks is known
    /// only to an app's own connection here.
    fn roster_asks(&self, _target: &RemoteActionTarget) -> bool {
        true
    }

    /// What `call`'s caller is offered of `query`, by the publisher and workspace each row is
    /// offered on.
    fn offer<'a>(
        &'a self,
        call: WorkspaceCall<'a>,
        query: &'a CatalogueQuery,
    ) -> Answering<'a, Vec<(CatalogueProvider, Vec<tilo_actions::remote::CatalogueRow>)>>;

    /// Answer `request` for `call`.
    fn answer<'a>(
        &'a self,
        call: WorkspaceCall<'a>,
        request: &'a InvokeRequest,
    ) -> Answering<'a, Result<Answered, ActionCallError>>;
}

/// How many invocations from other devices a registered provider took are remembered for routing
/// their observations; the oldest are let go beyond it, and their provider is asked no more.
const TAKEN_KEPT: usize = 4096;

/// The providers registered on a device.
#[derive(Clone, Default)]
pub struct Providers(Arc<Inner>);

/// [`Providers`], by weak reference: what a provider registered on a device keeps to answer its
/// calls, without keeping the registry alive.
#[derive(Clone)]
pub struct WeakProviders(std::sync::Weak<Inner>);

impl WeakProviders {
    /// The registry, while it is alive.
    #[must_use]
    pub fn upgrade(&self) -> Option<Providers> {
        Some(Providers(self.0.upgrade()?))
    }
}

#[derive(Default)]
struct Inner {
    provided: std::sync::RwLock<BTreeMap<Arc<str>, Registered>>,
    /// Which provider took each invocation another device asked for, or that was sent to a
    /// workspace's host, so its observations and its cancellation reach that provider.
    taken: std::sync::Mutex<HashMap<InvocationId, Taker>>,
    /// The hosts of the workspaces placed on another device, once the embedder says where they
    /// are reached.
    hosts: std::sync::RwLock<Option<(HostsOf, HostsAnswer)>>,
    /// What answers for the seat while a person's shell serves it ([`Providers::serve_seat`]):
    /// held apart from the providers a call names, so no caller reaches it.
    seat: std::sync::RwLock<Option<ServedSeat>>,
    /// What answers this device's app calls, whose record of the invocations it took
    /// another device reads over its link ([`Providers::invocations`]).
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    invocations: std::sync::RwLock<Option<super::app_invocations::WeakAppInvocations>>,
    /// The host that placed this device for records of one workspace, when it is such a device:
    /// the action service admitted its requests already ([`Providers::answer_placing_host`]).
    placing_host: std::sync::OnceLock<[u8; 32]>,
}

/// How a call of the seat hands the shell that serves it a file: queued on its link ahead of the
/// call, answered with the number the shell knows it by there.
pub type HandOver = Arc<dyn Fn(std::fs::File) -> std::io::Result<u32> + Send + Sync>;

/// The seat as a shell serves it: what answers its calls, and how a call hands it a file, when
/// its link carries descriptors.
#[derive(Clone)]
pub struct ServedSeat {
    pub answers: Arc<dyn ActionProvider + Send + Sync>,
    pub hand_over: Option<HandOver>,
}

/// The actions of the host a workspace is placed on, by workspace: `None` for one this device
/// hosts itself or holds alone.
pub type HostsOf = Arc<dyn Fn(&str) -> Option<Arc<dyn ActionProvider + Send + Sync>> + Send + Sync>;

/// Which typed actions of a placed workspace's records its host answers rather than this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostsAnswer {
    /// Those no provider here applies: a device that keeps a copy of the workspace applies the
    /// rest to it, and its copy carries them to the host.
    Unapplied,
    /// Every one: a device that keeps no copy of the workspace (one confined to it) has nothing
    /// to apply them to, so none is answered here in its host's place.
    Every,
}

/// What took an invocation: a provider registered here, by name (one withdrawn since is asked no
/// more), or the host of the workspace it was asked in.
#[derive(Clone)]
enum Taker {
    Registered(Arc<str>),
    Host(Arc<dyn ActionProvider + Send + Sync>),
}

impl Providers {
    /// Read the invocations this device's app calls were asked for from `apps`' record from
    /// now on: what another device reads of an invocation it was given proof of
    /// ([`super::RecordReads`]).
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    pub fn keep_invocations(&self, apps: super::app_invocations::WeakAppInvocations) {
        *self.0.invocations.write_recover() = Some(apps);
    }

    /// What answers this device's app calls, whose record of invocations this device answers reads of, while it runs.
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    pub(crate) fn invocations(&self) -> Option<super::app_invocations::WeakAppInvocations> {
        self.0.invocations.read_recover().clone()
    }

    /// This registry, by weak reference ([`WeakProviders`]): what a provider it holds keeps to
    /// answer its own calls without keeping the registry alive.
    #[must_use]
    pub fn downgrade(&self) -> WeakProviders {
        WeakProviders(Arc::downgrade(&self.0))
    }

    /// Answer the calls that name `provider`, or an address it publishes, with `answers` from now
    /// on, decided by the grants of the application `app` and reaching as far as `reach`.
    pub fn provide(
        &self,
        provider: &str,
        app: &str,
        reach: Reach,
        answers: Arc<dyn ActionProvider + Send + Sync>,
    ) {
        let published = answers.published();
        self.0.provided.write_recover().insert(
            Arc::from(provider),
            Registered {
                provider: Arc::from(provider),
                app: Arc::from(app),
                reach,
                answers,
                published,
                placed_for: None,
                in_workspace: None,
            },
        );
    }

    /// Answer the calls that name `provider`, or a child of a workspace's tree it answers
    /// ([`WorkspaceAnswers::children`]), with `answers` from now on, each as the connection that
    /// carried it admitted it ([`WorkspaceCall`]): on a connection to this device's workspaces,
    /// and on this device's own dispatch through the workspaces' governance, decided by the
    /// grants of the application `app`. What it answers decides who may ask it.
    pub fn answer_in_workspaces(
        &self,
        provider: &str,
        app: &str,
        answers: Arc<dyn WorkspaceAnswers>,
    ) {
        let through = Arc::new(ThroughGovernance(self.downgrade()));
        self.0.provided.write_recover().insert(
            Arc::from(provider),
            Registered {
                provider: Arc::from(provider),
                app: Arc::from(app),
                reach: Reach::ThisDevice,
                answers: through,
                published: Vec::new(),
                placed_for: None,
                in_workspace: Some(answers),
            },
        );
    }

    /// What answers `target` in a workspace ([`Self::answer_in_workspaces`]): the provider it
    /// names, or the one answering the child of a workspace's tree its address names.
    #[must_use]
    pub fn in_workspace(&self, target: &RemoteActionTarget) -> Option<Arc<dyn WorkspaceAnswers>> {
        let provided = self.0.provided.read_recover();
        if let Some(answers) = provided
            .get(target.provider.as_str())
            .and_then(|registered| registered.in_workspace.clone())
        {
            return Some(answers);
        }
        let child = tree_child(&target.address.resource)?;
        provided
            .values()
            .filter_map(|registered| registered.in_workspace.as_ref())
            .find(|answers| answers.children().contains(&child))
            .cloned()
    }

    /// Every provider answering in a workspace, by the provider id it is registered under.
    #[must_use]
    pub fn all_in_workspace(&self) -> Vec<(Arc<str>, Arc<dyn WorkspaceAnswers>)> {
        self.0
            .provided
            .read_recover()
            .values()
            .filter_map(|registered| {
                Some((
                    Arc::clone(&registered.provider),
                    registered.in_workspace.clone()?,
                ))
            })
            .collect()
    }

    /// Answer the calls that name `app`'s actions in `workspace` with `answers` from now on,
    /// decided by the workspace's grants for the app and reaching as far as `reach`: the one
    /// registration every app process's actions go through, whichever language the app is
    /// written in. An app's code calls by address and never knows which process answers it.
    pub fn provide_app(
        &self,
        workspace: &str,
        app: &str,
        reach: Reach,
        answers: Arc<dyn ActionProvider + Send + Sync>,
    ) {
        self.provide(
            &tilo_actions::remote::application_provider_token(workspace, app),
            app,
            reach,
            answers,
        );
    }

    /// Stop answering `app`'s calls in `workspace` with `answers`: a call that names them from
    /// now on finds no process. A provider registered since under that name is left as it is.
    pub fn withdraw_app(
        &self,
        workspace: &str,
        app: &str,
        answers: &Arc<dyn ActionProvider + Send + Sync>,
    ) {
        self.withdraw(
            &tilo_actions::remote::application_provider_token(workspace, app),
            answers,
        );
    }

    /// The provider id `app`'s process registers under, in `workspace`.
    #[must_use]
    pub fn app_provider(workspace: &str, app: &str) -> String {
        tilo_actions::remote::application_provider_token(workspace, app)
    }

    /// The registered provider that published the address `target` names, whichever provider
    /// token the call carries, and that a caller from `from` may reach: a workspace's own
    /// `app.<app>#open` is the open of the app a provider places, and is answered by it. A call
    /// from another device ([`Reach::Devices`]) reaches only a provider that reaches devices.
    /// `None` when no such provider publishes it.
    #[must_use]
    pub fn published(&self, target: &RemoteActionTarget, from: Reach) -> Option<Registered> {
        self.0
            .provided
            .read_recover()
            .values()
            .find(|registered| {
                (!from.devices() || registered.reach.devices())
                    && locate(&registered.published, &target.address).is_some()
            })
            .cloned()
    }

    /// The publishers the registered providers reaching devices list for `caller`: what joins a
    /// listing naming no provider, read from the providers that answer at once. A provider whose
    /// listing has to be asked elsewhere contributes nothing here; its own rows reach a caller
    /// that names it.
    #[must_use]
    pub fn publishers(&self, caller: &Caller, query: &CatalogueQuery) -> Vec<CatalogueProvider> {
        // Copied out before they are asked, so a provider's own listing may reach this registry
        // without waiting on the read this takes.
        let answers: Vec<Arc<dyn ActionProvider + Send + Sync>> = self
            .0
            .provided
            .read_recover()
            .values()
            .filter(|registered| registered.reach.devices())
            .map(|registered| Arc::clone(&registered.answers))
            .collect();
        let mut publishers = Vec::new();
        for answers in answers {
            let mut answer = answers.query(caller, query.clone());
            if let PendingPoll::Ready(Ok(page)) = answer.poll_ready() {
                publishers.extend(page.providers);
            }
        }
        publishers
    }

    /// Take the requests of `host`, the host that placed this device for records of one
    /// workspace, as the action service admitted them: this device keeps no copy of that
    /// workspace whose grants could decide them, and its action service takes no other device's
    /// (`tilo_peer_state::actions::ActionService::answer_only`).
    pub fn answer_placing_host(&self, host: [u8; 32]) {
        let _ = self.0.placing_host.set(host);
    }

    /// The catalogue a device placed for records reads here, as `caller`: the rows and
    /// publishers of the providers registered for `device` alone ([`Self::serve_placed`]), each
    /// answering at once. Nothing else this device answers is that device's to call, so nothing
    /// else is listed to it.
    #[must_use]
    pub fn placed_catalogue(
        &self,
        caller: &Caller,
        device: [u8; 32],
        query: &CatalogueQuery,
    ) -> CataloguePage {
        let answers: Vec<Arc<dyn ActionProvider + Send + Sync>> = self
            .0
            .provided
            .read_recover()
            .values()
            .filter(|registered| {
                registered.placed_for == Some(device)
                    && query
                        .provider
                        .as_deref()
                        .is_none_or(|named| named == &*registered.provider)
            })
            .map(|registered| Arc::clone(&registered.answers))
            .collect();
        // Every row each provider offers, read page by page to its end, then paged here as one
        // listing: a page this answers holds what fits a frame, and its cursor reaches the rest.
        let mut rows = Vec::new();
        let mut providers = Vec::new();
        let mut revisions = Vec::new();
        for answers in answers {
            let mut asked = CatalogueQuery {
                cursor: Vec::new(),
                revision: None,
                max_results: u32::MAX,
                ..query.clone()
            };
            loop {
                let PendingPoll::Ready(Ok(listed)) =
                    answers.query(caller, asked.clone()).poll_ready()
                else {
                    break;
                };
                rows.extend(listed.rows);
                providers.extend(listed.providers);
                revisions.push(listed.revision.clone());
                // A cursor that does not move on would read the same page forever.
                if listed.exhausted
                    || listed.next_cursor.is_empty()
                    || listed.next_cursor == asked.cursor
                {
                    break;
                }
                asked.cursor = listed.next_cursor;
                asked.revision = Some(listed.revision);
            }
        }
        let revision = format!(
            "placed:{}",
            blake3::hash(revisions.join("\n").as_bytes()).to_hex()
        );
        let mut page = CataloguePage::from_rows(revision, rows, query);
        if query.cursor.is_empty() {
            page.providers = providers;
        }
        page
    }

    /// What each registered provider publishes at fixed addresses, as the package an app's code
    /// calls it by.
    #[must_use]
    pub fn packages(&self) -> Vec<ProvidedPackage> {
        self.0
            .provided
            .read_recover()
            .values()
            .filter_map(|registered| ProvidedPackage::of(&registered.published))
            .collect()
    }

    /// Answer the calls that name `provider` with `answers` while it serves them, decided by the
    /// grants of the application `app` and reaching as far as `reach`: a provider a program
    /// serves over its connection, which goes with it ([`Self::withdraw`]). Refused where another
    /// provider is registered under that name: a program never takes over one.
    ///
    /// # Errors
    /// Another provider answers under `provider`.
    pub fn serve(
        &self,
        provider: &str,
        app: &str,
        reach: Reach,
        answers: Arc<dyn ActionProvider + Send + Sync>,
    ) -> Result<(), String> {
        self.serve_for(provider, app, reach, answers, None)
    }

    /// [`Self::serve`] for the calls of `device`, a device this host placed: what it asks of
    /// `provider`, the window it was placed for, reaches `answers`, which decides it as that
    /// device's. Every other device's calls are decided by the workspace's grants, as for any
    /// provider.
    ///
    /// # Errors
    ///
    /// Another provider is served under `provider` already.
    pub fn serve_placed(
        &self,
        provider: &str,
        app: &str,
        device: [u8; 32],
        answers: Arc<dyn ActionProvider + Send + Sync>,
    ) -> Result<(), String> {
        self.serve_for(provider, app, Reach::Devices, answers, Some(device))
    }

    fn serve_for(
        &self,
        provider: &str,
        app: &str,
        reach: Reach,
        answers: Arc<dyn ActionProvider + Send + Sync>,
        placed_for: Option<[u8; 32]>,
    ) -> Result<(), String> {
        let mut served = self.0.provided.write_recover();
        if let Some(registered) = served.get(provider)
            && !Arc::ptr_eq(&registered.answers, &answers)
        {
            return Err(format!("{provider} is served on this device already"));
        }
        served.insert(
            Arc::from(provider),
            Registered {
                provider: Arc::from(provider),
                app: Arc::from(app),
                reach,
                published: answers.published(),
                answers,
                placed_for,
                in_workspace: None,
            },
        );
        drop(served);
        Ok(())
    }

    /// Answer no more calls of `provider` with `answers`: a call that names it from now on finds
    /// no provider. A provider registered since under that name by another is left as it is.
    pub fn withdraw(&self, provider: &str, answers: &Arc<dyn ActionProvider + Send + Sync>) {
        let mut served = self.0.provided.write_recover();
        if served
            .get(provider)
            .is_some_and(|registered| Arc::ptr_eq(&registered.answers, answers))
        {
            served.remove(provider);
        }
    }

    /// The provider registered under `provider`, under the app a record provider names
    /// (`record:<app id>:<record id>`), or under the app an application token names in its
    /// record (`application:<workspace>:<app>:<record>`): a record is the app, live, served by
    /// the device holding it.
    #[must_use]
    pub fn get(&self, provider: &str) -> Option<Registered> {
        let served = self.0.provided.read_recover();
        served
            .get(provider)
            .cloned()
            .or_else(|| resources(&served, provider))
            .or_else(|| {
                let (app, _) = tilo_actions::remote::resource_parts(provider)?;
                served.get(app).cloned()
            })
            .or_else(|| {
                let (workspace, app) = tilo_actions::remote::application_provider(provider)?;
                let token = tilo_actions::remote::application_provider_token(workspace, app);
                served.get(token.as_str()).cloned()
            })
    }

    /// Answer for the seat with `answers` from now on: the services of the screen, the sound, the
    /// person's settings and the windows a desktop session holds for this device
    /// (the app host's seat). It is kept apart from every provider a call names: no app, agent
    /// or other device reaches it, only the services this device's broker forwards to it, with
    /// the facts it admitted the asking app on. One seat at a time; a later shell's replaces it.
    pub fn serve_seat(
        &self,
        answers: Arc<dyn ActionProvider + Send + Sync>,
        hand_over: Option<HandOver>,
    ) {
        *self.0.seat.write_recover() = Some(ServedSeat { answers, hand_over });
    }

    /// No seat answers through `answers` any more; a seat served since by another is left as it
    /// is.
    pub fn withdraw_seat(&self, answers: &Arc<dyn ActionProvider + Send + Sync>) {
        let mut seat = self.0.seat.write_recover();
        if seat
            .as_ref()
            .is_some_and(|serving| Arc::ptr_eq(&serving.answers, answers))
        {
            *seat = None;
        }
    }

    /// What answers for the seat now, while a shell serves it.
    #[must_use]
    pub fn seat(&self) -> Option<Arc<dyn ActionProvider + Send + Sync>> {
        self.0
            .seat
            .read_recover()
            .as_ref()
            .map(|seat| Arc::clone(&seat.answers))
    }

    /// The seat now, while a shell serves it, with how a call hands that shell a file: both of
    /// the one shell.
    #[must_use]
    pub fn served_seat(&self) -> Option<ServedSeat> {
        self.0.seat.read_recover().clone()
    }

    /// Send the typed actions of the records of each workspace `hosts` places on another device
    /// to that host from now on, those `answer` says ([`Self::invoke`]).
    pub fn place_on_hosts(&self, hosts: HostsOf, answer: HostsAnswer) {
        *self.0.hosts.write_recover() = Some((hosts, answer));
    }

    /// The actions of the host the workspace `caller` asks in is placed on, for `request` when it
    /// names a typed action of one of that workspace's records, or asks the host for what a
    /// workspace's VM device holds no keys for itself
    /// ([`tilo_action_services::environment_device`]: its media keys, a window's share, a vault
    /// item's text for a fill), which the host that placed it answers.
    fn host_of(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<Arc<dyn ActionProvider + Send + Sync>> {
        if !tilo_actions::remote::is_resource_provider(&request.target.provider)
            && tilo_action_services::environment_device::action_of(&request.target).is_none()
        {
            return None;
        }
        let (hosts, _) = self.0.hosts.read_recover().clone()?;
        hosts(caller.workspace()?)
    }

    /// Remember that the registered provider `provider` took invocation `id`: what its
    /// observation and cancellation reach while the call it runs goes on.
    pub fn took(&self, id: InvocationId, provider: &str) {
        self.take(id, Taker::Registered(provider.into()));
    }

    /// The actions of the host `workspace` is placed on, when this device keeps no copy of it and
    /// every call made in it here is its host's ([`HostsAnswer::Every`]): a device placed for
    /// records of that one workspace, which admits nothing itself. `None` for any other device or
    /// workspace, and while the host is not known yet.
    #[must_use]
    pub fn host_answering_every(
        &self,
        workspace: &str,
    ) -> Option<Arc<dyn ActionProvider + Send + Sync>> {
        let (hosts, answer) = self.0.hosts.read_recover().clone()?;
        if answer != HostsAnswer::Every {
            return None;
        }
        hosts(workspace)
    }

    /// Whether the hosts answer every typed action of a placed workspace's records
    /// ([`HostsAnswer::Every`]).
    fn hosts_answer_every(&self) -> bool {
        self.0
            .hosts
            .read_recover()
            .as_ref()
            .is_some_and(|(_, answer)| *answer == HostsAnswer::Every)
    }

    /// Remember that `taker` took invocation `id`.
    fn take(&self, id: InvocationId, taker: Taker) {
        let mut took = self.0.taken.lock_recover();
        if took.len() >= TAKEN_KEPT {
            took.clear();
        }
        took.insert(id, taker);
    }

    /// Hand `request`, a call made on this device as `caller`, to the provider it names: the one
    /// registered under that name, or the one applying the typed actions of the resource it names
    /// (registered under [`tilo_actions::remote::RESOURCE_PROVIDER`]); else, for a record of a
    /// workspace placed on a host ([`Self::place_on_hosts`]), that host, which runs its records'
    /// actions; else the one registered for its app. `None` when none is. On a device that keeps
    /// no copy of its placed workspace ([`HostsAnswer::Every`]) the host takes such a record's
    /// call first, before any provider here.
    #[must_use]
    pub fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Option<Pending<Result<InvocationReceipt, ActionCallError>>> {
        let provider = request.target.provider.as_str();
        let named = self.0.provided.read_recover().get(provider).cloned();
        let registered = named.or_else(|| resources(&self.0.provided.read_recover(), provider));
        if (registered.is_none() || self.hosts_answer_every())
            && let Some(host) = self.host_of(caller, &request)
        {
            self.take(request.invocation, Taker::Host(Arc::clone(&host)));
            return Some(host.invoke(caller, request));
        }
        // An address a registered provider published is that provider's, whichever provider token
        // the call names: a workspace's own `app.<app>#open` is the open of an app this device
        // places.
        let registered = registered
            .or_else(|| moves(&self.0.provided.read_recover(), &request.target))
            .or_else(|| self.published(&request.target, Reach::ThisDevice))
            .or_else(|| self.get(provider))?;
        Some(registered.answers.invoke(caller, request))
    }

    /// What the provider registered here as `provider` declares the action `address` as, with
    /// no argument: the need a turn of it this device runs is judged at beside its app's surface,
    /// as a call of it from another device is ([`Self::device_call`]). `None` where no provider
    /// registered here declares it.
    #[must_use]
    pub(super) fn declared_need(
        &self,
        provider: &str,
        address: &tilo_actions::ActionAddress,
    ) -> Option<tilo_actions::ActionNeed> {
        let registered = self.get(provider)?;
        let declared = registered
            .answers
            .invocation(address, None)
            .or_else(|| tilo_actions::api::described(&registered.published, address))?;
        Some(tilo_actions::ActionNeed {
            kind: declared.contract.kind,
            field: declared.contract.field,
            privileged: false,
        })
    }

    /// Every registered provider.
    #[must_use]
    pub fn all(&self) -> Vec<Registered> {
        self.0.provided.read_recover().values().cloned().collect()
    }

    /// The registered provider reaching devices that `provider` names.
    fn for_devices(&self, provider: &str) -> Option<Registered> {
        self.get(provider)
            .filter(|registered| registered.reach.devices())
    }

    /// Whether a call from another device naming `provider` reaches a registered provider.
    #[must_use]
    pub fn serves(&self, provider: &str) -> bool {
        self.for_devices(provider).is_some()
    }

    fn taker(&self, id: &InvocationId) -> Option<Arc<dyn ActionProvider + Send + Sync>> {
        let taker = self.0.taken.lock_recover().get(id)?.clone();
        match taker {
            Taker::Registered(name) => self.get(&name).map(|registered| registered.answers),
            Taker::Host(host) => Some(host),
        }
    }

    /// What the provider `query` names offers another device's `caller`; `None` when no provider
    /// reaching devices is registered under it.
    #[must_use]
    pub fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Option<Pending<Result<CataloguePage, ActionCallError>>> {
        let registered = self.for_devices(query.provider.as_deref()?)?;
        Some(registered.answers.query(caller, query))
    }

    /// Hand `request`, another device's call, to the provider it names, or the one that published
    /// the address it calls, once the workspace `caller` asks in admits that device to the action
    /// by its grants, read from this device's copy in `workspaces`. `None` when no provider
    /// reaching devices answers it.
    pub(super) fn device_call(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        (workspaces, surfaces): (
            &Arc<super::device_actions::WorkspacesMap>,
            &Arc<collab_types::app_surface::Surfaces>,
        ),
        runtime: &tokio::runtime::Handle,
    ) -> Option<Pending<Result<InvocationReceipt, ActionCallError>>> {
        let registered = self
            .for_devices(&request.target.provider)
            .or_else(|| self.published(&request.target, Reach::Devices))?;
        let (Some(device), Some(workspace)) = (
            caller.device_public_key().copied(),
            caller.workspace().map(str::to_owned),
        ) else {
            return Some(Pending::ready(Err(ActionCallError::NotPermitted)));
        };
        // A record's actions are decided as its own app's ([`resources`]).
        let app = Arc::clone(&registered.app);
        let providers = self.clone();
        let workspaces = Arc::clone(workspaces);
        let surfaces = Arc::clone(surfaces);
        let caller = caller.clone();
        let (completer, answer) = tilo_actions::remote::pending();
        // A device a host placed has no grants of its own: it reaches the provider registered for
        // it and the records its certificate names, nothing else.
        let placed = caller.placed_records().map(|placed| {
            registered.placed_for == Some(device)
                || placed.reaches(&crate::environment::document_channel(
                    &app,
                    &request.target.address.resource,
                ))
        });
        if placed == Some(false) {
            return Some(Pending::ready(Err(ActionCallError::NotPermitted)));
        }
        let placing_host = self.0.placing_host.get() == Some(&device);
        let argument = request.argument.to_slot();
        // Only a call the workspace's grants decide is judged by what its action is declared as.
        let judged = placed.is_none() && !placing_host;
        let declared = judged
            .then(|| {
                self.descriptor(
                    &registered,
                    &caller,
                    &request.target.address,
                    argument.as_deref(),
                )
            })
            .flatten();
        // The kind of the record the call names, as the provider that publishes it declares it:
        // the node its app's own dispatch decides the action at names it, so an answer the
        // person gave for the action reaches the call here too.
        let resource_kind = judged
            .then(|| {
                declared
                    .as_ref()
                    .map(|declared| declared.resource_kind.clone())
                    .filter(|kind| !kind.is_empty())
                    .or_else(|| {
                        self.published(&request.target, Reach::Devices)
                            .and_then(|published| {
                                locate(&published.published, &request.target.address)
                                    .map(|(resource, _)| resource.kind.clone())
                            })
                    })
            })
            .flatten();
        runtime.spawn(async move {
            let address = &request.target.address;
            if judged {
                let permitted = permits(
                    (&workspaces, &surfaces),
                    declared.as_ref(),
                    resource_kind.as_deref(),
                    device,
                    &workspace,
                    &app,
                    (&address.resource, &address.action),
                )
                .await;
                // A grant decides who may call; what the action's contract asks of its person
                // it asks of whoever calls, on the device they called from.
                let asked = declared.as_ref().is_none_or(|declared| {
                    declared
                        .contract
                        .confirmation_for_argument(argument.as_deref())
                        .met_by(request.confirmation.as_ref())
                });
                if let Err(refused) = permitted.and_then(|()| {
                    asked
                        .then_some(())
                        .ok_or_else(ActionCallError::confirmation_required)
                }) {
                    completer.complete(Err(refused));
                    return;
                }
            }
            providers.take(
                request.invocation,
                Taker::Registered(Arc::clone(&registered.provider)),
            );
            let receipt = registered.answers.invoke(&caller, request).await;
            completer.complete(receipt.unwrap_or(Err(ActionCallError::Unanswered)));
        });
        Some(answer)
    }

    /// The action `address` names as `registered` declares it to `caller`, with `argument` (its
    /// slot form): as that argument makes it where the provider says, else as `registered` or
    /// another provider registered for its app (the app's own process, serving it) publishes it,
    /// else as its catalogue lists it now. `None` where none of them declares it.
    #[must_use]
    pub fn descriptor(
        &self,
        registered: &Registered,
        caller: &Caller,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Option<tilo_actions::ActionDescriptor> {
        if let Some(descriptor) = registered.answers.invocation(address, argument) {
            return Some(descriptor);
        }
        let published =
            |resources: &[ResourceSpec]| tilo_actions::api::described(resources, address);
        published(&registered.published)
            .or_else(|| {
                self.0
                    .provided
                    .read_recover()
                    .values()
                    .filter(|other| other.app == registered.app)
                    .find_map(|other| published(&other.published))
            })
            .or_else(|| {
                Self::rows(registered, caller)
                    .into_iter()
                    .filter_map(|row| row.action)
                    .find(|action| &action.address == address)
            })
    }

    /// What `registered` lists for `caller`, as it answers at once: a registered provider lists
    /// what it already keeps, never what it must first go and ask.
    #[must_use]
    pub fn rows(
        registered: &Registered,
        caller: &Caller,
    ) -> Vec<tilo_actions::remote::CatalogueRow> {
        let mut rows = Vec::new();
        let mut query = CatalogueQuery {
            provider: Some(registered.provider.to_string()),
            max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
            ..CatalogueQuery::default()
        };
        loop {
            let mut listed = registered.answers.query(caller, query.clone());
            let tilo_actions::remote::PendingPoll::Ready(Ok(page)) = listed.poll_ready() else {
                return rows;
            };
            rows.extend(page.rows);
            if page.next_cursor.is_empty() || page.exhausted {
                return rows;
            }
            query.cursor = page.next_cursor;
            query.revision = Some(page.revision);
        }
    }

    /// Where invocation `id`, taken from another device by a registered provider or sent to a
    /// workspace's host, stands for `caller`; `None` when neither took it.
    #[must_use]
    pub fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<Option<InvocationState>> {
        Some(self.taker(id)?.observe(caller, id))
    }

    /// When invocation `id`, taken from another device by a registered provider or sent to a
    /// workspace's host, may have changed; `None` when neither took it.
    #[must_use]
    pub fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Option<Pending<()>>> {
        Some(self.taker(id)?.next_change(caller, id))
    }

    /// Ask the registered provider that took invocation `id` from another device, or the host it
    /// was sent to, to stop it; `None` when neither took it.
    #[must_use]
    pub fn cancel(
        &self,
        caller: &Caller,
        id: &InvocationId,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        Some(self.taker(id)?.cancel(caller, id))
    }
}

/// The child of a workspace's own tree `resource` names (`vault` for
/// `workspace.permissions:<id>/vault`), when it names one.
fn tree_child(resource: &str) -> Option<&str> {
    let named =
        resource.strip_prefix(tilo_actions::remote::workspace_governance_resource("").as_str())?;
    named.split_once('/').map(|(_, child)| child)
}

/// A provider answering in a workspace, on this device's own dispatch: reached through the
/// workspaces' governance, which admits a call made here as it admits every call of a workspace's
/// own tree and hands it on with the certificate of this device where the caller is this device.
struct ThroughGovernance(WeakProviders);

impl ThroughGovernance {
    fn governance(&self) -> Result<Arc<dyn ActionProvider + Send + Sync>, ActionCallError> {
        self.0
            .upgrade()
            .and_then(|providers| {
                providers.get(tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER)
            })
            .map(|registered| registered.answers)
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: "this device serves no workspaces yet".into(),
            })
    }
}

impl ActionProvider for ThroughGovernance {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        match self.governance() {
            Ok(governance) => governance.query(caller, query),
            Err(error) => Pending::ready(Err(error)),
        }
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        match self.governance() {
            Ok(governance) => governance.invoke(caller, request),
            Err(error) => Pending::ready(Err(error)),
        }
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.governance().ok()?.observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.governance().ok()?.next_change(caller, id)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        match self.governance() {
            Ok(governance) => governance.cancel(caller, id),
            Err(error) => Pending::ready(Err(error)),
        }
    }
}

/// The provider moving the record `target` names to another workspace, when `target` is a move:
/// any app's record moves through the store of the workspace that keeps it, as the caller's write
/// on both sides, whichever app it is of, decided as that app's ([`record_of`]).
fn moves(
    served: &BTreeMap<Arc<str>, Registered>,
    target: &RemoteActionTarget,
) -> Option<Registered> {
    if target.address.action != tilo_resource_store::MOVE_VERB {
        return None;
    }
    record_of(served, &target.provider)
}

/// The provider applying the typed actions of the built-in record `provider` names, registered
/// under the resource provider itself, when it applies that record's ([`record_of`]).
fn resources(served: &BTreeMap<Arc<str>, Registered>, provider: &str) -> Option<Registered> {
    record_of(served, provider).filter(|resources| resources.answers.serves(provider))
}

/// The provider registered under the resource provider itself, as it answers the record
/// `provider` names (`record:<app>:<id>`): decided by the grants of the record's own app, never
/// of the provider that applies its typed actions. Every path a call of such a record takes, this
/// device's own dispatch and another device's call alike, admits it under that app, so a rule on
/// the app's node holds whichever way the call came.
fn record_of(served: &BTreeMap<Arc<str>, Registered>, provider: &str) -> Option<Registered> {
    let (app, _) = tilo_actions::remote::resource_parts(provider)?;
    let registered = served.get(tilo_actions::remote::RESOURCE_PROVIDER)?;
    Some(Registered {
        app: Arc::from(app),
        ..registered.clone()
    })
}

/// Whether `workspace`'s grants let `device` run `action` on `resource` of the application `app`
/// now, judged at what the action is declared as: by the provider that answers it (`declared`)
/// and by `app`'s surface in the workspace's store (`surfaces` names what this device knows of
/// those), the greater where both declare it. Its kind, the field its declaration places it
/// under and the kind of its resource are all judged
/// ([`tilo_peer_state::StatePeer::app_action_answer`]): a commenter's verb under the `comments`
/// is theirs, though the same kind on the record is not, and an answer its person gave for the
/// action reaches the call at the kind its app publishes it on. The refusal its caller reads when
/// they do not; the answer that waits for its person is the one the caller asks again, since it
/// may not have reached this device yet. An action declared nowhere is refused: nothing says what
/// it is able to do.
async fn permits(
    (workspaces, surfaces): (
        &super::device_actions::WorkspacesMap,
        &collab_types::app_surface::Surfaces,
    ),
    declared: Option<&tilo_actions::ActionDescriptor>,
    resource_kind: Option<&str>,
    device: [u8; 32],
    workspace: &str,
    app: &str,
    (resource, action): (&str, &str),
) -> Result<(), ActionCallError> {
    let kept = workspaces.lock().await.get(workspace).cloned();
    let Some(kept) = kept else {
        return Err(ActionCallError::Refused {
            reason: "this device does not keep that workspace".into(),
        });
    };
    let mut kept = kept.lock().await;
    let declared_kind = declared.map(|declared| declared.contract.kind);
    let declared_field = declared.and_then(|declared| declared.contract.field.clone());
    let Some(need) = the_greater(
        declared_kind.map(|kind| tilo_actions::ActionNeed {
            kind,
            field: declared_field,
            privileged: false,
        }),
        kept.call_need(surfaces, app, action),
    ) else {
        return Err(ActionCallError::Refused {
            reason: format!("{app} declares no action {resource}#{action}"),
        });
    };
    let answered = kept.peer().app_action_answer(
        device,
        (app, resource, action),
        &need,
        resource_kind,
        tilo_clock::wall(),
    );
    drop(kept);
    match answered {
        Ok(tilo_action_policy::Answer::Allow) => Ok(()),
        // The person's answer for this action has not reached this device, or they answered it
        // nowhere here: the call is not run, and whoever asked asks again once the workspace's
        // answer is here.
        Ok(tilo_action_policy::Answer::Ask) => Err(ActionCallError::NotPermitted),
        Ok(tilo_action_policy::Answer::Deny) => Err(ActionCallError::Refused {
            reason: format!(
                "your grants in this workspace do not let this device run {resource}#{action} of \
                 {app}"
            ),
        }),
        Err(error) => Err(ActionCallError::Refused {
            reason: format!("this device is not admitted to the workspace: {error}"),
        }),
    }
}

/// The greater of two declarations of one action, `None` where neither declares it: the one able
/// to do more, and the field it places the action under where its declaration names one. Two
/// declarations of one kind differ at most in that field, and a declared field bounds the action
/// as the app's own dispatch bounds it.
pub(super) fn the_greater(
    declared: Option<tilo_actions::ActionNeed>,
    surface: Option<tilo_actions::ActionNeed>,
) -> Option<tilo_actions::ActionNeed> {
    match (declared, surface) {
        (Some(mut declared), Some(surface)) => {
            if surface.kind > declared.kind {
                return Some(surface);
            }
            if declared.kind == surface.kind && declared.field.is_none() {
                declared.field = surface.field;
            }
            Some(declared)
        }
        (declared, surface) => declared.or(surface),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use tilo_actions::remote::InvokeArgument;

    fn request(token: &str) -> InvokeRequest {
        InvokeRequest {
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: token.into(),
                address: tilo_actions::ActionAddress::new("console.window", "field-edit"),
            },
            argument: InvokeArgument::default(),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            on_behalf_of: None,
            appearance: None,
            signed: None,
        }
    }

    /// A provider answering in a workspace, which records the certificate each call reached it
    /// with.
    struct Certified(Arc<Mutex<Vec<Option<tilo_user_identity::DeviceCert>>>>);

    impl WorkspaceAnswers for Certified {
        fn offer<'a>(
            &'a self,
            _call: WorkspaceCall<'a>,
            _query: &'a CatalogueQuery,
        ) -> Answering<'a, Vec<(CatalogueProvider, Vec<tilo_actions::remote::CatalogueRow>)>>
        {
            Box::pin(std::future::ready(Vec::new()))
        }

        fn answer<'a>(
            &'a self,
            call: WorkspaceCall<'a>,
            _request: &'a InvokeRequest,
        ) -> Answering<'a, Result<Answered, ActionCallError>> {
            self.0
                .lock()
                .expect("the certificates seen")
                .push(call.certificate.cloned());
            Box::pin(std::future::ready(Ok(Answered::Value(
                serde_json::Value::Null,
            ))))
        }
    }

    /// A provider registered to answer in a workspace is handed the certificate the handshake
    /// of the caller's connection verified, as the workspace's own actions are; where the
    /// transport named none, the certificate the workspace holds for the caller's device.
    #[tokio::test]
    async fn a_provider_reached_over_a_handshake_sees_its_certificate() {
        use super::super::fixtures;
        let dir = tempfile::tempdir().expect("a scratch directory");
        let (key, owner) = fixtures::hosting_device();
        let host = fixtures::test_host(dir.path(), &key, &owner).await;
        let workspaces = host.workspaces();
        let (workspace, workspace_id, governance) = fixtures::hosted_governed(workspaces).await;
        let guest = fixtures::admit(workspaces, &workspace, &owner, 1).await;
        let seen = Arc::new(Mutex::new(Vec::new()));
        workspaces.providers().answer_in_workspaces(
            "test.certified",
            "test.certified",
            Arc::new(Certified(Arc::clone(&seen))),
        );
        let caller = fixtures::caller(&guest).in_workspace(&workspace);
        // The handshake verified a certificate of its own for the guest's device: a newer one
        // than the workspace's record holds.
        let mut handshake = guest.clone();
        handshake.issued_at_ns += 1;
        fixtures::settled(
            governance
                .in_workspace_as(workspace_id, &handshake)
                .invoke(&caller, request("test.certified")),
        )
        .await
        .expect("the provider answers over the handshake");
        fixtures::settled(
            governance
                .in_workspace(workspace_id)
                .invoke(&caller, request("test.certified")),
        )
        .await
        .expect("the provider answers where the transport named no certificate");
        assert_eq!(
            *seen.lock().expect("the certificates seen"),
            vec![Some(handshake), Some(guest)],
        );
    }
}
