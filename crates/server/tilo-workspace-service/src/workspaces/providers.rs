//! The providers a device's embedder registered ([`Workspaces::provide`](super::Workspaces::provide)),
//! and the one place every call of one is admitted and answered.
//!
//! A call reaches a registered provider from another device over the device action service
//! (`tilo-actions/1`), or from this device through its dispatch: `tilo do -w`, an agent's
//! `tilo_do`, a view's button. Both hand it to [`Providers::invoke`], which admits the caller's
//! device to the action by the grants of the workspace it asks in, read from this device's own
//! copy, before the provider sees it; a provider decides no grant itself. Following, waiting on and
//! cancelling the call go to the provider that took it.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending,
};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};

use super::device_actions::WorkspacesMap;

/// A registered provider.
type Provided = Arc<dyn ActionProvider + Send + Sync>;

/// How many invocations a registered provider took are remembered for routing their
/// observations; the oldest are let go beyond it, and their provider is asked no more.
const TAKEN_KEPT: usize = 4096;

/// The providers registered on this device, by the provider id a request names.
#[derive(Clone)]
pub struct Providers(Arc<Inner>);

struct Inner {
    provided: std::sync::RwLock<BTreeMap<&'static str, Provided>>,
    /// Which registered provider took each invocation, so its observations and its
    /// cancellation reach that provider.
    taken: std::sync::Mutex<HashMap<InvocationId, &'static str>>,
    /// The workspaces this device keeps, whose grants admit a call.
    workspaces: Arc<WorkspacesMap>,
    runtime: tokio::runtime::Handle,
}

impl Providers {
    pub(super) fn new(workspaces: Arc<WorkspacesMap>, runtime: tokio::runtime::Handle) -> Self {
        Self(Arc::new(Inner {
            provided: std::sync::RwLock::default(),
            taken: std::sync::Mutex::default(),
            workspaces,
            runtime,
        }))
    }

    /// Answer the calls that name `provider` with `answers` from now on.
    pub(super) fn provide(&self, provider: &'static str, answers: Provided) {
        self.0.provided.write_recover().insert(provider, answers);
    }

    /// Whether a provider is registered under `provider`.
    #[must_use]
    pub fn serves(&self, provider: &str) -> bool {
        self.0.provided.read_recover().contains_key(provider)
    }

    fn provided(&self, provider: &str) -> Option<(&'static str, Provided)> {
        self.0
            .provided
            .read_recover()
            .get_key_value(provider)
            .map(|(name, answers)| (*name, Arc::clone(answers)))
    }

    fn taker(&self, id: &InvocationId) -> Option<Provided> {
        let name = *self.0.taken.lock_recover().get(id)?;
        self.provided(name).map(|(_, answers)| answers)
    }

    /// What the provider `query` names offers `caller`; `None` when no provider is registered
    /// under it.
    #[must_use]
    pub fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Option<Pending<Result<CataloguePage, ActionCallError>>> {
        let (_, answers) = self.provided(query.provider.as_deref()?)?;
        Some(answers.query(caller, query))
    }

    /// `page`, a catalogue that names no provider, with what every registered provider offers
    /// `caller` added.
    pub(crate) fn listed(
        &self,
        caller: &Caller,
        query: &CatalogueQuery,
        mut page: CataloguePage,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        // A later page of the same listing carries on from the first, which already has them.
        let asked: Vec<_> = if query.provider.is_some() || !query.cursor.is_empty() {
            Vec::new()
        } else {
            let provided = self.0.provided.read_recover();
            provided
                .values()
                .map(|answers| answers.query(caller, query.clone()))
                .collect()
        };
        if asked.is_empty() {
            return Pending::ready(Ok(page));
        }
        let (completer, answer) = tilo_actions::remote::pending();
        self.0.runtime.spawn(async move {
            for listed in asked {
                // A provider that cannot list now is left out of this page, as an app that
                // closed is: what the rest offer is still listed.
                if let Some(Ok(listed)) = listed.await {
                    page.rows.extend(listed.rows);
                    page.providers.extend(listed.providers);
                }
            }
            completer.complete(Ok(page));
        });
        answer
    }

    /// Hand `request` to the provider it names once the workspace `caller` asks in admits its
    /// device to the action: the ordinary grant check, decided here once for a call from any
    /// device, this one included, so no provider decides it again. `None` when no provider is
    /// registered under the name the request targets.
    #[must_use]
    pub fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Option<Pending<Result<InvocationReceipt, ActionCallError>>> {
        let (name, answers) = self.provided(&request.target.provider)?;
        let (Some(device), Some(workspace)) = (
            caller.device_public_key().copied(),
            caller.workspace().map(str::to_owned),
        ) else {
            return Some(Pending::ready(Err(ActionCallError::NotPermitted)));
        };
        let providers = self.clone();
        let caller = caller.clone();
        let (completer, answer) = tilo_actions::remote::pending();
        self.0.runtime.spawn(async move {
            let address = &request.target.address;
            if let Err(refused) = providers
                .permits(device, &workspace, name, &address.resource, &address.action)
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
                taken.insert(request.invocation, name);
            }
            let receipt = answers.invoke(&caller, request).await;
            completer.complete(receipt.unwrap_or(Err(ActionCallError::Unanswered)));
        });
        Some(answer)
    }

    /// Whether `workspace`'s grants let `device` run `action` on `resource` of `provider` now;
    /// the refusal its caller reads when they do not.
    async fn permits(
        &self,
        device: [u8; 32],
        workspace: &str,
        provider: &str,
        resource: &str,
        action: &str,
    ) -> Result<(), ActionCallError> {
        let kept = self.0.workspaces.lock().await.get(workspace).cloned();
        let Some(kept) = kept else {
            return Err(ActionCallError::Refused {
                reason: "this device does not keep that workspace".into(),
            });
        };
        let permitted = kept.lock().await.peer().permits_app_action(
            device,
            provider,
            resource,
            action,
            tilo_clock::wall(),
        );
        match permitted {
            Ok(true) => Ok(()),
            Ok(false) => Err(ActionCallError::Refused {
                reason: format!(
                    "your grants in this workspace do not let this device run {resource}#{action} \
                     of {provider}"
                ),
            }),
            Err(error) => Err(ActionCallError::Refused {
                reason: format!("this device is not admitted to the workspace: {error}"),
            }),
        }
    }

    /// Where invocation `id`, taken by a registered provider, stands for `caller`; `None` when no
    /// registered provider took it.
    #[must_use]
    pub fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<Option<InvocationState>> {
        Some(self.taker(id)?.observe(caller, id))
    }

    /// When invocation `id`, taken by a registered provider, may have changed; `None` when no
    /// registered provider took it.
    #[must_use]
    pub fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Option<Pending<()>>> {
        Some(self.taker(id)?.next_change(caller, id))
    }

    /// Ask the registered provider that took invocation `id` to stop it; `None` when none did.
    #[must_use]
    pub fn cancel(
        &self,
        caller: &Caller,
        id: &InvocationId,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        Some(self.taker(id)?.cancel(caller, id))
    }
}
