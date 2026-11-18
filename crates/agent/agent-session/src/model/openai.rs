//! The model an agent works with on a host, and the endpoint a trusted host
//! was given.
//!
//! An endpoint's turns go through the model gateway's trusted-host client
//! ([`CompletionClient`]), which resolves the key and encodes the provider's
//! wire format. A call still running after the hedge delay
//! ([`crate::profile::HEDGE_ENV`], off unless set) is sent again, and
//! whichever answers first is the answer.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use tilo_lock::MutexRecover as _;
use tilo_model_gateway::host_backend::protocol::{
    ErrorKind, ModelError, ModelRequest, ModelResponse,
};
use tilo_model_gateway::host_backend::{CompletionClient, ProviderProtocol};
use tilo_model_gateway::openai::Target as ApiTarget;

use super::turns::{Attempt, PlanTurns, record_usage};
use super::{ApiKey, GatewayTurns, MakeTurns, ScriptedTurns};

/// The model Tilo Cloud meters when `cloud:` names no other.
pub const CLOUD_MODEL: &str = "gpt-4.1-mini-2025-04-14";

/// Turns pictures on (`1`, `on`) or off (`0`, `off`) for an endpoint's
/// model, whatever its provider's default ([`PlanTurns::sees`]).
pub const VISION_ENV: &str = "TILO_AGENT_VISION";

/// The turns of a model behind an OpenAI-compatible endpoint with tool calls
/// (`OpenAI`, vLLM, llama.cpp, Ollama, LM Studio, `OpenRouter`, Tilo Cloud's
/// metered machine endpoint) or Anthropic's Messages API: the trusted host's
/// own client, called from this process with the key it was given, each
/// kept between turns so later ones reuse its connection.
struct EndpointTurns {
    endpoint: String,
    model: String,
    api_key: ApiKey,
    target: ApiTarget,
    profile: crate::profile::Profile,
    /// The wire the endpoint speaks.
    protocol: ProviderProtocol,
    /// Whether it is shown pictures, set outright ([`VISION_ENV`]); `None`
    /// leaves its default.
    vision: Option<bool>,
    /// Clients not carrying a call now. A hedge needs a second one, and a
    /// call that lost to it hands its client back when it ends.
    idle: Arc<Mutex<Vec<CompletionClient>>>,
}

impl EndpointTurns {
    /// An idle client, or a new one with the profile's hints.
    fn client(&self) -> Result<CompletionClient, ModelError> {
        let idle = self.idle.lock_recover().pop();
        if let Some(client) = idle {
            return Ok(client);
        }
        let mut hints = serde_json::json!({});
        self.profile.apply(&mut hints);
        let mut client = CompletionClient::new()?
            .with_request_hints(&hints)
            .with_protocol(self.protocol);
        if let Some(param) = self.profile.token_limit_param() {
            client = client.with_token_limit_param(param);
        }
        if let Some(api) = self.profile.openai_api() {
            client = client.with_openai_api(api);
        }
        Ok(client)
    }

    /// The service tier the profile asks for, as the usage log names it.
    fn tier(&self) -> Option<String> {
        match self.protocol {
            ProviderProtocol::OpenAiCompatible => self.profile.service_tier().map(str::to_owned),
            ProviderProtocol::Anthropic => None,
        }
    }

    /// One call on a thread of its own, its answer sent to `answers`, its
    /// usage logged as the `attempt` it is, its client handed back.
    fn spawn(
        &self,
        request: &ModelRequest,
        key: String,
        deadline: Instant,
        attempt: Attempt,
        answers: &std::sync::mpsc::Sender<Result<ModelResponse, ModelError>>,
    ) -> Result<(), ModelError> {
        let mut client = self.client()?;
        let call = Call {
            endpoint: self.endpoint.clone(),
            api_key: self.api_key.clone(),
            target: self.target,
            model: self.model.clone(),
            request: request.clone(),
            tier: self.tier(),
        };
        let (idle, answers) = (Arc::clone(&self.idle), answers.clone());
        // The agent's name follows the call into the usage log.
        let name = std::thread::current().name().map(str::to_owned);
        let mut thread = std::thread::Builder::new();
        if let Some(name) = name {
            thread = thread.name(name);
        }
        thread
            .spawn(move || {
                let started = Instant::now();
                let answer = client.complete(
                    &call.endpoint,
                    &call.api_key,
                    call.target,
                    &call.model,
                    &call.request,
                    &key,
                    deadline,
                );
                record_usage(
                    (&call.model, call.tier.as_deref()),
                    &call.request,
                    started.elapsed(),
                    &answer,
                    attempt,
                );
                idle.lock_recover().push(client);
                let _ = answers.send(answer);
            })
            .map(drop)
            .map_err(|_| ModelError::new(ErrorKind::Unavailable))
    }
}

/// What one call on its own thread needs.
struct Call {
    endpoint: String,
    api_key: ApiKey,
    target: ApiTarget,
    model: String,
    request: ModelRequest,
    /// The service tier asked for, for the usage log.
    tier: Option<String>,
}

impl PlanTurns for EndpointTurns {
    fn turn(
        &mut self,
        request: &ModelRequest,
        key: &str,
        deadline: Instant,
        retry: bool,
    ) -> Result<ModelResponse, ModelError> {
        let Some(after) = self.profile.hedge_after() else {
            let mut client = self.client()?;
            let started = Instant::now();
            let answer = client.complete(
                &self.endpoint,
                &self.api_key,
                self.target,
                &self.model,
                request,
                key,
                deadline,
            );
            record_usage(
                (&self.model, self.tier().as_deref()),
                request,
                started.elapsed(),
                &answer,
                Attempt {
                    hedge: false,
                    retry,
                },
            );
            self.idle.lock_recover().push(client);
            return answer;
        };
        let (sent, answers) = std::sync::mpsc::channel();
        self.spawn(
            request,
            key.to_owned(),
            deadline,
            Attempt {
                hedge: false,
                retry,
            },
            &sent,
        )?;
        if let Ok(answer) = answers.recv_timeout(after) {
            return answer;
        }
        // Slower than a call of this model takes nearly always: the same
        // request again under its own key, and whichever answers first is
        // the answer. A failed first answer waits for the other.
        self.spawn(
            request,
            format!("{key}-hedge"),
            deadline,
            Attempt { hedge: true, retry },
            &sent,
        )?;
        drop(sent);
        match answers.recv() {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(error)) => answers.recv().unwrap_or(Err(error)),
            Err(_) => Err(ModelError::new(ErrorKind::Provider)),
        }
    }

    fn model(&self) -> &str {
        &self.model
    }

    /// `OpenAI`'s own endpoint, Anthropic's and Tilo Cloud take pictures;
    /// another server (a local model) is assumed not to unless it is said to
    /// ([`VISION_ENV`]).
    fn sees(&self) -> bool {
        self.vision.unwrap_or_else(|| {
            self.target == ApiTarget::CloudMetered
                || self.protocol == ProviderProtocol::Anthropic
                || self.endpoint.starts_with("https://api.openai.com/")
        })
    }
}

/// Which model an agent works with on a host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelChoice {
    /// The provider selected in Settings, through `tilo-model-gateway`.
    Settings,
    /// A scripted model: it plays the tool calls a task writes out and calls
    /// no network ([`ScriptedTurns`]).
    Scripted,
    /// A model behind an OpenAI-compatible endpoint at a base URL.
    OpenAi {
        /// `…/v1`.
        base_url: String,
        /// Model id.
        model: String,
        /// Bearer key, if the endpoint wants one.
        api_key: ApiKey,
        /// A plain provider, or Tilo Cloud's metered machine endpoint.
        target: ApiTarget,
    },
    /// Claude through Anthropic's Messages API.
    Anthropic {
        /// The API's origin (`https://api.anthropic.com`).
        base_url: String,
        /// Model id.
        model: String,
        /// The key, which Anthropic always wants.
        api_key: ApiKey,
    },
}

impl ModelChoice {
    /// `settings`, `scripted`, `openai:<base url>`, `cloud:<base url>`
    /// (Tilo Cloud's metered `…/api/machines/<id>/ai/v1`) or
    /// `anthropic:<base url>` (Anthropic's Messages API), with the model
    /// and key given separately (`--agent-model-name`; `TILO_AGENT_API_KEY`
    /// or `TILO_AGENT_API_KEY_FILE`). `cloud:` defaults to [`CLOUD_MODEL`]
    /// and needs a key; `anthropic:` needs a model and a key.
    pub fn parse(spec: &str, model: Option<&str>, api_key: ApiKey) -> Result<Self, String> {
        let model = model.filter(|model| !model.is_empty());
        match spec {
            "settings" => Ok(Self::Settings),
            "scripted" => Ok(Self::Scripted),
            _ if spec.starts_with("anthropic:") => {
                let base_url = spec.strip_prefix("anthropic:").unwrap_or_default();
                if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
                    return Err("expected anthropic:<base url>".into());
                }
                let model = model.ok_or("an anthropic: model needs a model name")?;
                if api_key == ApiKey::None {
                    return Err(
                        "an anthropic: model needs its key in TILO_AGENT_API_KEY or \
                                TILO_AGENT_API_KEY_FILE"
                            .into(),
                    );
                }
                Ok(Self::Anthropic {
                    base_url: base_url.to_owned(),
                    model: model.to_owned(),
                    api_key,
                })
            }
            _ => {
                let web = |url: &&str| url.starts_with("http://") || url.starts_with("https://");
                let (base_url, target) = if let Some(url) = spec.strip_prefix("cloud:") {
                    (url, ApiTarget::CloudMetered)
                } else {
                    (
                        spec.strip_prefix("openai:").unwrap_or_default(),
                        ApiTarget::Standard,
                    )
                };
                if !web(&base_url) {
                    return Err("expected settings, scripted, openai:<base url>, \
                                cloud:<base url> or anthropic:<base url>"
                        .into());
                }
                let model = match target {
                    ApiTarget::CloudMetered => model.unwrap_or(CLOUD_MODEL),
                    ApiTarget::Standard => model.ok_or("an openai: model needs a model name")?,
                };
                if target == ApiTarget::CloudMetered && api_key == ApiKey::None {
                    return Err("a cloud: model needs the machine's AI token in \
                                TILO_AGENT_API_KEY or TILO_AGENT_API_KEY_FILE"
                        .into());
                }
                Ok(Self::OpenAi {
                    base_url: base_url.to_owned(),
                    model: model.to_owned(),
                    api_key,
                    target,
                })
            }
        }
    }

    /// What carries a conversation's turns on this model, holding its credential: the gateway
    /// for the Settings provider, the trusted host's own client for an endpoint it was given,
    /// the script for [`Self::Scripted`]. `None` for Settings with no provider chosen.
    #[must_use]
    pub fn turns(&self) -> Option<Box<dyn PlanTurns>> {
        match self {
            Self::Scripted => Some(Box::new(ScriptedTurns)),
            Self::OpenAi { .. } | Self::Anthropic { .. } => self
                .endpoint_turns()
                .map(|turns| Box::new(turns) as Box<dyn PlanTurns>),
            Self::Settings => tilo_agent::providers::default_selection()
                .map(|selection| Box::new(GatewayTurns::new(selection)) as Box<dyn PlanTurns>),
        }
    }

    /// [`Self::turns`], made afresh on the thread of each agent that asks.
    #[must_use]
    pub fn make_turns(self) -> MakeTurns {
        Arc::new(move || self.turns())
    }

    /// The turns of the endpoint this choice names, with the profile the
    /// process's settings make for its model; `None` for a choice that names
    /// no endpoint.
    fn endpoint_turns(&self) -> Option<EndpointTurns> {
        let env = |name: &str| std::env::var(name).ok();
        let (endpoint, model, api_key, target, protocol) = match self {
            Self::OpenAi {
                base_url,
                model,
                api_key,
                target,
            } => {
                let base = base_url.trim_end_matches('/');
                let endpoint = if base.ends_with("/chat/completions") {
                    base.to_owned()
                } else {
                    format!("{base}/chat/completions")
                };
                (
                    endpoint,
                    model,
                    api_key,
                    *target,
                    ProviderProtocol::OpenAiCompatible,
                )
            }
            Self::Anthropic {
                base_url,
                model,
                api_key,
            } => {
                // The API's origin: a trailing `/v1` or `/v1/messages` is dropped.
                let base = base_url.trim_end_matches('/');
                let origin = base
                    .strip_suffix("/v1/messages")
                    .or_else(|| base.strip_suffix("/v1"))
                    .unwrap_or(base);
                (
                    origin.to_owned(),
                    model,
                    api_key,
                    ApiTarget::Standard,
                    ProviderProtocol::Anthropic,
                )
            }
            Self::Settings | Self::Scripted => return None,
        };
        Some(EndpointTurns {
            endpoint,
            model: model.clone(),
            api_key: api_key.clone(),
            target,
            profile: crate::profile::Profile::resolve(model, &env),
            protocol,
            vision: vision_setting(env(VISION_ENV).as_deref()),
            idle: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// A short description for logs and status.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Settings => "the Settings provider".into(),
            Self::Scripted => "a scripted model".into(),
            Self::OpenAi {
                base_url,
                model,
                target: ApiTarget::CloudMetered,
                ..
            } => format!("{model} on Tilo Cloud credits at {base_url}"),
            Self::OpenAi {
                base_url, model, ..
            }
            | Self::Anthropic {
                base_url, model, ..
            } => format!("{model} at {base_url}"),
        }
    }
}

/// Whether pictures are turned on or off for every endpoint's model
/// ([`VISION_ENV`]), as `setting` reads.
fn vision_setting(setting: Option<&str>) -> Option<bool> {
    match setting?.trim().to_ascii_lowercase().as_str() {
        "1" | "on" | "true" | "yes" => Some(true),
        "0" | "off" | "false" | "no" => Some(false),
        _ => None,
    }
}
