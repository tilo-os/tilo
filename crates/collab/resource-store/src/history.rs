//! A resource's versions: the changes that made, changed or removed it, read from the history of
//! the documents it is in, each as the action that made it stamped it ([`Stamp`]).

use automerge::Automerge;
use serde::{Deserialize, Serialize};

use crate::admit::Stamp;

/// What one change did to a resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VersionKind {
    Made,
    Changed,
    Removed,
}

/// One version of a resource.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    /// The action that made it; `None` for a change that names none.
    pub stamp: Option<Stamp>,
    pub kind: VersionKind,
    /// The fields it set; empty for one that made or removed the resource.
    pub fields: Vec<String>,
    /// It changed the resource's content.
    pub content: bool,
    /// The change, by its hash in hex.
    pub change: String,
    /// The counter of the change's first operation: its place in the order every holder agrees
    /// on.
    pub op: u64,
    /// For the change that removed the resource, the resource it was nested in then: where it
    /// stood. `None` for any other change, and where the store does not know it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// The resources nested in it the change made or removed, in the order it did: a change of
    /// what it holds rather than of the resource itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nested: Vec<Nested>,
}

/// A resource nested in another that a change made or removed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Nested {
    pub kind: String,
    pub id: String,
    /// Made by the change; else removed.
    pub made: bool,
}

/// The versions of every resource of one document, read once per change: a read of one
/// resource's history then costs its own versions, not every change of the document it shares
/// with a thousand others.
#[derive(Default)]
pub(crate) struct Touches {
    /// The heads of the document as read so far.
    heads: Vec<automerge::ChangeHash>,
    by_id: std::collections::HashMap<String, Vec<Version>>,
    /// The changes that made or removed a resource, in the order they were read: each a
    /// version of what held that resource.
    steps: Vec<Version>,
    /// The resources each holder had made in it: the step, the place in that step, and the
    /// kind and id of what was made.
    made_in: std::collections::HashMap<String, Vec<(usize, usize, String, String)>>,
    /// The steps that removed each resource, with its place in each.
    removed_by: std::collections::HashMap<String, Vec<(usize, usize)>>,
}

impl Touches {
    /// The versions of `id`, oldest first, in the document `doc_id` as it is now: the changes
    /// it took since the last read are read first.
    pub(crate) fn of(&mut self, doc: &Automerge, doc_id: &str, id: &str) -> Vec<Version> {
        self.read(doc, doc_id);
        self.by_id.get(id).cloned().unwrap_or_default()
    }

    /// The versions of the resource `parent` the changes of this document made, oldest
    /// first: each change that made a resource nested in it, or removed one of `removed` (the
    /// kinds and ids of those removed from it).
    pub(crate) fn nested(
        &mut self,
        doc: &Automerge,
        doc_id: &str,
        parent: &str,
        removed: &std::collections::BTreeSet<(String, String)>,
    ) -> Vec<Version> {
        self.read(doc, doc_id);
        let mut picked: Vec<(usize, usize, Nested)> = Vec::new();
        for (step, at, kind, id) in self.made_in.get(parent).into_iter().flatten() {
            picked.push((
                *step,
                *at,
                Nested {
                    kind: kind.clone(),
                    id: id.clone(),
                    made: true,
                },
            ));
        }
        for (kind, id) in removed {
            for (step, at) in self.removed_by.get(id).into_iter().flatten() {
                picked.push((
                    *step,
                    *at,
                    Nested {
                        kind: kind.clone(),
                        id: id.clone(),
                        made: false,
                    },
                ));
            }
        }
        picked.sort_by_key(|(step, at, _)| (*step, *at));
        let mut versions: Vec<(usize, Version)> = Vec::new();
        for (step, _, nested) in picked {
            match versions.last_mut() {
                Some((last, version)) if *last == step => version.nested.push(nested),
                _ => {
                    if let Some(version) = self.steps.get(step) {
                        let mut version = version.clone();
                        version.nested.push(nested);
                        versions.push((step, version));
                    }
                }
            }
        }
        versions.into_iter().map(|(_, version)| version).collect()
    }

    /// The versions of `id` as last read, oldest first.
    pub(crate) fn versions_of(&self, id: &str) -> Option<&Vec<Version>> {
        self.by_id.get(id)
    }

    /// The versions of every resource of this document, by id, oldest first: borrowed, so a
    /// reader that looks at every resource of a document copies none of them.
    pub(crate) fn lasts(
        &mut self,
        doc: &Automerge,
        doc_id: &str,
    ) -> &std::collections::HashMap<String, Vec<Version>> {
        self.read(doc, doc_id);
        &self.by_id
    }

    /// Read the changes the document took since the last read.
    fn read(&mut self, doc: &Automerge, doc_id: &str) {
        let heads = doc.get_heads();
        if heads != self.heads {
            for change in doc.get_changes(&self.heads) {
                let Some((touched, _)) = crate::admit::touched(doc, doc_id, &change) else {
                    continue;
                };
                let step = self.steps.len();
                let mut stepped = false;
                for (at, (resource, touch)) in touched.resources.iter().enumerate() {
                    let version = version(&change, touch);
                    if touch.made
                        && let (Some(parent), Some(kind)) = (&touch.parent, &touch.kind)
                    {
                        self.made_in.entry(parent.clone()).or_default().push((
                            step,
                            at,
                            kind.clone(),
                            resource.clone(),
                        ));
                        stepped = true;
                    }
                    if touch.removed {
                        self.removed_by
                            .entry(resource.clone())
                            .or_default()
                            .push((step, at));
                        stepped = true;
                    }
                    if stepped && self.steps.len() == step {
                        self.steps.push(Version {
                            kind: VersionKind::Changed,
                            fields: Vec::new(),
                            content: false,
                            ..version.clone()
                        });
                    }
                    self.by_id
                        .entry(resource.clone())
                        .or_default()
                        .push(version);
                }
            }
            self.heads = heads;
        }
    }
}

/// The versions the changes of the document `doc_id` whose stamp `stamped` admits made, oldest
/// first, each with the id of the resource it is a version of.
pub(crate) fn stamped(
    doc: &Automerge,
    doc_id: &str,
    stamped: &dyn Fn(&Stamp) -> bool,
) -> Vec<(Version, String)> {
    let mut versions = Vec::new();
    for change in doc.get_changes(&[]) {
        if !Stamp::of(&change).is_some_and(|stamp| stamped(&stamp)) {
            continue;
        }
        let Some((touched, _)) = crate::admit::touched(doc, doc_id, &change) else {
            continue;
        };
        for (id, touch) in &touched.resources {
            versions.push((version(&change, touch), id.clone()));
        }
    }
    versions
}

/// The version `change` is of a resource it touched as `touch` says.
fn version(change: &automerge::Change, touch: &crate::admit::Touch) -> Version {
    let kind = if touch.made {
        VersionKind::Made
    } else if touch.removed {
        VersionKind::Removed
    } else {
        VersionKind::Changed
    };
    Version {
        stamp: Stamp::of(change),
        kind,
        fields: touch.fields.iter().cloned().collect(),
        content: touch.content,
        change: change.hash().to_string(),
        op: change.start_op().get(),
        parent: None,
        nested: Vec::new(),
    }
}
