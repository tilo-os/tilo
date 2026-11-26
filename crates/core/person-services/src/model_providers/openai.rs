//! Signing a person in with their `OpenAI` account on the server.
//!
//! `OpenAI` lets no other application sign people in to a `ChatGPT` plan,
//! so this signs in the way `OpenAI`'s own Codex CLI does where it has no
//! browser beside it (`codex login --device-auth`), with Codex's public
//! client: the server asks `OpenAI` for a one-time code, the person enters it
//! on `OpenAI`'s own page in any browser, and the server's polling is then
//! handed an authorization code and its PKCE verifier, which it exchanges
//! for the account's tokens. Nothing comes back to a loopback address, so a
//! page with no device of the person's behind it signs in as well as a
//! computer does.
//!
//! The grant is kept in the workspace's vault in the place a provider's key
//! takes, bound to the backend it is sent to, and refreshed there before it
//! expires or when the backend turns it away. No answer, log line or
//! `Debug` output carries a token.
//!
//! With [`AccountUse::ApiUsage`] the server has `OpenAI` issue an API key for
//! the account's API organisation instead, as Codex does, and keeps that as
//! an ordinary `OpenAI` provider key.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_provider_config::presets::{CHATGPT_ENDPOINT, Preset};
use agent_provider_config::{ProviderConfig, ProviderProfile, SignInIssuer, transform};
use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use tilo_net::proxy::host::guarded_client;
use tilo_workspace_service::models::{AccountUse, ProviderView, SignInStarted, SignInState};
use zeroize::Zeroizing;

use super::{Reach, listing, merged};
use crate::model_answers::{ModelRefusal, Models};
use crate::person_settings::{
    ProviderKeys, key_kept, provider_config, save_named_key, save_provider_config, view_of,
};

/// Codex's public client, the one `OpenAI` signs a `ChatGPT` plan in to.
const CODEX_CLIENT: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Where `OpenAI` signs its accounts in.
const OPENAI_ISSUER: &str = "https://auth.openai.com";
/// How long a person has to enter their code, as Codex gives them.
const SIGN_IN_LIFETIME: Duration = Duration::from_secs(15 * 60);
/// How long one wait for a sign-in is answered after at the latest.
const WAIT_ANSWER: Duration = Duration::from_secs(20);
/// How long one request to `OpenAI`'s sign-in may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long before it expires an access token is refreshed.
const REFRESH_AHEAD_SECS: u64 = 5 * 60;
/// The most of an answer of `OpenAI`'s sign-in read.
const MAX_ANSWER_BYTES: usize = 64 * 1024;

/// Where this server signs people in to `OpenAI` and reaches their plan: the
/// real addresses, unless its operator or a test names others.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenAiSignIn {
    /// `OpenAI`'s sign-in, without a trailing slash.
    pub issuer: String,
    /// The `ChatGPT` backend a signed-in plan is asked at.
    pub backend: String,
    /// `OpenAI`'s API, which an API key issued at sign-in is used at.
    pub api: String,
    /// The client signed in as.
    pub client_id: String,
}

impl Default for OpenAiSignIn {
    fn default() -> Self {
        Self {
            issuer: OPENAI_ISSUER.to_owned(),
            backend: CHATGPT_ENDPOINT.to_owned(),
            api: Preset::OPENAI.endpoint().to_owned(),
            client_id: CODEX_CLIENT.to_owned(),
        }
    }
}

impl OpenAiSignIn {
    fn issuer(&self) -> &str {
        self.issuer.trim_end_matches('/')
    }

    fn page(&self) -> String {
        format!("{}/codex/device", self.issuer())
    }

    fn token_url(&self) -> String {
        format!("{}/oauth/token", self.issuer())
    }
}

/// A `ChatGPT` plan's grant, as the vault keeps it.
#[derive(Clone, Serialize, Deserialize)]
#[expect(
    clippy::struct_field_names,
    reason = "OpenAI's own names for its tokens, kept as it writes them"
)]
pub(crate) struct Grant {
    access_token: String,
    refresh_token: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    id_token: String,
}

impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Grant([redacted])")
    }
}

impl Drop for Grant {
    fn drop(&mut self) {
        use zeroize::Zeroize as _;
        self.access_token.zeroize();
        self.refresh_token.zeroize();
        self.id_token.zeroize();
    }
}

impl Grant {
    /// The grant a vault entry holds; `None` for anything else.
    pub(crate) fn read(kept: &str) -> Option<Self> {
        tilo_failure::ignored!(
            "a vault entry that is not a grant is a typed key, which this reads as no grant",
            serde_json::from_str(kept)
        )
        .ok()
    }

    fn sealed(&self) -> Zeroizing<String> {
        Zeroizing::new(serde_json::to_string(self).unwrap_or_default())
    }

    /// The access token, for the request's own header.
    pub(crate) fn access(&self) -> Zeroizing<String> {
        Zeroizing::new(self.access_token.clone())
    }

    /// Whether the access token expires within a few minutes of `now`
    /// (seconds since the epoch). One that says nothing of when is used
    /// until the backend turns it away.
    fn stale(&self, now: u64) -> bool {
        transform::token_claims(&self.access_token)
            .and_then(|claims| claims.get("exp")?.as_u64())
            .is_some_and(|expires| expires <= now.saturating_add(REFRESH_AHEAD_SECS))
    }
}

/// What `OpenAI` grants at the end of a sign-in and at each refresh.
#[derive(Deserialize)]
#[expect(
    clippy::struct_field_names,
    reason = "OpenAI's own names for its tokens, as it writes them"
)]
struct Tokens {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

impl Drop for Tokens {
    fn drop(&mut self) {
        use zeroize::Zeroize as _;
        for token in [
            &mut self.access_token,
            &mut self.refresh_token,
            &mut self.id_token,
        ]
        .into_iter()
        .flatten()
        {
            token.zeroize();
        }
    }
}

/// One sign-in a person began and has not finished.
struct Pending {
    person: [u8; 32],
    state: tokio::sync::watch::Receiver<SignInState>,
    task: tokio::task::AbortHandle,
}

/// The sign-ins people are in the middle of, and the one refresh of a grant
/// at a time.
#[derive(Default)]
pub(crate) struct SignIns {
    pending: Mutex<HashMap<String, Pending>>,
    /// `OpenAI` replaces a refresh token at each use and refuses the old
    /// one: two calls that both found a grant stale refresh it one after
    /// the other, and the second finds it fresh.
    refreshing: tokio::sync::Mutex<()>,
}

impl Drop for SignIns {
    fn drop(&mut self) {
        for pending in self.pending.lock_recover().values() {
            pending.task.abort();
        }
    }
}

fn now_secs() -> u64 {
    tilo_clock::unix_now()
}

impl crate::PersonServices {
    /// Sign this server's people in to `OpenAI` at `sign_in`'s addresses in
    /// place of the real ones: an operator's own proxy, or a test's fake.
    pub fn sign_in_to_openai_at(&self, sign_in: OpenAiSignIn) {
        *self
            .inner
            .models
            .openai
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = sign_in;
    }

    fn openai(&self) -> OpenAiSignIn {
        self.inner.models.openai()
    }

    /// Begin `person`'s sign-in for `workspace` ([`BEGIN_SIGN_IN`]): ask
    /// `OpenAI` for a code, then wait for the person on a task of its own,
    /// and keep the grant in the workspace's vault. A person signs in once
    /// at a time: one begun before is let go.
    ///
    /// [`BEGIN_SIGN_IN`]: tilo_workspace_service::models::BEGIN_SIGN_IN
    pub(super) async fn begin_openai_sign_in(
        &self,
        (person, workspace): ([u8; 32], &str),
        using: AccountUse,
        reach: Reach,
        call: Option<tilo_actions::remote::InvokeRequest>,
    ) -> Result<SignInStarted, String> {
        let openai = self.openai();
        let code: UserCode = post_json(
            &reach,
            &format!("{}/api/accounts/deviceauth/usercode", openai.issuer()),
            &serde_json::json!({ "client_id": openai.client_id }),
        )
        .await
        .map_err(|failure| match failure {
            Failure::Status(404) => {
                "OpenAI does not offer signing in with a code right now.".to_owned()
            }
            other => other.words(),
        })?;
        let id = uuid::Uuid::new_v4().to_string();
        let tell = tokio::sync::watch::Sender::new(SignInState::Waiting);
        let state = tell.subscribe();
        let workspaces = self.clone();
        let device = code.clone();
        let workspace = workspace.to_owned();
        let task = tokio::spawn(async move {
            let outcome = workspaces
                .signed_in(
                    (&workspace, person),
                    &openai,
                    &device,
                    using,
                    &reach,
                    call.as_ref(),
                )
                .await;
            let state = match outcome {
                Ok(provider) => {
                    workspaces.person_settings_changed();
                    SignInState::Done { provider }
                }
                Err(reason) => SignInState::Failed { reason },
            };
            let _ = tell.send(state);
        })
        .abort_handle();
        let started = SignInStarted {
            sign_in: id.clone(),
            code: code.user_code,
            page: self.openai().page(),
        };
        {
            let mut pending = self.inner.models.sign_ins.pending.lock_recover();
            pending.retain(|_, other| {
                let theirs = other.person == person;
                if theirs {
                    other.task.abort();
                }
                !theirs
            });
            pending.insert(
                id,
                Pending {
                    person,
                    state,
                    task,
                },
            );
        }
        Ok(started)
    }

    /// How `person`'s sign-in goes ([`WAIT_SIGN_IN`]): answered as soon as
    /// it changed, or after a while as still waiting. A sign-in that ended
    /// is answered once and then forgotten.
    ///
    /// [`WAIT_SIGN_IN`]: tilo_workspace_service::models::WAIT_SIGN_IN
    pub(super) async fn wait_openai_sign_in(
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
    /// [`CANCEL_SIGN_IN`]: tilo_workspace_service::models::CANCEL_SIGN_IN
    pub(super) fn cancel_openai_sign_in(&self, person: [u8; 32], sign_in: &str) {
        let mut pending = self.inner.models.sign_ins.pending.lock_recover();
        if pending
            .get(sign_in)
            .is_some_and(|pending| pending.person == person)
            && let Some(pending) = pending.remove(sign_in)
        {
            pending.task.abort();
        }
    }

    /// Wait for the person to enter `code`, take the account's grant, and
    /// keep the provider it answers on: what one sign-in does from its code on.
    async fn signed_in(
        &self,
        (workspace, person): (&str, [u8; 32]),
        openai: &OpenAiSignIn,
        code: &UserCode,
        using: AccountUse,
        reach: &Reach,
        call: Option<&tilo_actions::remote::InvokeRequest>,
    ) -> Result<ProviderView, String> {
        let root = &self.workspace_root(workspace);
        // The person signs in themselves: the use of what is kept is theirs, and the write keeps
        // the call that began the sign-in as they signed it.
        let asker = tilo_action_policy::Acting::person(hex::encode(person));
        let vault = self.vault_for(workspace, &asker);
        let vault = call.map_or(vault, |call| vault.serving(call));
        let keys = ProviderKeys::of(vault, "");
        let granted = tokio::time::timeout(SIGN_IN_LIFETIME, entered(openai, code, reach))
            .await
            .map_err(|_| "The code expired before it was entered. Begin again.".to_owned())??;
        let tokens: Tokens = post_form(
            reach,
            &openai.token_url(),
            &[
                ("grant_type", "authorization_code"),
                ("code", &granted.authorization_code),
                (
                    "redirect_uri",
                    &format!("{}/deviceauth/callback", openai.issuer()),
                ),
                ("client_id", &openai.client_id),
                ("code_verifier", &granted.code_verifier),
            ],
        )
        .await
        .map_err(|failure| failure.words())?;
        let (Some(access_token), Some(refresh_token)) =
            (tokens.access_token.clone(), tokens.refresh_token.clone())
        else {
            return Err("OpenAI granted no access to the account.".to_owned());
        };
        let id_token = tokens.id_token.clone().unwrap_or_default();
        match using {
            AccountUse::Subscription => {
                let grant = Grant {
                    access_token,
                    refresh_token,
                    id_token,
                };
                let mut profile = kept_provider(root, openai, using)?;
                keys.put(&mut profile, &grant.sealed()).await?;
                save_named_key(root, &profile)?;
                listed_and_chosen(root, profile, &grant.access(), reach).await
            }
            AccountUse::ApiUsage => {
                drop((access_token, refresh_token));
                let key = api_key(openai, &id_token, reach).await?;
                let mut profile = kept_provider(root, openai, using)?;
                keys.put(&mut profile, &key).await?;
                save_named_key(root, &profile)?;
                listed_and_chosen(root, profile, &key, reach).await
            }
        }
    }
}

impl Models {
    fn openai(&self) -> OpenAiSignIn {
        self.openai
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The access token of the grant the workspace's vault (`keys`) keeps for
    /// `profile`, refreshed first when it expires soon, or with `force`
    /// when the backend just turned it away.
    pub(crate) async fn fresh_access(
        &self,
        (keys, root): (ProviderKeys<'_>, &Path),
        profile: &ProviderProfile,
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
        let grant = Grant::read(&kept).ok_or_else(signed_out)?;
        if !force && !grant.stale(now_secs()) {
            return Ok(grant.access());
        }
        let openai = self.openai();
        let reach = Reach::of(self);
        let refreshed: Tokens = post_json(
            &reach,
            &openai.token_url(),
            &serde_json::json!({
                "client_id": openai.client_id,
                "grant_type": "refresh_token",
                "refresh_token": grant.refresh_token,
                "scope": "openid profile email",
            }),
        )
        .await
        .map_err(|failure| match failure {
            // Turned away: only signing in again mends it.
            Failure::Status(400 | 401 | 403) => signed_out(),
            other => {
                tracing::info!(failure = %other.words(), "a ChatGPT sign-in was not refreshed");
                ModelRefusal::Provider(agent_provider_config::protocol::ModelError::new(
                    agent_provider_config::protocol::ErrorKind::Unavailable,
                ))
            }
        })?;
        let renewed = Grant {
            access_token: refreshed
                .access_token
                .clone()
                .unwrap_or_else(|| grant.access_token.clone()),
            refresh_token: refreshed
                .refresh_token
                .clone()
                .unwrap_or_else(|| grant.refresh_token.clone()),
            id_token: refreshed
                .id_token
                .clone()
                .unwrap_or_else(|| grant.id_token.clone()),
        };
        let sealed = renewed.sealed();
        let reference = crate::person_settings::key_reference(profile).ok_or_else(signed_out)?;
        keys.vault()
            .renew_sign_in(&reference, sealed)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "a refreshed ChatGPT sign-in was not kept");
                ModelRefusal::SettingsUnreadable
            })?;
        Ok(renewed.access())
    }
}

/// `OpenAI`'s one-time code for a sign-in.
#[derive(Clone, Deserialize)]
#[expect(
    clippy::struct_field_names,
    reason = "OpenAI's own name for the code, as it writes it"
)]
struct UserCode {
    device_auth_id: String,
    #[serde(alias = "usercode")]
    user_code: String,
    /// Seconds between two asks; `OpenAI` writes it as a string.
    #[serde(default, deserialize_with = "seconds")]
    interval: u64,
}

fn seconds<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Written {
        Number(u64),
        Text(String),
    }
    Ok(match Written::deserialize(deserializer)? {
        Written::Number(seconds) => seconds,
        Written::Text(text) => text.trim().parse().map_err(serde::de::Error::custom)?,
    })
}

/// What `OpenAI` hands over once the code was entered.
#[derive(Deserialize)]
struct Granted {
    authorization_code: String,
    code_verifier: String,
}

/// Ask `OpenAI` whether `code` was entered, every few seconds as it says,
/// until it was.
async fn entered(openai: &OpenAiSignIn, code: &UserCode, reach: &Reach) -> Result<Granted, String> {
    let every = Duration::from_secs(code.interval.clamp(1, 30));
    let url = format!("{}/api/accounts/deviceauth/token", openai.issuer());
    let asking = serde_json::json!({
        "device_auth_id": code.device_auth_id,
        "user_code": code.user_code,
    });
    let mut unanswered = 0_u32;
    loop {
        tokio::time::sleep(every).await;
        match post_json::<Granted>(reach, &url, &asking).await {
            Ok(granted) => return Ok(granted),
            // Not entered yet.
            Err(Failure::Status(403 | 404)) => unanswered = 0,
            // A connection lost now and then is asked again.
            Err(Failure::Unreachable) if unanswered < 5 => {
                unanswered = unanswered.saturating_add(1);
            }
            Err(failure) => return Err(failure.words()),
        }
    }
}

/// An API key for the account's API organisation, issued for its id token.
async fn api_key(
    openai: &OpenAiSignIn,
    id_token: &str,
    reach: &Reach,
) -> Result<Zeroizing<String>, String> {
    if id_token.is_empty() {
        return Err("OpenAI did not say which account signed in.".to_owned());
    }
    let issued: Tokens = post_form(
        reach,
        &openai.token_url(),
        &[
            (
                "grant_type",
                "urn:ietf:params:oauth:grant-type:token-exchange",
            ),
            ("client_id", &openai.client_id),
            ("requested_token", "openai-api-key"),
            ("subject_token", id_token),
            (
                "subject_token_type",
                "urn:ietf:params:oauth:token-type:id_token",
            ),
        ],
    )
    .await
    .map_err(|failure| match failure {
        Failure::Status(400..=403) => "OpenAI issued no API key for this account. It may have \
             no API organisation yet: set one up at platform.openai.com, or sign in to use your \
             ChatGPT plan instead."
            .to_owned(),
        other => other.words(),
    })?;
    issued
        .access_token
        .clone()
        .map(Zeroizing::new)
        .ok_or_else(|| "OpenAI issued no API key for this account.".to_owned())
}

/// The provider an account `using` answers as: the one kept already, or a new
/// one. A plan's provider signs in; API usage is `OpenAI`'s API with a key.
fn kept_provider(
    root: &Path,
    openai: &OpenAiSignIn,
    using: AccountUse,
) -> Result<ProviderProfile, String> {
    let mut config = provider_config(root);
    let is_it = |profile: &ProviderProfile| match using {
        AccountUse::Subscription => profile.sign_in_issuer() == Some(SignInIssuer::ChatGpt),
        AccountUse::ApiUsage => {
            profile.sign_in.is_none()
                && profile.endpoint.trim_end_matches('/') == openai.api.trim_end_matches('/')
        }
    };
    if let Some(kept) = config.profiles.iter_mut().find(|profile| is_it(profile)) {
        kept.enabled = true;
        let kept = kept.clone();
        save_provider_config(root, &config)?;
        return Ok(kept);
    }
    let mut profile = match using {
        AccountUse::Subscription => {
            let mut profile = Preset::CHATGPT.profile();
            profile.endpoint.clone_from(&openai.backend);
            profile
        }
        AccountUse::ApiUsage => {
            let mut profile = Preset::OPENAI.profile();
            profile.endpoint.clone_from(&openai.api);
            profile
        }
    };
    profile.enabled = true;
    config.profiles.push(profile.clone());
    save_provider_config(root, &config)?;
    Ok(profile)
}

/// List `profile`'s models with `key`, keep them, and where no default
/// answers yet, make the first the default: a person who just signed in can use
/// it at once.
async fn listed_and_chosen(
    root: &Path,
    profile: ProviderProfile,
    key: &str,
    reach: &Reach,
) -> Result<ProviderView, String> {
    let listed = listing(&profile, Some(key), reach)
        .await
        .unwrap_or_default();
    let mut config = provider_config(root);
    let none_chosen = !default_answers(&config);
    let kept = config
        .profiles
        .iter_mut()
        .find(|kept| kept.id == profile.id)
        .ok_or_else(|| "That provider is not kept here".to_owned())?;
    if !listed.is_empty() {
        kept.models = merged(
            std::mem::take(&mut kept.models),
            listed,
            &std::collections::BTreeSet::default(),
        );
    }
    if none_chosen && let Some(first) = kept.models.first() {
        kept.default_model = Some(first.id.clone());
        config.default_profile = Some(profile.id.clone());
    }
    let kept = config
        .profiles
        .iter()
        .find(|kept| kept.id == profile.id)
        .cloned()
        .ok_or_else(|| "That provider is not kept here".to_owned())?;
    save_provider_config(root, &config)?;
    Ok(view_of(&kept))
}

/// Whether the default model is one that can answer: its provider is kept
/// and reached with a key or a grant.
fn default_answers(config: &ProviderConfig) -> bool {
    config
        .default_profile
        .as_ref()
        .and_then(|id| config.profiles.iter().find(|profile| profile.id == *id))
        .is_some_and(|profile| profile.default_model.is_some() && key_kept(profile))
}

/// Why a request to `OpenAI`'s sign-in got no answer it could use.
enum Failure {
    Unreachable,
    Status(u16),
    Unreadable,
}

impl Failure {
    fn words(&self) -> String {
        match self {
            Self::Unreachable => "OpenAI could not be reached. Try again in a moment.".to_owned(),
            Self::Status(status) => format!("OpenAI refused the sign-in ({status})."),
            Self::Unreadable => "OpenAI answered something unexpected.".to_owned(),
        }
    }
}

async fn post_json<T: serde::de::DeserializeOwned>(
    reach: &Reach,
    url: &str,
    body: &serde_json::Value,
) -> Result<T, Failure> {
    let body = serde_json::to_vec(body).map_err(|_| Failure::Unreadable)?;
    post(reach, url, "application/json", body).await
}

async fn post_form<T: serde::de::DeserializeOwned>(
    reach: &Reach,
    url: &str,
    fields: &[(&str, &str)],
) -> Result<T, Failure> {
    let body = Zeroizing::new(
        form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields)
            .finish(),
    );
    post(
        reach,
        url,
        "application/x-www-form-urlencoded",
        body.as_bytes().to_vec(),
    )
    .await
}

/// One request to `OpenAI`'s sign-in, at an address this server calls.
async fn post<T: serde::de::DeserializeOwned>(
    reach: &Reach,
    url: &str,
    content_type: &'static str,
    body: Vec<u8>,
) -> Result<T, Failure> {
    let body = Zeroizing::new(body);
    let url = tilo_net::Url::parse(url).map_err(|_| Failure::Unreachable)?;
    tilo_net::proxy::host::check_destination(&url, reach.destinations)
        .map_err(|_| Failure::Unreachable)?;
    let client = guarded_client(reach.destinations, Arc::clone(&reach.lookup))
        .map_err(|_| Failure::Unreachable)?;
    let mut response = client
        .post(url)
        .timeout(REQUEST_TIMEOUT)
        .header("content-type", content_type)
        .header("accept", "application/json")
        .body(body.to_vec())
        .send()
        .await
        .map_err(|error| {
            tracing::info!(error = %error.without_url(), "OpenAI's sign-in did not answer");
            Failure::Unreachable
        })?;
    let status = response.status().as_u16();
    if !(200..=299).contains(&status) {
        return Err(Failure::Status(status));
    }
    let mut read = Zeroizing::new(Vec::new());
    while let Some(chunk) = response.chunk().await.map_err(|_| Failure::Unreachable)? {
        if read.len().saturating_add(chunk.len()) > MAX_ANSWER_BYTES {
            return Err(Failure::Unreadable);
        }
        read.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&read).map_err(|_| Failure::Unreadable)
}
