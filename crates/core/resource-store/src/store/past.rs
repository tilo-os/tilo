//! What a parent held at one of its versions: the resources nested in it then, each with its
//! fields and content as they stood. A parent's resources fill documents of their own, so a
//! version (one change, in one of them) is read across all of them at one cut.

use std::collections::{HashMap, HashSet};

use automerge::{Automerge, ChangeHash, ROOT};
use collab_types::store::{Indexed, PastChild};

use super::{HELD, Store, content_id, read_as_of};
use crate::admit::Stamp;

/// Where one version stands among the changes of every document: the change itself, and the
/// moment its writer stamped it with ([`Stamp::moment`]).
pub(super) struct Cut {
    change: ChangeHash,
    moment: u64,
}

impl Cut {
    /// The heads `doc` stood at, at this version. The document holding the version's change
    /// stands at it and everything it was built on; every document also holds what was written
    /// at the version's moment or before (other documents' changes, and other devices' changes
    /// the version was not built on), as far as what each is built on is held too. A change
    /// whose writer kept no moment counts as written before every moment.
    pub(super) fn heads(&self, doc: &Automerge) -> Vec<ChangeHash> {
        let mut past: HashSet<ChangeHash> = HashSet::new();
        if doc.get_change_meta_by_hash(&self.change).is_some() {
            let mut next = vec![self.change];
            while let Some(hash) = next.pop() {
                if past.insert(hash)
                    && let Some(meta) = doc.get_change_meta_by_hash(&hash)
                {
                    next.extend(meta.deps);
                }
            }
        }
        let mut held: HashSet<ChangeHash> = HashSet::new();
        let mut built_on: HashSet<ChangeHash> = HashSet::new();
        for meta in doc.get_changes_meta(&[]) {
            let written = meta
                .message
                .as_deref()
                .and_then(Stamp::of_message)
                .map_or(0, |stamp| stamp.moment);
            if meta.deps.iter().all(|dep| held.contains(dep))
                && (past.contains(&meta.hash) || written <= self.moment)
            {
                held.insert(meta.hash);
                built_on.extend(meta.deps);
            }
        }
        held.retain(|hash| !built_on.contains(hash));
        let mut heads: Vec<ChangeHash> = held.into_iter().collect();
        heads.sort_unstable();
        heads
    }
}

impl Store {
    /// The resources nested in `parent` as they stood at the version the change `change` (its
    /// hash in hex) made, those whose readers `may_read` admits, ordered by their place and
    /// then their id: what a reader comparing a parent's past states reads its collections
    /// from. The change is one of `parent`'s versions or of a resource it is nested in: one in
    /// its own documents, or one that made or removed a resource nested in it. Resources
    /// removed since stand as they were; those made since are not there.
    ///
    /// `None` where the change is not a hash, or no document of `parent` or what it is nested
    /// in holds it in memory ([`Self::read_children_at`] reads them back first).
    #[must_use]
    pub fn children_at(
        &self,
        parent: &str,
        change: &str,
        may_read: &dyn Fn(&str) -> bool,
    ) -> Option<Vec<PastChild>> {
        let cut = self.cut_of(parent, change)?;
        let mut heads_of: HashMap<String, Vec<ChangeHash>> = HashMap::new();
        let mut out = Vec::new();
        for doc_id in self.nested_in.get(parent).into_iter().flatten() {
            let (Some(doc), Some(readers)) = (self.docs.get(doc_id), self.index.readers(doc_id))
            else {
                continue;
            };
            if !may_read(readers) {
                continue;
            }
            let heads = cut.heads(doc);
            let look = crate::wire::AsOf {
                doc,
                heads: Some(&heads),
            };
            for key in look.keys(&ROOT) {
                let Some(id) = key.strip_prefix(HELD) else {
                    continue;
                };
                let Some((kind, resource)) = read_as_of(doc, Some(&heads), id) else {
                    continue;
                };
                if resource.parent.as_deref() != Some(parent) {
                    continue;
                }
                // A record whose type names its readers is shown to them, wherever it was held.
                if self.rules.contains_key(kind.as_str())
                    && !self
                        .readers_of(&kind, id)
                        .is_some_and(|readers| may_read(&readers))
                {
                    continue;
                }
                let content_doc = content_id(readers, &kind, id);
                let content = self.docs.get(&content_doc).and_then(|content| {
                    let heads = heads_of
                        .entry(content_doc.clone())
                        .or_insert_with(|| cut.heads(content));
                    let look = crate::wire::AsOf {
                        doc: content,
                        heads: Some(heads),
                    };
                    (look.length(&ROOT) > 0).then(|| look.json(&ROOT))
                });
                out.push(PastChild {
                    kind,
                    id: id.to_owned(),
                    indexed: Indexed {
                        doc: doc_id.clone(),
                        parent: resource.parent,
                        order: resource.order,
                        fields: resource.fields,
                    },
                    content,
                });
            }
        }
        out.sort_by(|a, b| (&a.indexed.order, &a.id).cmp(&(&b.indexed.order, &b.id)));
        Some(out)
    }

    /// [`Self::children_at`], with the documents it reads (those of `parent` and of what it is
    /// nested in) read back from the source first, as [`Self::read_versions`] does.
    ///
    /// # Errors
    /// A saved document does not decode.
    pub fn read_children_at(
        &mut self,
        parent: &str,
        change: &str,
        may_read: &dyn Fn(&str) -> bool,
    ) -> Result<Option<Vec<PastChild>>, super::StoreError> {
        for doc in self.documents_up_from(parent) {
            self.existing(&doc)?;
        }
        Ok(self.children_at(parent, change, may_read))
    }

    /// The cut the change `change` makes: found in the documents of `parent`, or of a resource
    /// it is nested in, nearest first.
    pub(super) fn cut_of(&self, parent: &str, change: &str) -> Option<Cut> {
        let hash: ChangeHash = change.parse().ok()?;
        let message = self.documents_up_from(parent).into_iter().find_map(|doc| {
            let meta = self.docs.get(&doc)?.get_change_meta_by_hash(&hash)?;
            Some(meta.message.map(std::borrow::Cow::into_owned))
        })?;
        let moment = message
            .as_deref()
            .and_then(Stamp::of_message)
            .map_or(0, |stamp| stamp.moment);
        Some(Cut {
            change: hash,
            moment,
        })
    }

    /// The documents a version of `parent`, or of what it is nested in, is a change of: each
    /// one's own and content documents and those it holds resources in, `parent` first, then
    /// up the resources it is nested in.
    fn documents_up_from(&self, parent: &str) -> Vec<String> {
        let mut docs = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        let mut at = Some(parent.to_owned());
        while let Some(id) = at.take() {
            if seen.len() >= super::MOST_PARENTS || seen.contains(&id) {
                break;
            }
            docs.extend(self.nested_in.get(&id).into_iter().flatten().cloned());
            if let Some(kind) = self.kind_of_any(&id)
                && let Some((held, readers)) = self.placed(&kind, &id)
            {
                docs.push(content_id(&readers, &kind, &id));
                at = self
                    .index
                    .get(&kind, &id)
                    .and_then(|held| held.parent.clone())
                    .or_else(|| {
                        self.removed
                            .get(&(kind.clone(), id.clone()))
                            .and_then(|removed| removed.parent.clone())
                    });
                docs.push(held);
            }
            seen.push(id);
        }
        docs
    }

    /// The kind of the resource `id`, held now or removed.
    fn kind_of_any(&self, id: &str) -> Option<String> {
        self.index.kind_anywhere(id).or_else(|| {
            self.removed
                .keys()
                .find(|(_, held)| held == id)
                .map(|(kind, _)| kind.clone())
        })
    }
}
