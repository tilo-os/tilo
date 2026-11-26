//! Making a workspace of the person's from a device of theirs that founds none itself.
//!
//! The person's workspaces are founded on the device that keeps their root: the computer they
//! first set Tilo up on, which founded their own workspace. Another device of theirs asks that one
//! to make a workspace (`account/workspaces#create`) over the action service both already serve
//! (`tilo-actions/1`), signed as itself, and hands the person its answer as it came ([`ask`]).
//! Nothing falls back to their server: a server founds none of their workspaces for a device.
//!
//! The founding device answers only the person's own other devices ([`FoundingCreate`]): the
//! certificate the asking device presents names the same person, and their devices record lists
//! it, not revoked. The workspace the call is asked in admits it by the person's grants there for
//! that device, as it admits every action another device calls here: a computer on an operating
//! system Tilo does not control makes nothing that reaches out without the person's yes. It
//! makes the workspace as it makes one a key of theirs asks for, as the asking device's call
//! ([`super::services::create_here`]).

use std::sync::Arc;
use std::time::Duration;

use iroh::{EndpointAddr, EndpointId, TransportAddr};
use serde_json::{Value, json};
use tilo_actions::api::{ActionAddress, PlacedAction};
use tilo_actions::remote::admitted::offered;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus, RemoteActionTarget,
};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};
use tilo_api::session::{Creating, NotFounded};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::workspaces::person_workspace::devices;
use tilo_workspace_service::workspaces::providers::{Providers, Reach};

use super::workspaces::ServiceWorkspaces;

/// The provider the founding device answers another device of its person's under.
pub const PROVIDER: &str = "so.tilo.founding";
/// The application whose grants decide the calls: the person's workspaces are their settings'.
pub const APP: &str = tilo_app_id::apps::SETTINGS;
/// The resource the person's workspaces are made under, as their account names it.
pub const RESOURCE: &str = "account/workspaces";
/// Make a workspace.
pub const CREATE: &str = "create";

/// How long the device asking waits for the founding device: the bound the daemon waits on any
/// part of the person's that does not answer.
pub(crate) const WAITED: Duration = crate::daemon::WAITED_WITHIN;

/// The workspaces made for another device of the person's, as its actions publish them.
#[derive(tilo_resource::Row)]
#[row(kind = "founded-workspaces", label = "Workspaces founded here")]
struct FoundedRow;

/// What another device of the person's asks the founding device.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.founding")]
pub enum FoundingAction {
    /// Make a workspace another of your devices asked for.
    #[effect(kind = "outside")]
    Create,
}

/// The resource and its action, as a call declaring it reads them.
fn resources() -> Vec<tilo_actions::ResourceSpec> {
    let mut spec = tilo_resource::offer::<FoundingAction>(tilo_resource::typed_resource::<
        FoundedRow,
    >(RESOURCE, "Your workspaces"));
    spec.app_id = Some(APP.to_owned());
    vec![spec]
}

/// What the person's device that founds their workspaces answers their other devices.
pub(crate) struct FoundingCreate {
    workspaces: Arc<ServiceWorkspaces>,
    resources: Vec<tilo_actions::ResourceSpec>,
}

impl FoundingCreate {
    /// The founding of `workspaces`' person's workspaces, for their other devices.
    pub(crate) fn new(workspaces: &Arc<ServiceWorkspaces>) -> Arc<Self> {
        Arc::new(Self {
            workspaces: Arc::clone(workspaces),
            resources: resources(),
        })
    }

    /// Answer the person's other devices on this device's action service from now on.
    pub(crate) fn register(self: &Arc<Self>, providers: &Providers) {
        providers.provide(
            PROVIDER,
            APP,
            Reach::Devices,
            Arc::clone(self) as Arc<dyn ActionProvider + Send + Sync>,
        );
    }
}

/// The certificate of the device `caller` asks from, when it is another device of the person
/// `workspaces` serves, listed in their devices record and not revoked: no one else, whatever
/// part they have in a workspace, has their workspaces made here.
async fn asking_device(workspaces: &ServiceWorkspaces, caller: &Caller) -> Option<DeviceCert> {
    let own = workspaces.device_cert();
    let person = own.user_public_key;
    if caller.user_public_key() != Some(&person)
        || caller.acting().app().is_some()
        || caller.launched_app().is_some()
    {
        return None;
    }
    let device = *caller.device_public_key()?;
    if device == own.device_public_key {
        return None;
    }
    let now = tilo_clock::unix_now_ns();
    devices::of(workspaces.copies(), &person)
        .await
        .listed(&person)
        .into_iter()
        .find(|listed| {
            listed.cert.device_public_key == device
                && !listed.device.revoked(&listed.cert)
                && tilo_user_identity::verify_device_cert_at(&listed.cert, now).is_ok()
        })
        .map(|listed| listed.cert)
}

/// Make the workspace `request` asks for on the device `workspaces` serves, as the device `caller`
/// asks it.
async fn create(
    workspaces: &ServiceWorkspaces,
    caller: &Caller,
    request: &InvokeRequest,
) -> Result<Value, ActionCallError> {
    let address = &request.target.address;
    if address.resource != RESOURCE || address.action != CREATE {
        return Err(ActionCallError::UnknownAction);
    }
    let Some(asking) = asking_device(workspaces, caller).await else {
        return Err(ActionCallError::NotPermitted);
    };
    let InvokeArgument::Json(fields) = &request.argument else {
        return Err(ActionCallError::invalid_request(
            "a create names the workspace to make",
        ));
    };
    let creating: Creating = serde_json::from_value(Value::Object(fields.clone()))
        .map_err(|error| ActionCallError::invalid_request(error.to_string()))?;
    if creating.name.trim().is_empty() {
        return Err(ActionCallError::invalid_request(
            "a workspace is made with a name",
        ));
    }
    let device = workspaces.device_cert();
    let made = super::services::create_here(
        workspaces,
        &asking,
        &device,
        (creating.name, creating.labels, creating.id),
    )
    .await
    .map_err(|error| ActionCallError::refused(error.to_string()))?;
    Ok(json!({ "workspace": made }))
}

impl ActionProvider for FoundingCreate {
    fn published(&self) -> Vec<tilo_actions::ResourceSpec> {
        self.resources.clone()
    }

    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        // Only the person's own devices ask it, each as itself: no catalogue lists it.
        let rows: Vec<PlacedAction> = Vec::new();
        offered(PROVIDER, APP, rows, &query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let (workspaces, caller) = (Arc::clone(&self.workspaces), caller.clone());
        let (done, answer) = tilo_actions::remote::pending();
        tilo_rt::app().spawn(async move {
            let answered =
                create(&workspaces, &caller, &request)
                    .await
                    .map(|value| InvocationReceipt {
                        invocation: request.invocation,
                        status: ReceiptStatus::Completed {
                            output: ActionOutput {
                                value: Some(value),
                                resources: Vec::new(),
                            },
                        },
                    });
            done.complete(answered);
        });
        answer
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }
}

/// Make the workspace `creating` names on the person's device that founds their workspaces, asked
/// in `workspace`, which both keep, by the device `workspaces` serves, as itself: that device's
/// answer as it gave it. Waits [`WAITED`] for it at most, and never asks anyone else.
///
/// # Errors
/// None of the person's devices founds their workspaces, the one that does did not answer in
/// time, or it refused.
pub(crate) async fn ask(
    workspaces: &ServiceWorkspaces,
    workspace: &str,
    creating: Creating,
) -> Result<Value, NotFounded> {
    let own = workspaces.device_cert();
    let person = own.user_public_key;
    let record = devices::of(workspaces.copies(), &person).await;
    // The device that keeps the person's root founded their own workspace, and founds the rest.
    let Some(founding) = record
        .root_holders(&person)
        .into_iter()
        .find(|device| *device != own.device_public_key)
    else {
        return Err(NotFounded::NoFoundingDevice);
    };
    let addrs = record
        .listed(&person)
        .into_iter()
        .find(|listed| listed.cert.device_public_key == founding)
        .map(|listed| listed.device.endpoint_addrs())
        .unwrap_or_default();
    let mut arguments = serde_json::Map::new();
    arguments.insert("name".into(), json!(creating.name));
    if !creating.labels.is_empty() {
        arguments.insert("labels".into(), json!(creating.labels));
    }
    if let Some(id) = creating.id {
        arguments.insert("id".into(), json!(id));
    }
    let unreached = NotFounded::Unreached { waited: WAITED };
    // The founding device is dialled where the person's record says it answers, or by its key
    // alone when no address is kept.
    let id = EndpointId::from_bytes(&founding).map_err(|_| NotFounded::NoFoundingDevice)?;
    let addr = EndpointAddr::from_parts(id, addrs.into_iter().map(TransportAddr::Ip));
    let remote = Arc::new(workspaces.copies().action_service().remote(
        &workspaces.endpoint(),
        &addr,
        workspace,
    ));
    let request = InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: ActionAddress::new(RESOURCE, CREATE),
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
    let answered = tokio::time::timeout(WAITED, remote.invoke_until_settled(request, WAITED))
        .await
        .map_err(|_| unreached.clone())?;
    match answered {
        Some(Ok(InvocationUpdate::Succeeded { output, .. })) => {
            Ok(output.value.unwrap_or(Value::Null))
        }
        Some(Ok(InvocationUpdate::Failed { error, .. })) => Err(NotFounded::Refused(error.message)),
        Some(Err(
            ActionCallError::Refused { reason } | ActionCallError::InvalidRequest { reason },
        )) => Err(NotFounded::Refused(reason)),
        Some(Err(ActionCallError::NotPermitted | ActionCallError::UnknownAction)) => Err(
            NotFounded::Refused("it does not make workspaces for this device".to_owned()),
        ),
        // Not reached, not answered, or stopped before it answered: nothing says it made one.
        Some(Ok(_) | Err(_)) | None => Err(unreached),
    }
}

#[cfg(test)]
#[path = "founding_create_tests.rs"]
mod tests;
