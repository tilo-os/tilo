//! What a workspace's model providers publish at `workspace/models`: the
//! providers it keeps, their keys sealed in its vault, and the model each kind
//! of work runs on, as actions of the workspace's own tree, reached by a path as
//! every other action is. Each is decided by the workspace's policy on the
//! providers as any call of it is; a key travels in and is never read back.

use tilo_actions::remote::{
    CatalogueRow, WORKSPACE_MODELS, person_models, workspace_governance_child,
    workspace_governance_resource,
};
use tilo_actions::{ActionKind, ActionParam, ParamType};

use super::Verb;
use crate::workspaces::workspace_stores::Store;

/// The `tilo.models` actions a person asks of the workspace's providers, in
/// catalogue order.
const VERBS: [Verb; 15] = [
    Verb {
        id: person_models::LIST,
        label: "List model providers",
        check: "",
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: person_models::ADD_PROVIDER,
        label: "Add a model provider",
        check: "",
        kind: ActionKind::Undoable,
        params: add_provider_params,
    },
    Verb {
        id: person_models::SET_KEY,
        label: "Keep a provider's key",
        check: "",
        kind: ActionKind::Undoable,
        params: set_key_params,
    },
    Verb {
        id: person_models::REMOVE_KEY,
        label: "Remove a provider's key",
        check: "",
        kind: ActionKind::Undoable,
        params: provider_params,
    },
    Verb {
        id: person_models::REMOVE_PROVIDER,
        label: "Remove a model provider",
        check: "",
        kind: ActionKind::Undoable,
        params: provider_params,
    },
    Verb {
        id: person_models::ADD_PRESET,
        label: "Add a ready-made provider",
        check: "",
        kind: ActionKind::Undoable,
        params: add_preset_params,
    },
    Verb {
        id: person_models::REFRESH_MODELS,
        label: "Ask a provider for its models",
        check: "",
        kind: ActionKind::Undoable,
        params: provider_params,
    },
    Verb {
        id: person_models::ADD_MODEL,
        label: "Add a model a provider lists nowhere",
        check: "",
        kind: ActionKind::Undoable,
        params: add_model_params,
    },
    Verb {
        id: person_models::SET_DEFAULT,
        label: "Choose the model work runs on",
        check: "",
        kind: ActionKind::Undoable,
        params: set_default_params,
    },
    Verb {
        id: person_models::SET_ASSIGNMENT,
        label: "Choose the model one kind of work runs on",
        check: "",
        kind: ActionKind::Undoable,
        params: set_assignment_params,
    },
    Verb {
        id: person_models::SET_ASSIGNMENTS,
        label: "Choose one model for several kinds of work",
        check: "",
        kind: ActionKind::Undoable,
        params: set_assignments_params,
    },
    Verb {
        id: person_models::SET_REASONING,
        label: "Choose how hard a provider's models think",
        check: "",
        kind: ActionKind::Undoable,
        params: set_reasoning_params,
    },
    Verb {
        id: person_models::BEGIN_SIGN_IN,
        label: "Begin signing in to a provider",
        check: "",
        kind: ActionKind::Undoable,
        params: begin_sign_in_params,
    },
    Verb {
        id: person_models::WAIT_SIGN_IN,
        label: "Wait for a provider sign-in",
        check: "",
        kind: ActionKind::Read,
        params: sign_in_params,
    },
    Verb {
        id: person_models::CANCEL_SIGN_IN,
        label: "Stop waiting for a provider sign-in",
        check: "",
        kind: ActionKind::Undoable,
        params: sign_in_params,
    },
];

/// Each verb and the `tilo.models` action it is.
const MODEL_VERBS: [(&Verb, &str); 15] = [
    (&VERBS[0], person_models::LIST),
    (&VERBS[1], person_models::ADD_PROVIDER),
    (&VERBS[2], person_models::SET_KEY),
    (&VERBS[3], person_models::REMOVE_KEY),
    (&VERBS[4], person_models::REMOVE_PROVIDER),
    (&VERBS[5], person_models::ADD_PRESET),
    (&VERBS[6], person_models::REFRESH_MODELS),
    (&VERBS[7], person_models::ADD_MODEL),
    (&VERBS[8], person_models::SET_DEFAULT),
    (&VERBS[9], person_models::SET_ASSIGNMENT),
    (&VERBS[10], person_models::SET_ASSIGNMENTS),
    (&VERBS[11], person_models::SET_REASONING),
    (&VERBS[12], person_models::BEGIN_SIGN_IN),
    (&VERBS[13], person_models::WAIT_SIGN_IN),
    (&VERBS[14], person_models::CANCEL_SIGN_IN),
];

fn provider_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "provider",
        ParamType::Text,
        "The provider's id, as the providers list names it.",
    )]
}

fn add_provider_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required("name", ParamType::Text, "The name the person gives it."),
        ActionParam::required(
            "protocol",
            ParamType::Text,
            "How it is spoken to: `open-ai-compatible` or `anthropic`.",
        ),
        ActionParam::required(
            "endpoint",
            ParamType::Text,
            "The base URL every request goes to.",
        ),
        ActionParam::optional(
            "headers",
            ParamType::Array,
            "Headers sent with every request beside the key, each with `name` and `value`.",
        ),
    ]
}

fn add_preset_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "preset",
        ParamType::Text,
        "The ready-made provider's name: `anthropic`, `gemini`, `openai` or `openrouter`.",
    )]
}

fn set_key_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "provider",
            ParamType::Text,
            "The provider's id, as the providers list names it.",
        ),
        ActionParam::required(
            "key",
            ParamType::Secret,
            "The API key: sealed in the workspace's vault, never answered back.",
        ),
    ]
}

fn add_model_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required("provider", ParamType::Text, "The provider's id."),
        ActionParam::required(
            "model",
            ParamType::Text,
            "The model's id, as the provider's API names it.",
        ),
    ]
}

fn set_default_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required("provider", ParamType::Text, "The provider's id."),
        ActionParam::required("model", ParamType::Text, "The model's id."),
    ]
}

fn set_assignment_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "kind",
            ParamType::Text,
            "The kind of work: `one_shot`, `task`, `subtask`, `request`, `classifier` or \
             `dictation`.",
        ),
        ActionParam::optional(
            "provider",
            ParamType::Text,
            "The provider's id; both left out, the kind follows the default again.",
        ),
        ActionParam::optional("model", ParamType::Text, "The model's id."),
    ]
}

fn set_assignments_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "kinds",
            ParamType::Array,
            "The kinds of work chosen together; both provider and model left out, they follow \
             the default again.",
        ),
        ActionParam::optional("provider", ParamType::Text, "The provider's id."),
        ActionParam::optional("model", ParamType::Text, "The model's id."),
    ]
}

fn set_reasoning_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required("provider", ParamType::Text, "The provider's id."),
        ActionParam::optional(
            "effort",
            ParamType::Text,
            "How hard its models think: `none`, `minimal`, `low`, `medium`, `high` or `xhigh`; \
             left out, the provider's own default applies again.",
        ),
    ]
}

fn begin_sign_in_params() -> Vec<ActionParam> {
    vec![ActionParam::optional(
        "using",
        ParamType::Text,
        "What the account is used for: `subscription` when left out, or `api-usage`.",
    )]
}

fn sign_in_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "signIn",
        ParamType::Text,
        "The sign-in, as beginning one named it.",
    )]
}

/// The `tilo.models` action `address` asks of a workspace's model providers in
/// its governance tree, and the workspace it names.
pub(super) fn models_action(address: &tilo_actions::ActionAddress) -> Option<(&str, &str)> {
    let workspace = address
        .resource
        .strip_prefix(tilo_actions::remote::workspace_governance_resource("").as_str())?
        .strip_suffix(WORKSPACE_MODELS)?
        .strip_suffix('/')?;
    let action = MODEL_VERBS
        .iter()
        .find(|(verb, _)| verb.id == address.action)
        .map(|(_, action)| *action)?;
    Some((workspace, action))
}

impl crate::workspaces::Workspaces {
    /// The rows of `workspace`'s model providers (its UUID) `asked` is offered:
    /// those the workspace's policy on the providers lets it ask.
    pub(super) async fn models_rows(
        &self,
        asked: super::Asked<'_>,
        workspace: &str,
    ) -> Vec<CatalogueRow> {
        let root = workspace_governance_resource(workspace);
        let resource = workspace_governance_child(workspace, Some(WORKSPACE_MODELS));
        let mut rows = Vec::new();
        for (verb, action) in MODEL_VERBS {
            if self
                .reaching(asked, Store::MODELS, action, verb.kind)
                .await
                .is_ok()
            {
                let mut row = verb.row(super::GOVERNANCE, &resource, "Model providers");
                row.parent_resource.clone_from(&root);
                // Reading `workspace/models` lists the providers.
                if action == person_models::LIST
                    && let Some(listing) = row.action.as_mut()
                {
                    listing
                        .contract
                        .tags
                        .push(tilo_actions::DOCUMENT_READ_TAG.to_owned());
                }
                rows.push(row);
            }
        }
        rows
    }

    /// Run `request`, one of the workspace's model provider actions in its
    /// governance tree, as the `tilo.models` action `action` it is, in the
    /// workspace `workspace` (its UUID) the request came through.
    pub(super) async fn models_invoke(
        &self,
        asked: super::Asked<'_>,
        (workspace, action): (&str, &str),
        request: &tilo_actions::remote::InvokeRequest,
    ) -> Result<serde_json::Value, tilo_actions::remote::ActionCallError> {
        let (workspace_id, caller, certificate) = asked;
        let here = match workspace_id {
            Some(id) => self.workspace_identity(id).await,
            None => None,
        };
        if here.as_deref() != Some(workspace) {
            return Err(tilo_actions::remote::ActionCallError::UnknownAction);
        }
        let mut asking = request.clone();
        asking.target = person_models::target(action);
        self.person_models_invoke(workspace_id, caller, certificate, &asking)
            .await
    }
}
