//! Automerge-backed collaborative configuration for the Backups app.
//!
//! The catalog is derived from the device-local storage backend and navigation,
//! selections, modal state, focus, and backend observations are local widget
//! state. The CRDT deliberately contains only durable policy that a peer can
//! safely converge and replay:
//!
//! ```text
//! root
//! ├── "ret_hourly"       (Int)
//! ├── "ret_daily"        (Int)
//! ├── "ret_weekly"       (Int)
//! └── "ret_monthly"      (Int)
//! ```
//!
//! Display getters tolerate a missing/foreign-typed key by returning the
//! authored default. History uses [`BackupsDoc::stored_retention`] so absence,
//! invalid content, conflicts, and exact stored integers remain distinct.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ReadDoc, Value};
use serde::{Deserialize, Serialize};

use crate::model::RetentionPolicy;

pub struct BackupsDoc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoredRetention {
    Absent,
    Int(i64),
    WrongType,
    Conflict,
}

impl BackupsDoc {
    // --- typed scalar readers -------------------------------------------

    pub fn stored_retention(doc: &AutoCommit, root: &ObjId, field: PolicyField) -> StoredRetention {
        let Ok(values) = doc.get_all(root, field.key()) else {
            return StoredRetention::WrongType;
        };
        match values.as_slice() {
            [] => StoredRetention::Absent,
            [(Value::Scalar(value), _)] => value
                .as_ref()
                .to_i64()
                .map_or(StoredRetention::WrongType, StoredRetention::Int),
            [_] => StoredRetention::WrongType,
            _ => StoredRetention::Conflict,
        }
    }

    // --- public typed accessors -----------------------------------------

    pub fn policy(doc: &AutoCommit, root: &ObjId) -> RetentionPolicy {
        RetentionPolicy {
            hourly: Self::retention(doc, root, PolicyField::Hourly),
            daily: Self::retention(doc, root, PolicyField::Daily),
            weekly: Self::retention(doc, root, PolicyField::Weekly),
            monthly: Self::retention(doc, root, PolicyField::Monthly),
        }
    }

    /// Read one retention field's stored count, clamped to `[0, 999]`. A missing
    /// or foreign-typed key falls back to the seeded default for that class, so
    /// this reads cleanly against a fresh (un-seeded) document.
    pub fn retention(doc: &AutoCommit, root: &ObjId, field: PolicyField) -> u32 {
        let d = RetentionPolicy::default();
        let default = i64::from(match field {
            PolicyField::Hourly => d.hourly,
            PolicyField::Daily => d.daily,
            PolicyField::Weekly => d.weekly,
            PolicyField::Monthly => d.monthly,
        });
        match Self::stored_retention(doc, root, field) {
            StoredRetention::Int(value) => value.clamp(0, 999) as u32,
            StoredRetention::Absent | StoredRetention::WrongType | StoredRetention::Conflict => {
                default as u32
            }
        }
    }

    /// Set one retention field to an absolute `count`, clamped to `[0, 999]`.
    /// This is the sole document write behind the retention-policy action (see
    /// `crate::actions`); the on-screen +/- affordances resolve their target
    /// count at authoring time and dispatch that absolute set.
    pub fn set_retention(doc: &mut AutoCommit, root: &ObjId, field: PolicyField, count: u32) {
        let _ = doc.put(root, field.key(), i64::from(count).clamp(0, 999));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyField {
    Hourly,
    Daily,
    Weekly,
    Monthly,
}

impl PolicyField {
    pub const ALL: [PolicyField; 4] = [
        PolicyField::Hourly,
        PolicyField::Daily,
        PolicyField::Weekly,
        PolicyField::Monthly,
    ];

    pub fn key(self) -> &'static str {
        match self {
            PolicyField::Hourly => "ret_hourly",
            PolicyField::Daily => "ret_daily",
            PolicyField::Weekly => "ret_weekly",
            PolicyField::Monthly => "ret_monthly",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PolicyField::Hourly => "Hourly",
            PolicyField::Daily => "Daily",
            PolicyField::Weekly => "Weekly",
            PolicyField::Monthly => "Monthly",
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            PolicyField::Hourly => "last 24 hours",
            PolicyField::Daily => "past days",
            PolicyField::Weekly => "past weeks",
            PolicyField::Monthly => "past months",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ROOT;

    #[test]
    fn empty_doc_reads_default_without_seed() {
        let doc = AutoCommit::new();
        assert_eq!(BackupsDoc::policy(&doc, &ROOT), RetentionPolicy::default());
    }

    #[test]
    fn set_retention_clamps_and_round_trips() {
        let mut doc = AutoCommit::new();
        // Over-large values clamp to the ceiling; the getter agrees with policy().
        BackupsDoc::set_retention(&mut doc, &ROOT, PolicyField::Daily, 5000);
        assert_eq!(BackupsDoc::retention(&doc, &ROOT, PolicyField::Daily), 999);
        assert_eq!(BackupsDoc::policy(&doc, &ROOT).daily, 999);
        // An ordinary set round-trips exactly.
        BackupsDoc::set_retention(&mut doc, &ROOT, PolicyField::Daily, 5);
        assert_eq!(BackupsDoc::retention(&doc, &ROOT, PolicyField::Daily), 5);
    }

    #[test]
    fn retention_reads_default_before_seed() {
        // The action's change-guard reads a field's current value even against a
        // fresh doc; it must fall back to the class default, not 0.
        let doc = AutoCommit::new();
        assert_eq!(BackupsDoc::retention(&doc, &ROOT, PolicyField::Daily), 7);
        assert_eq!(BackupsDoc::retention(&doc, &ROOT, PolicyField::Hourly), 24);
    }
}
