//! What a workspace keeps to sign in with, in its vault on the host it runs
//! on: logins, keys, code generators, ssh keys and plain values such as a
//! card number, each bound to where it belongs.
//!
//! The items are entries of the workspace's one vault, Passwords' vault
//! document ([`super::vault`]), which this device holds and opens with the
//! document's own keys, and they are reached as the workspace's policy says
//! at Passwords' vault ([`Store::VAULT`]): a member through their role, an
//! agent or a key through a grant there. A Mail grant opens no vault.
//!
//! A secret travels in and never back. A listing says what is safe to say
//! of an item and no request answers more.
//!
//! The entries themselves are the exception: whoever reads the vault reads
//! them, secrets included, from any device, a browser page as much as a
//! laptop ([`person_vault::ENTRIES`]).
//!
//! What uses an item names it by its reference and carries no secret, so an
//! agent whose grants reach both the vault and what it uses the item for may
//! ask it: adding a mail account whose sign-in the vault keeps, keeping a
//! provider's key from it. The host resolves the reference in the
//! workspace's vault, hands the secret only to the code that signs in, only
//! where the item belongs, and the item remembers the use. The agent is
//! answered what the person would be: the account or the provider, never the
//! secret. Nothing here tells a caller why an argument did not read in the
//! argument's own words, so what was meant as a secret is never echoed in a
//! refusal.
//!
//! An agent on this server, working with no device of its person's on, uses
//! an item through the server's own entry ([`super::hosted_use`]), which asks
//! the person first. Their "Always" is a grant kept by the workspace. A
//! member reads the leaves beside the uses and takes them back here, and no
//! agent does either.

use std::path::Path;

use accounts::vault_items::{self, ItemFacts, NewItem, Place, UsedFor, VaultChange, VaultItems};
use serde_json::Value;
use tilo_action_policy::{ActionKind, Answer, Grant, Level};
use tilo_actions::remote::asked::{AskedFacts, Fact};
use tilo_actions::remote::person_accounts::{self, AddMailAccount, AddMailWithItem, MailAccount};
use tilo_actions::remote::person_models::{self, ProviderView, SetKey, SetKeyFromItem};
use tilo_actions::remote::person_vault::{
    self, AddItem, AlwaysUse, AskForItem, Entries, ForgetEntry, ItemKind, ItemRef, ItemUse,
    ItemUses, ItemView, KeepEntry, KeptItems, RemoveItem, TakeBack, UsesOf,
};
use tilo_actions::remote::{ActionCallError, Caller, InvokeArgument, InvokeRequest};
use tilo_user_identity::DeviceCert;

use super::workspace_stores::Store;
use super::{GovernedWorkspace, WorkspaceId, Workspaces};

impl Workspaces {
    /// The item `workspace`'s vault keeps for what `asked` asked its person for: the newest
    /// of its kind that belongs where it was asked to, which their own add kept.
    ///
    /// # Errors
    /// The vault does not open or read.
    pub(crate) async fn asked_item(
        &self,
        workspace: &str,
        asked: &AskForItem,
    ) -> Result<Option<ItemRef>, String> {
        let opened = self.open_vault(workspace).await?;
        Ok(kept(&opened.items())
            .map_err(|error| error.to_string())?
            .items
            .into_iter()
            .filter(|item| item.kind == asked.kind && item.belongs_to == asked.belongs_to)
            .max_by_key(|item| item.added_at)
            .map(|item| item.reference))
    }

    /// Take back the grants `ids` of `workspace`, written as the host's change to it.
    pub(super) async fn revoke_grants(
        &self,
        workspace: &mut GovernedWorkspace,
        at: std::time::SystemTime,
        ids: &[tilo_action_policy::GrantId],
    ) -> Result<(), String> {
        if !self.governs(workspace) {
            return Err("this server's person does not govern the workspace".to_owned());
        }
        let key = &self.inner.host.key;
        let mut candidate = workspace.peer().clone();
        if candidate
            .governance_mut()
            .remove_governed_grants(ids, key, at)?
        {
            let state = workspace.collaboration().clone();
            workspace.commit(candidate, key, at, |_, _| Ok(state))?;
            self.settle(workspace).await?;
        }
        Ok(())
    }

    /// Answer a [`person_vault::PROVIDER`] request.
    pub(super) async fn person_vault_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let action = request.target.address.action.as_str();
        let effect = match action {
            person_vault::LIST | person_vault::ENTRIES | person_vault::USES => ActionKind::Read,
            _ => ActionKind::Undoable,
        };
        // Who takes back what an agent may always use is a member, never an
        // agent: an agent's call of it is no call of the vault.
        if action == person_vault::TAKE_BACK && caller.acting().is_agent() {
            return Err(ActionCallError::UnknownAction);
        }
        let reach = self
            .reaching_allowed(
                (workspace_id, caller, certificate),
                Store::VAULT,
                action,
                effect,
                request,
            )
            .await?;
        let workspace = reach.workspace.to_string();
        match action {
            person_vault::LIST => {
                let opened = self
                    .open_vault(&workspace)
                    .await
                    .map_err(ActionCallError::refused)?;
                answer(kept(&opened.items()))
            }
            person_vault::ADD => {
                let adding: AddItem = argument(
                    request,
                    "an item to keep names its kind, where it belongs and its secret",
                )?;
                said(
                    self.changing_vault(&workspace, |items| add(items, adding))
                        .await,
                )
            }
            person_vault::REMOVE => {
                let removing: RemoveItem =
                    argument(request, "an item to forget is named by its reference")?;
                said(
                    self.changing_vault(&workspace, |items| remove(items, &removing.item))
                        .await,
                )
            }
            person_vault::ENTRIES => said(self.vault_entries(&workspace).await),
            person_vault::KEEP_ENTRY => {
                let keeping: KeepEntry =
                    argument(request, "an entry to keep is an entry of Passwords")?;
                said(self.keep_entry(&workspace, keeping).await)
            }
            person_vault::FORGET_ENTRY => {
                let forgetting: ForgetEntry =
                    argument(request, "an entry to forget is named by its id")?;
                said(self.forget_entry(&workspace, forgetting).await)
            }
            person_vault::USES => {
                let of: UsesOf = argument(request, "an item's uses are read by its reference")?;
                let workspace_id = workspace_id.ok_or(ActionCallError::UnknownAction)?;
                let cert = certificate.ok_or(ActionCallError::UnknownAction)?;
                let opened = self
                    .open_vault(&workspace)
                    .await
                    .map_err(ActionCallError::refused)?;
                let mut history = uses(&opened.items(), &of.item)
                    .map_err(|err| ActionCallError::refused(err.to_string()))?;
                history.always = self
                    .vault_approvals(workspace_id, cert, &of.item, None)
                    .await?;
                answer(Ok(history))
            }
            person_vault::TAKE_BACK => {
                let taking: TakeBack = argument(
                    request,
                    "what an agent may always use is taken back by the item's reference",
                )?;
                let workspace_id = workspace_id.ok_or(ActionCallError::UnknownAction)?;
                let cert = certificate.ok_or(ActionCallError::UnknownAction)?;
                let opened = self
                    .open_vault(&workspace)
                    .await
                    .map_err(ActionCallError::refused)?;
                let mut history = uses(&opened.items(), &taking.item)
                    .map_err(|err| ActionCallError::refused(err.to_string()))?;
                history.always = self
                    .vault_approvals(workspace_id, cert, &taking.item, Some(&taking))
                    .await?;
                answer(Ok(history))
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }

    /// Every entry of `workspace`'s vault, secrets included.
    async fn vault_entries(&self, workspace: &str) -> Result<Entries, String> {
        let opened = self.open_vault(workspace).await?;
        entries_of(opened.entries().iter().map(|(entry, _)| entry))
    }

    /// Keep `keeping`'s entry, a new one or the edit of one kept, answering every entry then.
    /// What was handed is never echoed in a refusal.
    async fn keep_entry(&self, workspace: &str, keeping: KeepEntry) -> Result<Entries, String> {
        let mut entry: accounts::SecretItem = serde_json::from_value(keeping.entry)
            .map_err(|_| "That is no entry of Passwords".to_owned())?;
        if !accounts::is_entry_id(&entry.id.0) {
            return Err("That is no entry of Passwords".to_owned());
        }
        entry.updated_unix = tilo_clock::unix_now();
        let opened = self.open_vault(workspace).await?;
        self.write_vault(&opened, vec![VaultChange::Keep(entry)])
            .await
            .map_err(|error| format!("The entry could not be kept: {error}"))?;
        self.vault_entries(workspace).await
    }

    /// Forget the entry `forgetting` names, answering every entry left. One already gone is no
    /// refusal: another device may have forgotten it first.
    async fn forget_entry(
        &self,
        workspace: &str,
        forgetting: ForgetEntry,
    ) -> Result<Entries, String> {
        let opened = self.open_vault(workspace).await?;
        if opened.entry(&forgetting.id).is_some() {
            self.write_vault(
                &opened,
                vec![VaultChange::Forget(accounts::SecretId(forgetting.id))],
            )
            .await
            .map_err(|error| format!("The entry could not be forgotten: {error}"))?;
        }
        self.vault_entries(workspace).await
    }

    /// Read a vault item's grants, and revoke those the person chose to take back.
    async fn vault_approvals(
        &self,
        workspace: WorkspaceId,
        device: &DeviceCert,
        item: &ItemRef,
        take_back: Option<&TakeBack>,
    ) -> Result<Vec<AlwaysUse>, ActionCallError> {
        let name = workspace.to_string();
        let workspace = self
            .workspace(&name)
            .await
            .map_err(|err| ActionCallError::refused(err.to_string()))?;
        let mut workspace = workspace.lock().await;
        let at = tilo_clock::wall();
        let access = workspace
            .peer()
            .permissions(device.device_public_key, at)
            .map_err(|_| ActionCallError::NotPermitted)?;
        let person = hex::encode(device.user_public_key);
        if !access.acting.is_person()
            || access.person_id() != Some(person.as_str())
            || access.policy.active_role(&person).is_none()
        {
            return Err(ActionCallError::NotPermitted);
        }
        let mut kept: Vec<_> = access
            .policy
            .grants
            .iter()
            .filter_map(|grant| Some((grant.clone(), vault_approval(grant, item)?)))
            .collect();
        drop(access);
        if let Some(taking) = take_back {
            let mut ids = Vec::new();
            kept.retain(|(grant, shown)| {
                if taking
                    .agent
                    .as_ref()
                    .is_some_and(|agent| *agent != shown.agent)
                {
                    return true;
                }
                ids.push(grant.id.clone());
                false
            });
            if !ids.is_empty() {
                self.revoke_grants(&mut workspace, at, &ids)
                    .await
                    .map_err(ActionCallError::refused)?;
            }
        }
        let grants = kept
            .iter()
            .map(|(grant, _)| grant.clone())
            .collect::<Vec<_>>();
        let times = workspace.peer().governance().governed_grant_times(&grants);
        drop(workspace);
        let mut shown = Vec::with_capacity(kept.len());
        for (grant, mut approval) in kept {
            approval.since = times.get(&grant.id).map_or(0, |at| at / 1_000_000_000);
            approval.by = self
                .agent_name(&name, &approval.agent)
                .await
                .unwrap_or_default();
            shown.push(approval);
        }
        shown.sort_by_key(|approval| approval.since);
        Ok(shown)
    }

    /// Answer a request that uses a vault item by its reference:
    /// [`person_accounts::ADD_MAIL_WITH_ITEM`] or
    /// [`person_models::SET_KEY_FROM_ITEM`].
    pub(super) async fn vault_use_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let target = &request.target;
        let action = target.address.action.as_str();
        let asked = (workspace_id, caller, certificate);
        // The item is used, never read: whoever reaches the vault's items
        // and what the item is used for may.
        let used_for = if target.provider == person_accounts::PROVIDER {
            Store::MAIL
        } else {
            Store::MODELS
        };
        let reach = self
            .reaching_allowed(asked, used_for, action, ActionKind::Undoable, request)
            .await?;
        self.reaching_allowed(asked, Store::VAULT, action, ActionKind::Undoable, request)
            .await?;
        let by = reach.by();
        let workspace = reach.workspace.to_string();
        let root = self.workspace_root(&workspace);
        // What the item is used for is written down with the use. How it
        // was allowed is not the vault's to say here: nothing of it asked.
        let word = person_vault::use_word(target);
        let purpose = UsedFor {
            using: &word,
            ..UsedFor::default()
        };
        match (target.provider.as_str(), target.address.action.as_str()) {
            (person_accounts::PROVIDER, person_accounts::ADD_MAIL_WITH_ITEM) => {
                let adding: AddMailWithItem = argument(
                    request,
                    "a mail account to add names its address, its servers and the vault item that is its password",
                )?;
                let kept = self
                    .mail_with_item((&workspace, &root), adding, &by, purpose)
                    .await;
                if kept.is_ok() {
                    self.person_settings_changed();
                }
                said(kept)
            }
            (person_models::PROVIDER, person_models::SET_KEY_FROM_ITEM) => {
                let setting: SetKeyFromItem = argument(
                    request,
                    "a key to keep names its provider and the vault item that is the key",
                )?;
                said(
                    self.key_from_item((&workspace, &root), &setting, &by, purpose)
                        .await,
                )
            }
            (person_vault::PROVIDER, person_vault::ASK) => {
                let asked: AskForItem = argument(
                    request,
                    "an ask for a secret names its kind, where it belongs and, when known, the \
                     name it signs in with, and never the secret",
                )?;
                let asked = asked.checked().map_err(ActionCallError::refused)?;
                Self::asking_person(caller, certificate, workspace_id)?;
                if request.approval().is_none() {
                    return Err(ActionCallError::asks(secret_asked(caller, &by, &asked)));
                }
                let reference = self.asked_item(&workspace, &asked).await;
                said(reference.map(|reference| person_vault::Asked { reference }))
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }
}

/// What the person `caller` acts for is shown when its agent, known to them as `by`, asks for
/// the secret `asked` names: the system's words, and the vault's own add, filled with what is
/// asked for, which they answer by calling themselves.
pub fn secret_asked(caller: &Caller, by: &str, asked: &AskForItem) -> AskedFacts {
    let mut facts = vec![
        Fact::new(
            "kind",
            serde_json::to_value(asked.kind)
                .ok()
                .and_then(|kind| kind.as_str().map(str::to_owned))
                .unwrap_or_default(),
        ),
        Fact::new("belongsTo", asked.belongs_to.join(" ")),
    ];
    if !asked.username.is_empty() {
        facts.push(Fact::new("username", asked.username.clone()));
    }
    if !asked.sent_in.is_empty() {
        facts.push(Fact::new("sentIn", asked.sent_in.clone()));
    }
    AskedFacts {
        words: asked.words(by),
        facts,
        verb: "keep".to_owned(),
        takes: Some(format!(
            "{}/{}#{}",
            person_vault::PROVIDER,
            person_vault::RESOURCE,
            person_vault::ADD
        )),
        ..AskedFacts::by(super::asking::asked_by(caller))
    }
}

/// What a request's arguments name, or `what` they should have named. The
/// reason never quotes the arguments.
pub(super) fn argument<T: serde::de::DeserializeOwned>(
    request: &InvokeRequest,
    what: &str,
) -> Result<T, ActionCallError> {
    let invalid = || ActionCallError::InvalidRequest {
        reason: what.to_owned(),
    };
    let InvokeArgument::Json(argument) = &request.argument else {
        return Err(invalid());
    };
    serde_json::from_value(Value::Object(argument.clone())).map_err(|_| invalid())
}

/// What the caller reads of an outcome, or the reason it was refused.
fn answer<T: serde::Serialize>(
    outcome: Result<T, vault_items::ItemError>,
) -> Result<Value, ActionCallError> {
    let refused = |reason: String| ActionCallError::Refused { reason };
    let value = outcome.map_err(|error| refused(error.to_string()))?;
    serde_json::to_value(value).map_err(|error| refused(error.to_string()))
}

fn view(facts: ItemFacts) -> Result<ItemView, vault_items::ItemError> {
    Ok(ItemView {
        reference: ItemRef::from_id(facts.id.as_str()).ok_or(vault_items::ItemError::Damaged)?,
        kind: match facts.kind {
            vault_items::ItemKind::Login => ItemKind::Login,
            vault_items::ItemKind::Key => ItemKind::Key,
            vault_items::ItemKind::CodeGenerator => ItemKind::CodeGenerator,
            vault_items::ItemKind::SshKey => ItemKind::SshKey,
            vault_items::ItemKind::Value => ItemKind::Value,
        },
        belongs_to: facts.places.into_iter().map(String::from).collect(),
        title: Some(facts.title).filter(|title| !title.trim().is_empty()),
        added_at: facts.added_unix,
        last_used_at: facts.last_used_unix,
    })
}

/// What is safe to say of every item `items` holds.
fn kept(items: &VaultItems) -> Result<KeptItems, vault_items::ItemError> {
    Ok(KeptItems {
        items: items
            .facts()
            .into_iter()
            .map(view)
            .collect::<Result<_, _>>()?,
    })
}

fn add(items: &mut VaultItems, adding: AddItem) -> Result<ItemView, vault_items::ItemError> {
    let AddItem {
        kind,
        belongs_to,
        username,
        sent_in,
        secret,
    } = adding;
    let places = belongs_to
        .iter()
        .map(|place| Place::parse(place))
        .collect::<Result<Vec<_>, _>>()?;
    let kind = match kind {
        ItemKind::Login => vault_items::ItemKind::Login,
        ItemKind::Key => vault_items::ItemKind::Key,
        ItemKind::CodeGenerator => vault_items::ItemKind::CodeGenerator,
        ItemKind::SshKey => vault_items::ItemKind::SshKey,
        ItemKind::Value => vault_items::ItemKind::Value,
    };
    let item = NewItem::new(kind, places, username, secret);
    let kept = if sent_in.trim().is_empty() {
        items.add(item)?
    } else {
        // Only a key says where it rides in a request to its service.
        items.add_key(item, &vault_items::KeyIn::parse(&sent_in)?)?
    };
    view(kept)
}

fn remove(items: &mut VaultItems, item: &ItemRef) -> Result<KeptItems, vault_items::ItemError> {
    let id = vault_items::ItemId::parse(item.id()).ok_or(vault_items::ItemError::NotFound)?;
    if !items.remove(&id) {
        return Err(vault_items::ItemError::NotFound);
    }
    kept(items)
}

/// `entries` as Passwords reads them, secrets included.
fn entries_of<'a>(
    entries: impl Iterator<Item = &'a accounts::SecretItem>,
) -> Result<Entries, String> {
    let mut listed: Vec<&accounts::SecretItem> = entries.collect();
    listed.sort_by(|a, b| {
        a.title
            .to_lowercase()
            .cmp(&b.title.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    let entries = listed
        .into_iter()
        .map(serde_json::to_value)
        .collect::<Result<_, _>>()
        .map_err(|_| "An entry of Passwords could not be read".to_owned())?;
    Ok(Entries { entries })
}

/// What the caller reads of a use's outcome, or the reason it was refused.
fn said<T: serde::Serialize>(outcome: Result<T, String>) -> Result<Value, ActionCallError> {
    let refused = |reason: String| ActionCallError::Refused { reason };
    serde_json::to_value(outcome.map_err(refused)?).map_err(|error| refused(error.to_string()))
}

/// What `grant` lets an agent always use of the workspace's vault item
/// `item`, if it is such a grant.
fn vault_approval(grant: &Grant, item: &ItemRef) -> Option<AlwaysUse> {
    if grant.answer != Answer::Allow {
        return None;
    }
    let agent = grant.grantee.agent_id()?;
    let (app, action) = (
        grant.node.id_at(Level::App)?,
        grant.node.id_at(Level::Action)?,
    );
    if !matches!(
        (app, action),
        (
            person_accounts::PROVIDER,
            person_accounts::ADD_MAIL_WITH_ITEM
        ) | (person_models::PROVIDER, person_models::SET_KEY_FROM_ITEM)
            | (person_vault::PROVIDER, person_vault::FILL_PAGE)
    ) {
        return None;
    }
    let (named, places) = grant.node.id_at(Level::Resource)?.split_once('/')?;
    if named != item.id() {
        return None;
    }
    Some(AlwaysUse {
        agent: agent.to_owned(),
        by: String::new(),
        using: grant.node.id_at(Level::Kind)?.to_owned(),
        places: serde_json::from_str(places).ok()?,
        since: 0,
    })
}

/// When and where the item `item` names was handed to something that signs
/// in, for whom and what for; and who may use it without its person being
/// asked.
fn uses(items: &VaultItems, item: &ItemRef) -> Result<ItemUses, vault_items::ItemError> {
    let id = vault_items::ItemId::parse(item.id()).ok_or(vault_items::ItemError::NotFound)?;
    Ok(ItemUses {
        always: Vec::new(),
        uses: items
            .uses(&id)?
            .into_iter()
            .map(|used| ItemUse {
                at: used.at_unix,
                places: used.places.into_iter().map(String::from).collect(),
                by: used.by,
                using: used.using,
                what: used.what,
                allowed: used
                    .allowed
                    .map_or_else(String::new, |allowed| allowed.word().to_owned()),
            })
            .collect(),
    })
}

impl Workspaces {
    /// Hand the item `item` of `workspace`'s vault names to the code that signs in at every one
    /// of `places` with a `kind`, for `by`, with what it is used for written down with the use:
    /// the one way a reference becomes its secret, and only inside this device. The use is
    /// written to the vault before the secret is answered.
    ///
    /// # Errors
    /// The vault does not open here, does not hold the item, the item is not a `kind` that
    /// belongs to every one of `places`, or the use was not written down.
    pub(super) async fn release_item(
        &self,
        workspace: &str,
        item: &ItemRef,
        (kind, places): (vault_items::ItemKind, &[Place]),
        by: &str,
        purpose: UsedFor<'_>,
    ) -> Result<vault_items::Released, String> {
        let id = vault_items::ItemId::parse(item.id())
            .ok_or_else(|| vault_items::ItemError::NotFound.to_string())?;
        self.changing_vault(workspace, |items| {
            items.use_for(&id, kind, places, by, purpose)
        })
        .await
    }

    /// Add `adding` to `workspace`'s mail accounts, kept under `root`, signing in with the vault
    /// item it names, once its server accepted it. The item must be a login that belongs to both
    /// servers the account names: a server it does not belong to is never sent it. The account
    /// names the item from then on, and nothing copies its password.
    pub(super) async fn mail_with_item(
        &self,
        (workspace, root): (&str, &Path),
        adding: AddMailWithItem,
        by: &str,
        purpose: UsedFor<'_>,
    ) -> Result<MailAccount, String> {
        let server = |scheme: &str, host: &str, port: u16| {
            Place::server(scheme, host, port).map_err(|error| error.to_string())
        };
        let places = [
            server("imap", &adding.incoming_host, adding.incoming_port)?,
            server("smtp", &adding.outgoing_host, adding.outgoing_port)?,
        ];
        let mut sign_in = self
            .release_item(
                workspace,
                &adding.password,
                (vault_items::ItemKind::Login, &places),
                by,
                purpose,
            )
            .await?;
        // The account signs in under the name the request gives, else the
        // item's own, else its address.
        let username = adding
            .username
            .filter(|username| !username.trim().is_empty())
            .or_else(|| {
                Some(std::mem::take(&mut sign_in.username)).filter(|name| !name.is_empty())
            });
        let kept = adding.password.clone();
        Box::pin(super::person_settings::add_mail_with(
            self.account_tester(),
            (self.vault_at(workspace), root),
            AddMailAccount {
                address: adding.address,
                display_name: adding.display_name,
                username,
                incoming_host: adding.incoming_host,
                incoming_port: adding.incoming_port,
                outgoing_host: adding.outgoing_host,
                outgoing_port: adding.outgoing_port,
                password: sign_in.secret.to_string(),
                identities: Vec::new(),
                services: person_accounts::Services::default(),
            },
            Some(kept),
            person_accounts::Provider::Other,
        ))
        .await
    }

    /// Name the key the vault item `setting` names as its provider's API key. The item must be
    /// a key that belongs to the provider's endpoint, as the person set it: a key is never sent
    /// to an endpoint it does not belong to. Where the provider names a key already, the item
    /// takes its place only once the provider took it ([`super::person_models::take_key`]); the
    /// provider names the item from then on, and nothing copies the key.
    pub(super) async fn key_from_item(
        &self,
        (workspace, root): (&str, &Path),
        setting: &SetKeyFromItem,
        by: &str,
        purpose: UsedFor<'_>,
    ) -> Result<ProviderView, String> {
        let endpoint = {
            let config = super::person_settings::provider_config(root);
            let profile = config
                .profiles
                .iter()
                .find(|profile| profile.id == setting.provider)
                .ok_or_else(|| "That provider is not kept here".to_owned())?;
            Place::parse(&profile.endpoint).map_err(|error| error.to_string())?
        };
        let key = self
            .release_item(
                workspace,
                &setting.key,
                (vault_items::ItemKind::Key, std::slice::from_ref(&endpoint)),
                by,
                purpose,
            )
            .await?;
        let keeping = SetKey {
            provider: setting.provider.clone(),
            key: key.secret.to_string(),
        };
        let keys = super::person_settings::ProviderKeys::of(self.vault_at(workspace), by);
        super::person_models::take_key(
            (keys, root),
            keeping,
            Some(&setting.key),
            &self.model_reach(),
        )
        .await
    }
}
