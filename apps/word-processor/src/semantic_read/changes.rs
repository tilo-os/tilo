//! What a document edit answers: the blocks it changed, as
//! [`read-text`](super::markdown) renders them, so its caller sees the
//! result without reading the document again.
//!
//! ```text
//! 2 changed:
//! ## Standup {#b1}
//! Maya is out today {#b2}
//! 1 removed
//! ```

use std::collections::BTreeSet;

use automerge::{AutoCommit, PatchAction, Prop};

use super::{Segment, Shape};

/// The most changed blocks one answer lists; `read-text` reads the rest.
pub const MAX_LISTED: usize = 32;

/// The blocks `changes`, an edit's patches over `doc`, touched.
pub fn summary(doc: &AutoCommit, changes: &[automerge::Patch]) -> String {
    let Ok(containers) = qdoc::model::open(doc, &automerge::ROOT) else {
        return "no document".to_owned();
    };
    let mut touched = BTreeSet::new();
    for patch in changes {
        let under_blocks = patch
            .path
            .iter()
            .find(|(object, _)| *object == containers.blocks)
            .map(|(_, prop)| prop.clone());
        let block = under_blocks.or_else(|| {
            (patch.obj == containers.blocks)
                .then(|| match &patch.action {
                    PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } => {
                        Some(Prop::Map(key.clone()))
                    }
                    _ => None,
                })
                .flatten()
        });
        if let Some(Prop::Map(id)) = block {
            touched.insert(id);
        }
    }
    if touched.is_empty() {
        return "no change".to_owned();
    }
    let changed: Vec<Segment> = qdoc::model::materialize(doc, &containers)
        .blocks
        .into_iter()
        .filter(|block| touched.contains(&block.id))
        .map(|block| Segment {
            shape: Shape::of_attrs(&block.attrs),
            block: block.id,
            kind: block.kind,
            text: block.text,
            complete: true,
        })
        .collect();
    let removed = touched.len().saturating_sub(changed.len());
    let mut out = String::new();
    if !changed.is_empty() {
        out.push_str(&format!("{} changed:\n", changed.len()));
        let listed = changed.len().min(MAX_LISTED);
        out.push_str(&super::markdown::render(&changed[..listed]));
        if changed.len() > listed {
            out.push_str(&format!(
                "… {} more; read-text reads them\n",
                changed.len() - listed
            ));
        }
    }
    if removed > 0 {
        out.push_str(&format!("{removed} removed\n"));
    }
    out
}
