//! What a workspace keeps to sign in with, in its vault on the host it runs
//! on: logins, keys, code generators, ssh keys and plain values such as a
//! card number, each bound to where it belongs.
//!
//! The items are sealed in the vault that already keeps the workspace's mail
//! sign-ins and provider keys ([`super::person_settings`]), under the
//! workspace's own key, and they are reached as the workspace's policy says
//! at Passwords' vault ([`Store::VAULT`]): a member through their role, an
//! agent or a key through a grant there. A Mail grant opens no vault.
//!
//! A secret travels in and never back. A listing says what is safe to say
//! of an item and no request answers more.
//!
//! The entries of the workspace's Passwords are kept in the same vault, and
//! they are the exception: whoever reads the vault reads them, secrets
//! included, from any device, a browser page as much as a laptop
//! ([`person_vault::ENTRIES`]).
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
use std::sync::Arc;

use accounts::vault_items::{self, ItemFacts, NewItem, Place, UsedFor, VaultItems};
use serde_json::Value;
use tilo_account_probe::AccountTester;
use tilo_action_policy::{ActionKind, Answer, Grant, Level};
use tilo_actions::remote::person_accounts::{self, AddMailAccount, AddMailWithItem, MailAccount};
use tilo_actions::remote::person_models::{self, ProviderView, SetKey, SetKeyFromItem};
use tilo_actions::remote::person_vault::{
    self, AddItem, AlwaysUse, Entries, ForgetEntry, ItemKind, ItemRef, ItemUse, ItemUses, ItemView,
    KeepEntry, KeptItems, RemoveItem, TakeBack, UsesOf,
};
use tilo_actions::remote::{ActionCallError, Caller, InvokeArgument, InvokeRequest};
use tilo_user_identity::DeviceCert;

use super::person_asking::{ApprovalChange, is_scoped_approval};
use super::workspace_stores::Store;
use super::{WorkspaceId, Workspaces};

/// What the vault is asked to keep where a person is asked for a login's password: where it
/// belongs and the name it signs in with. It holds no secret.
#[derive(Clone, Debug)]
pub struct LoginAsked {
    pub belongs_to: Vec<String>,
    pub username: String,
}

impl Workspaces {
    /// Ask `person`, on their own devices, for the password `question` words, and keep what
    /// they type in `workspace`'s vault as a login `asked` names: the reference of the item it
    /// is kept as, `None` where they declined. The password goes from their device's protected
    /// field to this host and into the vault; only the reference comes back.
    ///
    /// # Errors
    /// No device of theirs could be asked, nobody answered in time, or the vault did not keep
    /// what they typed.
    pub async fn ask_for_login(
        &self,
        (person, workspace): ([u8; 32], &str),
        question: tilo_server_protocol::certify::PendingConfirmation,
        asked: LoginAsked,
    ) -> Result<Option<ItemRef>, String> {
        use tilo_server_protocol::certify::ConfirmationAnswer;
        if !question.ask.as_ref().is_some_and(|ask| ask.secret) {
            return Err("only a question for a secret is answered into the vault".to_owned());
        }
        let ConfirmationAnswer::Secret(typed) =
            self.ask_person_confirmation(person, question).await?
        else {
            return Ok(None);
        };
        let root = self.workspace_root(workspace);
        let adding = AddItem {
            kind: ItemKind::Login,
            belongs_to: asked.belongs_to,
            username: asked.username,
            sent_in: String::new(),
            secret: typed.expose().to_owned(),
        };
        let kept = tokio::task::spawn_blocking(move || add(&root, adding))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        Ok(Some(kept.reference))
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
            )
            .await?;
        let root = self.workspace_root(&reach.workspace.to_string());
        match action {
            person_vault::LIST => answer(kept(&root)),
            person_vault::ADD => {
                let adding: AddItem = argument(
                    request,
                    "an item to keep names its kind, where it belongs and its secret",
                )?;
                answer(add(&root, adding))
            }
            person_vault::REMOVE => {
                let removing: RemoveItem =
                    argument(request, "an item to forget is named by its reference")?;
                answer(remove(&root, &removing.item))
            }
            person_vault::ENTRIES => said(entries(&root)),
            person_vault::KEEP_ENTRY => {
                let keeping: KeepEntry =
                    argument(request, "an entry to keep is an entry of Passwords")?;
                said(keep_entry(&root, keeping))
            }
            person_vault::FORGET_ENTRY => {
                let forgetting: ForgetEntry =
                    argument(request, "an entry to forget is named by its id")?;
                said(forget_entry(&root, forgetting))
            }
            person_vault::USES => {
                let of: UsesOf = argument(request, "an item's uses are read by its reference")?;
                let workspace = workspace_id.ok_or(ActionCallError::UnknownAction)?;
                let cert = certificate.ok_or(ActionCallError::UnknownAction)?;
                let mut history = uses(&root, &of.item).map_err(|err| refused(err.to_string()))?;
                history.always = self
                    .vault_approvals(workspace, cert, &of.item, None)
                    .await?;
                answer(Ok(history))
            }
            person_vault::TAKE_BACK => {
                let taking: TakeBack = argument(
                    request,
                    "what an agent may always use is taken back by the item's reference",
                )?;
                let workspace = workspace_id.ok_or(ActionCallError::UnknownAction)?;
                let cert = certificate.ok_or(ActionCallError::UnknownAction)?;
                let mut history =
                    uses(&root, &taking.item).map_err(|err| refused(err.to_string()))?;
                history.always = self
                    .vault_approvals(workspace, cert, &taking.item, Some(&taking))
                    .await?;
                answer(Ok(history))
            }
            _ => Err(ActionCallError::UnknownAction),
        }
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
            .map_err(|err| refused(err.to_string()))?;
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
                self.change_approval(&mut workspace, at, ApprovalChange::Revoke(&ids))
                    .await
                    .map_err(refused)?;
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
            .reaching_allowed(asked, used_for, action, ActionKind::Undoable)
            .await?;
        self.reaching_allowed(asked, Store::VAULT, action, ActionKind::Undoable)
            .await?;
        let by = reach.by();
        let root = self.workspace_root(&reach.workspace.to_string());
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
                let kept = mail_with_item(self.account_tester(), &root, adding, &by, purpose).await;
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
                said(key_from_item(&root, &setting, &by, purpose, &self.model_reach()).await)
            }
            _ => Err(ActionCallError::UnknownAction),
        }
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
        added_at: facts.added_unix,
        last_used_at: facts.last_used_unix,
    })
}

/// What is safe to say of every item the vault under `root` keeps.
fn kept(root: &Path) -> Result<KeptItems, vault_items::ItemError> {
    Ok(KeptItems {
        items: VaultItems::open_in(root)?
            .facts()
            .into_iter()
            .map(view)
            .collect::<Result<_, _>>()?,
    })
}

fn add(root: &Path, adding: AddItem) -> Result<ItemView, vault_items::ItemError> {
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
    let mut items = VaultItems::open_in(root)?;
    let kept = if sent_in.trim().is_empty() {
        items.add(item)?
    } else {
        // Only a key says where it rides in a request to its service.
        items.add_key(item, &vault_items::KeyIn::parse(&sent_in)?)?
    };
    view(kept)
}

fn remove(root: &Path, item: &ItemRef) -> Result<KeptItems, vault_items::ItemError> {
    let id = vault_items::ItemId::parse(item.id()).ok_or(vault_items::ItemError::NotFound)?;
    let mut items = VaultItems::open_in(root)?;
    if !items.remove(&id)? {
        return Err(vault_items::ItemError::NotFound);
    }
    kept(root)
}

/// The workspace's Passwords, in the vault under `root`, refusing a vault
/// whose key is gone rather than answering it empty.
fn passwords(root: &Path) -> Result<accounts::SecretStore, String> {
    let store = accounts::SecretStore::open_in(root);
    match store.recovery_error() {
        Some(error) => Err(format!("Your vault could not be opened: {error}")),
        None => Ok(store),
    }
}

/// Every entry of the workspace's Passwords, secrets included.
fn entries(root: &Path) -> Result<Entries, String> {
    let entries = passwords(root)?
        .list()
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<_, _>>()
        .map_err(|_| "An entry of Passwords could not be read".to_owned())?;
    Ok(Entries { entries })
}

/// Keep `keeping`'s entry, a new one or the edit of one kept, answering
/// every entry then. What was handed is never echoed in a refusal.
fn keep_entry(root: &Path, keeping: KeepEntry) -> Result<Entries, String> {
    let mut entry: accounts::SecretItem = serde_json::from_value(keeping.entry)
        .map_err(|_| "That is no entry of Passwords".to_owned())?;
    passwords(root)?
        .put(&mut entry)
        .map_err(|error| format!("The entry could not be kept: {error}"))?;
    entries(root)
}

/// Forget the entry `forgetting` names, answering every entry left. One
/// already gone is no refusal: another device may have
/// forgotten it first.
fn forget_entry(root: &Path, forgetting: ForgetEntry) -> Result<Entries, String> {
    passwords(root)?
        .remove(&accounts::SecretId(forgetting.id))
        .map_err(|error| format!("The entry could not be forgotten: {error}"))?;
    entries(root)
}

/// What the caller reads of a use's outcome, or the reason it was refused.
fn said<T: serde::Serialize>(outcome: Result<T, String>) -> Result<Value, ActionCallError> {
    let refused = |reason: String| ActionCallError::Refused { reason };
    serde_json::to_value(outcome.map_err(refused)?).map_err(|error| refused(error.to_string()))
}

fn refused(reason: String) -> ActionCallError {
    ActionCallError::Refused { reason }
}

/// What `grant` lets an agent always use of the workspace's vault item
/// `item`, if it is such a grant.
fn vault_approval(grant: &Grant, item: &ItemRef) -> Option<AlwaysUse> {
    if !is_scoped_approval(grant) || grant.answer != Answer::Allow {
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
fn uses(root: &Path, item: &ItemRef) -> Result<ItemUses, vault_items::ItemError> {
    let id = vault_items::ItemId::parse(item.id()).ok_or(vault_items::ItemError::NotFound)?;
    let items = VaultItems::open_in(root)?;
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

/// Hand the item `item` names to the code that signs in at every one of
/// `places` with a `kind`, for `by`: the one way a reference becomes its
/// secret, and only inside this host.
///
/// Built where its one caller is: a person's calendars, which need the
/// feature `agents`.
#[cfg(feature = "agents")]
pub(super) fn released(
    root: &Path,
    item: &ItemRef,
    kind: vault_items::ItemKind,
    places: &[Place],
    by: &str,
) -> Result<vault_items::Released, String> {
    released_for(root, item, kind, places, by, UsedFor::default())
}

/// [`released`], with what the item is used for written down with the use:
/// the use's word, and how its person allowed it where the vault asked.
fn released_for(
    root: &Path,
    item: &ItemRef,
    kind: vault_items::ItemKind,
    places: &[Place],
    by: &str,
    purpose: UsedFor<'_>,
) -> Result<vault_items::Released, String> {
    let id = vault_items::ItemId::parse(item.id()).ok_or(vault_items::ItemError::NotFound);
    id.and_then(|id| VaultItems::open_in(root)?.use_for(&id, kind, places, by, purpose))
        .map_err(|error| error.to_string())
}

/// Add `adding` with the sign-in its vault item keeps, once `tester` saw its
/// server accept it. The item must be a login that belongs to both servers
/// the account names: a server it does not belong to is never sent it.
pub(super) async fn mail_with_item(
    tester: Arc<dyn AccountTester>,
    root: &Path,
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
    let mut sign_in = released_for(
        root,
        &adding.password,
        vault_items::ItemKind::Login,
        &places,
        by,
        purpose,
    )?;
    // The account signs in under the name the request gives, else the
    // item's own, else its address.
    let username = adding
        .username
        .filter(|username| !username.trim().is_empty())
        .or_else(|| Some(std::mem::take(&mut sign_in.username)).filter(|name| !name.is_empty()));
    super::person_settings::add_mail_with(
        tester,
        root,
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
    )
    .await
}

/// Keep the key the vault item `setting` names as its provider's API key.
/// The item must be a key that belongs to the provider's endpoint, as the
/// person set it: a key is never kept for an endpoint it does not belong to.
/// Where the provider keeps a key already, the item's key takes its place
/// only once the provider took it ([`super::person_models::take_key`]).
pub(super) async fn key_from_item(
    root: &Path,
    setting: &SetKeyFromItem,
    by: &str,
    purpose: UsedFor<'_>,
    reach: &super::person_models::Reach,
) -> Result<ProviderView, String> {
    let keeping = {
        let config = super::person_settings::provider_config(root);
        let profile = config
            .profiles
            .iter()
            .find(|profile| profile.id == setting.provider)
            .ok_or_else(|| "That provider is not kept here".to_owned())?;
        let endpoint = Place::parse(&profile.endpoint).map_err(|error| error.to_string())?;
        let key = released_for(
            root,
            &setting.key,
            vault_items::ItemKind::Key,
            std::slice::from_ref(&endpoint),
            by,
            purpose,
        )?;
        SetKey {
            provider: setting.provider.clone(),
            key: key.secret.to_string(),
        }
    };
    super::person_models::take_key(root, keeping, reach).await
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use std::sync::Mutex;

    use super::*;

    /// A tester that accepts every sign-in, and keeps what it was asked.
    #[derive(Default)]
    struct Accepting {
        asked: Mutex<Vec<String>>,
    }

    impl AccountTester for Accepting {
        fn test(
            &self,
            account: &accounts::Account,
            secret: &accounts::Secret,
        ) -> Result<String, String> {
            self.asked
                .lock()
                .unwrap()
                .push(format!("{} with {}", account.id, secret.password));
            Ok("signed in".into())
        }
    }

    /// A mail account is added with the sign-in a vault item keeps: the
    /// server sees the password, the account signs in under the item's own
    /// name, the item remembers the use, and a server the item does not
    /// belong to is never sent the password.
    #[test]
    fn a_mail_account_is_added_with_a_sign_in_the_vault_keeps() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let both = vec![
            Place::server("imap", "imap.example.test", 993).unwrap(),
            Place::server("smtp", "smtp.example.test", 465).unwrap(),
        ];
        let kept = VaultItems::open_in(root)
            .unwrap()
            .add(NewItem::new(
                vault_items::ItemKind::Login,
                both.clone(),
                "ada".into(),
                "mail-password".into(),
            ))
            .unwrap();
        let password = ItemRef::from_id(kept.id.as_str()).unwrap();
        let adding = |incoming: &str| AddMailWithItem {
            address: "ada@example.test".into(),
            display_name: "Ada".into(),
            username: None,
            incoming_host: incoming.into(),
            incoming_port: 993,
            outgoing_host: "smtp.example.test".into(),
            outgoing_port: 465,
            password: password.clone(),
            reason: String::new(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let tester = Arc::new(Accepting::default());

        let refused = runtime
            .block_on(mail_with_item(
                tester.clone(),
                root,
                adding("imap.attacker.example"),
                "Helper",
                UsedFor::default(),
            ))
            .unwrap_err();
        assert_eq!(refused, "That item belongs somewhere else");
        assert!(tester.asked.lock().unwrap().is_empty());
        let mail = accounts::AccountTypeId::from(accounts::email::TYPE_ID);
        assert!(accounts::AccountStore::load_in(root).list(&mail).is_empty());
        assert!(
            VaultItems::open_in(root)
                .unwrap()
                .uses(&kept.id)
                .unwrap()
                .is_empty()
        );

        let account = runtime
            .block_on(mail_with_item(
                tester.clone(),
                root,
                adding("IMAP.example.test"),
                "Helper",
                UsedFor::default(),
            ))
            .unwrap();
        assert!(account.has_secret);
        assert_eq!(account.address, "ada@example.test");
        assert_eq!(
            tester.asked.lock().unwrap().clone(),
            vec!["ada@example.test with mail-password".to_owned()],
            "the sign-in is proved with the password the item keeps"
        );
        let said = serde_json::to_string(&account).unwrap();
        assert!(!said.contains("mail-password"), "{said}");
        let store = accounts::AccountStore::load_in(root);
        let stored = store
            .get(&accounts::AccountId::from("ada@example.test"))
            .expect("the account is kept");
        let config = accounts::email::AccountConfig::from_value(stored.config.clone()).unwrap();
        assert_eq!(config.username, "ada");
        let uses = VaultItems::open_in(root).unwrap().uses(&kept.id).unwrap();
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].places, both);
        assert_eq!(uses[0].by, "Helper");
    }
}
