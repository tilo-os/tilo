//! The typed actions of the workspace resources this device keeps for the person's own shell,
//! applied here as each resource's owner declares them.
//!
//! A resource is named as every caller names one: `record:<app>:<id>` in the caller's workspace.
//! Its type is the one its owner declares for it ([`Declared`]): the list of a workspace's
//! agents (the workspace's), its emoji (Chat's), its contact directory (Contacts'), the person's
//! settings, their list of workspaces and the record of their devices in the workspace whose only
//! member is them (the settings', the identity's and the devices'). Any app's record also moves
//! to another workspace by the system's `move` ([`Workspaces::move_record`]). The workspace's
//! grants on the owner's node decide the call, as a window of the app decides a click. What the
//! action changed is this device's write of the record in the workspace's store, so every app of
//! the workspace and the person's other devices take it as any other.
//!
//! It is registered once on every device's providers ([`register`]) under the resource provider
//! itself: a call made on this device reaches it ([`crate::workspaces::providers::Providers::invoke`]),
//! and so does a call a device of the workspace makes here, a person's server included, decided
//! by the workspace's grants for that device and the record's app as every other device call is.

use std::sync::Arc;

use serde_json::Map;
use tilo_actions::api::{self, InvocationProvenance};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, RESOURCE_PROVIDER, ReceiptStatus,
};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId, LocalTransport};
use tilo_server_protocol::WorkspaceId;

use crate::workspaces::WeakWorkspaces;
use crate::workspaces::Workspaces;
use crate::workspaces::person_workspace::{
    self,
    device_answers::{DEVICE_ANSWERS, DeviceAnswers},
    devices::{DEVICES, Devices},
};
use crate::workspaces::providers::Reach;

/// Apply the typed actions of the resources [`Declared`] names on this device from now on, for
/// its own callers and the devices of the workspaces it keeps.
pub fn register(workspaces: &Workspaces) {
    workspaces.providers().provide(
        RESOURCE_PROVIDER,
        RESOURCE_PROVIDER,
        Reach::Devices,
        Arc::new(ResourceActions {
            workspaces: workspaces.downgrade(),
            declared: Arc::from(Declared::all()),
            taken_up: TakenUp::default(),
        }),
    );
}

/// The resources of the workspaces a device keeps, as their owners' actions change them.
#[derive(Clone)]
struct ResourceActions {
    /// Held without keeping them alive: this answers from a provider they keep.
    workspaces: WeakWorkspaces,
    declared: Arc<[Declared]>,
    taken_up: TakenUp,
}

/// The person's writes to their own workspace this device answered as taken up before it held
/// the workspace, by invocation: who asked, and how each stands, so following one hears it was
/// made or why not.
#[derive(Clone, Default)]
struct TakenUp {
    states:
        Arc<std::sync::Mutex<std::collections::HashMap<InvocationId, (Caller, InvocationState)>>>,
    /// Moves each time one of them settles.
    settled: Arc<tokio::sync::watch::Sender<u64>>,
}

impl TakenUp {
    fn keep(
        &self,
        caller: &Caller,
        invocation: InvocationId,
        update: tilo_actions::InvocationUpdate,
    ) {
        self.states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                invocation,
                (caller.clone(), InvocationState::new(invocation, update)),
            );
        self.settled
            .send_modify(|moves| *moves = moves.wrapping_add(1));
    }

    fn state(&self, caller: &Caller, invocation: &InvocationId) -> Option<InvocationState> {
        self.states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(invocation)
            .filter(|(owner, _)| caller.continues(owner))
            .map(|(_, state)| state.clone())
    }
}

/// How an invocation that ran stands, by its receipt's `status`.
fn settled_update(
    invocation: InvocationId,
    status: ReceiptStatus,
) -> tilo_actions::InvocationUpdate {
    use tilo_actions::InvocationUpdate;
    match status {
        ReceiptStatus::Completed { output } => InvocationUpdate::Succeeded { invocation, output },
        ReceiptStatus::Failed { error } => InvocationUpdate::Failed { invocation, error },
        ReceiptStatus::Rejected { error } => InvocationUpdate::Failed {
            invocation,
            error: ActionFailure {
                code: "refused".to_owned(),
                message: error.to_string(),
                details: None,
                retryable: false,
            },
        },
        ReceiptStatus::Cancelled { message } => InvocationUpdate::Cancelled {
            invocation,
            message,
        },
        ReceiptStatus::Accepted => InvocationUpdate::Accepted { invocation },
    }
}

/// A resource its owner declares by the app it is of and its id, with the type its actions are:
/// a kind every device declares, so the record is the workspace store's.
struct Declared {
    /// The app it is of; `None` for every app's.
    app: Option<&'static str>,
    id: String,
    kind: tilo_resource::ResourceKind,
}

impl Declared {
    fn all() -> Vec<Self> {
        let workspace = |kind: tilo_resource::ResourceKind, app: &'static str| Self {
            app: Some(app),
            id: collab_types::workspace_document_id(kind.kind()),
            kind,
        };
        vec![
            workspace(
                tilo_resource::ResourceKind::of::<tilo_workspace_agent::WorkspaceAgents>()
                    .hosted_by(tilo_workspace_agent::OWNER, "Workspace"),
                tilo_workspace_agent::OWNER,
            ),
            workspace(
                tilo_resource::ResourceKind::of::<tilo_workspace_emoji::WorkspaceEmoji>()
                    .hosted_by(tilo_workspace_emoji::OWNER, "Chat"),
                tilo_workspace_emoji::OWNER,
            ),
            workspace(
                tilo_resource::ResourceKind::of::<tilo_contact_directory::ContactDirectory>()
                    .hosted_by(tilo_app_id::apps::CONTACTS, "Contacts"),
                tilo_app_id::apps::CONTACTS,
            ),
            Self {
                app: Some(tilo_user_identity::workspace_list::APP),
                id: tilo_user_identity::workspace_list::RESOURCE.to_owned(),
                kind: tilo_user_identity::workspace_list::kind(),
            },
            // The record of the person's devices: what each of their devices says of itself (its
            // screens, its session, how it wakes), kept by that device's shell.
            Self {
                app: Some(DEVICES.0),
                id: DEVICES.1.to_owned(),
                kind: tilo_resource::ResourceKind::of::<Devices>().hosted_by(DEVICES.0, "Devices"),
            },
            // What the person answered about their devices' cameras and microphones, kept by the
            // device that asked them.
            Self {
                app: Some(DEVICE_ANSWERS.0),
                id: DEVICE_ANSWERS.1.to_owned(),
                kind: tilo_resource::ResourceKind::of::<DeviceAnswers>()
                    .hosted_by(DEVICE_ANSWERS.0, "Devices"),
            },
        ]
    }

    fn names(&self, app: &str, id: &str) -> bool {
        self.app.is_none_or(|declared| declared == app) && self.id == id
    }
}

/// Who a call of a resource's action is decided as, in the workspace where `person` is the
/// caller's person: the caller as its transport admitted it (an agent bounded by its person's
/// answers, software by its own grants), or, where the person's own shell names an agent of
/// theirs as the one asking ([`tilo_actions::ActionContext::invoker`], what the change is
/// attributed to), that agent, handed the person's session as the shell hands it. An invoker that
/// is neither the person nor an agent of theirs is no one here, and asks for nothing.
/// An agent its person narrowed with their answers here takes only what those answers name
/// ([`tilo_action_policy::WorkspacePolicy::answers_narrow`]): the delegation it was handed adds
/// nothing.
fn deciding_actor(
    caller: &Caller,
    context: Option<&tilo_actions::ActionContext>,
    (person, policy, now): (&str, &tilo_action_policy::WorkspacePolicy, u64),
) -> Option<tilo_broker_core::BrokerActor> {
    let someone_else = context
        .and_then(|context| context.invoker.as_ref())
        .map(|invoker| &invoker.acting)
        .filter(|acting| {
            acting
                .member_id()
                .and_then(tilo_actions::MemberId::person_id)
                != Some(person)
        });
    let asking = match someone_else {
        None => caller.clone(),
        Some(acting) => match acting
            .member_id()
            .and_then(tilo_actions::MemberId::as_agent)
        {
            Some((agent, of)) if of.as_str() == person && caller.acting().is_person() => {
                caller.clone().delegated_to(agent)
            }
            _ => return None,
        },
    };
    let narrowed = asking
        .acting()
        .member_id()
        .and_then(tilo_actions::MemberId::as_agent)
        .filter(|(agent, of)| {
            policy.answers_narrow(
                agent,
                of.as_str(),
                &tilo_action_policy::Context::at_secs(now),
            )
        });
    Some(match narrowed {
        Some((agent, of)) => {
            tilo_broker_core::BrokerActor::agent(agent, of.as_str()).for_person(of.as_str())
        }
        None => tilo_broker_core::BrokerActor::for_caller(&asking),
    })
}

/// `workspace` under its policy `policy`, for deciding a call there: without the grants
/// that last one use, which a call decided here never spends.
fn under_policy(
    workspace: &str,
    policy: &tilo_action_policy::WorkspacePolicy,
) -> Option<tilo_desktop_model::Workspace> {
    let mut scoped = tilo_desktop_model::Workspace::new(
        tilo_desktop_model::WorkspaceId::parse(workspace)?,
        String::new(),
    );
    scoped.policy = policy.clone();
    scoped
        .policy
        .grants
        .retain(|grant| grant.lifetime != tilo_action_types::Lifetime::Once);
    Some(scoped)
}

/// A call refused for `error`.
fn rejected(error: ActionCallError) -> ReceiptStatus {
    ReceiptStatus::Rejected { error }
}

/// A failure that ran nothing to its end, with why.
fn failed(code: &str, message: String) -> ReceiptStatus {
    ReceiptStatus::Failed {
        error: ActionFailure {
            code: code.to_owned(),
            message,
            details: None,
            retryable: false,
        },
    }
}

/// The named arguments a resource's action takes, or the refusal that it took something else.
fn named_arguments(
    argument: &InvokeArgument,
) -> Result<Map<String, serde_json::Value>, Box<ReceiptStatus>> {
    match argument {
        InvokeArgument::Json(arguments) => Ok(arguments.clone()),
        InvokeArgument::None => Ok(Map::new()),
        InvokeArgument::Text(_) => Err(Box::new(ReceiptStatus::Rejected {
            error: ActionCallError::InvalidRequest {
                reason: "a resource's action takes named arguments".to_owned(),
            },
        })),
    }
}

/// A resource's action, decided by the workspace's grants on the record's app and this device's,
/// applied to the record as it stands.
struct Asked<'a> {
    declared: &'a Declared,
    request: &'a InvokeRequest,
    arguments: &'a Map<String, serde_json::Value>,
    workspace_policy: &'a tilo_desktop_model::Workspace,
    decider: &'a tilo_broker_core::BrokerActor,
    permissions: &'a collaboration::permissions::PermissionContext,
    unix_time: u64,
    ambient: &'a tilo_actions::ActionContext,
    actor: &'a [u8],
    registry: &'a tilo_actions::ActionRegistry,
}

impl Asked<'_> {
    /// The action on `doc` as it stands, or the refusal decided against it.
    fn on(&self, doc: &mut automerge::AutoCommit) -> Result<(), Box<ReceiptStatus>> {
        let declared = self.declared;
        let tree = declared
            .kind
            .publish(declared.kind.kind(), doc)
            .map_err(|error| failed("unreadable", error.to_string()))
            .map_err(Box::new)?;
        let address = &self.request.target.address;
        let (resource, action) = api::locate(std::slice::from_ref(&tree), address)
            .ok_or_else(|| rejected(ActionCallError::UnknownAction))
            .map_err(Box::new)?;
        let app = declared.kind.declaring_app();
        let (verdict, _) = self.decider.action_verdict_in(
            self.workspace_policy,
            app,
            (&resource.kind, resource.id.as_str(), &action.id),
            &action.need_for(Some(self.arguments), None),
            self.unix_time,
        );
        let device =
            self.permissions
                .invocation_answer(app, resource, action, Some(self.arguments), None);
        match verdict.min(device) {
            collaboration::permissions::Answer::Deny => {
                return Err(Box::new(rejected(ActionCallError::NotPermitted)));
            }
            // Only the person's approve of the asked record this call was kept as answers an ask:
            // a confirmation the call writes itself is none.
            collaboration::permissions::Answer::Ask if self.request.approval().is_none() => {
                return Err(Box::new(rejected(ActionCallError::confirmation_required())));
            }
            _ => {}
        }
        api::invoke_json(
            doc,
            std::slice::from_ref(&tree),
            address,
            self.arguments,
            (
                InvocationProvenance::caller(self.actor).within(self.ambient),
                self.request.approval().is_some(),
            ),
            self.registry,
            &mut LocalTransport::new(),
        )
        .map(drop)
        .map_err(|error| tilo_agent_session::record_actions::api_status(&error))
        .map_err(Box::new)
    }
}

impl ResourceActions {
    /// The workspaces this answers for, while the device still keeps them.
    fn here(&self) -> Option<Workspaces> {
        self.workspaces.upgrade()
    }

    /// Whether `caller` is this device's person asking in their own workspace of `workspaces`.
    fn own_workspace_of(workspaces: &Workspaces, caller: &Caller) -> bool {
        caller.workspace() == Some(person_workspace::workspace_id(workspaces).as_str())
            && caller.user_public_key() == Some(&workspaces.device_cert().user_public_key)
    }

    async fn applied(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<ActionOutput, ReceiptStatus> {
        let invalid = |reason: &str| {
            rejected(ActionCallError::InvalidRequest {
                reason: reason.to_owned(),
            })
        };
        let (app, id) = tilo_actions::remote::resource_parts(&request.target.provider)
            .ok_or_else(|| rejected(ActionCallError::UnknownAction))?;
        let workspace = caller
            .workspace()
            .ok_or_else(|| invalid("a resource's action is asked in the workspace that keeps it"))?
            .to_owned();
        // Any app's record moves to another workspace of the caller's: the store judges each
        // side of it as the caller's write there.
        if request.target.address.action == tilo_resource_store::MOVE_VERB {
            return self.moved(&workspace, app, id, caller, request).await;
        }
        let declared = self
            .declared
            .iter()
            .find(|declared| declared.names(app, id))
            .ok_or_else(|| rejected(ActionCallError::UnknownAction))?;
        let workspace_id =
            WorkspaceId::parse(&workspace).ok_or_else(|| invalid("that is not a workspace"))?;
        let workspaces = self
            .here()
            .ok_or_else(|| rejected(ActionCallError::UnknownAction))?;
        // The person's devices write into their own workspace before this device is taken in
        // (what its desktop says of it, as it starts): the person's own write waits until it is,
        // and is decided then as every other.
        if Self::own_workspace_of(&workspaces, caller)
            && !person_workspace::when_held(&workspaces).await
        {
            return Err(failed("not_written", person_workspace::STOPPED.to_owned()));
        }
        let arguments = named_arguments(&request.argument).map_err(|status| *status)?;
        // Whoever the workspace does not admit is refused before anything of it is read.
        let policy = workspaces
            .workspace_policy(workspace_id, caller)
            .await
            .ok_or_else(|| rejected(ActionCallError::NotPermitted))?;
        let person = policy
            .access
            .person_id()
            .ok_or_else(|| rejected(ActionCallError::NotPermitted))?
            .to_owned();
        let member = tilo_actions::MemberId::person(person.clone());
        // Who the call is decided as: never the person alone where someone else asks.
        let decider = deciding_actor(
            caller,
            request.context.as_ref(),
            (&person, &policy.access.policy, policy.unix_time),
        )
        .ok_or_else(|| rejected(ActionCallError::NotPermitted))?;
        let workspace_policy = under_policy(&workspace, &policy.access.policy)
            .ok_or_else(|| invalid("that is not a workspace"))?;
        // What this device itself may do here bounds every call it carries.
        let permissions = collaboration::permissions::PermissionContext::new(
            collaboration::permissions::Acting::person(person),
            policy.sandbox.clone(),
            policy.access.clone(),
        )
        .at_unix_time(policy.unix_time);
        // Who asked: the person, or whoever the shell says acts for them (an agent it admitted),
        // which the change's history names.
        let mut ambient = request.context.clone().unwrap_or_default();
        if ambient.invoker.is_none() {
            ambient = ambient.with_invoker(tilo_actions::ActionInvoker::member(member.clone()));
        }
        // The change is the record's own app's action, which is what the record's history and its
        // receipt check read: a name guessed from the record's type is no app the record has, and
        // every device's write of it would be refused.
        ambient = ambient.with_app(app, declared.kind.app_title().unwrap_or(app));
        let actor = member.actor_bytes();
        let mut registry = tilo_actions::ActionRegistry::new();
        declared.kind.register(&mut registry);
        let asked = Asked {
            declared,
            request,
            arguments: &arguments,
            workspace_policy: &workspace_policy,
            decider: &decider,
            permissions: &permissions,
            unix_time: policy.unix_time,
            ambient: &ambient,
            actor: &actor,
            registry: &registry,
        };
        // The action is this device's write of the record in the workspace's store, founding it
        // as its kind does where the store holds none yet.
        workspaces
            .author_record(&workspace, (app, declared.kind.kind(), id), |doc| {
                Ok(asked.on(doc))
            })
            .await
            .map_err(|error| failed("not_written", error))?
            .map(|()| ActionOutput::default())
            .map_err(|status| *status)
    }

    /// `caller`'s move of the record (`app`, `id`) to the workspace `request` names, judged by
    /// the store as the caller's write on each side of it.
    async fn moved(
        &self,
        workspace: &str,
        app: &str,
        id: &str,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<ActionOutput, ReceiptStatus> {
        let argument = |name: &str| match &request.argument {
            InvokeArgument::Json(arguments) => arguments
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            _ => None,
        };
        let (Some(to), Some(target)) = (argument("to"), argument("target")) else {
            return Err(ReceiptStatus::Rejected {
                error: ActionCallError::InvalidRequest {
                    reason: "a move names the workspace it goes `to` and its `target` id there"
                        .to_owned(),
                },
            });
        };
        let workspaces = self
            .here()
            .ok_or_else(|| rejected(ActionCallError::UnknownAction))?;
        workspaces
            .move_record((workspace, &to), (app, id, &target), caller)
            .await
            .map_err(|error| failed("refused", error))?;
        Ok(ActionOutput::default())
    }
}

impl ActionProvider for ResourceActions {
    fn serves(&self, provider: &str) -> bool {
        tilo_actions::remote::resource_parts(provider)
            .is_some_and(|(app, id)| self.declared.iter().any(|declared| declared.names(app, id)))
    }

    /// Their actions are listed where the resources are read; nothing is listed here.
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::from_rows(
            "0".to_owned(),
            Vec::new(),
            &query,
        )))
    }

    /// Answered once the action ran: its receipt says how it ended, and nothing is left to
    /// follow. The person's own write to their workspace before this device holds it is
    /// answered accepted at once, since the device may never be taken in: it is written once
    /// the device is, and following the invocation ([`Self::observe`]) hears how it went.
    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return Pending::ready(Err(ActionCallError::Unavailable {
                reason: "this device is not running".into(),
            }));
        };
        let (done, answer) = tilo_actions::remote::pending();
        let (actions, caller) = (self.clone(), caller.clone());
        runtime.spawn(async move {
            let receipt = |status| {
                Ok(InvocationReceipt {
                    invocation: request.invocation,
                    status,
                })
            };
            let mut done = Some(done);
            if let Some(workspaces) = actions.here()
                && Self::own_workspace_of(&workspaces, &caller)
                && !person_workspace::holds_person_workspace(&workspaces).await
                && let Some(done) = done.take()
            {
                let invocation = request.invocation;
                actions.taken_up.keep(
                    &caller,
                    invocation,
                    tilo_actions::InvocationUpdate::Accepted { invocation },
                );
                done.complete(receipt(ReceiptStatus::Accepted));
            }
            let status = match Box::pin(actions.applied(&caller, &request)).await {
                Ok(output) => ReceiptStatus::Completed { output },
                Err(status) => status,
            };
            match done {
                Some(done) => done.complete(receipt(status)),
                None => actions.taken_up.keep(
                    &caller,
                    request.invocation,
                    settled_update(request.invocation, status),
                ),
            }
        });
        answer
    }

    /// Only a write taken up before this device held the person's workspace runs on after its
    /// receipt; every other is answered once it ran.
    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.taken_up.state(caller, id)
    }

    /// Resolves when one of the writes taken up settles, or within a second regardless, as a
    /// waiter's deadline is looked at only when it wakes.
    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.taken_up.state(caller, id)?;
        let runtime = tokio::runtime::Handle::try_current().ok()?;
        let mut settled = self.taken_up.settled.subscribe();
        let (done, changed) = tilo_actions::remote::pending();
        runtime.spawn(async move {
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(1), settled.changed()).await;
            done.complete(());
        });
        Some(changed)
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }
}

#[cfg(test)]
#[path = "resource_actions_tests.rs"]
mod tests;
