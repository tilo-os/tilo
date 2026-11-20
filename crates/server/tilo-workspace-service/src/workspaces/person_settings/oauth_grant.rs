//! One provider's grant, kept in the person's vault.
//!
//! A provider that signs a person in on its own page leaves a grant: the
//! access token mail, calendar and models read with, the token that renews
//! it, when it expires and the scopes it was given. It is sealed as the
//! shared `oauth_secret` shape, so every reader of a person's accounts reads
//! it the same way, and it is renewed through the one credential store,
//! behind a lock so two readers of one grant never renew it at once.
//!
//! Renewal needs the registration the grant was given to, which for a server
//! is the provider's web client, read from the server's environment: the
//! vault store below is the one place that pairs them.

use std::path::Path;
use std::sync::Mutex;

use account_credentials::{Capability, CredentialStore, OAuthIssuer, OAuthUse};
use tilo_oauth::{Registration, TokenSet};

/// One provider's grant for one of a person's accounts.
#[allow(
    dead_code,
    reason = "where every provider's grant is kept and read: mail renews it today, and a model provider's sign-in reads it next"
)]
pub(crate) struct ProviderGrant {
    /// The account it was signed in for, as the person's store names it.
    pub account: String,
    pub issuer: OAuthIssuer,
    pub tokens: TokenSet,
}

impl ProviderGrant {
    /// Keep `tokens` as `account`'s grant, typed for the servers its settings
    /// name, in the person's vault.
    pub(crate) fn seal(
        root: &Path,
        account: &accounts::Account,
        tokens: &TokenSet,
    ) -> Result<(), String> {
        let mut vault = super::credentials(root)?;
        vault.put_for(account, &account_credentials::oauth_secret(tokens));
        vault.try_save()
    }

    /// The grant kept for `account`, when there is one.
    #[allow(
        dead_code,
        reason = "a reader of a grant that does not read mail (the model provider's sign-in) lands separately"
    )]
    pub(crate) fn load(root: &Path, account: &str) -> Result<Option<Self>, String> {
        let vault = super::credentials(root)?;
        let Some(secret) = vault.get(&accounts::AccountId::from(account.to_owned())) else {
            return Ok(None);
        };
        let Some(tokens) = account_credentials::oauth_tokens(&secret) else {
            return Ok(None);
        };
        let issuer = issuer_of_account(root, account)?;
        Ok(Some(Self {
            account: account.to_owned(),
            issuer,
            tokens,
        }))
    }

    /// The access token to use now, renewed through the one shared path when
    /// it ran out. The renewal holds the grant's lock, so two readers never
    /// renew the same grant at once.
    pub(crate) fn access_token(
        root: &Path,
        account: &str,
        use_: OAuthUse,
    ) -> Result<String, String> {
        // Renewal is rare and short, and one at a time: a second reader
        // waits here and then finds the fresh token.
        let _renewing = RENEWING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        vault_store(root)
            .oauth_access_token(&accounts::AccountId::from(account.to_owned()), use_, false)
            .map(|token| token.to_string())
            .map_err(|_| "That account's sign-in needs renewing. Sign in again".to_string())
    }
}

/// The grants being renewed now: one at a time, so two readers never renew
/// one grant at once.
static RENEWING: Mutex<()> = Mutex::new(());

/// The person's vault as a reader of a web sign-in's grants: each use of an
/// issuer is renewed with the web client it was signed in with, never with
/// another registration.
pub(super) fn vault_store(root: &Path) -> CredentialStore {
    let mut store = CredentialStore::for_root(Some(root.to_path_buf()));
    for issuer in [OAuthIssuer::Google, OAuthIssuer::Microsoft] {
        let Some(client) = account_credentials::web_client(issuer) else {
            continue;
        };
        for use_ in [OAuthUse::mail(issuer), OAuthUse::calendar(issuer)] {
            if use_.capability == Capability::Calendar && issuer != OAuthIssuer::Google {
                continue;
            }
            let scopes = account_credentials::web_scopes(
                issuer,
                use_.capability == Capability::Mail,
                use_.capability == Capability::Calendar,
            );
            let provider = account_credentials::web_provider(issuer, scopes, &client);
            let registration = Registration {
                client_id: client.client_id.clone(),
                client_secret: client.client_secret.clone(),
                paste_code_fallback: false,
                ..Registration::default()
            };
            store = store.with_use(use_, provider, registration);
        }
    }
    store
}

/// The issuer a kept account signs in with, from its settings.
#[allow(
    dead_code,
    reason = "only reading a grant back asks this; mail's reader knows its issuer from the account it was given"
)]
fn issuer_of_account(root: &Path, account: &str) -> Result<OAuthIssuer, String> {
    let store = accounts::AccountStore::load_in(root);
    let kept = store
        .get(&accounts::AccountId::from(account.to_owned()))
        .ok_or_else(|| "That account is not kept here".to_string())?;
    match kept
        .config
        .get("provider")
        .and_then(serde_json::Value::as_str)
    {
        Some("google" | "google-drive") => Ok(OAuthIssuer::Google),
        Some("microsoft" | "onedrive") => Ok(OAuthIssuer::Microsoft),
        _ => Err("That account's provider is not one this server signs in with".into()),
    }
}
