//! API Tester's exported **actions** and the **resource tree** it publishes to
//! Ctrl+K.
//!
//! Every document-mutating operation the API tester performs on its shared
//! workspace is an [`ApiTesterAction`]: a small serializable value that
//! deterministically mutates the CRDT. The workspace is persisted as JSON
//! snapshots keyed in the document (see [`crate::doc`]) — the collection list,
//! the environment list, and the selected environment — so an action's
//! [`apply`](Action::apply) reads the relevant snapshot, mutates exactly the
//! field(s) it owns, and writes it back: a change that replicates and replays
//! for free. The exact same [`EncodedAction`] is what the on-screen affordances
//! (today the environment picker; tomorrow a New / Rename / Delete control)
//! dispatch and what the Ctrl+K palette dispatches — one source of truth, one
//! replay path.
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

use qos_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus,
    OperationKind, Resource, ResourceProvider, ResultSpec,
};

use crate::doc::ApiDoc;
use crate::model::{Collection, Environment, KeyValue, Request};

/// Registry `type_id` for every API Tester action.
pub const API_TESTER_ACTION_TYPE: &str = "api_tester.action";

/// Operation type for API Tester's non-document resource operations.
pub const API_TESTER_OPERATION_TYPE: &str = "api_tester.operation";

/// Effects which are meaningful on an API Tester resource but must never be
/// replayed as document mutations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ApiTesterOperation {
    SendRequest {
        collection_id: String,
        request_id: String,
    },
}

pub fn send_request_operation(collection_id: Uuid, request_id: Uuid) -> ActionSpec {
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
    .expect("ApiTesterOperation is always serializable")
    .with_description("Send this saved request using the active environment")
    .returns(ResultSpec::Json {
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "status": {"type": "integer"},
                "duration_ms": {"type": "integer"},
                "body": {"type": "string"}
            }
        }),
    })
}

/// Every collaborative operation the API tester exports on its shared workspace.
///
/// Each variant is a whole-record write on one of the persisted snapshots (the
/// collection list, the environment list, or the selected-environment scalar),
/// carrying enough payload to replay deterministically from a shared base state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ApiTesterAction {
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
    /// [`Request`] — method, url, params, headers, auth, body — is carried, so
    /// this one action covers "save a request" and "set method/url/headers/body/
    /// params" as a single deterministic write.
    SaveRequest {
        collection_id: String,
        request: Request,
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
        key: String,
        value: String,
    },
    /// Delete a variable by key from an environment.
    DeleteVariable { env_id: String, key: String },
}

/// Which persisted snapshot an action mutates — so [`Action::apply`] reads,
/// mutates, and writes back exactly one, never churning the others.
enum Target {
    Collections,
    Environments,
    ActiveEnv,
}

impl ApiTesterAction {
    fn target(&self) -> Target {
        match self {
            ApiTesterAction::CreateCollection { .. }
            | ApiTesterAction::RenameCollection { .. }
            | ApiTesterAction::DeleteCollection { .. }
            | ApiTesterAction::SaveRequest { .. }
            | ApiTesterAction::RenameRequest { .. }
            | ApiTesterAction::DeleteRequest { .. } => Target::Collections,
            ApiTesterAction::CreateEnvironment { .. }
            | ApiTesterAction::RenameEnvironment { .. }
            | ApiTesterAction::DeleteEnvironment { .. }
            | ApiTesterAction::SetVariable { .. }
            | ApiTesterAction::DeleteVariable { .. } => Target::Environments,
            ApiTesterAction::SetActiveEnvironment { .. } => Target::ActiveEnv,
        }
    }

    /// The pure mutation on the collection list — the single implementation the
    /// action runs against the document. Deterministic w.r.t. `(self, cols)`: no
    /// RNG, no wall-clock, stable iteration.
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
            ApiTesterAction::SetVariable { env_id, key, value } => {
                if let Some(e) = find_environment_mut(envs, env_id) {
                    if let Some(v) = e.variables.iter_mut().find(|v| v.key == *key) {
                        v.value.clone_from(value); // update in place
                    } else {
                        e.variables.push(KeyValue::new(key.clone(), value.clone()));
                    }
                }
            }
            ApiTesterAction::DeleteVariable { env_id, key } => {
                if let Some(e) = find_environment_mut(envs, env_id) {
                    e.variables.retain(|v| v.key != *key);
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
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let root: ObjId = ROOT;
        match self.target() {
            Target::Collections => {
                let mut cols = ApiDoc::collections(doc, &root);
                self.apply_to_collections(&mut cols);
                ApiDoc::set_collections(doc, &root, &cols);
            }
            Target::Environments => {
                let mut envs = ApiDoc::environments(doc, &root);
                self.apply_to_environments(&mut envs);
                ApiDoc::set_environments(doc, &root, &envs);
            }
            Target::ActiveEnv => {
                if let ApiTesterAction::SetActiveEnvironment { id } = self {
                    ApiDoc::set_active_env(doc, &root, id.as_deref());
                }
            }
        }
        Ok(())
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

pub fn set_variable_action(env_id: Uuid, key: &str, value: &str) -> ApiTesterAction {
    ApiTesterAction::SetVariable {
        env_id: env_id.to_string(),
        key: key.to_string(),
        value: value.to_string(),
    }
}

pub fn delete_variable_action(env_id: Uuid, key: &str) -> ApiTesterAction {
    ApiTesterAction::DeleteVariable {
        env_id: env_id.to_string(),
        key: key.to_string(),
    }
}

/// Split a prompted `"key=value"` argument into its two halves (value may hold
/// further `=` signs, e.g. a base64 token). A bare `"key"` yields an empty
/// value.
fn split_kv(arg: &str) -> (String, String) {
    match arg.split_once('=') {
        Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
        None => (arg.trim().to_string(), String::new()),
    }
}

// ===========================================================================
// Resource tree
// ===========================================================================

fn immediate(id: &str, label: impl Into<String>, action: &ApiTesterAction) -> ActionSpec {
    let label = label.into();
    ActionSpec::immediate(id, label.clone(), API_TESTER_ACTION_TYPE, action).unwrap_or_else(|_| {
        // The fallible builder's fallback. `ActionSpec`'s fields are
        // private now that it carries typed parameters, so construct it
        // through the constructor that takes an already-encoded action.
        ActionSpec::immediate_encoded(id, label, action.locus(), encode(action))
    })
}

/// One-line label for a saved request: its method and name.
fn request_label(r: &Request) -> String {
    let name = if r.name.trim().is_empty() {
        "Untitled request"
    } else {
        r.name.trim()
    };
    format!("{} {}", r.method.as_str(), name)
}

/// Build the resource tree the API tester currently has open: the workspace, its
/// collections (each owning its saved requests), and — under a single
/// environments node — its environments (each owning its variables). Every
/// document-mutating operation is exported as the same action an on-screen
/// affordance would dispatch.
///
/// The shared state lives in the document, so this reads the model straight from
/// `doc`. The workspace root is always published (so Ctrl+K can create the first
/// collection / environment even on an empty document); children appear only for
/// rows that actually exist. Presentation — the loaded request, the active tab —
/// is never exported.
pub fn provide_resources(doc: &AutoCommit) -> Vec<Resource> {
    let root_obj: ObjId = ROOT;
    let collections = ApiDoc::collections(doc, &root_obj);
    let environments = ApiDoc::environments(doc, &root_obj);
    let active_env = ApiDoc::active_env(doc, &root_obj);
    let active_request = ApiDoc::request(doc, &root_obj);

    let mut root = Resource::new(
        "api_tester.workspace",
        "api_tester.workspace",
        "API workspace",
    );

    // Workspace-level: create a collection / environment (prompted for a name;
    // the id is minted once when the argument is committed, then baked into the
    // envelope, so replay stays deterministic).
    root = root.with_action(ActionSpec::prompted(
        "new-collection",
        "New collection",
        "Collection name",
        "e.g. Users API",
        ExecutionLocus::ClientLocal,
        move |name: &str| encode(&create_collection_action(Uuid::new_v4(), name.trim())),
    ));
    root = root.with_action(ActionSpec::prompted(
        "new-environment",
        "New environment",
        "Environment name",
        "e.g. Production",
        ExecutionLocus::ClientLocal,
        move |name: &str| encode(&create_environment_action(Uuid::new_v4(), name.trim())),
    ));

    root = add_collection_children(root, &collections, active_request.as_ref());
    root = add_environment_children(root, &environments, active_env.as_deref());

    vec![root]
}

/// Collections → a resource each, owning its saved requests. Split out of
/// [`provide_resources`] to keep it under the line budget.
fn add_collection_children(
    mut root: Resource,
    collections: &[Collection],
    active_request: Option<&Request>,
) -> Resource {
    for c in collections {
        let cid = c.id;
        let mut node = Resource::new(
            format!("api_tester.collection:{cid}"),
            "api_tester.collection",
            c.name.clone(),
        );
        node = node.with_action(ActionSpec::prompted(
            "rename",
            "Rename collection",
            "Collection name",
            "new name…",
            ExecutionLocus::ClientLocal,
            move |name: &str| encode(&rename_collection_action(cid, name.trim())),
        ));
        node = node.with_action(immediate(
            "delete",
            "Delete collection",
            &delete_collection_action(cid),
        ));
        // Add a new blank request (a fresh GET named by the argument).
        node = node.with_action(ActionSpec::prompted(
            "add-request",
            "Add request",
            "Request name",
            "e.g. List users",
            ExecutionLocus::ClientLocal,
            move |name: &str| {
                let mut req = Request::new(name.trim());
                req.id = Uuid::new_v4();
                encode(&save_request_action(cid, req))
            },
        ));
        // Save the request currently in the builder into this collection, as a
        // new saved copy — exercises the full whole-record write with the live
        // method / url / params / body. Only offered when a request is loaded.
        if let Some(active) = active_request {
            let mut copy = active.clone();
            copy.id = Uuid::new_v4();
            if copy.name.trim().is_empty() {
                copy.name = "Saved request".to_string();
            }
            node = node.with_action(immediate(
                "save-active",
                "Save current request here",
                &save_request_action(cid, copy),
            ));
        }

        for r in &c.requests {
            let rid = r.id;
            let child = Resource::new(
                format!("api_tester.request:{rid}"),
                "api_tester.request",
                request_label(r),
            )
            .with_action(ActionSpec::prompted(
                "rename",
                "Rename request",
                "Request name",
                "new name…",
                ExecutionLocus::ClientLocal,
                move |name: &str| encode(&rename_request_action(cid, rid, name.trim())),
            ))
            .with_action(immediate(
                "delete",
                "Delete request",
                &delete_request_action(cid, rid),
            ))
            .with_action(send_request_operation(cid, rid));
            node = node.with_child(child);
        }
        root = root.with_child(node);
    }
    root
}

/// Environments → grouped under one env node, each owning its variables.
/// Split out of [`provide_resources`] to keep it under the line budget.
fn add_environment_children(
    mut root: Resource,
    environments: &[Environment],
    active_env: Option<&str>,
) -> Resource {
    if environments.is_empty() {
        return root;
    }
    let mut env_root = Resource::new(
        "api_tester.environments",
        "api_tester.environments",
        "Environments",
    );
    for e in environments {
        let eid = e.id;
        let is_active = active_env == Some(eid.to_string().as_str());
        let mut node = Resource::new(
            format!("api_tester.environment:{eid}"),
            "api_tester.environment",
            e.name.clone(),
        );
        if !is_active {
            node = node.with_action(immediate(
                "activate",
                format!("Use environment “{}”", e.name),
                &set_active_environment_action(Some(eid)),
            ));
        }
        node = node.with_action(ActionSpec::prompted(
            "rename",
            "Rename environment",
            "Environment name",
            "new name…",
            ExecutionLocus::ClientLocal,
            move |name: &str| encode(&rename_environment_action(eid, name.trim())),
        ));
        node = node.with_action(immediate(
            "delete",
            "Delete environment",
            &delete_environment_action(eid),
        ));
        node = node.with_action(ActionSpec::prompted(
            "set-variable",
            "Set variable",
            "key=value",
            "e.g. base_url=https://api.example.com",
            ExecutionLocus::ClientLocal,
            move |arg: &str| {
                let (k, v) = split_kv(arg);
                encode(&set_variable_action(eid, &k, &v))
            },
        ));

        for v in &e.variables {
            let key = v.key.clone();
            let child = Resource::new(
                format!("api_tester.variable:{eid}:{}", v.key),
                "api_tester.variable",
                format!("{} = {}", v.key, v.value),
            )
            .with_action(immediate(
                "delete",
                "Delete variable",
                &delete_variable_action(eid, &key),
            ));
            node = node.with_child(child);
        }
        env_root = env_root.with_child(node);
    }
    root = root.with_child(env_root);
    root
}

impl ResourceProvider for crate::app::ApiTesterApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::HttpMethod;
    use qos_actions::{ActionInput, LocalTransport};

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } => panic!("expected an immediate action: {}", spec.id),
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) => panic!("expected a prompted action: {}", spec.id),
        }
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

    /// Ctrl+K on the API tester yields the workspace tree: a collection child (its
    /// saved request nested under it), and — under an environments node — an
    /// environment child (its variable nested under it), plus the workspace-level
    /// create actions. Drives the real resource path.
    #[test]
    fn ctrl_k_publishes_the_workspace_tree() {
        let (doc, _cid, _rid, _eid) = seeded();
        let resources = provide_resources(&doc);
        assert_eq!(resources.len(), 1, "one workspace root");
        let root = &resources[0];
        assert_eq!(root.kind, "api_tester.workspace");

        let root_actions: Vec<&str> = root.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(root_actions.contains(&"new-collection"), "{root_actions:?}");
        assert!(
            root_actions.contains(&"new-environment"),
            "{root_actions:?}"
        );

        let col = root
            .children
            .iter()
            .find(|r| r.kind == "api_tester.collection")
            .expect("a collection resource");
        let col_actions: Vec<&str> = col.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(col_actions.contains(&"rename"), "{col_actions:?}");
        assert!(col_actions.contains(&"delete"), "{col_actions:?}");
        assert!(col_actions.contains(&"add-request"), "{col_actions:?}");
        let req = col
            .children
            .iter()
            .find(|r| r.kind == "api_tester.request")
            .expect("a request resource nested under the collection");
        let req_actions: Vec<&str> = req.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(req_actions.contains(&"rename"), "{req_actions:?}");
        assert!(req_actions.contains(&"delete"), "{req_actions:?}");
        assert!(req_actions.contains(&"send"), "{req_actions:?}");
        let send = req
            .actions
            .iter()
            .find(|action| action.id == "send")
            .expect("saved requests export their network effect");
        assert_eq!(send.kind, OperationKind::Effect);
        let encoded = immediate_encode(send);
        assert_eq!(encoded.type_id, API_TESTER_OPERATION_TYPE);
        assert!(matches!(
            serde_json::from_slice::<ApiTesterOperation>(&encoded.payload).unwrap(),
            ApiTesterOperation::SendRequest { .. }
        ));

        let env_node = root
            .children
            .iter()
            .find(|r| r.kind == "api_tester.environments")
            .expect("an environments node");
        let env = env_node
            .children
            .iter()
            .find(|r| r.kind == "api_tester.environment")
            .expect("an environment resource");
        let env_actions: Vec<&str> = env.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(env_actions.contains(&"activate"), "{env_actions:?}");
        assert!(env_actions.contains(&"rename"), "{env_actions:?}");
        assert!(env_actions.contains(&"delete"), "{env_actions:?}");
        assert!(env_actions.contains(&"set-variable"), "{env_actions:?}");
        let var = env
            .children
            .iter()
            .find(|r| r.kind == "api_tester.variable")
            .expect("a variable nested under the environment");
        assert!(
            var.actions.iter().any(|a| a.id == "delete"),
            "variable exports delete"
        );

        // The one palette surfaces the tree's actions as rows (the Actions group
        // is capped, so assert on the leading, always-present ones).
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("New collection")),
            "{labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("Delete collection")),
            "{labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("Add request")),
            "{labels:?}"
        );
    }

    /// An empty document still publishes the workspace root with the create
    /// actions (so the first collection can be made from Ctrl+K), but no
    /// collection / environment children and no environments node.
    #[test]
    fn empty_doc_publishes_workspace_with_create_actions_only() {
        let doc = AutoCommit::new();
        let resources = provide_resources(&doc);
        assert_eq!(resources.len(), 1);
        let root = &resources[0];
        assert_eq!(root.kind, "api_tester.workspace");
        assert!(root.actions.iter().any(|a| a.id == "new-collection"));
        assert!(root.actions.iter().any(|a| a.id == "new-environment"));
        assert!(
            root.children.is_empty(),
            "no fabricated collections or environments"
        );
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// mutates the document and records exactly one undo boundary, and Ctrl+Z
    /// (the host's `undo`) reverts precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let root: ObjId = ROOT;
        let (seeded_doc, cid, rid, _eid) = seeded();
        let mut state = ui::State::new();
        state.doc = seeded_doc;
        // `get_heads` flushes the seed writes into committed changes; the action
        // that follows lands in its own change bracketed by these heads.
        let before_heads = state.doc.get_heads();
        assert_eq!(
            ApiDoc::collections(&state.doc, &root)[0].requests.len(),
            1,
            "precondition: the saved request is present"
        );

        let action = delete_request_action(cid, rid);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert!(
            ApiDoc::collections(&state.doc, &root)[0]
                .requests
                .is_empty(),
            "the action deleted the saved request"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        state.undo(None, None).expect("an undo entry to pop");
        assert_eq!(
            ApiDoc::collections(&state.doc, &root)[0].requests.len(),
            1,
            "undo restored the request and nothing else"
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a second
    /// independent replica (from the same base state) lands on the byte-identical
    /// document — same head hashes, not merely the same values.
    #[test]
    fn action_replays_byte_identical_on_a_peer() {
        let root: ObjId = ROOT;
        let (mut a, cid, _rid, _eid) = seeded();
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        // A create carrying a pre-minted id so `apply` needs no RNG.
        let mut req = Request::new("Create user");
        req.id = Uuid::from_u128(0x1234_5678_90ab_cdef_1234_5678_90ab_cdef);
        req.method = HttpMethod::Post;
        let action = save_request_action(cid, req);
        let encoded = encode(&action);

        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert_eq!(
            ApiDoc::collections(&a, &root)[0].requests.len(),
            ApiDoc::collections(&b, &root)[0].requests.len(),
        );
        assert!(
            ApiDoc::collections(&b, &root)[0]
                .requests
                .iter()
                .any(|r| r.name == "Create user"),
            "the saved request landed on the replica"
        );
    }

    /// `SaveRequest` is insert-or-replace keyed by request id: saving a request
    /// whose id already exists edits it in place rather than appending.
    #[test]
    fn save_request_replaces_in_place_by_id() {
        let (doc, cid, rid, _eid) = seeded();
        let mut cols = ApiDoc::collections(&doc, &ROOT);
        let mut edited = cols[0].requests[0].clone();
        assert_eq!(edited.id, rid);
        edited.method = HttpMethod::Delete;
        edited.name = "Remove users".to_string();
        save_request_action(cid, edited).apply_to_collections(&mut cols);
        assert_eq!(cols[0].requests.len(), 1, "same id replaces, not appends");
        assert_eq!(cols[0].requests[0].method, HttpMethod::Delete);
        assert_eq!(cols[0].requests[0].name, "Remove users");
    }

    /// Single source of truth: the palette's "Delete request" spec and the
    /// builder the on-screen trash affordance calls encode byte-identically; and
    /// a prompted "Rename collection" spec matches its builder for the same text.
    #[test]
    fn palette_and_builder_encode_identically() {
        let (doc, cid, rid, _eid) = seeded();
        let resources = provide_resources(&doc);
        let root = &resources[0];
        let col = root
            .children
            .iter()
            .find(|r| r.kind == "api_tester.collection")
            .unwrap();

        let req = col
            .children
            .iter()
            .find(|r| r.kind == "api_tester.request")
            .unwrap();
        let del = req.actions.iter().find(|a| a.id == "delete").unwrap();
        let from_spec = immediate_encode(del);
        let from_builder = encode(&delete_request_action(cid, rid));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "one source of truth for delete"
        );

        let rename = col.actions.iter().find(|a| a.id == "rename").unwrap();
        let from_spec = prompted_encode(rename, "Accounts API");
        let from_builder = encode(&rename_collection_action(cid, "Accounts API"));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "rename is one source of truth"
        );
    }
}
