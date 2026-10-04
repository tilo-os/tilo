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
//!         ├── "algorithm" (Str)   # SHA1 / SHA256 / SHA512
//!         ├── "digits"    (Int)
//!         ├── "period"    (Int)
//!         ├── "color"     (Int)
//!         └── "order"     (Int)
//! ```
//!
//! Every function takes the borrowed `&mut AutoCommit` (the app's shared
//! `State::doc`) explicitly — the namespace is stateless. Fields are
//! plain scalars (last-writer-wins on concurrent edits). **Secrets are never
//! stored here.** This document is intentionally secret-free when replicated,
//! inspected, indexed and retained in history. It still contains potentially
//! private account identities, so normal collaboration access controls apply.
//! Each device keeps the base32 key material in its secret store
//! (`crate::secrets`).

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use ui::doc::{read_i64_coerced, read_str};

use crate::model::{Account, AccountId, Algorithm};

pub struct AuthDoc;

impl AuthDoc {
    /// Get-or-create the top-level `accounts` Map.
    ///
    /// # Panics
    /// Never in practice: `put_object` only fails when the target object id
    /// doesn't resolve to a document object, and `ROOT` always does.
    #[allow(
        clippy::expect_used,
        reason = "put_object on ROOT is infallible; see this function's # Panics doc"
    )]
    pub fn accounts_map(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "accounts") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "accounts", ObjType::Map)
                .expect("put_object accounts on root never fails"),
        }
    }

    /// Get-or-create the per-account Map for `id`.
    #[allow(
        clippy::expect_used,
        reason = "put_object on an existing map object is infallible; see AuthDoc::accounts_map's # Panics doc"
    )]
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
        let _ = doc.put(&obj, "algorithm", acc.algorithm.as_str().to_string());
        let _ = doc.put(&obj, "digits", i64::from(acc.digits));
        let _ = doc.put(&obj, "period", i64::from(acc.period));
        let _ = doc.put(&obj, "color", i64::from(acc.color));
        let _ = doc.put(&obj, "order", i64::from(acc.order));
    }

    /// Tombstone an account by id. Tombstones make a concurrent field update
    /// unable to resurrect a deleted credential; undo restores the preceding
    /// document snapshot, as usual for the host action system.
    pub fn delete_account(doc: &mut AutoCommit, id: AccountId) {
        let accounts = Self::accounts_map(doc);
        let _ = doc.delete(&accounts, id.to_string().as_str());
        let tombstones = Self::tombstones_map(doc);
        let _ = doc.put(&tombstones, id.to_string().as_str(), true);
    }

    #[allow(
        clippy::expect_used,
        reason = "put_object on ROOT is infallible; see accounts_map"
    )]
    fn tombstones_map(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "account_tombstones") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "account_tombstones", ObjType::Map)
                .expect("put_object tombstones on root never fails"),
        }
    }

    fn is_tombstoned(doc: &AutoCommit, id: AccountId) -> bool {
        let Ok(Some((Value::Object(ObjType::Map), tombstones))) =
            doc.get(&ROOT, "account_tombstones")
        else {
            return false;
        };
        matches!(
            doc.get(&tombstones, id.to_string().as_str()),
            Ok(Some((Value::Scalar(value), _))) if value.as_bool() == Some(true)
        )
    }

    /// The per-account Map for `id` **only if it already exists** — never
    /// created. A field set on a missing (e.g. concurrently deleted) account is
    /// then a clean no-op rather than resurrecting a half-formed record.
    fn existing_account(doc: &AutoCommit, id: AccountId) -> Option<ObjId> {
        let Ok(Some((Value::Object(ObjType::Map), accounts))) = doc.get(&ROOT, "accounts") else {
            return None;
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
    pub fn set_params(
        doc: &mut AutoCommit,
        id: AccountId,
        algorithm: Algorithm,
        digits: u8,
        period: u32,
    ) {
        if let Some(obj) = Self::existing_account(doc, id) {
            let _ = doc.put(&obj, "algorithm", algorithm.as_str().to_string());
            let _ = doc.put(&obj, "digits", i64::from(digits));
            let _ = doc.put(&obj, "period", i64::from(period));
        }
    }

    pub fn set_order(doc: &mut AutoCommit, id: AccountId, order: u32) {
        if let Some(obj) = Self::existing_account(doc, id) {
            let _ = doc.put(&obj, "order", i64::from(order));
        }
    }

    /// Read every account out of the document, sorted by `order`.
    /// Records with an unparseable id key are skipped.
    pub fn list_accounts(doc: &AutoCommit) -> Vec<Account> {
        let Ok(Some((Value::Object(ObjType::Map), accounts))) = doc.get(&ROOT, "accounts") else {
            return Vec::new();
        };

        let mut out = Vec::new();
        for key in doc.keys(&accounts) {
            let Ok(id) = key.parse::<AccountId>() else {
                continue;
            };
            if Self::is_tombstoned(doc, id) {
                continue;
            }
            let Ok(Some((Value::Object(ObjType::Map), obj))) = doc.get(&accounts, key.as_str())
            else {
                continue;
            };

            let issuer = read_str(doc, &obj, "issuer").unwrap_or_default();
            let label = read_str(doc, &obj, "label").unwrap_or_default();
            let algorithm = Algorithm::parse(&read_str(doc, &obj, "algorithm").unwrap_or_default());
            let digits = read_i64_coerced(doc, &obj, "digits")
                .unwrap_or(6)
                .clamp(4, 10) as u8;
            let period = read_i64_coerced(doc, &obj, "period")
                .unwrap_or(30)
                .clamp(5, 300) as u32;
            let color = read_i64_coerced(doc, &obj, "color")
                .unwrap_or(0)
                .clamp(0, 255) as u8;
            let order = read_i64_coerced(doc, &obj, "order").unwrap_or(0).max(0) as u32;

            out.push(Account {
                id,
                issuer,
                label,
                secret: String::new(),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::demo_accounts;
    use tilo_automerge::ApplyIncoming;

    /// The vault document round-trips metadata without secrets, lets tombstones win, and merges.
    #[test]
    fn vault_doc_merges_metadata_without_secrets_or_resurrection() {
        // Case `round_trips_metadata_without_a_secret`.
        {
            let mut doc = AutoCommit::new();
            let accounts = demo_accounts();
            for account in &accounts {
                AuthDoc::upsert_account(&mut doc, account);
            }
            let read = AuthDoc::list_accounts(&doc);
            assert_eq!(read.len(), accounts.len());
            // Order preserved.
            assert_eq!(read[0].issuer, accounts[0].issuer);
            assert!(
                read.iter().all(|account| account.secret.is_empty()),
                "the collaborative document contains metadata only"
            );
        }

        // Case `tombstone_wins_over_a_concurrent_metadata_update`.
        {
            let mut a = AutoCommit::new();
            let account = demo_accounts().remove(0);
            AuthDoc::upsert_account(&mut a, &account);
            a.commit();
            let mut b = AutoCommit::load(&a.save()).unwrap();

            AuthDoc::delete_account(&mut a, account.id);
            AuthDoc::set_label(&mut b, account.id, "updated elsewhere");
            a.merge_incoming(&mut b).unwrap();

            assert!(
                AuthDoc::list_accounts(&a)
                    .iter()
                    .all(|entry| entry.id != account.id),
                "a concurrent field update cannot resurrect a deleted account"
            );
        }

        // Case `two_docs_merge_accounts`.
        {
            let mut a = AutoCommit::new();
            let acc_a = demo_accounts();
            AuthDoc::upsert_account(&mut a, &acc_a[0]);

            let mut b = AutoCommit::load(&a.save()).unwrap();
            AuthDoc::upsert_account(&mut b, &acc_a[1]);

            let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
            a.merge_incoming(&mut b_loaded).unwrap();

            let issuers: Vec<_> = AuthDoc::list_accounts(&a)
                .into_iter()
                .map(|x| x.issuer)
                .collect();
            assert!(issuers.contains(&acc_a[0].issuer));
            assert!(issuers.contains(&acc_a[1].issuer));
        }
    }
}
