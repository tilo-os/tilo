//! CRDT bridge between the in-memory [`FirewallConfig`] and the shared
//! Automerge document.
//!
//! Storage shape (Phase 1 — coarse-grained, like the notes app's first
//! phase): the entire configuration is serialised to JSON and held in a
//! single `Text` object at `root["config"]`. Storing it as `Text` (rather
//! than a scalar string) means concurrent edits splice-merge at the
//! character level instead of last-writer-wins clobbering the whole blob —
//! good enough to demonstrate live collaboration on the ruleset. A later
//! phase can promote each zone/rule to its own automerge Map/List for
//! field-level convergence.
//!
//! ```text
//! root
//! └── "config" (Text)   # JSON of FirewallConfig
//! ```

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};

use crate::model::FirewallConfig;

const CONFIG_KEY: &str = "config";

/// Stateless namespace for the document helpers.
pub struct FirewallDoc;

impl FirewallDoc {
    /// Get-or-create the `config` Text object.
    fn config_text(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, CONFIG_KEY) {
            Ok(Some((Value::Object(ObjType::Text), id))) => id,
            _ => doc
                .put_object(&ROOT, CONFIG_KEY, ObjType::Text)
                .expect("put_object config on root never fails"),
        }
    }

    /// Read the JSON config string out of the document, if present and
    /// non-empty.
    pub fn load_json(doc: &AutoCommit) -> Option<String> {
        match doc.get(&ROOT, CONFIG_KEY) {
            Ok(Some((Value::Object(ObjType::Text), id))) => {
                let s = doc.text(&id).unwrap_or_default();
                if s.is_empty() { None } else { Some(s) }
            }
            _ => None,
        }
    }

    /// Parse the document into a [`FirewallConfig`], if one is stored.
    pub fn load(doc: &AutoCommit) -> Option<FirewallConfig> {
        Self::load_json(doc).and_then(|s| FirewallConfig::from_json(&s))
    }

    /// Persist `cfg` into the document, replacing the whole `config` Text.
    /// Returns the JSON that was written so callers can cache it and skip
    /// redundant adopt-on-sync churn.
    pub fn store(doc: &mut AutoCommit, cfg: &FirewallConfig) -> String {
        let json = cfg.to_json();
        let id = Self::config_text(doc);
        let cur = doc.text(&id).unwrap_or_default();
        if cur != json {
            let cur_chars = cur.chars().count() as isize;
            let _ = doc.splice_text(&id, 0, cur_chars, &json);
        }
        json
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::sample_config;

    #[test]
    fn store_then_load_round_trips() {
        let mut doc = AutoCommit::new();
        let cfg = sample_config();
        FirewallDoc::store(&mut doc, &cfg);
        let back = FirewallDoc::load(&doc).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn empty_doc_loads_none() {
        let doc = AutoCommit::new();
        assert!(FirewallDoc::load(&doc).is_none());
    }

    #[test]
    fn store_is_idempotent_text() {
        let mut doc = AutoCommit::new();
        let cfg = sample_config();
        let a = FirewallDoc::store(&mut doc, &cfg);
        let b = FirewallDoc::store(&mut doc, &cfg);
        assert_eq!(a, b);
        // A second identical store must not grow the document into two
        // copies; load still yields exactly one parseable config.
        assert_eq!(FirewallDoc::load(&doc).unwrap(), cfg);
    }

    #[test]
    fn two_peers_converge_on_edit() {
        // Peer A seeds; peer B joins from A's state, edits, A merges back.
        let mut a = AutoCommit::new();
        let cfg = sample_config();
        FirewallDoc::store(&mut a, &cfg);

        let mut b = AutoCommit::load(&a.save()).unwrap();
        let mut cfg_b = FirewallDoc::load(&b).unwrap();
        cfg_b.panic_mode = true;
        FirewallDoc::store(&mut b, &cfg_b);

        let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
        a.merge(&mut b_loaded).unwrap();
        let merged = FirewallDoc::load(&a).unwrap();
        assert!(merged.panic_mode, "panic edit converged into peer A");
    }
}
