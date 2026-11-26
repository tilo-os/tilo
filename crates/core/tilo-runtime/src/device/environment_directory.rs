//! The workspace's environments as the system publishes them: any app, agent or script creates
//! one, renames it, grants it a directory and lists them all by an ordinary action call
//! (`tilo.environment/environment#create`, `#rename`, `#grant-dir`, `#list`), whichever language
//! it is written in.
//!
//! The machine a workspace has for a shell, a Browser or a Linux app is the system's, not an
//! app's: the environments are the workspace's own resources, kept where the workspace keeps
//! them. Creating one and changing its record is the shell runtime's work — `ush`'s supervisor
//! owns the records and their verbs, and checks every change against the record's own rules — so
//! this provider keeps no model of its own and no state: it forwards each call through the
//! device's one shell runtime ([`ShellRuntime`]) and answers what it said, refusals included.
//!
//! The record's verbs are the ones an app may ask of it ([`environment_commands::APP_ACTIONS`]):
//! the workspace's environments answer `run` on the device that takes its Linux work, and these
//! answer its records here. Both are the one resource, `environment`, under the one provider,
//! `tilo.environment`.

use std::sync::Arc;

use serde_json::{Map, Value, json};
use tilo_action_services::environment_commands;
use tilo_actions::api::{ActionAddress, ActionDescriptor, PlacedAction};
use tilo_actions::contract::{ActionContract, ActionKind};
use tilo_actions::params::ActionParam;
use tilo_actions::remote::admitted::{offered, top_level};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
};
use tilo_actions::{ActionOutput, InvocationId};

use super::shell_runtime::{ShellRuntime, shell_runtime};

/// The provider the workspace's environments are answered under.
pub const PROVIDER: &str = environment_commands::PROVIDER;
/// The resource they are an action of: the workspace's environments.
pub const RESOURCE: &str = environment_commands::RESOURCE;
/// Make a new environment of the workspace the call is made in.
pub const CREATE: &str = "create";
/// List the workspace's environments.
pub const LIST: &str = "list";
/// The argument naming the environment a record verb acts on.
pub const RECORD: &str = "record";

/// The workspace's environments, as this device answers them: the supervisor runs the records,
/// and this forwards each call to it.
pub(crate) struct Environments {
    /// The daemon's one shell runtime, which every provider asking the supervisor shares.
    runtime: Arc<ShellRuntime>,
}

impl Environments {
    /// The environments of this login's shell runtime.
    pub(crate) fn here() -> Self {
        Self {
            runtime: shell_runtime(),
        }
    }
}

impl ActionProvider for Environments {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let mut list = descriptor(LIST, "List the environments", ActionKind::Read, Vec::new());
        // What the list answers, as its declaration types it: the environments, each as the
        // reading a view follows publishes them.
        list.contract.result = tilo_actions::ResultSpec::Json {
            schema: environments_schema(),
        };
        let mut actions = vec![
            descriptor(
                CREATE,
                "Create an environment",
                ActionKind::Undoable,
                vec![ActionParam::required(
                    "name",
                    tilo_actions::params::ParamType::Text,
                    "What it is called.",
                )],
            ),
            list,
        ];
        for action in environment_commands::APP_ACTIONS {
            actions.push(descriptor(
                action,
                "Change one of the workspace's environments",
                ActionKind::Undoable,
                vec![ActionParam::required(
                    RECORD,
                    tilo_actions::params::ParamType::Text,
                    "The environment, by the key `list` names it.",
                )],
            ));
        }
        let rows: Vec<PlacedAction> = top_level(actions);
        offered(PROVIDER, PROVIDER, rows, &query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        let answered = forwarded(caller, &request).and_then(|(workspace, asked)| {
            self.runtime
                .ask(&workspace, &asked)
                .map(|value| InvocationReceipt {
                    invocation,
                    status: ReceiptStatus::Completed {
                        output: ActionOutput {
                            value: Some(value),
                            resources: Vec::new(),
                        },
                    },
                })
        });
        // Answered as it is asked: the caller reads the environments' answer, never a call that
        // was admitted and left to follow, which this provider keeps no state to be followed by.
        Pending::ready(answered)
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }
}

/// What the supervisor is asked, and in which workspace: the call as its address and arguments.
fn forwarded(caller: &Caller, request: &InvokeRequest) -> Result<(String, Value), ActionCallError> {
    let address = &request.target.address;
    if address.resource != RESOURCE {
        return Err(ActionCallError::UnknownAction);
    }
    let workspace = caller.workspace().ok_or_else(|| ActionCallError::Refused {
        reason: "the environments are the workspace's: this call names none".into(),
    })?;
    let mut arguments = match &request.argument {
        InvokeArgument::Json(arguments) => arguments.clone(),
        InvokeArgument::None => Map::new(),
        other @ InvokeArgument::Text(_) => {
            return Err(ActionCallError::InvalidRequest {
                reason: format!("the environments are asked for as JSON, not {other:?}"),
            });
        }
    };
    let asked = match address.action.as_str() {
        CREATE => {
            let name = text(&mut arguments, "name")?;
            json!({ "op": "create-environment", "name": name })
        }
        LIST => json!({ "op": "list-environments" }),
        action if environment_commands::APP_ACTIONS.contains(&action) => {
            let record = text(&mut arguments, RECORD)?;
            json!({
                "op": "environment-action",
                "record": record,
                "action": action,
                "args": Value::Object(arguments),
            })
        }
        _ => return Err(ActionCallError::UnknownAction),
    };
    Ok((workspace.to_owned(), asked))
}

/// The text `name` carries in `arguments`, taken out of them.
fn text(arguments: &mut Map<String, Value>, name: &str) -> Result<String, ActionCallError> {
    match arguments.remove(name) {
        Some(Value::String(text)) if !text.trim().is_empty() => Ok(text),
        _ => Err(ActionCallError::InvalidRequest {
            reason: format!("the call names no {name}"),
        }),
    }
}

/// The JSON schema of what `list` answers: the workspace's environments, each as the reading a
/// view follows publishes them, and the record a create just made.
fn environments_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["result", "environments"],
        "properties": {
            "result": { "type": "string" },
            "environments": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["key", "name", "tools", "grants"],
                    "properties": {
                        "key": { "type": "string" },
                        "name": { "type": "string" },
                        "tools": { "type": "array", "items": { "type": "string" } },
                        "grants": {
                            "type": "array",
                            "items": { "type": "array", "items": {} },
                        },
                    },
                },
            },
            "created": { "type": ["string", "null"] },
        },
    })
}

/// One action of the workspace's environments, as the catalogue offers it.
fn descriptor(
    action: &str,
    label: &str,
    kind: ActionKind,
    params: Vec<ActionParam>,
) -> ActionDescriptor {
    ActionDescriptor {
        address: ActionAddress::new(RESOURCE, action),
        semantic_identity: None,
        resource_kind: RESOURCE.to_owned(),
        resource_label: "Environments".to_owned(),
        action_label: label.to_owned(),
        keywords: Vec::new(),
        did: None,
        state: tilo_actions::ActionState::default(),
        icon: None,
        description: None,
        locus: tilo_actions::ExecutionLocus::default(),
        kind: tilo_actions::OperationKind::Effect,
        contract: ActionContract {
            kind,
            ..ActionContract::default()
        },
        input: tilo_actions::InputSpec::None,
        params,
        view: None,
    }
}
