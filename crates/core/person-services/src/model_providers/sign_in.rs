//! Signing a person in to a model provider on the server, whichever way the
//! provider signs people in: one registry of sign-ins begun, and a flow per
//! kind of issuer.
//!
//! `OpenAI` signs in with its own device code ([`super::openai`]). The others
//! sign in through the committed client registrations
//! ([`account_credentials::ClientTable`]): GitHub for a Copilot subscription
//! and Hugging Face by device code, Google for Vertex AI and the Gemini API
//! on its own page, and `OpenRouter` by its PKCE flow, whose answer is a key.
//!
//! A grant is kept in the workspace's vault in the place a provider's key
//! takes, and named there by the provider's settings (`sign_in.kept`). Each
//! call renews it before it expires: GitHub's grant is traded for a Copilot
//! token, Google's for an access token. No answer, log line or `Debug` output
//! carries a secret.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use account_credentials::openrouter::{AUTH_ENDPOINT, KEYS_ENDPOINT, KeyAuthorization};
use account_credentials::{
    Capability, CredentialError, CredentialStore, DeviceCancel, LoopbackCancel, OAuthIssuer,
    OAuthUse, RenewedGrant, TokenSet, oauth_secret,
};
use agent_provider_config::presets::Preset;
use agent_provider_config::{
    EndpointLocation, ModelDescriptor, ProviderProfile, SignInCredential, SignInIssuer,
    VERTEX_DEFAULT_LOCATION, VERTEX_DEFAULT_MODEL, is_cloud_project_id,
};
use tilo_lock::MutexRecover as _;
use tilo_workspace_records::models::{AccountUse, ProviderView, SignInStarted, SignInState};
use zeroize::Zeroizing;

use super::{Reach, listing, merged};
use crate::model_answers::{ModelRefusal, Models};
use crate::person_settings::{
    ProviderKeys, provider_config, save_named_key, save_provider_config, view_of,
};

/// How long one wait for a sign-in is answered after at the latest.
pub(super) const WAIT_ANSWER: Duration = Duration::from_secs(20);

/// The service a person signs in to, as `begin-sign-in` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Service {
    ChatGpt,
    Copilot,
    HuggingFace,
    Vertex,
    Gemini,
    OpenRouter,
}

impl Service {
    /// The service `slug` names, or why it names none.
    pub(super) fn from_slug(slug: &str) -> Result<Self, String> {
        Ok(match slug.trim() {
            "" | "chatgpt" => Self::ChatGpt,
            "github-copilot" => Self::Copilot,
            "hugging-face" => Self::HuggingFace,
            "google-vertex" => Self::Vertex,
            "google-gemini" => Self::Gemini,
            "openrouter" => Self::OpenRouter,
            other => {
                return Err(format!(
                    "`{other}` is not a service this server signs people in to"
                ));
            }
        })
    }

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::ChatGpt => "ChatGPT",
            Self::Copilot => "GitHub Copilot",
            Self::HuggingFace => "Hugging Face",
            Self::Vertex => "Google Vertex AI",
            Self::Gemini => "Google Gemini",
            Self::OpenRouter => "OpenRouter",
        }
    }

    /// The use its grant is renewed through: the models a provider answers
    /// with, or the downloads a Hugging Face sign-in opens.
    const fn use_(self) -> Option<OAuthUse> {
        match self {
            Self::HuggingFace => Some(OAuthUse::new(
                OAuthIssuer::HuggingFace,
                Capability::ModelDownloads,
            )),
            Self::Copilot => Some(models_use(SignInIssuer::GitHubCopilot)),
            Self::Vertex | Self::Gemini => Some(models_use(SignInIssuer::Google)),
            Self::ChatGpt => Some(models_use(SignInIssuer::ChatGpt)),
            Self::OpenRouter => None,
        }
    }
}

/// What `begin-sign-in` asks: the service, and what its sign-in needs.
pub(super) struct Asked {
    pub(super) service: Service,
    pub(super) using: AccountUse,
    /// The Google Cloud project billed for a Vertex AI or Gemini sign-in.
    pub(super) project: String,
    /// The Gemini model to answer with, or the Vertex AI model to offer.
    pub(super) model: String,
}

/// How a begun sign-in is let go.
pub(super) enum Cancel {
    /// `OpenAI`'s own wait, on a task.
    Task(tokio::task::AbortHandle),
    /// A device code's wait.
    Device(DeviceCancel),
    /// A page that redirects to a loopback the host waits on.
    Loopback(LoopbackCancel),
}

impl Cancel {
    fn cancel(&self) {
        match self {
            Self::Task(task) => task.abort(),
            Self::Device(cancel) => cancel.cancel(),
            Self::Loopback(cancel) => cancel.cancel(),
        }
    }
}

/// One sign-in a person began and has not finished.
pub(super) struct Pending {
    pub(super) person: [u8; 32],
    pub(super) state: tokio::sync::watch::Receiver<SignInState>,
    pub(super) cancel: Cancel,
}

/// The sign-ins people are in the middle of, and the one renewal of a grant
/// at a time.
#[derive(Default)]
pub(crate) struct SignIns {
    pub(super) pending: Mutex<std::collections::HashMap<String, Pending>>,
    /// A provider replaces a refresh token at each use and refuses the old
    /// one: two calls that both found a grant stale renew it one after the
    /// other, and the second finds it fresh.
    pub(super) refreshing: tokio::sync::Mutex<()>,
}

impl Drop for SignIns {
    fn drop(&mut self) {
        for pending in self.pending.lock_recover().values() {
            pending.cancel.cancel();
        }
    }
}

/// The use an issuer's grant of a model provider is renewed through.
const fn models_use(issuer: SignInIssuer) -> OAuthUse {
    OAuthUse::new(
        match issuer {
            SignInIssuer::Google => OAuthIssuer::Google,
            SignInIssuer::ChatGpt => OAuthIssuer::OpenAi,
            SignInIssuer::GitHubCopilot => OAuthIssuer::GitHub,
            SignInIssuer::HuggingFace => OAuthIssuer::HuggingFace,
        },
        Capability::Models,
    )
}

impl crate::PersonServices {
    /// Sign this server's people in to the model providers at `store`'s
    /// addresses in place of the committed ones: an operator's own
    /// registration, or a test's fake issuer.
    pub fn sign_in_through(&self, store: CredentialStore) {
        *self
            .inner
            .models
            .oauth
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(store);
    }

    /// Sign in to `OpenRouter` at `auth` and `keys` in place of its own
    /// pages: a test's fake, which never sees a real account.
    pub fn sign_in_to_openrouter_at(&self, auth: String, keys: String) {
        *self
            .inner
            .models
            .openrouter
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((auth, keys));
    }

    /// Begin `person`'s sign-in for `workspace` ([`BEGIN_SIGN_IN`]) to
    /// `asked`'s service: where it waits for the person, and what they enter
    /// there. A person signs in once at a time: one begun before is let go.
    ///
    /// [`BEGIN_SIGN_IN`]: tilo_workspace_records::models::BEGIN_SIGN_IN
    pub(super) async fn begin_model_sign_in(
        &self,
        (person, workspace): ([u8; 32], &str),
        asked: Asked,
        reach: Reach,
        call: Option<tilo_actions::remote::InvokeRequest>,
    ) -> Result<SignInStarted, String> {
        match asked.service {
            Service::ChatGpt => {
                let started = self
                    .start_chatgpt_sign_in((person, workspace), asked.using, reach, call)
                    .await?;
                self.register_sign_in(
                    person,
                    started.begun.sign_in.clone(),
                    started.state,
                    Cancel::Task(started.task),
                );
                Ok(started.begun)
            }
            Service::OpenRouter => {
                let (auth, keys) = self.inner.models.openrouter_endpoints();
                let begun = KeyAuthorization::begin(&auth, &keys).map_err(|error| {
                    format!("OpenRouter's sign-in could not begin here: {error}")
                })?;
                let id = uuid::Uuid::new_v4().to_string();
                let page = begun.url().to_owned();
                let cancel = Cancel::Loopback(begun.canceller());
                let started = SignInStarted {
                    sign_in: id.clone(),
                    code: String::new(),
                    page,
                };
                let workspaces = self.clone();
                let workspace = workspace.to_owned();
                let (tell, state) = tokio::sync::watch::channel(SignInState::Waiting);
                tokio::spawn(async move {
                    let finished = tokio::task::spawn_blocking(move || begun.finish()).await;
                    let outcome = match finished {
                        Ok(Ok(key)) => {
                            workspaces
                                .keep_openrouter_key(&workspace, person, key, call.as_ref())
                                .await
                        }
                        Ok(Err(error)) => Err(error.to_string()),
                        Err(_) => Err("OpenRouter's answer was not read".to_owned()),
                    };
                    tell_end(&workspaces, &tell, outcome);
                });
                self.register_sign_in(person, id, state, cancel);
                Ok(started)
            }
            Service::HuggingFace => {
                let root = self.workspace_root(workspace);
                if local_provider(&root).is_none() {
                    return Err(
                        "Start a model on this computer first: its provider keeps the Hugging \
                         Face sign-in its downloads use"
                            .to_owned(),
                    );
                }
                self.begin_device_sign_in(
                    (person, workspace),
                    Service::HuggingFace,
                    asked.project,
                    asked.model,
                    call,
                )
                .await
            }
            Service::Copilot => {
                self.begin_device_sign_in(
                    (person, workspace),
                    Service::Copilot,
                    asked.project,
                    asked.model,
                    call,
                )
                .await
            }
            Service::Vertex | Service::Gemini => {
                if asked.project.trim().is_empty() {
                    return Err(
                        "Name the Google Cloud project a Vertex AI or Gemini sign-in is billed to"
                            .to_owned(),
                    );
                }
                self.begin_page_sign_in(
                    (person, workspace),
                    asked.service,
                    asked.project,
                    asked.model,
                    call,
                )
                .await
            }
        }
    }

    /// How `person`'s sign-in goes ([`WAIT_SIGN_IN`]): answered as soon as it
    /// changed, or after a while as still waiting. A sign-in that ended is
    /// answered once and then forgotten.
    ///
    /// [`WAIT_SIGN_IN`]: tilo_workspace_records::models::WAIT_SIGN_IN
    pub(super) async fn wait_model_sign_in(
        &self,
        person: [u8; 32],
        sign_in: &str,
    ) -> Result<SignInState, String> {
        let mut state = self
            .inner
            .models
            .sign_ins
            .pending
            .lock_recover()
            .get(sign_in)
            .filter(|pending| pending.person == person)
            .map(|pending| pending.state.clone())
            .ok_or_else(|| "That sign-in has ended. Begin it again.".to_owned())?;
        if *state.borrow_and_update() == SignInState::Waiting {
            let _ = tokio::time::timeout(WAIT_ANSWER, state.changed()).await;
        }
        let now = state.borrow().clone();
        if now != SignInState::Waiting {
            self.inner
                .models
                .sign_ins
                .pending
                .lock_recover()
                .remove(sign_in);
        }
        Ok(now)
    }

    /// Let go of `person`'s sign-in ([`CANCEL_SIGN_IN`]).
    ///
    /// [`CANCEL_SIGN_IN`]: tilo_workspace_records::models::CANCEL_SIGN_IN
    pub(super) fn cancel_model_sign_in(&self, person: [u8; 32], sign_in: &str) {
        let mut pending = self.inner.models.sign_ins.pending.lock_recover();
        if pending
            .get(sign_in)
            .is_some_and(|pending| pending.person == person)
            && let Some(pending) = pending.remove(sign_in)
        {
            pending.cancel.cancel();
        }
    }

    /// Keep `sign_in` as the one `person` waits on, letting their earlier one
    /// go.
    fn register_sign_in(
        &self,
        person: [u8; 32],
        sign_in: String,
        state: tokio::sync::watch::Receiver<SignInState>,
        cancel: Cancel,
    ) {
        let mut pending = self.inner.models.sign_ins.pending.lock_recover();
        pending.retain(|_, other| {
            let theirs = other.person == person;
            if theirs {
                other.cancel.cancel();
            }
            !theirs
        });
        pending.insert(
            sign_in,
            Pending {
                person,
                state,
                cancel,
            },
        );
    }

    /// Begin a device-code sign-in: the person enters `code` on the issuer's
    /// page, and the server waits there.
    async fn begin_device_sign_in(
        &self,
        (person, workspace): ([u8; 32], &str),
        service: Service,
        project: String,
        model: String,
        call: Option<tilo_actions::remote::InvokeRequest>,
    ) -> Result<SignInStarted, String> {
        let root = self.workspace_root(workspace);
        let store = self.inner.models.sign_in_store(&root);
        let use_ = service
            .use_()
            .ok_or_else(|| format!("{} does not sign in with a code", service.label()))?;
        let authorization = tokio::task::spawn_blocking(move || store.begin_device_sign_in(use_))
            .await
            .map_err(|_| "the sign-in was not begun".to_owned())?
            .map_err(|error| not_set_up(service, error))?;
        let id = uuid::Uuid::new_v4().to_string();
        let started = SignInStarted {
            sign_in: id.clone(),
            code: authorization.user_code().to_owned(),
            page: authorization.verification_uri().to_owned(),
        };
        let cancel = Cancel::Device(authorization.canceller());
        let workspaces = self.clone();
        let workspace = workspace.to_owned();
        let (tell, state) = tokio::sync::watch::channel(SignInState::Waiting);
        tokio::spawn(async move {
            let finished = tokio::task::spawn_blocking(move || {
                CredentialStore::finish_device_sign_in(authorization)
            })
            .await;
            let outcome = match finished {
                Ok(Ok(tokens)) => {
                    workspaces
                        .keep_grant(
                            (&workspace, person),
                            service,
                            &project,
                            &model,
                            tokens,
                            call.as_ref(),
                        )
                        .await
                }
                Ok(Err(error)) => Err(error.to_string()),
                Err(_) => Err("the sign-in was not read to the end".to_owned()),
            };
            tell_end(&workspaces, &tell, outcome);
        });
        self.register_sign_in(person, id, state, cancel);
        Ok(started)
    }

    /// Begin signing in on Google's own page: the loopback waits for the
    /// browser, and the grant it leaves is kept as the provider's sign-in.
    async fn begin_page_sign_in(
        &self,
        (person, workspace): ([u8; 32], &str),
        service: Service,
        project: String,
        model: String,
        call: Option<tilo_actions::remote::InvokeRequest>,
    ) -> Result<SignInStarted, String> {
        let root = self.workspace_root(workspace);
        let store = self.inner.models.sign_in_store(&root);
        let use_ = service
            .use_()
            .ok_or_else(|| format!("{} does not sign in on a page", service.label()))?;
        let authorization =
            tokio::task::spawn_blocking(move || store.begin_sign_in_for(use_, None))
                .await
                .map_err(|_| "the sign-in was not begun".to_owned())?
                .map_err(|error| not_set_up(service, error))?;
        let Some(cancel) = authorization.canceller() else {
            return Err(format!(
                "{}'s sign-in could not wait for your browser here. Try again",
                service.label()
            ));
        };
        let id = uuid::Uuid::new_v4().to_string();
        let started = SignInStarted {
            sign_in: id.clone(),
            code: String::new(),
            page: authorization.url().to_owned(),
        };
        let workspaces = self.clone();
        let workspace = workspace.to_owned();
        let (tell, state) = tokio::sync::watch::channel(SignInState::Waiting);
        tokio::spawn(async move {
            let finished =
                tokio::task::spawn_blocking(move || CredentialStore::finish_sign_in(authorization))
                    .await;
            let outcome = match finished {
                Ok(Ok(tokens)) => {
                    workspaces
                        .keep_grant(
                            (&workspace, person),
                            service,
                            &project,
                            &model,
                            tokens,
                            call.as_ref(),
                        )
                        .await
                }
                Ok(Err(error)) => Err(error.to_string()),
                Err(_) => Err("the sign-in was not read to the end".to_owned()),
            };
            tell_end(&workspaces, &tell, outcome);
        });
        self.register_sign_in(person, id, state, Cancel::Loopback(cancel));
        Ok(started)
    }

    /// Keep the grant a sign-in left as the provider's sign-in in the
    /// workspace's vault, list its models, and answer the provider.
    async fn keep_grant(
        &self,
        (workspace, person): (&str, [u8; 32]),
        service: Service,
        project: &str,
        model: &str,
        tokens: TokenSet,
        call: Option<&tilo_actions::remote::InvokeRequest>,
    ) -> Result<ProviderView, String> {
        let root = self.workspace_root(workspace);
        let asker = tilo_member_types::Acting::person(hex::encode(person));
        let vault = self.vault_for(workspace, &asker);
        let vault = call.map_or(vault, |call| vault.serving(call));
        let keys = ProviderKeys::of(vault, "");
        let reading = ProviderKeys::of(self.vault_at(workspace), "");
        if service == Service::HuggingFace {
            return keep_hugging_face_key(&root, (keys, &tokens.access_token)).await;
        }
        let mut profile = profile_for(&root, service, project, model)?;
        ensure_kept(&root, &profile)?;
        keys.put(
            &mut profile,
            &oauth_secret(&tokens).token.unwrap_or_default(),
        )
        .await?;
        save_named_key(&root, &profile)?;
        let use_ = service
            .use_()
            .ok_or_else(|| format!("{} keeps no grant here", service.label()))?;
        let token = self
            .inner
            .models
            .oauth_access((reading, &root), &profile, use_, false)
            .await
            .map_err(|refusal| refusal.words(None))?;
        let reach = Reach::of(&self.inner.models);
        let listed = listing(&profile, Some(&token), &reach).await;
        keep_listed(&root, &profile.id, listed)
    }

    /// Keep the key an `OpenRouter` sign-in was issued as the provider's key,
    /// list its models, and answer the provider.
    async fn keep_openrouter_key(
        &self,
        workspace: &str,
        person: [u8; 32],
        key: Zeroizing<String>,
        call: Option<&tilo_actions::remote::InvokeRequest>,
    ) -> Result<ProviderView, String> {
        let root = self.workspace_root(workspace);
        let asker = tilo_member_types::Acting::person(hex::encode(person));
        let vault = self.vault_for(workspace, &asker);
        let vault = call.map_or(vault, |call| vault.serving(call));
        let keys = ProviderKeys::of(vault, "");
        let mut profile = profile_for(&root, Service::OpenRouter, "", "")?;
        ensure_kept(&root, &profile)?;
        keys.put(&mut profile, &key).await?;
        save_named_key(&root, &profile)?;
        let reach = Reach::of(&self.inner.models);
        let listed = listing(&profile, Some(&key), &reach).await;
        keep_listed(&root, &profile.id, listed)
    }
}

/// The provider `service` answers as: the one kept already, or a new one from
/// the catalogue.
fn profile_for(
    root: &Path,
    service: Service,
    project: &str,
    model: &str,
) -> Result<ProviderProfile, String> {
    let config = provider_config(root);
    let kept = |wanted: &dyn Fn(&ProviderProfile) -> bool| {
        config
            .profiles
            .iter()
            .find(|profile| wanted(profile))
            .cloned()
    };
    Ok(match service {
        Service::Copilot => {
            let mut profile =
                kept(&|profile| profile.sign_in_issuer() == Some(SignInIssuer::GitHubCopilot))
                    .unwrap_or_else(|| Preset::COPILOT.profile());
            profile.sign_in = Some(SignInCredential::new(SignInIssuer::GitHubCopilot));
            profile.credential = None;
            profile.enabled = true;
            profile
        }
        Service::Vertex | Service::Gemini => {
            let project = project.trim();
            if !is_cloud_project_id(project) {
                return Err(
                    "A Google Cloud project id is lowercase letters, digits and hyphens".to_owned(),
                );
            }
            let mut profile =
                kept(&|profile| profile.sign_in_issuer() == Some(SignInIssuer::Google))
                    .unwrap_or_else(|| {
                        if service == Service::Vertex {
                            ProviderProfile::google_vertex(
                                project,
                                VERTEX_DEFAULT_LOCATION,
                                if model.trim().is_empty() {
                                    VERTEX_DEFAULT_MODEL
                                } else {
                                    model.trim()
                                },
                            )
                        } else {
                            ProviderProfile::google_gemini(project)
                        }
                    });
            if service == Service::Vertex {
                let fresh = ProviderProfile::google_vertex(
                    project,
                    VERTEX_DEFAULT_LOCATION,
                    if model.trim().is_empty() {
                        VERTEX_DEFAULT_MODEL
                    } else {
                        model.trim()
                    },
                );
                profile.endpoint = fresh.endpoint;
                profile.models.clone_from(&fresh.models);
                profile.default_model.clone_from(&fresh.default_model);
            } else {
                let model = model.trim();
                if model.is_empty() {
                    return Err("Name the Gemini model to answer with".to_owned());
                }
                let fresh = ProviderProfile::google_gemini(project);
                profile.endpoint = fresh.endpoint;
                profile.models = vec![ModelDescriptor::new(model)];
                profile.default_model = Some(model.to_owned());
            }
            profile.sign_in = Some(SignInCredential::google(project));
            profile.credential = None;
            profile.enabled = true;
            profile
        }
        Service::OpenRouter => {
            let mut profile = kept(&|profile| {
                profile.sign_in.is_none()
                    && profile.endpoint.trim_end_matches('/')
                        == Preset::OPENROUTER.endpoint().trim_end_matches('/')
            })
            .unwrap_or_else(|| Preset::OPENROUTER.profile());
            profile.credential = None;
            profile.sign_in = None;
            profile.enabled = true;
            profile
        }
        Service::ChatGpt | Service::HuggingFace => {
            return Err(format!("{} does not keep a grant here", service.label()));
        }
    })
}

/// Keep `listed` models on the provider `id`, choose one where none is
/// chosen, and make its provider the default where nothing answers yet.
fn keep_listed(
    root: &Path,
    id: &str,
    listed: Result<Vec<ModelDescriptor>, super::ListingProblem>,
) -> Result<ProviderView, String> {
    let mut config = provider_config(root);
    let nothing_answers = !config.default_profile.as_ref().is_some_and(|chosen| {
        config
            .profiles
            .iter()
            .find(|profile| profile.id == *chosen)
            .is_some_and(|profile| profile.default_model.is_some())
    });
    let view = {
        let kept = config
            .profiles
            .iter_mut()
            .find(|profile| profile.id == id)
            .ok_or_else(|| "That provider is not kept here".to_owned())?;
        if let Ok(listed) = listed
            && !listed.is_empty()
        {
            kept.models = merged(
                std::mem::take(&mut kept.models),
                listed,
                &std::collections::BTreeSet::default(),
            );
        }
        if kept.default_model.is_none() {
            kept.default_model = kept.models.first().map(|model| model.id.clone());
        }
        view_of(kept)
    };
    if nothing_answers && view.default_model.is_some() {
        config.default_profile = Some(id.to_owned());
    }
    save_provider_config(root, &config)?;
    Ok(view)
}

impl Models {
    /// The stores a model sign-in is driven through: the operator's, or the
    /// workspace's own registration table.
    pub(crate) fn sign_in_store(&self, root: &Path) -> CredentialStore {
        self.oauth
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| CredentialStore::for_root(Some(root.to_owned())))
    }

    /// The pages `OpenRouter` signs in at: its own, or a test's fake.
    pub(crate) fn openrouter_endpoints(&self) -> (String, String) {
        self.openrouter
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| (AUTH_ENDPOINT.to_owned(), KEYS_ENDPOINT.to_owned()))
    }

    /// The access token of the sign-in `profile` names, renewed first where
    /// it is due, or with `force`: `OpenAI`'s own grant, or one the committed
    /// client registrations renew (GitHub's for Copilot, Google's).
    pub(crate) async fn fresh_access(
        &self,
        (keys, root): (ProviderKeys<'_>, &Path),
        profile: &ProviderProfile,
        force: bool,
    ) -> Result<Zeroizing<String>, ModelRefusal> {
        match profile.sign_in_issuer() {
            Some(SignInIssuer::ChatGpt) => self.chatgpt_access((keys, root), profile, force).await,
            Some(issuer) => {
                self.oauth_access((keys, root), profile, models_use(issuer), force)
                    .await
            }
            None => Err(ModelRefusal::SignedOut {
                provider: profile.display_label().to_owned(),
            }),
        }
    }

    /// The access token of a grant the committed registrations renew, kept in
    /// the workspace's vault (`keys`), renewed first where it is due or with
    /// `force`. A grant the provider no longer renews signs nobody in.
    pub(crate) async fn oauth_access(
        &self,
        (keys, root): (ProviderKeys<'_>, &Path),
        profile: &ProviderProfile,
        use_: OAuthUse,
        force: bool,
    ) -> Result<Zeroizing<String>, ModelRefusal> {
        let signed_out = || ModelRefusal::SignedOut {
            provider: profile.display_label().to_owned(),
        };
        let _one = self.sign_ins.refreshing.lock().await;
        // The settings are read again: what `profile` named may have been replaced since.
        let profile = &provider_config(root)
            .profiles
            .into_iter()
            .find(|kept| kept.id == profile.id)
            .ok_or_else(signed_out)?;
        let kept = keys
            .get(profile)
            .await
            .map_err(|_| ModelRefusal::SettingsUnreadable)?
            .ok_or_else(signed_out)?;
        let reference = crate::person_settings::key_reference(profile).ok_or_else(signed_out)?;
        let secret = accounts::Secret {
            password: String::new(),
            token: Some(kept.to_string()),
        };
        let store = self.sign_in_store(root);
        let renewed = tokio::task::spawn_blocking(move || store.renew_grant(&secret, use_, force))
            .await
            .map_err(|_| ModelRefusal::SettingsUnreadable)?
            .map_err(|error| match error {
                CredentialError::NeedsAuthentication => signed_out(),
                CredentialError::Offline => {
                    ModelRefusal::Provider(agent_provider_config::protocol::ModelError::new(
                        agent_provider_config::protocol::ErrorKind::Unavailable,
                    ))
                }
                other => {
                    tracing::info!(%other, "a model sign-in was not renewed");
                    ModelRefusal::SettingsUnreadable
                }
            })?;
        match renewed {
            RenewedGrant {
                tokens: Some(tokens),
                keep,
            } => {
                if let Some(renewed) = keep.and_then(|secret| secret.token) {
                    keys.vault()
                        .renew_sign_in(&reference, Zeroizing::new(renewed))
                        .await
                        .map_err(|error| {
                            tracing::warn!(%error, "a renewed model sign-in was not kept");
                            ModelRefusal::SettingsUnreadable
                        })?;
                }
                Ok(Zeroizing::new(tokens.access_token))
            }
            RenewedGrant { tokens: None, .. } => Err(signed_out()),
        }
    }
}

/// The provider this computer runs its own models at, if it keeps one: the
/// key a Hugging Face sign-in becomes.
fn local_provider(root: &Path) -> Option<ProviderProfile> {
    provider_config(root)
        .profiles
        .into_iter()
        .find(|profile| profile.location == EndpointLocation::ThisDevice)
}

/// Keep a Hugging Face sign-in as the key of this computer's model provider,
/// where its downloads take it from.
async fn keep_hugging_face_key(
    root: &Path,
    (keys, token): (ProviderKeys<'_>, &str),
) -> Result<ProviderView, String> {
    let mut profile = local_provider(root).ok_or_else(|| {
        "Start a model on this computer first: its provider keeps the Hugging Face sign-in"
            .to_owned()
    })?;
    keys.put(&mut profile, token).await?;
    save_named_key(root, &profile)
}

/// Keep `profile` in `root`'s provider settings, making it the one this id
/// names.
fn ensure_kept(root: &Path, profile: &ProviderProfile) -> Result<(), String> {
    let mut config = provider_config(root);
    match config
        .profiles
        .iter_mut()
        .find(|kept| kept.id == profile.id)
    {
        Some(kept) => kept.clone_from(profile),
        None => config.profiles.push(profile.clone()),
    }
    save_provider_config(root, &config)
}

/// Say why a sign-in did not begin: a registration this build lacks, or the
/// issuer's own words.
fn not_set_up(service: Service, error: CredentialError) -> String {
    match error {
        CredentialError::NotConfigured { .. } => {
            format!(
                "Signing in with {} is not set up in this build",
                service.label()
            )
        }
        other => format!("{}'s sign-in did not begin: {other}", service.label()),
    }
}

/// Tell whoever waits on a sign-in how it ended, and ring the people who read
/// the workspace's models when it did.
fn tell_end(
    workspaces: &crate::PersonServices,
    tell: &tokio::sync::watch::Sender<SignInState>,
    outcome: Result<ProviderView, String>,
) {
    let state = match outcome {
        Ok(provider) => {
            workspaces.person_settings_changed();
            SignInState::Done { provider }
        }
        Err(reason) => SignInState::Failed { reason },
    };
    let _ = tell.send(state);
}
