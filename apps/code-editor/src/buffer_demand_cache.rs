//! Bounded peer-local materializations of authoritative committed Text objects.
//! Public resource state exposes only requested files. Pending transactions and
//! changed heads discard these snapshots rather than treating edit counts as
//! content identities; closing the last view releases every retained body.

use super::{Demand, Doc, EditorDoc, ProjectState, Rc};
use ui::automerge::ChangeHash;
use ui::automerge::transaction::Transactable;

const SOURCE_BUDGET: usize = 48 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub(super) struct BufferDemandCache {
    heads: Option<Vec<ChangeHash>>,
    inactive: Vec<(String, Rc<String>)>,
}

impl BufferDemandCache {
    fn retain(&mut self, id: String, text: String) {
        if text.len() > SOURCE_BUDGET {
            return;
        }
        self.inactive.retain(|(old, _)| old != &id);
        self.inactive.push((id, Rc::new(text)));
        while self.inactive.len() > 2
            || self
                .inactive
                .iter()
                .map(|(_, text)| text.len())
                .sum::<usize>()
                > SOURCE_BUDGET
        {
            self.inactive.remove(0);
        }
    }

    fn take(&mut self, id: &str) -> Option<String> {
        let index = self.inactive.iter().position(|(old, _)| old == id)?;
        let (_, text) = self.inactive.remove(index);
        Some(Rc::try_unwrap(text).unwrap_or_else(|text| text.as_ref().clone()))
    }
}

impl ProjectState {
    pub(crate) fn cached_buffer_stats(&self) -> (usize, usize) {
        (
            self.buffer_cache.inactive.len(),
            self.buffer_cache
                .inactive
                .iter()
                .map(|(_, text)| text.len())
                .sum(),
        )
    }

    pub(super) fn load_buffers(&mut self, doc: &Doc, demand: &Demand) {
        let heads = (doc.pending_ops() == 0).then(|| doc.committed_heads());
        if heads.is_none() || heads != self.buffer_cache.heads || demand.keys.is_empty() {
            self.buffers.clear();
            self.buffer_cache.inactive.clear();
        }
        self.buffer_cache.heads = heads;
        let old = std::mem::take(&mut self.buffers);
        for (id, text) in old {
            if demand.keys.contains(&id) {
                self.buffers.insert(id, text);
            } else if self.buffer_cache.heads.is_some() {
                self.buffer_cache.retain(id, text);
            }
        }
        for id in &demand.keys {
            if !self.buffers.contains_key(id) {
                let text = self
                    .buffer_cache
                    .take(id)
                    .or_else(|| EditorDoc::read_buffer_key(doc, id));
                if let Some(text) = text {
                    self.buffers.insert(id.clone(), text);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::CodeEditor;
    use ui::tilo_resource::Resource;

    fn body(doc: &mut Doc, id: &str, text: &str) {
        let object = EditorDoc::ensure_buffer_key(doc, id, "").unwrap();
        let length = ui::automerge::ReadDoc::text(doc, &object)
            .unwrap()
            .chars()
            .count();
        EditorDoc::splice(doc, &object, 0, length, text);
    }
    fn demand(state: &mut ProjectState, doc: &Doc, id: &str, expected: &str) {
        CodeEditor::load(state, doc, &Demand::none().with_keys([id.to_string()])).unwrap();
        assert_eq!(state.buffers.len(), 1);
        assert_eq!(state.buffers.get(id).map(String::as_str), Some(expected));
    }

    #[test]
    fn committed_switches_and_history_keep_exact_requested_bodies() {
        let mut doc = Doc::new();
        body(&mut doc, "first", "first body");
        body(&mut doc, "other", "other body");
        doc.commit();
        let history = doc.committed_heads();
        let mut state = CodeEditor::read(&doc).unwrap();
        demand(&mut state, &doc, "first", "first body");
        demand(&mut state, &doc, "other", "other body");
        assert!(
            state
                .buffer_cache
                .inactive
                .iter()
                .any(|(id, text)| id == "first" && text.as_str() == "first body")
        );
        demand(&mut state, &doc, "first", "first body");
        body(&mut doc, "first", "externally edited");
        doc.commit();
        demand(&mut state, &doc, "first", "externally edited");
        assert!(state.buffer_cache.inactive.is_empty());
        let historical = doc.fork_at(&history).unwrap();
        demand(&mut state, &historical, "first", "first body");
        let refreshed = CodeEditor::read(&doc).unwrap();
        assert!(refreshed.buffers.is_empty());
        assert!(refreshed.buffer_cache.inactive.is_empty());
        CodeEditor::load(&mut state, &historical, &Demand::none()).unwrap();
        assert!(state.buffers.is_empty());
        assert!(state.buffer_cache.inactive.is_empty());
    }

    #[test]
    fn equal_pending_counts_never_alias_or_close_transactions() {
        let mut doc = Doc::new();
        body(&mut doc, "first", "base body");
        doc.commit();
        let mut first = doc.fork();
        let mut other = doc.fork();
        body(&mut first, "first", "pending A");
        body(&mut other, "first", "pending B");
        let count = first.pending_ops();
        assert!(count > 0);
        assert_eq!(other.pending_ops(), count);
        let mut state = CodeEditor::read(&doc).unwrap();
        demand(&mut state, &doc, "first", "base body");
        demand(&mut state, &first, "first", "pending A");
        demand(&mut state, &other, "first", "pending B");
        assert!(state.buffer_cache.heads.is_none());
        assert!(state.buffer_cache.inactive.is_empty());
        assert_eq!(first.pending_ops(), count);
        assert_eq!(other.pending_ops(), count);
        demand(&mut state, &doc, "first", "base body");
    }

    #[test]
    fn retained_source_budget_refuses_oversized_bodies_and_evicts_oldest() {
        let mut cache = BufferDemandCache::default();
        cache.retain("oversized".into(), "x".repeat(SOURCE_BUDGET + 1));
        assert!(cache.inactive.is_empty());
        cache.retain("old".into(), "x".repeat(SOURCE_BUDGET / 2 + 1));
        cache.retain("new".into(), "y".repeat(SOURCE_BUDGET / 2));
        assert_eq!(cache.inactive.len(), 1);
        assert_eq!(
            cache.inactive.first().map(|(id, _)| id.as_str()),
            Some("new")
        );
        for id in ["third", "fourth"] {
            cache.retain(id.into(), id.into());
        }
        assert_eq!(cache.inactive.len(), 2);
        assert!(
            cache
                .inactive
                .iter()
                .map(|(_, text)| text.len())
                .sum::<usize>()
                <= SOURCE_BUDGET
        );
    }
}
