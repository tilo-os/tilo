//! Natural-language tasks, planned by a model from the published surface.
//!
//! [`ModelPlanner`] sits behind the explicit-steps path: a task that is JSON
//! steps is read exactly as [`crate::DirectPlanner`] reads it; any other
//! text goes to a [`PlanModel`]. The model is `apps/agent`'s action
//! discovery (`tilo_agent::tools` over a `tilo_agent::surface::Catalogue`):
//! it is shown the published operations with the task, lists and describes
//! more through read-only tools when it needs to, and finishes with a `plan`
//! naming only operations it was shown. Its answer is
//! untrusted: every step is mapped back to an offered address here, and then
//! checked against the delegation and the app's door by the participant like
//! any other step.
//!
//! [`GatewayModel`] asks through `tilo-model-gateway`, which holds the
//! provider credential; the agent never sees a key. Tests use their own
//! [`PlanModel`] over the same tools.

use std::collections::BTreeMap;

use tilo_actions::door::{ActionAddress, ActionDescriptor};
use tilo_agent::surface::{self, Catalogue, Offer};
use tilo_agent::tools::Answer;
use tilo_browser_peer::DocumentRoute;
/// Trusted-host CLI credential source; sandboxed agents use the gateway capability.
pub use tilo_model_gateway::host_backend::ApiKey;
use tilo_model_gateway::openai::{self, Target as ApiTarget};

use crate::planner::{PlanContext, Planner, Step, StepAction, written_steps};

/// The most steps one task may be planned into.
pub const MOST_STEPS: usize = 8;

/// Chooses operations for a task from a catalogue.
pub trait PlanModel: Send {
    /// The operations (by offer id) and their arguments that carry out
    /// `task`, using only `catalogue`.
    ///
    /// # Errors
    ///
    /// Returns why no plan was made: no model, a failed call, or an answer
    /// the discovery tools refused.
    fn choose(
        &mut self,
        task: &str,
        catalogue: &Catalogue,
        most_steps: usize,
    ) -> Result<Answer, String>;
}

/// `apps/agent`'s chooser over the Settings-selected provider, through the
/// per-user model gateway.
pub struct GatewayModel(tilo_agent::model::Chooser);

impl GatewayModel {
    /// A model call over `selection`, bounded by `timeout`.
    #[must_use]
    pub fn new(
        selection: tilo_agent::providers::ProviderSelection,
        timeout: std::time::Duration,
    ) -> Self {
        Self(tilo_agent::model::Chooser::with_provider(
            true, timeout, selection,
        ))
    }
}

impl PlanModel for GatewayModel {
    fn choose(
        &mut self,
        task: &str,
        catalogue: &Catalogue,
        most_steps: usize,
    ) -> Result<Answer, String> {
        self.0
            .choose_actions(task, catalogue, most_steps)
            .map_err(|route| format!("no model plan: {}", route.detail()))
    }
}

/// Explicit steps as written; anything else planned by a model.
pub struct ModelPlanner {
    model: Box<dyn PlanModel>,
}

impl ModelPlanner {
    /// Plan natural-language tasks with `model`.
    #[must_use]
    pub fn new(model: Box<dyn PlanModel>) -> Self {
        Self { model }
    }
}

/// Where an offered operation runs.
enum Target {
    Record(DocumentRoute),
    Open(String),
    OpenRecord(DocumentRoute),
}

/// Records of one app the room already has that are offered to open.
const KNOWN_OFFERED: usize = 4;

/// The catalogue a model sees for `context`, and where each offer runs.
fn catalogue(context: &PlanContext) -> (Catalogue, BTreeMap<String, Target>) {
    let mut offers = Vec::new();
    let mut targets = BTreeMap::new();
    for (route, descriptors) in &context.open {
        for descriptor in descriptors {
            let offer = Offer {
                app: route.app_id.clone(),
                broker_peer_id: None,
                broker_provider: None,
                descriptor: descriptor.clone(),
            };
            targets
                .entry(offer.id())
                .or_insert_with(|| Target::Record(route.clone()));
            offers.push(offer);
        }
    }
    for app in &context.apps {
        if context.open.iter().any(|(route, _)| route.app_id == *app) {
            continue;
        }
        // The room's own documents of this app come before a new one, so
        // a task about "the notes" works on the notes people already have.
        for route in context
            .known
            .iter()
            .filter(|route| route.app_id == *app)
            .take(KNOWN_OFFERED)
        {
            let offer = Offer {
                app: app.clone(),
                broker_peer_id: None,
                broker_provider: None,
                descriptor: ActionDescriptor {
                    semantic_identity: None,
                    address: ActionAddress::new(
                        format!("record.{app}.{}", route.record),
                        surface::OPEN_ACTION,
                    ),
                    resource_kind: surface::APPLICATION_KIND.to_owned(),
                    resource_label: app.clone(),
                    action_label: format!("open {app} on the room's document {}", route.record),
                    description: Some(format!(
                        "A {app} document this room already has, which people may be \
                         looking at. Opening it puts its operations on the list."
                    )),
                    locus: tilo_actions::ExecutionLocus::ServerPreferred,
                    kind: tilo_actions::OperationKind::Effect,
                    contract: tilo_actions::ActionContract::default(),
                    params: Vec::new(),
                    input: tilo_actions::InputSpec::None,
                    view: None,
                },
            };
            targets.insert(offer.id(), Target::OpenRecord(route.clone()));
            offers.push(offer);
        }
        let offer = Offer {
            app: app.clone(),
            broker_peer_id: None,
            broker_provider: None,
            descriptor: ActionDescriptor {
                semantic_identity: None,
                address: ActionAddress::new(format!("app.{app}"), surface::OPEN_ACTION),
                resource_kind: surface::APPLICATION_KIND.to_owned(),
                resource_label: app.clone(),
                action_label: format!("open {app}"),
                description: Some(format!(
                    "{app} is not open, so none of what it can do is listed yet. \
                     Opening it on a new record puts its own operations on the list."
                )),
                locus: tilo_actions::ExecutionLocus::ServerPreferred,
                kind: tilo_actions::OperationKind::Effect,
                contract: tilo_actions::ActionContract::default(),
                params: Vec::new(),
                input: tilo_actions::InputSpec::None,
                view: None,
            },
        };
        targets.insert(offer.id(), Target::Open(app.clone()));
        offers.push(offer);
    }
    (
        Catalogue {
            offers,
            facts: Vec::new(),
            held: Vec::new(),
        },
        targets,
    )
}

impl Planner for ModelPlanner {
    fn plan(&mut self, task: &str, context: &PlanContext) -> Result<Vec<Step>, String> {
        if let Some(steps) = written_steps(task, context) {
            return steps;
        }
        let (catalogue, targets) = catalogue(context);
        let answer = self.model.choose(task, &catalogue, MOST_STEPS)?;
        let mut steps = Vec::with_capacity(answer.actions.len().saturating_add(1));
        for (id, arguments) in answer.actions.iter().zip(answer.arguments) {
            let address = surface::decode_address(id)
                .ok_or_else(|| format!("the model named {id:?}, which is not an operation"))?;
            match targets.get(id) {
                Some(Target::Record(route)) => steps.push(Step {
                    app: route.app_id.clone(),
                    record: Some(route.record.clone()),
                    action: StepAction::Invoke { address, arguments },
                }),
                Some(Target::Open(app)) => steps.push(Step {
                    app: app.clone(),
                    record: None,
                    action: StepAction::Open,
                }),
                Some(Target::OpenRecord(route)) => steps.push(Step {
                    app: route.app_id.clone(),
                    record: Some(route.record.clone()),
                    action: StepAction::Open,
                }),
                None => return Err(format!("the model named {id:?}, which was not offered")),
            }
        }
        if let Some(then) = answer.then {
            let app = steps
                .last()
                .map(|step| step.app.clone())
                .unwrap_or_default();
            steps.push(Step {
                app,
                record: None,
                action: StepAction::Continue(then),
            });
        }
        Ok(steps)
    }
}

/// Words that say nothing about which operation is meant.
const STOPWORDS: &[&str] = &[
    "the", "and", "then", "with", "into", "for", "that", "this", "please", "called", "named",
    "titled", "title", "name", "tilo",
];

/// Lowercase words of three letters or more, without a plural `s`.
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.len() > 2 && !STOPWORDS.contains(word))
        .map(|word| {
            word.strip_suffix('s')
                .filter(|stem| stem.len() > 2)
                .unwrap_or(word)
                .to_owned()
        })
        .collect()
}

/// The quoted phrases of `text`, in order.
fn quoted(text: &str) -> Vec<String> {
    text.split(['"', '\u{201c}', '\u{201d}'])
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

/// The clauses of a task: `;`, new lines and "then" separate them.
fn clauses(task: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in task.split([';', '\n']) {
        let mut rest = part.trim();
        loop {
            let lower = rest.to_lowercase();
            let Some(at) = lower.find(" then ") else {
                break;
            };
            let (head, tail) = rest.split_at(at);
            if !head.trim().is_empty() {
                out.push(head.trim().to_owned());
            }
            rest = tail.get(" then ".len()..).unwrap_or_default().trim();
        }
        let rest = rest
            .strip_prefix("then ")
            .or_else(|| rest.strip_prefix("and then "))
            .unwrap_or(rest)
            .trim();
        if !rest.is_empty() {
            out.push(rest.to_owned());
        }
    }
    out
}

/// An app's short name: `so.tilo.notes` → `notes`.
fn app_name(app: &str) -> Vec<String> {
    words(app.rsplit('.').next().unwrap_or(app))
}

/// Whether a clause's words name `app`: every word of its name is there.
fn names_app(clause: &[String], app: &str) -> bool {
    let name = app_name(app);
    !name.is_empty() && name.iter().all(|word| clause.contains(word))
}

/// A deterministic [`PlanModel`] for tests and local development: no
/// network, the same plan for the same task and surface every time.
///
/// It reads the task clause by clause (`;`, new lines and "then" separate
/// them). For the first clause it picks the offer sharing the most words
/// with it, weighing the app the clause names and the action's own verb,
/// and fills text parameters (required ones first, then optional ones) from
/// the clause's quoted phrases and number
/// parameters from its numbers. When that app is not open yet it opens it
/// and asks again with the same clause; otherwise the remaining clauses are
/// the plan's `then`. It reveals what it names before planning, bound by
/// the same rule a model is.
#[derive(Default)]
pub struct ScriptedModel;

impl ScriptedModel {
    fn score(clause: &[String], has_quote: bool, offer: &Offer) -> usize {
        let descriptor = &offer.descriptor;
        let own: std::collections::BTreeSet<String> = words(&format!(
            "{} {} {} {}",
            descriptor.resource_label,
            descriptor.address.resource,
            descriptor.action_label,
            descriptor.address.action
        ))
        .into_iter()
        .collect();
        let shared = clause.iter().filter(|word| own.contains(*word)).count();
        let verb = words(&descriptor.address.action)
            .iter()
            .any(|word| clause.contains(word));
        let names_app = names_app(clause, &offer.app);
        let wants_text = descriptor
            .params
            .iter()
            .any(|param| param.ty == tilo_actions::ParamType::Text);
        let mut score = shared;
        if verb {
            score = score.saturating_add(2);
        }
        if names_app {
            score = score.saturating_add(5);
        }
        if has_quote && wants_text {
            score = score.saturating_add(1);
        }
        if descriptor.contract.confirmation != tilo_actions::ConfirmationPolicy::None
            || descriptor.contract.availability != tilo_actions::Availability::Available
        {
            score = 0;
        }
        if shared == 0 && !names_app {
            score = 0;
        }
        score
    }
}

impl PlanModel for ScriptedModel {
    #[allow(
        clippy::too_many_lines,
        reason = "the scripted planner's keyword table reads as one match"
    )]
    fn choose(
        &mut self,
        task: &str,
        catalogue: &Catalogue,
        most_steps: usize,
    ) -> Result<Answer, String> {
        let mut clauses = clauses(task);
        if clauses.is_empty() {
            return Err("the task says nothing to do".into());
        }
        let clause = clauses.remove(0);
        let clause_words = words(&clause);
        let phrases = quoted(&clause);
        let offer = catalogue
            .offers
            .iter()
            .map(|offer| {
                (
                    Self::score(&clause_words, !phrases.is_empty(), offer),
                    offer,
                )
            })
            .filter(|(score, _)| *score > 0)
            // The first of the best, in catalogue order.
            .fold(None::<(usize, &Offer)>, |best, (score, offer)| match best {
                Some((top, _)) if top >= score => best,
                _ => Some((score, offer)),
            })
            .map(|(_, offer)| offer)
            .ok_or_else(|| format!("nothing offered fits {clause:?}"))?;
        let mut revealed = tilo_agent::tools::Revealed::new();
        tilo_agent::tools::answer(
            tilo_agent::tools::LIST_OPERATIONS,
            &serde_json::json!({ "resource": offer.address().resource }),
            catalogue,
            &mut revealed,
        );
        let opens = offer.address().action == surface::OPEN_ACTION
            && offer.descriptor.resource_kind == surface::APPLICATION_KIND;
        let mut arguments = serde_json::Map::new();
        if !opens {
            let mut texts = phrases.iter();
            let numbers: Vec<f64> = clause
                .split(|c: char| !(c.is_ascii_digit() || c == '.'))
                .filter_map(|part| part.parse().ok())
                .collect();
            for param in offer
                .descriptor
                .params
                .iter()
                .filter(|param| param.required)
            {
                let value = if let Some(choice) = param
                    .choices
                    .iter()
                    .find(|choice| clause_words.contains(&choice.to_lowercase()))
                    .or_else(|| param.choices.first())
                {
                    serde_json::Value::from(choice.as_ref())
                } else {
                    match param.ty {
                        tilo_actions::ParamType::Integer => {
                            #[allow(
                                clippy::cast_possible_truncation,
                                reason = "a number a person wrote in a task"
                            )]
                            let number = numbers.first().copied().unwrap_or(1.0) as i64;
                            number.into()
                        }
                        tilo_actions::ParamType::Number => {
                            numbers.first().copied().unwrap_or(1.0).into()
                        }
                        tilo_actions::ParamType::Boolean => true.into(),
                        _ => texts
                            .next()
                            .cloned()
                            .ok_or_else(|| {
                                format!(
                                    "{} needs {}: put it in quotes",
                                    offer.descriptor.action_label, param.name
                                )
                            })?
                            .into(),
                    }
                };
                arguments.insert(param.name.to_string(), value);
            }
            // A quoted phrase left over names an optional text parameter
            // (the new sheet's name), in the order they are declared.
            for param in offer.descriptor.params.iter().filter(|param| {
                !param.required
                    && param.ty == tilo_actions::ParamType::Text
                    && param.choices.is_empty()
            }) {
                let Some(text) = texts.next() else { break };
                arguments.insert(param.name.to_string(), text.clone().into());
            }
            if offer.descriptor.params.is_empty()
                && let tilo_actions::InputSpec::Text { .. } = offer.descriptor.input
                && let Some(text) = texts.next()
            {
                arguments.insert(tilo_actions::PROMPTED_PARAM.into(), text.clone().into());
            }
        }
        // Opening an app puts its own operations on the list: the same
        // clause is asked again against them.
        let then = if opens {
            Some(
                std::iter::once(clause)
                    .chain(clauses)
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        } else {
            (!clauses.is_empty()).then(|| clauses.join("; "))
        };
        tilo_agent::tools::read_plan(
            &serde_json::json!({
                "steps": [{"operation": offer.id(), "arguments": arguments}],
                "why": "scripted",
                "then": then,
            }),
            &revealed,
            most_steps,
        )
    }
}

/// The model Tilo Cloud meters when `cloud:` names no other.
pub const CLOUD_MODEL: &str = "gpt-4.1-mini-2025-04-14";

/// Output one discovery turn may use: a plan of [`MOST_STEPS`] fits well
/// within it, and a metered endpoint reserves credit for all of it.
const TURN_OUTPUT_TOKENS: u32 = 1024;

/// A model behind an OpenAI-compatible chat-completions endpoint with tool
/// calls (`OpenAI`, vLLM, llama.cpp, Ollama, LM Studio, `OpenRouter`, Tilo
/// Cloud's metered machine endpoint), driving the same discovery tools as
/// [`GatewayModel`]. It calls the endpoint from this process with the key it
/// was given: for a host operator's own endpoint, a Cloud machine's host
/// and local development, not a sandboxed app.
pub struct OpenAiModel {
    endpoint: String,
    model: String,
    api_key: ApiKey,
    timeout: std::time::Duration,
    target: ApiTarget,
    profile: crate::profile::Profile,
    client: Option<tilo_model_gateway::host_backend::CompletionClient>,
}

impl OpenAiModel {
    /// Chat completions at `base_url` (`…/v1`) with `model`, shaped for
    /// `target`.
    #[must_use]
    pub fn new(
        base_url: &str,
        model: &str,
        api_key: ApiKey,
        timeout: std::time::Duration,
        target: ApiTarget,
    ) -> Self {
        let base = base_url.trim_end_matches('/');
        let endpoint = if base.ends_with("/chat/completions") {
            base.to_owned()
        } else {
            format!("{base}/chat/completions")
        };
        Self {
            endpoint,
            model: model.to_owned(),
            api_key,
            timeout,
            target,
            profile: crate::profile::Profile::default(),
            client: None,
        }
    }

    /// Opt-in provider hints, filtered by the gateway target's field allowlist.
    #[must_use]
    pub fn with_profile(mut self, profile: crate::profile::Profile) -> Self {
        self.profile = profile;
        self
    }
}

/// Keep the host CLI's plain-string contract without changing typed provider errors.
fn cli_model_error(error: &tilo_model_gateway::host_backend::protocol::ModelError) -> String {
    use tilo_model_gateway::host_backend::protocol::ErrorKind;
    if error.kind == ErrorKind::CreditsExhausted {
        format!(
            "{}: check your provider's credit or subscription, or choose another model.",
            openai::OUT_OF_CREDITS
        )
    } else {
        error.to_string()
    }
}

impl PlanModel for OpenAiModel {
    fn choose(
        &mut self,
        task: &str,
        catalogue: &Catalogue,
        most_steps: usize,
    ) -> Result<Answer, String> {
        use tilo_model_gateway::host_backend::protocol::{Content, Conversation, ToolResult};
        if self.client.is_none() {
            self.client = Some(
                tilo_model_gateway::host_backend::CompletionClient::new()
                    .map_err(|error| cli_model_error(&error))?
                    .with_request_hints(&{
                        let mut hints = serde_json::json!({});
                        self.profile.apply(&mut hints);
                        hints
                    }),
            );
        }
        let client = self.client.as_mut().ok_or("no model transport")?;
        let deadline = std::time::Instant::now()
            .checked_add(self.timeout)
            .unwrap_or_else(std::time::Instant::now);
        // One identity per logical turn, stable across gateway-owned attempts.
        let mut run = [0_u8; 16];
        getrandom::fill(&mut run).map_err(|_| "no randomness for the model call".to_string())?;
        let mut revealed = tilo_agent::tools::Revealed::new();
        // Published operations are shown and revealed before the first turn.
        let task = tilo_agent::model::action_user_turn(task, catalogue, &mut revealed).text();
        let mut conversation = Conversation::new(
            "You drive Tilo apps only through the resource/action tools. \
             The task lists the operations published right now with their parameters: \
             when one of them does what is asked, plan it straight away. Use the listing \
             tools only for what that list leaves out, describe an operation only when \
             its arguments are unclear, and finish by calling `plan` exactly once. \
             Preserve every argument's declared JSON type. Never invent an operation id \
             or parameter. When an app you need is not open, plan its open operation and \
             put the rest of the request in `then`.",
            &task,
        );
        for turn in 0..12_u32 {
            let request = conversation.request(
                tilo_agent::tools::typed_definitions(most_steps),
                TURN_OUTPUT_TOKENS,
            );
            let key = openai::turn_key(&run, turn, &serde_json::json!((&self.model, &request)));
            let response = client
                .complete(
                    &self.endpoint,
                    &self.api_key,
                    self.target,
                    &self.model,
                    &request,
                    &key,
                    deadline,
                )
                .map_err(|error| cli_model_error(&error))?;
            let calls = conversation
                .validate_response(&response)
                .map_err(|error| error.to_string())?;
            if calls.is_empty() {
                let text: String = response
                    .message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        Content::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                return Err(format!(
                    "the model neither discovered operations nor planned: {}",
                    text.chars().take(300).collect::<String>()
                ));
            }
            let calls: Vec<_> = calls.into_iter().cloned().collect();
            // Validate the completed turn and entire batch before revealing or planning.
            tilo_agent::tools::validate_plan_calls(&calls)?;
            let mut results = Vec::new();
            for call in calls {
                let (content, is_error) = if call.name == tilo_agent::tools::PLAN {
                    match tilo_agent::tools::read_plan(&call.input, &revealed, most_steps) {
                        Ok(answer) => return Ok(answer),
                        Err(error) => (tilo_agent::tools::plan_refused(&error), true),
                    }
                } else {
                    (
                        tilo_agent::tools::answer(
                            &call.name,
                            &call.input,
                            catalogue,
                            &mut revealed,
                        ),
                        false,
                    )
                };
                results.push(ToolResult {
                    content,
                    call_id: call.id,
                    is_error,
                });
            }
            conversation
                .push(response, results)
                .map_err(|error| error.to_string())?;
        }
        Err("the model exhausted its discovery turns".into())
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
    },
}

impl ModelChoice {
    /// `settings`, `scripted`, `openai:<base url>` or `cloud:<base url>`
    /// (Tilo Cloud's metered `…/api/machines/<id>/ai/v1`), with the model
    /// and key given separately (`--agent-model-name`; `TILO_AGENT_API_KEY`
    /// or `TILO_AGENT_API_KEY_FILE`). `cloud:` defaults to [`CLOUD_MODEL`]
    /// and needs a key.
    ///
    /// # Errors
    ///
    /// Returns what is wrong with the spelling.
    pub fn parse(spec: &str, model: Option<&str>, api_key: ApiKey) -> Result<Self, String> {
        let model = model.filter(|model| !model.is_empty());
        match spec {
            "settings" => Ok(Self::Settings),
            "scripted" => Ok(Self::Scripted),
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
                    return Err(
                        "expected settings, scripted, openai:<base url> or cloud:<base url>".into(),
                    );
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

    /// A planner over this model, built on the agent's own thread.
    #[must_use]
    pub fn planner(&self) -> ModelPlanner {
        let timeout = std::time::Duration::from_secs(60);
        ModelPlanner::new(match self {
            Self::Scripted => Box::new(ScriptedModel),
            Self::OpenAi {
                base_url,
                model,
                api_key,
                target,
            } => Box::new(
                OpenAiModel::new(base_url, model, api_key.clone(), timeout, *target).with_profile(
                    crate::profile::Profile::resolve(model, &|name| std::env::var(name).ok()),
                ),
            ),
            Self::Settings => match tilo_agent::providers::default_selection() {
                Some(selection) => Box::new(GatewayModel::new(selection, timeout)),
                None => Box::new(Unconfigured),
            },
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_names_the_metered_endpoint_and_needs_a_token() {
        let key = || ApiKey::Value("tilo-ai-x".into());
        let choice = ModelChoice::parse(
            "cloud:http://127.0.0.1:1/api/machines/m1/ai/v1",
            None,
            key(),
        )
        .unwrap();
        assert_eq!(
            choice,
            ModelChoice::OpenAi {
                base_url: "http://127.0.0.1:1/api/machines/m1/ai/v1".into(),
                model: CLOUD_MODEL.into(),
                api_key: key(),
                target: ApiTarget::CloudMetered,
            }
        );
        assert!(ModelChoice::parse("cloud:http://x/v1", None, ApiKey::None).is_err());
        assert!(ModelChoice::parse("openai:http://x/v1", None, ApiKey::None).is_err());
        assert!(ModelChoice::parse("cloud:ftp://x", None, key()).is_err());
    }

    /// An endpoint answering `answers` in order, one connection each, and
    /// reporting every request's head and body.
    fn endpoint(answers: Vec<String>) -> (String, std::sync::mpsc::Receiver<(String, String)>) {
        use std::io::{BufRead as _, BufReader, Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let (sent, received) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for answer in answers {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let (mut head, mut length) = (String::new(), 0);
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                    if line == "\r\n" {
                        break;
                    }
                    head.push_str(&line.to_ascii_lowercase());
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                sent.send((head, String::from_utf8(body).unwrap())).unwrap();
                reader.into_inner().write_all(answer.as_bytes()).unwrap();
            }
        });
        (url, received)
    }

    fn http(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn open_notes() -> Catalogue {
        let (catalogue, _) = catalogue(&PlanContext {
            open: Vec::new(),
            known: Vec::new(),
            apps: vec!["so.tilo.notes".into()],
        });
        catalogue
    }

    #[test]
    fn a_metered_turn_is_retried_under_one_key_and_plans() {
        let offer = open_notes().offers[0].id();
        let list = serde_json::json!({"choices": [{"message": {"role": "assistant", "content": null,
            "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "list_operations",
                "arguments": "{\"application\":\"so.tilo.notes\"}"}}]}, "finish_reason": "tool_calls"}]});
        let plan = serde_json::json!({"choices": [{"message": {"role": "assistant", "content": null,
            "tool_calls": [{"id": "c2", "type": "function", "function": {"name": "plan",
                "arguments": serde_json::json!({"steps": [{"operation": offer, "arguments": {}}],
                    "why": "open", "then": "add a note"}).to_string()}}]}, "finish_reason": "tool_calls"}]});
        let (url, sent) = endpoint(vec![
            http(
                "429 Too Many Requests",
                r#"{"error":"slow down","code":"rate_limited"}"#,
            ),
            http("200 OK", &list.to_string()),
            http("200 OK", &plan.to_string()),
        ]);
        let mut model = OpenAiModel::new(
            &url,
            CLOUD_MODEL,
            ApiKey::None,
            std::time::Duration::from_secs(30),
            ApiTarget::CloudMetered,
        )
        .with_profile(crate::profile::Profile::preset(
            crate::profile::ModelClass::Small,
            crate::profile::Goal::Fastest,
        ));
        let answer = model
            .choose("open notes then add a note", &open_notes(), MOST_STEPS)
            .unwrap();
        assert_eq!(answer.actions, vec![offer]);
        assert_eq!(answer.then.as_deref(), Some("add a note"));
        let requests: Vec<(String, String)> = sent.try_iter().collect();
        let key = |head: &str| {
            head.lines()
                .find_map(|line| line.strip_prefix("idempotency-key: "))
                .map(str::to_owned)
                .unwrap()
        };
        assert_eq!(key(&requests[0].0), key(&requests[1].0));
        assert_ne!(key(&requests[1].0), key(&requests[2].0));
        assert_eq!(requests[0].1, requests[1].1, "retry the same shaped body");
        for (_, body) in &requests {
            let body: serde_json::Value = serde_json::from_str(body).unwrap();
            assert!(body.get("service_tier").is_none(), "{body}");
            assert!(body.get("reasoning_effort").is_none(), "{body}");
        }
        let second: serde_json::Value = serde_json::from_str(&requests[2].1).unwrap();
        assert_eq!(second["max_completion_tokens"], TURN_OUTPUT_TOKENS);
        assert_eq!(second["messages"][3]["role"], "tool");
        assert!(second.get("max_tokens").is_none());
    }

    /// A chat-completions answer calling `plan` with one step, `operation`.
    fn plan_answer(id: &str, operation: &str) -> String {
        let answer = serde_json::json!({"choices": [{"message": {"role": "assistant", "content": null,
            "tool_calls": [{"id": id, "type": "function", "function": {"name": "plan",
                "arguments": serde_json::json!({"steps": [{"operation": operation, "arguments": {}}],
                    "why": "open", "then": "add a note"}).to_string()}}]}, "finish_reason": "tool_calls"}]});
        http("200 OK", &answer.to_string())
    }

    fn standard(url: &str) -> OpenAiModel {
        OpenAiModel::new(
            url,
            "pinned",
            ApiKey::None,
            std::time::Duration::from_secs(30),
            ApiTarget::Standard,
        )
    }

    /// `OpenAI` permits text content as either a string or typed text blocks.
    fn message_text(message: &serde_json::Value, role: &str) -> String {
        assert_eq!(message["role"], role);
        let content = &message["content"];
        if let Some(text) = content.as_str() {
            return text.to_owned();
        }
        content
            .as_array()
            .expect("text content must be a string or text blocks")
            .iter()
            .map(|block| {
                assert_eq!(block["type"], "text");
                block["text"].as_str().expect("text block").to_owned()
            })
            .collect()
    }

    /// The published operations come with the task, so a task they answer
    /// is planned in ONE request — no listing or describing turn first.
    #[test]
    fn an_operation_the_task_turn_lists_is_planned_in_one_request() {
        let offer = open_notes().offers[0].id();
        let (url, sent) = endpoint(vec![plan_answer("c1", &offer)]);
        let answer = standard(&url)
            .choose("open notes then add a note", &open_notes(), MOST_STEPS)
            .unwrap();
        assert_eq!(answer.actions, vec![offer.clone()]);
        let requests: Vec<(String, String)> = sent.try_iter().collect();
        assert_eq!(requests.len(), 1, "one model turn");
        let first: serde_json::Value = serde_json::from_str(&requests[0].1).unwrap();
        let task = message_text(&first["messages"][1], "user");
        assert!(task.contains(&offer), "{task}");
        assert!(task.contains("open notes then add a note"), "{task}");
        let system = message_text(&first["messages"][0], "system");
        assert!(!system.contains("before planning"), "{system}");
    }

    #[test]
    fn an_incomplete_or_mixed_plan_turn_is_rejected_before_planning() {
        let offer = open_notes().offers[0].id();
        for (finish, mixed) in [("length", false), ("tool_calls", true)] {
            let mut calls = vec![serde_json::json!({
                "id": "plan", "type": "function", "function": {
                    "name": "plan",
                    "arguments": serde_json::json!({
                        "steps": [{"operation": offer, "arguments": {}}],
                        "why": "open", "then": null
                    }).to_string()
                }
            })];
            if mixed {
                calls.push(serde_json::json!({
                    "id": "list", "type": "function", "function": {
                        "name": "list_applications", "arguments": "{}"
                    }
                }));
            }
            let response = serde_json::json!({"choices": [{
                "message": {"role": "assistant", "content": null, "tool_calls": calls},
                "finish_reason": finish
            }]});
            let (url, sent) = endpoint(vec![http("200 OK", &response.to_string())]);
            assert!(
                standard(&url)
                    .choose("open notes", &open_notes(), MOST_STEPS)
                    .is_err()
            );
            assert_eq!(sent.try_iter().count(), 1);
        }
    }

    /// Environment settings are applied by the production planner factory.
    /// Each case runs in its own process so parallel tests never share them.
    #[test]
    fn host_planner_resolves_environment_hints() {
        const CHILD_ENDPOINT: &str = "TILO_PRESET_TEST_ENDPOINT";
        if let Ok(url) = std::env::var(CHILD_ENDPOINT) {
            let choice =
                ModelChoice::parse(&format!("openai:{url}"), Some("mini"), ApiKey::None).unwrap();
            let steps = choice
                .planner()
                .plan(
                    "open notes",
                    &PlanContext {
                        open: Vec::new(),
                        known: Vec::new(),
                        apps: vec!["so.tilo.notes".into()],
                    },
                )
                .unwrap();
            assert!(matches!(steps[0].action, StepAction::Open));
            return;
        }
        for (preset, tier, effort) in [
            (Some("fastest"), Some("priority"), Some("low")),
            (None, None, None),
            (Some("invalid"), None, None),
        ] {
            let offer = open_notes().offers[0].id();
            let (url, sent) = endpoint(vec![plan_answer("c1", &offer)]);
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "model::tests::host_planner_resolves_environment_hints",
                ])
                .env(CHILD_ENDPOINT, url)
                .env_remove(crate::profile::PRESET_ENV)
                .env_remove(crate::profile::MODEL_CLASS_ENV)
                .env_remove(crate::profile::SERVICE_TIER_ENV)
                .env_remove(crate::profile::REASONING_EFFORT_ENV);
            if let Some(preset) = preset {
                child.env(crate::profile::PRESET_ENV, preset);
            }
            let output = child.output().unwrap();
            assert!(output.status.success(), "{output:?}");
            let (_, body) = sent
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            let body: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(
                body.get("service_tier").and_then(serde_json::Value::as_str),
                tier
            );
            assert_eq!(
                body.get("reasoning_effort")
                    .and_then(serde_json::Value::as_str),
                effort
            );
            assert_eq!(sent.try_iter().count(), 0);
        }
    }

    #[test]
    fn preset_hints_reach_the_provider_without_changing_the_plan() {
        let offer = open_notes().offers[0].id();
        let (url, sent) = endpoint(vec![plan_answer("c1", &offer)]);
        let profile = crate::profile::Profile::resolve("mini", &|name| {
            (name == crate::profile::PRESET_ENV).then(|| "fastest".into())
        });
        let answer = standard(&url)
            .with_profile(profile)
            .choose("open notes", &open_notes(), MOST_STEPS)
            .unwrap();
        assert_eq!(answer.actions, vec![offer]);
        let requests: Vec<_> = sent.try_iter().collect();
        assert_eq!(requests.len(), 1, "presets never duplicate requests");
        let body: serde_json::Value = serde_json::from_str(&requests[0].1).unwrap();
        assert_eq!(body["service_tier"], "priority");
        assert_eq!(body["reasoning_effort"], "low");
        assert_eq!(body["max_tokens"], TURN_OUTPUT_TOKENS);
    }

    #[test]
    fn metered_targets_strip_preset_hints_before_sending() {
        let offer = open_notes().offers[0].id();
        let (url, sent) = endpoint(vec![plan_answer("c1", &offer)]);
        let answer = OpenAiModel::new(
            &url,
            CLOUD_MODEL,
            ApiKey::None,
            std::time::Duration::from_secs(30),
            ApiTarget::CloudMetered,
        )
        .with_profile(crate::profile::Profile::preset(
            crate::profile::ModelClass::Small,
            crate::profile::Goal::Fastest,
        ))
        .choose("open notes", &open_notes(), MOST_STEPS)
        .unwrap();
        assert_eq!(answer.actions, vec![offer]);
        let requests: Vec<_> = sent.try_iter().collect();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_str(&requests[0].1).unwrap();
        assert!(body.get("service_tier").is_none(), "{body}");
        assert!(body.get("reasoning_effort").is_none(), "{body}");
        assert_eq!(body["max_completion_tokens"], TURN_OUTPUT_TOKENS);
        assert!(requests[0].0.contains("idempotency-key: "));
    }

    #[test]
    fn default_planners_send_no_optional_hints() {
        let offer = open_notes().offers[0].id();
        let (url, sent) = endpoint(vec![plan_answer("c1", &offer)]);
        standard(&url)
            .choose("open notes", &open_notes(), MOST_STEPS)
            .unwrap();
        let (_, body) = sent.recv().unwrap();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(body.get("service_tier").is_none(), "{body}");
        assert!(body.get("reasoning_effort").is_none(), "{body}");
    }

    /// An id the conversation never showed is refused back to the model with
    /// what to name instead; the corrected plan is taken. The tools are the
    /// same bytes on every turn, so the prompt prefix stays cacheable.
    #[test]
    fn an_unshown_operation_is_refused_back_to_the_model_with_stable_tools() {
        let offer = open_notes().offers[0].id();
        let invented = "app.so.tilo.sheets#open";
        let (url, sent) = endpoint(vec![plan_answer("c1", invented), plan_answer("c2", &offer)]);
        let answer = standard(&url)
            .choose("open notes then add a note", &open_notes(), MOST_STEPS)
            .unwrap();
        assert_eq!(answer.actions, vec![offer.clone()]);
        let requests: Vec<serde_json::Value> = sent
            .try_iter()
            .map(|(_, body)| serde_json::from_str(&body).unwrap())
            .collect();
        assert_eq!(requests.len(), 2);
        let refusal = &requests[1]["messages"][3];
        assert_eq!(refusal["role"], "tool");
        assert_eq!(refusal["tool_call_id"], "c1");
        let text = refusal["content"].as_str().unwrap();
        assert!(text.contains(invented), "{text}");
        assert!(text.contains("nothing was performed"), "{text}");
        assert!(
            text.contains(&format!("Listed ids for that action: {offer}")),
            "{text}"
        );
        let tools = |request: &serde_json::Value| serde_json::to_string(&request["tools"]).unwrap();
        assert_eq!(tools(&requests[0]), tools(&requests[1]));
        assert!(!tools(&requests[0]).contains("\"enum\""));
        assert_eq!(requests[0]["messages"][0], requests[1]["messages"][0]);
    }

    /// An endpoint that keeps each connection open and answers `answers` in
    /// order across them, reporting how many requests each connection
    /// carried (one count per accepted connection).
    fn keep_alive_endpoint(
        answers: Vec<String>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<usize>>>) {
        use std::io::{BufRead as _, BufReader, Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let carried = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let answers = std::sync::Arc::new(std::sync::Mutex::new(
            answers
                .into_iter()
                .collect::<std::collections::VecDeque<_>>(),
        ));
        let counts = carried.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let connection = {
                    let mut counts = counts.lock().unwrap();
                    counts.push(0_usize);
                    counts.len().saturating_sub(1)
                };
                let (answers, counts) = (answers.clone(), counts.clone());
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.unwrap());
                    loop {
                        let mut length = 0;
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                                return;
                            }
                            if let Some(value) =
                                line.to_ascii_lowercase().strip_prefix("content-length:")
                            {
                                length = value.trim().parse().unwrap();
                            }
                            if line == "\r\n" {
                                break;
                            }
                        }
                        let mut body = vec![0; length];
                        reader.read_exact(&mut body).unwrap();
                        let mut served = counts.lock().unwrap();
                        served[connection] = served[connection].saturating_add(1);
                        drop(served);
                        let answer = answers.lock().unwrap().pop_front().unwrap();
                        reader.get_mut().write_all(answer.as_bytes()).unwrap();
                    }
                });
            }
        });
        (url, carried)
    }

    /// Later turns of a task go over a connection an earlier turn opened:
    /// the model keeps one client, not one per call (which would carry
    /// exactly one request on every connection).
    #[test]
    fn later_turns_reuse_an_earlier_turns_connection() {
        let offer = open_notes().offers[0].id();
        let list = serde_json::json!({"choices": [{"message": {"role": "assistant", "content": null,
            "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "list_operations",
                "arguments": "{\"application\":\"so.tilo.notes\"}"}}]}, "finish_reason": "tool_calls"}]});
        let plan = serde_json::json!({"choices": [{"message": {"role": "assistant", "content": null,
            "tool_calls": [{"id": "c2", "type": "function", "function": {"name": "plan",
                "arguments": serde_json::json!({"steps": [{"operation": offer, "arguments": {}}],
                    "why": "open", "then": null}).to_string()}}]}, "finish_reason": "tool_calls"}]});
        let kept_open = |body: &serde_json::Value| {
            let body = body.to_string();
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
        };
        let mut second_list = list.clone();
        second_list["choices"][0]["message"]["tool_calls"][0]["id"] = "c-middle".into();
        let (url, carried) = keep_alive_endpoint(vec![
            kept_open(&list),
            kept_open(&second_list),
            kept_open(&plan),
        ]);
        let mut model = OpenAiModel::new(
            &url,
            "test-model",
            ApiKey::None,
            std::time::Duration::from_secs(30),
            ApiTarget::Standard,
        );
        let answer = model
            .choose("open notes", &open_notes(), MOST_STEPS)
            .unwrap();
        assert_eq!(answer.actions, vec![offer]);
        let carried = carried.lock().unwrap().clone();
        assert_eq!(carried.iter().sum::<usize>(), 3, "{carried:?}");
        // A connection opened speculatively may carry nothing; one turn's
        // connection must have carried a later turn too.
        assert!(carried.iter().any(|&requests| requests >= 2), "{carried:?}");
    }

    #[test]
    fn running_out_of_credit_ends_the_task_with_a_plain_reason() {
        let (url, sent) = endpoint(vec![http(
            "402 Payment Required",
            r#"{"error":"Included credit is insufficient for this request","code":"insufficient_credit"}"#,
        )]);
        let mut model = OpenAiModel::new(
            &url,
            CLOUD_MODEL,
            ApiKey::None,
            std::time::Duration::from_secs(30),
            ApiTarget::CloudMetered,
        );
        let error = model
            .choose("add a note", &open_notes(), MOST_STEPS)
            .unwrap_err();
        assert!(error.starts_with(openai::OUT_OF_CREDITS), "{error}");
        assert_eq!(sent.try_iter().count(), 1);
    }

    #[test]
    fn typed_credit_failure_maps_to_cli_without_retry_or_provider_text() {
        use tilo_model_gateway::host_backend::protocol::{ErrorKind, ModelError};

        let expected = cli_model_error(&ModelError::new(ErrorKind::CreditsExhausted));
        assert!(expected.starts_with(openai::OUT_OF_CREDITS), "{expected}");
        assert!(expected.contains("choose another model"), "{expected}");
        // Classification comes from the machine code, not the provider's prose.
        let (url, sent) = endpoint(vec![http(
            "402 Payment Required",
            r#"{"error":"private upstream detail","code":"insufficient_credit"}"#,
        )]);
        let error = OpenAiModel::new(
            &url,
            CLOUD_MODEL,
            ApiKey::None,
            std::time::Duration::from_secs(30),
            ApiTarget::CloudMetered,
        )
        .choose("add a note", &open_notes(), MOST_STEPS)
        .unwrap_err();
        assert_eq!(error, expected);
        assert!(!error.contains("private upstream detail"));
        assert_eq!(
            sent.try_iter().count(),
            1,
            "credit failures are not retried"
        );

        let other = ModelError::new(ErrorKind::Authentication);
        assert_eq!(cli_model_error(&other), other.to_string());
    }

    #[test]
    fn a_clause_names_an_app_whose_name_has_two_words() {
        let clause = words("in the word processor replace the selection");
        assert!(names_app(&clause, "so.tilo.word-processor"));
        assert!(!names_app(&clause, "so.tilo.sheets"));
        assert!(names_app(&words("add a sheet in sheets"), "so.tilo.sheets"));
    }
}
