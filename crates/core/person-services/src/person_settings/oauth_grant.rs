//! One provider's grant, kept in the workspace's vault.
//!
//! A provider that signs a person in on its own page leaves a grant: the
//! access token mail and calendar read with, the token that renews it, when
//! it expires and the scopes it was given. It is kept as an entry of the
//! workspace's vault in the shared `oauth_secret` shape, the account's own
//! sign-in, and a device signing in is released only its access token
//! ([`signs_in_with`]), renewed through the one shared renewal
//! ([`account_credentials::CredentialStore::renew_grant`]) behind a lock so
//! two readers of one grant never renew it at once.
//!
//! Renewal needs the registration the grant was given to: the client a device
//! signs in with, from the committed client table ([`renewal`]). A mail or calendar
//! account's grant is renewed for what its provider's sign-in opened, and the workspace's
//! Hugging Face account's for the model manager's downloads.

use std::path::Path;

use account_credentials::{CredentialError, CredentialStore, OAuthIssuer, OAuthUse, RenewedGrant};
use tilo_workspace_records::vault::ItemRef;
use zeroize::Zeroizing;

use crate::VaultAt;
use crate::person_settings::kept_sign_ins::sign_in_of;

/// What an account whose provider will not renew its grant is told, in the words the model
/// manager knows an ended Hugging Face sign-in by.
const SIGN_IN_AGAIN: &str = "That account's sign-in needs renewing. Sign in again";

/// The access token the grant `grant` that `account` names at `reference` holds, renewed
/// through the one shared path when it expired, and kept renewed. Must be called holding
/// [`RENEWING`].
async fn renewed(
    vault: VaultAt<'_>,
    root: &Path,
    account: &accounts::Account,
    (reference, grant): (&ItemRef, Zeroizing<String>),
    use_: OAuthUse,
) -> Result<String, String> {
    let secret = accounts::Secret {
        password: String::new(),
        token: Some(grant.to_string()),
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
                    .renew_sign_in(reference, Zeroizing::new(renewed))
                    .await?;
            }
            Ok(tokens.access_token)
        }
        Ok(RenewedGrant { tokens: None, .. }) | Err(CredentialError::NeedsAuthentication) => {
            forget_dead_grant(vault, account).await;
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

/// What a device signs in to `account` with, from the sign-in `released` from its entry of the
/// workspace's vault: a password as it is, and a grant its provider's own page left as the access
/// token it holds now, renewed when it expired. A grant's refresh token never leaves this
/// service: whoever signs in is handed only what reads mail or a calendar for the next hour.
///
/// # Errors
/// The account names no provider this service renews grants of, or the grant was not renewed.
pub(crate) async fn signs_in_with(
    vault: VaultAt<'_>,
    root: &Path,
    account: &accounts::Account,
    released: accounts::vault_items::Released,
) -> Result<accounts::vault_items::Released, String> {
    let grant = accounts::Secret {
        password: String::new(),
        token: Some(released.secret.to_string()),
    };
    if account_credentials::oauth_tokens(&grant).is_none() {
        return Ok(released);
    }
    let reference = sign_in_of(&account.config).ok_or_else(|| SIGN_IN_AGAIN.to_owned())?;
    let use_ = use_of(account)?;
    let _renewing = RENEWING.lock().await;
    let token = renewed(vault, root, account, (&reference, released.secret), use_).await?;
    Ok(accounts::vault_items::Released {
        kind: released.kind,
        username: released.username,
        secret: Zeroizing::new(token),
    })
}

/// Take a grant the provider will not renew out of the workspace's vault, so the account asks
/// its person to sign in again rather than handing out a grant that no longer works.
async fn forget_dead_grant(vault: VaultAt<'_>, account: &accounts::Account) {
    if let Some(reference) = sign_in_of(&account.config)
        && let Err(error) = vault.forget_sign_in(account.id.as_str(), &reference).await
    {
        tracing::warn!(%error, "oauth: a grant the provider will not renew stays in the workspace's vault");
    }
}

/// The grants being renewed now: one at a time, so two readers never renew
/// one grant at once.
static RENEWING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// What renews a grant: the client a device signed in with, from the committed client table, so
/// each grant is renewed with the registration it was given to.
fn renewal(root: &Path) -> CredentialStore {
    CredentialStore::for_root(Some(root.to_path_buf()))
}

/// What `account`'s grant is renewed for: the mail or calendar its provider's sign-in opened.
fn use_of(account: &accounts::Account) -> Result<OAuthUse, String> {
    let issuer = issuer_of(account)?;
    Ok(if account.type_id.as_str() == accounts::email::TYPE_ID {
        OAuthUse::mail(issuer)
    } else {
        OAuthUse::calendar(issuer)
    })
}

/// The issuer `account` signs in with, from its settings: `provider` for a mail account,
/// `calendar_provider` for the calendar server the same sign-in opened.
fn issuer_of(account: &accounts::Account) -> Result<OAuthIssuer, String> {
    let provider = account
        .config
        .get("provider")
        .or_else(|| account.config.get("calendar_provider"))
        .and_then(serde_json::Value::as_str);
    match provider {
        Some("google" | "google-drive") => Ok(OAuthIssuer::Google),
        Some("microsoft" | "onedrive") => Ok(OAuthIssuer::Microsoft),
        _ => Err("That account's provider is not one this server signs in with".into()),
    }
}
