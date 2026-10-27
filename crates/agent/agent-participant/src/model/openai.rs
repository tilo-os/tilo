//! A planning model at an endpoint the trusted host was given, and the
//! choice of model an agent plans with.
//!
//! [`OpenAiModel`] plans in the same conversation as
//! [`super::GatewayModel`]; its turns go through the model gateway's
//! trusted-host client ([`CompletionClient`]), which resolves the key and
//! encodes the provider's wire format. A call still running after the
//! hedge delay ([`crate::profile::HEDGE_ENV`], off unless set) is sent
//! again, and whichever answers first is the answer.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_agent::surface::Catalogue;
use tilo_agent::tools::Answer;
use tilo_lock::MutexRecover as _;
use tilo_model_gateway::host_backend::protocol::{
    ErrorKind, ModelError, ModelRequest, ModelResponse,
};
use tilo_model_gateway::host_backend::{CompletionClient, ProviderProtocol};
use tilo_model_gateway::openai::Target as ApiTarget;

use super::converse::{self, Attempt, PlanTurns, record_usage};
use super::{
    ApiKey, Briefing, GatewayModel, ModelPlanner, NoPlanReads, PlanModel, ReadView, ScriptedModel,
};

/// The model Tilo Cloud meters when `cloud:` names no other.
pub const CLOUD_MODEL: &str = "gpt-4.1-mini-2025-04-14";

/// How long one plan may take: several reasoning turns, each of which a
/// real model may spend half a minute on.
const PLAN_TIMEOUT: Duration = Duration::from_secs(600);

/// A model behind an OpenAI-compatible endpoint with tool calls (`OpenAI`,
/// vLLM, llama.cpp, Ollama, LM Studio, `OpenRouter`, Tilo Cloud's metered
/// machine endpoint), planning as [`GatewayModel`] does: through chat
/// completions, or Responses at `OpenAI`'s own API, whose reasoning models
/// call tools only there ([`crate::profile::OPENAI_API_ENV`]). It
/// calls the endpoint from this process with the key it was given: for a
/// host operator's own endpoint, a Cloud machine's host and local
/// development, not a sandboxed app.
pub struct OpenAiModel {
    turns: EndpointTurns,
    timeout: Duration,
    /// Whether it is shown pictures, set outright
    /// ([`crate::vision::VISION_ENV`]); `None` leaves its default.
    vision: Option<bool>,
}

impl OpenAiModel {
    /// Chat completions at `base_url` (`…/v1`) with `model`, shaped for
    /// `target`.
    #[must_use]
    pub fn new(
        base_url: &str,
        model: &str,
        api_key: ApiKey,
        timeout: Duration,
        target: ApiTarget,
    ) -> Self {
        let base = base_url.trim_end_matches('/');
        let endpoint = if base.ends_with("/chat/completions") {
            base.to_owned()
        } else {
            format!("{base}/chat/completions")
        };
        Self {
            turns: EndpointTurns {
                endpoint,
                model: model.to_owned(),
                api_key,
                target,
                profile: crate::profile::Profile::default(),
                protocol: ProviderProtocol::OpenAiCompatible,
                vault: None,
                idle: Arc::new(Mutex::new(Vec::new())),
            },
            timeout,
            vision: None,
        }
    }

    /// Claude through Anthropic's Messages API at `base_url` (the API's
    /// origin, `https://api.anthropic.com`; a trailing `/v1` is dropped),
    /// planning as [`GatewayModel`] does. Anthropic always needs a key.
    #[must_use]
    pub fn anthropic(base_url: &str, model: &str, api_key: ApiKey, timeout: Duration) -> Self {
        let base = base_url.trim_end_matches('/');
        let origin = base
            .strip_suffix("/v1/messages")
            .or_else(|| base.strip_suffix("/v1"))
            .unwrap_or(base);
        let mut planner = Self::new(base_url, model, api_key, timeout, ApiTarget::Standard);
        origin.clone_into(&mut planner.turns.endpoint);
        planner.turns.protocol = ProviderProtocol::Anthropic;
        planner
    }

    /// Adopt the key into the vault under the config root `root` rather
    /// than the user's (see [`CompletionClient::with_credentials_in`]).
    #[must_use]
    pub fn with_vault(mut self, root: Option<std::path::PathBuf>) -> Self {
        self.turns.vault = root;
        self
    }

    /// Opt-in provider hints and hedging, filtered by the gateway target's
    /// field allowlist.
    #[must_use]
    pub fn with_profile(mut self, profile: crate::profile::Profile) -> Self {
        self.turns.profile = profile;
        self
    }

    /// Show it pictures (`Some(true)`) or not (`Some(false)`), whatever its
    /// default; `None` leaves the default.
    #[must_use]
    pub fn with_vision(mut self, vision: Option<bool>) -> Self {
        self.vision = vision;
        self
    }
}

impl PlanModel for OpenAiModel {
    fn choose(
        &mut self,
        task: &str,
        catalogue: &Catalogue,
        most_steps: usize,
    ) -> Result<Answer, String> {
        self.choose_briefed(
            task,
            &Briefing::default(),
            catalogue,
            most_steps,
            &mut NoPlanReads,
        )
    }

    fn choose_briefed(
        &mut self,
        task: &str,
        briefing: &Briefing,
        catalogue: &Catalogue,
        most_steps: usize,
        read: &mut ReadView<'_>,
    ) -> Result<Answer, String> {
        let sees = self.sees();
        converse::plan(
            &mut self.turns,
            &converse::Asked {
                task,
                briefing,
                catalogue,
                most_steps,
                sees,
                timeout: self.timeout,
            },
            read,
        )
    }

    /// `OpenAI`'s own endpoint and Tilo Cloud take pictures; another server
    /// (a local model) is assumed not to unless it is said to
    /// ([`crate::vision::VISION_ENV`]).
    fn sees(&self) -> bool {
        self.vision.unwrap_or_else(|| {
            self.turns.target == ApiTarget::CloudMetered
                || self.turns.protocol == ProviderProtocol::Anthropic
                || self.turns.endpoint.starts_with("https://api.openai.com/")
        })
    }
}

/// The turns of an [`OpenAiModel`]: the trusted host's own client, each
/// kept between turns so later ones reuse its connection.
struct EndpointTurns {
    endpoint: String,
    model: String,
    api_key: ApiKey,
    target: ApiTarget,
    profile: crate::profile::Profile,
    /// The wire the endpoint speaks.
    protocol: ProviderProtocol,
    /// The config root whose vault adopts the key; `None` the user's.
    vault: Option<std::path::PathBuf>,
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
        if let Some(root) = &self.vault {
            client = client.with_credentials_in(root);
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
        // The agent's name, and its part of a coordinated task, follow the
        // call into the usage log.
        let name = std::thread::current().name().map(str::to_owned);
        let role = converse::plan_role();
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
                converse::as_role(role, || {
                    record_usage(
                        (&call.model, call.tier.as_deref()),
                        &call.request,
                        started.elapsed(),
                        &answer,
                        attempt,
                    );
                });
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
}

/// Which model plans an agent's natural-language tasks on a host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelChoice {
    /// The provider selected in Settings, through `tilo-model-gateway`.
    Settings,
    /// [`ScriptedModel`].
    Scripted,
    /// [`OpenAiModel`] at a base URL.
    OpenAi {
        /// `…/v1`.
        base_url: String,
        /// Model id.
        model: String,
        /// Bearer key, if the endpoint wants one.
        api_key: ApiKey,
        /// A plain provider, or Tilo Cloud's metered machine endpoint.
        target: ApiTarget,
        /// The config root whose vault adopts the key; `None` the user's.
        vault: Option<std::path::PathBuf>,
    },
    /// [`OpenAiModel::anthropic`]: Claude through Anthropic's Messages API.
    Anthropic {
        /// The API's origin (`https://api.anthropic.com`).
        base_url: String,
        /// Model id.
        model: String,
        /// The key, which Anthropic always wants.
        api_key: ApiKey,
        /// The config root whose vault adopts the key; `None` the user's.
        vault: Option<std::path::PathBuf>,
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
                    vault: None,
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
                    vault: None,
                })
            }
        }
    }

    /// This choice adopting its key into the vault under the config root
    /// `root` rather than the user's: a run that keeps no credential past
    /// itself (a benchmark), or a session without the platform's secret
    /// store. The key never leaves the host's process for an agent's worker.
    #[must_use]
    pub fn keeping_keys_in(mut self, root: std::path::PathBuf) -> Self {
        if let Self::OpenAi { vault, .. } | Self::Anthropic { vault, .. } = &mut self {
            *vault = Some(root);
        }
        self
    }

    /// A planner over this model, built on the agent's own thread. The
    /// profile (preset and explicit settings) says whether its tasks are
    /// checked once done, and whether they are coordinated.
    #[must_use]
    pub fn planner(&self) -> ModelPlanner {
        let vision =
            crate::vision::vision_setting(std::env::var(crate::vision::VISION_ENV).ok().as_deref());
        match self {
            Self::Scripted => ModelPlanner::new(Box::new(ScriptedModel)),
            Self::OpenAi {
                base_url,
                model,
                api_key,
                target,
                vault,
            } => {
                let profile =
                    crate::profile::Profile::resolve(model, &|name| std::env::var(name).ok());
                let (checks_work, decomposes) = (profile.checks_work(), profile.decomposes());
                ModelPlanner::new(Box::new(
                    OpenAiModel::new(base_url, model, api_key.clone(), PLAN_TIMEOUT, *target)
                        .with_profile(profile)
                        .with_vision(vision)
                        .with_vault(vault.clone()),
                ))
                .checking_work(checks_work)
                .decomposing(decomposes)
            }
            Self::Anthropic {
                base_url,
                model,
                api_key,
                vault,
            } => {
                let profile =
                    crate::profile::Profile::resolve(model, &|name| std::env::var(name).ok());
                let (checks_work, decomposes) = (profile.checks_work(), profile.decomposes());
                ModelPlanner::new(Box::new(
                    OpenAiModel::anthropic(base_url, model, api_key.clone(), PLAN_TIMEOUT)
                        .with_profile(profile)
                        .with_vision(vision)
                        .with_vault(vault.clone()),
                ))
                .checking_work(checks_work)
                .decomposing(decomposes)
            }
            Self::Settings => match tilo_agent::providers::default_selection() {
                Some(selection) => {
                    let profile = crate::profile::Profile::resolve(&selection.model.id, &|name| {
                        std::env::var(name).ok()
                    });
                    ModelPlanner::new(Box::new(GatewayModel::new(selection, PLAN_TIMEOUT)))
                        .checking_work(profile.checks_work())
                        .decomposing(profile.decomposes())
                }
                None => ModelPlanner::new(Box::new(Unconfigured)),
            },
        }
    }

    /// A short description for logs and status.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Settings => "the Settings provider".into(),
            Self::Scripted => "the scripted model".into(),
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

/// No provider selected in Settings.
struct Unconfigured;

impl PlanModel for Unconfigured {
    fn choose(&mut self, _: &str, _: &Catalogue, _: usize) -> Result<Answer, String> {
        Err(
            "no agent model is configured: choose a provider and default model in \
             Settings, or start the host with --agent-model"
                .into(),
        )
    }
}
