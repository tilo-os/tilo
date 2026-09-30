//! Authenticator's exported **actions** and the **resource tree** it publishes
//! to Ctrl+K.
//!
//! Every document-mutating operation the shared account metadata performs —
//! adding an account identity, deleting one, renaming or relabelling it,
//! recolouring it, or reordering the
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
//! # Secret boundary
//! The base32 key deliberately never appears in this module's encoded actions,
//! resources, search records or Automerge document. It remains in the
//! encrypted, device-local vault. Collaboration can therefore organise and
//! locate account identities, while generating/copying a code requires a key
//! that is actually present on this device.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use uuid::Uuid;

use tilo_actions::{
    Action, ActionAddress, ActionContract, ActionError, ActionParam, ActionRegistry, ActionSpec,
    Availability, ConfirmationPolicy, Doc, EncodedAction, ExecutionLocus, OperationKind, ParamType,
    PermissionRequirement, Resource, ResourcePresentation, RetryPolicy, RiskLevel,
};

use crate::doc::AuthDoc;
use crate::model::{Account, AccountId, Algorithm, contains_secret_shaped_text};

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
    OpenAddAccount,
    SetUpLocal { id: String },
    RetryPersist,
    OpenAccount { id: String },
    CopyCode { id: String },
}

/// # Errors
/// Returns an error when a payload for this operation type is malformed.
pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<TotpOperation>, String> {
    if type_id != TOTP_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| format!("invalid Authenticator operation: {error}"))
}

#[allow(
    clippy::expect_used,
    reason = "stable ids and enum payloads are JSON serializable"
)]
fn account_operation(
    id: &str,
    label: &str,
    kind: OperationKind,
    operation: &TotpOperation,
) -> ActionSpec {
    ActionSpec::operation(id, label, kind, TOTP_OPERATION_TYPE, operation)
        .expect("Authenticator account ids are JSON serializable")
}

/// Local, bounded retry of an already-staged encrypted-vault write. It carries
/// no account fields and never enters the collaborative document.
#[allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "a fixed enum payload and literal identifiers are serializable"
)]
pub fn retry_persist_operation() -> ActionSpec {
    ActionSpec::operation(
        "retry-save",
        "Retry saving local vault",
        OperationKind::Effect,
        TOTP_OPERATION_TYPE,
        &TotpOperation::RetryPersist,
    )
    .expect("fixed Authenticator retry operation is serializable")
    .with_contract(ActionContract {
        permission: PermissionRequirement::Write,
        retry: RetryPolicy::AtMostOnce,
        tags: vec!["security".into(), "local-vault".into()],
        ..ActionContract::default()
    })
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
    /// including the pre-computed `order`/`color` — is
    /// carried so replay lands the byte-identical row on every peer.
    AddAccount {
        id: String,
        issuer: String,
        label: String,
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
                algorithm,
                digits,
                period,
                color,
                order,
            } => {
                if contains_secret_shaped_text(issuer) || contains_secret_shaped_text(label) {
                    return Ok(());
                }
                let Ok(uuid) = id.parse::<AccountId>() else {
                    return Ok(());
                };
                let acc = Account {
                    id: uuid,
                    issuer: issuer.clone(),
                    label: label.clone(),
                    secret: String::new(),
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
                if !contains_secret_shaped_text(issuer)
                    && let Ok(uuid) = id.parse::<AccountId>()
                {
                    AuthDoc::set_issuer(doc, uuid, issuer);
                }
            }
            TotpAction::SetLabel { id, label } => {
                if !contains_secret_shaped_text(label)
                    && let Ok(uuid) = id.parse::<AccountId>()
                {
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
pub fn provide_contextual_resources(_accounts: &[Account]) -> Vec<Resource> {
    let mut root = Resource::new("totp.vault", "totp.vault", "Accounts");

    // Adding a secret opens the local, protected form. A setup URI is never
    // accepted as a generic resource/action argument because resource payloads
    // are inspectable and durable; the form stores the secret directly in the
    // encrypted local vault and dispatches only its metadata action afterward.
    root = root.with_action(
        account_operation(
            "add-account",
            "Add account",
            OperationKind::View,
            &TotpOperation::OpenAddAccount,
        )
        .with_contract(ActionContract::view()),
    );

    vec![root]
}

/// Materialise one account after search names its stable resource id.
#[allow(
    clippy::too_many_lines,
    reason = "one resource's complete action contract stays co-located"
)]
pub fn provide_account_resource(
    acc: &Account,
    index: usize,
    account_count: usize,
    has_local_secret: bool,
) -> Resource {
    let id = acc.id;
    let cur_color = acc.color;
    let mut child = Resource::new(account_resource_id(id), "totp.account", account_label(acc))
        .with_presentation(ResourcePresentation {
            detail: Some(if has_local_secret {
                "Codes available on this device".into()
            } else {
                "Local setup required to generate codes".into()
            }),
            badge: Some(if has_local_secret {
                "Ready".into()
            } else {
                "Set up".into()
            }),
            preview: None,
        });

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
            availability: if has_local_secret {
                Availability::Available
            } else {
                Availability::Unavailable {
                    code: "local_secret_required".into(),
                    reason: "Set up this account on this device before copying a code".into(),
                }
            },
            permission: PermissionRequirement::Read,
            risk: RiskLevel::Privileged,
            confirmation: ConfirmationPolicy::Confirm,
            retry: RetryPolicy::AtMostOnce,
            tags: vec!["security".into(), "clipboard".into()],
            ..ActionContract::default()
        }),
    );
    if !has_local_secret {
        child = child.with_action(
            account_operation(
                "set-up-local",
                "Set up on this device",
                OperationKind::View,
                &TotpOperation::SetUpLocal { id: id.to_string() },
            )
            .with_contract(ActionContract::view()),
        );
    }

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
    vec![ui::tilo_resource::publish::<crate::resource::Totp>(
        "totp.vault",
        "Accounts",
        &accounts,
    )]
}

// Inherent resource publication (was ResourceProvider)
impl crate::app::AuthenticatorApp {
    pub fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        let mut roots = provide_resources(doc);
        let Some(root) = roots.first_mut() else {
            return roots;
        };
        let accounts = AuthDoc::list_accounts(doc);
        let count = accounts.len();
        root.children.retain(|child| child.kind != "totp.account");
        root.children
            .extend(accounts.iter().enumerate().map(|(index, account)| {
                provide_account_resource(account, index, count, self.has_local_secret(account.id))
            }));
        roots
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AuthenticatorApp;
    use tilo_actions::LocalTransport;
    use ui::testing::action_contract::spec_encoding;
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
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

    /// Vault resources keep navigation, effects, secrets and deletion distinct and never carry an account secret.
    #[test]
    fn vault_resources_keep_secrets_out_and_export_distinct_contracts() {
        // Case `resource_contracts_keep_navigation_effects_secrets_and_deletion_distinct`.
        {
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
            let encoded_copy = spec_encoding(copy, None);
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
            assert_eq!(add.kind, OperationKind::View);
            assert_eq!(add.contract, ActionContract::view());
            let encoded_add = spec_encoding(add, None);
            assert!(matches!(
                serde_json::from_slice::<TotpOperation>(&encoded_add.payload).unwrap(),
                TotpOperation::OpenAddAccount
            ));

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

        // Case `shared_resource_tree_never_contains_account_secrets`.
        {
            let account = seed_accounts().remove(0);
            let secret = account.secret.clone();
            let mut doc = AutoCommit::new();
            AuthDoc::sync_vault(&mut doc, &[account]);
            let bytes = doc.save();
            assert!(
                !bytes
                    .windows(secret.len())
                    .any(|window| window == secret.as_bytes()),
                "the CRDT never retains base32 key material"
            );
            let resource = &provide_resources(&doc)[0].children[0];
            assert!(!resource.label.contains(&secret));
            assert!(
                resource
                    .actions
                    .iter()
                    .all(|action| !action.label.contains(&secret))
            );
        }

        // Case `add_metadata_action_redacts_the_source_secret`.
        // A metadata action contains no secret even when its source account does.
        {
            let account = seed_accounts().remove(0);
            let secret = account.secret.clone();
            let encoded = encode(&add_account_action(&account, account.order));
            assert!(
                !encoded
                    .payload
                    .windows(secret.len())
                    .any(|window| window == secret.as_bytes()),
                "the action contains only replicated metadata"
            );
        }
    }

    /// Ctrl+K publishes the vault/account actions, add opens the local protected form, and `MoveDown` reorders the document.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn vault_actions_publish_local_forms_and_reorder_the_doc() {
        // Case `ctrl_k_publishes_the_vault_and_account_actions`.
        // Ctrl+K on the Authenticator yields the vault resource with one child per
        // account, each exporting the full account action set. Drives the real
        // `Widget::resources` hook through the demo app's live document.
        {
            let mut app = AuthenticatorApp::demo();
            let mut state = ui::State::new();
            app.sync(&mut state.doc, &state.root); // seeds the demo vault into the doc

            let resources = ui::resources::resources(&app, &state.doc);
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
            assert_eq!(add.kind, OperationKind::View);
            assert!(add.params().is_empty());
            assert!(
                vault
                    .children
                    .iter()
                    .any(|child| child.kind == "totp.account"),
                "accounts ride the vault door"
            );
            let account = AuthDoc::list_accounts(&state.doc)
                .into_iter()
                .next()
                .expect("demo account");
            let acct = ui::resources::resource(&app, &state.doc, &account_resource_id(account.id))
                .expect("account lazily resolves");
            assert_eq!(acct.kind, "totp.account");
            assert_eq!(
                acct.presentation
                    .as_ref()
                    .and_then(|value| value.badge.as_deref()),
                Some("Ready")
            );
            let presentation =
                serde_json::to_string(&acct.presentation).expect("presentation encodes");
            assert!(
                !contains_secret_shaped_text(&presentation),
                "resource presentation must never expose authenticator secrets"
            );
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
        }

        // Case `add_account_opens_local_protected_form_without_secret_payload`.
        // The vault's add entry is a local View route: secret-bearing setup links
        // are handled only by the encrypted local form, never a serialised action.
        {
            let doc = AutoCommit::new();
            let resources = provide_resources(&doc);
            let add = resources[0]
                .actions
                .iter()
                .find(|a| a.id == "add-account")
                .unwrap();
            let encoded = spec_encoding(add, None);
            assert!(matches!(
                serde_json::from_slice::<TotpOperation>(&encoded.payload).unwrap(),
                TotpOperation::OpenAddAccount
            ));
            assert!(
                !encoded
                    .payload
                    .windows(16)
                    .any(|window| window == b"JBSWY3DPEHPK3PXP"),
                "an add route contains no credential"
            );
        }

        // Case `move_down_action_swaps_order_in_the_doc`.
        // Reorder is deterministic against the document: `MoveDown` swaps the first
        // two accounts' order values, applied straight to the doc.
        {
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
            tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

            let ordered = AuthDoc::list_accounts(&doc);
            assert_eq!(ordered[0].id, second, "the second account moved to the top");
            assert_eq!(
                ordered[1].id, first,
                "the first account moved down one slot"
            );
        }
    }

    fn first_account() -> AccountId {
        seed_accounts()[0].id
    }

    fn first_account_resource() -> String {
        account_resource_id(first_account())
    }

    ui::action_contract_tests! {
        seed: seeded_doc(),
        registry: registry(),
        resources: |doc| provide_resources(doc),
        palette: ["Add account", "Relabel account", "Delete account", "Move down"],
        replay: [
            add_account => |_| encode(&add_account_action(
                &Account {
                    id: Uuid::from_u128(0x0BAD_0000_0000_0000_0000_0000_0000_FEED),
                    issuer: "Proton".into(),
                    label: "ada@pm.me".into(),
                    secret: "JBSWY3DPEHPK3PXP".into(),
                    algorithm: Algorithm::Sha1,
                    digits: 6,
                    period: 30,
                    color: 3,
                    order: 5,
                },
                5,
            )),
        ],
        undo: [
            delete_account => |_| encode(&delete_account_action(first_account())),
        ],
        encodes: [
            delete => (&first_account_resource(), "delete", |_, _| {
                encode(&delete_account_action(first_account()))
            }),
            move_down => (&first_account_resource(), "move-down", |_, _| {
                encode(&move_down_action(first_account()))
            }),
            rename => (&first_account_resource(), "rename", arg: "NewName", |_, _| {
                encode(&set_issuer_action(first_account(), "NewName"))
            }),
            relabel => (&first_account_resource(), "relabel", arg: "ada@corp.example", |_, _| {
                encode(&set_label_action(first_account(), "ada@corp.example"))
            }),
        ],
    }
}
