//! Automerge document model for the Authenticator vault.
//!
//! The account list is CRDT-native so undo/redo (full-doc snapshots in
//! `ui::State`) and live collaboration work without an extra layer:
//!
//! ```text
//! root
//! └── "accounts" (Map, keyed by AccountId string)
//!     └── <account-id> (Map)
//!         ├── "issuer"    (Str)
//!         ├── "label"     (Str)
//!         ├── "secret"    (Str)   # base32, normalised
//!         ├── "algorithm" (Str)   # SHA1 / SHA256 / SHA512
//!         ├── "digits"    (Int)
//!         ├── "period"    (Int)
//!         ├── "color"     (Int)
//!         └── "order"     (Int)
//! ```
//!
//! Every function takes the borrowed `&mut AutoCommit` (the app's shared
//! `State::doc`) explicitly — the namespace is stateless. Fields are
//! plain scalars (last-writer-wins on concurrent edits), which is the
//! right semantics for credential parameters: you never want two halves
//! of a merged secret.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};

use crate::model::{Account, AccountId, Algorithm};

pub struct AuthDoc;

impl AuthDoc {
    /// Get-or-create the top-level `accounts` Map.
    pub fn accounts_map(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "accounts") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "accounts", ObjType::Map)
                .expect("put_object accounts on root never fails"),
        }
    }

    /// Get-or-create the per-account Map for `id`.
    fn ensure_account(doc: &mut AutoCommit, id: AccountId) -> ObjId {
        let accounts = Self::accounts_map(doc);
        let key = id.to_string();
        match doc.get(&accounts, key.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), aid))) => aid,
            _ => doc
                .put_object(&accounts, key.as_str(), ObjType::Map)
                .expect("put_object account map never fails"),
        }
    }

    /// Insert or update a full account record.
    pub fn upsert_account(doc: &mut AutoCommit, acc: &Account) {
        let obj = Self::ensure_account(doc, acc.id);
        let _ = doc.put(&obj, "issuer", acc.issuer.clone());
        let _ = doc.put(&obj, "label", acc.label.clone());
        let _ = doc.put(&obj, "secret", acc.secret.clone());
        let _ = doc.put(&obj, "algorithm", acc.algorithm.as_str().to_string());
        let _ = doc.put(&obj, "digits", i64::from(acc.digits));
        let _ = doc.put(&obj, "period", i64::from(acc.period));
        let _ = doc.put(&obj, "color", i64::from(acc.color));
        let _ = doc.put(&obj, "order", i64::from(acc.order));
    }

    /// Delete an account by id. Idempotent.
    pub fn delete_account(doc: &mut AutoCommit, id: AccountId) {
        let accounts = Self::accounts_map(doc);
        let _ = doc.delete(&accounts, id.to_string().as_str());
    }

    /// The per-account Map for `id` **only if it already exists** — never
    /// created. A field set on a missing (e.g. concurrently deleted) account is
    /// then a clean no-op rather than resurrecting a half-formed record.
    fn existing_account(doc: &AutoCommit, id: AccountId) -> Option<ObjId> {
        let accounts = match doc.get(&ROOT, "accounts") {
            Ok(Some((Value::Object(ObjType::Map), aid))) => aid,
            _ => return None,
        };
        match doc.get(&accounts, id.to_string().as_str()) {
            Ok(Some((Value::Object(ObjType::Map), obj))) => Some(obj),
            _ => None,
        }
    }

    /// Rewrite one account's issuer (its display title). No-op if absent.
    pub fn set_issuer(doc: &mut AutoCommit, id: AccountId, issuer: &str) {
        if let Some(obj) = Self::existing_account(doc, id) {
            let _ = doc.put(&obj, "issuer", issuer.to_string());
        }
    }

    /// Rewrite one account's label. No-op if absent.
    pub fn set_label(doc: &mut AutoCommit, id: AccountId, label: &str) {
        if let Some(obj) = Self::existing_account(doc, id) {
            let _ = doc.put(&obj, "label", label.to_string());
        }
    }

    /// Rewrite one account's palette colour index. No-op if absent.
    pub fn set_color(doc: &mut AutoCommit, id: AccountId, color: u8) {
        if let Some(obj) = Self::existing_account(doc, id) {
            let _ = doc.put(&obj, "color", i64::from(color));
        }
    }

    /// Rewrite one account's list position. No-op if absent.
    pub fn set_order(doc: &mut AutoCommit, id: AccountId, order: u32) {
        if let Some(obj) = Self::existing_account(doc, id) {
            let _ = doc.put(&obj, "order", i64::from(order));
        }
    }

    /// Read every account out of the document, sorted by `order`.
    /// Records with an unparseable id key are skipped.
    pub fn list_accounts(doc: &AutoCommit) -> Vec<Account> {
        let accounts = match doc.get(&ROOT, "accounts") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return Vec::new(),
        };

        let mut out = Vec::new();
        for key in doc.keys(&accounts) {
            let Ok(id) = key.parse::<AccountId>() else {
                continue;
            };
            let Ok(Some((Value::Object(ObjType::Map), obj))) = doc.get(&accounts, key.as_str())
            else {
                continue;
            };

            let issuer = str_field(doc, &obj, "issuer");
            let label = str_field(doc, &obj, "label");
            let secret = str_field(doc, &obj, "secret");
            let algorithm = Algorithm::parse(&str_field(doc, &obj, "algorithm"));
            let digits = int_field(doc, &obj, "digits").unwrap_or(6).clamp(4, 10) as u8;
            let period = int_field(doc, &obj, "period").unwrap_or(30).clamp(5, 300) as u32;
            let color = int_field(doc, &obj, "color").unwrap_or(0).clamp(0, 255) as u8;
            let order = int_field(doc, &obj, "order").unwrap_or(0).max(0) as u32;

            out.push(Account {
                id,
                issuer,
                label,
                secret,
                algorithm,
                digits,
                period,
                color,
                order,
            });
        }
        out.sort_by_key(|a| a.order);
        out
    }

    /// Mirror an entire in-memory vault into the document: upserts every
    /// account and removes doc accounts that no longer exist locally.
    /// Idempotent — safe to call every frame.
    pub fn sync_vault(doc: &mut AutoCommit, accounts: &[Account]) {
        use std::collections::HashSet;
        let local: HashSet<AccountId> = accounts.iter().map(|a| a.id).collect();
        // Remove deleted.
        for existing in Self::list_accounts(doc) {
            if !local.contains(&existing.id) {
                Self::delete_account(doc, existing.id);
            }
        }
        // Upsert current.
        for a in accounts {
            Self::upsert_account(doc, a);
        }
    }
}

fn str_field(doc: &AutoCommit, obj: &ObjId, key: &str) -> String {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            automerge::ScalarValue::Str(s) => s.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

fn int_field(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<i64> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => v.to_i64(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Algorithm, demo_accounts};

    #[test]
    fn round_trips_a_full_account() {
        let mut doc = AutoCommit::new();
        let accounts = demo_accounts();
        AuthDoc::sync_vault(&mut doc, &accounts);
        let read = AuthDoc::list_accounts(&doc);
        assert_eq!(read.len(), accounts.len());
        // Order preserved.
        assert_eq!(read[0].issuer, accounts[0].issuer);
        // A SHA256 account round-trips its algorithm.
        let aws = read.iter().find(|a| a.issuer == "AWS").unwrap();
        assert_eq!(aws.algorithm, Algorithm::Sha256);
    }

    #[test]
    fn sync_vault_removes_deleted_accounts() {
        let mut doc = AutoCommit::new();
        let mut accounts = demo_accounts();
        AuthDoc::sync_vault(&mut doc, &accounts);
        let removed = accounts.remove(0);
        AuthDoc::sync_vault(&mut doc, &accounts);
        let read = AuthDoc::list_accounts(&doc);
        assert_eq!(read.len(), accounts.len());
        assert!(read.iter().all(|a| a.id != removed.id));
    }

    #[test]
    fn two_docs_merge_accounts() {
        let mut a = AutoCommit::new();
        let acc_a = demo_accounts();
        AuthDoc::upsert_account(&mut a, &acc_a[0]);

        let mut b = AutoCommit::load(&a.save()).unwrap();
        AuthDoc::upsert_account(&mut b, &acc_a[1]);

        let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
        a.merge(&mut b_loaded).unwrap();

        let issuers: Vec<_> = AuthDoc::list_accounts(&a)
            .into_iter()
            .map(|x| x.issuer)
            .collect();
        assert!(issuers.contains(&acc_a[0].issuer));
        assert!(issuers.contains(&acc_a[1].issuer));
    }
}
