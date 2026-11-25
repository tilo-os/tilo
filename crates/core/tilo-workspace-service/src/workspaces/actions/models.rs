//! What a workspace's model providers publish at `workspace/models`: the
//! providers it keeps, their keys sealed in its vault, and the model each kind
//! of work runs on, as actions of the workspace's own tree, reached by a path as
//! every other action is. Each is decided by the workspace's policy on the
//! providers as any call of it is; a key travels in and is never read back.

use tilo_actions::remote::{
    CatalogueRow, WORKSPACE_MODELS, model_providers, workspace_governance_child,
    workspace_governance_resource,
};
use tilo_actions::{ActionKind, ActionParam, ParamType};

use super::Verb;
use crate::workspaces::workspace_stores::Store;

/// The `tilo.models` actions a person asks of the workspace's providers, in
/// catalogue order.
const VERBS: [Verb; 19] = [
    Verb {
        id: model_providers::LIST,
        label: "List model providers",
        check: "",
        kind: ActionKind::Read,
        params: Vec::new,
    },
    Verb {
        id: model_providers::ADD_PROVIDER,
        label: "Add a model provider",
        check: "",
        kind: ActionKind::Undoable,
        params: add_provider_params,
    },
    Verb {
        id: model_providers::SET_KEY,
        label: "Keep a provider's key",
        check: "",
        kind: ActionKind::Undoable,
        params: set_key_params,
    },
    Verb {
        id: model_providers::REMOVE_KEY,
        label: "Remove a provider's key",
        check: "",
        kind: ActionKind::Undoable,
        params: provider_params,
    },
    Verb {
        id: model_providers::REMOVE_PROVIDER,
        label: "Remove a model provider",
        check: "",
        kind: ActionKind::Undoable,
        params: provider_params,
    },
    Verb {
        id: model_providers::ADD_PRESET,
        label: "Add a ready-made provider",
        check: "",
        kind: ActionKind::Undoable,
        params: add_preset_params,
    },
    Verb {
        id: model_providers::SET_ENABLED,
        label: "Offer a provider to agents, or stop",
        check: "",
        kind: ActionKind::Undoable,
        params: set_enabled_params,
    },
    Verb {
        id: model_providers::REFRESH_MODELS,
        label: "Ask a provider for its models",
        check: "",
        kind: ActionKind::Undoable,
        params: provider_params,
    },
    Verb {
        id: model_providers::ADD_MODEL,
        label: "Add a model a provider lists nowhere",
        check: "",
        kind: ActionKind::Undoable,
        params: add_model_params,
    },
    Verb {
        id: model_providers::SET_DEFAULT,
        label: "Choose the model work runs on",
        check: "",
        kind: ActionKind::Undoable,
        params: set_default_params,
    },
    Verb {
        id: model_providers::SET_ASSIGNMENT,
        label: "Choose the model one kind of work runs on",
        check: "",
        kind: ActionKind::Undoable,
        params: set_assignment_params,
    },
    Verb {
        id: model_providers::SET_ASSIGNMENTS,
        label: "Choose one model for several kinds of work",
        check: "",
        kind: ActionKind::Undoable,
        params: set_assignments_params,
    },
    Verb {
        id: model_providers::SET_REASONING,
        label: "Choose how hard a provider's models think",
        check: "",
        kind: ActionKind::Undoable,
        params: set_reasoning_params,
    },
    Verb {
        id: model_providers::BEGIN_SIGN_IN,
        label: "Begin signing in to a provider",
        check: "",
        kind: ActionKind::Undoable,
        params: begin_sign_in_params,
    },
    Verb {
        id: model_providers::WAIT_SIGN_IN,
        label: "Wait for a provider sign-in",
        check: "",
        kind: ActionKind::Read,
        params: sign_in_params,
    },
    Verb {
        id: model_providers::CANCEL_SIGN_IN,
        label: "Stop waiting for a provider sign-in",
        check: "",
        kind: ActionKind::Undoable,
        params: sign_in_params,
    },
    Verb {
        id: model_providers::SEARCH_HUB_MODELS,
        label: "Search Hugging Face for models this computer runs",
        check: "",
        kind: ActionKind::Read,
        params: search_hub_params,
    },
    Verb {
        id: model_providers::SHOW_HUB_MODEL,
        label: "Describe a Hugging Face model this computer runs",
        check: "",
        kind: ActionKind::Read,
        params: show_hub_params,
    },
    Verb {
        id: model_providers::INSTALL_HUB_MODEL,
        label: "Download a Hugging Face model to this computer",
        check: "",
        kind: ActionKind::Undoable,
        params: install_hub_params,
    },
];

/// Each verb and the `tilo.models` action it is.
const MODEL_VERBS: [(&Verb, &str); 19] = [
    (&VERBS[0], model_providers::LIST),
    (&VERBS[1], model_providers::ADD_PROVIDER),
    (&VERBS[2], model_providers::SET_KEY),
    (&VERBS[3], model_providers::REMOVE_KEY),
    (&VERBS[4], model_providers::REMOVE_PROVIDER),
    (&VERBS[5], model_providers::ADD_PRESET),
    (&VERBS[6], model_providers::SET_ENABLED),
    (&VERBS[7], model_providers::REFRESH_MODELS),
    (&VERBS[8], model_providers::ADD_MODEL),
    (&VERBS[9], model_providers::SET_DEFAULT),
    (&VERBS[10], model_providers::SET_ASSIGNMENT),
    (&VERBS[11], model_providers::SET_ASSIGNMENTS),
    (&VERBS[12], model_providers::SET_REASONING),
    (&VERBS[13], model_providers::BEGIN_SIGN_IN),
    (&VERBS[14], model_providers::WAIT_SIGN_IN),
    (&VERBS[15], model_providers::CANCEL_SIGN_IN),
    (&VERBS[16], model_providers::SEARCH_HUB_MODELS),
    (&VERBS[17], model_providers::SHOW_HUB_MODEL),
    (&VERBS[18], model_providers::INSTALL_HUB_MODEL),
];

fn search_hub_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "query",
            ParamType::Text,
            "What to look for, as Hugging Face searches it.",
        ),
        ActionParam::optional(
            "format",
            ParamType::Text,
            "`gguf` or `safetensors`; `gguf` when none is named.",
        ),
    ]
}

fn show_hub_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "model",
        ParamType::Text,
        "The model's id or page on Hugging Face.",
    )]
}

fn install_hub_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required("repo", ParamType::Text, "The model's repository id."),
        ActionParam::required("revision", ParamType::Text, "The commit to download."),
        ActionParam::required("variant", ParamType::Text, "The file variant to download."),
        ActionParam::optional(
            "acceptLicense",
            ParamType::Boolean,
            "Whether the model's licence is accepted.",
        ),
    ]
}

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

fn set_enabled_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "provider",
            ParamType::Text,
            "The provider's id, as the providers list names it.",
        ),
        ActionParam::required(
            "enabled",
            ParamType::Boolean,
            "Whether its models are offered to the workspace's agents.",
        ),
    ]
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

/// The parameters the model providers' action `action` (a `tilo.models` action, or a verb of
/// them in a workspace's governance tree) is published with.
pub(super) fn params_of(action: &str) -> Option<Vec<ActionParam>> {
    MODEL_VERBS
        .iter()
        .find(|(verb, named)| verb.id == action || *named == action)
        .map(|(verb, _)| (verb.params)())
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
                if action == model_providers::LIST
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
        asking.target = model_providers::target(action);
        self.model_providers_invoke(workspace_id, caller, certificate, &asking)
            .await
    }
}

#[cfg(test)]
mod surface_tests {
    use super::*;

    /// The workspace's model providers as a view of another language reads and changes them
    /// (`models.models`): the reading's fields, the records it holds and the verbs a view calls.
    /// It is the file beside this crate that `tilolang check` compiles a view against and a host
    /// loads the package from; `UPDATE_SNAPSHOTS=1` writes it from this one source.
    #[allow(
        clippy::too_many_lines,
        reason = "one reading's whole surface: its fields, its records and their fields, in one place \
                  a reviewer reads against the types"
    )]
    #[must_use]
    pub fn surface() -> tilo_actions::surface::Surface {
        use tilo_actions::field_type::{FieldType, NamedType, TypeCase, TypeField};
        use tilo_actions::surface::{FieldSurface, Surface};

        let reading = |path: &str, ty: FieldType, title: &str| FieldSurface {
            path: path.to_owned(),
            owner: String::new(),
            title: title.to_owned(),
            stored: "value".to_owned(),
            ty,
            readonly: true,
            unit: false,
            label: false,
            whole: false,
            ops: None,
        };
        let named = FieldType::Named;
        let list = |ty: FieldType| FieldType::List(Box::new(ty));
        let optional = |ty: FieldType| FieldType::Option(Box::new(ty));
        let record = |name: &str, doc: &str, fields: &[(&str, FieldType, &str)]| NamedType {
            name: name.to_owned(),
            doc: doc.to_owned(),
            fields: fields
                .iter()
                .map(|(name, ty, title)| TypeField {
                    name: (*name).to_owned(),
                    ty: ty.clone(),
                    title: (*title).to_owned(),
                })
                .collect(),
            cases: Vec::new(),
        };
        // One case per value `Protocol` serialises as: a view that adds a provider names one.
        let protocol = NamedType {
            name: "Protocol".to_owned(),
            doc: "How a provider is spoken to over HTTP.".to_owned(),
            fields: Vec::new(),
            cases: vec![
                TypeCase {
                    name: "open-ai-compatible".to_owned(),
                    label: "OpenAI-compatible".to_owned(),
                    fields: Vec::new(),
                },
                TypeCase {
                    name: "anthropic".to_owned(),
                    label: "Anthropic".to_owned(),
                    fields: Vec::new(),
                },
            ],
        };
        Surface {
            kind: "models".to_owned(),
            label: "Model providers".to_owned(),
            version: 1,
            person: false,
            readers: None,
            verbs: VERBS.iter().map(verb_surface).collect(),
            fields: vec![
                reading(
                    "providers",
                    list(named("ProviderView".to_owned())),
                    "The providers the workspace keeps, in their own order.",
                ),
                reading(
                    "defaultProvider",
                    optional(FieldType::Text),
                    "The provider work runs on when no kind names one.",
                ),
                reading(
                    "assignments",
                    list(named("Assignment".to_owned())),
                    "The provider and model each kind of work runs on.",
                ),
                reading(
                    "presets",
                    list(named("ProviderPreset".to_owned())),
                    "The providers a person can add by name, without signing in.",
                ),
            ],
            types: vec![
                record(
                    "ProviderView",
                    "One provider the workspace keeps, and the models agents may use from it.",
                    &[
                        (
                            "id",
                            FieldType::Text,
                            "The provider's id, as every action names it.",
                        ),
                        ("name", FieldType::Text, "What the person calls it."),
                        (
                            "protocol",
                            named("Protocol".to_owned()),
                            "How it is spoken to.",
                        ),
                        (
                            "endpoint",
                            FieldType::Text,
                            "The base URL every request goes to.",
                        ),
                        (
                            "configured",
                            FieldType::Bool,
                            "Whether the workspace's vault keeps a key for it.",
                        ),
                        (
                            "enabled",
                            FieldType::Bool,
                            "Whether its models are offered to agents.",
                        ),
                        (
                            "signsIn",
                            FieldType::Bool,
                            "Whether it is reached by signing in to an account.",
                        ),
                        (
                            "models",
                            list(named("OfferedModel".to_owned())),
                            "The models it offers.",
                        ),
                        (
                            "defaultModel",
                            optional(FieldType::Text),
                            "The model its agent runs when nothing else is chosen.",
                        ),
                        (
                            "reasoning",
                            optional(FieldType::Text),
                            "How hard its models are asked to think, where the person chose.",
                        ),
                        (
                            "headers",
                            list(named("RequestHeader".to_owned())),
                            "Headers sent with every request beside its key.",
                        ),
                    ],
                ),
                record(
                    "OfferedModel",
                    "One model a provider offers.",
                    &[
                        (
                            "id",
                            FieldType::Text,
                            "The model's id, as the provider's API names it.",
                        ),
                        ("label", FieldType::Text, "What it is called."),
                    ],
                ),
                record(
                    "RequestHeader",
                    "A header a provider is sent with every request: a setting, never its key.",
                    &[
                        ("name", FieldType::Text, "The header's name."),
                        ("value", FieldType::Text, "Its value."),
                    ],
                ),
                record(
                    "Assignment",
                    "The provider and model one kind of work runs on.",
                    &[
                        ("kind", FieldType::Text, "The kind of work."),
                        ("provider", FieldType::Text, "The provider's id."),
                        ("model", FieldType::Text, "The model's id."),
                    ],
                ),
                record(
                    "ProviderPreset",
                    "A provider the catalogue knows, which a person may add by its name.",
                    &[
                        ("preset", FieldType::Text, "The name `add-preset` takes."),
                        ("name", FieldType::Text, "What people call it."),
                        ("description", FieldType::Text, "What it is, in one line."),
                        (
                            "protocol",
                            named("Protocol".to_owned()),
                            "How it is spoken to.",
                        ),
                    ],
                ),
                protocol,
            ],
            shows: Vec::new(),
            live: None,
            setters: false,
        }
    }

    /// One of the models actions as a view of another language calls it: its verb, what it takes
    /// and what it is for.
    fn verb_surface(verb: &Verb) -> tilo_actions::surface::VerbSurface {
        use tilo_actions::field_type::FieldType;
        use tilo_actions::surface::{ParamSurface, VerbSurface};

        let params = (verb.params)()
            .into_iter()
            .map(|param| {
                let ty = match (param.ty, param.name.as_ref()) {
                    (ParamType::Text | ParamType::DateTime, _) => FieldType::Text,
                    (ParamType::Secret, _) => FieldType::Secret,
                    (ParamType::Integer, _) => FieldType::Int,
                    (ParamType::Number, _) => FieldType::Float,
                    (ParamType::Boolean, _) => FieldType::Bool,
                    // A provider's headers are records of their own; every other list is of text.
                    (ParamType::Array, "headers") => {
                        FieldType::List(Box::new(FieldType::Named("RequestHeader".to_owned())))
                    }
                    (ParamType::Array, _) => FieldType::List(Box::new(FieldType::Text)),
                    (ParamType::Object | ParamType::Resource, _) => FieldType::Any,
                };
                ParamSurface {
                    name: param.name.into_owned(),
                    required: param.required,
                    schema: param.schema.as_deref().cloned().unwrap_or_default(),
                    ty,
                    within: None,
                }
            })
            .collect();
        VerbSurface {
            id: verb.id.to_owned(),
            name: String::new(),
            label: verb.label.to_owned(),
            description: None,
            kind: verb.kind,
            destructive: false,
            incoming_message: false,
            params,
            returns: None,
            writes: vec!["record".to_owned()],
            field: None,
            tags: crate::workspaces::actions::tags_of(verb.id),
            outside: Vec::new(),
            notice: false,
            origin: None,
        }
    }

    /// The file a tilolang view compiles the workspace's model providers against is what this
    /// crate serves. `UPDATE_SNAPSHOTS=1` writes it.
    #[test]
    fn the_models_surface_file_is_what_this_crate_publishes() {
        let text =
            tilo_surface_compile::canonical(&serde_json::json!({ "resources": [surface()] }));
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("models-surface.json");
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|value| value == "1") {
            std::fs::write(&path, &text).expect("the surface file is written");
            return;
        }
        assert_eq!(
            std::fs::read_to_string(&path).ok().as_deref(),
            Some(text.as_str()),
            "crates/core/tilo-workspace-service/models-surface.json is not what this crate \
             publishes; regenerate it with UPDATE_SNAPSHOTS=1"
        );
    }

    /// Every verb the service answers is in the file, and nothing else is.
    #[test]
    fn the_surface_names_every_models_verb() {
        let surface = surface();
        let ids: Vec<&str> = surface.verbs.iter().map(|verb| verb.id.as_str()).collect();
        let served: Vec<&str> = MODEL_VERBS.iter().map(|(_, action)| *action).collect();
        assert_eq!(ids, served);
    }
}
