//! A provider that answers from a script, and a person's model linked
//! without a page: for the tests of whatever asks a person's model.
//!
//! [`ScriptedProvider`] stands where the host's own provider client does
//! ([`Workspaces::answer_models_with`]), so everything before it is the
//! production path: the person's settings, their vault, the choice by kind
//! of work, the refusals. It keeps what it was asked, the key it was handed
//! included, so a test proves whose model and whose key a call used.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use agent_provider_config::protocol::{
    Content, ErrorKind, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason,
    StreamEvent, ToolCall, Usage,
};
use agent_provider_config::{AgentModel, ModelDescriptor, ProviderProfile, ProviderProtocol};
use tilo_lock::MutexRecover as _;
use tilo_workspace_records::models::SetKey;

use super::{Events, Interaction, ModelProvider, ProviderCall, ProviderFuture};
use crate::person_settings::{ProviderKeys, provider_config, save_provider_config, set_key};

/// What a scripted provider says to one call.
#[derive(Clone, Debug)]
pub struct Reply {
    outcome: Outcome,
    cost_micros: u64,
    /// How long the provider takes over each word it writes.
    pause: Option<std::time::Duration>,
}

#[derive(Clone, Debug)]
enum Outcome {
    Says(Vec<Content>),
    Fails(ModelError),
    Never,
}

impl Reply {
    /// An answer in words, streamed a word at a time.
    #[must_use]
    pub fn text(text: &str) -> Self {
        Self::saying(vec![Content::Text {
            text: text.to_owned(),
        }])
    }

    /// An answer that calls tool `name` with `input`.
    #[must_use]
    pub fn tool_call(id: &str, name: &str, input: serde_json::Value) -> Self {
        Self::saying(vec![Content::ToolCall(ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            input,
            extra_content: None,
        })])
    }

    /// An answer of `content`, in order.
    #[must_use]
    pub fn saying(content: Vec<Content>) -> Self {
        Self {
            outcome: Outcome::Says(content),
            cost_micros: 0,
            pause: None,
        }
    }

    /// The provider fails with `error`.
    #[must_use]
    pub fn failing(error: ModelError) -> Self {
        Self {
            outcome: Outcome::Fails(error),
            cost_micros: 0,
            pause: None,
        }
    }

    /// The provider never answers: the call ends by its deadline or by
    /// being stopped.
    #[must_use]
    pub fn never() -> Self {
        Self {
            outcome: Outcome::Never,
            cost_micros: 0,
            pause: None,
        }
    }

    /// This answer, its words written `pause` apart, as a model that
    /// takes its time writes them.
    #[must_use]
    pub const fn pausing(mut self, pause: std::time::Duration) -> Self {
        self.pause = Some(pause);
        self
    }

    /// This answer, costing `micros` millionths of the currency.
    #[must_use]
    pub fn costing(mut self, micros: u64) -> Self {
        self.cost_micros = micros;
        self
    }
}

/// One call a scripted provider was asked.
#[derive(Clone)]
pub struct Asked {
    /// The provider's id in the person's settings.
    pub provider: String,
    /// The provider's address.
    pub endpoint: String,
    /// How it is spoken to.
    pub protocol: ProviderProtocol,
    /// The model asked.
    pub model: String,
    /// The key the call was handed.
    pub key: Option<String>,
    /// What was asked.
    pub request: ModelRequest,
}

impl std::fmt::Debug for Asked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Asked")
            .field("provider", &self.provider)
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("keyed", &self.key.is_some())
            .finish_non_exhaustive()
    }
}

/// A provider that answers each call with the next reply of its script.
#[derive(Default)]
pub struct ScriptedProvider {
    replies: Mutex<VecDeque<Reply>>,
    asked: Mutex<Vec<Asked>>,
}

impl ScriptedProvider {
    /// A provider answering `replies`, one per call, in order. A call past
    /// the script's end fails.
    #[must_use]
    pub fn answering(replies: impl IntoIterator<Item = Reply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into_iter().collect()),
            asked: Mutex::default(),
        })
    }

    /// Add `reply` to the script's end.
    pub fn then(&self, reply: Reply) {
        self.replies.lock_recover().push_back(reply);
    }

    /// Every call so far, in order.
    #[must_use]
    pub fn asked(&self) -> Vec<Asked> {
        self.asked.lock_recover().clone()
    }
}

/// How many words `text` holds: what a script counts tokens by.
fn words(text: &str) -> u64 {
    u64::try_from(text.split_whitespace().count()).unwrap_or(u64::MAX)
}

fn words_of(content: &[Content]) -> u64 {
    content
        .iter()
        .map(|part| match part {
            Content::Text { text } | Content::Reasoning { text, .. } => words(text),
            Content::ToolCall(call) => words(&call.input.to_string()),
            Content::ToolResult(result) => words(&result.content),
            Content::Image { .. } => 0,
        })
        .fold(0, u64::saturating_add)
}

/// `content` as a provider streams it, and the answer it ends in.
fn said(
    request: &ModelRequest,
    content: Vec<Content>,
    cost_micros: u64,
    events: Events<'_>,
) -> ModelResponse {
    let mut calls = 0_u32;
    for part in &content {
        match part {
            Content::Text { text } => {
                for piece in text.split_inclusive(' ') {
                    events(StreamEvent::TextDelta {
                        text: piece.to_owned(),
                    });
                }
            }
            Content::Reasoning { text, .. } => {
                events(StreamEvent::ReasoningDelta { text: text.clone() });
            }
            Content::ToolCall(call) => {
                events(StreamEvent::ToolCallDelta {
                    index: calls,
                    id: Some(call.id.clone()),
                    name: Some(call.name.clone()),
                    arguments: call.input.to_string(),
                });
                calls = calls.saturating_add(1);
            }
            Content::Image { .. } | Content::ToolResult(_) => {}
        }
    }
    let input_tokens = request
        .messages
        .iter()
        .map(|message| words_of(&message.content))
        .fold(0, u64::saturating_add);
    let output_tokens = words_of(&content);
    ModelResponse {
        message: Message {
            role: Role::Assistant,
            content,
        },
        stop_reason: if calls == 0 {
            StopReason::EndTurn
        } else {
            StopReason::ToolCalls
        },
        usage: Usage {
            input_tokens,
            output_tokens,
            total_tokens: input_tokens.saturating_add(output_tokens),
            ..Usage::default()
        },
        cost_micros,
    }
}

impl ModelProvider for ScriptedProvider {
    fn answer<'a>(&'a self, call: ProviderCall<'a>, events: Events<'a>) -> ProviderFuture<'a> {
        self.asked.lock_recover().push(Asked {
            provider: call.profile.id.clone(),
            endpoint: call.profile.endpoint.clone(),
            protocol: call.profile.protocol,
            model: call.model.id.clone(),
            key: call.key().map(str::to_owned),
            request: call.request.clone(),
        });
        let reply = self.replies.lock_recover().pop_front();
        Box::pin(async move {
            let Some(reply) = reply else {
                return Err(ModelError {
                    kind: ErrorKind::InvalidRequest,
                    message: "the script holds no further answer".into(),
                });
            };
            match reply.outcome {
                Outcome::Says(content) => {
                    let Some(pause) = reply.pause else {
                        return Ok(said(call.request, content, reply.cost_micros, events));
                    };
                    for part in &content {
                        if let Content::Text { text } = part {
                            for piece in text.split_inclusive(' ') {
                                tokio::time::sleep(pause).await;
                                events(StreamEvent::TextDelta {
                                    text: piece.to_owned(),
                                });
                            }
                        }
                    }
                    Ok(said(call.request, content, reply.cost_micros, &mut |_| {}))
                }
                Outcome::Fails(error) => Err(error),
                Outcome::Never => std::future::pending().await,
            }
        })
    }
}

/// A provider to keep for a person: what Settings' own actions keep once
/// the provider's models are listed.
#[derive(Clone, Copy)]
pub struct Linking<'a> {
    /// The name the person gives it.
    pub name: &'a str,
    /// How it is spoken to.
    pub protocol: ProviderProtocol,
    /// The address every request goes to.
    pub endpoint: &'a str,
    /// Its API key, kept in the workspace's vault; none keeps no key.
    pub key: Option<&'a str>,
    /// The models it offers. The first is the provider's default.
    pub models: &'a [&'a str],
}

impl crate::PersonServices {
    /// Keep `linking` for `workspace` as its settings keep a provider, its
    /// key an entry of the workspace's vault, and answer its id. With `default`, it is the
    /// provider its work runs on when no kind of work names another.
    ///
    /// # Errors
    /// The settings or the key were not kept.
    pub async fn link_model_for(
        &self,
        workspace: &str,
        linking: &Linking<'_>,
        default: bool,
    ) -> Result<String, String> {
        let root = self.workspace_root(workspace);
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let mut config = provider_config(&root);
        let id = uuid::Uuid::new_v4().to_string();
        config.profiles.push(ProviderProfile {
            id: id.clone(),
            label: linking.name.to_owned(),
            protocol: linking.protocol,
            endpoint: linking.endpoint.to_owned(),
            models: linking
                .models
                .iter()
                .map(|model| ModelDescriptor::new(*model))
                .collect(),
            default_model: linking.models.first().map(|model| (*model).to_owned()),
            ..ProviderProfile::default()
        });
        if default {
            config.default_profile = Some(id.clone());
        }
        save_provider_config(&root, &config)?;
        if let Some(key) = linking.key {
            set_key(
                (ProviderKeys::of(self.vault_at(workspace), ""), &root),
                SetKey {
                    provider: id.clone(),
                    key: key.to_owned(),
                },
                None,
            )
            .await?;
        }
        Ok(id)
    }

    /// List `models` as what `workspace`'s provider `provider` offers, in
    /// place of the listing the host reads from the provider itself.
    pub fn offer_models_for(
        &self,
        workspace: &str,
        provider: &str,
        models: &[&str],
    ) -> Result<(), String> {
        let root = self.workspace_root(workspace);
        let mut config = provider_config(&root);
        let profile = config
            .profiles
            .iter_mut()
            .find(|profile| profile.id == provider)
            .ok_or_else(|| "That provider is not kept here".to_owned())?;
        profile.models = models
            .iter()
            .map(|model| ModelDescriptor::new(*model))
            .collect();
        save_provider_config(&root, &config)
    }

    /// Choose `model` of `workspace`'s provider `provider` for `kind` of
    /// work, as Settings does.
    pub fn choose_model_for(
        &self,
        workspace: &str,
        kind: Interaction,
        provider: &str,
        model: &str,
    ) -> Result<(), String> {
        let root = self.workspace_root(workspace);
        let mut config = provider_config(&root);
        config
            .set_interaction_model(
                kind,
                Some(AgentModel {
                    profile: provider.to_owned(),
                    model: model.to_owned(),
                }),
            )
            .map_err(str::to_owned)?;
        save_provider_config(&root, &config)
    }
}
