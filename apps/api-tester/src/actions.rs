//! API Tester's exported **actions** and the **resource tree** it publishes to
//! Ctrl+K.
//!
//! Every document-mutating operation the API tester performs on its shared
//! workspace is an [`ApiTesterAction`]: a small serializable value that
//! deterministically mutates the CRDT. The workspace is persisted as keyed
//! records (see `crate::doc`), so an action reads the current projection,
//! mutates exactly the record(s) it owns, and writes the result back: a change
//! that replicates and replays for free. The exact same [`EncodedAction`] is
//! what the visible builder/rail affordances and the Ctrl+K palette dispatch —
//! one source of truth, one replay path.
//!
//! ## What is (and is not) an action here
//!
//! The migrated collaborative doc mutations are the **saved requests** and the
//! **collections** that hold them, the **environments** and their **variables**,
//! and the **selected environment**. Local presentation stays out of the
//! document action stream, while external effects are exported explicitly as
//! resource operations:
//!
//! - **Presentation** (per-peer, out of the document): which saved request is
//!   loaded into the builder, the active request-builder tab, the response
//!   view/scroll, the sidebar fold state, the rail overlay. The URL and body
//!   editor buffers are live Automerge `Text` objects edited in shared
//!   [`TextInput`](ui::widget::TextInput)s — character-level collaboration, not
//!   a discrete action. A whole-request **save** into a collection *is* an
//!   action.
//! - **External** (non-deterministic, off the document wire): actually *sending*
//!   an HTTP request is an [`OperationKind::Effect`] owned by this app. It is
//!   addressable through the same resource/action surface as the Send button,
//!   but its response is deliberately local and is never replayed as a CRDT edit.
//!
//! ## Determinism contract
//!
//! Ids (a collection's, a request's, an environment's) are minted at *authoring*
//! time and carried in the payload — a whole-record write, never a
//! read-and-mint. [`Action::apply`] never touches the RNG or the wall-clock, and
//! a payload that names no existing row (or an unparseable id) is a **no-op**
//! rather than an error, so a bad replay can't wedge a peer. Iteration is stable:
//! creating a row appends, editing one replaces it in place.

use automerge::{AutoCommit, ObjId, ROOT};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use tilo_actions::{
    Action, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, ExecutionMode, OperationKind, ParamType, Resource, ResultSpec,
};

use crate::doc::ApiDoc;
use crate::model::{Collection, Environment, HistoryEntry, KeyValue, Request};

/// Registry `type_id` for every API Tester action.
pub const API_TESTER_ACTION_TYPE: &str = "api_tester.action";

/// Operation type for API Tester's non-document resource operations.
pub const API_TESTER_OPERATION_TYPE: &str = "api_tester.operation";

/// Effects which are meaningful on an API Tester resource but must never be
/// replayed as document mutations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ApiTesterOperation {
    /// Send the current builder buffer. This is a device-local network effect,
    /// deliberately separate from saving a durable request resource.
    SendActiveRequest,
    SendRequest {
        collection_id: String,
        request_id: String,
    },
    /// Open a durable history record in this peer's request builder. This is
    /// deliberately a View operation: it is navigation, not a fabricated
    /// successful HTTP run and not a collaborative mutation.
    ReplayHistory { history_id: String },
    /// Open one of the request builder's visible contextual workflows.
    RequestMenu { action: String },
    /// Open one of the active environment's visible contextual workflows.
    EnvironmentMenu { action: String },
}

pub const REQUEST_MENU_CHOICES: &[&str] = &["Save request…", "Rename request…", "Delete request…"];
pub const ENVIRONMENT_MENU_CHOICES: &[&str] =
    &["Rename environment…", "Add variable…", "Delete variable…"];

#[allow(
    clippy::expect_used,
    reason = "menu payloads contain only strings, whose JSON serialization is infallible"
)]
fn menu_resource(
    id: &str,
    label: &str,
    action_id: &str,
    choices: &'static [&'static str],
    operation: fn(String) -> ApiTesterOperation,
) -> Resource {
    let spec = ActionSpec::typed_operation(
        action_id,
        label,
        "Choose an action",
        OperationKind::View,
        API_TESTER_OPERATION_TYPE,
        vec![
            ActionParam::required_owned("action", ParamType::Text, label.to_string())
                .with_choices(choices.iter().copied()),
        ],
        move |args| {
            EncodedAction::encode_operation(
                API_TESTER_OPERATION_TYPE,
                &operation(args.text("action").unwrap_or_default().to_string()),
            )
            .expect("API Tester menu operations contain only serializable strings")
        },
    );
    Resource::new(id, id, label).with_action(spec)
}

pub fn request_menu_resource() -> Resource {
    menu_resource(
        "api_tester.request_menu",
        "Request actions",
        "open",
        REQUEST_MENU_CHOICES,
        |action| ApiTesterOperation::RequestMenu { action },
    )
}

pub fn environment_menu_resource() -> Resource {
    menu_resource(
        "api_tester.environment_menu",
        "Environment actions",
        "open",
        ENVIRONMENT_MENU_CHOICES,
        |action| ApiTesterOperation::EnvironmentMenu { action },
    )
}

/// The builder's current request is an addressable local-effect resource so
/// Send, empty-state recovery, and assistive activation share one operation.
pub fn active_request_resource() -> Resource {
    let mut resource = Resource::new(
        "api_tester.active_request",
        "api_tester.active_request",
        "Current request",
    );
    if let Ok(action) = ActionSpec::operation(
        "send",
        "Send request",
        OperationKind::Effect,
        API_TESTER_OPERATION_TYPE,
        &ApiTesterOperation::SendActiveRequest,
    ) {
        resource = resource.with_action(
            action
                .with_description("Send the current request using this device's active network")
                .execution(ExecutionMode::Background { cancellable: false })
                .returns(ResultSpec::Job),
        );
    }
    resource
}

/// Build the external operation for sending one saved request.
///
/// # Errors
///
/// Returns [`ActionError`] if the operation payload cannot be encoded. Callers
/// must then omit the action rather than publish an operation they cannot run.
pub fn send_request_operation(
    collection_id: Uuid,
    request_id: Uuid,
) -> Result<ActionSpec, ActionError> {
    ActionSpec::operation(
        "send",
        "Send request",
        OperationKind::Effect,
        API_TESTER_OPERATION_TYPE,
        &ApiTesterOperation::SendRequest {
            collection_id: collection_id.to_string(),
            request_id: request_id.to_string(),
        },
    )
    .map(|spec| {
        spec.with_description("Queue this saved request using the active environment")
            .execution(ExecutionMode::Background { cancellable: false })
            .returns(ResultSpec::Job)
    })
}

/// Build the device-local navigation operation for one durable history row.
///
/// # Errors
///
/// Returns an error only if the serializable operation payload cannot be
/// encoded for the resource-action protocol.
pub fn replay_history_operation(history_id: Uuid) -> Result<ActionSpec, ActionError> {
    ActionSpec::operation(
        "replay",
        "Open request in builder",
        OperationKind::View,
        API_TESTER_OPERATION_TYPE,
        &ApiTesterOperation::ReplayHistory {
            history_id: history_id.to_string(),
        },
    )
    .map(|spec| spec.with_description("Load this historical request without sending it"))
}

/// Every collaborative operation the API tester exports on its shared workspace.
/// Each variant names only its durable target and is replayed against the
/// document's keyed projection; it never serializes presentation state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ApiTesterAction {
    /// Record a completed request without replaying its network effect.
    RecordHistory { entry: HistoryEntry },
    // --- Collections ---------------------------------------------------------
    /// Add a new, empty collection. `id` is minted at authoring time; a replay
    /// that finds the id already present is a no-op, so re-applying can't stack a
    /// duplicate.
    CreateCollection { id: String, name: String },
    /// Rename a collection by id.
    RenameCollection { id: String, name: String },
    /// Delete a collection (and every saved request it holds) by id.
    DeleteCollection { id: String },

    // --- Saved requests (within a collection) --------------------------------
    /// Insert or replace a saved request in a collection. Keyed by the request's
    /// own id: an id already in the collection is replaced *in place* (edit), a
    /// new id is appended (create / "save the current request here"). The whole
    /// `Request` — method, url, params, headers, auth, body — is carried, so
    /// this one action covers "save a request" and "set method/url/headers/body/
    /// params" as a single deterministic write.
    SaveRequest {
        collection_id: String,
        request: Request,
    },
    /// Save the live shared request into a collection without copying its
    /// URL, body, auth, or headers into a resource directory entry.
    SaveCurrentRequest {
        collection_id: String,
        request_id: String,
    },
    /// Rename a saved request by id within its collection.
    RenameRequest {
        collection_id: String,
        request_id: String,
        name: String,
    },
    /// Delete a saved request by id from its collection.
    DeleteRequest {
        collection_id: String,
        request_id: String,
    },

    // --- Environments --------------------------------------------------------
    /// Add a new, empty environment. `id` minted at authoring time; an existing
    /// id is a no-op.
    CreateEnvironment { id: String, name: String },
    /// Rename an environment by id.
    RenameEnvironment { id: String, name: String },
    /// Delete an environment by id.
    DeleteEnvironment { id: String },
    /// Select the active environment (whose `{{variables}}` resolve into
    /// outgoing requests), or clear it with `None`. A whole-value write on the
    /// shared `active_env` scalar.
    SetActiveEnvironment { id: Option<String> },

    // --- Variables (within an environment) -----------------------------------
    /// Add or update a `{{variable}}` in an environment, keyed by name: an
    /// existing key has its value replaced in place, a new key is appended.
    SetVariable {
        env_id: String,
        /// Stable row identity. A rename changes `key`, never this address.
        variable_id: String,
        key: String,
        value: String,
    },
    /// Rename a variable without reading or serializing its secret value.
    RenameVariable {
        env_id: String,
        variable_id: String,
        key: String,
    },
    /// Delete a variable by key from an environment.
    DeleteVariable { env_id: String, variable_id: String },
}

impl ApiTesterAction {
    /// Compatibility-only pure projection helper used by import tests. Normal
    /// action execution writes the addressed native record directly.
    pub fn apply_to_collections(&self, cols: &mut Vec<Collection>) {
        match self {
            ApiTesterAction::CreateCollection { id, name } => {
                let Ok(uuid) = id.parse::<Uuid>() else { return };
                if cols.iter().any(|c| c.id == uuid) {
                    return; // idempotent: a replayed create never stacks a dup.
                }
                let mut c = Collection::new(name.clone());
                c.id = uuid;
                cols.push(c);
            }
            ApiTesterAction::RenameCollection { id, name } => {
                if let Some(c) = find_collection_mut(cols, id) {
                    c.name.clone_from(name);
                }
            }
            ApiTesterAction::DeleteCollection { id } => {
                cols.retain(|c| c.id.to_string() != *id);
            }
            ApiTesterAction::SaveRequest {
                collection_id,
                request,
            } => {
                if let Some(c) = find_collection_mut(cols, collection_id) {
                    if let Some(existing) = c.requests.iter_mut().find(|r| r.id == request.id) {
                        *existing = request.clone(); // edit in place
                    } else {
                        c.requests.push(request.clone()); // create
                    }
                }
            }
            ApiTesterAction::RenameRequest {
                collection_id,
                request_id,
                name,
            } => {
                if let Some(c) = find_collection_mut(cols, collection_id)
                    && let Some(r) = c
                        .requests
                        .iter_mut()
                        .find(|r| r.id.to_string() == *request_id)
                {
                    r.name.clone_from(name);
                }
            }
            ApiTesterAction::DeleteRequest {
                collection_id,
                request_id,
            } => {
                if let Some(c) = find_collection_mut(cols, collection_id) {
                    c.requests.retain(|r| r.id.to_string() != *request_id);
                }
            }
            // Environment / active-env variants don't touch the collection list.
            _ => {}
        }
    }

    /// The pure mutation on the environment list. Deterministic: no RNG, no
    /// wall-clock, stable iteration.
    pub fn apply_to_environments(&self, envs: &mut Vec<Environment>) {
        match self {
            ApiTesterAction::CreateEnvironment { id, name } => {
                let Ok(uuid) = id.parse::<Uuid>() else { return };
                if envs.iter().any(|e| e.id == uuid) {
                    return;
                }
                let mut e = Environment::new(name.clone());
                e.id = uuid;
                envs.push(e);
            }
            ApiTesterAction::RenameEnvironment { id, name } => {
                if let Some(e) = find_environment_mut(envs, id) {
                    e.name.clone_from(name);
                }
            }
            ApiTesterAction::DeleteEnvironment { id } => {
                envs.retain(|e| e.id.to_string() != *id);
            }
            ApiTesterAction::SetVariable {
                env_id,
                variable_id,
                key,
                value,
            } => {
                if let Some(e) = find_environment_mut(envs, env_id) {
                    if let Some(v) = e.variables.iter_mut().find(|v| v.id == *variable_id) {
                        v.key.clone_from(key);
                        v.value.clone_from(value);
                    } else {
                        let mut variable = KeyValue::new(key.clone(), value.clone());
                        variable.id.clone_from(variable_id);
                        e.variables.push(variable);
                    }
                }
            }
            ApiTesterAction::RenameVariable {
                env_id,
                variable_id,
                key,
            } => {
                if let Some(e) = find_environment_mut(envs, env_id)
                    && let Some(v) = e.variables.iter_mut().find(|v| v.id == *variable_id)
                {
                    v.key.clone_from(key);
                }
            }
            ApiTesterAction::DeleteVariable {
                env_id,
                variable_id,
            } => {
                if let Some(e) = find_environment_mut(envs, env_id) {
                    e.variables.retain(|v| v.id != *variable_id);
                }
            }
            _ => {}
        }
    }
}

fn find_collection_mut<'a>(cols: &'a mut [Collection], id: &str) -> Option<&'a mut Collection> {
    cols.iter_mut().find(|c| c.id.to_string() == *id)
}

fn find_environment_mut<'a>(envs: &'a mut [Environment], id: &str) -> Option<&'a mut Environment> {
    envs.iter_mut().find(|e| e.id.to_string() == *id)
}

impl Action for ApiTesterAction {
    #[allow(
        clippy::too_many_lines,
        reason = "the exhaustive stable-action dispatch keeps every durable write auditable in one place"
    )]
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let root: ObjId = ROOT;
        match self {
            ApiTesterAction::RecordHistory { entry } => {
                let mut history = ApiDoc::history(doc, &root);
                if !history.iter().any(|existing| existing.id == entry.id) {
                    history.insert(0, entry.clone());
                    history.truncate(50);
                    ApiDoc::set_history(doc, &root, &history);
                }
                Ok(())
            }
            ApiTesterAction::CreateCollection { id, name } => {
                ApiDoc::create_collection(doc, &root, id, name);
                Ok(())
            }
            ApiTesterAction::RenameCollection { id, name } => {
                ApiDoc::rename_collection(doc, &root, id, name);
                Ok(())
            }
            ApiTesterAction::DeleteCollection { id } => {
                ApiDoc::delete_collection(doc, &root, id);
                Ok(())
            }
            ApiTesterAction::CreateEnvironment { id, name } => {
                ApiDoc::create_environment(doc, &root, id, name);
                Ok(())
            }
            ApiTesterAction::RenameEnvironment { id, name } => {
                ApiDoc::rename_environment(doc, &root, id, name);
                Ok(())
            }
            ApiTesterAction::DeleteEnvironment { id } => {
                ApiDoc::delete_environment(doc, &root, id);
                Ok(())
            }
            ApiTesterAction::SaveRequest {
                collection_id,
                request,
            } => {
                let before = ApiDoc::collections(doc, &root)
                    .iter()
                    .find(|collection| collection.id.to_string() == *collection_id)
                    .and_then(|collection| {
                        collection
                            .requests
                            .iter()
                            .find(|item| item.id == request.id)
                    })
                    .cloned();
                ApiDoc::save_request(doc, &root, collection_id, before.as_ref(), request);
                Ok(())
            }
            ApiTesterAction::SaveCurrentRequest {
                collection_id,
                request_id,
            } => {
                let Ok(request_id) = request_id.parse::<Uuid>() else {
                    return Ok(());
                };
                let Some(mut request) = ApiDoc::request(doc, &root) else {
                    return Ok(());
                };
                request.id = request_id;
                ApiDoc::save_request(doc, &root, collection_id, None, &request);
                Ok(())
            }
            ApiTesterAction::RenameRequest {
                collection_id,
                request_id,
                name,
            } => {
                let Ok(request_id) = request_id.parse::<Uuid>() else {
                    return Ok(());
                };
                let Some(mut request) = ApiDoc::collections(doc, &root)
                    .iter()
                    .find(|collection| collection.id.to_string() == *collection_id)
                    .and_then(|collection| {
                        collection
                            .requests
                            .iter()
                            .find(|item| item.id == request_id)
                    })
                    .cloned()
                else {
                    return Ok(());
                };
                request.name.clone_from(name);
                ApiDoc::save_request(doc, &root, collection_id, None, &request);
                Ok(())
            }
            ApiTesterAction::DeleteRequest { request_id, .. } => {
                ApiDoc::delete_request(doc, &root, request_id);
                Ok(())
            }
            ApiTesterAction::SetVariable {
                env_id,
                variable_id,
                key,
                value,
            } => {
                ApiDoc::set_variable(doc, &root, env_id, variable_id, key, value);
                Ok(())
            }
            ApiTesterAction::RenameVariable {
                env_id,
                variable_id,
                key,
            } => {
                ApiDoc::rename_variable(doc, &root, env_id, variable_id, key);
                Ok(())
            }
            ApiTesterAction::DeleteVariable {
                env_id,
                variable_id,
            } => {
                ApiDoc::delete_variable(doc, &root, env_id, variable_id);
                Ok(())
            }
            ApiTesterAction::SetActiveEnvironment { id } => {
                ApiDoc::set_active_env(doc, &root, id.as_deref());
                Ok(())
            }
        }
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Register API Tester's action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<ApiTesterAction>(API_TESTER_ACTION_TYPE);
}

/// Encode any [`ApiTesterAction`] into the wire form the palette and the
/// on-screen affordances both dispatch. The single point that turns an action
/// into a replayable envelope.
///
/// # Panics
///
/// Never panics: an `ApiTesterAction` that fails to serialize (it never
/// should — every field is a plain string or a nested plain-field struct)
/// falls back to an empty-payload envelope rather than crashing the caller.
pub fn encode(action: &ApiTesterAction) -> EncodedAction {
    EncodedAction::encode(API_TESTER_ACTION_TYPE, action).unwrap_or_else(|_| EncodedAction {
        type_id: API_TESTER_ACTION_TYPE.to_string(),
        payload: Vec::new(),
    })
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::app`) and the Ctrl+K resource tree below.
// ===========================================================================

pub fn set_active_environment_action(id: Option<Uuid>) -> ApiTesterAction {
    ApiTesterAction::SetActiveEnvironment {
        id: id.map(|i| i.to_string()),
    }
}

pub fn create_collection_action(id: Uuid, name: &str) -> ApiTesterAction {
    ApiTesterAction::CreateCollection {
        id: id.to_string(),
        name: name.to_string(),
    }
}

pub fn rename_collection_action(id: Uuid, name: &str) -> ApiTesterAction {
    ApiTesterAction::RenameCollection {
        id: id.to_string(),
        name: name.to_string(),
    }
}

pub fn delete_collection_action(id: Uuid) -> ApiTesterAction {
    ApiTesterAction::DeleteCollection { id: id.to_string() }
}

pub fn save_request_action(collection_id: Uuid, request: Request) -> ApiTesterAction {
    ApiTesterAction::SaveRequest {
        collection_id: collection_id.to_string(),
        request,
    }
}

pub fn save_current_request_action(collection_id: Uuid, request_id: Uuid) -> ApiTesterAction {
    ApiTesterAction::SaveCurrentRequest {
        collection_id: collection_id.to_string(),
        request_id: request_id.to_string(),
    }
}

pub fn rename_request_action(collection_id: Uuid, request_id: Uuid, name: &str) -> ApiTesterAction {
    ApiTesterAction::RenameRequest {
        collection_id: collection_id.to_string(),
        request_id: request_id.to_string(),
        name: name.to_string(),
    }
}

pub fn delete_request_action(collection_id: Uuid, request_id: Uuid) -> ApiTesterAction {
    ApiTesterAction::DeleteRequest {
        collection_id: collection_id.to_string(),
        request_id: request_id.to_string(),
    }
}

pub fn create_environment_action(id: Uuid, name: &str) -> ApiTesterAction {
    ApiTesterAction::CreateEnvironment {
        id: id.to_string(),
        name: name.to_string(),
    }
}

pub fn rename_environment_action(id: Uuid, name: &str) -> ApiTesterAction {
    ApiTesterAction::RenameEnvironment {
        id: id.to_string(),
        name: name.to_string(),
    }
}

pub fn delete_environment_action(id: Uuid) -> ApiTesterAction {
    ApiTesterAction::DeleteEnvironment { id: id.to_string() }
}

pub fn set_variable_action(
    env_id: Uuid,
    variable_id: impl Into<String>,
    key: &str,
    value: &str,
) -> ApiTesterAction {
    ApiTesterAction::SetVariable {
        env_id: env_id.to_string(),
        variable_id: variable_id.into(),
        key: key.to_string(),
        value: value.to_string(),
    }
}

pub fn rename_variable_action(
    env_id: Uuid,
    variable_id: impl Into<String>,
    key: &str,
) -> ApiTesterAction {
    ApiTesterAction::RenameVariable {
        env_id: env_id.to_string(),
        variable_id: variable_id.into(),
        key: key.to_string(),
    }
}

pub fn delete_variable_action(env_id: Uuid, variable_id: impl Into<String>) -> ApiTesterAction {
    ApiTesterAction::DeleteVariable {
        env_id: env_id.to_string(),
        variable_id: variable_id.into(),
    }
}

// ===========================================================================
// Resource tree
// ===========================================================================

pub(crate) fn immediate(
    id: &str,
    label: impl Into<String>,
    action: &ApiTesterAction,
) -> ActionSpec {
    let label = label.into();
    ActionSpec::immediate(id, label.clone(), API_TESTER_ACTION_TYPE, action).unwrap_or_else(|_| {
        // The fallible builder's fallback. `ActionSpec`'s fields are
        // private now that it carries typed parameters, so construct it
        // through the constructor that takes an already-encoded action.
        ActionSpec::immediate_encoded(id, label, action.locus(), encode(action))
    })
}

/// Prompted workspace creates for [`tilo_resource::publish`]: ids are minted
/// when the name is committed, then baked into the envelope so replay stays
/// deterministic.
pub(crate) fn workspace_bound_actions() -> Vec<ActionSpec> {
    vec![
        ActionSpec::prompted(
            "new-collection",
            "New collection",
            "Collection name",
            "e.g. Users API",
            ExecutionLocus::ClientLocal,
            move |name: &str| encode(&create_collection_action(Uuid::new_v4(), name.trim())),
        ),
        ActionSpec::prompted(
            "new-environment",
            "New environment",
            "Environment name",
            "e.g. Production",
            ExecutionLocus::ClientLocal,
            move |name: &str| encode(&create_environment_action(Uuid::new_v4(), name.trim())),
        ),
    ]
}

/// Static workspace children for [`tilo_resource::publish`]: the active
/// request Send Effect, contextual menus, and the history directory. Deep
/// collection / request / environment records resolve by stable id.
pub(crate) fn document_children() -> Vec<Resource> {
    vec![
        active_request_resource(),
        request_menu_resource(),
        environment_menu_resource(),
        history_root(),
    ]
}

fn history_root() -> Resource {
    Resource::new(
        "api_tester.history",
        "api_tester.history",
        "Request history",
    )
}

/// Standalone / `--shots` / unit-test tree: [`tilo_resource::publish`] owns the
/// workspace door. Deep records arrive through search or an exact stable-id
/// resolver, preventing palette opens from allocating every request and
/// variable action closure.
pub fn provide_resources(state: &crate::resource::WorkspaceState) -> Vec<Resource> {
    vec![ui::tilo_resource::publish::<crate::resource::ApiTester>(
        "api_tester.workspace",
        "API workspace",
        state,
    )]
}

/// Hosted-only extras that are not document state: a failed search-catalog
/// refresh surfaces as a palette diagnostic. Response bodies and in-flight
/// HTTP work stay widget-local and are never published.
pub(crate) fn live_search_status_resources(error: Option<String>) -> Vec<Resource> {
    match error {
        Some(message) => vec![Resource::new(
            "api_tester.search_status",
            "api_tester.search_status",
            format!("Search index unavailable: {message}"),
        )],
        None => Vec::new(),
    }
}

/// One stable, secret-safe variable resource. Kept separate so environment
/// traversal remains small and callers can materialize a single address later.
pub(crate) fn variable_resource(eid: Uuid, variable_id: &str, key: &str) -> Resource {
    let rename_id = variable_id.to_string();
    let set_value_id = variable_id.to_string();
    let key = key.to_string();
    Resource::new(
        format!("api_tester.variable:{eid}:{variable_id}"),
        "api_tester.variable",
        format!("Variable {key}"),
    )
    .with_action(ActionSpec::prompted(
        "rename",
        "Rename variable",
        "Variable key",
        "new key…",
        ExecutionLocus::ClientLocal,
        move |next_key: &str| {
            encode(&rename_variable_action(
                eid,
                rename_id.clone(),
                next_key.trim(),
            ))
        },
    ))
    .with_action(ActionSpec::prompted(
        "set-value",
        "Set variable value",
        "Variable value",
        "new value…",
        ExecutionLocus::ClientLocal,
        move |next_value: &str| {
            encode(&set_variable_action(
                eid,
                set_value_id.clone(),
                &key,
                next_value,
            ))
        },
    ))
    .with_action(immediate(
        "delete",
        "Delete variable",
        &delete_variable_action(eid, variable_id),
    ))
}

// Inherent resource publication (was ResourceProvider)
impl crate::app::ApiTesterApp {
    /// Standalone / `--shots` / unit-test tree via [`tilo_resource::publish`].
    pub fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        let state = <crate::resource::ApiTester as ui::tilo_resource::Resource>::read(doc)
            .unwrap_or_default();
        let mut roots = provide_resources(&state);
        if let Some(error) = self.search_error_message()
            && let Some(root) = roots.first_mut()
        {
            root.children.push(Resource::new(
                "api_tester.search_status",
                "api_tester.search_status",
                format!("Search index unavailable: {error}"),
            ));
        }
        roots
    }

    pub fn publish_resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        if id == "api_tester.workspace" {
            // The palette's published workspace. Collections, requests, and
            // variables still resolve from the search catalog.
            return self.publish_resources(doc).into_iter().next();
        }
        self.resource_from_catalog(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::HttpMethod;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    /// A workspace with one collection holding one saved request, and one
    /// environment holding one variable, seeded straight into the document.
    fn seeded() -> (AutoCommit, Uuid, Uuid, Uuid) {
        let root: ObjId = ROOT;
        let mut doc = AutoCommit::new();

        let mut col = Collection::new("Users API");
        let mut req = Request::new("List users");
        req.method = HttpMethod::Get;
        let rid = req.id;
        let cid = col.id;
        col.requests.push(req);
        ApiDoc::set_collections(&mut doc, &root, &[col]);

        let mut env = Environment::new("Production");
        let eid = env.id;
        env.variables
            .push(KeyValue::new("base_url", "https://api.example.com"));
        ApiDoc::set_environments(&mut doc, &root, &[env]);

        (doc, cid, rid, eid)
    }

    /// The workspace root directory is bounded: it publishes the create
    /// actions and device-local child resources, never the records, which
    /// resolve precisely by their canonical deep address instead.
    #[test]
    fn workspace_directory_is_bounded_and_resolves_every_deep_address() {
        // Phase "empty doc": an empty document still publishes the workspace
        // root with the create actions (so the first collection can be made
        // from Ctrl+K), plus the device-local current-request and contextual
        // menu resources, but no collection / environment records.
        {
            let resources = provide_resources(&crate::resource::WorkspaceState::default());
            assert_eq!(resources.len(), 1, "empty doc");
            let root = &resources[0];
            assert_eq!(root.kind, "api_tester.workspace", "empty doc");
            assert!(
                root.actions.iter().any(|a| a.id == "new-collection"),
                "empty doc"
            );
            assert!(
                root.actions.iter().any(|a| a.id == "new-environment"),
                "empty doc"
            );
            assert_eq!(
                root.children
                    .iter()
                    .map(|resource| resource.kind.as_str())
                    .collect::<Vec<_>>(),
                [
                    "api_tester.active_request",
                    "api_tester.request_menu",
                    "api_tester.environment_menu",
                    "api_tester.history",
                ],
                "empty doc"
            );
        }

        // Phase "seeded": every canonical deep address resolves on demand.
        {
            let mut app = crate::app::ApiTesterApp::new_demo();
            let (doc, collection, request, environment) = seeded();
            let collections = ApiDoc::collections(&doc, &ROOT);
            let environments = ApiDoc::environments(&doc, &ROOT);
            app.refresh_search_records(&collections, &environments);
            let variable = ApiDoc::environments(&doc, &ROOT)[0].variables[0].id.clone();
            let root = crate::app::ApiTesterApp::publish_resources(&app, &doc);
            assert_eq!(root.len(), 1, "directory is bounded to the workspace root");
            assert!(
                root[0]
                    .children
                    .iter()
                    .all(|child| child.kind != "api_tester.collection")
            );
            for (id, actions) in [
                (
                    format!("api_tester.collection:{collection}"),
                    &["rename", "delete", "add-request", "save-active"][..],
                ),
                (
                    format!("api_tester.request:{request}"),
                    &["rename", "delete", "send"][..],
                ),
                (
                    format!("api_tester.environment:{environment}"),
                    &["activate", "rename", "delete", "set-variable"][..],
                ),
                (
                    format!("api_tester.variable:{environment}:{variable}"),
                    &["rename", "set-value", "delete"][..],
                ),
            ] {
                let exact = crate::app::ApiTesterApp::publish_resource(&app, &doc, &id)
                    .expect("stable id resolves precisely on demand");
                assert_eq!(exact.id.0, id);
                for action in actions {
                    assert!(
                        exact
                            .actions
                            .iter()
                            .any(|candidate| candidate.id == *action),
                        "{id} exposes {action}"
                    );
                }
            }
        }

        // Phase "failed catalog refresh": exposed as a palette diagnostic
        // resource under the root.
        {
            let mut app = crate::app::ApiTesterApp::new_demo();
            let collection = Collection::new("Duplicate");
            let mut duplicate = collection.clone();
            duplicate.name = "Conflicting duplicate".to_string();
            app.refresh_search_records(&[collection, duplicate], &[]);

            let roots = crate::app::ApiTesterApp::publish_resources(&app, &AutoCommit::new());
            assert!(
                roots[0]
                    .children
                    .iter()
                    .any(|child| child.kind == "api_tester.search_status"),
                "failed catalog refresh"
            );
        }
    }

    #[test]
    fn resource_publish_owns_the_workspace_door_and_hosted_adds_no_second_tree() {
        let state = crate::resource::WorkspaceState::default();
        let published = ui::tilo_resource::publish::<crate::resource::ApiTester>(
            "api_tester.workspace",
            "API workspace",
            &state,
        );
        let root_actions: Vec<&str> = published.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(root_actions.contains(&"new-collection"), "{root_actions:?}");
        assert!(
            root_actions.contains(&"new-environment"),
            "{root_actions:?}"
        );
        assert_eq!(
            published
                .children
                .iter()
                .map(|child| child.kind.as_str())
                .collect::<Vec<_>>(),
            [
                "api_tester.active_request",
                "api_tester.request_menu",
                "api_tester.environment_menu",
                "api_tester.history",
            ],
            "send / menus / history ride Resource::children"
        );
        assert!(
            published.children[0]
                .actions
                .iter()
                .any(|action| action.id == "send"),
            "active request publishes the Send Effect"
        );

        let mut app = crate::app::ApiTesterApp::new();
        let doc = AutoCommit::new();
        assert!(
            !app.actions_are_hosted(),
            "standalone tests stay on the local host"
        );
        let standalone = app.publish_resources(&doc);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "api_tester.workspace");
        assert!(
            standalone[0]
                .actions
                .iter()
                .any(|action| action.id == "new-collection")
        );

        // Hosted surfaces must not republish the workspace door beside
        // ResourceRoot's tilo_resource::publish. Search-catalog failures remain
        // a widget-local diagnostic extra.
        app.mark_actions_hosted_for_test();
        assert!(app.actions_are_hosted());
        let extras = ui::resources::PublishesResources::resources(&app, &doc);
        assert!(
            extras.is_empty(),
            "hosted api-tester publishes only through the framework resource when search is healthy"
        );

        let collection = Collection::new("Duplicate");
        let mut duplicate = collection.clone();
        duplicate.name = "Conflicting duplicate".to_string();
        app.refresh_search_records(&[collection, duplicate], &[]);
        let extras = ui::resources::PublishesResources::resources(&app, &doc);
        assert_eq!(extras.len(), 1);
        assert_eq!(extras[0].kind, "api_tester.search_status");
    }

    /// Record edits address their target by id and change it in place.
    #[test]
    fn record_edits_apply_in_place_by_id() {
        // Case "variable rename": keeps the existing secret value in its own
        // CRDT field.
        {
            let root: ObjId = ROOT;
            let mut doc = AutoCommit::new();
            let mut environment = Environment::new("Production");
            let environment_id = environment.id;
            let variable = KeyValue::new("token", "keep-this-secret");
            let variable_id = variable.id.clone();
            environment.variables.push(variable);
            ApiDoc::set_environments(&mut doc, &root, &[environment]);

            rename_variable_action(environment_id, variable_id, "api_token")
                .apply(&mut doc)
                .expect("key-only rename applies");
            let variable = &ApiDoc::environments(&doc, &root)[0].variables[0];
            assert_eq!(variable.key, "api_token", "variable rename");
            assert_eq!(variable.value, "keep-this-secret", "variable rename");
        }

        // Case "save request": `SaveRequest` is insert-or-replace keyed by
        // request id: saving a request whose id already exists edits it in
        // place rather than appending.
        {
            let (doc, cid, rid, _eid) = seeded();
            let mut cols = ApiDoc::collections(&doc, &ROOT);
            let mut edited = cols[0].requests[0].clone();
            assert_eq!(edited.id, rid);
            edited.method = HttpMethod::Delete;
            edited.name = "Remove users".to_string();
            save_request_action(cid, edited).apply_to_collections(&mut cols);
            assert_eq!(cols[0].requests.len(), 1, "same id replaces, not appends");
            assert_eq!(
                cols[0].requests[0].method,
                HttpMethod::Delete,
                "save request"
            );
            assert_eq!(cols[0].requests[0].name, "Remove users", "save request");
        }
    }

    /// The workspace root plus every seeded record, each materialized through
    /// the app's exact-address resolver the way search and agents reach it
    /// (the root directory itself is bounded and lists no records).
    fn published(doc: &AutoCommit) -> Vec<Resource> {
        let collections = ApiDoc::collections(doc, &ROOT);
        let environments = ApiDoc::environments(doc, &ROOT);
        let mut app = crate::app::ApiTesterApp::new_demo();
        app.refresh_search_records(&collections, &environments);
        let mut ids = Vec::new();
        for collection in &collections {
            ids.push(format!("api_tester.collection:{}", collection.id));
            for request in &collection.requests {
                ids.push(format!("api_tester.request:{}", request.id));
            }
        }
        for environment in &environments {
            ids.push(format!("api_tester.environment:{}", environment.id));
            for variable in &environment.variables {
                ids.push(format!(
                    "api_tester.variable:{}:{}",
                    environment.id, variable.id
                ));
            }
        }
        let mut resources = crate::app::ApiTesterApp::publish_resources(&app, doc);
        resources.extend(ids.iter().map(|id| {
            crate::app::ApiTesterApp::publish_resource(&app, doc, id)
                .expect("a seeded record resolves")
        }));
        resources
    }

    fn collection_id(doc: &AutoCommit) -> Uuid {
        ApiDoc::collections(doc, &ROOT)[0].id
    }

    fn request_id(doc: &AutoCommit) -> Uuid {
        ApiDoc::collections(doc, &ROOT)[0].requests[0].id
    }

    fn environment_id(doc: &AutoCommit) -> Uuid {
        ApiDoc::environments(doc, &ROOT)[0].id
    }

    fn variable_id(doc: &AutoCommit) -> String {
        ApiDoc::environments(doc, &ROOT)[0].variables[0].id.clone()
    }

    ui::action_contract_tests! {
        seed: seeded().0,
        registry: registry(),
        resources: |doc| published(doc),
        palette: [
            "New collection",
            "Delete collection",
            "Delete request",
            "Use environment “Production”",
            "Delete variable",
        ],
        replay: [
            save_request => |doc| {
                // A create carrying a pre-minted id so `apply` needs no RNG.
                let mut request = Request::new("Create user");
                request.id = Uuid::from_u128(0x1234_5678_90ab_cdef_1234_5678_90ab_cdef);
                request.method = HttpMethod::Post;
                encode(&save_request_action(collection_id(doc), request))
            },
        ],
        undo: [
            delete_request => |doc| encode(&delete_request_action(collection_id(doc), request_id(doc))),
        ],
        encodes: [
            delete_collection => ("api_tester.collection", "delete", |doc, _| {
                encode(&delete_collection_action(collection_id(doc)))
            }),
            rename_collection => ("api_tester.collection", "rename", arg: "Accounts API", |doc, _| {
                encode(&rename_collection_action(collection_id(doc), "Accounts API"))
            }),
            delete_request => ("api_tester.request", "delete", |doc, _| {
                encode(&delete_request_action(collection_id(doc), request_id(doc)))
            }),
            rename_request => ("api_tester.request", "rename", arg: "All users", |doc, _| {
                encode(&rename_request_action(collection_id(doc), request_id(doc), "All users"))
            }),
            activate_environment => ("api_tester.environment", "activate", |doc, _| {
                encode(&set_active_environment_action(Some(environment_id(doc))))
            }),
            delete_environment => ("api_tester.environment", "delete", |doc, _| {
                encode(&delete_environment_action(environment_id(doc)))
            }),
            delete_variable => ("api_tester.variable", "delete", |doc, _| {
                encode(&delete_variable_action(environment_id(doc), variable_id(doc)))
            }),
            set_variable_value => ("api_tester.variable", "set-value", arg: "https://staging.example.com", |doc, _| {
                encode(&set_variable_action(
                    environment_id(doc),
                    variable_id(doc),
                    "base_url",
                    "https://staging.example.com",
                ))
            }),
        ],
    }
}
