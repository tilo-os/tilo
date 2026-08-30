//! CRDT view-state for the Logs viewer.
//!
//! The *journal entries* are external, read-only, and potentially huge —
//! they live in a render cache on the widget, not the document. What
//! lives in the Automerge doc is the **view state**: which entry is
//! selected, the active filters, the search string, and whether live
//! tail is on. Keeping these in the CRDT is what makes undo/redo and
//! live collaboration (two peers triaging the same boot together) fall
//! out for free, exactly as the toolkit intends.
//!
//! ```text
//! root
//! ├── "min_priority" (Int)   # severity threshold, 0..=7 (7 = all)
//! ├── "unit_filter"  (Str)   # exact unit, "" = all
//! ├── "boot_filter"  (Str)   # exact _BOOT_ID, "" = all
//! ├── "search"       (Text)  # collaborative free-text query
//! ├── "selected"     (Int)   # selected entry index, -1 = auto
//! └── "live_tail"    (Bool)
//! ```

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};

use crate::model::Filter;

pub const K_MIN_PRIORITY: &str = "min_priority";
pub const K_UNIT: &str = "unit_filter";
pub const K_BOOT: &str = "boot_filter";
pub const K_SEARCH: &str = "search";
pub const K_SELECTED: &str = "selected";
pub const K_LIVE: &str = "live_tail";

/// Stateless helpers over the app's shared `State::doc`. The `root` `ObjId`
/// passed in is the document root the toolkit hands to `sync`.
pub struct LogsDoc;

impl LogsDoc {
    /// Seed every view-state key with its default if absent. Idempotent —
    /// safe to call on every sync.
    pub fn ensure_defaults(doc: &mut AutoCommit, root: &ObjId) {
        if get_int(doc, root, K_MIN_PRIORITY).is_none() {
            let _ = doc.put(root, K_MIN_PRIORITY, 7i64);
        }
        if get_str(doc, root, K_UNIT).is_none() {
            let _ = doc.put(root, K_UNIT, "");
        }
        if get_str(doc, root, K_BOOT).is_none() {
            let _ = doc.put(root, K_BOOT, "");
        }
        if search_obj(doc, root).is_none() {
            let _ = doc.put_object(root, K_SEARCH, ObjType::Text);
        }
        if get_int(doc, root, K_SELECTED).is_none() {
            let _ = doc.put(root, K_SELECTED, -1i64);
        }
        if get_bool(doc, root, K_LIVE).is_none() {
            let _ = doc.put(root, K_LIVE, false);
        }
    }

    pub fn min_priority(doc: &AutoCommit, root: &ObjId) -> i64 {
        get_int(doc, root, K_MIN_PRIORITY).unwrap_or(7).clamp(0, 7)
    }
    pub fn set_min_priority(doc: &mut AutoCommit, root: &ObjId, v: i64) {
        let _ = doc.put(root, K_MIN_PRIORITY, v.clamp(0, 7));
    }

    pub fn unit(doc: &AutoCommit, root: &ObjId) -> String {
        get_str(doc, root, K_UNIT).unwrap_or_default()
    }
    pub fn set_unit(doc: &mut AutoCommit, root: &ObjId, v: &str) {
        let _ = doc.put(root, K_UNIT, v);
    }

    pub fn boot(doc: &AutoCommit, root: &ObjId) -> String {
        get_str(doc, root, K_BOOT).unwrap_or_default()
    }
    pub fn set_boot(doc: &mut AutoCommit, root: &ObjId, v: &str) {
        let _ = doc.put(root, K_BOOT, v);
    }

    pub fn search(doc: &AutoCommit, root: &ObjId) -> String {
        match search_obj_read(doc, root) {
            Some(id) => doc.text(&id).unwrap_or_default(),
            None => String::new(),
        }
    }

    /// Replace the whole search string.
    pub fn search_set(doc: &mut AutoCommit, root: &ObjId, s: &str) {
        let id = match search_obj(doc, root) {
            Some(id) => id,
            None => doc
                .put_object(root, K_SEARCH, ObjType::Text)
                .expect("put search text"),
        };
        ui::doc::splice_text_diffed(doc, &id, s);
    }

    pub fn selected(doc: &AutoCommit, root: &ObjId) -> i64 {
        get_int(doc, root, K_SELECTED).unwrap_or(-1)
    }
    pub fn set_selected(doc: &mut AutoCommit, root: &ObjId, v: i64) {
        let _ = doc.put(root, K_SELECTED, v);
    }

    pub fn live_tail(doc: &AutoCommit, root: &ObjId) -> bool {
        get_bool(doc, root, K_LIVE).unwrap_or(false)
    }
    pub fn set_live_tail(doc: &mut AutoCommit, root: &ObjId, v: bool) {
        let _ = doc.put(root, K_LIVE, v);
    }

    /// Clear every facet back to "show everything".
    pub fn reset_filters(doc: &mut AutoCommit, root: &ObjId) {
        Self::set_min_priority(doc, root, 7);
        Self::set_unit(doc, root, "");
        Self::set_boot(doc, root, "");
        Self::search_set(doc, root, "");
    }

    /// Build a [`Filter`] snapshot from the document.
    pub fn filter(doc: &AutoCommit, root: &ObjId) -> Filter {
        Filter {
            min_priority: Self::min_priority(doc, root),
            unit: Self::unit(doc, root),
            boot: Self::boot(doc, root),
            search: Self::search(doc, root),
        }
    }
}

// ---- low-level scalar accessors -----------------------------------

fn get_int(doc: &AutoCommit, root: &ObjId, key: &str) -> Option<i64> {
    match doc.get(root, key) {
        Ok(Some((Value::Scalar(v), _))) => v.to_i64(),
        _ => None,
    }
}

fn get_bool(doc: &AutoCommit, root: &ObjId, key: &str) -> Option<bool> {
    match doc.get(root, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Boolean(b) => Some(*b),
            _ => None,
        },
        _ => None,
    }
}

fn get_str(doc: &AutoCommit, root: &ObjId, key: &str) -> Option<String> {
    match doc.get(root, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Str(s) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn search_obj(doc: &mut AutoCommit, root: &ObjId) -> Option<ObjId> {
    match doc.get(root, K_SEARCH) {
        Ok(Some((Value::Object(ObjType::Text), id))) => Some(id),
        _ => None,
    }
}

fn search_obj_read(doc: &AutoCommit, root: &ObjId) -> Option<ObjId> {
    match doc.get(root, K_SEARCH) {
        Ok(Some((Value::Object(ObjType::Text), id))) => Some(id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ROOT;

    #[test]
    fn defaults_seed_then_round_trip() {
        let mut doc = AutoCommit::new();
        LogsDoc::ensure_defaults(&mut doc, &ROOT);
        assert_eq!(LogsDoc::min_priority(&doc, &ROOT), 7);
        assert_eq!(LogsDoc::unit(&doc, &ROOT), "");
        assert_eq!(LogsDoc::selected(&doc, &ROOT), -1);
        assert!(!LogsDoc::live_tail(&doc, &ROOT));
    }

    #[test]
    fn search_replacement_round_trips() {
        let mut doc = AutoCommit::new();
        LogsDoc::ensure_defaults(&mut doc, &ROOT);
        LogsDoc::search_set(&mut doc, &ROOT, "mount");
        assert_eq!(LogsDoc::search(&doc, &ROOT), "mount");
    }

    #[test]
    fn reset_clears_every_facet() {
        let mut doc = AutoCommit::new();
        LogsDoc::ensure_defaults(&mut doc, &ROOT);
        LogsDoc::set_min_priority(&mut doc, &ROOT, 3);
        LogsDoc::set_unit(&mut doc, &ROOT, "shell.service");
        LogsDoc::search_set(&mut doc, &ROOT, "boom");
        LogsDoc::reset_filters(&mut doc, &ROOT);
        let f = LogsDoc::filter(&doc, &ROOT);
        assert!(f.is_clear());
    }

    #[test]
    fn filter_snapshot_reflects_doc() {
        let mut doc = AutoCommit::new();
        LogsDoc::ensure_defaults(&mut doc, &ROOT);
        LogsDoc::set_min_priority(&mut doc, &ROOT, 4);
        LogsDoc::set_unit(&mut doc, &ROOT, "kernel");
        let f = LogsDoc::filter(&doc, &ROOT);
        assert_eq!(f.min_priority, 4);
        assert_eq!(f.unit, "kernel");
    }

    #[test]
    fn appending_one_char_to_search_is_bounded() {
        // Anti-churn: `search_set` used to rewrite the whole search Text; the
        // diff-splice makes a one-char change cost one op.
        let mut doc = AutoCommit::new();
        let long = "z".repeat(300);
        LogsDoc::search_set(&mut doc, &ROOT, &long);
        let before = doc.save().len();
        let mut longer = long.clone();
        longer.push('q');
        LogsDoc::search_set(&mut doc, &ROOT, &longer);
        let grew = doc.save().len() - before;
        assert_eq!(LogsDoc::search(&doc, &ROOT), longer, "value round-trips");
        assert!(grew < 40, "one-char search edit grew doc by {grew} bytes");
    }

    #[test]
    fn repeated_search_set_keeps_doc_bounded() {
        let mut doc = AutoCommit::new();
        let long = "w".repeat(300);
        LogsDoc::search_set(&mut doc, &ROOT, &long);
        let a = doc.save().len();
        for _ in 0..50 {
            LogsDoc::search_set(&mut doc, &ROOT, &long);
        }
        let b = doc.save().len();
        assert_eq!(
            a, b,
            "re-storing the same search grew the document {a} -> {b}"
        );
    }
}
