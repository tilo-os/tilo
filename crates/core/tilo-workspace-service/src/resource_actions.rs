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
//! action changed is written into the workspace as this device's write, signed and sealed as the
//! device, so every app of the workspace and the person's other devices take it as any other; in
//! the person's own workspace, as each of its records is written
//! ([`person_workspace::edit_on`]).
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
use tilo_peer_state::workspace_documents::{DeviceWrites as _, DocumentWriting};
use tilo_server_protocol::WorkspaceId;

use crate::device_documents::ServiceWrites;
use crate::workspaces::Workspaces;
use crate::workspaces::person_workspace::{
    self,
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
            writes: ServiceWrites::of(workspaces.clone()),
            workspaces: workspaces.clone(),
            declared: Arc::from(Declared::all()),
        }),
    );
}

/// The resources of the workspaces a device keeps, as their owners' actions change them.
#[derive(Clone)]
struct ResourceActions {
    workspaces: Workspaces,
    writes: ServiceWrites,
    declared: Arc<[Declared]>,
}

/// A resource its owner declares by the app it is of and its id, with the type its actions are.
struct Declared {
    /// The app it is of; `None` for every app's.
    app: Option<&'static str>,
    id: String,
    kind: tilo_resource::ResourceKind,
    /// How it is made when nothing wrote it yet.
    founding: Founding,
}

/// How a declared resource is made the first time an action changes it: byte for byte as every
/// other device makes it, so two first writes merge into one.
#[derive(Clone, Copy)]
enum Founding {
    /// A workspace resource of the app, with its identity.
    Identified,
    /// The person's own, made from its type alone.
    Typed,
}

impl Declared {
    fn all() -> Vec<Self> {
        let workspace = |kind: tilo_resource::ResourceKind, app: &'static str| Self {
            app: Some(app),
            id: collab_types::workspace_document_id(kind.kind()),
            kind,
            founding: Founding::Identified,
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
                founding: Founding::Typed,
            },
            // The record of the person's devices: what each of their devices says of itself (its
            // screens, its session, how it wakes), kept by that device's shell.
            Self {
                app: Some(DEVICES.0),
                id: DEVICES.1.to_owned(),
                kind: tilo_resource::ResourceKind::of::<Devices>().hosted_by(DEVICES.0, "Devices"),
                founding: Founding::Typed,
            },
        ]
    }

    fn names(&self, app: &str, id: &str) -> bool {
        self.app.is_none_or(|declared| declared == app) && self.id == id
    }

    /// The resource as it is made, in `workspace`.
    fn found(&self, workspace: &str) -> Result<automerge::AutoCommit, String> {
        use collaboration::resource::{DocumentCheckout, DocumentId, ResourceType};
        let initialize = |doc: &mut automerge::AutoCommit| {
            self.kind.initialize(doc).map_err(|error| error.to_string())
        };
        match self.founding {
            Founding::Typed => {
                let mut doc = automerge::AutoCommit::new().with_actor(
                    tilo_resource::genesis::actor("", self.kind.kind(), workspace),
                );
                initialize(&mut doc)?;
                doc.commit_with(
                    automerge::transaction::CommitOptions::default()
                        .with_message(tilo_actions::FrameworkChange::Genesis.change_message())
                        .with_time(0),
                );
                Ok(doc)
            }
            Founding::Identified => {
                let id = DocumentId::parse(&self.id).ok_or("the resource has no id")?;
                let checkout = DocumentCheckout::new(id, ResourceType::new(self.kind.kind()));
                let mut doc = automerge::AutoCommit::new();
                collaboration::resource::genesis(&mut doc, &checkout, workspace, initialize)
                    .map_err(|error| error.to_string())?;
                Ok(doc)
            }
        }
    }
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

impl ResourceActions {
    async fn applied(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<ActionOutput, ReceiptStatus> {
        let rejected = |error| ReceiptStatus::Rejected { error };
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
        let arguments = named_arguments(&request.argument).map_err(|status| *status)?;
        // Whoever the workspace does not admit is refused before anything of it is read.
        let policy = self
            .workspaces
            .workspace_policy(workspace_id, caller)
            .await
            .ok_or_else(|| rejected(ActionCallError::NotPermitted))?;
        let person = policy
            .access
            .person_id()
            .ok_or_else(|| rejected(ActionCallError::NotPermitted))?
            .to_owned();
        let member = tilo_actions::MemberId::person(person.clone());
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
        // The action on the record as it stands, once the workspace's grants on its node allow it.
        let change = |doc: &mut automerge::AutoCommit| -> Result<(), Box<ReceiptStatus>> {
            let tree = declared
                .kind
                .publish(declared.kind.kind(), doc)
                .map_err(|error| failed("unreadable", error.to_string()))
                .map_err(Box::new)?;
            let address = &request.target.address;
            let (resource, action) = api::locate(std::slice::from_ref(&tree), address)
                .ok_or_else(|| rejected(ActionCallError::UnknownAction))
                .map_err(Box::new)?;
            match permissions.invocation_answer(
                declared.kind.declaring_app(),
                resource,
                action,
                Some(&arguments),
                None,
            ) {
                collaboration::permissions::Answer::Deny => {
                    return Err(Box::new(rejected(ActionCallError::NotPermitted)));
                }
                collaboration::permissions::Answer::Ask if request.confirmation.is_none() => {
                    return Err(Box::new(rejected(ActionCallError::confirmation_required())));
                }
                _ => {}
            }
            api::invoke_json_confirmed(
                doc,
                std::slice::from_ref(&tree),
                address,
                &arguments,
                InvocationProvenance::caller(&actor).within(&ambient),
                &registry,
                &mut LocalTransport::new(),
            )
            .map(drop)
            .map_err(|error| tilo_agent_session::record_actions::api_status(&error))
            .map_err(Box::new)
        };
        let genesis = declared
            .found(&workspace)
            .map_err(|error| failed("not_founded", error))?;
        // The person's own workspace is written as its every record is: on the copy this device
        // keeps of it, or carried to the device of theirs that keeps it.
        if workspace == person_workspace::workspace_id(&self.workspaces) {
            return self
                .write_person_workspace(&workspace, (app, id), genesis, change)
                .await;
        }
        self.write_record(&workspace, (app, id), genesis, change)
            .await
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
        self.workspaces
            .move_record((workspace, &to), (app, id, &target), caller)
            .await
            .map_err(|error| failed("refused", error))?;
        Ok(ActionOutput::default())
    }

    /// Write the record (`app`, `id`) into the caller's own workspace: on the copy this device
    /// keeps of it, or carried to the device of theirs that keeps it.
    async fn write_person_workspace(
        &self,
        workspace: &str,
        (app, id): (&str, &str),
        genesis: automerge::AutoCommit,
        change: impl Fn(&mut automerge::AutoCommit) -> Result<(), Box<ReceiptStatus>> + Send + Sync,
    ) -> Result<ActionOutput, ReceiptStatus> {
        let device = self.workspaces.device_cert().device_public_key;
        let mut refused = None;
        let written = person_workspace::edit_on(
            &self.workspaces,
            workspace,
            (app, id),
            Box::new(genesis),
            |doc| {
                // This device's own actor: the genesis is one actor's change already, and two
                // devices continuing it under that actor would make one sequence of two
                // different changes.
                doc.set_actor(automerge::ActorId::from(device.to_vec()));
                change(doc).map_err(|status| {
                    refused = Some(*status);
                    "the record's action was refused".to_owned()
                })
            },
        )
        .await;
        match (written, refused) {
            (Ok(()), _) => Ok(ActionOutput::default()),
            (Err(_), Some(status)) => Err(status),
            (Err(error), None) => Err(failed("not_written", error)),
        }
    }

    /// Write the record (`app`, `id`) on the copy this device keeps of it.
    async fn write_record(
        &self,
        workspace: &str,
        (app, id): (&str, &str),
        genesis: automerge::AutoCommit,
        change: impl Fn(&mut automerge::AutoCommit) -> Result<(), Box<ReceiptStatus>> + Send + Sync,
    ) -> Result<ActionOutput, ReceiptStatus> {
        let kept = self
            .workspaces
            .resource_document(workspace, app, id)
            .await
            .map_err(|error| failed("not_read", error.to_string()))?;
        let (mut doc, founding) = match kept {
            Some(bytes) => (
                automerge::AutoCommit::load(&bytes)
                    .map_err(|error| failed("unreadable", error.to_string()))?,
                false,
            ),
            None => (genesis, true),
        };
        let heads = doc.get_heads();
        change(&mut doc).map_err(|status| *status)?;
        let changes = if founding {
            doc.save()
        } else {
            doc.save_after(&heads)
        };
        if !changes.is_empty() {
            self.writes
                .write(
                    workspace,
                    &changes,
                    DocumentWriting {
                        app,
                        document: id,
                        readers: None,
                        founding,
                        snapshot: founding,
                        sealed: None,
                    },
                )
                .await
                .map_err(|refusal| failed("not_written", refusal.to_string()))?;
        }
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
    /// follow.
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
            let status = match Box::pin(actions.applied(&caller, &request)).await {
                Ok(output) => ReceiptStatus::Completed { output },
                Err(status) => status,
            };
            done.complete(Ok(InvocationReceipt {
                invocation: request.invocation,
                status,
            }));
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
