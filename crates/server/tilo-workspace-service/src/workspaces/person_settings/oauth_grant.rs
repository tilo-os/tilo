//! One provider's grant, kept in the workspace's vault.
//!
//! A provider that signs a person in on its own page leaves a grant: the
//! access token mail and calendar read with, the token that renews it, when
//! it expires and the scopes it was given. It is kept as an entry of the
//! workspace's vault in the shared `oauth_secret` shape, the account's own
//! sign-in ([`super::kept_sign_ins`]), and it is renewed through the one
//! shared renewal ([`account_credentials::CredentialStore::renew_grant`]),
//! behind a lock so two readers of one grant never renew it at once.
//!
//! Renewal needs the registration the grant was given to, which for a server
//! is the provider's web client, read from the server's environment
//! ([`renewal`]).

use std::path::Path;

use account_credentials::{
    Capability, CredentialError, CredentialStore, OAuthIssuer, OAuthUse, RenewedGrant,
};
use accounts::vault_items::ItemKind;
use tilo_oauth::{Registration, TokenSet};
use zeroize::Zeroizing;

use super::super::vault::VaultAt;
use super::kept_sign_ins::{SIGN_IN, places_of, sign_in_of};

/// What an account whose provider will not renew its grant is told: the
/// person signs in again, which is all that helps.
const SIGN_IN_AGAIN: &str = "That account's sign-in needs renewing. Sign in again";

/// Keep `tokens` as `account`'s grant, an entry of the workspace's vault that belongs to the
/// servers its settings name, in place of the grant it kept before; `account`'s settings name it
/// from then on, for whoever keeps the account to save.
///
/// # Errors
/// The account names no server, or the vault did not keep the grant.
pub(crate) async fn seal(
    vault: VaultAt<'_>,
    account: &mut accounts::Account,
    tokens: &TokenSet,
) -> Result<(), String> {
    let grant = account_credentials::oauth_secret(tokens)
        .token
        .ok_or_else(|| "the grant could not be kept".to_owned())?;
    let places = places_of(account)?;
    let username = account
        .config
        .get("email")
        .or_else(|| account.config.get("username"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let reference = vault
        .keep_sign_in(
            account.id.as_str(),
            (ItemKind::Login, places),
            &username,
            Zeroizing::new(grant),
            sign_in_of(&account.config).as_ref(),
        )
        .await?;
    accounts::email::set_password_item(&mut account.config, Some(&reference.to_string()));
    Ok(())
}

/// The access token `account`, kept under `root`, signs in with now for `use_`, for `by`:
/// its grant read from the workspace's vault (the use written down), renewed through the one
/// shared path when it expired, and kept renewed. The renewal holds one lock, so two readers
/// never renew a grant at once.
///
/// A provider that will not renew the grant again (it was revoked, or the refresh token is no
/// good) takes the dead grant out of the vault and the account's settings: the account then
/// lists as one whose person signs in again, in plain words. A provider that was merely not
/// reached keeps the grant, and the refusal says so, because asking again may end.
///
/// # Errors
/// The account names no grant, the vault does not hand it out, or it could not be renewed.
pub(crate) async fn access_token(
    vault: VaultAt<'_>,
    root: &Path,
    account: &accounts::Account,
    use_: OAuthUse,
    by: &str,
) -> Result<String, String> {
    // Renewal is rare and short, and one at a time: a second reader waits
    // here and then finds the fresh token.
    let _renewing = RENEWING.lock().await;
    let reference = sign_in_of(&account.config).ok_or_else(|| SIGN_IN_AGAIN.to_owned())?;
    let places = places_of(account)?;
    let released = vault
        .use_sign_in(&reference, (ItemKind::Login, &places), by, SIGN_IN)
        .await?;
    let secret = accounts::Secret {
        password: String::new(),
        token: Some(released.secret.to_string()),
    };
    let renewing = renewal(root);
    let renewed = tokio::task::spawn_blocking(move || renewing.renew_grant(&secret, use_, false))
        .await
        .map_err(|_| "That account's sign-in was not renewed to the end".to_owned())?;
    match renewed {
        Ok(RenewedGrant {
            tokens: Some(tokens),
            keep,
        }) => {
            if let Some(renewed) = keep.and_then(|keep| keep.token) {
                vault
                    .renew_sign_in(&reference, Zeroizing::new(renewed))
                    .await?;
            }
            Ok(tokens.access_token)
        }
        Ok(RenewedGrant { tokens: None, .. }) | Err(CredentialError::NeedsAuthentication) => {
            forget_dead_grant(vault, root, account).await;
            Err(SIGN_IN_AGAIN.to_owned())
        }
        Err(CredentialError::Offline) => Err(format!(
            "{} could not be reached to renew that account's sign-in. Try again",
            use_.issuer.label()
        )),
        Err(error) => Err(format!(
            "That account's sign-in could not be renewed: {error}"
        )),
    }
}

/// Take a grant the provider will not renew out of the vault and out of `account`'s settings
/// under `root`, so the account lists as one that needs its person to sign in again.
async fn forget_dead_grant(vault: VaultAt<'_>, root: &Path, account: &accounts::Account) {
    if let Some(reference) = sign_in_of(&account.config)
        && let Err(error) = vault.forget_sign_in(account.id.as_str(), &reference).await
    {
        tracing::warn!(%error, "oauth: a grant the provider will not renew stays in the workspace's vault");
    }
    let mut store = accounts::AccountStore::load_in(root);
    let Some(mut kept) = store.get(&account.id).cloned() else {
        return;
    };
    accounts::email::set_password_item(&mut kept.config, None);
    store.upsert(kept);
    if let Err(error) = store.save_in(root) {
        tracing::warn!(%error, "oauth: an account whose grant went still names it");
    }
}

/// The grants being renewed now: one at a time, so two readers never renew
/// one grant at once.
static RENEWING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// What renews a web sign-in's grants: each use of an issuer is renewed with the web client it
/// was signed in with, never with another registration.
fn renewal(root: &Path) -> CredentialStore {
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

/// The issuer a kept account signs in with, from its settings: `provider` for
/// a mail account, `calendar_provider` for the calendar server the same
/// sign-in opened.
#[cfg_attr(
    not(feature = "agents"),
    allow(
        dead_code,
        reason = "the calendar reader that reads a grant back is built with the agents feature"
    )
)]
pub(crate) fn issuer_of_account(root: &Path, account: &str) -> Result<OAuthIssuer, String> {
    let store = accounts::AccountStore::load_in(root);
    let kept = store
        .get(&accounts::AccountId::from(account.to_owned()))
        .ok_or_else(|| "That account is not kept here".to_string())?;
    let provider = kept
        .config
        .get("provider")
        .or_else(|| kept.config.get("calendar_provider"))
        .and_then(serde_json::Value::as_str);
    match provider {
        Some("google" | "google-drive") => Ok(OAuthIssuer::Google),
        Some("microsoft" | "onedrive") => Ok(OAuthIssuer::Microsoft),
        _ => Err("That account's provider is not one this server signs in with".into()),
    }
}
