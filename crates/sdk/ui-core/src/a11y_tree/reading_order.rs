//! The reading order of a tree's widget and declared-resource nodes, the ids
//! of the subtrees a refresh kept shared rather than copied.

use std::sync::Arc;

use super::SemanticId;

/// Node ids in reading order: the ids a refresh built, in runs, between the
/// ids of the subtrees it kept ([`RetainedSubtree`](super::RetainedSubtree)),
/// each the very list the subtree is kept with. A keystroke keeps every
/// block row but one, and carrying their ids costs a pointer per row, not a
/// copy per node.
#[derive(Debug, Default)]
pub(super) struct ReadingOrder {
    runs: Vec<Run>,
}

#[derive(Debug)]
enum Run {
    Built(Vec<SemanticId>),
    Kept(Arc<[SemanticId]>),
}

impl Run {
    fn ids(&self) -> &[SemanticId] {
        match self {
            Self::Built(ids) => ids,
            Self::Kept(ids) => ids,
        }
    }
}

/// A place in a [`ReadingOrder`], to take the ids pushed after it.
#[derive(Clone, Copy)]
pub(super) struct Mark {
    runs: usize,
    within: usize,
}

impl ReadingOrder {
    pub(super) fn from_ids(ids: Vec<SemanticId>) -> Self {
        Self {
            runs: vec![Run::Built(ids)],
        }
    }

    /// A node built this refresh, next in reading order.
    pub(super) fn push(&mut self, id: SemanticId) {
        if let Some(Run::Built(ids)) = self.runs.last_mut() {
            ids.push(id);
        } else {
            self.runs.push(Run::Built(vec![id]));
        }
    }

    /// A kept subtree's nodes, next in reading order.
    pub(super) fn keep(&mut self, ids: &Arc<[SemanticId]>) {
        self.runs.push(Run::Kept(Arc::clone(ids)));
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &SemanticId> {
        self.runs.iter().flat_map(Run::ids)
    }

    /// The ids of every kept subtree.
    pub(super) fn kept(&self) -> impl Iterator<Item = &SemanticId> {
        self.runs
            .iter()
            .filter(|run| matches!(run, Run::Kept(_)))
            .flat_map(Run::ids)
    }

    /// How many ids the kept subtrees hold.
    pub(super) fn kept_len(&self) -> usize {
        self.runs
            .iter()
            .filter_map(|run| match run {
                Run::Kept(ids) => Some(ids.len()),
                Run::Built(_) => None,
            })
            .sum()
    }

    pub(super) fn mark(&self) -> Mark {
        Mark {
            runs: self.runs.len(),
            within: match self.runs.last() {
                Some(Run::Built(ids)) => ids.len(),
                _ => 0,
            },
        }
    }

    /// The ids after `mark`, in reading order.
    pub(super) fn since(&self, mark: Mark) -> Vec<SemanticId> {
        let first = mark.runs.saturating_sub(1);
        self.runs
            .iter()
            .enumerate()
            .skip(first)
            .flat_map(|(index, run)| {
                let ids = run.ids();
                if index.saturating_add(1) == mark.runs {
                    // The run open at the mark: only what came after it.
                    match run {
                        Run::Built(_) => ids.get(mark.within..).unwrap_or_default(),
                        Run::Kept(_) => &[],
                    }
                } else {
                    ids
                }
            })
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(text: &str) -> SemanticId {
        SemanticId(text.to_owned())
    }

    #[test]
    fn kept_runs_share_their_ids_and_read_in_place() {
        let kept: Arc<[SemanticId]> = vec![id("k1"), id("k2")].into();
        let mut order = ReadingOrder::from_ids(vec![id("root")]);
        let mark = order.mark();
        order.push(id("a"));
        order.keep(&kept);
        order.push(id("b"));
        assert_eq!(
            order.iter().map(SemanticId::as_str).collect::<Vec<_>>(),
            ["root", "a", "k1", "k2", "b"]
        );
        assert_eq!(order.kept_len(), 2);
        assert_eq!(
            order.kept().map(SemanticId::as_str).collect::<Vec<_>>(),
            ["k1", "k2"]
        );
        assert_eq!(
            order
                .since(mark)
                .iter()
                .map(SemanticId::as_str)
                .collect::<Vec<_>>(),
            ["a", "k1", "k2", "b"]
        );
        let after_kept = order.mark();
        order.push(id("c"));
        assert_eq!(order.since(after_kept), vec![id("c")]);
    }
}
