//! Signing in on a provider's own page.
//!
//! A page has no vault and cannot keep the `OAuth` code its provider sends
//! back, so the server that keeps a person's accounts does both: it begins
//! the sign-in with a proof key and a `state` bound to that person
//! ([`BEGIN_SIGN_IN`]), sends their browser to the provider, and finishes it
//! when the provider sends the browser back to `GET /accounts/return`
//! ([`Workspaces::accounts_return`]). The grant it gets is sealed in the
//! person's vault the way a device's own sign-in seals it
//! ([`account_credentials::oauth_secret`]), so one refresh path renews it for
//! mail, calendar and every reader.
//!
//! Nextcloud signs in by its own login flow: the server starts the flow,
//! shows its page, and polls in the background until the person approved
//! access there or the flow ran out. The app password it mints is sealed the
//! same way.
//!
//! What is begun is kept in memory alone, one entry per sign-in, expiring;
//! nothing a provider returns travels in a URL beyond its one-time `state`
//! and code, and no token is ever answered to a caller.
//!
//! [`BEGIN_SIGN_IN`]: tilo_actions::remote::person_accounts::BEGIN_SIGN_IN

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use account_credentials::{CredentialStore, OAuthIssuer, OAuthUse, nextcloud};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use tilo_actions::remote::person_accounts::{
    BeginSignIn, CancelSignIn, Provider, Services, SignInStep, SigningIn,
};
use tilo_net::Url;
use tilo_oauth::client::OAuthClient;
use tilo_oauth::{AuthorizationRequest, Pkce, Registration, TokenSet};

use crate::workspaces::Workspaces;

/// How long a person has to come back from a provider's page.
const SIGN_IN_MINUTES: u64 = 10;
/// How long a Nextcloud login flow may stay open.
const NEXTCLOUD_MINUTES: u64 = 20;
/// How often a Nextcloud server is asked whether the person approved.
const POLL_EVERY: Duration = Duration::from_secs(2);
/// The most sign-ins begun and not forgotten at once.
const MAX_BEGUN: usize = 64;
/// The Google endpoint that says which address a grant belongs to when the
/// grant carries no mail permission to read a profile with.
const GOOGLE_USERINFO_URL: &str = "https://openidconnect.googleapis.com/v1/userinfo";
/// The account type a calendar server is kept as, in the shared store.
const CALDAV_TYPE: &str = "caldav";
/// The calendar server Google answers `CalDAV` on, and the name it is kept
/// under.
const GOOGLE_CALDAV: &str = "https://apidata.googleusercontent.com/caldav/v2/";
const GOOGLE_CALDAV_NAME: &str = "apidata.googleusercontent.com";

/// Where a browser comes back to: the origin this server's routes are
/// published at, read once at start.
static ORIGIN: Mutex<Option<String>> = Mutex::new(None);

/// A sign-in begun and not finished.
struct Begun {
    id: String,
    person: [u8; 32],
    /// The person's store this sign-in keeps its account in.
    root: PathBuf,
    until_ms: u64,
    failed: Option<String>,
    kind: Kind,
}

enum Kind {
    /// A provider's own page, waiting for the browser to come back with a
    /// code.
    Web {
        issuer: OAuthIssuer,
        verifier: String,
        redirect: String,
        services: Services,
        /// The account this signs in again, when the person named one.
        account: String,
    },
    /// A Nextcloud login flow, polled in the background.
    Nextcloud {
        server: String,
        /// Held by the poll; dropping it stops it.
        stop: Option<mpsc::Sender<()>>,
    },
}

/// Every sign-in begun and not yet forgotten, by its id.
static BEGUN: Mutex<BTreeMap<String, Begun>> = Mutex::new(BTreeMap::new());

fn now_ms() -> u64 {
    tilo_clock::unix_now_ns() / 1_000_000
}

fn minutes_ms(minutes: u64) -> u64 {
    minutes.saturating_mul(60_000)
}

fn begun() -> std::sync::MutexGuard<'static, BTreeMap<String, Begun>> {
    BEGUN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The providers this server can sign a person in with now, in the order a
/// page offers them: Google or Microsoft once a web client is configured for
/// it, Nextcloud always.
#[must_use]
pub(super) fn signs_in_with() -> Vec<Provider> {
    let mut providers = Vec::new();
    for entry in WEB_SIGN_IN {
        if account_credentials::web_client(entry.issuer).is_some() {
            providers.push(entry.provider);
        }
    }
    providers.push(Provider::Nextcloud);
    providers
}

/// The providers this server signs in with on their own page, in the order a
/// page offers them: the browser redirect and code exchange are the same for
/// every one, so another provider is a row here and its client in the
/// server's environment.
struct WebSignIn {
    issuer: OAuthIssuer,
    provider: Provider,
}

const WEB_SIGN_IN: [WebSignIn; 2] = [
    WebSignIn {
        issuer: OAuthIssuer::Google,
        provider: Provider::Google,
    },
    WebSignIn {
        issuer: OAuthIssuer::Microsoft,
        provider: Provider::Microsoft,
    },
];

const fn provider_of(issuer: OAuthIssuer) -> Provider {
    match issuer {
        OAuthIssuer::Google => Provider::Google,
        OAuthIssuer::Microsoft => Provider::Microsoft,
        _ => Provider::Other,
    }
}

const fn issuer_of(provider: Provider) -> Option<OAuthIssuer> {
    match provider {
        Provider::Google => Some(OAuthIssuer::Google),
        Provider::Microsoft => Some(OAuthIssuer::Microsoft),
        _ => None,
    }
}

const fn provider_label(provider: Provider) -> &'static str {
    match provider {
        Provider::Google => "Google",
        Provider::Microsoft => "Microsoft",
        Provider::Icloud => "iCloud",
        Provider::Nextcloud => "Nextcloud",
        Provider::Other => "That provider",
    }
}

/// What the person signs in for: what they chose, or, when they chose
/// nothing, what the provider is used for by default.
fn chosen(provider: Provider, services: Services) -> Services {
    let chosen = if services.none() {
        Services::default()
    } else {
        services
    }
    .within(provider);
    if !chosen.none() {
        return chosen;
    }
    match provider {
        Provider::Google | Provider::Microsoft => Services {
            mail: true,
            ..Services::default()
        },
        Provider::Nextcloud => Services {
            mail: false,
            calendar: true,
            files: false,
        },
        Provider::Icloud | Provider::Other => services.within(provider),
    }
}

/// The sign-ins begun by `person` that made no account yet, oldest first.
pub(super) fn signing_in(root: &Path) -> Vec<SigningIn> {
    prune();
    begun()
        .values()
        .filter(|entry| entry.root == root)
        .map(|entry| SigningIn {
            id: entry.id.clone(),
            provider: match &entry.kind {
                Kind::Web { issuer, .. } => provider_of(*issuer),
                Kind::Nextcloud { .. } => Provider::Nextcloud,
            },
            server: match &entry.kind {
                Kind::Nextcloud { server, .. } => server.clone(),
                Kind::Web { .. } => String::new(),
            },
            failed: entry.failed.clone(),
        })
        .collect()
}

/// Forget every sign-in that ran out.
fn prune() {
    let now = now_ms();
    begun().retain(|_, entry| entry.until_ms > now);
}

/// Where a browser comes back to: the origin this server's routes are
/// published at, as the server was started.
fn return_to() -> Result<String, String> {
    let origin = ORIGIN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .ok_or_else(|| {
            "This server was not told where a browser comes back to, so nobody can sign in here"
                .to_string()
        })?;
    Ok(format!("{}/accounts/return", origin.trim_end_matches('/')))
}

impl Workspaces {
    /// Say the origin this server's browser routes are published at, so a
    /// provider can send a browser back here to finish a sign-in.
    pub fn accounts_call_back_at(&self, origin: impl Into<String>) {
        *ORIGIN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(origin.into());
    }

    /// Begin signing in on `asking`'s provider page for `person`, answering
    /// where their browser goes. What reaches the person's store is what
    /// [`BEGIN_SIGN_IN`] says: nothing yet.
    ///
    /// # Errors
    /// The provider is not one this server signs in with, nothing says where
    /// a browser comes back to, the server it would ask is one it may not
    /// call, or too many sign-ins are already begun.
    ///
    /// [`BEGIN_SIGN_IN`]: tilo_actions::remote::person_accounts::BEGIN_SIGN_IN
    pub(super) async fn begin_sign_in(
        &self,
        person: [u8; 32],
        asking: &BeginSignIn,
        reach: &tilo_account_probe::imap::Reach,
    ) -> Result<SignInStep, String> {
        let provider = asking.provider;
        let services = chosen(provider, asking.services);
        prune();
        if begun().len() >= MAX_BEGUN {
            return Err("Too many sign-ins are open here. Finish or cancel one first".into());
        }
        if let Some(issuer) = issuer_of(provider) {
            self.begin_web(person, issuer, services, asking)
        } else if provider == Provider::Nextcloud {
            self.begin_nextcloud(person, services, asking, reach).await
        } else {
            Err(format!(
                "{} signs in with a password, not on its own page",
                provider_label(provider)
            ))
        }
    }

    fn begin_web(
        &self,
        person: [u8; 32],
        issuer: OAuthIssuer,
        services: Services,
        asking: &BeginSignIn,
    ) -> Result<SignInStep, String> {
        let client = account_credentials::web_client(issuer).ok_or_else(|| {
            format!(
                "{} sign-in is not set up on this server",
                provider_label(provider_of(issuer))
            )
        })?;
        let scopes = account_credentials::web_scopes(issuer, services.mail, services.calendar);
        if scopes.is_empty() {
            return Err("Choose what the account is for first".into());
        }
        let redirect = return_to()?;
        let pkce = Pkce::generate().map_err(|error| error.to_string())?;
        let state = tilo_oauth::random_token().map_err(|error| error.to_string())?;
        let id = hex::encode(Sha256::digest(state.as_bytes()));
        let descriptor = account_credentials::web_provider(issuer, scopes, &client);
        let open = AuthorizationRequest {
            provider: &descriptor,
            client_id: &client.client_id,
            redirect_uri: &redirect,
            code_challenge: &pkce.challenge,
            state: &state,
            login_hint: None,
        }
        .url();
        begun().insert(
            id.clone(),
            Begun {
                id: id.clone(),
                person,
                root: self.person_settings_root(&person),
                until_ms: now_ms().saturating_add(minutes_ms(SIGN_IN_MINUTES)),
                failed: None,
                kind: Kind::Web {
                    issuer,
                    verifier: pkce.verifier,
                    redirect,
                    services,
                    account: asking.account.trim().to_owned(),
                },
            },
        );
        Ok(SignInStep { id, open })
    }

    async fn begin_nextcloud(
        &self,
        person: [u8; 32],
        services: Services,
        asking: &BeginSignIn,
        reach: &tilo_account_probe::imap::Reach,
    ) -> Result<SignInStep, String> {
        let server = asking.server.trim().to_owned();
        if server.is_empty() {
            return Err("Enter the address of the Nextcloud server".into());
        }
        let parsed = nextcloud::server_url(&server).map_err(|error| error.to_string())?;
        let named = named_by(&parsed)?;
        if !reach.takes(&named) {
            return Err("That server's address is not one this server may call".into());
        }
        let flow = tokio::task::spawn_blocking({
            let server = server.clone();
            move || nextcloud::begin(&server).map_err(|error| error.to_string())
        })
        .await
        .map_err(|_| "the Nextcloud server was not asked to the end".to_string())??;
        let id = tilo_oauth::random_token().map_err(|error| error.to_string())?;
        let open = flow.login_url().to_owned();
        let (stop, cancelled) = nextcloud::cancellation();
        let account = asking.account.trim().to_owned();
        let root = self.person_settings_root(&person);
        begun().insert(
            id.clone(),
            Begun {
                id: id.clone(),
                person,
                root: root.clone(),
                until_ms: now_ms().saturating_add(minutes_ms(NEXTCLOUD_MINUTES)),
                failed: None,
                kind: Kind::Nextcloud {
                    server,
                    stop: Some(stop),
                },
            },
        );
        let workspaces = self.clone();
        let kept_id = id.clone();
        let cancelled_at = now_ms().saturating_add(minutes_ms(NEXTCLOUD_MINUTES));
        tokio::spawn(async move {
            let waited = tokio::task::spawn_blocking(move || {
                let login = flow
                    .wait(
                        &cancelled,
                        POLL_EVERY,
                        Duration::from_secs(minutes_ms(NEXTCLOUD_MINUTES) / 1000),
                    )
                    .map_err(|error| error.to_string())?;
                keep_nextcloud(&root, &login, services, &account)
            })
            .await;
            match waited {
                Ok(Ok(())) => {
                    begun().remove(&kept_id);
                    workspaces.person_settings_changed();
                }
                Ok(Err(reason)) => keep_failed(&kept_id, &reason, cancelled_at),
                Err(_) => keep_failed(&kept_id, "the Nextcloud answer was not read", cancelled_at),
            }
        });
        Ok(SignInStep { id, open })
    }

    /// Begin the sign-in a request asks for, and wake the windows following
    /// the accounts once it is listed.
    pub(super) async fn begin_account_sign_in(
        &self,
        person: [u8; 32],
        asking: &BeginSignIn,
    ) -> Result<SignInStep, String> {
        let reach = self.inner.models.mail_reach();
        let step = self.begin_sign_in(person, asking, &reach).await;
        if step.is_ok() {
            self.person_settings_changed();
        }
        step
    }

    /// Stop waiting for a sign-in, or forget one that failed, and answer the
    /// accounts as they now are.
    pub(super) fn cancel_account_sign_in(
        &self,
        person: [u8; 32],
        stopping: &CancelSignIn,
        root: &Path,
    ) -> Result<super::MailAccounts, String> {
        cancel(&person, stopping)?;
        let listed = super::mail_accounts(root)?;
        self.person_settings_changed();
        Ok(listed)
    }

    /// Finish the sign-in a browser came back from at `GET /accounts/return`,
    /// keeping the person's account.
    ///
    /// # Errors
    /// The callback names no sign-in begun here, ran out, was refused by the
    /// provider, or could not be kept. The words are the person's.
    pub async fn accounts_return(&self, query: &str) -> Result<(), String> {
        let state = query_fields(query)
            .get("state")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_default();
        let id = hex::encode(Sha256::digest(state.as_bytes()));
        let person = begun()
            .get(&id)
            .filter(|entry| entry.until_ms > now_ms())
            .map(|entry| entry.person)
            .ok_or_else(|| {
                "That sign-in was not begun here or took too long. Begin it again".to_string()
            })?;
        let root = self.person_settings_root(&person);
        finish(&root, &id, person, query).await?;
        self.person_settings_changed();
        Ok(())
    }
}

/// Keep a sign-in's failure with it, so the person reads why and can cancel
/// it, and stop a Nextcloud poll that is still running.
fn keep_failed(id: &str, reason: &str, until_ms: u64) {
    let mut entries = begun();
    if let Some(entry) = entries.get_mut(id) {
        entry.failed = Some(reason.to_owned());
        entry.until_ms = until_ms;
        if let Kind::Nextcloud { stop, .. } = &mut entry.kind {
            *stop = None;
        }
    }
}

/// Answer a request of the sign-in actions: where a browser goes, or the
/// accounts as they are once a sign-in was stopped.
///
/// # Errors
/// The request does not read, or names a provider or sign-in this server
/// does not know.
pub(super) async fn accounts_sign_in(
    workspaces: &Workspaces,
    person: [u8; 32],
    root: &Path,
    request: &tilo_actions::remote::InvokeRequest,
) -> Result<serde_json::Value, tilo_actions::remote::ActionCallError> {
    use tilo_actions::remote::person_accounts::{BEGIN_SIGN_IN, CANCEL_SIGN_IN};

    match request.target.address.action.as_str() {
        BEGIN_SIGN_IN => {
            let asking: BeginSignIn = super::argument(request, "a provider to sign in with")?;
            super::answer(&workspaces.begin_account_sign_in(person, &asking).await)
        }
        CANCEL_SIGN_IN => {
            let stopping: CancelSignIn = super::argument(request, "a sign-in to stop")?;
            super::answer(&workspaces.cancel_account_sign_in(person, &stopping, root))
        }
        _ => Err(tilo_actions::remote::ActionCallError::UnknownAction),
    }
}

/// Let a sign-in go stale now, as the minutes passing would.
#[cfg(test)]
pub(super) fn expire(id: &str) {
    if let Some(entry) = begun().get_mut(id) {
        entry.until_ms = 0;
    }
}

/// Stop waiting for the sign-in `cancel` names, or forget one that failed.
///
/// # Errors
/// No such sign-in was begun by this person.
pub(super) fn cancel(person: &[u8; 32], cancel: &CancelSignIn) -> Result<(), String> {
    let theirs = begun()
        .get(&cancel.sign_in)
        .is_some_and(|entry| &entry.person == person);
    if !theirs {
        return Err("That sign-in was not begun here".into());
    }
    begun().remove(&cancel.sign_in);
    Ok(())
}

/// Finish the sign-in the browser came back from, keeping the account on
/// success and saying why nothing was kept otherwise.
async fn finish(root: &Path, id: &str, person: [u8; 32], query: &str) -> Result<(), String> {
    let fields = query_fields(query);
    let waiting = {
        let mut entries = begun();
        entries.remove(id)
    }
    .ok_or_else(|| {
        "That sign-in was not begun here or took too long. Begin it again".to_string()
    })?;
    let Kind::Web {
        issuer,
        verifier,
        redirect,
        services,
        account,
    } = waiting.kind
    else {
        return Err("That answer does not finish a web sign-in".into());
    };
    if let Some(reason) = fields.get("error").and_then(Value::as_str) {
        let said = declined(reason);
        keep_failed(id, &said, waiting.until_ms);
        return Err(said);
    }
    let code = fields
        .get("code")
        .and_then(Value::as_str)
        .filter(|code| !code.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "The provider sent no sign-in code".to_string())?;
    let client = account_credentials::web_client(issuer)
        .ok_or_else(|| "Sign-in with that provider is not set up on this server".to_string())?;
    let scopes = account_credentials::web_scopes(issuer, services.mail, services.calendar);
    let descriptor = account_credentials::web_provider(issuer, scopes, &client);
    let registration = Registration {
        client_id: client.client_id.clone(),
        client_secret: client.client_secret.clone(),
        paste_code_fallback: false,
        ..Registration::default()
    };
    let profile = client.profile.clone();
    let root_owned = root.to_path_buf();
    let kept = tokio::task::spawn_blocking(move || {
        let http = OAuthClient::new().map_err(|error| error.to_string())?;
        let tokens = http
            .exchange_code(
                &descriptor,
                &registration,
                Some(&redirect),
                &code,
                &verifier,
            )
            .map_err(|error| plain_exchange(&error))?
            .stamp_now();
        let address =
            identity_of(&root_owned, issuer, &profile, services, &tokens).map_err(|_| {
                "The provider did not confirm which mailbox signed in. Try again".to_string()
            })?;
        keep_oauth(&root_owned, issuer, &address, services, &tokens, &account)
    })
    .await
    .map_err(|_| "The provider's answer was not read to the end".to_string())?;
    if let Err(said) = kept {
        let mut entries = begun();
        entries.insert(
            id.to_owned(),
            Begun {
                id: id.to_owned(),
                person,
                root: root.to_path_buf(),
                until_ms: now_ms().saturating_add(minutes_ms(SIGN_IN_MINUTES)),
                failed: Some(said.clone()),
                kind: Kind::Web {
                    issuer,
                    verifier: String::new(),
                    redirect: String::new(),
                    services,
                    account: String::new(),
                },
            },
        );
        drop(entries);
        return Err(said);
    }
    Ok(())
}

/// The words a declined sign-in is shown with.
fn declined(reason: &str) -> String {
    let reason: String = reason
        .chars()
        .filter(|letter| letter.is_ascii_alphanumeric() || matches!(letter, '_' | '-'))
        .take(64)
        .collect();
    if reason == "access_denied" {
        "The sign-in was cancelled on the provider's page".into()
    } else {
        "The provider did not sign you in. Begin it again".into()
    }
}

/// Why a code exchange failed, in words the person reads: nothing of the
/// provider's protocol or its answer.
fn plain_exchange(error: &tilo_oauth::OAuthError) -> String {
    match error {
        tilo_oauth::OAuthError::InvalidGrant => {
            "That sign-in link was already used or ran out. Begin it again".to_string()
        }
        tilo_oauth::OAuthError::Denied(_) => {
            "The sign-in was cancelled on the provider's page".to_string()
        }
        tilo_oauth::OAuthError::Network(_) | tilo_oauth::OAuthError::Protocol(_) => {
            "This server could not reach the provider. Try again".to_string()
        }
        _ => "The provider did not finish the sign-in. Try again".to_string(),
    }
}

/// Whether `account` signs in with a bearer token instead of a password.
fn signed_in_with_oauth(account: &accounts::Account) -> bool {
    accounts::email::is_oauth(&account.config)
}

/// The issuer a mail account signs in with: what its settings say, or, for
/// an account kept before that was written, its servers' names.
fn issuer_for(account: &accounts::Account) -> Result<OAuthIssuer, String> {
    match account.config.get("provider").and_then(Value::as_str) {
        Some("google") => return Ok(OAuthIssuer::Google),
        Some("microsoft") => return Ok(OAuthIssuer::Microsoft),
        _ => {}
    }
    let marked = |domain: &str| {
        account
            .config
            .get("imap_host")
            .and_then(Value::as_str)
            .is_some_and(|name| {
                name.trim().trim_end_matches('.').to_ascii_lowercase() == domain
                    || name
                        .trim()
                        .trim_end_matches('.')
                        .to_ascii_lowercase()
                        .ends_with(&format!(".{domain}"))
            })
    };
    if marked("gmail.com") || marked("googlemail.com") {
        Ok(OAuthIssuer::Google)
    } else if marked("outlook.com") || marked("office365.com") || marked("office.com") {
        Ok(OAuthIssuer::Microsoft)
    } else {
        Err("That account's provider is not one this server signs in with".into())
    }
}

/// The sign-in a mail account reads and sends with: its bearer token, kept
/// fresh through the one shared renewal path, or its password.
pub(super) fn mail_secret(
    root: &Path,
    account: &accounts::Account,
) -> Result<accounts::Secret, String> {
    let vault = super::credentials(root)?;
    let secret = vault
        .get_for(account)
        .ok_or_else(|| "That account has no sign-in yet".to_string())?;
    if !signed_in_with_oauth(account) {
        return Ok(secret);
    }
    let issuer = issuer_for(account)?;
    let token = super::oauth_grant::ProviderGrant::access_token(
        root,
        account.id.as_str(),
        OAuthUse::mail(issuer),
    )?;
    Ok(accounts::Secret::password(token))
}

/// Which address the grant belongs to: what the account is kept as, shown
/// to nobody else.
fn identity_of(
    root: &Path,
    issuer: OAuthIssuer,
    profile: &str,
    services: Services,
    tokens: &TokenSet,
) -> Result<String, String> {
    let store = CredentialStore::for_root(Some(root.to_path_buf()))
        .with_gmail_profile_endpoint(profile.to_owned())
        .with_microsoft_profile_endpoint(profile.to_owned());
    match issuer {
        OAuthIssuer::Microsoft if services.mail => store
            .microsoft_mail_identity(tokens)
            .map_err(|error| error.to_string()),
        OAuthIssuer::Google if services.mail => store
            .gmail_identity(tokens)
            .map_err(|error| error.to_string()),
        OAuthIssuer::Google => {
            let endpoint = match std::env::var("TILO_OAUTH_GOOGLE_USERINFO_URL") {
                Ok(url) if !url.trim().is_empty() => url.trim().to_owned(),
                _ => GOOGLE_USERINFO_URL.to_owned(),
            };
            let userinfo = OAuthClient::new()
                .map_err(|error| error.to_string())?
                .userinfo(&endpoint, &tokens.access_token)
                .map_err(|error| error.to_string())?;
            userinfo
                .get("email")
                .and_then(Value::as_str)
                .filter(|email| !email.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| "Google returned no mailbox address".to_string())
        }
        _ => Err("that provider signs nobody in here".to_string()),
    }
}

/// Keep the grant `tokens` as `address`'s account, in the person's own store
/// and vault: the account a page's Mail reads, and, for Google with
/// calendar, the calendar server the same grant opens.
fn keep_oauth(
    root: &Path,
    issuer: OAuthIssuer,
    address: &str,
    services: Services,
    tokens: &TokenSet,
    account: &str,
) -> Result<(), String> {
    let address = address.trim();
    let confirmed = issuer
        .mail_config(address)
        .ok_or_else(|| "The provider confirmed no mailbox address".to_string())?;
    let mut config = match confirmed.to_value().map_err(|error| error.to_string())? {
        Value::Object(config) => config,
        _ => Map::new(),
    };
    config.insert("provider".into(), json!(issuer.id()));
    config.insert(
        "services".into(),
        serde_json::to_value(services).map_err(|error| error.to_string())?,
    );
    let id = if account.trim().is_empty() {
        address.to_owned()
    } else {
        let existing = accounts::AccountStore::load_in(root)
            .get(&accounts::AccountId::from(account.trim().to_owned()))
            .filter(|existing| existing.type_id.as_str() == accounts::email::TYPE_ID)
            .cloned()
            .ok_or_else(|| "That account is not kept here".to_string())?;
        let known = existing
            .config
            .get("email")
            .and_then(Value::as_str)
            .unwrap_or(account)
            .to_owned();
        if !known.eq_ignore_ascii_case(address) {
            return Err(format!(
                "That sign-in is for {address}, but the account it replaces is {known}"
            ));
        }
        existing.id.to_string()
    };
    let mut kept = accounts::Account::new(accounts::AccountId::from(id), accounts::email::TYPE_ID);
    address.clone_into(&mut kept.display_name);
    kept.config = Value::Object(config);
    super::oauth_grant::ProviderGrant::seal(root, &kept, tokens)?;
    let mut store = accounts::AccountStore::load_in(root);
    store.upsert(kept);
    store.save_in(root).map_err(|error| error.to_string())?;
    if issuer == OAuthIssuer::Google && services.calendar {
        keep_google_calendar(root, address, tokens)?;
    }
    Ok(())
}

/// Keep the calendar server the same Google grant opens, beside the mail
/// account, so Calendar reads it where a calendar account is read.
fn keep_google_calendar(root: &Path, address: &str, tokens: &TokenSet) -> Result<(), String> {
    let mut base =
        Url::parse(GOOGLE_CALDAV).map_err(|_| "invalid Google calendar endpoint".to_string())?;
    base.path_segments_mut()
        .map_err(|()| "invalid Google calendar endpoint".to_string())?
        .extend([address, "user"]);
    let server_url = base.to_string();
    let id = format!("caldav:{address}@{GOOGLE_CALDAV_NAME}");
    let mut kept = accounts::Account::new(accounts::AccountId::from(id), CALDAV_TYPE);
    address.clone_into(&mut kept.display_name);
    kept.config = json!({
        "display_name": address,
        "server_url": server_url,
        "username": address,
        "calendar_provider": "google",
    });
    super::oauth_grant::ProviderGrant::seal(root, &kept, tokens)?;
    let mut store = accounts::AccountStore::load_in(root);
    store.upsert(kept);
    store.save_in(root).map_err(|error| error.to_string())?;
    Ok(())
}

/// Keep the Nextcloud account a finished login flow made, its app password in
/// the vault, and the calendar server of the same sign-in when the person
/// asked for calendars.
fn keep_nextcloud(
    root: &Path,
    login: &nextcloud::NextcloudLogin,
    services: Services,
    replace: &str,
) -> Result<(), String> {
    let server = login.server.trim().trim_end_matches('/').to_owned();
    let Ok(url) = Url::parse(&server) else {
        return Err("the server's address does not read".into());
    };
    let site = match url.domain() {
        Some(domain) => domain.to_owned(),
        None => named_by(&url)?,
    };
    let existing = accounts::AccountStore::load_in(root)
        .all()
        .iter()
        .find(|account| {
            account.type_id.as_str() == accounts::cloud_types::NEXTCLOUD
                && account.config.get("server_url").and_then(Value::as_str) == Some(server.as_str())
                && account.config.get("username").and_then(Value::as_str)
                    == Some(login.login_name.as_str())
        })
        .map(|account| account.id.to_string());
    let id = if replace.trim().is_empty() {
        existing.unwrap_or_else(|| format!("nextcloud:{}@{site}", login.login_name))
    } else {
        replace.trim().to_owned()
    };
    let mut kept = accounts::Account::new(
        accounts::AccountId::from(id.clone()),
        accounts::cloud_types::NEXTCLOUD,
    );
    site.clone_into(&mut kept.display_name);
    kept.config = json!({
        "id": id,
        "server_url": server,
        "username": login.login_name.clone(),
        "provider": "nextcloud",
        "use_files": false,
        "use_calendar": services.calendar,
        "use_mail": false,
    });
    let mut vault = super::credentials(root)?;
    vault.put_for(
        &kept,
        &accounts::Secret::password(login.app_password.to_string()),
    );
    let mut store = accounts::AccountStore::load_in(root);
    store.upsert(kept);
    store.save_in(root).map_err(|error| error.to_string())?;
    vault.try_save()?;
    if services.calendar {
        keep_nextcloud_calendar(root, &server, &site, login)?;
    }
    Ok(())
}

/// Keep the `CalDAV` account of a Nextcloud server at its own DAV address,
/// signed in with the app password the flow minted.
fn keep_nextcloud_calendar(
    root: &Path,
    server: &str,
    site: &str,
    login: &nextcloud::NextcloudLogin,
) -> Result<(), String> {
    let server_url = format!("{server}/remote.php/dav");
    let id = format!("caldav:{}@{site}", login.login_name);
    let mut kept = accounts::Account::new(accounts::AccountId::from(id), CALDAV_TYPE);
    login.login_name.clone_into(&mut kept.display_name);
    kept.config = json!({
        "display_name": login.login_name.clone(),
        "server_url": server_url,
        "username": login.login_name.clone(),
        "calendar_provider": "nextcloud",
    });
    let mut vault = super::credentials(root)?;
    vault.put_for(
        &kept,
        &accounts::Secret::password(login.app_password.to_string()),
    );
    let mut store = accounts::AccountStore::load_in(root);
    store.upsert(kept);
    store.save_in(root).map_err(|error| error.to_string())?;
    vault.try_save()?;
    Ok(())
}

/// The name a server's address answers to, without its scheme, port or
/// path: what a reach judges.
fn named_by(url: &Url) -> Result<String, String> {
    if let Some(domain) = url.domain() {
        return Ok(domain.to_owned());
    }
    let authority = url
        .as_str()
        .split_once("://")
        .map_or("", |(_, authority)| authority);
    let name = authority
        .split(['/', ':', '?', '#'])
        .next()
        .unwrap_or_default();
    if name.is_empty() {
        return Err("The server's address does not read".into());
    }
    Ok(name.to_owned())
}

/// The key/values a browser came back with.
fn query_fields(query: &str) -> Map<String, Value> {
    let url = tilo_failure::ignored!(
        "a callback query that does not read names no sign-in",
        Url::parse(&format!("http://localhost/?{query}")).ok()
    );
    let Some(url) = url else {
        return Map::new();
    };
    url.query_pairs()
        .map(|(key, value)| (key.into_owned(), Value::String(value.into_owned())))
        .collect()
}
