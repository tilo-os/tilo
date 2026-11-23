//! A workspace's model providers on the host: the ready-made providers a
//! page adds by name, the list of models the host asks a provider for with
//! the key it keeps, and the model each kind of work runs on.
//!
//! The store is the one [`super::person_settings`] keeps: the workspace's
//! model configuration and vault ([`Workspaces::workspace_root`]), reached as
//! the workspace's policy says ([`Store::MODELS`]). Here is what changes it
//! for a page, and the one outbound request those changes make: a provider's
//! own list of models, asked by the host because only it holds the key.
//!
//! A member names a provider's address, so the host calls an address a
//! person typed. Unless its operator lets it call its own network
//! ([`Workspaces::call_private_model_addresses`], the one leave the model
//! call itself reads), it keeps and asks only https addresses on the
//! internet, checked again at the lookup the connection dials, and it
//! follows no redirect.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agent_provider_config::presets::{Authentication, Preset};
use agent_provider_config::protocol::{ModelError, ModelRequest, ModelResponse};
use agent_provider_config::{
    AgentModel, EndpointLocation, Interaction, ModelDescriptor, ProviderConfig, ProviderProfile,
    capabilities, transform,
};
use serde_json::Value;
use tilo_actions::remote::person_models::{
    self, AddModel, AddPreset, BeginSignIn, ListedModels, ListingProblem, ModelProviders,
    ProviderView, SetAssignment, SetAssignments, SetDefault, SetKey, SetReasoning, WaitSignIn,
};
use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest};
use tilo_net::proxy::host::{
    Destinations, Lookup, check_destination, guarded_client, system_lookup,
};
use tilo_user_identity::DeviceCert;

use super::Workspaces;
use super::person_model::Models;
use super::person_settings::{
    ProviderKeys, add_provider, answer, argument, provider_argument, provider_config, providers,
    remove_key, remove_provider, save_provider_config, set_assignment, set_key, view_of,
};
use super::workspace_stores::Store;

/// The most of a provider's list of models read.
const MAX_MODEL_LIST_BYTES: usize = 2 * 1024 * 1024;
/// How long a provider may take to list its models.
const MODEL_LIST_TIMEOUT: Duration = Duration::from_secs(20);
/// The longest a window's turn of the person's model may take.
const TURN_TIMEOUT: Duration = Duration::from_secs(120);
/// The longest id a model named by hand has.
const MAX_MODEL_ID_BYTES: usize = 256;

/// What the host calls for a person's providers, and how it looks a name
/// up.
#[derive(Clone)]
pub(super) struct Reach {
    pub(super) destinations: Destinations,
    lookup: Lookup,
}

impl Reach {
    /// What a server with `models` calls: https addresses on the internet,
    /// and its own network too where its operator lets it.
    pub(super) fn of(models: &Models) -> Self {
        Self {
            destinations: if models.private_addresses() {
                Destinations::Any
            } else {
                Destinations::Public
            },
            lookup: system_lookup(),
        }
    }
}

impl Workspaces {
    /// What this host keeps and asks for its workspaces' providers: https
    /// addresses on the internet, and its own network too where its
    /// operator lets it call there.
    pub(super) fn model_reach(&self) -> Reach {
        Reach::of(&self.inner.models)
    }

    /// A provider's sign-in, as the person's page drives it: begun, waited for or stopped. Answers
    /// the action's answer for one of those, and nothing for any other action.
    async fn sign_in_invoke(
        &self,
        action: &str,
        person: [u8; 32],
        here: &str,
        reach: &Reach,
        request: &InvokeRequest,
    ) -> Option<Result<Value, ActionCallError>> {
        match action {
            person_models::BEGIN_SIGN_IN => {
                // Named nothing, a sign-in is to the plan.
                let beginning: BeginSignIn =
                    if matches!(request.argument, tilo_actions::remote::InvokeArgument::None) {
                        BeginSignIn::default()
                    } else {
                        match argument(request, "a sign-in to begin") {
                            Ok(beginning) => beginning,
                            Err(error) => return Some(Err(error)),
                        }
                    };
                Some(answer(
                    &self
                        .begin_openai_sign_in((person, here), beginning, reach.clone())
                        .await,
                ))
            }
            person_models::WAIT_SIGN_IN => {
                let waiting: WaitSignIn = match argument(request, "a sign-in to wait for") {
                    Ok(waiting) => waiting,
                    Err(error) => return Some(Err(error)),
                };
                Some(answer(&self.wait_openai_sign_in(person, &waiting).await))
            }
            person_models::CANCEL_SIGN_IN => {
                let cancelling: WaitSignIn = match argument(request, "a sign-in to stop") {
                    Ok(cancelling) => cancelling,
                    Err(error) => return Some(Err(error)),
                };
                self.cancel_openai_sign_in(person, &cancelling);
                Some(answer(&Ok(())))
            }
            _ => None,
        }
    }

    /// Answer a [`person_models::PROVIDER`] request.
    pub(super) async fn person_models_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let action = request.target.address.action.as_str();
        let effect = match action {
            // Asking a model reads with the key the workspace keeps.
            person_models::LIST | person_models::ASK | person_models::WAIT_SIGN_IN => {
                tilo_action_policy::ActionKind::Read
            }
            _ => tilo_action_policy::ActionKind::Undoable,
        };
        let reach = self
            .reaching_allowed(
                (workspace_id, caller, certificate),
                Store::MODELS,
                action,
                effect,
 request,
            )
            .await?;
        if action == person_models::ASK {
            let asked: ModelRequest = argument(request, "a model request")?;
            return self
                .ask_workspace_model(reach.workspace, &asked, &mut |_| {})
                .await;
        }
        let here = reach.workspace.to_string();
        let root = self.workspace_root(&here);
        let person = reach.person;
        let by = reach.by();
        let keys = ProviderKeys::of(self.vault_at(&here), &by);
        let reach = self.model_reach();
        if let Some(answered) = self
            .sign_in_invoke(action, person, &here, &reach, request)
            .await
        {
            return answered;
        }
        let changed = match request.target.address.action.as_str() {
            person_models::LIST => return answer(&Ok(providers(&root))),
            person_models::ADD_PROVIDER => {
                let adding: person_models::AddProvider = argument(request, "a provider to add")?;
                answer(&add_provider(&root, &adding, reach.destinations))
            }
            person_models::ADD_PRESET => {
                let adding: AddPreset = argument(request, "a provider to add")?;
                answer(&add_preset(&root, &adding))
            }
            person_models::SET_KEY => {
                let setting: SetKey = argument(request, "a key to keep")?;
                answer(&take_key((keys, &root), setting, None, &reach).await)
            }
            person_models::REMOVE_KEY => {
                let provider = provider_argument(request)?;
                answer(&remove_key((keys, &root), &provider).await)
            }
            person_models::REMOVE_PROVIDER => {
                let provider = provider_argument(request)?;
                answer(&remove_provider((keys, &root), &provider).await)
            }
            person_models::REFRESH_MODELS => {
                let provider = provider_argument(request)?;
                answer(&refresh_models((keys, &root), &provider, &reach).await)
            }
            person_models::ADD_MODEL => {
                let adding: AddModel = argument(request, "a model to add")?;
                answer(&add_model(&root, &adding))
            }
            person_models::SET_DEFAULT => {
                let choosing: SetDefault = argument(request, "a model choice")?;
                answer(&set_default(&root, &choosing))
            }
            person_models::SET_ASSIGNMENT => {
                let choosing: SetAssignment = argument(request, "a model choice")?;
                answer(&set_assignment(&root, choosing))
            }
            person_models::SET_ASSIGNMENTS => {
                let choosing: SetAssignments = argument(request, "a model choice")?;
                answer(&set_assignments(&root, choosing))
            }
            person_models::SET_REASONING => {
                let setting: SetReasoning = argument(request, "a thinking level to keep")?;
                answer(&set_reasoning(&root, &setting))
            }
            person_models::LINK_THIS_COMPUTER => answer(&self.link_from_this_computer(&here)),
            _ => return Err(ActionCallError::UnknownAction),
        };
        if changed.is_ok() {
            self.person_settings_changed();
        }
        changed
    }
}

impl Workspaces {
    /// [`person_models::LINK_THIS_COMPUTER`] in `workspace`: this computer's Agent models choice,
    /// read from its own settings, made the workspace's.
    fn link_from_this_computer(&self, workspace: &str) -> Result<ModelProviders, String> {
        if !self.inner.models.on_a_computer() {
            return Err(
                "Only a computer of yours links its own models; this workspace is kept elsewhere"
                    .into(),
            );
        }
        self.link_this_computer(
            workspace,
            &ProviderConfig::load(),
            super::this_computer::Linking::Chosen,
        )?;
        Ok(providers(&self.workspace_root(workspace)))
    }

    /// One turn of the workspace's model, for a window that has no model
    /// service of its own (a page): the model the workspace chose for a
    /// request, asked with the key its vault keeps for its provider, as every
    /// model call this host makes in it is. The answer is the turn or the
    /// reason there is none, as a model failure the window shows. One
    /// [`person_models::ASK`] of `caller`, its answer's stream events told to
    /// `events` as the model writes them.
    pub(super) async fn ask_streamed(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
        events: super::person_model::Events<'_>,
    ) -> Result<Value, ActionCallError> {
        let reach = self
            .reaching_allowed(
                (workspace_id, caller, certificate),
                Store::MODELS,
                person_models::ASK,
                tilo_action_policy::ActionKind::Read,
 request,
            )
            .await?;
        let asked: ModelRequest = argument(request, "a model request")?;
        self.ask_workspace_model(reach.workspace, &asked, events)
            .await
    }

    async fn ask_workspace_model(
        &self,
        workspace: collab_types::WorkspaceId,
        asked: &ModelRequest,
        events: super::person_model::Events<'_>,
    ) -> Result<Value, ActionCallError> {
        let model = self.workspace_model(workspace, Interaction::Request);
        let now = tilo_clock::now();
        let deadline = now.checked_add(TURN_TIMEOUT).unwrap_or(now);
        let answered: Result<ModelResponse, ModelError> = model
            .answer(asked, deadline, events)
            .await
            .map(|answer| answer.response)
            .map_err(|refusal| refusal.model_error());
        serde_json::to_value(answered).map_err(|error| ActionCallError::Refused {
            reason: error.to_string(),
        })
    }
}

fn not_kept() -> String {
    "That provider is not kept here".to_owned()
}

mod openai;
pub use openai::OpenAiSignIn;
pub(super) use openai::{Grant, SignIns};

/// Where a provider at `endpoint` is, when this host keeps one there: an
/// https address on the internet, or with [`Destinations::Any`] one on the
/// host's own network too, which alone is reached without TLS.
pub(super) fn kept_location(
    endpoint: &str,
    destinations: Destinations,
) -> Result<EndpointLocation, String> {
    let not_a_url = || "A provider's endpoint is a URL".to_owned();
    let url = tilo_net::Url::parse(endpoint).map_err(|_| not_a_url())?;
    let location = EndpointLocation::of_endpoint(endpoint).ok_or_else(not_a_url)?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(not_a_url());
    }
    let tls = url.scheme() == "https";
    let allowed = match destinations {
        Destinations::Public => {
            tls && location == EndpointLocation::Hosted
                && check_destination(&url, destinations).is_ok()
        }
        Destinations::Any => tls || location != EndpointLocation::Hosted,
    };
    if allowed {
        Ok(location)
    } else {
        Err("A provider's endpoint is an https address on the internet".to_owned())
    }
}

/// Whether a host adds `preset` by name: a hosted provider that takes an API
/// key at an address of its own. A sign-in runs on a device, a local server
/// is on one, and an endpoint with a part to fill in is typed.
fn added_by_name(preset: &Preset) -> bool {
    preset.authentication() == Authentication::ApiKey
        && preset.location() == EndpointLocation::Hosted
        && !preset.endpoint_editable()
}

fn add_preset(root: &Path, adding: &AddPreset) -> Result<ProviderView, String> {
    let preset = Preset::from_slug(adding.preset.trim())
        .filter(added_by_name)
        .ok_or_else(|| "That provider is not one this host adds by name".to_owned())?;
    let profile = preset.profile();
    let mut config = provider_config(root);
    let view = view_of(&profile);
    config.profiles.push(profile);
    save_provider_config(root, &config)?;
    Ok(view)
}

/// What a provider says of a key it is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Said {
    /// It answered the request that carried the key.
    Takes,
    /// It turned the key away, or no request can carry the key.
    Refuses,
    /// Nothing there says of a key: the provider lists no models, or this
    /// server does not call its address.
    Nothing,
    /// It could not be asked now: it did not answer, or answered neither
    /// yes nor no.
    NotNow,
}

/// Keep `setting`'s key for its provider: what [`person_models::SET_KEY`]
/// and [`person_models::SET_KEY_FROM_ITEM`] do with the key they are handed.
///
/// The provider is asked about a new key first ([`asked_about`]), and a key
/// it turns away is never kept: the person is told so, and a key kept
/// before stays in use. A key kept already also stays when the provider
/// cannot be asked about the new one now. A first key the provider cannot
/// be asked about now is kept, since no key is lost by it, and the
/// provider's list of models says of it once it answers
/// ([`refresh_models`]). Where nothing at the provider says of a key the
/// new one is kept as typed, since nothing else could ever replace a key
/// there. No refusal carries a key.
pub(super) async fn take_key(
    (keys, root): (ProviderKeys<'_>, &Path),
    setting: SetKey,
    kept_as: Option<&tilo_actions::remote::person_vault::ItemRef>,
    reach: &Reach,
) -> Result<ProviderView, String> {
    let profile = provider_config(root)
        .profiles
        .into_iter()
        .find(|profile| profile.id == setting.provider)
        .ok_or_else(not_kept)?;
    if profile.sign_in.is_some() {
        return Err(format!(
            "{} is signed in to, not reached with a key.",
            profile.display_label()
        ));
    }
    let kept = keys.get(&profile).await?;
    let replacing = kept.is_some();
    // The kept key typed again replaces nothing and asks nobody. The two
    // are compared by their hashes, which compare in constant time.
    let new = !setting.key.trim().is_empty()
        && kept.is_none_or(|kept| {
            blake3::hash(kept.as_bytes()) != blake3::hash(setting.key.as_bytes())
        });
    if new {
        let name = profile.display_label();
        let still = if replacing {
            " The key kept before is still in use."
        } else {
            ""
        };
        match asked_about(&profile, &setting.key, reach).await {
            Said::Takes | Said::Nothing => {}
            Said::NotNow if !replacing => {}
            Said::Refuses => return Err(format!("{name} did not accept this key.{still}")),
            Said::NotNow => {
                return Err(format!(
                    "{name} could not be asked about this key now.{still}"
                ));
            }
        }
    }
    set_key((keys, root), setting, kept_as).await
}

/// Where `profile`'s provider is asked with a key so that its answer says of
/// the key: its list of models, which the linking asks for too. `OpenRouter`
/// lists its models to anyone, key or none, and says of a key at an address
/// of its own.
fn key_question(profile: &ProviderProfile) -> Option<String> {
    let base = profile.endpoint.trim_end_matches('/');
    if base == Preset::OPENROUTER.endpoint() {
        return Some(format!("{base}/key"));
    }
    transform::models_url(profile)
}

/// What `profile`'s provider says of `key`, asked once at
/// [`key_question`].
async fn asked_about(profile: &ProviderProfile, key: &str, reach: &Reach) -> Said {
    let Some(url) = key_question(profile) else {
        return Said::Nothing;
    };
    let status = match asked(profile, &url, key, reach, false).await {
        Ok((status, _)) => status,
        Err(ListingProblem::KeyRefused) => return Said::Refuses,
        Err(ListingProblem::Unreachable | ListingProblem::Unreadable) => return Said::NotNow,
        Err(ListingProblem::NoKey | ListingProblem::NoList | ListingProblem::NotAllowed) => {
            return Said::Nothing;
        }
    };
    match status {
        200..=299 => Said::Takes,
        // Google answers 400 to a key it does not know.
        400 | 401 | 403 => Said::Refuses,
        404 | 405 => Said::Nothing,
        _ => Said::NotNow,
    }
}

/// Ask `provider` for its models with the key kept for it, and keep the
/// list it answers. A list that cannot be read is an answer too: it says
/// why, and what was kept stays.
async fn refresh_models(
    (keys, root): (ProviderKeys<'_>, &Path),
    provider: &str,
    reach: &Reach,
) -> Result<ListedModels, String> {
    let profile = provider_config(root)
        .profiles
        .into_iter()
        .find(|profile| profile.id == provider)
        .ok_or_else(not_kept)?;
    let mut key = keys.get(&profile).await?;
    // A signed-in account's models are asked for with its access token.
    if profile.sign_in.is_some() {
        key = key
            .and_then(|kept| Grant::read(&kept))
            .map(|grant| grant.access());
    }
    let listed = match listing(&profile, key.as_deref().map(String::as_str), reach).await {
        Ok(listed) => listed,
        Err(problem) => {
            return Ok(ListedModels {
                provider: view_of(&profile),
                problem: Some(problem),
            });
        }
    };
    drop(key);
    // The provider took its time: the list goes into the configuration as
    // it is now.
    let mut config = provider_config(root);
    let chosen = chosen_models(&config, provider);
    let kept = config
        .profiles
        .iter_mut()
        .find(|profile| profile.id == provider)
        .ok_or_else(not_kept)?;
    kept.models = merged(std::mem::take(&mut kept.models), listed, &chosen);
    let kept = kept.clone();
    save_provider_config(root, &config)?;
    Ok(ListedModels {
        provider: view_of(&kept),
        problem: None,
    })
}

/// The models of `provider` a choice names: its default, and what a kind of
/// work or an agent runs on.
fn chosen_models(config: &ProviderConfig, provider: &str) -> BTreeSet<String> {
    let default = config
        .profiles
        .iter()
        .find(|profile| profile.id == provider)
        .and_then(|profile| profile.default_model.clone());
    config
        .interaction_models
        .values()
        .chain(config.agent_models.values())
        .filter(|chosen| chosen.profile == provider)
        .map(|chosen| chosen.model.clone())
        .chain(default)
        .collect()
}

/// The models a provider lists now. One kept before keeps what was set on
/// it, and one a choice names stays though the list dropped it: a choice
/// never turns into another model unasked, and the provider's own refusal
/// names a model it retired.
fn merged(
    before: Vec<ModelDescriptor>,
    listed: Vec<ModelDescriptor>,
    chosen: &BTreeSet<String>,
) -> Vec<ModelDescriptor> {
    let mut models: Vec<ModelDescriptor> = listed
        .into_iter()
        .map(|model| {
            let Some(stored) = before.iter().find(|stored| stored.id == model.id) else {
                return model;
            };
            let mut stored = stored.clone();
            capabilities::refresh(&mut stored, &model);
            stored.label = model.label;
            stored
        })
        .collect();
    for stored in before {
        if chosen.contains(&stored.id) && !models.iter().any(|model| model.id == stored.id) {
            models.push(stored);
        }
    }
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models
}

/// The models `profile`'s provider lists, asked with `key`.
async fn listing(
    profile: &ProviderProfile,
    key: Option<&str>,
    reach: &Reach,
) -> Result<Vec<ModelDescriptor>, ListingProblem> {
    let key = key.ok_or(ListingProblem::NoKey)?;
    let url = transform::models_url(profile).ok_or(ListingProblem::NoList)?;
    let (status, body) = asked(profile, &url, key, reach, true).await?;
    match status {
        200..=299 => {}
        401 | 403 => return Err(ListingProblem::KeyRefused),
        404 | 405 => return Err(ListingProblem::NoList),
        _ => return Err(ListingProblem::Unreadable),
    }
    transform::parse_models(&String::from_utf8_lossy(&body)).map_err(|_| ListingProblem::Unreadable)
}

/// What `profile`'s provider answers at `url` to a request that carries
/// `key`: the answer's status, and with `read` the body of an answer that
/// succeeded. Every request this file sends a provider is sent here, and the
/// key goes into that request's own header and nowhere else.
async fn asked(
    profile: &ProviderProfile,
    url: &str,
    key: &str,
    reach: &Reach,
    read: bool,
) -> Result<(u16, Vec<u8>), ListingProblem> {
    let url = tilo_net::Url::parse(url).map_err(|_| ListingProblem::NotAllowed)?;
    if kept_location(&profile.endpoint, reach.destinations).is_err()
        || check_destination(&url, reach.destinations).is_err()
    {
        return Err(ListingProblem::NotAllowed);
    }
    let headers =
        transform::profile_headers(profile, Some(key)).map_err(|_| ListingProblem::NoKey)?;
    let client =
        guarded_client(reach.destinations, Arc::clone(&reach.lookup)).map_err(|error| {
            tracing::warn!(%error, "no client to ask a model provider with");
            ListingProblem::Unreachable
        })?;
    let mut request = client.get(url).timeout(MODEL_LIST_TIMEOUT);
    for header in headers {
        // A key no header can carry is no key the provider issued.
        let mut value = tilo_net::HeaderValue::from_str(header.value.as_str())
            .map_err(|_| ListingProblem::KeyRefused)?;
        value.set_sensitive(header.secret);
        request = request.header(header.name.as_ref(), value);
    }
    let unreachable = |error: &dyn std::fmt::Display| {
        tracing::info!(provider = %profile.id, %error, "a model provider did not list its models");
        ListingProblem::Unreachable
    };
    // An endpoint a person typed may carry a token of its own: the address
    // stays out of the log.
    let mut response = request
        .send()
        .await
        .map_err(|error| unreachable(&error.without_url()))?;
    let status = response.status().as_u16();
    let mut body = Vec::new();
    if !read || !(200..=299).contains(&status) {
        return Ok((status, body));
    }
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| unreachable(&error.without_url()))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_MODEL_LIST_BYTES {
            return Err(ListingProblem::Unreadable);
        }
        body.extend_from_slice(&chunk);
    }
    Ok((status, body))
}

fn add_model(root: &Path, adding: &AddModel) -> Result<ProviderView, String> {
    let id = adding.model.trim();
    if id.is_empty() || id.len() > MAX_MODEL_ID_BYTES || id.contains(char::is_control) {
        return Err("A model is named by its id at the provider".into());
    }
    let mut config = provider_config(root);
    let profile = config
        .profiles
        .iter_mut()
        .find(|profile| profile.id == adding.provider)
        .ok_or_else(not_kept)?;
    if !profile.models.iter().any(|model| model.id == id) {
        profile.models.push(ModelDescriptor::named(id));
        profile.models.sort_by(|a, b| a.id.cmp(&b.id));
    }
    let profile = profile.clone();
    save_provider_config(root, &config)?;
    Ok(view_of(&profile))
}

/// Keep how hard `setting`'s provider is asked to think, or with no level,
/// leave it to the provider again. The level is the person's own to choose:
/// nothing else writes it, and where none is kept the provider is told
/// nothing. A provider is told in the field it takes a level in; one spoken
/// to in a way that has no such field here says so and keeps nothing.
fn set_reasoning(root: &Path, setting: &SetReasoning) -> Result<ProviderView, String> {
    use agent_provider_config::{REASONING_EFFORTS, ReasoningParam, ReasoningSetting};

    let mut config = provider_config(root);
    let profile = config
        .profiles
        .iter_mut()
        .find(|profile| profile.id == setting.provider)
        .ok_or_else(not_kept)?;
    profile.reasoning = match setting.effort.as_deref().map(str::trim) {
        None => None,
        Some(effort) => {
            if !REASONING_EFFORTS.contains(&effort) {
                return Err(format!(
                    "A thinking level is one of {}",
                    REASONING_EFFORTS.join(", ")
                ));
            }
            if profile.protocol != agent_provider_config::ProviderProtocol::OpenAiCompatible {
                return Err(
                    "A thinking level cannot be chosen for this provider yet: it is told to OpenAI-compatible providers only"
                        .into(),
                );
            }
            // The field a provider already takes its level in stays; a first
            // choice goes where the provider documents it.
            let router = Preset::OPENROUTER.endpoint().trim_end_matches('/');
            let param = match profile.reasoning.as_ref().map(|kept| kept.param) {
                Some(param) if param != ReasoningParam::Omit => param,
                _ if profile.endpoint.trim().trim_end_matches('/') == router => {
                    ReasoningParam::ReasoningObject
                }
                _ => ReasoningParam::ReasoningEffort,
            };
            Some(ReasoningSetting {
                param,
                effort: effort.to_owned(),
            })
        }
    };
    let profile = profile.clone();
    save_provider_config(root, &config)?;
    Ok(view_of(&profile))
}

fn set_default(root: &Path, choosing: &SetDefault) -> Result<ModelProviders, String> {
    let mut config = provider_config(root);
    let profile = config
        .profiles
        .iter_mut()
        .find(|profile| profile.id == choosing.provider)
        .ok_or_else(not_kept)?;
    if !profile
        .models
        .iter()
        .any(|model| model.id == choosing.model)
    {
        return Err("That provider does not offer this model".into());
    }
    profile.default_model = Some(choosing.model.clone());
    config.default_profile = Some(choosing.provider.clone());
    save_provider_config(root, &config)?;
    Ok(providers(root))
}

/// One model for every kind `choosing` names, or none of them changed.
fn set_assignments(root: &Path, choosing: SetAssignments) -> Result<ModelProviders, String> {
    if choosing.kinds.is_empty() {
        return Err("A model choice names the kinds of work it is for".into());
    }
    let chosen = match (choosing.provider, choosing.model) {
        (Some(profile), Some(model)) => Some(AgentModel { profile, model }),
        (None, None) => None,
        _ => return Err("A model choice names both a provider and its model".into()),
    };
    let mut config = provider_config(root);
    for kind in &choosing.kinds {
        let kind =
            Interaction::from_id(kind).ok_or_else(|| "That is not a kind of work".to_owned())?;
        config
            .set_interaction_model(kind, chosen.clone())
            .map_err(str::to_owned)?;
    }
    save_provider_config(root, &config)?;
    Ok(providers(root))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use std::net::IpAddr;

    use test_http::{Reply, Responder};
    use tilo_actions::remote::person_models::Protocol;

    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use super::super::vault::VaultAt;
    use super::*;

    const KEY: &str = "sk-fake-provider-key";
    const LIST: &str = r#"{"data":[{"id":"small"},{"id":"large","display_name":"Large"}]}"#;

    fn temp_root() -> tempfile::TempDir {
        tempfile::tempdir().expect("a temporary root")
    }

    /// The host's own network reached, as its operator may let it.
    fn any() -> Reach {
        Reach {
            destinations: Destinations::Any,
            lookup: system_lookup(),
        }
    }

    fn public() -> Reach {
        Reach {
            destinations: Destinations::Public,
            lookup: system_lookup(),
        }
    }

    /// A provider that lists two models at `path` to whoever shows `shown`
    /// in `header`, and turns everyone else away.
    fn provider(path: &'static str, header: &'static str, shown: String) -> Responder {
        Responder::handle(move |request| {
            if request.method != "GET" || request.target != path {
                return Some(Reply::status(404));
            }
            if request.header(header) != Some(shown.as_str()) {
                return Some(Reply::status(401).body(r#"{"error":"that key is unknown"}"#));
            }
            Some(
                Reply::status(200)
                    .header("Content-Type", "application/json")
                    .body(LIST),
            )
        })
    }

    /// A provider kept at `endpoint`, by its id.
    fn kept(root: &Path, protocol: Protocol, endpoint: &str, destinations: Destinations) -> String {
        add_provider(
            root,
            &person_models::AddProvider {
                name: "Fake provider".into(),
                protocol,
                endpoint: endpoint.into(),
                headers: Vec::new(),
            },
            destinations,
        )
        .expect("the provider is added")
        .id
    }

    /// The workspace at `root` with the vault it keeps: one for each root, as each workspace
    /// keeps one of its own.
    fn at(root: &Path) -> (ProviderKeys<'static>, &Path) {
        type Vaults = BTreeMap<PathBuf, &'static crate::workspaces::vault::TestStores>;
        static VAULTS: Mutex<Vaults> = Mutex::new(BTreeMap::new());
        let vault = *VAULTS
            .lock()
            .unwrap()
            .entry(root.to_owned())
            .or_insert_with(|| Box::leak(Box::default()));
        (ProviderKeys::of(VaultAt::Held(vault), ""), root)
    }

    async fn keep_key(root: &Path, provider: &str, key: &str) {
        set_key(
            at(root),
            SetKey {
                provider: provider.into(),
                key: key.into(),
            },
            None,
        )
        .await
        .expect("the key is kept");
    }

    fn ids(provider: &ProviderView) -> Vec<&str> {
        provider
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect()
    }

    /// The four providers a page offers by name are added as the catalogue
    /// says them, whole; what only a device can run is not added here.
    #[test]
    fn a_ready_made_provider_is_added_as_the_catalogue_says_it() {
        let root = temp_root();
        for (slug, name, protocol, endpoint) in [
            (
                "gemini",
                "Google Gemini",
                Protocol::OpenAiCompatible,
                "https://generativelanguage.googleapis.com/v1beta/openai",
            ),
            (
                "anthropic",
                "Anthropic",
                Protocol::Anthropic,
                "https://api.anthropic.com",
            ),
            (
                "openai",
                "OpenAI",
                Protocol::OpenAiCompatible,
                "https://api.openai.com/v1",
            ),
            (
                "openrouter",
                "OpenRouter",
                Protocol::OpenAiCompatible,
                "https://openrouter.ai/api/v1",
            ),
        ] {
            let added = add_preset(
                root.path(),
                &AddPreset {
                    preset: slug.into(),
                },
            )
            .expect("a provider of the catalogue is added");
            assert_eq!(
                (added.name.as_str(), added.protocol, added.endpoint.as_str()),
                (name, protocol, endpoint)
            );
            assert!(!added.configured, "{slug} has no key yet");
        }
        // The catalogue's whole row is kept, so the host's own call speaks
        // to each provider as it wants: where it lists its models, the API
        // its agent turns take.
        let config = provider_config(root.path());
        let named = |name: &str| {
            config
                .profiles
                .iter()
                .find(|profile| profile.label == name)
                .expect("the provider is kept")
        };
        assert_eq!(
            transform::models_url(named("Anthropic")).as_deref(),
            Some("https://api.anthropic.com/v1/models")
        );
        assert_eq!(
            named("OpenAI").openai_api,
            Preset::OPENAI.profile().openai_api
        );
        assert!(named("OpenAI").openai_api.is_some());

        for slug in [
            "chatgpt",
            "ollama",
            "azure-openai",
            "openai-compatible",
            "nowhere",
            "",
        ] {
            let refused = add_preset(
                root.path(),
                &AddPreset {
                    preset: slug.into(),
                },
            );
            assert!(refused.is_err(), "{slug} is not added by name on a host");
        }
        assert_eq!(providers(root.path()).providers.len(), 4);
    }

    /// The host asks an OpenAI-compatible provider for its models with the
    /// key it keeps, and keeps the list. The key goes to the provider and
    /// into no answer.
    #[tokio::test]
    async fn the_host_asks_a_provider_for_its_models_with_the_key_it_keeps() {
        let root = temp_root();
        let fake = provider("/v1/models", "authorization", format!("Bearer {KEY}"));
        let id = kept(
            root.path(),
            Protocol::OpenAiCompatible,
            &format!("{}/v1", fake.url()),
            Destinations::Any,
        );
        keep_key(root.path(), &id, KEY).await;

        let listed = refresh_models(at(root.path()), &id, &any())
            .await
            .expect("the provider is kept here");
        assert_eq!(listed.problem, None);
        assert_eq!(ids(&listed.provider), ["large", "small"]);
        assert_eq!(listed.provider.models[0].label, "Large");
        let asked = fake.next_request();
        assert_eq!(
            asked.header("authorization"),
            Some(format!("Bearer {KEY}").as_str())
        );
        let answered = serde_json::to_string(&listed).unwrap();
        assert!(!answered.contains(KEY), "{answered}");

        // The list is kept: a reading has it, and asks the provider nothing.
        let read = providers(root.path());
        assert_eq!(ids(&read.providers[0]), ["large", "small"]);
        assert!(fake.drain_requests().is_empty());
    }

    /// Anthropic's API takes the key in its own header and lists its models
    /// under `/v1`.
    #[tokio::test]
    async fn an_anthropic_provider_is_asked_in_its_own_way() {
        let root = temp_root();
        let fake = provider("/v1/models", "x-api-key", KEY.to_owned());
        let id = kept(
            root.path(),
            Protocol::Anthropic,
            &fake.url(),
            Destinations::Any,
        );
        keep_key(root.path(), &id, KEY).await;
        let listed = refresh_models(at(root.path()), &id, &any()).await.unwrap();
        assert_eq!(listed.problem, None);
        assert_eq!(ids(&listed.provider), ["large", "small"]);
        let asked = fake.next_request();
        assert_eq!(asked.header("x-api-key"), Some(KEY));
        assert!(asked.header("anthropic-version").is_some());
    }

    /// A header the person gives a provider (an account or workspace id the
    /// provider asks for beside its key) goes with the host's requests to it.
    #[tokio::test]
    async fn a_providers_own_header_goes_with_the_hosts_requests() {
        let root = temp_root();
        let fake = provider("/v1/models", "x-api-key", KEY.to_owned());
        let id = add_provider(
            root.path(),
            &person_models::AddProvider {
                name: "Fake provider".into(),
                protocol: Protocol::Anthropic,
                endpoint: fake.url(),
                headers: vec![person_models::RequestHeader {
                    name: " account-id ".into(),
                    value: "acct_1".into(),
                }],
            },
            Destinations::Any,
        )
        .expect("the provider is added")
        .id;
        assert_eq!(
            providers(root.path()).providers[0].headers[0].name,
            "account-id"
        );
        keep_key(root.path(), &id, KEY).await;
        let listed = refresh_models(at(root.path()), &id, &any()).await.unwrap();
        assert_eq!(listed.problem, None);
        let asked = fake.next_request();
        assert_eq!(asked.header("account-id"), Some("acct_1"));
        assert_eq!(asked.header("x-api-key"), Some(KEY));
    }

    /// A header that would stand for the key, or that is not a header at
    /// all, is refused when the provider is added.
    #[test]
    fn a_header_that_stands_for_the_key_is_refused() {
        let root = temp_root();
        for name in ["x-api-key", "Authorization", "two words"] {
            let refused = add_provider(
                root.path(),
                &person_models::AddProvider {
                    name: "Fake provider".into(),
                    protocol: Protocol::Anthropic,
                    endpoint: "https://api.example.com".into(),
                    headers: vec![person_models::RequestHeader {
                        name: name.into(),
                        value: "x".into(),
                    }],
                },
                Destinations::Any,
            );
            assert!(refused.is_err(), "{name}");
        }
        assert!(providers(root.path()).providers.is_empty());
    }

    /// A list that cannot be read says why, in a word the page puts its own
    /// sentence to, and the models kept before stay.
    #[tokio::test]
    async fn a_list_that_cannot_be_read_says_why_and_keeps_what_was_kept() {
        let root = temp_root();
        let fake = provider("/v1/models", "authorization", format!("Bearer {KEY}"));
        let id = kept(
            root.path(),
            Protocol::OpenAiCompatible,
            &format!("{}/v1", fake.url()),
            Destinations::Any,
        );
        // No key is kept: the provider is not asked at all.
        let unasked = refresh_models(at(root.path()), &id, &any()).await.unwrap();
        assert_eq!(unasked.problem, Some(ListingProblem::NoKey));
        assert!(fake.drain_requests().is_empty());

        keep_key(root.path(), &id, KEY).await;
        let listed = refresh_models(at(root.path()), &id, &any()).await.unwrap();
        assert_eq!(listed.problem, None);

        // The provider turns a wrong key away.
        keep_key(root.path(), &id, "sk-not-the-key").await;
        let refused = refresh_models(at(root.path()), &id, &any()).await.unwrap();
        assert_eq!(refused.problem, Some(ListingProblem::KeyRefused));
        assert_eq!(ids(&refused.provider), ["large", "small"]);
        assert_eq!(
            ids(&providers(root.path()).providers[0]),
            ["large", "small"]
        );

        // A provider with no list at its address.
        let listless = kept(
            root.path(),
            Protocol::OpenAiCompatible,
            &format!("{}/elsewhere", fake.url()),
            Destinations::Any,
        );
        keep_key(root.path(), &listless, KEY).await;
        let none = refresh_models(at(root.path()), &listless, &any())
            .await
            .unwrap();
        assert_eq!(none.problem, Some(ListingProblem::NoList));

        // One that answers something else.
        let other = Responder::handle(|_| Some(Reply::status(200).body("<html>Welcome</html>")));
        let odd = kept(
            root.path(),
            Protocol::OpenAiCompatible,
            &other.url(),
            Destinations::Any,
        );
        keep_key(root.path(), &odd, KEY).await;
        let unreadable = refresh_models(at(root.path()), &odd, &any()).await.unwrap();
        assert_eq!(unreadable.problem, Some(ListingProblem::Unreadable));

        // One that is not there.
        let gone = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}", listener.local_addr().unwrap())
        };
        let absent = kept(
            root.path(),
            Protocol::OpenAiCompatible,
            &gone,
            Destinations::Any,
        );
        keep_key(root.path(), &absent, KEY).await;
        let unreachable = refresh_models(at(root.path()), &absent, &any())
            .await
            .unwrap();
        assert_eq!(unreachable.problem, Some(ListingProblem::Unreachable));

        // And one nobody added is a refusal, not a list.
        assert!(
            refresh_models(at(root.path()), "nobody's", &any())
                .await
                .is_err()
        );
    }

    /// A provider is asked about a key where its answer says of the key: by
    /// its list of models, and `OpenRouter`, which lists its models to
    /// anyone, at the address where it says of a key. One that lists no
    /// models has nothing that says of a key.
    #[test]
    fn a_key_is_asked_about_where_its_provider_says_of_one() {
        assert_eq!(
            key_question(&Preset::OPENROUTER.profile()).as_deref(),
            Some("https://openrouter.ai/api/v1/key")
        );
        for preset in [Preset::OPENAI, Preset::ANTHROPIC, Preset::GEMINI] {
            let profile = preset.profile();
            assert_eq!(key_question(&profile), transform::models_url(&profile));
            assert!(key_question(&profile).is_some(), "{}", preset.slug());
        }
        let mut listless = Preset::OPENAI.profile();
        listless.models_path = Some(String::new());
        assert_eq!(key_question(&listless), None);
    }

    /// What a provider answers to the request that carries a key says of
    /// the key: yes, no, nothing at all, or not now. A key no request can
    /// carry is one it never issued, and a provider this server does not
    /// call is asked nothing.
    #[tokio::test]
    async fn a_providers_answer_says_of_the_key_it_was_shown() {
        let root = temp_root();
        let fake = Responder::handle(|request| {
            let status = match request.header("authorization") {
                Some("Bearer sk-fake-taken") => 200,
                Some("Bearer sk-fake-malformed") => 400,
                Some("Bearer sk-fake-unknown") => 401,
                Some("Bearer sk-fake-forbidden") => 403,
                Some("Bearer sk-fake-listless") => 404,
                Some("Bearer sk-fake-busy") => 429,
                Some("Bearer sk-fake-broken") => 500,
                _ => return None,
            };
            Some(Reply::status(status))
        });
        let id = kept(
            root.path(),
            Protocol::OpenAiCompatible,
            &format!("{}/v1", fake.url()),
            Destinations::Any,
        );
        let profile = provider_config(root.path())
            .profiles
            .into_iter()
            .find(|profile| profile.id == id)
            .expect("the provider is kept");
        for (key, said) in [
            ("sk-fake-taken", Said::Takes),
            ("sk-fake-malformed", Said::Refuses),
            ("sk-fake-unknown", Said::Refuses),
            ("sk-fake-forbidden", Said::Refuses),
            ("sk-fake\u{1}carried-by-no-request", Said::Refuses),
            ("sk-fake-listless", Said::Nothing),
            ("sk-fake-busy", Said::NotNow),
            ("sk-fake-broken", Said::NotNow),
            ("sk-fake-given-no-answer", Said::NotNow),
        ] {
            assert_eq!(
                asked_about(&profile, key, &any()).await,
                said,
                "{}",
                key.escape_debug()
            );
        }
        fake.drain_requests();
        assert_eq!(
            asked_about(&profile, "sk-fake-taken", &public()).await,
            Said::Nothing
        );
        assert!(fake.drain_requests().is_empty());
    }

    /// A fresh list replaces the old one, except a model a choice names:
    /// that one stays though the provider no longer lists it.
    #[tokio::test]
    async fn a_chosen_model_stays_when_the_list_no_longer_has_it() {
        let root = temp_root();
        let fake = provider("/v1/models", "authorization", format!("Bearer {KEY}"));
        let id = kept(
            root.path(),
            Protocol::OpenAiCompatible,
            &format!("{}/v1", fake.url()),
            Destinations::Any,
        );
        keep_key(root.path(), &id, KEY).await;
        for model in ["retired", "stray"] {
            add_model(
                root.path(),
                &AddModel {
                    provider: id.clone(),
                    model: model.into(),
                },
            )
            .expect("a model is named by hand");
        }
        set_assignments(
            root.path(),
            SetAssignments {
                kinds: vec!["subtask".into()],
                provider: Some(id.clone()),
                model: Some("retired".into()),
            },
        )
        .expect("the model is chosen");

        let listed = refresh_models(at(root.path()), &id, &any()).await.unwrap();
        assert_eq!(listed.problem, None);
        assert_eq!(ids(&listed.provider), ["large", "retired", "small"]);
    }

    /// Without its operator's leave a host keeps and calls only https
    /// addresses on the internet: not its own network by address, not by a
    /// name that resolves into it, and not a provider kept while it still
    /// reached there.
    #[tokio::test]
    async fn a_host_calls_only_https_addresses_on_the_internet_unless_let() {
        let root = temp_root();
        for endpoint in [
            "http://models.example/v1",
            "https://127.0.0.1:8443/v1",
            "https://10.0.0.7/v1",
            "https://[::1]/v1",
            "https://localhost/v1",
            "https://models.lan/v1",
            "https://someone:secret@models.example/v1",
            "ftp://models.example/v1",
            "models.example/v1",
        ] {
            let refused = add_provider(
                root.path(),
                &person_models::AddProvider {
                    name: "Inside".into(),
                    protocol: Protocol::OpenAiCompatible,
                    endpoint: endpoint.into(),
                    headers: Vec::new(),
                },
                Destinations::Public,
            );
            assert!(refused.is_err(), "{endpoint} is kept");
        }
        assert!(providers(root.path()).providers.is_empty());
        // The operator's leave reaches the host's own network, there alone
        // without TLS.
        assert!(kept_location("http://127.0.0.1:8000/v1", Destinations::Any).is_ok());
        assert!(kept_location("http://models.example/v1", Destinations::Any).is_err());

        let fake = provider("/v1/models", "authorization", format!("Bearer {KEY}"));
        let inside = kept(
            root.path(),
            Protocol::OpenAiCompatible,
            &format!("{}/v1", fake.url()),
            Destinations::Any,
        );
        keep_key(root.path(), &inside, KEY).await;
        let uncalled = refresh_models(at(root.path()), &inside, &public())
            .await
            .unwrap();
        assert_eq!(uncalled.problem, Some(ListingProblem::NotAllowed));

        let named = kept(
            root.path(),
            Protocol::OpenAiCompatible,
            &format!("https://models.example:{}/v1", fake.port()),
            Destinations::Public,
        );
        keep_key(root.path(), &named, KEY).await;
        let resolves_inside: Lookup =
            Arc::new(|_| Box::pin(async { Ok(vec![IpAddr::from([127, 0, 0, 1])]) }));
        let undialled = refresh_models(
            at(root.path()),
            &named,
            &Reach {
                destinations: Destinations::Public,
                lookup: resolves_inside,
            },
        )
        .await
        .unwrap();
        assert_eq!(undialled.problem, Some(ListingProblem::Unreachable));
        assert!(fake.drain_requests().is_empty());
    }

    /// A model named by hand is kept once, under the id the provider knows
    /// it by.
    #[test]
    fn a_model_is_named_by_hand_where_a_provider_lists_none() {
        let root = temp_root();
        let id = kept(
            root.path(),
            Protocol::OpenAiCompatible,
            "https://models.example/v1",
            Destinations::Public,
        );
        let naming = |model: &str| {
            add_model(
                root.path(),
                &AddModel {
                    provider: id.clone(),
                    model: model.into(),
                },
            )
        };
        naming(" typed-model ").expect("a model is named");
        let again = naming("typed-model").expect("naming it again changes nothing");
        assert_eq!(ids(&again), ["typed-model"]);
        for unusable in ["", "   ", "two\nlines"] {
            assert!(naming(unusable).is_err(), "{unusable:?}");
        }
        assert!(
            add_model(
                root.path(),
                &AddModel {
                    provider: "nobody's".into(),
                    model: "typed-model".into(),
                },
            )
            .is_err()
        );
    }

    /// A provider at `endpoint` that offers `models`, each named by hand.
    fn offering(root: &Path, endpoint: &str, models: &[&str]) -> String {
        let id = kept(
            root,
            Protocol::OpenAiCompatible,
            endpoint,
            Destinations::Public,
        );
        for model in models {
            add_model(
                root,
                &AddModel {
                    provider: id.clone(),
                    model: (*model).into(),
                },
            )
            .expect("a model is named");
        }
        id
    }

    fn choose(
        root: &Path,
        kinds: &[&str],
        provider: &str,
        model: &str,
    ) -> Result<ModelProviders, String> {
        set_assignments(
            root,
            SetAssignments {
                kinds: kinds.iter().map(|kind| (*kind).to_owned()).collect(),
                provider: Some(provider.into()),
                model: Some(model.into()),
            },
        )
    }

    /// The provider and model `kind` runs on in `read`.
    fn runs_on(read: &ModelProviders, kind: &str) -> Option<(String, String)> {
        read.assignments
            .iter()
            .find(|chosen| chosen.kind == kind)
            .map(|chosen| (chosen.provider.clone(), chosen.model.clone()))
    }

    /// Two providers with a default of the first, the main thread on the
    /// first and sub-agents and quick judgments on the second: their ids.
    fn chosen(root: &Path) -> (String, String) {
        let first = offering(root, "https://first.example/v1", &["everyday", "main"]);
        let second = offering(root, "https://second.example/v1", &["quick", "sub"]);
        set_default(
            root,
            &SetDefault {
                provider: first.clone(),
                model: "everyday".into(),
            },
        )
        .expect("a default is chosen");
        choose(root, &["request", "task"], &first, "main").expect("the main thread's model");
        choose(root, &["subtask"], &second, "sub").expect("the sub-agents' model");
        set_assignment(
            root,
            SetAssignment {
                kind: "classifier".into(),
                provider: Some(second.clone()),
                model: Some("quick".into()),
            },
        )
        .expect("the model for quick judgments");
        (first, second)
    }

    /// A default and a model for each kind of work are kept as the model
    /// configuration every reader of the store reads, and read back.
    #[test]
    fn a_default_and_a_model_for_each_kind_of_work_are_kept_and_read_back() {
        let root = temp_root();
        let (first, second) = chosen(root.path());

        let read = providers(root.path());
        assert_eq!(read.default_provider.as_deref(), Some(first.as_str()));
        let default_model = read
            .providers
            .iter()
            .find(|provider| provider.id == first)
            .and_then(|provider| provider.default_model.as_deref());
        assert_eq!(default_model, Some("everyday"));
        for kind in ["request", "task"] {
            assert_eq!(runs_on(&read, kind), Some((first.clone(), "main".into())));
        }
        assert_eq!(
            runs_on(&read, "subtask"),
            Some((second.clone(), "sub".into()))
        );
        assert_eq!(
            runs_on(&read, "classifier"),
            Some((second.clone(), "quick".into()))
        );

        // What a reader of the store resolves is the same.
        let config = provider_config(root.path());
        let resolved = |kind| {
            config
                .interaction_selection(kind)
                .map(|(profile, model)| (profile.id.clone(), model.id.clone()))
        };
        assert_eq!(
            resolved(Interaction::Task),
            Some((first.clone(), "main".into()))
        );
        assert_eq!(resolved(Interaction::Subtask), Some((second, "sub".into())));
        assert_eq!(resolved(Interaction::OneShot), None);
        let active = config.active_profile().expect("the default provider");
        assert_eq!(
            (active.id.as_str(), active.default_model.as_deref()),
            (first.as_str(), Some("everyday"))
        );
    }

    /// A choice names a model its provider offers and kinds of work that
    /// exist, and half a choice changes nothing. One cleared follows the
    /// default again, and a provider removed takes the choices that named
    /// it.
    #[tokio::test]
    async fn half_a_choice_changes_nothing_and_a_cleared_one_follows_the_default() {
        let root = temp_root();
        let (first, second) = chosen(root.path());
        let read = providers(root.path());

        assert!(
            set_default(
                root.path(),
                &SetDefault {
                    provider: first.clone(),
                    model: "sub".into(),
                },
            )
            .is_err()
        );
        assert!(choose(root.path(), &["request", "errand"], &second, "sub").is_err());
        assert!(choose(root.path(), &[], &second, "sub").is_err());
        assert!(choose(root.path(), &["task"], &second, "main").is_err());
        assert_eq!(providers(root.path()), read);

        let following = set_assignments(
            root.path(),
            SetAssignments {
                kinds: vec!["request".into(), "task".into()],
                provider: None,
                model: None,
            },
        )
        .expect("the main thread follows the default");
        assert_eq!(runs_on(&following, "request"), None);
        assert_eq!(runs_on(&following, "task"), None);
        assert!(runs_on(&following, "subtask").is_some());

        let remaining = remove_provider(at(root.path()), &second)
            .await
            .expect("the provider is removed");
        assert!(remaining.assignments.is_empty(), "{remaining:?}");
        assert_eq!(remaining.default_provider.as_deref(), Some(first.as_str()));
    }
}
