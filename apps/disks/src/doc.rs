//! Automerge document layer for the Disks app.
//!
//! All UI state lives in the CRDT so selection / active-tab / pending
//! confirmations are undoable and replicate to collaborators for free.
//! The probed topology is serialised as a JSON string scalar (it is
//! machine-derived, not collaboratively edited character-by-character,
//! so a plain scalar is the right granularity).
//!
//! ```text
//! root
//! ├── "topology"      (Str)   # serde_json of model::Topology
//! ├── "selected"      (Int)   # index into topology.drives
//! ├── "selected_part" (Int)   # index into the drive's partitions, -1 = whole disk
//! ├── "tab"           (Str)   # overview | smart | partitions
//! ├── "pending"       (Str)   # "" or "<action>:<drive>:<part>" awaiting confirm
//! └── "status"        (Str)   # last action / status-line message
//! ```

use ui::automerge::transaction::Transactable;
use ui::automerge::{AutoCommit, ROOT, ReadDoc, Value};
use ui::doc::read_str;

use crate::model::Topology;

pub struct DisksDoc;

impl DisksDoc {
    /// True when the document has never been seeded.
    pub fn is_empty(doc: &AutoCommit) -> bool {
        !matches!(doc.get(&ROOT, "topology"), Ok(Some((Value::Scalar(_), _))))
    }

    /// Seed the document with an initial topology + default UI state.
    /// Idempotent: only writes fields that are missing.
    pub fn seed(doc: &mut AutoCommit, topo: &Topology) {
        Self::write_topology(doc, topo);
        if read_str(doc, &ROOT, "tab").is_none() {
            let _ = doc.put(&ROOT, "tab", "overview");
        }
        if !matches!(doc.get(&ROOT, "selected"), Ok(Some((Value::Scalar(_), _)))) {
            let _ = doc.put(&ROOT, "selected", 0i64);
        }
        if !matches!(
            doc.get(&ROOT, "selected_part"),
            Ok(Some((Value::Scalar(_), _)))
        ) {
            let _ = doc.put(&ROOT, "selected_part", -1i64);
        }
        if read_str(doc, &ROOT, "pending").is_none() {
            let _ = doc.put(&ROOT, "pending", "");
        }
        if read_str(doc, &ROOT, "status").is_none() {
            let _ = doc.put(&ROOT, "status", "");
        }
    }

    pub fn write_topology(doc: &mut AutoCommit, topo: &Topology) {
        let json = serde_json::to_string(topo).unwrap_or_else(|_| "{\"drives\":[]}".into());
        Self::write_topology_json(doc, &json);
    }

    pub fn write_topology_json(doc: &mut AutoCommit, json: &str) {
        let _ = doc.put(&ROOT, "topology", json);
    }

    /// Read the raw topology JSON, if present.
    pub fn topology_json(doc: &AutoCommit) -> Option<String> {
        read_str(doc, &ROOT, "topology")
    }

    pub fn selected(doc: &AutoCommit) -> usize {
        match doc.get(&ROOT, "selected") {
            Ok(Some((Value::Scalar(v), _))) => v.as_i64().unwrap_or(0).max(0) as usize,
            _ => 0,
        }
    }

    pub fn set_selected(doc: &mut AutoCommit, idx: usize) {
        let _ = doc.put(&ROOT, "selected", idx as i64);
    }

    /// Selected partition within the current drive, or `None` for the whole
    /// disk (`selected_part` scalar of `-1`).
    pub fn selected_part(doc: &AutoCommit) -> Option<usize> {
        match doc.get(&ROOT, "selected_part") {
            Ok(Some((Value::Scalar(v), _))) => {
                let n = v.as_i64().unwrap_or(-1);
                if n < 0 { None } else { Some(n as usize) }
            }
            _ => None,
        }
    }

    pub fn set_selected_part(doc: &mut AutoCommit, part: Option<usize>) {
        let v = part.map_or(-1, |p| p as i64);
        let _ = doc.put(&ROOT, "selected_part", v);
    }

    pub fn tab(doc: &AutoCommit) -> String {
        read_str(doc, &ROOT, "tab").unwrap_or_else(|| "overview".into())
    }

    pub fn set_tab(doc: &mut AutoCommit, tab: &str) {
        let _ = doc.put(&ROOT, "tab", tab);
    }

    pub fn pending(doc: &AutoCommit) -> String {
        read_str(doc, &ROOT, "pending").unwrap_or_default()
    }

    pub fn set_pending(doc: &mut AutoCommit, p: &str) {
        let _ = doc.put(&ROOT, "pending", p);
    }

    pub fn status(doc: &AutoCommit) -> String {
        read_str(doc, &ROOT, "status").unwrap_or_default()
    }

    pub fn set_status(doc: &mut AutoCommit, s: &str) {
        let _ = doc.put(&ROOT, "status", s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The disks document seeds the topology once, round-trips it, and persists UI fields without re-seeding clobbering them.
    #[test]
    fn disks_doc_seeds_once_and_persists_ui_state() {
        // --- phase: seed_then_read_round_trips ---
        {
            let mut doc = AutoCommit::new();
            assert!(DisksDoc::is_empty(&doc));
            let topo = Topology::sample();
            DisksDoc::seed(&mut doc, &topo);
            assert!(!DisksDoc::is_empty(&doc));
            assert_eq!(DisksDoc::selected(&doc), 0);
            assert_eq!(DisksDoc::tab(&doc), "overview");
            let json = DisksDoc::topology_json(&doc).unwrap();
            let back: Topology = serde_json::from_str(&json).unwrap();
            assert_eq!(back.drives.len(), 4);
        }

        // --- phase: ui_mutations_persist ---
        {
            let mut doc = AutoCommit::new();
            DisksDoc::seed(&mut doc, &Topology::sample());
            DisksDoc::set_selected(&mut doc, 2);
            DisksDoc::set_tab(&mut doc, "smart");
            DisksDoc::set_pending(&mut doc, "format:sdb");
            assert_eq!(DisksDoc::selected(&doc), 2);
            assert_eq!(DisksDoc::tab(&doc), "smart");
            assert_eq!(DisksDoc::pending(&doc), "format:sdb");
        }

        // --- phase: seed_is_idempotent_for_ui_fields ---
        {
            let mut doc = AutoCommit::new();
            DisksDoc::seed(&mut doc, &Topology::sample());
            DisksDoc::set_selected(&mut doc, 3);
            // Re-seeding must not clobber an existing selection.
            DisksDoc::seed(&mut doc, &Topology::sample());
            assert_eq!(DisksDoc::selected(&doc), 3);
        }
    }
}
