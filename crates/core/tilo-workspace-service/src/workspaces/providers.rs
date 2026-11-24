//! The providers registered on a device, by the provider id a call names: the one registry every
//! caller of them reaches.
//!
//! An embedder registers a provider with the application whose grants decide its actions and how
//! far it reaches ([`Reach`]). A call on this device reaches it through the device's dispatch
//! (`tilo do -w`, an agent's `tilo_do`, a view's button, the session's own steps), which admits it
//! against the workspace's policy under that application, asks the person where the action's
//! contract says so, keeps the call's record and hands it to the provider's
//! [`run`](ActionProvider::run). A call from another device over the device action service
//! (`tilo-actions/1`) reaches a provider registered to reach devices ([`Providers::from_device`]),
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
    hosts: std::sync::RwLock<Option<HostsOf>>,
}

/// The actions of the host a workspace is placed on, by workspace: `None` for one this device
/// hosts itself or holds alone.
pub type HostsOf = Arc<dyn Fn(&str) -> Option<Arc<dyn ActionProvider + Send + Sync>> + Send + Sync>;

/// What took an invocation: a provider registered here, by name (one withdrawn since is asked no
/// more), or the host of the workspace it was asked in.
#[derive(Clone)]
enum Taker {
    Registered(Arc<str>),
    Host(Arc<dyn ActionProvider + Send + Sync>),
}

impl Providers {
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
            },
        );
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
        let mut provided = self.0.provided.write_recover();
        if let Some(registered) = provided.get(provider)
            && !Arc::ptr_eq(&registered.answers, &answers)
        {
            return Err(format!("{provider} is served on this device already"));
        }
        provided.insert(
            Arc::from(provider),
            Registered {
                provider: Arc::from(provider),
                app: Arc::from(app),
                reach,
                published: answers.published(),
                answers,
            },
        );
        Ok(())
    }

    /// Answer no more calls of `provider` with `answers`: a call that names it from now on finds
    /// no provider. A provider registered since under that name by another is left as it is.
    pub fn withdraw(&self, provider: &str, answers: &Arc<dyn ActionProvider + Send + Sync>) {
        let mut provided = self.0.provided.write_recover();
        if provided
            .get(provider)
            .is_some_and(|registered| Arc::ptr_eq(&registered.answers, answers))
        {
            provided.remove(provider);
        }
    }

    /// The provider registered under `provider`, under the app a record provider names
    /// (`record:<app id>:<record id>`), or under the app an application token names in its
    /// record (`application:<workspace>:<app>:<record>`): a record is the app, live, served by
    /// the device holding it.
    #[must_use]
    pub fn get(&self, provider: &str) -> Option<Registered> {
        let provided = self.0.provided.read_recover();
        provided
            .get(provider)
            .or_else(|| {
                let (app, _) = tilo_actions::remote::resource_parts(provider)?;
                provided.get(app)
            })
            .or_else(|| {
                let (workspace, app) = tilo_actions::remote::application_provider(provider)?;
                let token = tilo_actions::remote::application_provider_token(workspace, app);
                provided.get(token.as_str())
            })
            .cloned()
    }

    /// Send the typed actions of the records of each workspace `hosts` places on another device
    /// to that host from now on ([`Self::invoke`]).
    pub fn place_on_hosts(&self, hosts: HostsOf) {
        *self.0.hosts.write_recover() = Some(hosts);
    }

    /// The actions of the host the workspace `caller` asks in is placed on, for `request` when it
    /// names a typed action of one of that workspace's records.
    fn host_of(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<Arc<dyn ActionProvider + Send + Sync>> {
        if !tilo_actions::remote::is_resource_provider(&request.target.provider) {
            return None;
        }
        let hosts = self.0.hosts.read_recover().clone()?;
        hosts(caller.workspace()?)
    }

    /// Remember that `taker` took invocation `id`.
    fn take(&self, id: InvocationId, taker: Taker) {
        let mut taken = self.0.taken.lock_recover();
        if taken.len() >= TAKEN_KEPT {
            taken.clear();
        }
        taken.insert(id, taker);
    }

    /// Hand `request`, a call made on this device as `caller`, to the provider it names: the one
    /// registered under that name, or the one applying the typed actions of the resource it names
    /// (registered under [`tilo_actions::remote::RESOURCE_PROVIDER`]); else, for a record of a
    /// workspace placed on a host ([`Self::place_on_hosts`]), that host, which runs its records'
    /// actions; else the one registered for its app. `None` when none is.
    #[must_use]
    pub fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Option<Pending<Result<InvocationReceipt, ActionCallError>>> {
        let provider = request.target.provider.as_str();
        let named = self.0.provided.read_recover().get(provider).cloned();
        let registered = named.or_else(|| {
            self.0
                .provided
                .read_recover()
                .get(tilo_actions::remote::RESOURCE_PROVIDER)
                .filter(|resources| resources.answers.serves(provider))
                .cloned()
        });
        if registered.is_none()
            && let Some(host) = self.host_of(caller, &request)
        {
            self.take(request.invocation, Taker::Host(Arc::clone(&host)));
            return Some(host.invoke(caller, request));
        }
        // An address a registered provider published is that provider's, whichever provider token
        // the call names: a workspace's own `app.<app>#open` is the open of an app this device
        // places.
        let registered = registered
            .or_else(|| self.published(&request.target, Reach::ThisDevice))
            .or_else(|| self.get(provider))?;
        Some(registered.answers.invoke(caller, request))
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
    pub(super) fn from_device(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        workspaces: &Arc<super::device_actions::WorkspacesMap>,
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
        let providers = self.clone();
        let workspaces = Arc::clone(workspaces);
        let caller = caller.clone();
        let (completer, answer) = tilo_actions::remote::pending();
        runtime.spawn(async move {
            let address = &request.target.address;
            if let Err(refused) = permits(
                &workspaces,
                device,
                &workspace,
                &registered.app,
                &address.resource,
                &address.action,
            )
            .await
            {
                completer.complete(Err(refused));
                return;
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

/// Whether `workspace`'s grants let `device` run `action` on `resource` of the application `app`
/// now; the refusal its caller reads when they do not.
async fn permits(
    workspaces: &super::device_actions::WorkspacesMap,
    device: [u8; 32],
    workspace: &str,
    app: &str,
    resource: &str,
    action: &str,
) -> Result<(), ActionCallError> {
    let kept = workspaces.lock().await.get(workspace).cloned();
    let Some(kept) = kept else {
        return Err(ActionCallError::Refused {
            reason: "this device does not keep that workspace".into(),
        });
    };
    let permitted = kept.lock().await.peer().permits_app_action(
        device,
        app,
        resource,
        action,
        tilo_clock::wall(),
    );
    match permitted {
        Ok(true) => Ok(()),
        Ok(false) => Err(ActionCallError::Refused {
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use tilo_actions::remote::{
        CataloguePage, CatalogueQuery, InvokeArgument, InvokeRequest, Pending, PendingPoll,
        RemoteActionTarget, application_action_target, application_provider_token,
    };

    /// An app process's answers, which record the provider token each call reached it under.
    #[derive(Default)]
    struct Process(Mutex<Vec<String>>);

    impl ActionProvider for Process {
        fn query(
            &self,
            _caller: &Caller,
            _query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            Pending::ready(Ok(CataloguePage::default()))
        }

        fn invoke(
            &self,
            _caller: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            self.0
                .lock()
                .expect("the calls taken")
                .push(request.target.provider.to_string());
            Pending::ready(Err(ActionCallError::Unavailable {
                reason: "the app's answer is its own".into(),
            }))
        }

        fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
            None
        }
    }

    fn request(token: &str) -> InvokeRequest {
        InvokeRequest {
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: token.into(),
                address: tilo_actions::ActionAddress::new("scratchpad.note", "field-edit"),
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

    /// An app process registers under its workspace's token and is taken back when it ends; a
    /// call naming the token reaches it, and a device reaches it when it reaches devices.
    #[test]
    fn an_apps_process_is_registered_under_its_token_and_taken_back_when_it_ends() {
        const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
        const APP: &str = "so.tilo.scratchpad";
        let providers = Providers::default();
        let process = Arc::new(Process::default());
        assert_eq!(
            application_provider_token(WORKSPACE, APP),
            Providers::app_provider(WORKSPACE, APP),
            "the app's process registers under the token a call names"
        );
        let answers: Arc<dyn ActionProvider + Send + Sync> = process.clone();
        providers.provide_app(WORKSPACE, APP, Reach::Devices, Arc::clone(&answers));
        let registered = providers
            .get(&Providers::app_provider(WORKSPACE, APP))
            .expect("the token names the app's process");
        assert_eq!(
            registered.app.as_ref(),
            APP,
            "its calls are decided by the app's own grants"
        );
        assert!(providers.serves(&Providers::app_provider(WORKSPACE, APP)));

        let target = application_action_target(WORKSPACE, APP, "scratchpad.note", "field-edit");
        let caller = Caller::local_person("ada").in_workspace(WORKSPACE);
        let mut answer = providers
            .invoke(&caller, request(&target.provider))
            .expect("the call reaches the app's process");
        assert!(
            matches!(answer.poll_ready(), PendingPoll::Ready(Err(_))),
            "the process answered"
        );
        assert_eq!(
            process.0.lock().expect("the calls taken").clone(),
            vec![target.provider.clone()],
            "the call reached the app's process under its token"
        );

        providers.withdraw_app(WORKSPACE, APP, &answers);
        assert!(
            providers
                .get(&Providers::app_provider(WORKSPACE, APP))
                .is_none(),
            "a call after the process ended finds it no more"
        );
        assert!(!providers.serves(&Providers::app_provider(WORKSPACE, APP)));
    }
}
