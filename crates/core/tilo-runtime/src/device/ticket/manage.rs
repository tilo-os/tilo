//! The person's devices, as every device of theirs manages them through its daemon's `devices`
//! actions: [`RENAME`] and [`REVOKE`] are the ticket server's, which every device of
//! theirs answers alike, and [`UPDATE`] is carried out by the device it names.
//!
//! A call naming another device goes to that device as an ordinary action over the action service
//! between devices (`tilo-actions/1`), signed with this device's key. That device's copy of the
//! workspace admits it by the person's grants there, as it admits every provided action, and it
//! answers it itself ([`OnThisDevice`]).

use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use iroh::{EndpointAddr, EndpointId};
use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, RemoteActionTarget,
};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_device_identity::login::{APPROVE, PENDING, PROVIDER, RENAME, RESOURCE, REVOKE, UPDATE};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::actions::{ActionService, NoActions};

use std::sync::Arc;

use collab_types::signer::DeviceSigner;
use ed25519_dalek::SigningKey;
use futures_util::future::BoxFuture;
use iroh::Endpoint;
use tilo_desktop_model::WorkspaceId;
use tilo_device_identity::login::{Entry, Reached};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::device_tickets::{Issuer, Role, Root};
use tilo_workspace_service::workspaces::Workspaces;

use super::super::workspaces::ServiceWorkspaces;
use super::{RootSource, Tickets, catalogue_of, failed, plan_entry, server_entry, succeeded};

/// How long another device is given to answer what it was asked.
const ANSWER_WAIT: Duration = Duration::from_secs(60);

/// What starts this device's update; `Err` says why it could not.
pub(crate) type UpdateStart = std::sync::Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

impl Tickets {
    /// Answer `action`, one of [`RENAME`], [`REVOKE`] and [`UPDATE`], asked with `args`.
    pub(super) async fn manage(&self, action: &str, args: &Value) -> Result<Value> {
        match action {
            RENAME => self.0.fleet.rename(args).await.map_err(anyhow::Error::msg),
            REVOKE => self.0.fleet.revoke(args).await.map_err(anyhow::Error::msg),
            UPDATE => self.update(args).await,
            _ => bail!("{action} is no action of the person's devices"),
        }
    }

    /// Hand `then`, for a call only the person's root signs, whether their devices record marks
    /// this device as one that keeps that root, and the record `workspace` keeps of the call
    /// `invocation` already, where it was handed on before: once both are read.
    pub(crate) fn root_only_call(
        &self,
        workspace: String,
        invocation: tilo_actions::InvocationId,
        then: impl FnOnce(bool, Option<tilo_actions::remote::asked::KeptAsk>) + Send + 'static,
    ) {
        let (workspaces, cert) = (self.0.workspaces.clone(), self.0.cert.clone());
        self.0.runtime.spawn(async move {
            let Some(workspaces) = workspaces else {
                return then(false, None);
            };
            let copies = workspaces.copies();
            let marked = tilo_workspace_service::workspaces::person_workspace::devices::keeps_root(
                copies, &cert,
            )
            .await;
            then(marked, copies.asked_record(&workspace, invocation).await);
        });
    }

    /// Certify the agent's grant `args` names with the root kept here.
    pub(super) fn certify_agent(&self, args: &Value) -> Result<Value> {
        let grant = args
            .get("grant")
            .and_then(Value::as_str)
            .and_then(|grant| hex::decode(grant).ok())
            .and_then(|grant| <[u8; 32]>::try_from(grant).ok())
            .ok_or_else(|| anyhow!("an agent's grant is named by its key, as hex"))?;
        Ok(super::super::services::certify_agent(
            self.0.place.keys(),
            grant,
        )?)
    }

    /// This device's key.
    fn this(&self) -> [u8; 32] {
        self.0.key.verifying_key().to_bytes()
    }

    /// The workspace this daemon serves, where it asks the person's other devices.
    fn workspace(&self) -> Option<String> {
        self.here().map(|here| here.workspace).or_else(|| {
            self.0
                .first
                .as_ref()
                .and_then(|first| first.get().map(str::to_owned))
        })
    }

    /// Update the device `args` names: this one here, another by asking it.
    async fn update(&self, args: &Value) -> Result<Value> {
        match device_in(args)? {
            Some(device) if device != self.this() => self.ask_device(device, UPDATE, args).await,
            _ => self.update_here(),
        }
    }

    /// Start this device's update, and say it runs.
    fn update_here(&self) -> Result<Value> {
        match self.0.update.get() {
            Some(start) => start(),
            None => start_update(),
        }
        .map_err(anyhow::Error::msg)?;
        Ok(json!({ "device": hex::encode(self.this()), "updating": true }))
    }

    /// Ask the person's device `device` `action` with `args`, in this daemon's workspace, as this
    /// device: its answer, or why it gave none.
    async fn ask_device(&self, device: [u8; 32], action: &str, args: &Value) -> Result<Value> {
        let workspace = self
            .workspace()
            .context("this device is in no workspace to reach its other devices from")?;
        let id = EndpointId::from_bytes(&device)
            .map_err(|_| anyhow!("{} is not a device key", hex::encode(device)))?;
        let addr = self
            .0
            .reach
            .lock_recover()
            .get(&device)
            .cloned()
            .unwrap_or_else(|| EndpointAddr::new(id));
        let Value::Object(argument) = args.clone() else {
            bail!("a device action asks with its arguments");
        };
        let service = ActionService::new(self.0.key.clone(), std::sync::Arc::new(NoActions));
        let remote = std::sync::Arc::new(service.remote(&self.0.endpoint, &addr, &workspace));
        let request = InvokeRequest {
            on_behalf_of: None,
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(RESOURCE, action),
            },
            argument: InvokeArgument::Json(argument),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        match remote.invoke_until_settled(request, ANSWER_WAIT).await {
            Some(Ok(InvocationUpdate::Succeeded { output, .. })) => {
                Ok(output.value.unwrap_or(Value::Null))
            }
            Some(Ok(InvocationUpdate::Failed { error, .. })) => bail!("{}", error.message),
            Some(Err(ActionCallError::Refused { reason })) => bail!("{reason}"),
            Some(Err(error)) => bail!("that device refused: {error:?}"),
            Some(Ok(_)) | None => {
                bail!("that device did not answer: it may be off or away from its network")
            }
        }
    }

    /// Start this device's updates with `start` rather than `tilo update`.
    #[cfg(test)]
    pub(crate) fn update_with(&self, start: UpdateStart) {
        let _ = self.0.update.set(start);
    }

    /// Reach the person's device `device` at `addr`, where no lookup finds it.
    #[cfg(test)]
    pub(crate) fn reach_at(&self, device: [u8; 32], addr: EndpointAddr) {
        self.0.reach.lock_recover().insert(device, addr);
    }
}

/// Start `tilo update` on this device, followed to its end in the log.
fn start_update() -> Result<(), String> {
    let mut child = std::process::Command::new("tilo")
        .args(["update", "--yes"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| format!("`tilo update` could not start on this device: {error}"))?;
    std::thread::spawn(move || match child.wait() {
        Ok(status) if status.success() => tracing::info!("this device is updated"),
        Ok(status) => {
            tracing::warn!(%status, "this device's update failed: `tilo update` on it says why");
        }
        Err(error) => tracing::warn!(%error, "this device's update was not followed"),
    });
    Ok(())
}

/// The device `args` names by its key, when it names one.
fn device_in(args: &Value) -> Result<Option<[u8; 32]>> {
    let Some(text) = args
        .get("device")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    else {
        return Ok(None);
    };
    let mut key = [0; 32];
    hex::decode_to_slice(text, &mut key)
        .map_err(|_| anyhow!("{text} is not a device's key: `tilo device ls` lists them"))?;
    Ok(Some(key))
}

/// What this device answers the person's other devices on its action service: the device
/// actions that are its own to carry out. Their caller was admitted by the workspace's grants
/// before a request reaches it.
pub(crate) struct OnThisDevice(pub(crate) Tickets);

impl OnThisDevice {
    fn keep(&self, invocation: InvocationId, update: InvocationUpdate) -> InvocationReceipt {
        let state = InvocationState::new(invocation, update);
        let receipt = state.receipt();
        self.0.0.calls.lock_recover().insert(invocation, state);
        receipt
    }
}

impl ActionProvider for OnThisDevice {
    fn serves(&self, provider: &str) -> bool {
        provider == PROVIDER
    }

    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(catalogue_of(&query, |action| {
            matches!(action, UPDATE | PENDING | APPROVE)
        })))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        if let Some(state) = self.0.0.calls.lock_recover().get(&invocation) {
            return Pending::ready(Ok(state.receipt()));
        }
        let address = &request.target.address;
        let args = match &request.argument {
            InvokeArgument::Json(argument) => Value::Object(argument.clone()),
            _ => Value::Object(Map::new()),
        };
        // An agent's key this daemon serves a sign-in ticket for, shown to and approved by a
        // device that holds the person's root.
        if address.resource == RESOURCE
            && matches!(address.action.as_str(), PENDING | APPROVE)
            && self.0.asks_for_agent_key(&args)
        {
            let update = match self.0.agent_key(&address.action, &args) {
                Ok(value) => succeeded(invocation, value),
                Err(error) => failed(invocation, &format!("{error:#}")),
            };
            return Pending::ready(Ok(self.keep(invocation, update)));
        }
        if address.resource != RESOURCE || address.action != UPDATE {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        let update = match device_in(&args) {
            Ok(Some(device)) if device != self.0.this() => {
                return Pending::ready(Err(ActionCallError::refused(
                    "this device carries out only what is asked of itself",
                )));
            }
            Ok(_) => match self.0.update_here() {
                Ok(value) => succeeded(invocation, value),
                Err(error) => failed(invocation, &format!("{error:#}")),
            },
            Err(error) => {
                return Pending::ready(Err(ActionCallError::refused(format!("{error:#}"))));
            }
        };
        Pending::ready(Ok(self.keep(invocation, update)))
    }

    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.0.0.calls.lock_recover().get(id).cloned()
    }

    fn cancel(&self, _caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(match self.0.0.calls.lock_recover().get(id) {
            Some(_) => ActionCallError::NotCancellable {
                reason: "an update, once started, runs to its end".into(),
            },
            None => ActionCallError::UnknownInvocation,
        }))
    }
}

/// This device, as the person's devices are added, named and revoked from it: its key, the
/// person's root when it holds it, the server it enters through (once it knows one), and the
/// workspace service a certified computer's entry is written through.
pub(super) struct Fleet {
    pub key: SigningKey,
    pub cert: DeviceCert,
    pub endpoint: Endpoint,
    /// Where this device enters its person's workspaces, on a device a ticket made.
    pub home: Option<Arc<super::super::home::Home>>,
    pub root: RootSource,
    /// The person's workspace here: on a computer no ticket made, the one a computer it adds
    /// enters. `None` where this computer holds none another computer can enter.
    pub first: Option<Arc<super::super::first_workspace::FirstWorkspace>>,
    pub workspaces: Option<Arc<ServiceWorkspaces>>,
    pub service: ActionService,
}

impl Fleet {
    /// Where a computer this device adds enters: the server this device enters through, or, on
    /// one no ticket made, the person's workspace here through this daemon's endpoint, founded
    /// now when they have none yet.
    async fn entered(&self) -> Result<Entry, String> {
        if let Some(here) = self.home.as_ref().and_then(|home| home.get()) {
            return tilo_api::daemon::enter::login_entry(&here);
        }
        let first = self.first.as_ref().ok_or_else(|| {
            "This computer holds no workspace another computer can enter.".to_owned()
        })?;
        let workspace = first.found().await?;
        let name = self
            .workspaces
            .as_ref()
            .zip(WorkspaceId::parse(&workspace))
            .and_then(|(workspaces, id)| workspaces.workspace(id))
            .map(|described| described.name)
            .unwrap_or_default();
        // The session a computer enters over proves the certificate the workspace service runs
        // under, which is the device's own unless the service has one of its own.
        let server_cert = self.workspaces.as_ref().map_or_else(
            || self.cert.clone(),
            |service| service.copies().serving_certificate().clone(),
        );
        Ok(tilo_api::daemon::enter::founded_entry(
            &self.endpoint,
            (workspace, name),
            server_cert,
        ))
    }
}

impl Issuer for Fleet {
    fn serving(&self) -> Result<(Reached, ActionService), String> {
        Ok((
            tilo_api::daemon::enter::reached_of(&self.endpoint.addr()),
            self.service.clone(),
        ))
    }

    fn device(&self) -> Result<(DeviceCert, Arc<dyn DeviceSigner>), String> {
        Ok((self.cert.clone(), Arc::new(self.key.clone())))
    }

    fn root(&self) -> Result<Option<Root>, String> {
        (self.root)()
            .map(|root| root.map(|root| Arc::new(root) as Root))
            .map_err(|error| format!("{error:#}"))
    }

    fn entry(&self) -> BoxFuture<'_, Result<Entry, String>> {
        Box::pin(self.entered())
    }

    /// A member device asks its host to write the entry, through its session; one that holds the
    /// person's Servers workspace writes it itself, founding it when it founded the person's own
    /// workspaces.
    fn admit<'a>(
        &'a self,
        role: &'a Role,
        cert: &'a DeviceCert,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let service = self.workspaces.as_ref().ok_or_else(|| {
                "this computer holds no workspace service to take a computer in".to_owned()
            })?;
            let session = service.session();
            match role {
                Role::Server => {
                    server_entry(
                        service.copies(),
                        (session, cert),
                        None,
                        self.first.as_ref().is_some_and(|first| first.founds()),
                    )
                    .await
                }
                Role::Plan(plan) => plan_entry(service.copies(), session, plan, cert).await,
            }
        })
    }

    fn servers(&self) -> Option<Workspaces> {
        self.workspaces
            .as_ref()
            .map(|service| service.copies().clone())
    }

    /// The fleet store keeps what this computer's root revoked: an agent's key it made here
    /// signs nothing from then on, its next call is refused and its connection ends.
    fn keep_revocation(&self, entry: &tilo_user_identity::RevocationEntry) {
        let kept = tilo_identity::device_fleet::DeviceFleetStore::from_env()
            .map_err(|error| error.to_string())
            .and_then(|fleets| {
                fleets
                    .record_revocation(&hex::encode(entry.user_public_key), entry)
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = kept {
            tracing::warn!(%error, "this computer does not keep the revocation it signed");
        }
    }
}
