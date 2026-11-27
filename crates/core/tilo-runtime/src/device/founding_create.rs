//! Making a workspace of the person's from a device of theirs that founds none itself.
//!
//! The person's workspaces are founded on the device that keeps their root: the computer they
//! first set Tilo up on, which founded their own workspace. Another device of theirs asks that one
//! to make a workspace (`account/workspaces#create`) over the action service both already serve
//! (`tilo-actions/1`), signed as itself, in the person's own workspace, and hands the person its
//! answer as it came ([`ask`]). Nothing falls back to their server: a server founds none of their
//! workspaces for a device. The asking device then holds the new workspace as a device of theirs
//! holds any workspace of their list: the founding device lists it there, with where it is
//! reached, and the asking device enters it at that address as a member once its copy of the list
//! says so, so it opens there as soon as the answer comes.
//!
//! The founding device answers only the person's own other devices ([`FoundingCreate`]), asking
//! as the person: the certificate the connection verified names the same person, and their
//! devices record lists it, not revoked. An agent, an app or anyone acting for the person is
//! refused, whatever device it asks from. The person's grants in their own workspace for that
//! device decide the rest, as they decide every action another device calls here: a computer on
//! an operating system Tilo does not control makes nothing that reaches out without the person's
//! yes, asked on this device as every other own-device question is, and declined as one. It
//! makes the workspace as it makes one a key of theirs asks for, as the asking device's call
//! ([`super::services::create_here`]).

use std::sync::Arc;
use std::time::Duration;

use iroh::{EndpointAddr, EndpointId, TransportAddr};
use serde_json::{Value, json};
use tilo_action_policy::{ActionKind, Answer, Context, TreePath};
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::asked::AskedFacts;
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, CatalogueProvider, CatalogueQuery, CatalogueRow,
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionMessage, RemoteActionTarget,
    UpdateMark,
};
use tilo_actions::{ActionParam, InvocationId, InvocationUpdate};
use tilo_api::session::{Creating, NotFounded};
use tilo_peer_state::join::TransportHint;
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_server_protocol::workspaces::IDENTITY_LIST;
use tilo_user_identity::DeviceCert;
use tilo_user_identity::WorkspaceList;
use tilo_workspace_service::workspaces::asking::approved_or_asks;
use tilo_workspace_service::workspaces::person_workspace::{self, devices};
use tilo_workspace_service::workspaces::providers::{
    Answered, Answering, Providers, WorkspaceAnswers, WorkspaceCall,
};

use super::workspaces::ServiceWorkspaces;

/// The provider the founding device answers another device of its person's under.
pub const PROVIDER: &str = "so.tilo.founding";
/// The application whose grants decide the calls: the person's workspaces are their settings'.
pub const APP: &str = tilo_app_id::apps::SETTINGS;
/// The resource the person's workspaces are made under, as their account names it.
pub const RESOURCE: &str = "account/workspaces";
/// Make a workspace.
pub const CREATE: &str = "create";

/// How long the device asking waits for the founding device to answer at all, and for the
/// workspace it made to arrive here: the bound the daemon waits on any part of the person's that
/// does not answer.
pub(crate) const WAITED: Duration = crate::daemon::WAITED_WITHIN;

/// How long the device asking waits once the founding device asks the person: long enough to
/// walk over to it and answer.
pub(crate) const ANSWERED_WITHIN: Duration = Duration::from_secs(5 * 60);

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

    /// Answer the person's other devices in the workspaces this device holds from now on: a call
    /// reaches it through the workspace's governance, which verified the asking device's
    /// certificate and keeps a call that waits for the person's yes as their question.
    pub(crate) fn register(self: &Arc<Self>, providers: &Providers) {
        providers.answer_in_workspaces(
            PROVIDER,
            APP,
            Arc::clone(self) as Arc<dyn WorkspaceAnswers>,
        );
    }
}

/// The device of `workspaces`' person that `call` asks from, with what their devices record
/// calls it: another of their devices, listed and not revoked, whose certificate the connection
/// verified, asking as the person themselves in their own workspace. No one else has their
/// workspaces made here, whatever part they have in a workspace: not another person, and not an
/// agent, an app or a program acting for the person, from whichever device.
async fn asking_device(
    workspaces: &ServiceWorkspaces,
    call: &WorkspaceCall<'_>,
) -> Option<(DeviceCert, Option<String>)> {
    let own = workspaces.device_cert();
    let person = own.user_public_key;
    let acting = call.caller.acting();
    // Making a workspace for the person is theirs to ask: an agent acting for them asks it
    // where they confirm what it does, never through a device of theirs.
    if acting.is_agent()
        || !acting.is_person()
        || acting.app().is_some()
        || acting.acted_for_person().is_some()
        || call.caller.launched_app().is_some()
    {
        return None;
    }
    let certificate = call.certificate?;
    if certificate.user_public_key != person
        || certificate.capabilities.agent.is_some()
        || call.caller.user_public_key() != Some(&person)
        || call.caller.device_public_key() != Some(&certificate.device_public_key)
        || certificate.device_public_key == own.device_public_key
        || call.workspace.map(|workspace| workspace.to_string())
            != Some(person_workspace_id(&person))
    {
        return None;
    }
    let now = tilo_clock::unix_now_ns();
    devices::of(workspaces.copies(), &person)
        .await
        .listed(&person)
        .into_iter()
        .find(|listed| {
            listed.cert.device_public_key == certificate.device_public_key
                && !listed.device.revoked(&listed.cert)
                && tilo_user_identity::verify_device_cert_at(&listed.cert, now).is_ok()
        })
        .map(|listed| (listed.cert, listed.device.name))
}

/// What the person's grants in `workspace` answer `device` making one of their workspaces here:
/// it reaches out beyond the device, so a device on an operating system Tilo does not control is
/// asked about. A device the workspace does not admit is refused.
fn granted(workspaces: &ServiceWorkspaces, workspace: &str, device: [u8; 32]) -> Answer {
    let now = tilo_clock::wall();
    workspaces
        .copies()
        .read_now(workspace, |peer| {
            peer.permissions(device, now).map(|access| {
                let node = TreePath::workspace(access.workspace())
                    .app(APP)
                    .rest()
                    .resource(RESOURCE)
                    .rest()
                    .action(CREATE);
                access.answer(&node, ActionKind::Outside, &Context::at_time(now))
            })
        })
        .and_then(|(_, answer)| answer.ok())
        .unwrap_or(Answer::Deny)
}

/// What the person reads on this device before saying yes to making `creating` for the device
/// their record calls `asking`.
fn question(creating: &Creating, asking: Option<&str>) -> AskedFacts {
    let device = asking.map_or_else(
        || crate::i18n::t("founding-create-another-device"),
        str::to_owned,
    );
    AskedFacts {
        words: crate::i18n::t_args(
            "founding-create-question",
            &[
                ("name", creating.name.as_str().into()),
                ("device", device.as_str().into()),
            ],
        ),
        on: Some(creating.name.clone()),
        effect: Some(ActionKind::Outside),
        ..AskedFacts::default()
    }
}

/// Make the workspace `request` asks for on the device `workspaces` serves, as the device `call`
/// asks it, once the person's grants say it may or the person said yes.
async fn create(
    workspaces: &ServiceWorkspaces,
    call: WorkspaceCall<'_>,
    request: &InvokeRequest,
) -> Result<Value, ActionCallError> {
    let address = &request.target.address;
    if address.resource != RESOURCE || address.action != CREATE {
        return Err(ActionCallError::UnknownAction);
    }
    let Some((asking, called)) = asking_device(workspaces, &call).await else {
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
    let own = person_workspace_id(&asking.user_public_key);
    match granted(workspaces, &own, asking.device_public_key) {
        Answer::Allow => {}
        Answer::Ask => approved_or_asks(request, || question(&creating, called.as_deref()))?,
        Answer::Deny => return Err(ActionCallError::NotPermitted),
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

impl WorkspaceAnswers for FoundingCreate {
    fn params(&self, target: &RemoteActionTarget) -> Option<Vec<ActionParam>> {
        if target.provider != PROVIDER {
            return None;
        }
        tilo_actions::api::locate(&self.resources, &target.address)
            .map(|(_, action)| action.params().to_vec())
    }

    fn offer<'a>(
        &'a self,
        _call: WorkspaceCall<'a>,
        _query: &'a CatalogueQuery,
    ) -> Answering<'a, Vec<(CatalogueProvider, Vec<CatalogueRow>)>> {
        // Only the person's own devices ask it, each as itself: no catalogue lists it.
        Box::pin(std::future::ready(Vec::new()))
    }

    fn answer<'a>(
        &'a self,
        call: WorkspaceCall<'a>,
        request: &'a InvokeRequest,
    ) -> Answering<'a, Result<Answered, ActionCallError>> {
        Box::pin(async move {
            create(&self.workspaces, call, request)
                .await
                .map(Answered::Value)
        })
    }
}

/// Make the workspace `creating` names on the person's device that founds their workspaces,
/// asked in their own workspace, which both keep, by the device `workspaces` serves, as itself:
/// that device's answer as it gave it, once the workspace it made is held here too. Waits
/// [`WAITED`] for it to answer, [`ANSWERED_WITHIN`] more where it asks the person first, and never
/// asks anyone else.
///
/// # Errors
/// None of the person's devices founds their workspaces, the one that does did not answer in
/// time, the person said no there or did not answer, or it refused.
pub(crate) async fn ask(
    workspaces: &ServiceWorkspaces,
    creating: Creating,
) -> Result<Value, NotFounded> {
    let own = workspaces.device_cert();
    let person = own.user_public_key;
    let workspace = person_workspace_id(&person);
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
    // The founding device is dialled where the person's record says it answers, or by its key
    // alone when no address is kept.
    let id = EndpointId::from_bytes(&founding).map_err(|_| NotFounded::NoFoundingDevice)?;
    let addr = EndpointAddr::from_parts(id, addrs.into_iter().map(TransportAddr::Ip));
    let remote = Arc::new(workspaces.copies().action_service().remote(
        &workspaces.endpoint(),
        &addr,
        &workspace,
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
        // Its first answer comes within a change's wait: what is made by then, else that it is
        // under way or waits for the person, and the call is followed from there.
        wait_ms: Some(tilo_peer_state::actions::CHANGE_WAIT_MS),
        appearance: None,
        signed: None,
    };
    // Asked as this device, never signed as the person: the founding device's grants for this
    // device decide, and the person answers there what they leave to them.
    let asking = Caller::authenticated(person, own.device_public_key).in_workspace(workspace);
    let update = settled(&remote, &asking, request).await?;
    let made = match update {
        Ok(InvocationUpdate::Succeeded { output, .. }) => output.value.unwrap_or(Value::Null),
        Ok(InvocationUpdate::Failed { error, .. }) => {
            return Err(NotFounded::Refused(error.message));
        }
        Ok(InvocationUpdate::Cancelled { message, .. })
            if message.as_deref() == Some(tilo_actions::remote::asked::DECLINED) =>
        {
            return Err(NotFounded::Declined);
        }
        Ok(InvocationUpdate::Cancelled { message, .. }) => {
            return Err(NotFounded::Refused(message.unwrap_or_default()));
        }
        Err(ActionCallError::Refused { reason } | ActionCallError::InvalidRequest { reason }) => {
            return Err(NotFounded::Refused(reason));
        }
        Err(ActionCallError::NotPermitted | ActionCallError::UnknownAction) => {
            return Err(NotFounded::Refused(
                "it does not make workspaces for this device".to_owned(),
            ));
        }
        // Stopped before it answered, or answered nothing that ends it: nothing says it made one.
        Ok(_) | Err(_) => return Err(NotFounded::Unreached { waited: WAITED }),
    };
    held_here(workspaces, &made).await;
    Ok(made)
}

/// How `request`, asked of the founding device `remote` as `asking`, ended there: its first
/// answer within [`WAITED`], and, where it waits for the person's yes there or is still being
/// made, how it ended within [`ANSWERED_WITHIN`] after.
///
/// # Errors
/// The founding device did not answer in time, or the call was still waiting when that ran out.
async fn settled(
    remote: &Arc<tilo_peer_state::actions::RemoteDevice>,
    asking: &Caller,
    request: InvokeRequest,
) -> Result<Result<InvocationUpdate, ActionCallError>, NotFounded> {
    let unreached = NotFounded::Unreached { waited: WAITED };
    let invocation = request.invocation;
    let receipt = tokio::time::timeout(WAITED, remote.invoke(asking, request))
        .await
        .map_err(|_| unreached.clone())?;
    let status = match receipt {
        Some(Ok(receipt)) => receipt.status,
        Some(Err(error)) => return Ok(Err(error)),
        None => return Err(unreached),
    };
    Ok(match status {
        ReceiptStatus::Completed { output } => {
            Ok(InvocationUpdate::Succeeded { invocation, output })
        }
        ReceiptStatus::Failed { error } => Ok(InvocationUpdate::Failed { invocation, error }),
        ReceiptStatus::Cancelled { message } => Ok(InvocationUpdate::Cancelled {
            invocation,
            message,
        }),
        ReceiptStatus::Rejected { error } => Err(error),
        // It waits for the person's yes there, or is still making it: it is followed until it
        // ends, for as long as a person takes to answer.
        ReceiptStatus::Accepted => {
            match tokio::time::timeout(ANSWERED_WITHIN, followed(remote, invocation)).await {
                Ok(Ok(update)) => Ok(update),
                Ok(Err(
                    error @ (ActionCallError::Refused { .. }
                    | ActionCallError::InvalidRequest { .. }
                    | ActionCallError::NotPermitted
                    | ActionCallError::UnknownAction),
                )) => Err(error),
                // It ran out while it waited there: the person's question went unanswered, or the
                // device went away before it ended.
                Ok(Err(_)) | Err(_) => {
                    return Err(NotFounded::Unanswered {
                        waited: ANSWERED_WITHIN,
                    });
                }
            }
        }
    })
}

/// How `invocation`, under way on the founding device `remote`, ended there: watched a change at
/// a time, for as long as its caller waits. Only watched, never asked again: the call the person
/// answers is theirs to admit, and asking it again would be another call.
///
/// # Errors
/// The device stopped answering, or forgot the call.
async fn followed(
    remote: &Arc<tilo_peer_state::actions::RemoteDevice>,
    invocation: InvocationId,
) -> Result<InvocationUpdate, ActionCallError> {
    let mut seen = Some(UpdateMark::Accepted);
    loop {
        let answer = remote
            .ask_message(RemoteActionMessage::Observe {
                correlation: 0,
                invocation,
                wait_ms: Some(tilo_peer_state::actions::CHANGE_WAIT_MS),
                seen,
            })
            .await
            .unwrap_or(Err(ActionCallError::Unanswered))?;
        match answer {
            RemoteActionMessage::Update {
                state: Some(state), ..
            } if state.is_terminal() => return Ok(state.update),
            RemoteActionMessage::Update {
                state: Some(state), ..
            } => seen = Some(UpdateMark::of(&state.update)),
            RemoteActionMessage::Update { state: None, .. } => {
                return Err(ActionCallError::UnknownInvocation);
            }
            _ => return Err(ActionCallError::Unanswered),
        }
    }
}

/// Hold the workspace the founding device `made` here, [`WAITED`] at most: the founding device
/// lists it in the person's list, in their own workspace, with where it is reached, and once that
/// reaches this device's copy it is entered there as a member, as a device of theirs enters any
/// workspace of their list it does not hold yet ([`tilo_api::session::Here::keep`]). One that is
/// not held in time is said, and stays in the list for the next time it is kept.
async fn held_here(workspaces: &ServiceWorkspaces, made: &Value) {
    let Some(id) = made
        .get("workspace")
        .and_then(|workspace| workspace.get("id"))
        .and_then(Value::as_str)
    else {
        return;
    };
    let Some(workspace) = WorkspaceId::parse(id) else {
        return;
    };
    let copies = workspaces.copies();
    if copies.state(id).await.is_some() {
        return;
    }
    let own = person_workspace_id(&workspaces.device_cert().user_public_key);
    let mut writes = copies.document_writes();
    let held = tokio::time::timeout(WAITED, async {
        loop {
            if let Some(hint) = listed_at(copies, &own, id).await {
                return copies.hold_as_member(workspace, &hint).await.map(drop);
            }
            // The list moves as the founding device's write reaches this device's copy.
            loop {
                match writes.recv().await {
                    Ok(write)
                        if write.workspace == own
                            && write.app == IDENTITY_LIST.0
                            && write.record == IDENTITY_LIST.1 =>
                    {
                        break;
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return Err("this device stopped keeping its workspaces".to_owned());
                    }
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        Err("the person's list did not say where it is reached in time".to_owned())
    });
    if let Err(why) = held {
        tracing::warn!(workspace = %id, %why, "a workspace made on the founding device is not held here yet");
    }
}

/// Where the person's list, as this device's copy of their own workspace `own` holds it, says
/// `workspace` is reached, once it says so.
async fn listed_at(
    copies: &tilo_workspace_service::workspaces::Workspaces,
    own: &str,
    workspace: &str,
) -> Option<TransportHint> {
    let list = person_workspace::read_on::<WorkspaceList>(copies, own, IDENTITY_LIST)
        .await
        .ok()?;
    let address = list
        .listed()
        .find(|(listed, _)| listed.to_string() == workspace)?
        .1
        .address()?;
    postcard::from_bytes(&address).ok()
}

#[cfg(test)]
#[path = "founding_create_tests.rs"]
mod tests;
