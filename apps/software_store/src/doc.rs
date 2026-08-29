//! Automerge document model for the Software store.
//!
//! The mutable state of the store — *which apps are installed at which
//! version*, and *which apps are mid-install with how much progress* —
//! lives in the shared CRDT so it round-trips through undo/redo and would
//! replicate to a collaborator looking at the same store. The immutable
//! catalog is NOT in the document (it is rebuilt from `catalog.rs` /
//! the live probe each launch).
//!
//! ```text
//! root
//! └── "store" (Map)
//!     ├── "seeded"     (Bool)   # first-run default seed has happened
//!     ├── "installed"  (Map)    # app-id (Str) -> installed version (Str)
//!     └── "installing" (Map)    # app-id (Str) -> progress f64 (0.0..=1.0)
//! ```

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::model::InstallSnapshot;

/// Stateless namespace for the store-document helpers.
pub struct StoreDoc;

impl StoreDoc {
    fn store_map(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "store") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "store", ObjType::Map)
                .expect("put_object store on root never fails"),
        }
    }

    fn submap(doc: &mut AutoCommit, key: &str) -> ObjId {
        let store = Self::store_map(doc);
        match doc.get(&store, key) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&store, key, ObjType::Map)
                .expect("put_object store submap never fails"),
        }
    }

    fn installed_map(doc: &mut AutoCommit) -> ObjId {
        Self::submap(doc, "installed")
    }

    fn installing_map(doc: &mut AutoCommit) -> ObjId {
        Self::submap(doc, "installing")
    }

    /// Whether the one-time default seed has run.
    pub fn is_seeded(doc: &mut AutoCommit) -> bool {
        let store = Self::store_map(doc);
        matches!(
            doc.get(&store, "seeded"),
            Ok(Some((Value::Scalar(v), _))) if matches!(v.as_ref(), ScalarValue::Boolean(true))
        )
    }

    /// Mark the default seed as done.
    pub fn mark_seeded(doc: &mut AutoCommit) {
        let store = Self::store_map(doc);
        let _ = doc.put(&store, "seeded", true);
    }

    /// Record `id` as installed at `version` (and clear any in-flight
    /// install progress for it).
    pub fn set_installed(doc: &mut AutoCommit, id: &str, version: &str) {
        let m = Self::installed_map(doc);
        let _ = doc.put(&m, id, version);
        Self::clear_installing(doc, id);
    }

    /// Remove `id` from the installed set (uninstall).
    pub fn remove_installed(doc: &mut AutoCommit, id: &str) {
        let m = Self::installed_map(doc);
        let _ = doc.delete(&m, id);
        Self::clear_installing(doc, id);
    }

    /// Set the in-flight install progress for `id` (0.0..=1.0).
    pub fn set_installing(doc: &mut AutoCommit, id: &str, progress: f64) {
        let m = Self::installing_map(doc);
        let _ = doc.put(&m, id, progress.clamp(0.0, 1.0));
    }

    /// Clear the in-flight install record for `id`.
    pub fn clear_installing(doc: &mut AutoCommit, id: &str) {
        let m = Self::installing_map(doc);
        let _ = doc.delete(&m, id);
    }

    /// Read the full mutable install state out of the document.
    pub fn read_snapshot(doc: &AutoCommit) -> InstallSnapshot {
        let mut snap = InstallSnapshot::default();

        if let Ok(Some((Value::Object(ObjType::Map), store))) = doc.get(&ROOT, "store") {
            if let Ok(Some((Value::Object(ObjType::Map), installed))) = doc.get(&store, "installed")
            {
                for key in doc.keys(&installed) {
                    if let Ok(Some((Value::Scalar(v), _))) = doc.get(&installed, key.as_str())
                        && let ScalarValue::Str(s) = v.as_ref()
                    {
                        snap.installed.insert(key, s.to_string());
                    }
                }
            }
            if let Ok(Some((Value::Object(ObjType::Map), installing))) =
                doc.get(&store, "installing")
            {
                for key in doc.keys(&installing) {
                    if let Ok(Some((Value::Scalar(v), _))) = doc.get(&installing, key.as_str())
                        && let Some(f) = scalar_to_f64(v.as_ref())
                    {
                        snap.installing.insert(key, f);
                    }
                }
            }
        }
        snap
    }

    /// One-time default seed: marks a handful of catalog apps installed,
    /// a couple deliberately a version behind so the Updates tab has
    /// content. No-op if the document is already seeded. `behind` entries
    /// are `(id, installed_version)`; `current` entries are installed at
    /// their latest version (the caller passes the catalog's latest).
    pub fn seed_defaults(
        doc: &mut AutoCommit,
        current: &[(&str, &str)],
        behind: &[(&str, &str)],
        installing: &[(&str, f64)],
    ) {
        if Self::is_seeded(doc) {
            return;
        }
        for (id, ver) in current {
            Self::set_installed(doc, id, ver);
        }
        for (id, ver) in behind {
            // Installed, but at an older version than the catalog's
            // latest → derives as UpdateAvailable.
            let m = Self::installed_map(doc);
            let _ = doc.put(&m, *id, *ver);
        }
        for (id, p) in installing {
            Self::set_installing(doc, id, *p);
        }
        Self::mark_seeded(doc);
    }
}

fn scalar_to_f64(v: &ScalarValue) -> Option<f64> {
    match v {
        ScalarValue::F64(f) => Some(*f),
        ScalarValue::Int(i) => Some(*i as f64),
        ScalarValue::Uint(u) => Some(*u as f64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_round_trips_through_snapshot() {
        let mut doc = AutoCommit::new();
        StoreDoc::set_installed(&mut doc, "org.gimp.GIMP", "3.0.2");
        StoreDoc::set_installing(&mut doc, "com.valvesoftware.Steam", 0.42);
        let snap = StoreDoc::read_snapshot(&doc);
        assert_eq!(snap.installed.get("org.gimp.GIMP").unwrap(), "3.0.2");
        assert!((snap.installing.get("com.valvesoftware.Steam").unwrap() - 0.42).abs() < 1e-9);
    }

    #[test]
    fn set_installed_clears_installing() {
        let mut doc = AutoCommit::new();
        StoreDoc::set_installing(&mut doc, "a", 0.5);
        StoreDoc::set_installed(&mut doc, "a", "1.0");
        let snap = StoreDoc::read_snapshot(&doc);
        assert!(snap.installing.get("a").is_none());
        assert_eq!(snap.installed.get("a").unwrap(), "1.0");
    }

    #[test]
    fn remove_installed_uninstalls() {
        let mut doc = AutoCommit::new();
        StoreDoc::set_installed(&mut doc, "a", "1.0");
        StoreDoc::remove_installed(&mut doc, "a");
        let snap = StoreDoc::read_snapshot(&doc);
        assert!(snap.installed.get("a").is_none());
    }

    #[test]
    fn seed_is_idempotent() {
        let mut doc = AutoCommit::new();
        StoreDoc::seed_defaults(&mut doc, &[("a", "1.0")], &[("b", "0.9")], &[("c", 0.3)]);
        // A second seed with different data must not apply.
        StoreDoc::seed_defaults(&mut doc, &[("z", "9.9")], &[], &[]);
        let snap = StoreDoc::read_snapshot(&doc);
        assert_eq!(snap.installed.get("a").unwrap(), "1.0");
        assert_eq!(snap.installed.get("b").unwrap(), "0.9");
        assert!((snap.installing.get("c").unwrap() - 0.3).abs() < 1e-9);
        assert!(
            snap.installed.get("z").is_none(),
            "second seed must be a no-op"
        );
    }

    #[test]
    fn seed_marks_seeded_flag() {
        let mut doc = AutoCommit::new();
        assert!(!StoreDoc::is_seeded(&mut doc));
        StoreDoc::seed_defaults(&mut doc, &[], &[], &[]);
        assert!(StoreDoc::is_seeded(&mut doc));
    }
}
