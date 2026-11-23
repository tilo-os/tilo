//! The typed actions of the workspace resources this device keeps for the person's own shell,
//! applied here as each resource's owner declares them.
//!
//! A resource is named as every caller names one: `record:<app>:<id>` in the caller's workspace.
//! Its type is the one its owner declares for it ([`Declared`]): the list of a workspace's
//! agents (Assistant's), its emoji (Chat's), its contact directory (Contacts'), the person's
//! settings and their list of workspaces in the workspace whose only member is them (the
//! settings' and the identity's), and every app's index of
//! the workspace (the index's own). The workspace's grants on the owner's node decide the call,
//! as a window of the app decides a click. What the action changed is written into the
//! workspace as this device's write, signed and sealed as the device, so every app of the
//! workspace and the person's other devices take it as any other.
//!
//! It is registered once on a device's providers ([`register`]) under the resource provider
//! itself, reached by a call made on this device ([`crate::workspaces::providers::Providers::invoke`])
//! and never by another device's.

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
use crate::workspaces::providers::Reach;

/// Apply the typed actions of the resources [`Declared`] names on this device from now on.
pub fn register(workspaces: &Workspaces) {
    workspaces.providers().provide(
        RESOURCE_PROVIDER,
        RESOURCE_PROVIDER,
        Reach::ThisDevice,
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
    /// An app's own resource of the workspace: its index.
    Primary,
    /// The person's own, made from its type alone.
    Typed,
}

impl Declared {
    fn all() -> Vec<Self> {
        use collaboration::resource::index;
        let workspace = |kind: tilo_resource::ResourceKind, app: &'static str| Self {
            app: Some(app),
            id: collab_types::workspace_document_id(kind.kind()),
            kind,
            founding: Founding::Identified,
        };
        vec![
            workspace(
                tilo_resource::ResourceKind::of::<tilo_workspace_agent::WorkspaceAgents>()
                    .hosted_by(tilo_workspace_agent::OWNER, "Assistant"),
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
            Self {
                app: None,
                id: String::new(),
                kind: index::kind(),
                founding: Founding::Primary,
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
            Founding::Identified | Founding::Primary => {
                let id = match self.founding {
                    Founding::Primary => DocumentId::package_primary(),
                    _ => DocumentId::parse(&self.id).ok_or("the resource has no id")?,
                };
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
        let declared = self
            .declared
            .iter()
            .find(|declared| declared.names(app, id))
            .ok_or_else(|| rejected(ActionCallError::UnknownAction))?;
        let workspace = caller
            .workspace()
            .ok_or_else(|| invalid("a resource's action is asked in the workspace that keeps it"))?
            .to_owned();
        let workspace_id =
            WorkspaceId::parse(&workspace).ok_or_else(|| invalid("that is not a workspace"))?;
        let arguments = match &request.argument {
            InvokeArgument::Json(arguments) => arguments.clone(),
            InvokeArgument::None => Map::new(),
            InvokeArgument::Text(_) => {
                return Err(invalid("a resource's action takes named arguments"));
            }
        };
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
        let kept = self
            .workspaces
            .resource_document(&workspace, app, id)
            .await
            .map_err(|error| failed("not_read", error.to_string()))?;
        let (mut doc, founding) = match kept {
            Some(bytes) => (
                automerge::AutoCommit::load(&bytes)
                    .map_err(|error| failed("unreadable", error.to_string()))?,
                false,
            ),
            None => (
                declared
                    .found(&workspace)
                    .map_err(|error| failed("not_founded", error))?,
                true,
            ),
        };
        let heads = doc.get_heads();
        let tree = declared
            .kind
            .publish(declared.kind.kind(), &doc)
            .map_err(|error| failed("unreadable", error.to_string()))?;
        let address = &request.target.address;
        let (resource, action) = api::locate(std::slice::from_ref(&tree), address)
            .ok_or_else(|| rejected(ActionCallError::UnknownAction))?;
        let member = tilo_actions::MemberId::person(person.clone());
        let permissions = collaboration::permissions::PermissionContext::new(
            collaboration::permissions::Acting::person(person),
            policy.sandbox.clone(),
            policy.access.clone(),
        )
        .at_unix_time(policy.unix_time);
        match permissions.invocation_answer(
            declared.kind.declaring_app(),
            resource,
            action,
            Some(&arguments),
            None,
        ) {
            collaboration::permissions::Answer::Deny => {
                return Err(rejected(ActionCallError::NotPermitted));
            }
            collaboration::permissions::Answer::Ask if request.confirmation.is_none() => {
                return Err(rejected(ActionCallError::confirmation_required()));
            }
            _ => {}
        }
        // Who asked: the person, or whoever the shell says acts for them (an agent it admitted),
        // which the change's history names.
        let mut ambient = request.context.clone().unwrap_or_default();
        if ambient.invoker.is_none() {
            ambient = ambient.with_invoker(tilo_actions::ActionInvoker::member(member.clone()));
        }
        let actor = member.actor_bytes();
        let mut registry = tilo_actions::ActionRegistry::new();
        declared.kind.register(&mut registry);
        api::invoke_json_confirmed(
            &mut doc,
            std::slice::from_ref(&tree),
            address,
            &arguments,
            InvocationProvenance::caller(&actor).within(&ambient),
            &registry,
            &mut LocalTransport::new(),
        )
        .map_err(|error| tilo_agent_session::record_actions::api_status(&error))?;
        let changes = if founding {
            doc.save()
        } else {
            doc.save_after(&heads)
        };
        if !changes.is_empty() {
            self.writes
                .write(
                    &workspace,
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
            let status = match actions.applied(&caller, &request).await {
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
