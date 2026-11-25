//! The cloud-storage accounts a device's mount reads, and the sign-ins it is handed.
//!
//! A mount runs outside any sandbox and keeps no vault: it holds a cloud account's files at
//! `~/Remote` and speaks to the provider over HTTPS. Everything it needs to sign in comes from
//! here, where the workspace's accounts and vault are: each mount is told the accounts it mounts
//! ([`person_accounts::CLOUD_ACCOUNTS`]), and hands back what one signs in with now
//! ([`person_accounts::CLOUD_CREDENTIAL`]): a password, a grant's current access token, or an
//! iCloud session's headers. A grant that was renewed is kept in the workspace's vault here, so
//! the mount never writes a sign-in down.
//!
//! These answers are the person's own: only a plain connection of theirs to their own
//! computer's daemon is answered, never an agent's, an app's or another device's. A credential's
//! answer is a secret ([`tilo_actions::SECRET_ANSWER_TAG`]): the receipt carries it, and nothing
//! is kept of it.

use accounts::vault_items::ItemKind;
use serde_json::Value;
use tilo_actions::remote::person_accounts::{
    self, CloudAccount, CloudAccounts, CloudCredential, CloudCredentialAt, CloudCredentialKind,
    CloudHeader, SyncReport,
};
use tilo_actions::remote::{ActionCallError, InvokeRequest};
use zeroize::Zeroizing;

use super::kept_sign_ins::{self, Signing};
use super::{answer, argument};
use crate::workspaces::Workspaces;

/// What a cloud mount's sign-in is written down as among its item's uses.
const SIGN_IN: &str = "tilo.accounts/sign-in";

/// Answer one of `tilo.accounts`' cloud mount actions, [`request`] as `signing` names it.
///
/// # Errors
/// The caller is not the person on their own connection, or the vault or the account refused.
pub(super) async fn invoke(
    workspaces: &Workspaces,
    signing: Signing<'_>,
    workspace: &str,
    acting: &tilo_action_policy::Acting,
    request: &InvokeRequest,
) -> Result<Value, ActionCallError> {
    match request.target.address.action.as_str() {
        person_accounts::CLOUD_ACCOUNTS => {
            let held = signing
                .vault
                .accounts()
                .await
                .map_err(ActionCallError::refused)?;
            let accounts: Vec<CloudAccount> = held
                .all()
                .iter()
                .filter(|account| {
                    account.enabled && account_credentials::is_cloud_type(account.type_id.as_str())
                })
                .filter_map(|account| {
                    let signed_in = held
                        .sync_of(account.id.as_str())
                        .is_some_and(|sync| sync.signed_in_ms.is_some());
                    Some(CloudAccount {
                        account: serde_json::to_value(account).ok()?,
                        signed_in,
                    })
                })
                .collect();
            answer(&Ok::<_, String>(CloudAccounts { accounts }))
        }
        person_accounts::CLOUD_CREDENTIAL => {
            let asking: CloudCredentialAt = argument(request, "an account to sign in with")?;
            let credential = credential(workspaces, signing, asking)
                .await
                .map_err(ActionCallError::refused)?;
            Ok(
                serde_json::to_value(credential).map_err(|error| ActionCallError::Refused {
                    reason: error.to_string(),
                })?,
            )
        }
        person_accounts::REPORT_SYNC_VERB => {
            let report: SyncReport = argument(request, "how a mount's sync went")?;
            report_sync(workspaces, workspace, acting, report)
                .await
                .map_err(ActionCallError::refused)?;
            Ok(Value::Null)
        }
        _ => Err(ActionCallError::UnknownAction),
    }
}

/// What the account `asking` names signs in with now, from the sign-in the workspace's vault
/// keeps for it: a password as it is, an `OAuth` grant's access token renewed when it was due
/// (or when the mount refused the last one), or an iCloud session's headers. A renewed grant is
/// kept in the vault in its place, as a change to the account's own sign-in.
///
/// # Errors
/// The account is not the workspace's cloud account, or its sign-in is not here or not renewed.
async fn credential(
    workspaces: &Workspaces,
    signing: Signing<'_>,
    asking: CloudCredentialAt,
) -> Result<CloudCredential, String> {
    let held = signing.vault.accounts().await?;
    let account = held
        .account(&asking.account)
        .filter(|account| account.enabled)
        .filter(|account| account_credentials::is_cloud_type(account.type_id.as_str()))
        .ok_or_else(|| "That account is not kept here".to_owned())?;
    let reference = kept_sign_ins::sign_in_of(&account.config)
        .ok_or_else(|| "That account's sign-in is not kept here".to_owned())?;
    let places = kept_sign_ins::places_of(&account)?;
    let released = signing
        .vault
        .use_sign_in(
            &reference,
            (ItemKind::Login, &places),
            signing.by,
            accounts::vault_items::UsedFor {
                using: SIGN_IN,
                what: &format!("{}'s files on this device", account.display_name),
                allowed: None,
            },
        )
        .await?;
    let password = account.type_id.as_str();
    if matches!(
        password,
        accounts::cloud_types::WEBDAV | accounts::cloud_types::NEXTCLOUD
    ) {
        let username = account
            .config
            .get("username")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        return Ok(CloudCredential {
            kind: CloudCredentialKind::Basic,
            username,
            secret: released.secret.to_string(),
            headers: Vec::new(),
            expires_at_ms: None,
        });
    }
    let store = workspaces.cloud_credentials(signing.root);
    let secret = accounts::Secret {
        password: String::new(),
        token: Some(released.secret.to_string()),
    };
    let rejected = asking.rejected;
    let account = account.clone();
    let made =
        tokio::task::spawn_blocking(move || store.drive_credential(&account, &secret, rejected))
            .await
            .map_err(|_| "That account's sign-in was not read to the end".to_owned())?
            .map_err(|error| match error {
                account_credentials::CredentialError::NeedsAuthentication => {
                    "That account's sign-in needs renewing. Sign in again".to_owned()
                }
                account_credentials::CredentialError::Offline => {
                    "That account's provider could not be reached. Try again".to_owned()
                }
                other => format!("That account's sign-in could not be read: {other}"),
            })?;
    if let Some(renewed) = made.renewed {
        signing
            .vault
            .renew_sign_in(
                &reference,
                Zeroizing::new(renewed.token.unwrap_or_default()),
            )
            .await?;
    }
    let Some(made_credential) = made.credential else {
        return Err("That account's sign-in needs renewing. Sign in again".to_owned());
    };
    let expires_at_ms = match &made_credential {
        cloud_source::Credential::Bearer { expires, .. }
        | cloud_source::Credential::Session { expires, .. } => expires.and_then(|time| {
            time.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|since| u64::try_from(since.as_millis()).ok())
        }),
        cloud_source::Credential::Basic { .. } => None,
    };
    let credential = match made_credential {
        cloud_source::Credential::Basic { username, password } => CloudCredential {
            kind: CloudCredentialKind::Basic,
            username,
            secret: password.to_string(),
            headers: Vec::new(),
            expires_at_ms,
        },
        cloud_source::Credential::Bearer { token, .. } => CloudCredential {
            kind: CloudCredentialKind::Bearer,
            username: String::new(),
            secret: token.to_string(),
            headers: Vec::new(),
            expires_at_ms,
        },
        cloud_source::Credential::Session { headers, .. } => CloudCredential {
            kind: CloudCredentialKind::Session,
            username: String::new(),
            secret: String::new(),
            headers: headers
                .into_iter()
                .map(|(name, value)| CloudHeader {
                    name,
                    value: value.to_string(),
                })
                .collect(),
            expires_at_ms,
        },
    };
    Ok(credential)
}

/// Report how the sync of `report`'s account went on this device, as every member's Settings
/// reads it.
///
/// # Errors
/// The account is not the workspace's, its health is no health, or the record does not take the
/// write on this device.
async fn report_sync(
    workspaces: &Workspaces,
    workspace: &str,
    acting: &tilo_action_policy::Acting,
    report: SyncReport,
) -> Result<(), String> {
    let health = accounts::SyncHealth::from_word(&report.health)
        .ok_or_else(|| format!("{} is no account's sync health", report.health))?;
    workspaces
        .report_account_sync(
            (workspace, acting, None),
            &report.account,
            health,
            report.signed_in,
            &report.message,
        )
        .await
}
