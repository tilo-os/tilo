//! Authenticator's exported **actions** and the **resource tree** it publishes
//! to Ctrl+K.
//!
//! Every document-mutating operation the vault performs — adding an account,
//! deleting one, renaming or relabelling it, recolouring it, or reordering the
//! list — is a [`TotpAction`]: a small serializable value that deterministically
//! mutates the shared CRDT. Because the account list already lives in the shared
//! document (`root["accounts"]`, keyed by uuid — see [`crate::doc`]), an action
//! replicates and replays for free. The exact same [`EncodedAction`] is what the
//! on-screen affordances (the list's delete / reorder buttons, the Add-account
//! save) dispatch and what the Ctrl+K palette dispatches — one source of truth,
//! one replay path.
//!
//! Presentation stays out of the document and never becomes an action: the live
//! rotating TOTP code and its countdown ring are *computed* from the account's
//! secret and the wall-clock every frame (a doc mutation would be wrong — the
//! code is derived, not stored), and so are reveal/hide, copy-to-clipboard,
//! search, and selection.
//!
//! # A note on the secret
//! An [`TotpAction::AddAccount`] carries the account's base32 secret in its
//! payload. It *must* — the secret is the account, and replay has to reproduce
//! it byte-for-byte on every replica. The document already stores the same
//! secret, so the action surface exposes nothing the CRDT didn't already.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use qos_actions::{
    Action, ActionAddress, ActionContract, ActionError, ActionParam, ActionRegistry, ActionSpec,
    Availability, ConfirmationPolicy, Doc, EncodedAction, ExecutionLocus, OperationKind, ParamType,
    PermissionRequirement, Resource, ResourceProvider, RetryPolicy, RiskLevel,
};

use crate::doc::AuthDoc;
use crate::model::{Account, AccountId, Algorithm, parse_otpauth_uri};

/// Registry `type_id` for every Authenticator action.
pub const TOTP_ACTION_TYPE: &str = "totp.action";
/// Operation payload type for Authenticator presentation and local effects.
/// These operations never enter the collaborative action decoder.
pub const TOTP_OPERATION_TYPE: &str = "totp.operation";

/// Stable resource id for one authenticator account. It contains only the
/// account UUID; neither the long-lived secret nor the rotating code is ever
/// placed in an address or label.
pub fn account_resource_id(id: AccountId) -> String {
    format!("totp.account:{id}")
}

/// Canonical View action used by search, the palette and agents to reveal an
/// account in the Authenticator window.
pub fn open_account_address(id: AccountId) -> ActionAddress {
    ActionAddress::new(account_resource_id(id), "open")
}

/// Local operations exported by account resources. Payloads carry only stable
/// account ids; code generation happens after the broker has resolved the
/// fresh resource and admitted the operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TotpOperation {
    OpenAccount { id: String },
    CopyCode { id: String },
}

pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<TotpOperation>, String> {
    if type_id != TOTP_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| format!("invalid Authenticator operation: {error}"))
}

fn account_operation(
    id: &str,
    label: &str,
    kind: OperationKind,
    operation: &TotpOperation,
) -> ActionSpec {
    ActionSpec::operation(id, label, kind, TOTP_OPERATION_TYPE, operation)
        .expect("Authenticator account ids are JSON serializable")
}

/// Every collaborative operation the vault exports on its account store.
///
/// Determinism contract: ids are minted at *authoring* time and carried in the
/// payload, never inside [`Action::apply`], which must not touch the RNG or the
/// wall-clock. A malformed payload (unparseable id, empty secret) is a **no-op**
/// rather than an error, so a bad replay can't wedge a peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TotpAction {
    /// Add a full account. Every parameter needed to reproduce the record —
    /// including the base32 `secret` and the pre-computed `order`/`color` — is
    /// carried so replay lands the byte-identical row on every peer.
    AddAccount {
        id: String,
        issuer: String,
        label: String,
        secret: String,
        algorithm: String,
        digits: u8,
        period: u32,
        color: u8,
        order: u32,
    },
    /// Remove an account by id. Idempotent.
    DeleteAccount { id: String },
    /// Rewrite an account's issuer (its display title).
    SetIssuer { id: String, issuer: String },
    /// Rewrite an account's label.
    SetLabel { id: String, label: String },
    /// Recolour an account (index into the theme palette).
    SetColor { id: String, color: u8 },
    /// Move an account one slot earlier. Deterministic against the document at
    /// the replay's base state: it reads the current order, finds the neighbour,
    /// and swaps the two `order` values.
    MoveUp { id: String },
    /// Move an account one slot later. Deterministic against the base state, as
    /// [`MoveUp`](Self::MoveUp).
    MoveDown { id: String },
}

impl Action for TotpAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            TotpAction::AddAccount {
                id,
                issuer,
                label,
                secret,
                algorithm,
                digits,
                period,
                color,
                order,
            } => {
                // A malformed id or an empty secret is never produced by our own
                // builder — treat it as a no-op so a bad replay can't wedge.
                if secret.trim().is_empty() {
                    return Ok(());
                }
                let Ok(uuid) = id.parse::<AccountId>() else {
                    return Ok(());
                };
                let acc = Account {
                    id: uuid,
                    issuer: issuer.clone(),
                    label: label.clone(),
                    secret: secret.clone(),
                    algorithm: Algorithm::parse(algorithm),
                    digits: (*digits).clamp(4, 10),
                    period: (*period).clamp(5, 300),
                    color: *color,
                    order: *order,
                };
                AuthDoc::upsert_account(doc, &acc);
            }
            TotpAction::DeleteAccount { id } => {
                if let Ok(uuid) = id.parse::<AccountId>() {
                    AuthDoc::delete_account(doc, uuid);
                }
            }
            TotpAction::SetIssuer { id, issuer } => {
                if let Ok(uuid) = id.parse::<AccountId>() {
                    AuthDoc::set_issuer(doc, uuid, issuer);
                }
            }
            TotpAction::SetLabel { id, label } => {
                if let Ok(uuid) = id.parse::<AccountId>() {
                    AuthDoc::set_label(doc, uuid, label);
                }
            }
            TotpAction::SetColor { id, color } => {
                if let Ok(uuid) = id.parse::<AccountId>() {
                    AuthDoc::set_color(doc, uuid, *color);
                }
            }
            TotpAction::MoveUp { id } => move_within(doc, id, -1),
            TotpAction::MoveDown { id } => move_within(doc, id, 1),
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Swap an account with its `delta`-neighbour in list order. Reads the document
/// at the replay's base state (deterministic — no RNG / wall-clock) and rewrites
/// only the two affected `order` scalars.
fn move_within(doc: &mut Doc, id: &str, delta: i64) {
    let Ok(uuid) = id.parse::<AccountId>() else {
        return;
    };
    let ordered = AuthDoc::list_accounts(doc); // sorted by order, deterministic
    let Some(pos) = ordered.iter().position(|a| a.id == uuid) else {
        return;
    };
    let target = pos as i64 + delta;
    if target < 0 || target as usize >= ordered.len() {
        return; // already at the edge
    }
    let Some(a) = ordered.get(pos) else {
        return;
    };
    let Some(b) = ordered.get(target as usize) else {
        return;
    };
    AuthDoc::set_order(doc, a.id, b.order);
    AuthDoc::set_order(doc, b.id, a.order);
}

/// Register the Authenticator action decoder for palette execution + remote
/// replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<TotpAction>(TOTP_ACTION_TYPE);
}

/// Encode any [`TotpAction`] into the wire form the palette and the on-screen
/// affordances both dispatch. The single point that turns an action into a
/// replayable envelope.
///
/// # Panics
/// Never in practice: every `TotpAction` field is a `String`, `u8` or `u32`,
/// all of which `serde_json` always serializes; this only panics if a future
/// field type introduces a case JSON can't represent (e.g. a non-string map
/// key or a non-finite float).
#[allow(
    clippy::expect_used,
    reason = "TotpAction's fields are all JSON-safe primitives; see this function's # Panics doc"
)]
pub fn encode(action: &TotpAction) -> EncodedAction {
    EncodedAction::encode(TOTP_ACTION_TYPE, action).expect("TotpAction is always serializable")
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::app`) and the Ctrl+K resource tree below.
// ===========================================================================

/// Build an [`TotpAction::AddAccount`] from a resolved account plus its
/// authored `order`. `color` and `order` are baked in at authoring time so
/// replay is deterministic.
pub fn add_account_action(acc: &Account, order: u32) -> TotpAction {
    TotpAction::AddAccount {
        id: acc.id.to_string(),
        issuer: acc.issuer.clone(),
        label: acc.label.clone(),
        secret: acc.secret.clone(),
        algorithm: acc.algorithm.as_str().to_string(),
        digits: acc.digits,
        period: acc.period,
        color: acc.color,
        order,
    }
}

pub fn delete_account_action(id: AccountId) -> TotpAction {
    TotpAction::DeleteAccount { id: id.to_string() }
}

pub fn set_issuer_action(id: AccountId, issuer: &str) -> TotpAction {
    TotpAction::SetIssuer {
        id: id.to_string(),
        issuer: issuer.to_string(),
    }
}

pub fn set_label_action(id: AccountId, label: &str) -> TotpAction {
    TotpAction::SetLabel {
        id: id.to_string(),
        label: label.to_string(),
    }
}

pub fn set_color_action(id: AccountId, color: u8) -> TotpAction {
    TotpAction::SetColor {
        id: id.to_string(),
        color,
    }
}

pub fn move_up_action(id: AccountId) -> TotpAction {
    TotpAction::MoveUp { id: id.to_string() }
}

pub fn move_down_action(id: AccountId) -> TotpAction {
    TotpAction::MoveDown { id: id.to_string() }
}

// ===========================================================================
// Resource tree
// ===========================================================================

#[allow(
    clippy::expect_used,
    reason = "TotpAction's fields are all JSON-safe primitives, and id/label are static strings; see encode()'s # Panics doc"
)]
fn encode_spec(id: &str, label: &str, action: &TotpAction) -> ActionSpec {
    ActionSpec::immediate(id, label, TOTP_ACTION_TYPE, action)
        .expect("encode totp action")
        .with_contract(vault_contract())
}

fn vault_contract() -> ActionContract {
    ActionContract {
        permission: PermissionRequirement::Write,
        retry: RetryPolicy::Idempotent,
        reversible: true,
        tags: vec!["security".into(), "credential".into()],
        ..ActionContract::default()
    }
}

/// One-line palette label for an account resource.
pub(crate) fn safe_identity_field<'a>(value: &'a str, secret: &str) -> &'a str {
    let value = value.trim();
    let secret = secret.trim();
    if !secret.is_empty()
        && value
            .to_ascii_uppercase()
            .contains(&secret.to_ascii_uppercase())
    {
        ""
    } else {
        value
    }
}

fn account_label(a: &Account) -> String {
    let issuer = safe_identity_field(&a.issuer, &a.secret);
    let label = safe_identity_field(&a.label, &a.secret);
    match (issuer.is_empty(), label.is_empty()) {
        (false, false) => format!("{issuer} — {label}"),
        (false, true) => issuer.to_string(),
        (true, false) => label.to_string(),
        (true, true) => "Account".to_string(),
    }
}

/// Build the resource tree the Authenticator currently has open: the vault and,
/// nested under it, one resource per account — every document-mutating
/// operation exported as an action. Read straight from the document so the tree
/// is a level-triggered value, recomputed each frame.
/// Build only the vault root for the initial palette snapshot.
pub fn provide_contextual_resources(accounts: &[Account]) -> Vec<Resource> {
    let mut root = Resource::new("totp.vault", "totp.vault", "Accounts");

    // Vault-level: add an account by pasting an `otpauth://` setup link. One
    // free-text argument carries everything (issuer, label, secret, params); the
    // uuid is minted once when the argument is committed and the colour/order are
    // pinned from the current vault, so the built envelope replays deterministically.
    let next_order = accounts.iter().map(|a| a.order).max().map_or(0, |m| m + 1);
    let next_color = (accounts.len() % 7) as u8;
    root = root.with_action(
        ActionSpec::typed(
            "add-account",
            "Add account",
            "otpauth://totp/Issuer:you@example.com?secret=\u{2026}",
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required(
                    "uri",
                    ParamType::Secret,
                    "otpauth setup URI; redacted from logs",
                )
                .with_schema(serde_json::json!({ "pattern": "^otpauth://totp/" })),
            ],
            move |args| {
                let uri = args.text("uri").unwrap_or_default();
                let action = match parse_otpauth_uri(uri, Uuid::new_v4()) {
                    Ok(mut acc) => {
                        acc.color = next_color;
                        add_account_action(&acc, next_order)
                    }
                    Err(error) => {
                        return EncodedAction {
                            type_id: qos_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                            payload: format!("invalid otpauth setup URI: {error}").into_bytes(),
                        };
                    }
                };
                encode(&action)
            },
        )
        .with_contract(ActionContract {
            risk: RiskLevel::Privileged,
            confirmation: ConfirmationPolicy::Confirm,
            ..vault_contract()
        }),
    );

    vec![root]
}

/// Materialise one account after search names its stable resource id.
pub fn provide_account_resource(acc: &Account, index: usize, account_count: usize) -> Resource {
    let id = acc.id;
    let cur_color = acc.color;
    let mut child = Resource::new(account_resource_id(id), "totp.account", account_label(acc));

    child = child.with_action(
        account_operation(
            "open",
            "Open account",
            OperationKind::View,
            &TotpOperation::OpenAccount { id: id.to_string() },
        )
        .with_contract(ActionContract::view()),
    );
    child = child.with_action(
        account_operation(
            "copy-code",
            "Copy current code",
            OperationKind::Effect,
            &TotpOperation::CopyCode { id: id.to_string() },
        )
        .with_contract(ActionContract {
            permission: PermissionRequirement::Read,
            risk: RiskLevel::Privileged,
            confirmation: ConfirmationPolicy::Confirm,
            retry: RetryPolicy::AtMostOnce,
            tags: vec!["security".into(), "clipboard".into()],
            ..ActionContract::default()
        }),
    );

    child = child.with_action(
        ActionSpec::typed(
            "rename",
            "Rename",
            "e.g. GitHub",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required(
                "issuer",
                ParamType::Text,
                "Account issuer",
            )],
            move |args| {
                encode(&set_issuer_action(
                    id,
                    args.text("issuer").unwrap_or_default().trim(),
                ))
            },
        )
        .with_contract(vault_contract()),
    );
    child = child.with_action(
        ActionSpec::typed(
            "relabel",
            "Relabel account",
            "e.g. you@example.com",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required(
                "label",
                ParamType::Text,
                "Account label",
            )],
            move |args| {
                encode(&set_label_action(
                    id,
                    args.text("label").unwrap_or_default().trim(),
                ))
            },
        )
        .with_contract(vault_contract()),
    );
    child = child.with_action(
        ActionSpec::typed(
            "set-color",
            "Set colour",
            "0",
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required("colour", ParamType::Integer, "Colour index")
                    .with_schema(serde_json::json!({ "minimum": 0, "maximum": 6 })),
            ],
            move |args| {
                encode(&set_color_action(
                    id,
                    args.integer("colour")
                        .and_then(|value| u8::try_from(value).ok())
                        .unwrap_or(cur_color),
                ))
            },
        )
        .with_contract(vault_contract()),
    );
    child = child.with_action(
        encode_spec("move-up", "Move up", &move_up_action(id)).with_contract(ActionContract {
            availability: if index > 0 {
                Availability::Available
            } else {
                Availability::Unavailable {
                    code: "already_first".into(),
                    reason: "This account is already first".into(),
                }
            },
            ..vault_contract()
        }),
    );
    child = child.with_action(
        encode_spec("move-down", "Move down", &move_down_action(id)).with_contract(
            ActionContract {
                availability: if index + 1 < account_count {
                    Availability::Available
                } else {
                    Availability::Unavailable {
                        code: "already_last".into(),
                        reason: "This account is already last".into(),
                    }
                },
                ..vault_contract()
            },
        ),
    );
    child = child.with_action(
        encode_spec("delete", "Delete account", &delete_account_action(id)).with_contract(
            ActionContract {
                risk: RiskLevel::Destructive,
                confirmation: ConfirmationPolicy::Confirm,
                ..vault_contract()
            },
        ),
    );
    child
}

pub fn provide_resources(doc: &AutoCommit) -> Vec<Resource> {
    let accounts = AuthDoc::list_accounts(doc);
    let mut root = provide_contextual_resources(&accounts)
        .pop()
        .unwrap_or_else(|| Resource::new("totp.vault", "totp.vault", "Accounts"));
    let account_count = accounts.len();
    for (index, acc) in accounts.iter().enumerate() {
        root = root.with_child(provide_account_resource(acc, index, account_count));
    }

    vec![root]
}

impl ResourceProvider for crate::app::AuthenticatorApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AuthenticatorApp;
    use qos_actions::{ActionInput, LocalTransport};
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    /// Build an [`EncodedAction`] from a prompted spec's closure — the same path
    /// the palette takes on Enter.
    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) => panic!("expected a prompted action: {}", spec.id),
        }
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } => panic!("expected an immediate action: {}", spec.id),
        }
    }

    /// Two deterministic accounts seeded straight into a fresh document (the
    /// initial load path — not itself an action).
    fn seed_accounts() -> Vec<Account> {
        vec![
            Account {
                id: Uuid::from_u128(0x1111_0000_0000_0000_0000_0000_0000_0001),
                issuer: "GitHub".into(),
                label: "ada@example.com".into(),
                secret: "JBSWY3DPEHPK3PXP".into(),
                algorithm: Algorithm::Sha1,
                digits: 6,
                period: 30,
                color: 0,
                order: 0,
            },
            Account {
                id: Uuid::from_u128(0x2222_0000_0000_0000_0000_0000_0000_0002),
                issuer: "AWS".into(),
                label: "iam/ada-ops".into(),
                secret: "NB2W45DFOIZA".into(),
                algorithm: Algorithm::Sha256,
                digits: 6,
                period: 30,
                color: 2,
                order: 1,
            },
        ]
    }

    fn seeded_doc() -> AutoCommit {
        let mut doc = AutoCommit::new();
        AuthDoc::sync_vault(&mut doc, &seed_accounts());
        doc
    }

    #[test]
    fn resource_contracts_keep_navigation_effects_secrets_and_deletion_distinct() {
        let doc = seeded_doc();
        let resources = provide_resources(&doc);
        let vault = &resources[0];
        let account = &vault.children[0];

        let open = account
            .actions
            .iter()
            .find(|action| action.id == "open")
            .unwrap();
        assert_eq!(open.kind, OperationKind::View);
        assert_eq!(open.contract, ActionContract::view());

        let copy = account
            .actions
            .iter()
            .find(|action| action.id == "copy-code")
            .unwrap();
        assert_eq!(copy.kind, OperationKind::Effect);
        assert_eq!(copy.contract.permission, PermissionRequirement::Read);
        assert_eq!(copy.contract.risk, RiskLevel::Privileged);
        assert_eq!(copy.contract.confirmation, ConfirmationPolicy::Confirm);
        assert_eq!(copy.contract.retry, RetryPolicy::AtMostOnce);
        let encoded_copy = immediate_encode(copy);
        let operation: TotpOperation = serde_json::from_slice(&encoded_copy.payload).unwrap();
        assert!(matches!(operation, TotpOperation::CopyCode { .. }));
        for secret in seed_accounts()
            .iter()
            .map(|account| account.secret.as_str())
        {
            assert!(
                !encoded_copy
                    .payload
                    .windows(secret.len())
                    .any(|bytes| bytes == secret.as_bytes()),
                "copy authority carries an id, never secret material"
            );
        }

        let add = vault
            .actions
            .iter()
            .find(|action| action.id == "add-account")
            .unwrap();
        assert_eq!(add.kind, OperationKind::Document);
        assert_eq!(add.params()[0].ty, ParamType::Secret);
        assert_eq!(add.contract.permission, PermissionRequirement::Write);
        assert_eq!(add.contract.risk, RiskLevel::Privileged);
        assert_eq!(add.contract.confirmation, ConfirmationPolicy::Confirm);

        let delete = account
            .actions
            .iter()
            .find(|action| action.id == "delete")
            .unwrap();
        assert_eq!(delete.kind, OperationKind::Document);
        assert_eq!(delete.contract.permission, PermissionRequirement::Write);
        assert_eq!(delete.contract.risk, RiskLevel::Destructive);
        assert_eq!(delete.contract.confirmation, ConfirmationPolicy::Confirm);
        assert!(delete.contract.reversible);
    }

    #[test]
    fn account_resource_labels_redact_secret_shaped_identity_fields() {
        let mut account = seed_accounts().remove(0);
        let secret = account.secret.clone();
        account.issuer = format!("Security key {secret}");
        account.label = secret.clone();
        let mut doc = AutoCommit::new();
        AuthDoc::sync_vault(&mut doc, &[account]);
        let resource = &provide_resources(&doc)[0].children[0];
        assert!(!resource.label.contains(&secret));
        assert!(
            resource
                .actions
                .iter()
                .all(|action| !action.label.contains(&secret))
        );
    }

    /// Ctrl+K on the Authenticator yields the vault resource with one child per
    /// account, each exporting the full account action set. Drives the real
    /// `Widget::resources` hook through the demo app's live document.
    #[test]
    fn ctrl_k_publishes_the_vault_and_account_actions() {
        let mut app = AuthenticatorApp::demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root); // seeds the demo vault into the doc

        let resources = Widget::resources(&app, &state.doc);
        assert_eq!(resources.len(), 1, "one open vault");
        let vault = &resources[0];
        assert_eq!(vault.kind, "totp.vault");
        assert!(
            vault.actions.iter().any(|a| a.id == "add-account"),
            "the vault exports add-account"
        );
        let add = vault
            .actions
            .iter()
            .find(|action| action.id == "add-account")
            .unwrap();
        assert_eq!(add.params()[0].ty, ParamType::Secret);
        assert!(
            vault.children.is_empty(),
            "the contextual root stays bounded"
        );
        let account = AuthDoc::list_accounts(&state.doc)
            .into_iter()
            .next()
            .expect("demo account");
        let acct = Widget::resource(&app, &state.doc, &account_resource_id(account.id))
            .expect("account lazily resolves");
        assert_eq!(acct.kind, "totp.account");
        let ids: Vec<&str> = acct.actions.iter().map(|a| a.id.as_str()).collect();
        for want in [
            "open",
            "copy-code",
            "rename",
            "relabel",
            "set-color",
            "move-up",
            "move-down",
            "delete",
        ] {
            assert!(ids.contains(&want), "account exports {want}: {ids:?}");
        }
        assert!(matches!(
            acct.actions
                .iter()
                .find(|action| action.id == "move-up")
                .expect("move-up")
                .contract
                .availability,
            Availability::Unavailable { .. }
        ));

        // The one palette surfaces the tree's actions as rows.
        let mut palette = ui::ActionPalette::new();
        let mut palette_resources = resources.clone();
        palette_resources.push(acct.clone());
        palette.load_open_resources(&palette_resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Add account")),
            "{labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("Copy current code")),
            "{labels:?}"
        );
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// mutates the doc and records exactly one undo boundary, and Ctrl+Z (the
    /// host's `undo`) reverts precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let mut state = ui::State::new();
        let accts = seed_accounts();
        AuthDoc::sync_vault(&mut state.doc, &accts);
        let before_heads = state.doc.get_heads();
        let before_len = AuthDoc::list_accounts(&state.doc).len();
        let target = accts[0].id;

        // Delete an account via the action + the host's undo recording.
        let action = delete_account_action(target);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert_eq!(
            AuthDoc::list_accounts(&state.doc).len(),
            before_len - 1,
            "the action removed one account from the document"
        );
        assert!(
            AuthDoc::list_accounts(&state.doc)
                .iter()
                .all(|a| a.id != target),
            "the targeted account is gone"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        // Ctrl+Z reverts exactly this action.
        state.undo(None, None).expect("an undo entry to pop");
        assert_eq!(
            AuthDoc::list_accounts(&state.doc).len(),
            before_len,
            "undo restored the deleted account and nothing else"
        );
        assert!(
            AuthDoc::list_accounts(&state.doc)
                .iter()
                .any(|a| a.id == target),
            "the exact account is back after undo"
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a second
    /// independent replica (from the same base state) lands on the byte-identical
    /// document — same head hashes, not merely the same values.
    #[test]
    fn add_account_replays_byte_identical_on_a_peer() {
        let mut a = seeded_doc();
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let new = Account {
            id: Uuid::from_u128(0x0BAD_0000_0000_0000_0000_0000_0000_FEED),
            issuer: "Proton".into(),
            label: "ada@pm.me".into(),
            secret: "JBSWY3DPEHPK3PXP".into(),
            algorithm: Algorithm::Sha1,
            digits: 6,
            period: 30,
            color: 3,
            order: 5,
        };
        let action = add_account_action(&new, 5);
        let encoded = encode(&action);

        // A applies locally, producing the envelope; B replays it.
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert!(
            AuthDoc::list_accounts(&a).iter().any(|x| x.id == new.id),
            "the account landed on the origin"
        );
        assert_eq!(
            AuthDoc::list_accounts(&a).len(),
            AuthDoc::list_accounts(&b).len(),
            "both replicas hold the same account set"
        );
    }

    /// Single source of truth: the account resource's immediate "Delete" spec and
    /// the prompted "Rename" spec encode byte-identically to their direct builders
    /// — proving the on-screen affordances and the palette dispatch the same action.
    #[test]
    fn palette_and_builder_encode_identically() {
        let doc = seeded_doc();
        let accts = seed_accounts();
        let resources = provide_resources(&doc);
        let vault = &resources[0];
        let acct = vault
            .children
            .iter()
            .find(|r| r.id.as_str() == format!("totp.account:{}", accts[0].id))
            .expect("the first account is a resource");

        // Immediate delete spec == delete builder.
        let del = acct.actions.iter().find(|a| a.id == "delete").unwrap();
        let from_spec = immediate_encode(del);
        let from_builder = encode(&delete_account_action(accts[0].id));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "one source of truth for delete"
        );

        // Immediate move-down spec == move-down builder.
        let mv = acct.actions.iter().find(|a| a.id == "move-down").unwrap();
        assert_eq!(
            immediate_encode(mv).payload,
            encode(&move_down_action(accts[0].id)).payload,
            "one source of truth for reorder"
        );

        // Prompted rename spec == rename builder for the same argument.
        let rn = acct.actions.iter().find(|a| a.id == "rename").unwrap();
        let from_spec = prompted_encode(rn, "NewName");
        let from_builder = encode(&set_issuer_action(accts[0].id, "NewName"));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "one source of truth for rename"
        );
    }

    /// The vault's prompted "Add account" builds an action that, dispatched,
    /// parses the pasted `otpauth://` link and inserts the account into the doc.
    #[test]
    fn add_account_prompt_parses_otpauth_and_inserts() {
        let mut doc = AutoCommit::new();
        let resources = provide_resources(&doc);
        let add = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "add-account")
            .unwrap();
        let encoded = prompted_encode(
            add,
            "otpauth://totp/Proton:ada@pm.me?secret=JBSWY3DPEHPK3PXP&issuer=Proton",
        );
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

        let accts = AuthDoc::list_accounts(&doc);
        assert_eq!(accts.len(), 1, "the pasted link added exactly one account");
        assert_eq!(accts[0].issuer, "Proton");
        assert_eq!(accts[0].secret, "JBSWY3DPEHPK3PXP");
    }

    /// Reorder is deterministic against the document: `MoveDown` swaps the first
    /// two accounts' order values, applied straight to the doc.
    #[test]
    fn move_down_action_swaps_order_in_the_doc() {
        let mut doc = seeded_doc();
        let accts = seed_accounts();
        let (first, second) = (accts[0].id, accts[1].id);
        assert_eq!(
            AuthDoc::list_accounts(&doc)[0].id,
            first,
            "first starts on top"
        );

        let action = move_down_action(first);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

        let ordered = AuthDoc::list_accounts(&doc);
        assert_eq!(ordered[0].id, second, "the second account moved to the top");
        assert_eq!(
            ordered[1].id, first,
            "the first account moved down one slot"
        );
    }

    /// A malformed `otpauth://` paste is refused during binding, before a
    /// replayable action or half-formed account can exist.
    #[test]
    fn malformed_add_link_is_refused_before_dispatch() {
        let doc = AutoCommit::new();
        let resources = provide_resources(&doc);
        let add = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "add-account")
            .unwrap();
        let encoded = prompted_encode(add, "otpauth://totp/broken");
        assert_eq!(
            encoded.type_id,
            qos_actions::resource::INVALID_ARGUMENTS_TYPE
        );
        assert!(
            AuthDoc::list_accounts(&doc).is_empty(),
            "no partial account was inserted"
        );
    }
}
