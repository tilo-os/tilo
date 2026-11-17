//! What a person keeps to sign in with, in their own vault on the host
//! their workspace runs on: logins, keys, code generators, ssh keys and
//! plain values such as a card number, each bound to where it belongs.
//!
//! The items are sealed in the vault that already keeps the person's mail
//! sign-ins and provider keys ([`super::person_settings`]), and they are
//! reached as those are: only by the person themselves, on a device of
//! theirs whose certificate names them. Another member reaches only their
//! own vault, and an agent's request is refused, whoever it acts for.
//!
//! A secret travels in and never back. A listing says what is safe to say
//! of an item and no request answers more.
//!
//! What uses an item names it by its reference and carries no secret, so an
//! agent acting for the person may ask it ([`Workspaces::asking_for_person`]):
//! adding a mail account whose sign-in the vault keeps, keeping a provider's
//! key from it. The host resolves the reference in the person's own vault,
//! hands the secret only to the code that signs in, only where the item
//! belongs, and the item remembers the use. The agent is answered what the
//! person would be: the account or the provider, never the secret. Nothing here tells a caller why
//! an argument did not read in the argument's own words, so what was meant
//! as a secret is never echoed in a refusal.
//!
//! An agent this host runs itself, with no device of the person's on, uses
//! an item through the host's own entry ([`super::hosted_use`]), which asks
//! the person first. Their "Always" is a standing leave kept with the item.
//! The person reads the leaves beside the uses and takes them back here,
//! and no agent does either.

use std::path::Path;
use std::sync::Arc;

use accounts::vault_items::{self, ItemFacts, NewItem, Place, VaultItems};
use serde_json::Value;
use tilo_account_probe::AccountTester;
use tilo_actions::remote::person_accounts::{self, AddMailAccount, AddMailWithItem, MailAccount};
use tilo_actions::remote::person_models::{self, ProviderView, SetKey, SetKeyFromItem};
use tilo_actions::remote::person_vault::{
    self, AddItem, ItemKind, ItemRef, ItemUse, ItemUses, ItemView, KeptItems, RemoveItem,
    StandingUse, TakeBack, UsesOf,
};
use tilo_actions::remote::{ActionHostError, Caller, InvokeArgument, InvokeRequest};
use tilo_user_identity::DeviceCert;

use super::Workspaces;

impl Workspaces {
    /// Answer a [`person_vault::PROVIDER`] request.
    pub(super) fn person_vault_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionHostError> {
        let person = Self::asking_person(caller, certificate, workspace_id)?;
        let root = self.person_settings_root(&person);
        match request.target.address.action.as_str() {
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
            person_vault::USES => {
                let of: UsesOf = argument(request, "an item's uses are read by its reference")?;
                answer(uses(&root, &of.item))
            }
            person_vault::TAKE_BACK => {
                let taking: TakeBack = argument(
                    request,
                    "what an agent may always use is taken back by the item's reference",
                )?;
                answer(take_back(&root, &taking))
            }
            _ => Err(ActionHostError::UnknownAction),
        }
    }

    /// The person a request that uses what their vault keeps is for, and
    /// whom the use is remembered for: the person themselves on a device of
    /// theirs (nobody is named), or an agent acting for them, by the name
    /// they know it under. The device is the one `certificate` certifies for
    /// that person, which the transport verified. A bot, an app and an agent
    /// no such certificate stands behind are told what a caller asking for
    /// something that does not exist is told.
    pub(super) fn asking_for_person(
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        workspace_id: Option<collab_types::WorkspaceId>,
    ) -> Result<([u8; 32], String), ActionHostError> {
        workspace_id.ok_or(ActionHostError::UnknownAction)?;
        let acting = caller.acting();
        if acting.app().is_some() || !(acting.is_person() || acting.is_agent()) {
            return Err(ActionHostError::UnknownAction);
        }
        let certificate = certificate
            .filter(|certificate| {
                caller.device_public_key() == Some(&certificate.device_public_key)
                    && caller.user_public_key() == Some(&certificate.user_public_key)
            })
            .ok_or(ActionHostError::UnknownAction)?;
        let by = if acting.is_agent() {
            caller
                .agent_name()
                .or_else(|| acting.agent_id())
                .unwrap_or_default()
                .to_owned()
        } else {
            String::new()
        };
        Ok((certificate.user_public_key, by))
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
    ) -> Result<Value, ActionHostError> {
        let (person, by) = Self::asking_for_person(caller, certificate, workspace_id)?;
        let root = self.person_settings_root(&person);
        let target = &request.target;
        match (target.provider.as_str(), target.address.action.as_str()) {
            (person_accounts::PROVIDER, person_accounts::ADD_MAIL_WITH_ITEM) => {
                let adding: AddMailWithItem = argument(
                    request,
                    "a mail account to add names its address, its servers and the vault item that is its password",
                )?;
                let kept = mail_with_item(self.account_tester(), &root, adding, &by).await;
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
                said(key_from_item(&root, &setting, &by))
            }
            _ => Err(ActionHostError::UnknownAction),
        }
    }
}

/// What a request's arguments name, or `what` they should have named. The
/// reason never quotes the arguments.
pub(super) fn argument<T: serde::de::DeserializeOwned>(
    request: &InvokeRequest,
    what: &str,
) -> Result<T, ActionHostError> {
    let invalid = || ActionHostError::InvalidRequest {
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
) -> Result<Value, ActionHostError> {
    let refused = |reason: String| ActionHostError::Refused { reason };
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
    let kept = VaultItems::open_in(root)?.add(NewItem::new(kind, places, username, secret))?;
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

/// What the caller reads of a use's outcome, or the reason it was refused.
fn said<T: serde::Serialize>(outcome: Result<T, String>) -> Result<Value, ActionHostError> {
    let refused = |reason: String| ActionHostError::Refused { reason };
    serde_json::to_value(outcome.map_err(refused)?).map_err(|error| refused(error.to_string()))
}

/// Take back the standing leaves of the item `taking` names, every agent's
/// or one's, answering what the item remembers then.
fn take_back(root: &Path, taking: &TakeBack) -> Result<ItemUses, vault_items::ItemError> {
    let id =
        vault_items::ItemId::parse(taking.item.id()).ok_or(vault_items::ItemError::NotFound)?;
    VaultItems::open_in(root)?.take_back(&id, taking.agent.as_deref())?;
    uses(root, &taking.item)
}

/// When and where the item `item` names was handed to something that signs
/// in, and for whom; and who may use it without its person being asked.
fn uses(root: &Path, item: &ItemRef) -> Result<ItemUses, vault_items::ItemError> {
    let id = vault_items::ItemId::parse(item.id()).ok_or(vault_items::ItemError::NotFound)?;
    let items = VaultItems::open_in(root)?;
    Ok(ItemUses {
        always: items
            .standing(&id)?
            .into_iter()
            .map(|leave| StandingUse {
                agent: leave.by,
                by: leave.name,
                using: leave.using,
                places: leave.places.into_iter().map(String::from).collect(),
                since: leave.since_unix,
            })
            .collect(),
        uses: items
            .uses(&id)?
            .into_iter()
            .map(|used| ItemUse {
                at: used.at_unix,
                places: used.places.into_iter().map(String::from).collect(),
                by: used.by,
            })
            .collect(),
    })
}

/// Hand the item `item` names to the code that signs in at every one of
/// `places` with a `kind`, for `by`: the one way a reference becomes its
/// secret, and only inside this host.
fn released(
    root: &Path,
    item: &ItemRef,
    kind: vault_items::ItemKind,
    places: &[Place],
    by: &str,
) -> Result<vault_items::Released, String> {
    let id = vault_items::ItemId::parse(item.id()).ok_or(vault_items::ItemError::NotFound);
    id.and_then(|id| VaultItems::open_in(root)?.use_as(&id, kind, places, by))
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
) -> Result<MailAccount, String> {
    let server = |scheme: &str, host: &str, port: u16| {
        Place::server(scheme, host, port).map_err(|error| error.to_string())
    };
    let places = [
        server("imap", &adding.incoming_host, adding.incoming_port)?,
        server("smtp", &adding.outgoing_host, adding.outgoing_port)?,
    ];
    let mut sign_in = released(
        root,
        &adding.password,
        vault_items::ItemKind::Login,
        &places,
        by,
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
        },
    )
    .await
}

/// Keep the key the vault item `setting` names as its provider's API key.
/// The item must be a key that belongs to the provider's endpoint, as the
/// person set it: a key is never kept for an endpoint it does not belong to.
pub(super) fn key_from_item(
    root: &Path,
    setting: &SetKeyFromItem,
    by: &str,
) -> Result<ProviderView, String> {
    let config = super::person_settings::provider_config(root);
    let profile = config
        .profiles
        .iter()
        .find(|profile| profile.id == setting.provider)
        .ok_or_else(|| "That provider is not kept here".to_owned())?;
    let endpoint = Place::parse(&profile.endpoint).map_err(|error| error.to_string())?;
    let key = released(
        root,
        &setting.key,
        vault_items::ItemKind::Key,
        std::slice::from_ref(&endpoint),
        by,
    )?;
    super::person_settings::set_key(
        root,
        SetKey {
            provider: setting.provider.clone(),
            key: key.secret.to_string(),
        },
    )
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
