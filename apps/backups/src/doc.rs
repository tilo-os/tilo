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
//! Every getter tolerates a missing/foreign-typed key by returning the
//! seeded default, so a fresh `State::new()` doc (the `--shots` path) reads
//! cleanly even before [`BackupsDoc::seed`] has run.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId};
use serde::{Deserialize, Serialize};
use ui::doc::read_i64_coerced;

use crate::model::RetentionPolicy;

pub struct BackupsDoc;

impl BackupsDoc {
    /// Write the default UI state into the document if (and only if) it has
    /// not been seeded yet. Idempotent: keys already present are left alone
    /// so a peer's adopted state is never clobbered.
    pub fn seed(doc: &mut AutoCommit, root: &ObjId) {
        let p = RetentionPolicy::default();
        Self::seed_int(doc, root, "ret_hourly", i64::from(p.hourly));
        Self::seed_int(doc, root, "ret_daily", i64::from(p.daily));
        Self::seed_int(doc, root, "ret_weekly", i64::from(p.weekly));
        Self::seed_int(doc, root, "ret_monthly", i64::from(p.monthly));
    }

    fn seed_int(doc: &mut AutoCommit, root: &ObjId, key: &str, val: i64) {
        if read_i64_coerced(doc, root, key).is_none() {
            let _ = doc.put(root, key, val);
        }
    }

    // --- public typed accessors -----------------------------------------

    pub fn policy(doc: &AutoCommit, root: &ObjId) -> RetentionPolicy {
        let d = RetentionPolicy::default();
        RetentionPolicy {
            hourly: read_i64_coerced(doc, root, "ret_hourly")
                .unwrap_or_else(|| i64::from(d.hourly))
                .max(0) as u32,
            daily: read_i64_coerced(doc, root, "ret_daily")
                .unwrap_or_else(|| i64::from(d.daily))
                .max(0) as u32,
            weekly: read_i64_coerced(doc, root, "ret_weekly")
                .unwrap_or_else(|| i64::from(d.weekly))
                .max(0) as u32,
            monthly: read_i64_coerced(doc, root, "ret_monthly")
                .unwrap_or_else(|| i64::from(d.monthly))
                .max(0) as u32,
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
        read_i64_coerced(doc, root, field.key())
            .unwrap_or(default)
            .clamp(0, 999) as u32
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
    use automerge::{ROOT, ReadDoc};

    /// The durable retention policy: defaults before and after seeding, seeding
    /// only durable fields and only once, and clamped round-tripping writes; each
    /// case uses a fresh document.
    #[test]
    fn retention_policy_reads_defaults_seeds_once_and_clamps_writes() {
        seed_then_read_defaults();
        seed_only_creates_durable_policy_fields();
        empty_doc_reads_default_without_seed();
        set_retention_clamps_and_round_trips();
        retention_reads_default_before_seed();
        seed_is_idempotent();
    }

    /// Case of `retention_policy_reads_defaults_seeds_once_and_clamps_writes`.
    fn seed_then_read_defaults() {
        let mut doc = AutoCommit::new();
        BackupsDoc::seed(&mut doc, &ROOT);
        assert_eq!(BackupsDoc::policy(&doc, &ROOT), RetentionPolicy::default());
    }

    /// Case of `retention_policy_reads_defaults_seeds_once_and_clamps_writes`.
    fn seed_only_creates_durable_policy_fields() {
        let mut doc = AutoCommit::new();
        BackupsDoc::seed(&mut doc, &ROOT);
        for transient in [
            "view",
            "sel_source",
            "sel_snapshot",
            "sel_dest",
            "restore_open",
        ] {
            assert!(doc.get(&ROOT, transient).unwrap().is_none(), "{transient}");
        }
    }

    /// Case of `retention_policy_reads_defaults_seeds_once_and_clamps_writes`.
    fn empty_doc_reads_default_without_seed() {
        // The --shots path syncs against a fresh doc; readers must not panic
        // and must fall back to defaults before seed runs.
        let doc = AutoCommit::new();
        assert_eq!(BackupsDoc::policy(&doc, &ROOT), RetentionPolicy::default());
    }

    /// Case of `retention_policy_reads_defaults_seeds_once_and_clamps_writes`.
    fn set_retention_clamps_and_round_trips() {
        let mut doc = AutoCommit::new();
        BackupsDoc::seed(&mut doc, &ROOT);
        // Over-large values clamp to the ceiling; the getter agrees with policy().
        BackupsDoc::set_retention(&mut doc, &ROOT, PolicyField::Daily, 5000);
        assert_eq!(BackupsDoc::retention(&doc, &ROOT, PolicyField::Daily), 999);
        assert_eq!(BackupsDoc::policy(&doc, &ROOT).daily, 999);
        // An ordinary set round-trips exactly.
        BackupsDoc::set_retention(&mut doc, &ROOT, PolicyField::Daily, 5);
        assert_eq!(BackupsDoc::retention(&doc, &ROOT, PolicyField::Daily), 5);
    }

    /// Case of `retention_policy_reads_defaults_seeds_once_and_clamps_writes`.
    fn retention_reads_default_before_seed() {
        // The action's change-guard reads a field's current value even against a
        // fresh doc; it must fall back to the class default, not 0.
        let doc = AutoCommit::new();
        assert_eq!(BackupsDoc::retention(&doc, &ROOT, PolicyField::Daily), 7);
        assert_eq!(BackupsDoc::retention(&doc, &ROOT, PolicyField::Hourly), 24);
    }

    /// Case of `retention_policy_reads_defaults_seeds_once_and_clamps_writes`.
    fn seed_is_idempotent() {
        let mut doc = AutoCommit::new();
        BackupsDoc::seed(&mut doc, &ROOT);
        BackupsDoc::set_retention(&mut doc, &ROOT, PolicyField::Daily, 42);
        BackupsDoc::seed(&mut doc, &ROOT); // must not reset a peer's policy
        assert_eq!(BackupsDoc::retention(&doc, &ROOT, PolicyField::Daily), 42);
    }
}
