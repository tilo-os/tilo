//! Automerge-backed UI / collaborative state for the Backups app.
//!
//! Only the *mutable, shareable* state lives here — the catalog (the
//! snapshot listing) is derived from the storage backend and is held in
//! memory (see `model::Catalog`). What goes in the CRDT is everything a
//! second peer (or undo/redo) should see converge:
//!
//! ```text
//! root
//! ├── "view"             (Str)  timeline | sources | destinations | schedules
//! ├── "sel_source"       (Str)  source id, or "" for "all sources"
//! ├── "sel_snapshot"     (Str)  snapshot id, or ""
//! ├── "sel_dest"         (Str)  destination id, or ""
//! ├── "restore_open"     (Bool)
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
use automerge::{AutoCommit, ObjId, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};

use crate::model::{RetentionPolicy, View};

pub struct BackupsDoc;

impl BackupsDoc {
    /// Write the default UI state into the document if (and only if) it has
    /// not been seeded yet. Idempotent: keys already present are left alone
    /// so a peer's adopted state is never clobbered.
    pub fn seed(doc: &mut AutoCommit, root: &ObjId) {
        if Self::get_str(doc, root, "view").is_none() {
            let _ = doc.put(root, "view", View::Timeline.key());
        }
        Self::seed_str(doc, root, "sel_source", "");
        Self::seed_str(doc, root, "sel_snapshot", "");
        Self::seed_str(doc, root, "sel_dest", "pool");
        if Self::get_bool(doc, root, "restore_open").is_none() {
            let _ = doc.put(root, "restore_open", false);
        }
        let p = RetentionPolicy::default();
        Self::seed_int(doc, root, "ret_hourly", i64::from(p.hourly));
        Self::seed_int(doc, root, "ret_daily", i64::from(p.daily));
        Self::seed_int(doc, root, "ret_weekly", i64::from(p.weekly));
        Self::seed_int(doc, root, "ret_monthly", i64::from(p.monthly));
    }

    fn seed_str(doc: &mut AutoCommit, root: &ObjId, key: &str, val: &str) {
        if Self::get_str(doc, root, key).is_none() {
            let _ = doc.put(root, key, val);
        }
    }

    fn seed_int(doc: &mut AutoCommit, root: &ObjId, key: &str, val: i64) {
        if Self::get_int(doc, root, key).is_none() {
            let _ = doc.put(root, key, val);
        }
    }

    // --- typed scalar readers -------------------------------------------

    fn get_str(doc: &AutoCommit, root: &ObjId, key: &str) -> Option<String> {
        match doc.get(root, key) {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                ScalarValue::Str(v) => Some(v.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    fn get_bool(doc: &AutoCommit, root: &ObjId, key: &str) -> Option<bool> {
        match doc.get(root, key) {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                ScalarValue::Boolean(v) => Some(*v),
                _ => None,
            },
            _ => None,
        }
    }

    fn get_int(doc: &AutoCommit, root: &ObjId, key: &str) -> Option<i64> {
        match doc.get(root, key) {
            Ok(Some((Value::Scalar(s), _))) => s.as_ref().to_i64(),
            _ => None,
        }
    }

    // --- public typed accessors -----------------------------------------

    pub fn view(doc: &AutoCommit, root: &ObjId) -> View {
        View::from_key(&Self::get_str(doc, root, "view").unwrap_or_default())
    }

    pub fn set_view(doc: &mut AutoCommit, root: &ObjId, v: View) {
        let _ = doc.put(root, "view", v.key());
    }

    /// Selected source id, or `None` for "all sources".
    pub fn selected_source(doc: &AutoCommit, root: &ObjId) -> Option<String> {
        match Self::get_str(doc, root, "sel_source") {
            Some(s) if !s.is_empty() => Some(s),
            _ => None,
        }
    }

    pub fn set_selected_source(doc: &mut AutoCommit, root: &ObjId, id: Option<&str>) {
        let _ = doc.put(root, "sel_source", id.unwrap_or(""));
    }

    pub fn selected_snapshot(doc: &AutoCommit, root: &ObjId) -> Option<String> {
        match Self::get_str(doc, root, "sel_snapshot") {
            Some(s) if !s.is_empty() => Some(s),
            _ => None,
        }
    }

    pub fn set_selected_snapshot(doc: &mut AutoCommit, root: &ObjId, id: Option<&str>) {
        let _ = doc.put(root, "sel_snapshot", id.unwrap_or(""));
    }

    pub fn selected_dest(doc: &AutoCommit, root: &ObjId) -> Option<String> {
        match Self::get_str(doc, root, "sel_dest") {
            Some(s) if !s.is_empty() => Some(s),
            _ => None,
        }
    }

    pub fn set_selected_dest(doc: &mut AutoCommit, root: &ObjId, id: &str) {
        let _ = doc.put(root, "sel_dest", id);
    }

    pub fn restore_open(doc: &AutoCommit, root: &ObjId) -> bool {
        Self::get_bool(doc, root, "restore_open").unwrap_or(false)
    }

    pub fn set_restore_open(doc: &mut AutoCommit, root: &ObjId, open: bool) {
        let _ = doc.put(root, "restore_open", open);
    }

    pub fn policy(doc: &AutoCommit, root: &ObjId) -> RetentionPolicy {
        let d = RetentionPolicy::default();
        RetentionPolicy {
            hourly: Self::get_int(doc, root, "ret_hourly")
                .unwrap_or(i64::from(d.hourly))
                .max(0) as u32,
            daily: Self::get_int(doc, root, "ret_daily")
                .unwrap_or(i64::from(d.daily))
                .max(0) as u32,
            weekly: Self::get_int(doc, root, "ret_weekly")
                .unwrap_or(i64::from(d.weekly))
                .max(0) as u32,
            monthly: Self::get_int(doc, root, "ret_monthly")
                .unwrap_or(i64::from(d.monthly))
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
        Self::get_int(doc, root, field.key())
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
    use automerge::ROOT;

    #[test]
    fn seed_then_read_defaults() {
        let mut doc = AutoCommit::new();
        BackupsDoc::seed(&mut doc, &ROOT);
        assert_eq!(BackupsDoc::view(&doc, &ROOT), View::Timeline);
        assert_eq!(BackupsDoc::selected_source(&doc, &ROOT), None);
        assert!(!BackupsDoc::restore_open(&doc, &ROOT));
        assert_eq!(BackupsDoc::policy(&doc, &ROOT), RetentionPolicy::default());
    }

    #[test]
    fn view_round_trips() {
        let mut doc = AutoCommit::new();
        BackupsDoc::seed(&mut doc, &ROOT);
        BackupsDoc::set_view(&mut doc, &ROOT, View::Destinations);
        assert_eq!(BackupsDoc::view(&doc, &ROOT), View::Destinations);
    }

    #[test]
    fn empty_doc_reads_default_without_seed() {
        // The --shots path syncs against a fresh doc; readers must not panic
        // and must fall back to defaults before seed runs.
        let doc = AutoCommit::new();
        assert_eq!(BackupsDoc::view(&doc, &ROOT), View::Timeline);
        assert_eq!(BackupsDoc::policy(&doc, &ROOT), RetentionPolicy::default());
    }

    #[test]
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

    #[test]
    fn retention_reads_default_before_seed() {
        // The action's change-guard reads a field's current value even against a
        // fresh doc; it must fall back to the class default, not 0.
        let doc = AutoCommit::new();
        assert_eq!(BackupsDoc::retention(&doc, &ROOT, PolicyField::Daily), 7);
        assert_eq!(BackupsDoc::retention(&doc, &ROOT, PolicyField::Hourly), 24);
    }

    #[test]
    fn seed_is_idempotent() {
        let mut doc = AutoCommit::new();
        BackupsDoc::seed(&mut doc, &ROOT);
        BackupsDoc::set_view(&mut doc, &ROOT, View::Sources);
        BackupsDoc::seed(&mut doc, &ROOT); // must not reset to Timeline
        assert_eq!(BackupsDoc::view(&doc, &ROOT), View::Sources);
    }
}
