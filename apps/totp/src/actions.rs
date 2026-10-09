//! Authenticator's document **actions** and the account rows it publishes.
//!
//! Every change to the shared account metadata — adding an account identity,
//! deleting one, renaming or relabelling it, recolouring it, changing its
//! code parameters, or reordering the list — is a [`TotpAction`]: a small
//! serializable value that deterministically mutates the shared CRDT. The
//! on-screen affordances and the published account actions build the same
//! values, submitted through the framework's dispatcher.
//!
//! Presentation stays out of the document and never becomes an action: the
//! live rotating TOTP code and its countdown ring are *computed* from the
//! account's secret and the wall-clock every frame, and so are copy, search
//! and selection.
//!
//! # Secret boundary
//! The base32 key never appears in an action, a published resource, a search
//! entry or the Automerge document. It stays in the app's secret store on
//! this device (`crate::secrets`). Collaboration organises and locates
//! account identities, while generating or copying a code needs a key that is
//! present on this device.

use serde::{Deserialize, Serialize};

use tilo_actions::{
    Action, ActionError, ActionParam, ActionRegistry, Availability, Doc, ExecutionLocus, ParamType,
    Resource, RiskLevel,
};
use ui::tilo_resource::{ActionDecl, declaration_spec};

use crate::doc::AuthDoc;
use crate::model::{Account, AccountId, Algorithm, contains_secret_shaped_text};
use crate::resource::Totp;

/// Registry `type_id` for every Authenticator action.
pub const TOTP_ACTION_TYPE: &str = "totp.action";

/// Stable resource id for one authenticator account. It contains only the
/// account UUID; neither the long-lived secret nor the rotating code is ever
/// placed in an address or label.
pub fn account_resource_id(id: AccountId) -> String {
    format!("totp.account:{id}")
}

/// Every collaborative operation the vault exports on its account store.
///
/// Determinism contract: ids are minted at *authoring* time and carried in the
/// payload, never inside [`Action::apply`], which must not touch the RNG or the
/// wall-clock. A malformed payload (unparseable id) is a **no-op** rather than
/// an error, so a bad replay can't wedge a peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TotpAction {
    /// Add an account. Every parameter needed to reproduce the record —
    /// including the pre-computed `order`/`color` — is carried so replay
    /// lands the byte-identical row on every peer.
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
    /// Change how an account's codes are computed.
    SetParams {
        id: String,
        algorithm: String,
        digits: u8,
        period: u32,
    },
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
            TotpAction::SetParams {
                id,
                algorithm,
                digits,
                period,
            } => {
                if let Ok(uuid) = id.parse::<AccountId>() {
                    AuthDoc::set_params(
                        doc,
                        uuid,
                        Algorithm::parse(algorithm),
                        (*digits).clamp(4, 10),
                        (*period).clamp(5, 300),
                    );
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

    /// Deleting an account also forgets its secret on every device that
    /// held it, so it asks first wherever it comes from.
    fn risk(&self) -> RiskLevel {
        match self {
            TotpAction::DeleteAccount { .. } => RiskLevel::Destructive,
            _ => RiskLevel::Low,
        }
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

/// Register the Authenticator action decoder for replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<TotpAction>(TOTP_ACTION_TYPE);
}

// ===========================================================================
// Action builders — shared by the on-screen affordances and the published
// account actions.
// ===========================================================================

/// Build an [`TotpAction::AddAccount`] from a resolved account plus its
/// authored `order`. `color` and `order` are baked in at authoring time so
/// replay is deterministic. The secret is never part of it.
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

pub fn set_params_action(acc: &Account) -> TotpAction {
    TotpAction::SetParams {
        id: acc.id.to_string(),
        algorithm: acc.algorithm.as_str().to_string(),
        digits: acc.digits,
        period: acc.period,
    }
}

pub fn move_up_action(id: AccountId) -> TotpAction {
    TotpAction::MoveUp { id: id.to_string() }
}

pub fn move_down_action(id: AccountId) -> TotpAction {
    TotpAction::MoveDown { id: id.to_string() }
}

/// The granular edits that turn `before` into `after`, one per field that
/// changed, so an edit never overwrites what a collaborator changed in
/// another field meanwhile.
pub fn edit_actions(before: &Account, after: &Account) -> Vec<TotpAction> {
    let mut edits = Vec::new();
    if before.issuer != after.issuer {
        edits.push(set_issuer_action(after.id, &after.issuer));
    }
    if before.label != after.label {
        edits.push(set_label_action(after.id, &after.label));
    }
    if before.color != after.color {
        edits.push(set_color_action(after.id, after.color));
    }
    if (before.algorithm, before.digits, before.period)
        != (after.algorithm, after.digits, after.period)
    {
        edits.push(set_params_action(after));
    }
    edits
}

// ===========================================================================
// Published accounts
// ===========================================================================

/// One-line palette label for an account resource, never echoing a secret
/// someone typed into a name.
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

pub(crate) fn account_label(a: &Account) -> String {
    let issuer = safe_identity_field(&a.issuer, &a.secret);
    let label = safe_identity_field(&a.label, &a.secret);
    match (issuer.is_empty(), label.is_empty()) {
        (false, false) => format!("{issuer} — {label}"),
        (false, true) => issuer.to_string(),
        (true, false) => label.to_string(),
        (true, true) => "Account".to_string(),
    }
}

fn unavailable(code: &str, reason: &str) -> Availability {
    Availability::Unavailable {
        code: code.into(),
        reason: reason.into(),
    }
}

/// One account as the resource publishes it: its document actions with the
/// account already bound. What only this device knows — whether it holds the
/// account's secret, and so copying a code — joins it from the view as a live
/// row of the same id.
pub fn account_resource(acc: &Account, index: usize, account_count: usize) -> Resource {
    let id = acc.id;
    let rename = ActionDecl::<Totp>::new(
        "rename",
        "Rename",
        vec![ActionParam::required(
            "issuer",
            ParamType::Text,
            "Account issuer",
        )],
        move |args, _| {
            Ok(set_issuer_action(
                id,
                args.text("issuer").unwrap_or_default().trim(),
            ))
        },
    );
    let relabel = ActionDecl::<Totp>::new(
        "relabel",
        "Relabel account",
        vec![ActionParam::required(
            "label",
            ParamType::Text,
            "Account label",
        )],
        move |args, _| {
            Ok(set_label_action(
                id,
                args.text("label").unwrap_or_default().trim(),
            ))
        },
    );
    let current = acc.color;
    let recolour = ActionDecl::<Totp>::new(
        "set-color",
        "Set colour",
        vec![
            ActionParam::required("colour", ParamType::Integer, "Colour index")
                .with_schema(serde_json::json!({ "minimum": 0, "maximum": 6 })),
        ],
        move |args, _| {
            Ok(set_color_action(
                id,
                args.integer("colour")
                    .and_then(|value| u8::try_from(value).ok())
                    .unwrap_or(current),
            ))
        },
    );
    let up = ActionDecl::<Totp>::new("move-up", "Move up", Vec::new(), move |_, _| {
        Ok(move_up_action(id))
    });
    let down = ActionDecl::<Totp>::new("move-down", "Move down", Vec::new(), move |_, _| {
        Ok(move_down_action(id))
    });
    let delete = ActionDecl::<Totp>::new("delete", "Delete account", Vec::new(), move |_, _| {
        Ok(delete_account_action(id))
    })
    .describe("Delete the account and forget its secret on every device")
    .destructive();

    let mut move_up = declaration_spec(&up);
    if index == 0 {
        move_up.contract.availability =
            unavailable("already_first", "This account is already first");
    }
    let mut move_down = declaration_spec(&down);
    if index + 1 >= account_count {
        move_down.contract.availability =
            unavailable("already_last", "This account is already last");
    }
    Resource::new(account_resource_id(id), "totp.account", account_label(acc))
        .with_action(declaration_spec(&rename))
        .with_action(declaration_spec(&relabel))
        .with_action(declaration_spec(&recolour))
        .with_action(move_up)
        .with_action(move_down)
        .with_action(declaration_spec(&delete))
}
