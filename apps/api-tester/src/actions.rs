//! API Tester's vocabulary: the workspace's actions, its effects, and the
//! resources it publishes inside the workspace.
//!
//! Every change to the shared workspace is an [`ApiTesterAction`]: creating,
//! renaming and deleting collections, saved requests, environments and
//! variables; choosing the active environment; and editing the request in the
//! builder — its method, URL and body, its auth scheme, body mode and rows.
//! The URL and body are plain-text children of the workspace, so typing in
//! them is a splice that merges with a collaborator's typing. The builder's
//! controls, the palette, agents and scripts all submit the same values.
//!
//! Sending a request is an effect ([`ApiTesterEffect`]): it reaches the
//! network from this device, and what it brings back is local, except the
//! history row its completion records through an action. A credential — a
//! request's token or password, a secret variable's value — never enters the
//! document or an action: it is set through an effect into the device's
//! secret store, keyed by the record it belongs to.
//!
//! ## Determinism contract
//!
//! Ids (a collection's, a request's, an environment's) are minted when an
//! action is built and carried in its payload. [`Action::apply`] never
//! touches the RNG or the wall-clock, and a payload that names no existing
//! row is a no-op rather than an error, so a bad replay can't wedge a peer.

use automerge::{ObjId, ROOT};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use tilo_actions::{
    Action, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, ExecutionMode, OperationKind, ParamType, Resource, ResultSpec,
};
use ui::prelude::Effect;
// The effect derive names its runtime crate by this path.
use ui::tilo_resource;
use ui::tilo_resource::{
    ActionDecl, PlainText, SearchEntry, TextEdit, child_resource_id, publish_child,
};

use crate::doc::ApiDoc;
use crate::model::{
    Auth, BodyMode, Collection, Environment, HistoryEntry, HttpMethod, KeyValue, Request,
};
use crate::resource::{ApiTester, Workspace};

/// Registry `type_id` for every API Tester action.
pub const API_TESTER_ACTION_TYPE: &str = "api_tester.action";

/// How many deep records (saved requests, variables, history rows) the
/// workspace publishes at once; the rest resolve by id.
pub const PUBLISHED_RECORDS: usize = 256;

pub const REQUEST_MENU_CHOICES: &[&str] = &["Save request…", "Rename request…", "Delete request…"];
pub const ENVIRONMENT_MENU_CHOICES: &[&str] =
    &["Rename environment…", "Add variable…", "Delete variable…"];

/// Work that reaches outside the document.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "api_tester.effects", device_only)]
pub enum ApiTesterEffect {
    /// Send request
    #[effect(job)]
    Send,
    /// Send saved request
    #[effect(job)]
    SendSaved {
        #[param("Id of the saved request")]
        request: String,
    },
    /// Set the request's credential
    SetCredential {
        #[param("The token, password or key, kept on this device")]
        secret: String,
    },
    /// Edit authentication metadata
    EditAuthField {
        #[param("Authentication field", choices = ["username", "header"])]
        field: String,
        #[param("The shared field value; credentials use Set credential")]
        value: String,
    },
    /// Choose an environment or open its management form
    #[effect(view, idempotent)]
    PickEnvironment {
        #[param("Environment name, No environment, or an environment management command")]
        choice: String,
    },
    /// Set a secret variable's value
    SetSecretValue {
        #[param("Id of the variable")]
        variable: String,
        #[param("The value, kept on this device")]
        value: String,
    },
    /// Request actions
    #[effect(view, row, idempotent)]
    RequestMenu {
        #[param("Request actions", choices = REQUEST_MENU_CHOICES.iter().copied())]
        action: String,
    },
    /// Environment actions
    #[effect(view, row, idempotent)]
    EnvironmentMenu {
        #[param("Environment actions", choices = ENVIRONMENT_MENU_CHOICES.iter().copied())]
        action: String,
    },
}

/// The rows of the request builder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RowSection {
    Params,
    Headers,
    Form,
}

impl RowSection {
    fn rows(self, request: &mut Request) -> &mut Vec<KeyValue> {
        match self {
            RowSection::Params => &mut request.params,
            RowSection::Headers => &mut request.headers,
            RowSection::Form => &mut request.form,
        }
    }
}

/// A contextual menu of the builder: a child offering its effect, whose
/// choice opens a form in this window and changes nothing in the document.
fn menu_resource(id: &str, label: &str, effect: &str) -> Resource {
    let mut resource = Resource::new(id, id, label);
    if let Some(menu) = ApiTesterEffect::declarations()
        .into_iter()
        .find(|decl| decl.id == effect)
    {
        resource = resource.with_action(menu.spec(ApiTesterEffect::OPERATION));
    }
    resource
}

pub fn request_menu_resource() -> Resource {
    menu_resource("api_tester.request_menu", "Request actions", "request-menu")
}

pub fn environment_menu_resource() -> Resource {
    menu_resource(
        "api_tester.environment_menu",
        "Environment actions",
        "environment-menu",
    )
}

/// Every change to the shared workspace. Each variant names only its
/// durable target and is replayed against the document's keyed records; it
/// never carries presentation state or a credential.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ApiTesterAction {
    /// Record a completed request without replaying its network effect.
    RecordHistory { entry: HistoryEntry },
    // --- Collections ---------------------------------------------------------
    /// Add a new, empty collection. A replay that finds the id already
    /// present is a no-op, so re-applying can't stack a duplicate.
    CreateCollection { id: String, name: String },
    /// Rename a collection by id.
    RenameCollection { id: String, name: String },
    /// Delete a collection (and every saved request it holds) by id.
    DeleteCollection { id: String },

    // --- Saved requests (within a collection) --------------------------------
    /// Insert or replace a saved request in a collection, keyed by the
    /// request's own id: an existing id is replaced in place, a new one is
    /// added. Its credential is not stored.
    SaveRequest {
        collection_id: String,
        request: Request,
    },
    /// Save the request in the builder into a collection as `request_id`,
    /// named `name` (its own name when empty).
    SaveCurrentRequest {
        collection_id: String,
        request_id: String,
        #[serde(default)]
        name: String,
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

    // --- The request in the builder ------------------------------------------
    /// Open `request` in the builder: a saved request, or a new one.
    OpenRequest { request: Request },
    /// Open a history row's method and URL in the builder.
    OpenHistory { history_id: String },
    /// Set the builder's HTTP method.
    SetMethod { method: HttpMethod },
    /// One edit of the builder's URL.
    EditUrl(TextEdit),
    /// One edit of the builder's body.
    EditBody(TextEdit),
    /// Set the builder's auth scheme and its public fields (the username,
    /// the key's header); the credential is set on the device.
    SetAuth { auth: Auth },
    /// Set how the builder's body is encoded.
    SetBodyMode { mode: BodyMode },
    /// Add or replace a param, header or form row, keyed by its id.
    SetRow { section: RowSection, row: KeyValue },
    /// Edit one public row field without replacing the other fields.
    EditRowField {
        section: RowSection,
        row_id: String,
        field: String,
        value: String,
    },
    /// Remove a param, header or form row by id.
    RemoveRow { section: RowSection, row_id: String },

    // --- Environments --------------------------------------------------------
    /// Add a new, empty environment. An existing id is a no-op.
    CreateEnvironment { id: String, name: String },
    /// Rename an environment by id.
    RenameEnvironment { id: String, name: String },
    /// Delete an environment by id.
    DeleteEnvironment { id: String },
    /// Select the active environment (whose `{{variables}}` resolve into
    /// outgoing requests), or clear it with `None`.
    SetActiveEnvironment { id: Option<String> },

    // --- Variables (within an environment) -----------------------------------
    /// Add or update a `{{variable}}` at its stable id. A secret variable's
    /// value is kept on the device, so its `value` here is ignored.
    SetVariable {
        env_id: String,
        /// Stable row identity. A rename changes `key`, never this address.
        variable_id: String,
        key: String,
        value: String,
        #[serde(default)]
        secret: bool,
    },
    /// Rename a variable without reading or serializing its value.
    RenameVariable {
        env_id: String,
        variable_id: String,
        key: String,
    },
    /// Delete a variable from an environment.
    DeleteVariable { env_id: String, variable_id: String },
}

fn saved_request(
    doc: &Doc,
    root: &ObjId,
    collection_id: &str,
    request_id: &str,
) -> Option<Request> {
    ApiDoc::collections(doc, root)
        .into_iter()
        .find(|collection| collection.id.to_string() == collection_id)?
        .requests
        .into_iter()
        .find(|request| request.id.to_string() == request_id)
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
            }
            ApiTesterAction::CreateCollection { id, name } => {
                ApiDoc::create_collection(doc, &root, id, name);
            }
            ApiTesterAction::RenameCollection { id, name } => {
                ApiDoc::rename_collection(doc, &root, id, name);
            }
            ApiTesterAction::DeleteCollection { id } => {
                ApiDoc::delete_collection(doc, &root, id);
            }
            ApiTesterAction::CreateEnvironment { id, name } => {
                ApiDoc::create_environment(doc, &root, id, name);
            }
            ApiTesterAction::RenameEnvironment { id, name } => {
                ApiDoc::rename_environment(doc, &root, id, name);
            }
            ApiTesterAction::DeleteEnvironment { id } => {
                ApiDoc::delete_environment(doc, &root, id);
            }
            ApiTesterAction::SaveRequest {
                collection_id,
                request,
            } => {
                let before = saved_request(doc, &root, collection_id, &request.id.to_string());
                ApiDoc::save_request(doc, &root, collection_id, before.as_ref(), request);
            }
            ApiTesterAction::SaveCurrentRequest {
                collection_id,
                request_id,
                name,
            } => {
                let Ok(request_id) = request_id.parse::<Uuid>() else {
                    return Ok(());
                };
                let Some(mut request) = ApiDoc::request(doc, &root) else {
                    return Ok(());
                };
                request.id = request_id;
                if !name.trim().is_empty() {
                    name.trim().clone_into(&mut request.name);
                }
                ApiDoc::save_request(doc, &root, collection_id, None, &request);
            }
            ApiTesterAction::RenameRequest {
                collection_id,
                request_id,
                name,
            } => {
                let Some(mut request) = saved_request(doc, &root, collection_id, request_id) else {
                    return Ok(());
                };
                request.name.clone_from(name);
                ApiDoc::save_request(doc, &root, collection_id, None, &request);
            }
            ApiTesterAction::DeleteRequest { request_id, .. } => {
                ApiDoc::delete_request(doc, &root, request_id);
            }
            ApiTesterAction::OpenRequest { request } => {
                let request = request.clone();
                ApiDoc::update_request(doc, &root, |current| *current = request);
            }
            ApiTesterAction::OpenHistory { history_id } => {
                let Some(entry) = ApiDoc::history(doc, &root)
                    .into_iter()
                    .find(|entry| entry.id.to_string() == *history_id)
                else {
                    return Ok(());
                };
                ApiDoc::update_request(doc, &root, |request| {
                    request.method = entry.method;
                    request.url = entry.url;
                });
            }
            ApiTesterAction::SetMethod { method } => {
                ApiDoc::update_request(doc, &root, |request| request.method = *method);
            }
            ApiTesterAction::EditUrl(edit) => {
                let text = ApiDoc::url_text(doc, &root);
                edit.apply_under(doc, &text)?;
            }
            ApiTesterAction::EditBody(edit) => {
                let text = ApiDoc::body_text(doc, &root);
                edit.apply_under(doc, &text)?;
            }
            ApiTesterAction::SetAuth { auth } => {
                ApiDoc::update_request(doc, &root, |request| request.auth = auth.without_secret());
            }
            ApiTesterAction::SetBodyMode { mode } => {
                ApiDoc::update_request(doc, &root, |request| request.body_mode = *mode);
            }
            ApiTesterAction::SetRow { section, row } => {
                ApiDoc::update_request(doc, &root, |request| {
                    let rows = section.rows(request);
                    match rows.iter_mut().find(|existing| existing.id == row.id) {
                        Some(existing) => *existing = row.clone(),
                        None => rows.push(row.clone()),
                    }
                });
            }
            ApiTesterAction::EditRowField {
                section,
                row_id,
                field,
                value,
            } => {
                if !matches!(field.as_str(), "key" | "value" | "enabled")
                    || Uuid::parse_str(row_id).is_err()
                {
                    return Err(ActionError::Refused("invalid request row field".into()));
                }
                let enabled =
                    if field == "enabled" {
                        Some(value.parse::<bool>().map_err(|_| {
                            ActionError::Refused("invalid row inclusion value".into())
                        })?)
                    } else {
                        None
                    };
                ApiDoc::update_request(doc, &root, |request| {
                    let rows = section.rows(request);
                    let index = if let Some(index) = rows.iter().position(|row| row.id == *row_id) {
                        index
                    } else {
                        if value.is_empty() || enabled.is_some() {
                            return;
                        }
                        rows.push(KeyValue {
                            id: row_id.clone(),
                            key: String::new(),
                            value: String::new(),
                            enabled: true,
                            secret: false,
                        });
                        rows.len().saturating_sub(1)
                    };
                    if let Some(row) = rows.get_mut(index) {
                        if let Some(enabled) = enabled {
                            row.enabled = enabled;
                        } else if field == "key" {
                            row.key.clone_from(value);
                        } else {
                            row.value.clone_from(value);
                        }
                    }
                });
            }
            ApiTesterAction::RemoveRow { section, row_id } => {
                ApiDoc::update_request(doc, &root, |request| {
                    section.rows(request).retain(|row| row.id != *row_id);
                });
            }
            ApiTesterAction::SetVariable {
                env_id,
                variable_id,
                key,
                value,
                secret,
            } => {
                ApiDoc::set_variable(doc, &root, env_id, variable_id, key, value, *secret);
            }
            ApiTesterAction::RenameVariable {
                env_id,
                variable_id,
                key,
            } => {
                ApiDoc::rename_variable(doc, &root, env_id, variable_id, key);
            }
            ApiTesterAction::DeleteVariable {
                env_id,
                variable_id,
            } => {
                ApiDoc::delete_variable(doc, &root, env_id, variable_id);
            }
            ApiTesterAction::SetActiveEnvironment { id } => {
                ApiDoc::set_active_env(doc, &root, id.as_deref());
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

/// Encode any [`ApiTesterAction`] into its wire form.
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
// Action builders — shared by the on-screen affordances (see `crate::app`)
// and the published tree below.
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

pub fn save_current_request_action(
    collection_id: Uuid,
    request_id: Uuid,
    name: &str,
) -> ApiTesterAction {
    ApiTesterAction::SaveCurrentRequest {
        collection_id: collection_id.to_string(),
        request_id: request_id.to_string(),
        name: name.to_string(),
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
    secret: bool,
) -> ApiTesterAction {
    ApiTesterAction::SetVariable {
        env_id: env_id.to_string(),
        variable_id: variable_id.into(),
        key: key.to_string(),
        value: if secret {
            String::new()
        } else {
            value.to_string()
        },
        secret,
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

/// The actions that build `workspace` from an empty one: its collections,
/// environments and variables, the active environment, the builder's request
/// and the history. A secret variable's value is not among them.
pub fn workspace_actions(
    collections: &[Collection],
    environments: &[Environment],
    request: &Request,
    history: &[HistoryEntry],
) -> Vec<ApiTesterAction> {
    let mut actions = Vec::new();
    for collection in collections {
        actions.push(create_collection_action(collection.id, &collection.name));
        actions.extend(
            collection
                .requests
                .iter()
                .map(|request| save_request_action(collection.id, request.clone())),
        );
    }
    for environment in environments {
        actions.push(create_environment_action(environment.id, &environment.name));
        actions.extend(environment.variables.iter().map(|variable| {
            set_variable_action(
                environment.id,
                variable.id.clone(),
                &variable.key,
                &variable.value,
                variable.secret,
            )
        }));
    }
    actions.push(set_active_environment_action(
        environments.first().map(|environment| environment.id),
    ));
    actions.push(ApiTesterAction::OpenRequest {
        request: request.clone(),
    });
    actions.extend(
        history
            .iter()
            .rev()
            .map(|entry| ApiTesterAction::RecordHistory {
                entry: entry.clone(),
            }),
    );
    actions
}

// ===========================================================================
// The published tree
// ===========================================================================

pub(crate) fn immediate(
    id: &str,
    label: impl Into<String>,
    action: &ApiTesterAction,
) -> ActionSpec {
    let label = label.into();
    ActionSpec::immediate(id, label.clone(), API_TESTER_ACTION_TYPE, action).unwrap_or_else(|_| {
        ActionSpec::immediate_encoded(id, label, action.locus(), encode(action))
    })
}

fn prompted(
    id: &str,
    label: &str,
    prompt: &str,
    placeholder: &str,
    build: impl Fn(&str) -> ApiTesterAction + Send + Sync + 'static,
) -> ActionSpec {
    ActionSpec::prompted(
        id,
        label,
        prompt,
        placeholder,
        ExecutionLocus::ClientLocal,
        move |text: &str| encode(&build(text.trim())),
    )
}

/// The label of the "no environment" choice.
pub const NO_ENVIRONMENT: &str = "No Environment";

/// Choosing the builder's HTTP method, as the method picker does.
pub(crate) fn set_method_declaration() -> ActionDecl<ApiTester> {
    ActionDecl::new(
        "set-method",
        "Set request method",
        vec![
            ActionParam::required("method", ParamType::Text, "the HTTP method")
                .with_choices(HttpMethod::ALL.iter().map(|method| method.as_str())),
        ],
        |args, _| {
            Ok(ApiTesterAction::SetMethod {
                method: HttpMethod::from_str(args.text("method").unwrap_or_default()),
            })
        },
    )
}

/// Choosing the active environment by name, as the environment picker does.
pub(crate) fn use_environment_declaration(state: &Workspace) -> ActionDecl<ApiTester> {
    let environments: Vec<(String, Uuid)> = state
        .environments
        .iter()
        .map(|environment| (environment.name.clone(), environment.id))
        .collect();
    let choices: Vec<String> = std::iter::once(NO_ENVIRONMENT.to_string())
        .chain(environments.iter().map(|(name, _)| name.clone()))
        .collect();
    ActionDecl::new(
        "use-environment",
        "Use environment",
        vec![ActionParam::required_owned(
            "environment",
            ParamType::Text,
            format!("the environment's name: {}", choices.join(", ")),
        )],
        move |args, _| {
            let name = args.text("environment").unwrap_or_default();
            if name == NO_ENVIRONMENT {
                return Ok(set_active_environment_action(None));
            }
            environments
                .iter()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, id)| set_active_environment_action(Some(*id)))
                .ok_or_else(|| format!("no environment is named {name}"))
        },
    )
}

/// The workspace's create actions: the new record's id is minted when the
/// name is given.
pub(crate) fn workspace_bound_actions() -> Vec<ActionSpec> {
    vec![
        prompted(
            "new-collection",
            "New collection",
            "Collection name",
            "e.g. Users API",
            |name| create_collection_action(Uuid::new_v4(), name),
        ),
        prompted(
            "new-environment",
            "New environment",
            "Environment name",
            "e.g. Production",
            |name| create_environment_action(Uuid::new_v4(), name),
        ),
    ]
}

/// The id the builder's URL is published under, as a plain-text child.
pub(crate) fn url_resource_id() -> String {
    child_resource_id::<ApiTester>("url")
}

/// The id the builder's body is published under, as a plain-text child.
pub(crate) fn body_resource_id() -> String {
    child_resource_id::<ApiTester>("body")
}

pub(crate) fn collection_resource_id(id: Uuid) -> String {
    format!("api_tester.collection:{id}")
}

pub(crate) fn request_resource_id(id: Uuid) -> String {
    format!("api_tester.request:{id}")
}

pub(crate) fn environment_resource_id(id: Uuid) -> String {
    format!("api_tester.environment:{id}")
}

pub(crate) fn variable_resource_id(environment: Uuid, variable: &str) -> String {
    format!("api_tester.variable:{environment}:{variable}")
}

pub(crate) fn history_resource_id(id: Uuid) -> String {
    format!("api_tester.history:{id}")
}

/// Everything the workspace publishes inside itself: the builder's URL and
/// body, its menus, and the records — collections with their requests,
/// environments with their variables, and the history — up to
/// [`PUBLISHED_RECORDS`] deep records; [`resolve_child`] answers the rest.
pub(crate) fn document_children(state: &Workspace) -> Vec<Resource> {
    let mut children = vec![
        publish_child::<PlainText, ApiTester>(
            &url_resource_id(),
            "Request URL",
            Some(&state.request.url),
            ApiTesterAction::EditUrl,
        ),
        publish_child::<PlainText, ApiTester>(
            &body_resource_id(),
            "Request body",
            Some(&state.request.body_text),
            ApiTesterAction::EditBody,
        ),
        request_menu_resource(),
        environment_menu_resource(),
    ];
    let mut budget = PUBLISHED_RECORDS;
    for collection in &state.collections {
        let mut resource = collection_resource(collection);
        for request in collection.requests.iter().take(budget) {
            resource = resource.with_child(request_resource(collection.id, request));
        }
        budget = budget.saturating_sub(collection.requests.len());
        children.push(resource);
    }
    for environment in &state.environments {
        let mut resource = environment_resource(environment);
        for variable in environment.variables.iter().take(budget) {
            resource = resource.with_child(variable_resource(environment.id, variable));
        }
        budget = budget.saturating_sub(environment.variables.len());
        children.push(resource);
    }
    let mut history = Resource::new(
        "api_tester.history",
        "api_tester.history",
        "Request history",
    );
    for entry in state.history.iter().take(budget) {
        history = history.with_child(history_resource(entry));
    }
    children.push(history);
    children
}

/// The record `id`, published or not.
/// Position in one immutable authoritative projection.
#[derive(Clone, Copy, Debug)]
pub(crate) enum RecordLocation {
    Collection(usize),
    Request(usize, usize),
    Environment(usize),
    Variable(usize, usize),
    History(usize),
}

pub(crate) fn resource_index(
    state: &Workspace,
) -> std::collections::HashMap<String, RecordLocation> {
    let mut index = std::collections::HashMap::new();
    for (ci, collection) in state.collections.iter().enumerate() {
        index.insert(
            collection_resource_id(collection.id),
            RecordLocation::Collection(ci),
        );
        for (ri, request) in collection.requests.iter().enumerate() {
            index.insert(
                request_resource_id(request.id),
                RecordLocation::Request(ci, ri),
            );
        }
    }
    for (ei, environment) in state.environments.iter().enumerate() {
        index.insert(
            environment_resource_id(environment.id),
            RecordLocation::Environment(ei),
        );
        for (vi, variable) in environment.variables.iter().enumerate() {
            index.insert(
                variable_resource_id(environment.id, &variable.id),
                RecordLocation::Variable(ei, vi),
            );
        }
    }
    for (hi, entry) in state.history.iter().enumerate() {
        index.insert(history_resource_id(entry.id), RecordLocation::History(hi));
    }
    index
}

pub(crate) fn resolve_child(state: &Workspace, id: &str) -> Option<Resource> {
    #[cfg(test)]
    state.exact_inspected.set(0);
    let location = state.index.get(id)?;
    #[cfg(test)]
    state.exact_inspected.set(state.exact_inspected.get() + 1);
    match *location {
        RecordLocation::Collection(ci) => state.collections.get(ci).map(collection_resource),
        RecordLocation::Request(ci, ri) => {
            let collection = state.collections.get(ci)?;
            collection
                .requests
                .get(ri)
                .map(|request| request_resource(collection.id, request))
        }
        RecordLocation::Environment(ei) => state.environments.get(ei).map(environment_resource),
        RecordLocation::Variable(ei, vi) => {
            let environment = state.environments.get(ei)?;
            environment
                .variables
                .get(vi)
                .map(|variable| variable_resource(environment.id, variable))
        }
        RecordLocation::History(hi) => state.history.get(hi).map(history_resource),
    }
}

/// What system search finds in the workspace: records by their names and
/// routes, never a body, a credential or a variable's value. Each hit opens
/// its record's main action.
pub(crate) fn search_entries(state: &Workspace) -> Vec<SearchEntry> {
    let hit = |id: String, title: String, action: &str| {
        SearchEntry::new(title, String::new())
            .anchored(id.clone())
            .invoking(tilo_actions::ActionAddress::new(id, action))
    };
    let mut entries = Vec::new();
    for collection in &state.collections {
        entries.push(hit(
            collection_resource_id(collection.id),
            collection.name.clone(),
            "rename",
        ));
        for request in &collection.requests {
            entries.push(hit(
                request_resource_id(request.id),
                request_label(request),
                "open",
            ));
        }
    }
    for environment in &state.environments {
        entries.push(hit(
            environment_resource_id(environment.id),
            environment.name.clone(),
            "activate",
        ));
        for variable in &environment.variables {
            entries.push(hit(
                variable_resource_id(environment.id, &variable.id),
                format!("Variable {}", variable.key),
                "rename",
            ));
        }
    }
    for entry in &state.history {
        entries.push(hit(
            history_resource_id(entry.id),
            history_label(entry),
            "replay",
        ));
    }
    entries
}

fn request_label(request: &Request) -> String {
    format!("{} {}", request.method.as_str(), request.name)
}

fn collection_resource(collection: &Collection) -> Resource {
    let id = collection.id;
    Resource::new(
        collection_resource_id(id),
        "api_tester.collection",
        &collection.name,
    )
    .with_action(prompted(
        "rename",
        "Rename collection",
        "Collection name",
        "new name…",
        move |name| rename_collection_action(id, name),
    ))
    .with_action(immediate(
        "delete",
        "Delete collection",
        &delete_collection_action(id),
    ))
    .with_action(prompted(
        "add-request",
        "Add request",
        "Request name",
        "e.g. List users",
        move |name| {
            let mut request = Request::new(name);
            request.id = Uuid::new_v4();
            save_request_action(id, request)
        },
    ))
    .with_action(prompted(
        "save-active",
        "Save current request here",
        "Request name",
        "e.g. List users",
        move |name| save_current_request_action(id, Uuid::new_v4(), name),
    ))
}

fn request_resource(collection_id: Uuid, request: &Request) -> Resource {
    let id = request.id;
    let mut resource = Resource::new(
        request_resource_id(id),
        "api_tester.request",
        request_label(request),
    )
    .with_action(immediate(
        "open",
        "Open in builder",
        &ApiTesterAction::OpenRequest {
            request: request.clone(),
        },
    ))
    .with_action(prompted(
        "rename",
        "Rename request",
        "Request name",
        "new name…",
        move |name| rename_request_action(collection_id, id, name),
    ))
    .with_action(immediate(
        "delete",
        "Delete request",
        &delete_request_action(collection_id, id),
    ));
    // Sending this request is the workspace's send-saved effect, bound to it.
    let send = ApiTesterEffect::SendSaved {
        request: id.to_string(),
    };
    if let Ok(spec) = ActionSpec::operation(
        "send",
        "Send request",
        OperationKind::Effect,
        <ApiTesterEffect as Effect>::OPERATION,
        &serde_json::json!({ "effect": send.id(), "args": send.arguments() }),
    ) {
        resource = resource.with_action(
            spec.with_description("Send this saved request using the active environment")
                .execution(ExecutionMode::Background { cancellable: false })
                .returns(ResultSpec::Job),
        );
    }
    resource
}

fn environment_resource(environment: &Environment) -> Resource {
    let id = environment.id;
    Resource::new(
        environment_resource_id(id),
        "api_tester.environment",
        &environment.name,
    )
    .with_action(immediate(
        "activate",
        format!("Use environment “{}”", environment.name),
        &set_active_environment_action(Some(id)),
    ))
    .with_action(prompted(
        "rename",
        "Rename environment",
        "Environment name",
        "new name…",
        move |name| rename_environment_action(id, name),
    ))
    .with_action(immediate(
        "delete",
        "Delete environment",
        &delete_environment_action(id),
    ))
    .with_action(prompted(
        "set-variable",
        "Set variable",
        "key=value",
        "e.g. base_url=https://api.example.com",
        move |arg| {
            let (key, value) = arg.split_once('=').unwrap_or((arg, ""));
            set_variable_action(
                id,
                Uuid::new_v4().to_string(),
                key.trim(),
                value.trim(),
                false,
            )
        },
    ))
}

/// One variable. A secret variable's value is set on the device
/// ([`ApiTesterEffect::SetSecretValue`]), never through the document.
fn variable_resource(environment: Uuid, variable: &KeyValue) -> Resource {
    let rename_id = variable.id.clone();
    let mut resource = Resource::new(
        variable_resource_id(environment, &variable.id),
        "api_tester.variable",
        format!("Variable {}", variable.key),
    )
    .with_action(prompted(
        "rename",
        "Rename variable",
        "Variable key",
        "new key…",
        move |key| rename_variable_action(environment, rename_id.clone(), key),
    ))
    .with_action(immediate(
        "delete",
        "Delete variable",
        &delete_variable_action(environment, variable.id.clone()),
    ));
    if !variable.secret {
        let set_id = variable.id.clone();
        let key = variable.key.clone();
        resource = resource.with_action(ActionSpec::prompted(
            "set-value",
            "Set variable value",
            "Variable value",
            "new value…",
            ExecutionLocus::ClientLocal,
            move |value: &str| {
                encode(&set_variable_action(
                    environment,
                    set_id.clone(),
                    &key,
                    value,
                    false,
                ))
            },
        ));
    }
    resource
}

fn history_resource(entry: &HistoryEntry) -> Resource {
    Resource::new(
        history_resource_id(entry.id),
        "api_tester.history_entry",
        history_label(entry),
    )
    .with_action(immediate(
        "replay",
        "Open request in builder",
        &ApiTesterAction::OpenHistory {
            history_id: entry.id.to_string(),
        },
    ))
}

/// Preserve enough route context to find a history entry without indexing URL
/// userinfo, query values, or fragments. A malformed URL gets a truthful but
/// non-sensitive fallback instead of leaking its raw text into the catalogue.
fn history_label(entry: &HistoryEntry) -> String {
    let Some((scheme, remainder)) = entry.url.split_once("://") else {
        return format!(
            "{} request returned status {}",
            entry.method.as_str(),
            entry.status
        );
    };
    let safe = remainder.split(['?', '#']).next().unwrap_or_default();
    let Some((authority, path)) = safe.split_once('/') else {
        let authority = safe.rsplit('@').next().unwrap_or_default();
        return format!(
            "{} {scheme}://{authority} returned status {}",
            entry.method.as_str(),
            entry.status
        );
    };
    let authority = authority.rsplit('@').next().unwrap_or_default();
    format!(
        "{} {scheme}://{authority}/{path} returned status {}",
        entry.method.as_str(),
        entry.status
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use tilo_actions::{ActionInput, LocalTransport};
    use ui::tilo_resource::Resource as _;

    fn registry() -> ActionRegistry {
        ActionRegistry::with(register)
    }

    fn apply(doc: &mut AutoCommit, action: &ApiTesterAction) {
        let encoded = encode(action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
    }

    /// A workspace with one collection holding one saved request, and one
    /// environment holding one variable, built through the actions.
    fn seeded() -> AutoCommit {
        let mut doc = AutoCommit::new();
        ApiTester::initialize(&mut doc).unwrap();
        let mut collection = Collection::new("Users API");
        let mut request = Request::new("List users");
        request.url = "https://api.example.com/users".into();
        collection.requests.push(request);
        let mut environment = Environment::new("Production");
        environment
            .variables
            .push(KeyValue::new("base_url", "https://api.example.com"));
        for action in workspace_actions(
            &[collection],
            &[environment],
            &crate::resource::blank_request(),
            &[],
        ) {
            apply(&mut doc, &action);
        }
        doc.commit();
        doc
    }

    fn state(doc: &AutoCommit) -> Workspace {
        ApiTester::read(doc).unwrap()
    }

    fn published(doc: &AutoCommit) -> Vec<Resource> {
        vec![ui::tilo_resource::publish::<ApiTester>(
            ApiTester::KIND,
            ApiTester::LABEL,
            &state(doc),
        )]
    }

    fn collection_id(doc: &AutoCommit) -> Uuid {
        state(doc).collections[0].id
    }

    fn request(doc: &AutoCommit) -> Request {
        state(doc).collections[0].requests[0].clone()
    }

    fn environment_id(doc: &AutoCommit) -> Uuid {
        state(doc).environments[0].id
    }

    fn variable_id(doc: &AutoCommit) -> String {
        state(doc).environments[0].variables[0].id.clone()
    }

    ui::action_contract_tests! {
        seed: seeded(),
        registry: registry(),
        resources: |doc| published(doc),
        palette: [
            "New collection",
            "Delete collection",
            "Delete request",
            "Open in builder",
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
            set_method => |_| encode(&ApiTesterAction::SetMethod { method: HttpMethod::Put }),
        ],
        undo: [
            delete_request => |doc| encode(&delete_request_action(collection_id(doc), request(doc).id)),
            open_request => |doc| encode(&ApiTesterAction::OpenRequest { request: request(doc) }),
        ],
        encodes: [
            delete_collection => ("api_tester.collection", "delete", |doc, _| {
                encode(&delete_collection_action(collection_id(doc)))
            }),
            rename_collection => ("api_tester.collection", "rename", arg: "Accounts API", |doc, _| {
                encode(&rename_collection_action(collection_id(doc), "Accounts API"))
            }),
            open_request => ("api_tester.request", "open", |doc, _| {
                encode(&ApiTesterAction::OpenRequest { request: request(doc) })
            }),
            delete_request => ("api_tester.request", "delete", |doc, _| {
                encode(&delete_request_action(collection_id(doc), request(doc).id))
            }),
            rename_request => ("api_tester.request", "rename", arg: "All users", |doc, _| {
                encode(&rename_request_action(collection_id(doc), request(doc).id, "All users"))
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
                    false,
                ))
            }),
        ],
    }

    /// The workspace publishes its texts, menus and records in one tree; a
    /// record past the published page still resolves by id, and search finds
    /// records by name without indexing a value.
    #[test]
    fn the_workspace_publishes_its_records_and_resolves_late_ones() {
        let doc = seeded();
        let root = &published(&doc)[0];
        let kinds: Vec<&str> = root.children.iter().map(|c| c.kind.as_str()).collect();
        for kind in [
            "api_tester.request_menu",
            "api_tester.environment_menu",
            "api_tester.collection",
            "api_tester.environment",
            "api_tester.history",
        ] {
            assert!(kinds.contains(&kind), "{kind}: {kinds:?}");
        }
        for id in [url_resource_id(), body_resource_id()] {
            assert!(root.children.iter().any(|child| child.id.as_str() == id));
        }
        let actions: Vec<&str> = root.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(actions.contains(&"new-collection") && actions.contains(&"new-environment"));
        let request = &root
            .children
            .iter()
            .find(|c| c.kind == "api_tester.collection")
            .unwrap()
            .children[0];
        let send = request.actions.iter().find(|a| a.id == "send").unwrap();
        assert_eq!(send.kind, OperationKind::Effect);
        let ActionInput::Immediate(payload) = &send.input else {
            panic!("send is bound to its request");
        };
        let effect = ApiTesterEffect::from_payload(&payload.payload).unwrap();
        assert_eq!(
            effect,
            ApiTesterEffect::SendSaved {
                request: state(&doc).collections[0].requests[0].id.to_string()
            }
        );

        let mut large = state(&doc);
        large.collections[0].requests = (0..PUBLISHED_RECORDS + 10)
            .map(|index| Request::new(format!("Request {index}")))
            .collect();
        large.index = resource_index(&large);
        let late = large.collections[0].requests.last().unwrap().id;
        let published = ui::tilo_resource::publish::<ApiTester>(ApiTester::KIND, "", &large);
        let collection = published
            .children
            .iter()
            .find(|c| c.kind == "api_tester.collection")
            .unwrap();
        assert_eq!(
            collection.children.len(),
            PUBLISHED_RECORDS,
            "a bounded page"
        );
        let resolved = ApiTester::child(&large, &request_resource_id(late))
            .expect("a late request resolves by id");
        assert!(resolved.actions.iter().any(|action| action.id == "send"));

        let mut secret = state(&doc);
        secret.environments[0].variables[0].value = "do-not-index".into();
        let entries = ApiTester::search(&secret);
        assert!(entries.iter().any(|entry| entry.title == "Users API"));
        assert!(
            entries
                .iter()
                .any(|entry| entry.title == "Variable base_url")
        );
        assert!(
            entries
                .iter()
                .all(|entry| !entry.title.contains("do-not-index")
                    && !entry.text.contains("do-not-index"))
        );
    }

    /// A credential never reaches the document: a saved or opened request's
    /// auth and a secret variable's value are stored without it.
    #[test]
    fn credentials_never_enter_the_document() {
        let mut doc = seeded();
        let mut request = Request::new("Private");
        request.auth = Auth::Bearer {
            token: "sk-live-credential".into(),
        };
        let collection = collection_id(&doc);
        apply(&mut doc, &save_request_action(collection, request.clone()));
        apply(&mut doc, &ApiTesterAction::OpenRequest { request });
        apply(
            &mut doc,
            &ApiTesterAction::SetAuth {
                auth: Auth::Basic {
                    username: "ada".into(),
                    password: "sk-live-password".into(),
                },
            },
        );
        let environment = environment_id(&doc);
        apply(
            &mut doc,
            &ApiTesterAction::SetVariable {
                env_id: environment.to_string(),
                variable_id: "token".into(),
                key: "token".into(),
                value: "sk-live-variable".into(),
                secret: true,
            },
        );
        let saved = String::from_utf8_lossy(&doc.save()).to_string();
        for secret in ["sk-live-credential", "sk-live-password", "sk-live-variable"] {
            assert!(!saved.contains(secret), "{secret} reached the document");
        }
        let state = state(&doc);
        assert_eq!(
            state.request.auth,
            Auth::Basic {
                username: "ada".into(),
                password: String::new()
            }
        );
        let token = state.environments[0]
            .variables
            .iter()
            .find(|variable| variable.key == "token")
            .unwrap();
        assert!(token.secret && token.value.is_empty());
    }

    /// The builder's edits change the request in it, and record edits
    /// address their target by id.
    #[test]
    fn edits_change_the_builder_and_records_in_place() {
        let mut doc = seeded();
        apply(
            &mut doc,
            &ApiTesterAction::SetMethod {
                method: HttpMethod::Delete,
            },
        );
        apply(
            &mut doc,
            &ApiTesterAction::EditUrl(TextEdit::Splice {
                at: 0,
                delete: 0,
                insert: "https://x.test".into(),
            }),
        );
        let row = KeyValue::new("page", "1");
        apply(
            &mut doc,
            &ApiTesterAction::SetRow {
                section: RowSection::Params,
                row: row.clone(),
            },
        );
        apply(
            &mut doc,
            &ApiTesterAction::SetBodyMode {
                mode: BodyMode::Json,
            },
        );
        let request = state(&doc).request;
        assert_eq!(request.method, HttpMethod::Delete);
        assert_eq!(request.url, "https://x.test");
        assert_eq!(request.params, vec![row.clone()]);
        assert_eq!(request.body_mode, BodyMode::Json);
        apply(
            &mut doc,
            &ApiTesterAction::EditRowField {
                section: RowSection::Params,
                row_id: row.id.clone(),
                field: "key".into(),
                value: "limit".into(),
            },
        );
        assert_eq!(
            state(&doc).request.params[0].value,
            "1",
            "editing the key preserves the current value"
        );
        apply(
            &mut doc,
            &ApiTesterAction::EditRowField {
                section: RowSection::Params,
                row_id: row.id.clone(),
                field: "value".into(),
                value: "50".into(),
            },
        );
        assert_eq!(
            state(&doc).request.params[0].key,
            "limit",
            "editing the value preserves the prior field edit"
        );
        apply(
            &mut doc,
            &ApiTesterAction::RemoveRow {
                section: RowSection::Params,
                row_id: row.id,
            },
        );
        assert!(state(&doc).request.params.is_empty());

        check_history_variables_and_saved_request_updates(&mut doc);
    }

    fn check_history_variables_and_saved_request_updates(doc: &mut AutoCommit) {
        let entry = HistoryEntry {
            id: Uuid::from_u128(7),
            method: HttpMethod::Patch,
            url: "https://api.example.com/users/7".into(),
            status: 204,
            duration_ms: 3,
            when: "just now".into(),
        };
        apply(
            doc,
            &ApiTesterAction::RecordHistory {
                entry: entry.clone(),
            },
        );
        apply(
            doc,
            &ApiTesterAction::OpenHistory {
                history_id: entry.id.to_string(),
            },
        );
        let request = state(doc).request;
        assert_eq!(
            (request.method, request.url.as_str()),
            (HttpMethod::Patch, entry.url.as_str())
        );

        // A variable rename keeps its value in its own field.
        let environment = environment_id(doc);
        let variable = variable_id(doc);
        apply(
            doc,
            &rename_variable_action(environment, variable, "api_url"),
        );
        let renamed = &state(doc).environments[0].variables[0];
        assert_eq!(renamed.key, "api_url");
        assert_eq!(renamed.value, "https://api.example.com");

        // Saving a request whose id exists edits it in place.
        let mut edited = request_of(doc);
        edited.method = HttpMethod::Put;
        let collection = collection_id(doc);
        apply(doc, &save_request_action(collection, edited));
        let requests = &state(doc).collections[0].requests;
        assert_eq!(requests.len(), 1, "same id replaces, not appends");
        assert_eq!(requests[0].method, HttpMethod::Put);

        // The current request is saved under a new id and the given name.
        let id = Uuid::from_u128(9);
        apply(
            doc,
            &save_current_request_action(collection, id, "Saved copy"),
        );
        let saved = state(doc).collections[0]
            .requests
            .iter()
            .find(|request| request.id == id)
            .cloned()
            .expect("the current request was saved");
        assert_eq!(saved.name, "Saved copy");
        assert_eq!(saved.url, "https://api.example.com/users/7");
    }

    fn request_of(doc: &AutoCommit) -> Request {
        request(doc)
    }
}
