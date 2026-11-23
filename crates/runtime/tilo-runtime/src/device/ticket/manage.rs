//! The person's devices, as every device of theirs manages them through its daemon's `devices`
//! actions: [`LIST`], [`RENAME`] and [`REVOKE`] are the ticket server's, which every device of
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
use tilo_device_identity::login::{LIST, PROVIDER, RENAME, RESOURCE, REVOKE, UPDATE};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::actions::{ActionService, NoActions};

use std::sync::Arc;

use collab_types::signer::DeviceSigner;
use ed25519_dalek::SigningKey;
use futures_util::future::BoxFuture;
use iroh::Endpoint;
use tilo_device_identity::login::{Entry, Reached};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::device_tickets::{Issuer, Root};
use tilo_workspace_service::workspaces::Workspaces;

use super::super::workspaces::ServiceWorkspaces;
use super::{RootSource, Tickets, catalogue_of, failed, succeeded};

/// How long another device is given to answer what it was asked.
const ANSWER_WAIT: Duration = Duration::from_secs(60);

/// What starts this device's update; `Err` says why it could not.
pub(crate) type UpdateStart = std::sync::Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

impl Tickets {
    /// Answer `action`, one of [`LIST`], [`RENAME`], [`REVOKE`] and [`UPDATE`], asked with `args`.
    pub(super) async fn manage(&self, action: &str, args: &Value) -> Result<Value> {
        match action {
            LIST => self.0.fleet.list().await.map_err(anyhow::Error::msg),
            RENAME => self
                .0
                .fleet
                .rename(args)
                .await
                .map_err(anyhow::Error::msg),
            REVOKE => self
                .0
                .fleet
                .revoke(args)
                .await
                .map_err(anyhow::Error::msg),
            UPDATE => self.update(args).await,
            _ => bail!("{action} is no action of the person's devices"),
        }
    }

    /// This device's key.
    fn this(&self) -> [u8; 32] {
        self.0.key.verifying_key().to_bytes()
    }

    /// The workspace this daemon serves, where it asks the person's other devices.
    fn workspace(&self) -> Option<String> {
        self.0
            .login
            .as_ref()
            .map(|(_, device)| device.workspace.clone())
            .or_else(|| {
                self.0
                    .founded
                    .as_ref()
                    .map(|founded| founded.workspace.clone())
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
        .map_err(|_| anyhow!("{text} is not a device's key: `devices#list` lists them"))?;
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
        Pending::ready(Ok(catalogue_of(&query, |action| action == UPDATE)))
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
        if address.resource != RESOURCE || address.action != UPDATE {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        let args = match request.argument {
            InvokeArgument::Json(argument) => Value::Object(argument),
            _ => Value::Object(Map::new()),
        };
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

/// This device, as the person's devices are listed, named and revoked from it: its key, the
/// person's root when it holds it, and the host it entered its workspace on.
pub(super) struct Fleet {
    pub key: SigningKey,
    pub cert: DeviceCert,
    pub endpoint: Endpoint,
    pub root: RootSource,
    pub workspaces: Option<Arc<ServiceWorkspaces>>,
    pub service: ActionService,
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

    /// A computer is added by `devices#issue`, never through this.
    fn entry(&self) -> BoxFuture<'_, Result<Entry, String>> {
        Box::pin(std::future::ready(Err(
            "a computer is added with `devices#issue`".to_owned()
        )))
    }

    fn servers(&self) -> Option<Workspaces> {
        self.workspaces
            .as_ref()
            .map(|service| service.copies().clone())
    }

    /// A revocation this daemon's root signed goes to the host it entered its workspace on, once
    /// it has: the host refuses the key from then on, and marks it in the person's record.
    fn keep_revocation(&self, entry: &tilo_user_identity::RevocationEntry) {
        use tilo_session_client::Outbox as _;
        let Some(session) = self
            .workspaces
            .as_ref()
            .and_then(|workspaces| workspaces.session())
        else {
            return;
        };
        if session
            .session()
            .send(tilo_session_client::Cmd::Revoke(entry.clone()))
            .is_err()
        {
            tracing::info!(
                host = session.label(),
                "a host did not hear that the person took a device back"
            );
        }
    }
}
