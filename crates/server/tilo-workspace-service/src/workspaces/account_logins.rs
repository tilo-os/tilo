//! A workspace's accounts sign in with entries of its one vault.
//!
//! An account of the workspace's accounts record (`tilo.accounts`) keeps no secret: its settings
//! name the vault entry its password is kept in ([`accounts::email::password_item`]), and every
//! member's device that may use Passwords signs in with that one entry, read from its own copy
//! of the vault document ([`super::vault`]). Nothing here asks another device, and no password
//! leaves for another device: a member who may not read the vault reads and sends the
//! workspace's mail through a device that may ([`tilo_actions::remote::person_accounts`]).
//!
//! Each call is decided once, before anything is read, for the device it comes from
//! ([`Workspaces::using_vault`]). A password leaves the vault only to sign in at
//! the places its entry names, and each release is written down in the entry's uses first.

use accounts::person::PersonAccounts;
use accounts::vault_items::{ItemId, ItemKind, NewItem, Place, Released, UsedFor};
use tilo_actions::remote::Caller;
use tilo_actions::remote::person_vault::ItemRef;
use tilo_peer_state::workspace_documents::DocumentRead;
use tilo_resource::{Replica, Resource as _};
use zeroize::Zeroizing;

use super::Workspaces;
use super::person_settings::kept_sign_ins;

/// What a release of an account's password is written down as.
const SIGN_IN: &str = "tilo.accounts/sign-in";

/// The most bytes of the accounts record one read takes.
const MAX_ACCOUNTS_BYTES: usize = 8 * 1024 * 1024;

/// The words for somebody whose role does not let them use Passwords.
pub const MAY_NOT_USE_PASSWORDS: &str = "Your role in this workspace does not include Passwords, \
                                         so this account's password stays in the workspace's \
                                         vault. Ask an owner of the workspace to let you use \
                                         Passwords.";

/// What a release is refused with where the workspace's rules ask its person first: no password
/// leaves the vault on a question, and the person's mail is read and sent where the vault is,
/// asked as every action the rules ask about is ([`tilo_actions::remote::person_accounts`]).
pub const ASKS_FIRST: &str = "The workspace asks you first before this account's password is \
                              used, so it stays in the workspace's vault and this device does not \
                              sign in with it.";

/// The words for somebody who may not read the app asking to sign in.
pub const MAY_NOT_READ_APP: &str = "Your role in this workspace does not let you use this app, so \
                                    it signs in to none of the workspace's accounts. Ask an owner \
                                    of the workspace to let you use it.";

/// The words for a device the workspace does not count as one of its members' now.
pub const NOT_A_MEMBER_DEVICE: &str = "This device is not one of this workspace's now, so it signs \
                                       in to none of the workspace's accounts.";

/// The words for a device that is not the person's own.
pub const NOT_ON_A_FOREIGN_DEVICE: &str = "This device is not one of your own, so no account's \
                                           password is handed to it. Sign in from your own \
                                           device.";

/// The words for a device that cannot open the workspace's vault now: its workspaces are still
/// opening, or the vault's keys have not reached it yet.
pub const VAULT_NOT_HERE: &str = "This device cannot open the workspace's vault yet, so no \
                                  account's password is available. Try again in a moment.";

/// The words for an entry of the vault that no enabled account of the workspace signs in with.
pub const NOT_NAMED: &str = "No account of this workspace signs in with that entry of its vault";

/// A login an account signs in with, as the vault keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountLogin {
    /// What Passwords shows the entry as: the account's address.
    pub title: String,
    /// The name it signs in with.
    pub username: String,
    /// The servers it signs in at, and nowhere else.
    pub places: Vec<Place>,
}

/// The route the workspace's accounts record travels on in `workspace`.
fn accounts_route(workspace: &str) -> tilo_peer_state::document_frame::DocumentRoute {
    tilo_peer_state::document_frame::DocumentRoute {
        app_id: accounts::person::ACCOUNTS_OWNER.to_owned(),
        cluster_id: workspace.to_owned(),
        document: accounts_document(),
    }
}

/// The accounts record's document id: one per workspace, the same on every device, kept by
/// [`accounts::person::ACCOUNTS_OWNER`]. A follower of the workspace's record changes waits on it
/// for the workspace's accounts.
#[must_use]
pub fn accounts_document() -> String {
    collab_types::workspace_document_id(PersonAccounts::KIND)
}

impl Workspaces {
    /// That `by` may use the workspace's vault and, signing `app` in, read `app` there: the one
    /// decision every route to an account's password takes. The device `by` asks from must be
    /// a member's now, with its role and approved keys, bounded by `by`'s acting (an agent's
    /// approval and its turn's rules); a password is released only to the person's own device.
    /// Where the rules ask first, the person's own yes (`confirmed`, never an agent's) lets it.
    pub(crate) async fn using_vault(
        &self,
        workspace: &str,
        by: &Caller,
        app: Option<&str>,
        confirmed: bool,
    ) -> Result<(), String> {
        use tilo_action_policy::{ActionKind, Answer, Context};
        let now = tilo_clock::wall();
        let device = by.device_public_key().ok_or(NOT_A_MEMBER_DEVICE)?;
        let state = self
            .workspace(workspace)
            .await
            .map_err(|_| NOT_A_MEMBER_DEVICE.to_owned())?;
        let access = state
            .lock()
            .await
            .peer()
            .permissions(*device, now)
            .map_err(|_| NOT_A_MEMBER_DEVICE.to_owned())?;
        if app.is_some() && access.acting.foreign_device {
            return Err(NOT_ON_A_FOREIGN_DEVICE.to_owned());
        }
        let certified = by.certified(&access.policy.workspace);
        let access = access.invoked_by(by.acting()).granted(certified);
        let asked = confirmed && by.agent_called().is_none();
        let reads = |node: &tilo_action_policy::TreePath, refused: &str| match access.answer(
            node,
            ActionKind::Read,
            &Context::at_time(now),
        ) {
            Answer::Allow => Ok(()),
            Answer::Ask if asked => Ok(()),
            Answer::Ask => Err(ASKS_FIRST.to_owned()),
            Answer::Deny => {
                let person = access.person_id().unwrap_or_default();
                tracing::warn!(
                    %workspace,
                    person,
                    role = ?access.policy.role_of(person),
                    members = access.policy.members.len(),
                    %node,
                    "the workspace's rules on this device refused its person"
                );
                Err(refused.to_owned())
            }
        };
        reads(
            &collab_types::document_node(
                workspace,
                super::vault::APP,
                tilo_action_policy::roles::WORKSPACE_VAULT_DOCUMENT,
            ),
            MAY_NOT_USE_PASSWORDS,
        )?;
        if let Some(app) = app {
            reads(
                &tilo_action_policy::TreePath::workspace(workspace).app(app),
                MAY_NOT_READ_APP,
            )?;
        }
        Ok(())
    }

    /// Keep `password` as `login` in `workspace`'s vault, for `by`: in the entry `item` names
    /// where the vault keeps it (a password typed anew), in a new entry otherwise. Answers the
    /// entry's reference, which the account names.
    ///
    /// # Errors
    /// `by` may not use the vault, the login is not one to keep, or the vault does not open or
    /// take the write on this device.
    pub async fn keep_account_password(
        &self,
        workspace: &str,
        by: &Caller,
        item: Option<&ItemRef>,
        login: AccountLogin,
        password: Zeroizing<String>,
    ) -> Result<ItemRef, String> {
        // The person typing the password is the yes a rule that asks first waits for.
        self.using_vault(workspace, by, None, true).await?;
        let id = item.and_then(|item| ItemId::parse(item.id()));
        let AccountLogin {
            title,
            username,
            places,
        } = login;
        let kept = self
            .changing_vault(workspace, |items| {
                items.keep(
                    id.as_ref(),
                    NewItem {
                        kind: ItemKind::Login,
                        places,
                        username,
                        secret: password,
                    },
                    &title,
                )
            })
            .await?;
        ItemRef::from_id(kept.id.as_str()).ok_or_else(|| "the vault named no entry".to_owned())
    }

    /// The secret of the vault entry `item` names, for `app` to sign in with on `by`'s behalf:
    /// the sign-in of the enabled account of `workspace`'s accounts that names that entry
    /// ([`accounts::email::password_item`]), released only for the servers that account names
    /// now, with the use written into the entry first, saying which app signed in to which
    /// account. Any kind of account and any app: what decides is whether `by` may use
    /// Passwords. A grant a provider's own page left is released as its access token alone.
    /// Where the workspace's rules ask first, nothing is released ([`ASKS_FIRST`]).
    ///
    /// # Errors
    /// `by` may not use the vault, no enabled account names the entry, the account names no
    /// server its sign-in belongs to, or the entry is gone or kept for other servers.
    pub async fn account_secret(
        &self,
        workspace: &str,
        by: &Caller,
        app: &str,
        item: &ItemRef,
    ) -> Result<Released, String> {
        self.using_vault(workspace, by, Some(app), false).await?;
        let held = self.workspace_accounts(workspace).await?;
        let store = held.store();
        let account = store
            .all()
            .iter()
            .find(|account| {
                account.enabled && kept_sign_ins::sign_in_of(&account.config).as_ref() == Some(item)
            })
            .ok_or(NOT_NAMED)?;
        let places = kept_sign_ins::places_of(account)?;
        let id = kept_sign_ins::id_of(item)?;
        // An agent is named; a person's own use is theirs, as every other release writes it.
        let who = by
            .agent_called()
            .map_or_else(String::new, ToOwned::to_owned);
        let name = if account.display_name.trim().is_empty() {
            account.id.as_str()
        } else {
            account.display_name.as_str()
        };
        let what = format!("{app} signed in to {name}");
        let mut refusal = None;
        let released = self
            .changing_vault(workspace, |items| {
                items
                    .use_as_kept(
                        &id,
                        &places,
                        &who,
                        UsedFor {
                            using: SIGN_IN,
                            what: &what,
                            allowed: None,
                        },
                    )
                    .inspect_err(|error| refusal = Some(kept_sign_ins::refused(error)))
            })
            .await
            .map_err(|error| refusal.unwrap_or(error))?;
        // A grant a provider's page left is handed out as the access token it holds now, never
        // with the refresh token that renews it.
        super::person_settings::oauth_grant::signs_in_with(
            self.vault_at(workspace),
            &self.workspace_root(workspace),
            account,
            released,
        )
        .await
    }

    /// Whether `workspace`'s vault keeps the entry `item` names, on this device.
    ///
    /// # Errors
    /// The vault does not open on this device.
    pub async fn vault_keeps(&self, workspace: &str, item: &ItemRef) -> Result<bool, String> {
        Ok(self.open_vault(workspace).await?.entry(item.id()).is_some())
    }

    /// `workspace`'s accounts as this device holds them: none where nobody kept one yet.
    ///
    /// # Errors
    /// This device holds no such workspace, may not read the record, or it does not read.
    pub async fn workspace_accounts(&self, workspace: &str) -> Result<PersonAccounts, String> {
        let Some(bytes) = self.accounts_document(workspace).await? else {
            return Ok(PersonAccounts::default());
        };
        Replica::<PersonAccounts>::load(&bytes)
            .and_then(|replica| replica.state())
            .map_err(|error| error.to_string())
    }

    async fn accounts_document(&self, workspace: &str) -> Result<Option<Vec<u8>>, String> {
        let document = accounts_document();
        if !self.holds_resource(workspace, accounts::person::ACCOUNTS_OWNER, &document) {
            return Ok(None);
        }
        let read = self
            .own_read(
                workspace,
                (accounts::person::ACCOUNTS_OWNER, &document),
                &[],
                MAX_ACCOUNTS_BYTES,
            )
            .await?;
        match read {
            DocumentRead::Whole { bytes, .. } | DocumentRead::Changes { bytes, .. } => {
                Ok(Some(bytes))
            }
            DocumentRead::Absent => Ok(None),
            DocumentRead::Pending => Err(
                "The workspace's accounts are still arriving on this device; try again in a moment"
                    .to_owned(),
            ),
            DocumentRead::Unchanged(_) => Err("the accounts record did not read".to_owned()),
        }
    }

    /// The accounts of every workspace this device holds that `keep` keeps, each with its
    /// workspace, in the order the workspaces and their records list them. A workspace whose
    /// accounts do not read here yet lists none.
    pub async fn held_accounts(
        &self,
        keep: impl Fn(&accounts::Account) -> bool,
    ) -> Vec<(String, accounts::Account)> {
        let mut held = Vec::new();
        let mut workspaces = self.held_workspaces();
        workspaces.sort();
        for workspace in workspaces {
            let Ok(accounts) = self.workspace_accounts(&workspace).await else {
                continue;
            };
            held.extend(
                accounts
                    .store()
                    .all()
                    .iter()
                    .filter(|account| keep(account))
                    .map(|account| (workspace.clone(), account.clone())),
            );
        }
        held
    }

    /// Name `item` as where `account` of `workspace`'s accounts keeps its password, as this
    /// device's own write of the accounts record. The rest of the account stays as it is.
    ///
    /// What Settings does once the session tells it the entry, which tests do here.
    ///
    /// # Errors
    /// The record does not hold the account, or does not take the write on this device.
    #[cfg(all(test, any(feature = "agents", feature = "apps")))]
    pub(crate) async fn name_account_password(
        &self,
        workspace: &str,
        account: &str,
        item: &ItemRef,
    ) -> Result<(), String> {
        self.change_workspace_accounts(workspace, |held| {
            let mut kept = held
                .account(account)
                .ok_or("the workspace's accounts hold no such account")?;
            let before = kept.config.clone();
            accounts::email::set_password_item(&mut kept.config, Some(&item.to_string()));
            Ok((
                (kept.config != before)
                    .then(|| PersonAccounts::saving(&kept))
                    .into_iter()
                    .collect(),
                (),
            ))
        })
        .await
    }

    /// Keep `account` in `workspace`'s accounts: added, or in place of the one with its id.
    ///
    /// # Errors
    /// The record does not take the write on this device.
    pub async fn keep_workspace_account(
        &self,
        workspace: &str,
        account: &accounts::Account,
    ) -> Result<(), String> {
        self.change_workspace_accounts(workspace, |held| {
            let edit = (held.account(account.id.as_str()).as_ref() != Some(account))
                .then(|| PersonAccounts::saving(account));
            Ok((edit.into_iter().collect(), ()))
        })
        .await
    }

    /// Remove the account `id` from `workspace`'s accounts. Whether it was there.
    ///
    /// # Errors
    /// The record does not take the write on this device.
    pub async fn remove_workspace_account(
        &self,
        workspace: &str,
        id: &str,
    ) -> Result<bool, String> {
        self.change_workspace_accounts(workspace, |held| {
            let kept = held.account(id).is_some();
            let edit = kept.then(|| {
                PersonAccounts::REMOVE_ACCOUNT.with(tilo_resource::Id::from_raw(id.to_owned()))
            });
            Ok((edit.into_iter().collect(), kept))
        })
        .await
    }

    /// Read `workspace`'s accounts, let `change` say the edits it makes of them and what it
    /// answers, and write those edits as this device's own write of the record, made where
    /// nobody made it yet. Each edit claims Settings' verb, which every member decides against
    /// Settings' surface.
    ///
    /// # Errors
    /// `change` refused, the record does not read, or it does not take the write on this
    /// device.
    pub(crate) async fn change_workspace_accounts<T>(
        &self,
        workspace: &str,
        change: impl FnOnce(
            &PersonAccounts,
        ) -> Result<(Vec<tilo_resource::Invoke<PersonAccounts>>, T), String>,
    ) -> Result<T, String> {
        let bytes = self.accounts_document(workspace).await?;
        let founding = bytes.is_none();
        let (changes, answer) = {
            let mut replica = match &bytes {
                Some(bytes) => {
                    Replica::<PersonAccounts>::load(bytes).map_err(|error| error.to_string())?
                }
                None => Replica::from_doc(founding_accounts(workspace)?),
            }
            .in_app(accounts::person::ACCOUNTS_OWNER, "Settings");
            let state = replica.state().map_err(|error| error.to_string())?;
            let (edits, answer) = change(&state)?;
            if edits.is_empty() {
                return Ok(answer);
            }
            replica.doc_mut().set_actor(automerge::ActorId::from(
                self.signing_device().device().to_vec(),
            ));
            let heads = if founding {
                Vec::new()
            } else {
                replica.heads()
            };
            for edit in edits {
                replica
                    .apply_confirmed(edit)
                    .map_err(|error| error.to_string())?;
            }
            let changes = if founding {
                replica.save()
            } else {
                replica.doc_mut().save_after(&heads)
            };
            (changes, answer)
        };
        let keys = match self
            .own_route_keys(&accounts_route(workspace), founding)
            .await
        {
            Some(Ok(keys)) => Some(keys),
            Some(Err(reason)) => return Err(reason),
            None => None,
        };
        self.own_write(
            workspace,
            (accounts::person::ACCOUNTS_OWNER, &accounts_document()),
            None,
            &changes,
            founding,
            founding,
            keys.as_ref(),
        )
        .await
        .map_err(|refusal| refusal.to_string())?;
        Ok(answer)
    }
}

/// The accounts record's genesis in `workspace`: the same change Settings makes it with on any
/// device, so a record this device makes and one a window made at the same time are one
/// document.
#[cfg(any(feature = "agents", feature = "apps"))]
fn founding_accounts(workspace: &str) -> Result<automerge::AutoCommit, String> {
    let id = collab_types::document::DocumentId::parse(&accounts_document())
        .ok_or("the accounts record's id is no document's")?;
    let kind = collab_types::document::ResourceType::new(PersonAccounts::KIND);
    let mut doc = automerge::AutoCommit::new();
    collaboration::resource::genesis(
        &mut doc,
        &collab_types::document::DocumentCheckout::new(id, kind),
        workspace,
        |doc| PersonAccounts::initialize(doc).map_err(|error| error.to_string()),
    )
    .map_err(|error| error.to_string())?;
    Ok(doc)
}

/// Where this service is built without the resources' genesis, the accounts nobody kept yet are
/// made by the first member's Settings.
#[cfg(not(any(feature = "agents", feature = "apps")))]
fn founding_accounts(_workspace: &str) -> Result<automerge::AutoCommit, String> {
    Err("The workspace's accounts are made by Settings on a member's device first".to_owned())
}
