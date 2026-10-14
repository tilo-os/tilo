//! Compute placement as an action: provider [`COMPUTE_PROVIDER`]
//! (`tilo.compute`).
//!
//! A device asks the relay which of its person's devices should run a
//! compute job by invoking [`PLACE_JOB`] (`compute.place`) on the
//! workspace's compute resource (`workspace.compute:<workspace id>`). The
//! answer names the chosen worker, the terms it won on, and why every other
//! advertised device was passed over ([`crate::placement`]); when none can
//! take the job, the worker is `null` and the reasons say why, so the caller
//! runs the job locally or not at all.
//!
//! The action only chooses. The requester then binds its job to that exact
//! worker and submits it over the compute ALPN, where the relay admits it
//! again against the worker's live advertisement and the worker runs it as a
//! signed package entrypoint under its launcher sandbox. Nothing here starts
//! work, reads content or widens what a worker may do.
//!
//! Who may ask is who may drive this host's compute: a person acting
//! themselves from one of the relay owner's devices
//! ([`HostOperation::ComputeControl`](crate::host_authority::HostOperation::ComputeControl)),
//! the principal the compute control stream admits. The remote-action door
//! already decided the device may act in the workspace. Anyone else is
//! offered nothing, and invoking is [`ActionHostError::UnknownAction`], the
//! same answer as for an action that does not exist.

use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CallerPrincipal, CataloguePage, CatalogueProvider,
    CatalogueQuery, CatalogueResource, CatalogueRow, InvocationLedger, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, RemoteActionMessage,
};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionKind, ActionOutput, ActionParam,
    ExecutionLocus, InputSpec, InvocationId, InvocationUpdate, OperationKind, ParamType, RiskLevel,
};

use crate::app_serving::AppServingPlane;
use crate::host_authority::HostOperation;
use crate::peer::WorkspaceRegistry;
use crate::placement::{ComputeRequirements, PlacementCandidate};

/// The provider compute placement is answered under. It is the host's, not
/// an app's, so a caller may name it without reading a catalogue first.
pub(crate) const COMPUTE_PROVIDER: &str = "tilo.compute";
/// The resource kind of a workspace's compute resource.
pub(crate) const COMPUTE_KIND: &str = "workspace.compute";
/// Choosing the device a compute job runs on.
pub(crate) const PLACE_JOB: &str = "compute.place";

/// The id of workspace `workspace_id`'s compute resource.
pub(crate) fn compute_resource(workspace_id: u64) -> String {
    format!("{COMPUTE_KIND}:{workspace_id}")
}

fn place_row(workspace_id: u64) -> CatalogueRow {
    let resource = compute_resource(workspace_id);
    let label = "Compute";
    CatalogueRow {
        provider: COMPUTE_PROVIDER.into(),
        app_id: COMPUTE_PROVIDER.into(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: resource.clone(),
            kind: COMPUTE_KIND.into(),
            label: label.into(),
            presentation: None,
        },
        action: Some(ActionDescriptor {
            semantic_identity: None,
            address: ActionAddress::new(resource, PLACE_JOB),
            resource_kind: COMPUTE_KIND.into(),
            resource_label: label.into(),
            action_label: "Choose a device for a compute job".into(),
            description: Some(
                "Names the device of yours best placed to run a job of this app now, \
                 and why each other device was passed over. Runs nothing."
                    .into(),
            ),
            locus: ExecutionLocus::ServerPreferred,
            kind: OperationKind::Effect,
            contract: ActionContract {
                kind: ActionKind::Read,
                risk: RiskLevel::None,
                ..ActionContract::default()
            },
            input: InputSpec::None,
            params: vec![
                ActionParam::required(
                    "app",
                    ParamType::Text,
                    "The installed app whose entrypoint the job runs.",
                ),
                ActionParam::optional("gpu", ParamType::Boolean, "Whether the job needs a GPU."),
            ],
            view: None,
        }),
        has_children: false,
        visibility: None,
    }
}

fn invalid(reason: impl Into<String>) -> ActionHostError {
    ActionHostError::InvalidRequest {
        reason: reason.into(),
    }
}

/// The job terms named by an invocation's arguments.
fn job_terms(argument: &InvokeArgument) -> Result<(String, bool), ActionHostError> {
    let empty = Map::new();
    let arguments = match argument {
        InvokeArgument::None => &empty,
        InvokeArgument::Json(object) => object,
        InvokeArgument::Text(_) => return Err(invalid("compute placement takes named arguments")),
    };
    let app = match arguments.get("app") {
        Some(Value::String(app)) if !app.is_empty() => app.clone(),
        Some(_) => return Err(invalid("`app` must be text")),
        None => return Err(invalid("`app` is required")),
    };
    let gpu = match arguments.get("gpu") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(gpu)) => *gpu,
        Some(_) => return Err(invalid("`gpu` must be true or false")),
    };
    Ok((app, gpu))
}

fn candidate_json(candidate: &PlacementCandidate) -> Value {
    match &candidate.verdict {
        Ok(rank) => json!({
            "device": hex::encode(candidate.device),
            "eligible": true,
            "rank": rank,
        }),
        Err(reason) => json!({
            "device": hex::encode(candidate.device),
            "eligible": false,
            "reason": reason.token(),
        }),
    }
}

/// Compute placement for every workspace this relay carries.
#[derive(Clone)]
pub(crate) struct ComputeActions {
    shared: Arc<Shared>,
}

struct Shared {
    /// The person whose devices drive this host's compute.
    own_user: [u8; 32],
    workspaces: WorkspaceRegistry,
    runtime: tokio::runtime::Handle,
    /// What each admitted invocation came to.
    ledger: Mutex<InvocationLedger<InvocationState>>,
}

impl ComputeActions {
    /// Placement over `workspaces`' compute advertisements, for the devices
    /// of `own_user`. Call it inside the runtime the relay runs on.
    pub(crate) fn new(own_user: [u8; 32], workspaces: WorkspaceRegistry) -> Self {
        Self {
            shared: Arc::new(Shared {
                own_user,
                workspaces,
                runtime: tokio::runtime::Handle::current(),
                ledger: Mutex::new(InvocationLedger::default()),
            }),
        }
    }

    fn ledger(&self) -> std::sync::MutexGuard<'_, InvocationLedger<InvocationState>> {
        self.shared
            .ledger
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether `caller` may drive this host's compute.
    fn permits(&self, caller: &Caller) -> bool {
        matches!(caller.principal(), CallerPrincipal::Person(_))
            && caller.user_public_key().is_some_and(|user| {
                HostOperation::ComputeControl.permits(&self.shared.own_user, user)
            })
    }

    /// Whether `caller` started invocation `id` here.
    fn admitted(&self, caller: &Caller, id: &InvocationId) -> bool {
        self.ledger().get(caller, id).is_some()
    }

    /// This host for requests about workspace `workspace_id`.
    fn in_workspace(&self, workspace_id: u64) -> InWorkspace {
        InWorkspace {
            actions: self.clone(),
            workspace_id,
        }
    }

    /// The placement for a job of `app` (needing a GPU when `gpu`) asked by
    /// `requester` in `workspace_id`, as the action answers it.
    async fn place(&self, workspace_id: u64, requester: [u8; 32], app: &str, gpu: bool) -> Value {
        let needs = ComputeRequirements {
            requester,
            workspace_id,
            app_id: app,
            gpu,
        };
        let workspace = self
            .shared
            .workspaces
            .lock()
            .await
            .get(&workspace_id)
            .cloned();
        let placed = match workspace {
            Some(workspace) => {
                let runtime = workspace.lock().await;
                let placed = runtime.compute.place(&needs, tilo_clock::unix_now_ms());
                drop(runtime);
                placed
            }
            None => crate::placement::place_compute_job(&needs, []),
        };
        match placed {
            Ok(placement) => {
                tracing::info!(workspace_id, app, gpu, %placement, "placed compute job");
                json!({
                    "worker": hex::encode(placement.worker),
                    "reason": placement.to_string(),
                    "candidates": placement.considered.iter().map(candidate_json).collect::<Vec<_>>(),
                })
            }
            Err(refused) => {
                tracing::info!(workspace_id, app, gpu, %refused, "no compute worker for job");
                json!({
                    "worker": Value::Null,
                    "reason": refused.to_string(),
                    "candidates": refused.considered.iter().map(candidate_json).collect::<Vec<_>>(),
                })
            }
        }
    }
}

/// [`ComputeActions`] for one workspace.
#[derive(Clone)]
struct InWorkspace {
    actions: ComputeActions,
    workspace_id: u64,
}

impl InWorkspace {
    fn page(&self, caller: &Caller, query: &CatalogueQuery) -> CataloguePage {
        if !self.actions.permits(caller) {
            return CataloguePage::from_rows("0".into(), Vec::new(), query);
        }
        let mut page =
            CataloguePage::from_rows("0".into(), vec![place_row(self.workspace_id)], query);
        if query.provider.is_none() {
            page.providers = vec![provider_entry()];
        }
        page
    }
}

fn provider_entry() -> CatalogueProvider {
    CatalogueProvider {
        provider: COMPUTE_PROVIDER.into(),
        app_id: COMPUTE_PROVIDER.into(),
        room: String::new(),
        record_id: String::new(),
        browsable: false,
    }
}

impl ActionHost for InWorkspace {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        Pending::ready(Ok(self.page(caller, &query)))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        if let Err(refused) = tilo_actions::appearance::refuse_here(&request) {
            return Pending::ready(Err(refused));
        }
        let address = &request.target.address;
        // Unknown and forbidden are the same answer.
        let Some(requester) = caller
            .device_public_key()
            .copied()
            .filter(|_| self.actions.permits(caller))
            .filter(|_| {
                address.action == PLACE_JOB
                    && address.resource == compute_resource(self.workspace_id)
            })
        else {
            return Pending::ready(Err(ActionHostError::UnknownAction));
        };
        let (app, gpu) = match job_terms(&request.argument) {
            Ok(terms) => terms,
            Err(error) => return Pending::ready(Err(error)),
        };
        let invocation = request.invocation;
        // The ledger's guard must not outlive the reservation: a retry reads
        // it again below.
        let reservation = self.actions.ledger().reserve(caller, &request);
        match reservation {
            Err(error) => return Pending::ready(Err(error)),
            Ok(tilo_actions::remote::Reservation::Admitted) => {
                return Pending::ready(
                    self.actions
                        .ledger()
                        .get(caller, &invocation)
                        .map(InvocationState::receipt)
                        .ok_or(ActionHostError::UnknownInvocation),
                );
            }
            Ok(tilo_actions::remote::Reservation::Reserved) => {}
        }
        let (actions, workspace_id) = (self.actions.clone(), self.workspace_id);
        let (completer, answer) = tilo_actions::remote::pending();
        self.actions.shared.runtime.spawn(async move {
            let value = actions.place(workspace_id, requester, &app, gpu).await;
            let state = InvocationState::new(
                invocation,
                InvocationUpdate::Succeeded {
                    invocation,
                    output: ActionOutput {
                        value: Some(value),
                        resources: Vec::new(),
                    },
                },
            );
            let receipt = state.receipt();
            actions.ledger().complete(&invocation, state);
            completer.complete(Ok(receipt));
        });
        answer
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.actions.ledger().get(caller, id).cloned()
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Result<(), ActionHostError> {
        if !self.actions.admitted(caller, id) {
            return Err(ActionHostError::UnknownInvocation);
        }
        Err(ActionHostError::NotCancellable {
            reason: "compute placement completes at once".into(),
        })
    }
}

/// What a certified host answers remote actions addressed to it with:
/// compute placement (provider [`COMPUTE_PROVIDER`]), and whatever plane
/// it already had for everything else.
pub(crate) struct ComputePlane {
    /// Answers compute placement.
    pub(crate) compute: ComputeActions,
    /// Answers everything else: room governance and the embedder's apps.
    pub(crate) rest: Option<Arc<dyn AppServingPlane>>,
}

impl ComputePlane {
    /// Whether `message` from `caller` is compute placement to answer.
    fn places(&self, caller: &Caller, message: &RemoteActionMessage) -> bool {
        match message {
            RemoteActionMessage::CatalogueRequest { query, .. } => {
                query.provider.as_deref() == Some(COMPUTE_PROVIDER)
            }
            RemoteActionMessage::Invoke(request) => request.target.provider == COMPUTE_PROVIDER,
            RemoteActionMessage::Observe { invocation, .. }
            | RemoteActionMessage::Cancel { invocation } => {
                self.compute.admitted(caller, invocation)
            }
            _ => false,
        }
    }
}

/// `reply`, a whole-catalogue page from the host's other providers, with
/// compute placement named among them.
fn name_compute_beside(reply: RemoteActionMessage) -> RemoteActionMessage {
    match reply {
        RemoteActionMessage::CatalogueResponse {
            correlation,
            result: Ok(mut page),
        } => {
            page.providers.push(provider_entry());
            RemoteActionMessage::CatalogueResponse {
                correlation,
                result: Ok(page),
            }
        }
        other => other,
    }
}

/// Whether `message` asks for every provider's catalogue at once.
fn whole_catalogue(message: &RemoteActionMessage) -> bool {
    matches!(
        message,
        RemoteActionMessage::CatalogueRequest { query, .. }
            if query.provider.is_none()
                && query.view_resource.is_none()
                && query.canonical_route.is_none()
    )
}

impl AppServingPlane for ComputePlane {
    fn answers(&self, caller: &Caller, message: &RemoteActionMessage) -> bool {
        self.places(caller, message)
            || (whole_catalogue(message) && self.compute.permits(caller))
            || self
                .rest
                .as_ref()
                .is_some_and(|rest| rest.answers(caller, message))
    }

    fn serve(
        &self,
        workspace_id: u64,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        let host = self.compute.in_workspace(workspace_id);
        if self.places(caller, &message) {
            return tilo_actions::remote::serve(&host, caller, message);
        }
        if !whole_catalogue(&message) || !self.compute.permits(caller) {
            return self.rest.as_ref()?.serve(workspace_id, caller, message);
        }
        // The whole catalogue: the rest's, naming this provider beside its
        // own, or this provider's alone when nothing else answers.
        match self
            .rest
            .as_ref()
            .filter(|rest| rest.answers(caller, &message))
        {
            Some(rest) => Some(
                rest.serve(workspace_id, caller, message)?
                    .map(name_compute_beside),
            ),
            None => tilo_actions::remote::serve(&host, caller, message),
        }
    }
}

#[cfg(test)]
mod tests;
