//! A device's actions in one workspace ([`DeviceActions`]): the changes to who is in it, which a
//! device of its person's runs and signs as its own write, and everything else, asked of the
//! workspace's host.

use serde_json::{Map, Value};
use tilo_actions::remote::{
    AWAITING_APPROVAL, ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery,
    ConfirmedInvocation, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest,
    Pending, ReceiptStatus, RemoteActionTarget, WORKSPACE_MEMBER_ADMIT, WORKSPACE_MEMBER_ADMITTED,
    WORKSPACE_MEMBER_PLACED, WORKSPACE_MEMBER_REMOVED, WatchScope, pending,
    workspace_governance_operation, workspace_person,
};
use tilo_actions::{ActionOutput, InvocationId};
use tilo_server_protocol::WorkspaceId;
use tilo_session_actions::SessionActions;

use super::{Device, DeviceRefusal};
use crate::membership::{MemberPlace, Membership};

/// The governance operations that change who is in a workspace: holding a person at a place
/// (`workspace/members#admit`, `workspace/members/<person>#role`) and removing one
/// (`workspace/members/<person>#remove`).
const ADD: &str = "member.add";
const REMOVE: &str = "remove-member";
/// The people listed, as each change answers them.
const MEMBERS: &str = "member.list";

/// A device's actions in one workspace: the changes to who is in it, run here and signed by this
/// device as its person's write when it is a device of theirs, and every other action asked of
/// the workspace's host.
#[derive(Clone)]
pub struct DeviceActions {
    device: Device,
    workspace_id: WorkspaceId,
    host: SessionActions,
}

/// A change asked of who is in the workspace: the action the tree publishes it at, its
/// arguments, and the change.
type Asked = (&'static str, Map<String, Value>, Change);

/// A change to who is in the workspace.
enum Change {
    /// The person, the place, and the name the workspace calls them by.
    Place([u8; 32], MemberPlace, Option<String>),
    Remove([u8; 32]),
}

impl Device {
    /// This device's actions in `workspace_id` ([`DeviceActions`]).
    #[must_use]
    pub fn workspace_actions(&self, workspace_id: WorkspaceId) -> DeviceActions {
        DeviceActions {
            device: self.clone(),
            workspace_id,
            host: self.actions(workspace_id),
        }
    }
}

impl DeviceActions {
    /// The change `request` asks of who is in this workspace, its operation and its arguments,
    /// when it asks one this device signs itself: `None` for any other request, and for a key
    /// made to act for the person (an agent's grant, a stand-in, a server key), whose changes the
    /// host makes as far as its own authority and the key's grants go.
    fn change_of(&self, request: &InvokeRequest) -> Option<Result<Asked, ActionCallError>> {
        let capabilities = &self.device.cert.capabilities;
        if capabilities.agent.is_some() || capabilities.stand_in || capabilities.server_key {
            return None;
        }
        let address = &request.target.address;
        let (workspace, operation) = workspace_governance_operation(address)?;
        let operation = [ADD, REMOVE].into_iter().find(|op| *op == operation)?;
        if workspace != self.workspace_id.to_string() {
            return None;
        }
        // The action the tree publishes it at: `admit` and `role` are both `member.add`, and the
        // host decides a role only for a member.
        let published = match address.action.as_str() {
            WORKSPACE_MEMBER_ADMITTED => WORKSPACE_MEMBER_ADMITTED,
            WORKSPACE_MEMBER_PLACED => WORKSPACE_MEMBER_PLACED,
            WORKSPACE_MEMBER_REMOVED => WORKSPACE_MEMBER_REMOVED,
            _ => return None,
        };
        let mut arguments = match &request.argument {
            InvokeArgument::Json(arguments) => arguments.clone(),
            _ => Map::new(),
        };
        if let Some(person) = workspace_person(address) {
            arguments.insert("person".into(), person.into());
        }
        Some(change(operation, &arguments).map(|change| (published, arguments, change)))
    }
}

/// The change `operation` with `arguments` makes, or why its arguments name none.
fn change(operation: &str, arguments: &Map<String, Value>) -> Result<Change, ActionCallError> {
    let invalid = |reason: &str| ActionCallError::InvalidRequest {
        reason: reason.to_owned(),
    };
    let text = |name: &str| arguments.get(name).and_then(Value::as_str).map(str::trim);
    let person = text("person")
        .and_then(|person| hex::decode(person).ok())
        .and_then(|key| <[u8; 32]>::try_from(key).ok())
        .ok_or_else(|| invalid("`person` is the person's user public key: 64 hex characters"))?;
    if operation == REMOVE {
        return Ok(Change::Remove(person));
    }
    let place = text("role").and_then(MemberPlace::parse).ok_or_else(|| {
        invalid("`role` is viewer, commenter, editor, moderator, administrator or owner")
    })?;
    // A name that is no display name is refused by the workspace, as it refuses the change.
    let name = text("name").filter(|name| !name.is_empty());
    Ok(Change::Place(person, place, name.map(str::to_owned)))
}

/// Make `change` (`published`, the action asked with `arguments`) in `workspace_id` as `device`'s
/// own write, once the host admits it: the people as the workspace holds them then.
async fn make(
    device: Device,
    workspace_id: WorkspaceId,
    published: &str,
    mut arguments: Map<String, Value>,
    change: Change,
) -> Result<Value, ActionCallError> {
    let workspace = workspace_id.to_string();
    let refused = |why: String| ActionCallError::Refused {
        reason: format!("the workspace refused: {why}"),
    };
    // Whether the change is this device's to make: by its person's place in the workspace and,
    // for a session that is an agent's run, by that agent's grants there, which ask the person
    // first where they say so. Asked of the host for now: the device will admit the call itself,
    // through `admit_invocation` over its synced grants, asking the person through the Confirm
    // frame, and that replaces this round trip.
    arguments.insert("change".into(), published.into());
    let admission = RemoteActionTarget::workspace_governance(&workspace, WORKSPACE_MEMBER_ADMIT);
    // Its own invocation, not the change's: what the host answers it is the admission alone.
    let invocation = InvocationId::new();
    let mut asked_again = false;
    loop {
        match device
            .ask_server_as(
                workspace_id,
                admission.clone(),
                arguments.clone(),
                invocation,
            )
            .await
        {
            Ok(_) => break,
            // The host answers at once that a question waits on the person, and the same
            // invocation asked again is followed until they answer.
            Err(error) if error.code == DeviceRefusal::AWAITING_APPROVAL && !asked_again => {
                asked_again = true;
            }
            Err(error) if error.code == DeviceRefusal::AWAITING_APPROVAL => {
                return Err(ActionCallError::Refused {
                    reason: format!("{AWAITING_APPROVAL}: {}", error.message),
                });
            }
            Err(error) if error.code == DeviceRefusal::UNANSWERED => {
                return Err(ActionCallError::Unavailable {
                    reason: error.message,
                });
            }
            Err(error) => return Err(refused(error.message)),
        }
    }
    if !device.admitted(workspace_id).await {
        return Err(ActionCallError::Unavailable {
            reason: "the workspace has not let this device in yet".to_owned(),
        });
    }
    let link = device.session();
    let membership = Membership::of(link.engine(), workspace_id);
    match &change {
        Change::Place(person, place, name) => {
            membership.place(*person, *place, name.as_deref()).await
        }
        Change::Remove(person) => membership.remove(*person).await,
    }
    .map_err(refused)?;
    membership.settled(link).await.map_err(refused)?;
    device
        .ask_host(
            workspace_id,
            RemoteActionTarget::workspace_governance(&workspace, MEMBERS),
            Map::new(),
        )
        .await
        .map_err(|error| ActionCallError::Unavailable {
            reason: error.message,
        })
}

impl ActionProvider for DeviceActions {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        self.host.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let (published, arguments, change) = match self.change_of(&request) {
            None => return self.host.invoke(caller, request),
            Some(Err(error)) => return Pending::ready(Err(error)),
            Some(Ok(asked)) => asked,
        };
        let (answer, answered) = pending();
        let (device, workspace_id) = (self.device.clone(), self.workspace_id);
        let invocation = request.invocation;
        n0_future::task::spawn(async move {
            let made = make(device, workspace_id, published, arguments, change).await;
            answer.complete(made.map(|value| InvocationReceipt {
                invocation,
                status: ReceiptStatus::Completed {
                    output: ActionOutput {
                        value: Some(value),
                        resources: Vec::new(),
                    },
                },
            }));
        });
        answered
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.host.observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.host.next_change(caller, id)
    }

    fn session_state(&self, caller: &Caller) -> Option<tilo_actions::remote::SessionState> {
        self.host.session_state(caller)
    }

    fn session_state_changed(&self, caller: &Caller) -> Option<Pending<()>> {
        self.host.session_state_changed(caller)
    }

    fn changed(
        &self,
        caller: &Caller,
        scope: WatchScope,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        self.host.changed(caller, scope)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        self.host.cancel(caller, id)
    }

    fn confirm(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<ConfirmedInvocation, ActionCallError>> {
        self.host.confirm(caller, request)
    }

    fn content_puts(&self) -> Option<std::sync::Arc<dyn tilo_actions::remote::ContentPuts>> {
        self.host.content_puts()
    }

    fn content_gets(&self) -> Option<std::sync::Arc<dyn tilo_actions::remote::ContentGets>> {
        self.host.content_gets()
    }

    fn here(&self, caller: &Caller) -> Option<Vec<tilo_actions::remote::Here>> {
        self.host.here(caller)
    }

    fn resource_search(&self) -> Option<std::sync::Arc<dyn tilo_actions::remote::ResourceSearch>> {
        self.host.resource_search()
    }

    fn change_feed(&self) -> Option<std::sync::Arc<dyn tilo_actions::remote::ChangeFeed>> {
        self.host.change_feed()
    }

    fn installed_surface(&self, app_id: &str) -> Pending<Option<Vec<u8>>> {
        self.host.installed_surface(app_id)
    }
}
