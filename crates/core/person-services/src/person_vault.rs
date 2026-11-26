//! What a workspace keeps to sign in with, in its vault on the host it runs
//! on: logins, keys, code generators, ssh keys and plain values such as a
//! card number, each bound to where it belongs.
//!
//! The items are entries of the workspace's one vault, Passwords' vault
//! document ([`tilo_workspace_service::workspaces::vault`]), which this device holds and opens with the
//! document's own keys, and they are reached as the workspace's policy says
//! at Passwords' vault ([`Store::VAULT`]): a member through their role, an
//! agent or a key through a grant there. A Mail grant opens no vault.
//!
//! A secret travels in and never back. A listing says what is safe to say
//! of an item and no request answers more. Its own person alone is handed one
//! secret at a time, written down as a use: put on their clipboard by this
//! device ([`workspace_vault::COPY`]), or answered to be shown while they look
//! ([`workspace_vault::REVEAL`]), from Passwords or as themselves.
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
//! an item through the server's own entry ([`crate::hosted_use`]), which asks
//! the person first. Their "Always" is a grant kept by the workspace. A
//! member reads the leaves beside the uses and takes them back here, and no
//! agent does either.

use std::path::Path;
use std::sync::Arc;

use accounts::vault_items::{self, ItemFacts, NewItem, Place, UsedFor, VaultItems};
use serde_json::Value;
use tilo_action_policy::{ActionKind, Answer, Grant, Level};
use tilo_actions::remote::asked::{AskedFacts, Fact};
use tilo_actions::remote::{ActionCallError, Caller, InvokeArgument, InvokeRequest};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::models::{
    self as model_providers, ProviderView, SetKey, SetKeyFromItem,
};
use tilo_workspace_service::workspace_accounts::{
    self, AddMailAccount, AddMailWithItem, MailAccount,
};
use tilo_workspace_service::workspace_vault::{
    self, AlwaysUse, AskForItem, ItemKind, ItemRef, ItemUse, ItemUses, ItemView, KeptItems,
    Revealed, VaultAction, part,
};

use collab_types::WorkspaceId;
use tilo_workspace_service::workspaces::workspace_stores::{Reach, Store};

/// Where a device puts a secret its person copies from the vault ([`workspace_vault::COPY`]): the
/// clipboard of the session they sit at, which keeps it out of its history and forgets it again
/// after a while. A device with no session copies nothing.
pub trait SecretClipboard: Send + Sync {
    /// Offer `secret` as the person's copy. False when the clipboard did not take it.
    fn copy_secret(&self, secret: &str) -> bool;
}

/// What a person is told when this device has no clipboard of theirs to copy to.
pub const NO_CLIPBOARD: &str = "This device has no clipboard to copy to";

impl crate::PersonServices {
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

    /// Answer a [`workspace_vault::PROVIDER`] request.
    pub(super) async fn person_vault_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let action = request.target.address.action.as_str();
        let decl = workspace_vault::declaration(action).ok_or(ActionCallError::UnknownAction)?;
        let effect = if decl.action_kind == ActionKind::Read {
            ActionKind::Read
        } else {
            ActionKind::Undoable
        };
        // A release, and an app keeping its own secret, reach the vault only through the
        // process they answer (`Workspaces::release_to`, `Workspaces::keep_for`): whatever calls
        // them here is refused, saying why.
        if workspace_vault::asked_by_app(&request.target)
            .is_some_and(|action| workspace_vault::PAGE_LOGIN_ACTIONS.contains(&action))
        {
            return Err(ActionCallError::refused(crate::page_logins::NOT_A_WINDOW));
        }
        if matches!(
            action,
            workspace_vault::RELEASE | workspace_vault::KEEP | workspace_vault::DEVICE_KEY
        ) {
            return Err(ActionCallError::refused(crate::vault_release::refusal_for(
                caller,
            )));
        }
        // Who takes back what an agent may always use is a member, never an
        // agent: an agent's call of it is no call of the vault.
        if action == workspace_vault::TAKE_BACK && caller.acting().is_agent() {
            return Err(ActionCallError::UnknownAction);
        }
        // A secret is shown or copied for its own person alone: no agent is, and an app reaches
        // the vault only as its grants let it ([`Workspaces::reaching`]).
        if matches!(action, workspace_vault::COPY | workspace_vault::REVEAL)
            && (caller.acting().is_agent() || caller.acting().person_id().is_none())
        {
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
        let asked = asked_of(request)?;
        match asked {
            VaultAction::Code { .. } => self.vault_code_invoke(&workspace, caller, request).await,
            VaultAction::Uses { item } => {
                self.vault_uses_invoke((workspace_id, certificate), &workspace, &item, None)
                    .await
            }
            VaultAction::TakeBack { item, agent, app } => {
                let taking = (agent.as_deref(), app.as_deref());
                self.vault_uses_invoke((workspace_id, certificate), &workspace, &item, Some(taking))
                    .await
            }
            asked => {
                self.vault_item_invoke(&workspace, caller, request, asked, &reach)
                    .await
            }
        }
    }

    /// Answer the vault's listing, adding, removing, copying or showing of one item, `asked`
    /// for `request`, in the vault `workspace`, acted on by `reach`'s person or agent.
    async fn vault_item_invoke(
        &self,
        workspace: &str,
        caller: &Caller,
        request: &InvokeRequest,
        asked: VaultAction,
        reach: &Reach,
    ) -> Result<Value, ActionCallError> {
        let acting = (Some(&reach.acting), Some(request));
        match asked {
            VaultAction::List => {
                let opened = self
                    .open_vault(workspace)
                    .await
                    .map_err(ActionCallError::refused)?;
                answer(kept(&opened.items()))
            }
            adding @ VaultAction::Add { .. } => said(
                self.changing_vault(workspace, acting, |items| add(items, adding))
                    .await,
            ),
            VaultAction::Remove { item } => said(
                self.changing_vault(workspace, acting, |items| remove(items, &item))
                    .await,
            ),
            VaultAction::Copy { item, part } | VaultAction::Reveal { item, part } => {
                let app = caller.acting().app().map(|app| app.as_str().to_owned());
                let by = app.as_deref().map_or(String::new(), app_name);
                let copying = request.target.address.action == workspace_vault::COPY;
                let purpose = UsedFor {
                    using: &request.target.address.action,
                    what: if copying {
                        "Copied to the clipboard"
                    } else {
                        "Shown"
                    },
                    allowed: None,
                };
                let part = part.as_deref().unwrap_or(part::SECRET);
                let shown = self
                    .changing_vault(workspace, acting, |items| {
                        show(items, (&item, part), &by, purpose)
                    })
                    .await
                    .map_err(ActionCallError::refused)?;
                if copying {
                    self.copy_secret(&shown).map_err(ActionCallError::refused)?;
                    answer(Ok(serde_json::json!({})))
                } else {
                    answer(Ok(Revealed {
                        value: shown.to_string(),
                    }))
                }
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }

    /// Answer the code an item's seed makes now, for `request` in the vault `workspace`: a read,
    /// so nothing is written and no use stands in its history. The item is the action's
    /// resource, so a grant covers that one account.
    async fn vault_code_invoke(
        &self,
        workspace: &str,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        self.using_vault(workspace, caller, None, true)
            .await
            .map_err(ActionCallError::refused)?;
        let reference = ItemRef::parse(&request.target.address.resource)
            .ok_or(ActionCallError::UnknownAction)?;
        let code = self
            .vault_code(workspace, &reference.to_string())
            .await
            .map_err(ActionCallError::refused)?;
        let now = tilo_clock::unix_now();
        answer(Ok(serde_json::json!({
            "code": code.digits(),
            "left": code.until_unix.saturating_sub(now),
            "period": code.params.period,
            "digits": code.params.digits,
            "algorithm": code.params.algorithm.word(),
        })))
    }

    /// Answer an item's uses with the grants that let something use it without its person
    /// being asked, after taking back those `taking` names (its agent and its app, each when
    /// named), in the vault `workspace`.
    async fn vault_uses_invoke(
        &self,
        (workspace_id, certificate): (Option<collab_types::WorkspaceId>, Option<&DeviceCert>),
        workspace: &str,
        item: &ItemRef,
        taking: Option<(Option<&str>, Option<&str>)>,
    ) -> Result<Value, ActionCallError> {
        let workspace_id = workspace_id.ok_or(ActionCallError::UnknownAction)?;
        let cert = certificate.ok_or(ActionCallError::UnknownAction)?;
        let opened = self
            .open_vault(workspace)
            .await
            .map_err(ActionCallError::refused)?;
        let mut history =
            uses(&opened.items(), item).map_err(|err| ActionCallError::refused(err.to_string()))?;
        history.always = self
            .vault_approvals(workspace_id, cert, item, taking)
            .await?;
        answer(Ok(history))
    }

    /// Copy the secrets of the vault this device holds to `clipboard` from now on: the
    /// session's, which the device's daemon reaches as its person does.
    pub fn copy_secrets_to(&self, clipboard: Arc<dyn SecretClipboard>) {
        let _ = self.inner.secret_clipboard.set(clipboard);
    }

    /// Put `secret` on this device's clipboard, or say why it was not.
    fn copy_secret(&self, secret: &str) -> Result<(), String> {
        let clipboard = self
            .inner
            .secret_clipboard
            .get()
            .ok_or_else(|| NO_CLIPBOARD.to_owned())?;
        if clipboard.copy_secret(secret) {
            Ok(())
        } else {
            Err(NO_CLIPBOARD.to_owned())
        }
    }

    /// Read a vault item's grants, and revoke those the person chose to take back.
    async fn vault_approvals(
        &self,
        workspace: WorkspaceId,
        device: &DeviceCert,
        item: &ItemRef,
        take_back: Option<(Option<&str>, Option<&str>)>,
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
        if let Some((agent, app)) = take_back {
            let mut ids = Vec::new();
            kept.retain(|(grant, shown)| {
                let other_agent = agent.is_some_and(|agent| agent != shown.agent);
                let other_app = app.is_some_and(|app| app != shown.app);
                if other_agent || other_app {
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
        let times = workspace.peer().governance().member_grant_times(&grants);
        drop(workspace);
        let mut shown = Vec::with_capacity(kept.len());
        for (grant, mut approval) in kept {
            approval.since = times.get(&grant.id).map_or(0, |at| at / 1_000_000_000);
            if !approval.agent.is_empty() {
                approval.by = self
                    .agent_name(&name, &approval.agent)
                    .await
                    .unwrap_or_default();
            }
            shown.push(approval);
        }
        shown.sort_by_key(|approval| approval.since);
        Ok(shown)
    }

    /// Answer a request that uses a vault item by its reference:
    /// [`workspace_accounts::ADD_MAIL_WITH_ITEM`] or
    /// [`model_providers::SET_KEY_FROM_ITEM`].
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
        let used_for = if target.provider == workspace_accounts::PROVIDER {
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
        let word = workspace_vault::use_word(target);
        let purpose = UsedFor {
            using: &word,
            ..UsedFor::default()
        };
        match (target.provider.as_str(), target.address.action.as_str()) {
            (workspace_accounts::PROVIDER, workspace_accounts::ADD_MAIL_WITH_ITEM) => {
                let adding = match &request.argument {
                    InvokeArgument::Json(arguments) => AddMailWithItem::asked(arguments),
                    _ => None,
                }
                .ok_or_else(|| ActionCallError::InvalidRequest {
                    reason: "a mail account to add names its address, its servers and the vault \
                             item that is its password"
                        .to_owned(),
                })?;
                let kept = self
                    .mail_with_item(
                        (&workspace, &root),
                        adding,
                        (&by, &reach.acting, Some(request)),
                        purpose,
                    )
                    .await;
                if kept.is_ok() {
                    self.person_settings_changed();
                }
                said(kept)
            }
            (model_providers::PROVIDER, model_providers::SET_KEY_FROM_ITEM) => {
                let setting = match &request.argument {
                    InvokeArgument::Json(arguments) => SetKeyFromItem::asked(arguments),
                    _ => None,
                }
                .ok_or_else(|| ActionCallError::InvalidRequest {
                    reason: "a key to keep names its provider and the vault item that is the key"
                        .to_owned(),
                })?;
                said(
                    self.key_from_item(
                        (&workspace, &root),
                        &setting,
                        (&by, &reach.acting, Some(request)),
                        purpose,
                    )
                    .await,
                )
            }
            (workspace_vault::PROVIDER, workspace_vault::ASK) => {
                let VaultAction::Ask { asked } = asked_of(request)? else {
                    return Err(ActionCallError::UnknownAction);
                };
                let asked = asked.checked().map_err(ActionCallError::refused)?;
                tilo_workspace_service::workspaces::requests::asking_person(
                    caller,
                    certificate,
                    workspace_id,
                )?;
                if request.approval().is_none() {
                    return Err(ActionCallError::asks(secret_asked(caller, &by, &asked)));
                }
                let reference = self.asked_item(&workspace, &asked).await;
                said(reference.map(|reference| workspace_vault::Asked { reference }))
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
        answered_by: Some(format!(
            "{}/{}#{}",
            workspace_vault::PROVIDER,
            workspace_vault::RESOURCE,
            workspace_vault::ADD
        )),
        ..AskedFacts::by(tilo_workspace_service::workspaces::asking::asked_by(caller))
    }
}

/// The vault's action `request` asks, its arguments read as the declaration names them. The
/// reason a call is refused never quotes its arguments.
pub(super) fn asked_of(request: &InvokeRequest) -> Result<VaultAction, ActionCallError> {
    let action = &request.target.address.action;
    if workspace_vault::declaration(action).is_none() {
        return Err(ActionCallError::UnknownAction);
    }
    let arguments = match &request.argument {
        InvokeArgument::Json(arguments) => arguments.clone(),
        InvokeArgument::None | InvokeArgument::Text(_) => serde_json::Map::new(),
    };
    workspace_vault::decoded(action, &arguments).map_err(ActionCallError::invalid_request)
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
    /// A year in seconds: an item last changed longer ago than this is stale.
    const YEAR: u64 = 365 * 24 * 60 * 60;
    let code = facts.code;
    Ok(ItemView {
        reference: ItemRef::from_id(facts.id.as_str()).ok_or(vault_items::ItemError::Damaged)?,
        kind: match facts.kind {
            vault_items::ItemKind::Login => ItemKind::Login,
            vault_items::ItemKind::Key => ItemKind::Key,
            vault_items::ItemKind::CodeGenerator => ItemKind::CodeGenerator,
            vault_items::ItemKind::SshKey => ItemKind::SshKey,
            vault_items::ItemKind::Value => ItemKind::Value,
            vault_items::ItemKind::Note => ItemKind::Note,
            vault_items::ItemKind::Wifi => ItemKind::Wifi,
        },
        belongs_to: facts.places.into_iter().map(String::from).collect(),
        title: Some(facts.title).filter(|title| !title.trim().is_empty()),
        added_at: facts.added_unix,
        last_used_at: facts.last_used_unix,
        algorithm: code.map(|code| code.algorithm.word().to_owned()),
        digits: code.map(|code| code.digits),
        period: code.map(|code| code.period),
        tags: facts.tags,
        fields: facts.field_labels,
        has_notes: facts.has_notes,
        changed_at: facts.changed_unix,
        concerns: [
            (facts.weak, workspace_vault::Concern::Weak),
            (facts.reused, workspace_vault::Concern::Reused),
            (
                tilo_clock::unix_now().saturating_sub(facts.changed_unix) > YEAR,
                workspace_vault::Concern::Stale,
            ),
        ]
        .into_iter()
        .filter_map(|(holds, concern)| holds.then_some(concern))
        .collect(),
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

/// Keep the item `adding` ([`VaultAction::Add`]) describes in `items`, or change the one it
/// names, answering what is safe to say of it.
pub(super) fn add(
    items: &mut VaultItems,
    adding: VaultAction,
) -> Result<ItemView, vault_items::ItemError> {
    let VaultAction::Add {
        item: changing,
        kind,
        belongs_to,
        username,
        sent_in,
        algorithm,
        digits,
        period,
        title,
        notes,
        fields,
        tags,
        generate,
        secret,
    } = adding
    else {
        return Err(vault_items::ItemError::Invalid("That is no item to keep"));
    };
    let (username, sent_in, algorithm, title) = (
        username.unwrap_or_default(),
        sent_in.unwrap_or_default(),
        algorithm.unwrap_or_default(),
        title.unwrap_or_default(),
    );
    let (notes, fields, tags) = (
        notes.unwrap_or_default(),
        fields.unwrap_or_default(),
        tags.unwrap_or_default(),
    );
    let secret = secret.unwrap_or_default();
    let places = belongs_to
        .unwrap_or_default()
        .iter()
        .map(|place| Place::parse(place))
        .collect::<Result<Vec<_>, _>>()?;
    let kind = match kind {
        ItemKind::Login => vault_items::ItemKind::Login,
        ItemKind::Key => vault_items::ItemKind::Key,
        ItemKind::CodeGenerator => vault_items::ItemKind::CodeGenerator,
        ItemKind::SshKey => vault_items::ItemKind::SshKey,
        ItemKind::Value => vault_items::ItemKind::Value,
        ItemKind::Note => vault_items::ItemKind::Note,
        ItemKind::Wifi => vault_items::ItemKind::Wifi,
    };
    let code = if kind == vault_items::ItemKind::CodeGenerator {
        // A code generator that names no hash makes its codes with SHA1, as a setup key that
        // says nothing of its hash does.
        let algorithm = if algorithm.trim().is_empty() {
            vault_items::Algorithm::default().word()
        } else {
            algorithm.as_str()
        };
        vault_items::CodeParams::checked(
            algorithm,
            digits.unwrap_or(vault_items::CODE_DIGITS),
            period.unwrap_or(vault_items::CODE_SECS),
        )?
    } else {
        vault_items::CodeParams::default()
    };
    let secret = match generate {
        None => zeroize::Zeroizing::new(secret),
        Some(_) if !secret.is_empty() => {
            return Err(vault_items::ItemError::Invalid(
                "An item takes a secret or draws one, not both",
            ));
        }
        Some(_) if kind != vault_items::ItemKind::Login => {
            return Err(vault_items::ItemError::Invalid(
                "Only a login's password is drawn by the vault",
            ));
        }
        Some(asked) => drawn(asked)?,
    };
    let extras = vault_items::Extras {
        title,
        notes: zeroize::Zeroizing::new(notes),
        fields: fields
            .into_iter()
            .map(|field| (field.label, zeroize::Zeroizing::new(field.value)))
            .collect(),
        tags,
    };
    let item = NewItem::new(kind, places, username, secret.to_string()).with_code(code);
    // Only a key says where it rides in a request to its service.
    let sent_in = if sent_in.trim().is_empty() {
        None
    } else {
        Some(vault_items::KeyIn::parse(&sent_in)?)
    };
    match changing {
        Some(changing) => {
            let id = vault_items::ItemId::parse(changing.id())
                .ok_or(vault_items::ItemError::NotFound)?;
            view(items.change(&id, item, sent_in.as_ref(), &extras)?)
        }
        None => view(items.add_with(item, sent_in.as_ref(), &extras)?),
    }
}

/// A strong password drawn here for `asked`: never sent, never typed.
fn drawn(
    asked: workspace_vault::Generate,
) -> Result<zeroize::Zeroizing<String>, vault_items::ItemError> {
    use workspace_vault::Generate;
    if !(Generate::SHORTEST..=Generate::LONGEST).contains(&asked.length) {
        return Err(vault_items::ItemError::Invalid(
            "A drawn password is 12 to 128 characters long",
        ));
    }
    accounts::GeneratedPasswordPolicy {
        length: usize::from(asked.length),
        lowercase: true,
        uppercase: asked.capitals,
        digits: asked.digits,
        symbols: asked.symbols,
    }
    .generate()
    .map_err(|_| vault_items::ItemError::Invalid("No password could be drawn here"))
}

/// The part `shown` names of its item its person is handed, as `by` and for `purpose`.
fn show(
    items: &mut VaultItems,
    (item, shown): (&ItemRef, &str),
    by: &str,
    purpose: vault_items::UsedFor<'_>,
) -> Result<zeroize::Zeroizing<String>, vault_items::ItemError> {
    let id = vault_items::ItemId::parse(item.id()).ok_or(vault_items::ItemError::NotFound)?;
    let part = match shown {
        part::SECRET => vault_items::Part::Secret,
        part::NOTES => vault_items::Part::Notes,
        part::USERNAME => vault_items::Part::Username,
        other => match other.strip_prefix(part::FIELD) {
            Some(label) => vault_items::Part::Field(label),
            None => {
                return Err(vault_items::ItemError::Invalid(
                    "A part is `secret`, `notes`, `username` or `field:<label>`",
                ));
            }
        },
    };
    items.show(&id, part, by, purpose)
}

/// What a use by `app` is written down under: the app's name where the system knows it.
fn app_name(app: &str) -> String {
    if app == tilo_app_id::apps::PASSWORDS {
        "Passwords".to_owned()
    } else {
        app.to_owned()
    }
}

fn remove(items: &mut VaultItems, item: &ItemRef) -> Result<KeptItems, vault_items::ItemError> {
    let id = vault_items::ItemId::parse(item.id()).ok_or(vault_items::ItemError::NotFound)?;
    if !items.remove(&id) {
        return Err(vault_items::ItemError::NotFound);
    }
    kept(items)
}

fn said<T: serde::Serialize>(outcome: Result<T, String>) -> Result<Value, ActionCallError> {
    let refused = |reason: String| ActionCallError::Refused { reason };
    serde_json::to_value(outcome.map_err(refused)?).map_err(|error| refused(error.to_string()))
}

/// What `grant` lets an agent always use of the workspace's vault item
/// `item`, or lets an app read of it, if it is such a grant.
fn vault_approval(grant: &Grant, item: &ItemRef) -> Option<AlwaysUse> {
    if grant.answer != Answer::Allow {
        return None;
    }
    if let Some(app) = grant.grantee.app_id() {
        let reads = grant.node.id_at(Level::App) == Some(workspace_vault::PROVIDER)
            && grant.node.id_at(Level::Kind) == Some(workspace_vault::ITEM_KIND)
            && grant.node.id_at(Level::Resource) == Some(item.to_string().as_str())
            && grant.node.id_at(Level::Action) == Some(workspace_vault::RELEASE);
        return reads.then(|| AlwaysUse {
            agent: String::new(),
            app: app.to_owned(),
            by: String::new(),
            using: workspace_vault::RELEASE.to_owned(),
            places: Vec::new(),
            since: 0,
        });
    }
    let agent = grant.grantee.agent_id()?;
    let (app, action) = (
        grant.node.id_at(Level::App)?,
        grant.node.id_at(Level::Action)?,
    );
    if !matches!(
        (app, action),
        (
            workspace_accounts::PROVIDER,
            workspace_accounts::ADD_MAIL_WITH_ITEM
        ) | (
            model_providers::PROVIDER,
            model_providers::SET_KEY_FROM_ITEM
        ) | (
            workspace_vault::PROVIDER,
            workspace_vault::FILL_PAGE | workspace_vault::SIGN
        )
    ) {
        return None;
    }
    let (named, places) = grant.node.id_at(Level::Resource)?.split_once('/')?;
    if named != item.id() {
        return None;
    }
    Some(AlwaysUse {
        agent: agent.to_owned(),
        app: String::new(),
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

impl crate::PersonServices {
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
        (by, asker, call): (&str, &tilo_action_policy::Acting, Option<&InvokeRequest>),
        purpose: UsedFor<'_>,
    ) -> Result<vault_items::Released, String> {
        let id = vault_items::ItemId::parse(item.id())
            .ok_or_else(|| vault_items::ItemError::NotFound.to_string())?;
        self.changing_vault(workspace, (Some(asker), call), |items| {
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
        (by, asker, call): (&str, &tilo_action_policy::Acting, Option<&InvokeRequest>),
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
                (by, asker, call),
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
        Box::pin(crate::person_settings::add_mail_with(
            self.account_tester(),
            (self.vault_serving(workspace, asker, call), root),
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
                services: workspace_accounts::AccountServices::default(),
            },
            Some(kept),
            workspace_accounts::Provider::Other,
            None,
        ))
        .await
    }

    /// Name the key the vault item `setting` names as its provider's API key. The item must be
    /// a key that belongs to the provider's endpoint, as the person set it: a key is never sent
    /// to an endpoint it does not belong to. Where the provider names a key already, the item
    /// takes its place only once the provider took it ([`crate::model_providers::take_key`]); the
    /// provider names the item from then on, and nothing copies the key.
    pub(super) async fn key_from_item(
        &self,
        (workspace, root): (&str, &Path),
        setting: &SetKeyFromItem,
        (by, asker, call): (&str, &tilo_action_policy::Acting, Option<&InvokeRequest>),
        purpose: UsedFor<'_>,
    ) -> Result<ProviderView, String> {
        let endpoint = {
            let config = crate::person_settings::provider_config(root);
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
                (by, asker, call),
                purpose,
            )
            .await?;
        let keeping = SetKey {
            provider: setting.provider.clone(),
            key: key.secret.to_string(),
        };
        let keys = crate::person_settings::ProviderKeys::of(
            self.vault_serving(workspace, asker, call),
            by,
        );
        crate::model_providers::take_key(
            (keys, root),
            keeping,
            Some(&setting.key),
            &self.model_reach(),
        )
        .await
    }
}

#[cfg(test)]
#[path = "asking_tests.rs"]
pub(crate) mod asking_tests;
#[cfg(all(test, feature = "agents"))]
#[path = "device_key_tests.rs"]
pub(crate) mod device_key_tests;
#[cfg(all(test, feature = "agents"))]
#[path = "keep_devices_tests.rs"]
pub(crate) mod keep_devices_tests;
#[cfg(all(test, feature = "agents"))]
#[path = "vault_asked_tests.rs"]
pub(crate) mod vault_asked_tests;
#[cfg(test)]
#[path = "vault_tests.rs"]
pub(crate) mod vault_tests;
