//! The sign-ins of a workspace's accounts and providers, as entries of its one vault.
//!
//! A mail, calendar or cloud account that signs in with a password or with the grant a
//! provider's own page left names the entry of the workspace's vault that holds it, by its
//! reference ([`accounts::email::PASSWORD_ITEM_KEY`] in the account's settings); a model
//! provider names its key or grant the same way ([`agent_provider_config::SecretReference`]). The
//! entry is all there is of the secret on this host. Whoever signs in (reading mail, sending it,
//! watching an inbox, pulling a calendar, an agent's turn calling its model) resolves the
//! reference at that moment, at the places the account names now, and the use is written into
//! the entry's uses before the secret is handed on ([`VaultAt::changing`]).
//!
//! An entry this host made for an account (a password typed for it, a grant its provider's page
//! left, a key) names the account it is kept for, and goes when the account goes. An account
//! added with an entry a member keeps for themselves only names it: that entry stays theirs.

use accounts::vault_items::{ItemError, ItemId, ItemKind, NewItem, Place, Released, UsedFor};
use tilo_actions::remote::person_vault::ItemRef;
use zeroize::Zeroizing;

use super::super::vault::VaultAt;

/// What an account that names no sign-in is told when something needs one.
pub(in crate::workspaces) const NO_SIGN_IN: &str = "That account has no sign-in yet";

/// What an account whose entry is gone from the workspace's vault is told.
const GONE: &str = "That account's sign-in is no longer in the workspace's vault. Sign in again";

/// What an account whose entry belongs to other servers than the ones its settings name now is
/// told: the secret is never sent where it was not kept for.
const ELSEWHERE: &str =
    "That account's sign-in belongs to other servers than the ones it names now. Sign in again";

/// The entry an account's settings `config` name as their sign-in.
pub(in crate::workspaces) fn sign_in_of(config: &serde_json::Value) -> Option<ItemRef> {
    accounts::email::password_item(config).and_then(ItemRef::parse)
}

/// Whether `account` names a sign-in to use: what a listing says as whether one is kept.
pub(in crate::workspaces) fn has_sign_in(account: &accounts::Account) -> bool {
    sign_in_of(&account.config).is_some()
}

/// The places `account`'s settings send its sign-in to: a mail account's incoming and outgoing
/// server, each by name and port, and the origin of a calendar's, a cloud server's or a `WebDAV`
/// server's address.
///
/// # Errors
/// The settings name no server, or one that is no place a sign-in can belong to.
pub(in crate::workspaces) fn places_of(account: &accounts::Account) -> Result<Vec<Place>, String> {
    let servers = accounts::typed_for::servers_of(account);
    let places = servers
        .iter()
        .map(|server| Place::parse(server))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "That account names a server its sign-in cannot belong to".to_owned())?;
    if places.is_empty() {
        return Err("That account names no server to sign in to".to_owned());
    }
    Ok(places)
}

/// The id the reference names.
pub(in crate::workspaces) fn id_of(reference: &ItemRef) -> Result<ItemId, String> {
    ItemId::parse(reference.id()).ok_or_else(|| GONE.to_owned())
}

/// The reference of the entry `id` names.
fn reference_of(id: &ItemId) -> Result<ItemRef, String> {
    ItemRef::from_id(id.as_str()).ok_or_else(|| "the vault made an entry with no id".to_owned())
}

/// Words for an account's sign-in that the vault did not hand out.
pub(in crate::workspaces) fn refused(error: &ItemError) -> String {
    match error {
        ItemError::NotFound => GONE.to_owned(),
        ItemError::WrongPlace => ELSEWHERE.to_owned(),
        other => other.to_string(),
    }
}

/// The owner a model provider's key or grant is kept for, beside the accounts' ids.
pub(in crate::workspaces) fn provider_owner(provider: &str) -> String {
    format!("model-provider:{provider}")
}

impl VaultAt<'_> {
    /// Keep `secret` as the own sign-in of `owner` (an account's id, or [`provider_owner`]), a
    /// `kind` that belongs to `places` and signs in as `username`, answering its reference.
    /// Where `replacing` names an entry kept for `owner` before, the secret takes its place under
    /// the same reference; a member's own entry it names stays theirs.
    ///
    /// # Errors
    /// The vault is not open here, or the secret is no item to keep.
    pub(in crate::workspaces) async fn keep_sign_in(
        &self,
        owner: &str,
        (kind, places): (ItemKind, Vec<Place>),
        username: &str,
        secret: Zeroizing<String>,
        replacing: Option<&ItemRef>,
    ) -> Result<ItemRef, String> {
        let replacing = replacing.and_then(|reference| ItemId::parse(reference.id()));
        let item = NewItem {
            kind,
            places,
            username: username.to_owned(),
            secret,
        };
        let kept = self
            .changing(|items| items.keep_for(owner, item, replacing.as_ref()))
            .await?;
        reference_of(&kept.id)
    }

    /// Hand the sign-in `reference` names to the code that signs in as a `kind` at every one of
    /// `places`, for `by` (empty for a person or this host itself), with what it is used for
    /// written down with the use first.
    ///
    /// # Errors
    /// The vault is not open here, the entry is gone, or it belongs elsewhere: in words for the
    /// account's person.
    pub(in crate::workspaces) async fn use_sign_in(
        &self,
        reference: &ItemRef,
        (kind, places): (ItemKind, &[Place]),
        by: &str,
        purpose: UsedFor<'_>,
    ) -> Result<Released, String> {
        let id = id_of(reference)?;
        let mut refusal = None;
        let released = self
            .changing(|items| {
                items
                    .use_for(&id, kind, places, by, purpose)
                    .inspect_err(|error| refusal = Some(refused(error)))
            })
            .await;
        released.map_err(|error| refusal.unwrap_or(error))
    }

    /// [`Self::use_sign_in`], answering none where the entry is gone or belongs elsewhere than
    /// `places`: a key never sent where it was not kept for is a key not kept there.
    ///
    /// # Errors
    /// The vault is not open here, or the use was not written down.
    pub(in crate::workspaces) async fn try_sign_in(
        &self,
        reference: &ItemRef,
        (kind, places): (ItemKind, &[Place]),
        by: &str,
        purpose: UsedFor<'_>,
    ) -> Result<Option<Released>, String> {
        let Ok(id) = id_of(reference) else {
            return Ok(None);
        };
        let used = self
            .changing(
                |items| match items.use_for(&id, kind, places, by, purpose) {
                    Ok(released) => Ok(Some(released)),
                    Err(ItemError::NotFound | ItemError::WrongPlace) => Ok(None),
                    Err(error) => Err(error),
                },
            )
            .await?;
        Ok(used)
    }

    /// A mark of what the entry `reference` names holds now, which changes whenever its secret
    /// or where it belongs does and says nothing of either: `None` where it is gone. Nothing is
    /// handed out and no use is written.
    ///
    /// # Errors
    /// The vault is not open here.
    pub(in crate::workspaces) async fn sign_in_mark(
        &self,
        reference: &ItemRef,
    ) -> Result<Option<[u8; 32]>, String> {
        let Ok(id) = id_of(reference) else {
            return Ok(None);
        };
        self.changing(|items| {
            Ok(items
                .kept()
                .into_iter()
                .find(|(entry, _)| entry.id.0 == id.as_str())
                .map(|(entry, _)| {
                    let mut hasher = blake3::Hasher::new();
                    hasher.update(entry.password.as_bytes());
                    for origin in &entry.origins {
                        hasher.update(b"\n");
                        hasher.update(origin.as_bytes());
                    }
                    *hasher.finalize().as_bytes()
                }))
        })
        .await
    }

    /// Put `secret` in place of what the entry `reference` names holds: a grant renewed.
    ///
    /// # Errors
    /// The vault is not open here, the entry is gone, or the renewed grant was not written.
    pub(in crate::workspaces) async fn renew_sign_in(
        &self,
        reference: &ItemRef,
        secret: Zeroizing<String>,
    ) -> Result<(), String> {
        let id = id_of(reference)?;
        self.changing(|items| items.renew(&id, &secret)).await
    }

    /// Forget the entry `reference` names where it is `owner`'s own sign-in: an account that
    /// goes takes its own sign-in with it, and leaves a member's entry it signed in with.
    ///
    /// # Errors
    /// The vault is not open here, or what was forgotten was not written.
    pub(in crate::workspaces) async fn forget_sign_in(
        &self,
        owner: &str,
        reference: &ItemRef,
    ) -> Result<(), String> {
        let Ok(id) = id_of(reference) else {
            return Ok(());
        };
        self.changing(|items| Ok(items.forget_for(&id, owner)))
            .await
            .map(drop)
    }
}

/// What is used to sign in to an account's servers with: a mail sign-in, a calendar's.
pub(in crate::workspaces) const SIGN_IN: UsedFor<'static> = UsedFor {
    using: "sign-in",
    what: "",
    allowed: None,
};

/// Where a workspace's accounts sign in from: the store they are kept in, the vault their
/// sign-ins are entries of, and who signs in with them (empty for a person or this host itself,
/// else the agent's name), which each use writes down.
#[derive(Clone, Copy, Debug)]
pub(in crate::workspaces) struct Signing<'a> {
    pub(in crate::workspaces) vault: VaultAt<'a>,
    pub(in crate::workspaces) root: &'a std::path::Path,
    pub(in crate::workspaces) by: &'a str,
}

#[cfg(feature = "device-keys")]
/// One sign-in the host sealed in its own store under a workspace's folder, before the
/// workspace's vault was the one place a secret is kept.
struct SealedBefore {
    /// Whose it is: an account's id, or a provider's ([`provider_owner`]).
    owner: Owner,
    kind: ItemKind,
    places: Vec<Place>,
    username: String,
    secret: Zeroizing<String>,
}

#[cfg(feature = "device-keys")]
/// What a sign-in the host kept before is for.
#[derive(Clone)]
pub(in crate::workspaces) enum Owner {
    Account(accounts::AccountId),
    Provider(String),
}

#[cfg(feature = "device-keys")]
impl Owner {
    fn id(&self) -> String {
        match self {
            Self::Account(account) => account.to_string(),
            Self::Provider(provider) => provider_owner(provider),
        }
    }
}

#[cfg(feature = "device-keys")]
/// The sign-ins the host kept in its own sealed store under a workspace's folder: each mail,
/// calendar and cloud account's password or grant, and each model provider's key or grant.
/// They are carried into the workspace's vault as their owners' own entries, and then the
/// store goes ([`Self::carry`]).
pub(in crate::workspaces) struct HostSignIns {
    root: std::path::PathBuf,
    /// The workspace's accounts whose sign-in the host kept, as they were read.
    accounts: Vec<accounts::Account>,
    kept: Vec<SealedBefore>,
}

#[cfg(feature = "device-keys")]
impl HostSignIns {
    /// What the host kept under `root` for the workspace's `accounts`: none where it keeps no
    /// store of its own there.
    ///
    /// # Errors
    /// The store is there and does not open: it stays, and nothing is carried.
    pub(in crate::workspaces) fn under(
        root: &std::path::Path,
        accounts: &[accounts::Account],
    ) -> Result<Option<Self>, String> {
        if !accounts::vault_dir_in(root).exists() {
            return Ok(None);
        }
        let credentials = accounts::AccountCredentials::open_in(root)?;
        let mut kept = Vec::new();
        let mut signed_in = Vec::new();
        for account in accounts {
            if sign_in_of(&account.config).is_some() {
                continue;
            }
            // A sign-in typed for other servers than its account names now is never sent there,
            // and one bound to none yet belongs to the servers it names.
            let secret = credentials.get_for(account).or_else(|| {
                credentials
                    .typed_for(&account.id)
                    .is_none()
                    .then(|| credentials.get(&account.id))
                    .flatten()
            });
            let Some(secret) = secret.filter(|secret| !secret.is_empty()) else {
                continue;
            };
            let places = match places_of(account) {
                Ok(places) => places,
                Err(error) => {
                    tracing::warn!(account = %account.id, %error, "a sign-in the host kept for an account with no server to sign in to is not carried into the workspace's vault");
                    continue;
                }
            };
            let username = ["email", "username"]
                .iter()
                .find_map(|field| {
                    account
                        .config
                        .get(*field)
                        .and_then(serde_json::Value::as_str)
                })
                .unwrap_or_default()
                .to_owned();
            let secret = secret
                .token
                .clone()
                .unwrap_or_else(|| secret.password.clone());
            signed_in.push(account.clone());
            kept.push(SealedBefore {
                owner: Owner::Account(account.id.clone()),
                kind: ItemKind::Login,
                places,
                username,
                secret: Zeroizing::new(secret),
            });
        }
        let keys = accounts::ServiceCredentials::open_in(root);
        for profile in super::provider_config(root).profiles {
            if super::key_kept(&profile) {
                continue;
            }
            let Ok((reference, authorization)) =
                accounts::provider_api_key(&profile.id, &profile.endpoint)
            else {
                continue;
            };
            let Ok(Some(key)) = keys.resolve(&reference, &authorization) else {
                continue;
            };
            let Ok(place) = super::key_place(&profile) else {
                continue;
            };
            if key.is_empty() {
                continue;
            }
            kept.push(SealedBefore {
                owner: Owner::Provider(profile.id.clone()),
                kind: ItemKind::Key,
                places: vec![place],
                username: String::new(),
                secret: key,
            });
        }
        Ok(Some(Self {
            root: root.to_owned(),
            accounts: signed_in,
            kept,
        }))
    }

    /// Keep every sign-in as its owner's own entry in `items`, answering the entry of each.
    ///
    /// # Errors
    /// An entry was refused.
    pub(in crate::workspaces) fn keep_in(
        &self,
        items: &mut accounts::vault_items::VaultItems,
    ) -> Result<Vec<(Owner, ItemRef)>, String> {
        self.kept
            .iter()
            .map(|sealed| {
                let item = NewItem {
                    kind: sealed.kind,
                    places: sealed.places.clone(),
                    username: sealed.username.clone(),
                    secret: sealed.secret.clone(),
                };
                let kept = items
                    .keep_for(&sealed.owner.id(), item, None)
                    .map_err(|error| error.to_string())?;
                Ok((sealed.owner.clone(), reference_of(&kept.id)?))
            })
            .collect()
    }

    /// Name each entry `kept` in its owner's settings, the workspace's accounts in
    /// `workspace`, then take the host's own store away: the workspace's vault keeps every
    /// sign-in now.
    ///
    /// # Errors
    /// The settings or the store were not written: the store stays, so nothing is lost.
    pub(in crate::workspaces) async fn carry(
        self,
        (workspaces, workspace): (&crate::workspaces::Workspaces, &str),
        kept: &[(Owner, ItemRef)],
    ) -> Result<(), String> {
        let root = &self.root;
        let mut config = super::provider_config(root);
        let mut providers_named = false;
        for (owner, reference) in kept {
            match owner {
                Owner::Account(id) => {
                    let Some(mut account) = self
                        .accounts
                        .iter()
                        .find(|account| account.id == *id)
                        .cloned()
                    else {
                        continue;
                    };
                    accounts::email::set_password_item(
                        &mut account.config,
                        Some(&reference.to_string()),
                    );
                    workspaces
                        .keep_workspace_account(workspace, &account)
                        .await?;
                }
                Owner::Provider(id) => {
                    if let Some(profile) =
                        config.profiles.iter_mut().find(|profile| profile.id == *id)
                    {
                        super::name_key(profile, Some(reference));
                        providers_named = true;
                    }
                }
            }
        }
        if providers_named {
            super::save_provider_config(root, &config)?;
        }
        std::fs::remove_dir_all(accounts::vault_dir_in(root)).map_err(|error| error.to_string())
    }

    /// Whether the host kept no sign-in here.
    pub(in crate::workspaces) fn is_empty(&self) -> bool {
        self.kept.is_empty()
    }
}
