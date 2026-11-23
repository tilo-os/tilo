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

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending,
};
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
    pub provider: &'static str,
    /// The application whose grants decide its actions.
    pub app: &'static str,
    /// How far it reaches.
    pub reach: Reach,
    /// What answers its calls.
    pub answers: Arc<dyn ActionProvider + Send + Sync>,
}

/// How many invocations from other devices a registered provider took are remembered for routing
/// their observations; the oldest are let go beyond it, and their provider is asked no more.
const TAKEN_KEPT: usize = 4096;

/// The providers registered on a device.
#[derive(Clone, Default)]
pub struct Providers(Arc<Inner>);

#[derive(Default)]
struct Inner {
    provided: std::sync::RwLock<BTreeMap<&'static str, Registered>>,
    /// Which registered provider took each invocation another device asked for, so its
    /// observations and its cancellation reach that provider.
    taken: std::sync::Mutex<HashMap<InvocationId, &'static str>>,
}

impl Providers {
    /// Answer the calls that name `provider` with `answers` from now on, decided by the grants of
    /// the application `app` and reaching as far as `reach`.
    pub fn provide(
        &self,
        provider: &'static str,
        app: &'static str,
        reach: Reach,
        answers: Arc<dyn ActionProvider + Send + Sync>,
    ) {
        self.0.provided.write_recover().insert(
            provider,
            Registered {
                provider,
                app,
                reach,
                answers,
            },
        );
    }

    /// The provider registered under `provider`, or under the app a record provider names
    /// (`record:<app id>:<record id>`): a record is the app, live, served by the device holding it.
    #[must_use]
    pub fn get(&self, provider: &str) -> Option<Registered> {
        let provided = self.0.provided.read_recover();
        provided
            .get(provider)
            .or_else(|| {
                let (app, _) = tilo_actions::remote::resource_parts(provider)?;
                provided.get(app)
            })
            .cloned()
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
        let name = *self.0.taken.lock_recover().get(id)?;
        self.get(name).map(|registered| registered.answers)
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

    /// Hand `request`, another device's call, to the provider it names once the workspace `caller`
    /// asks in admits that device to the action by its grants, read from this device's copy in
    /// `workspaces`. `None` when no provider reaching devices is registered under the name the
    /// request targets.
    pub(super) fn from_device(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        workspaces: &Arc<super::device_actions::WorkspacesMap>,
        runtime: &tokio::runtime::Handle,
    ) -> Option<Pending<Result<InvocationReceipt, ActionCallError>>> {
        let registered = self.for_devices(&request.target.provider)?;
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
                registered.app,
                &address.resource,
                &address.action,
            )
            .await
            {
                completer.complete(Err(refused));
                return;
            }
            {
                let mut taken = providers.0.taken.lock_recover();
                if taken.len() >= TAKEN_KEPT {
                    taken.clear();
                }
                taken.insert(request.invocation, registered.provider);
            }
            let receipt = registered.answers.invoke(&caller, request).await;
            completer.complete(receipt.unwrap_or(Err(ActionCallError::Unanswered)));
        });
        Some(answer)
    }

    /// Where invocation `id`, taken from another device by a registered provider, stands for
    /// `caller`; `None` when no registered provider took it.
    #[must_use]
    pub fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<Option<InvocationState>> {
        Some(self.taker(id)?.observe(caller, id))
    }

    /// When invocation `id`, taken from another device by a registered provider, may have changed;
    /// `None` when no registered provider took it.
    #[must_use]
    pub fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Option<Pending<()>>> {
        Some(self.taker(id)?.next_change(caller, id))
    }

    /// Ask the registered provider that took invocation `id` from another device to stop it;
    /// `None` when none did.
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
