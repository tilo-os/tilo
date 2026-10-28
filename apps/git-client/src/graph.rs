//! Commit-graph lane assignment.
//!
//! Given a linear, newest-first list of commits with parent pointers, this
//! computes which horizontal lane (column) each commit's dot occupies and
//! the set of edges connecting a row to the rows of its parents. The
//! algorithm is the standard "active lanes" sweep used by most git GUIs:
//! we keep a vector of lanes, each tracking the commit id it is currently
//! waiting to reach. When we visit a commit we place it in the first lane
//! reserving it (or a fresh lane), then hand its lanes off to its parents.
//!
//! Output is purely positional — the renderer in `history.rs` turns lanes
//! and edges into bezier curves and dots.

use crate::model::Commit;

/// A single connection drawn between two graph rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphEdge {
    /// Row index (into the commit list) the edge starts at (the child / newer).
    pub from_row: usize,
    pub from_lane: usize,
    /// Row index the edge ends at (the parent / older).
    pub to_row: usize,
    /// Lane the edge enters at `to_row`.
    pub to_lane: usize,
}

/// The computed layout: per-row lane plus the connecting edges, and the
/// total lane count for sizing the gutter.
#[derive(Clone, Debug, Default)]
pub struct GraphLayout {
    /// `lane_of[row]` is the column the dot at `row` sits in.
    pub lane_of: Vec<usize>,
    pub edges: Vec<GraphEdge>,
    pub lanes: usize,
}

/// Compute the lane layout for `commits` (assumed newest-first / topo-ish
/// order, as `git log` produces). Mutates `commit.lane` on each entry as a
/// convenience for callers that carry the lane on the model, and returns
/// the full layout with edges.
pub fn layout(commits: &mut [Commit]) -> GraphLayout {
    // Map commit id -> row index for O(1) parent lookups.
    let mut row_of: std::collections::HashMap<String, usize> =
        std::collections::HashMap::with_capacity(commits.len());
    for (i, c) in commits.iter().enumerate() {
        row_of.insert(c.id.clone(), i);
    }

    // Active lanes: each slot holds the commit id that lane is currently
    // "reserved for" (the next commit expected to land there), or None if
    // the lane is free.
    let mut lanes: Vec<Option<String>> = Vec::new();
    let mut lane_of = vec![0usize; commits.len()];
    let mut edges: Vec<GraphEdge> = Vec::new();
    let mut max_lanes = 0usize;

    for (row, commit) in commits.iter_mut().enumerate() {
        let id = commit.id.clone();

        let mut my_lane = lanes.iter().position(|l| l.as_deref() == Some(id.as_str()));
        if my_lane.is_none() {
            // Not reserved (a tip / branch head) — take the first free lane.
            my_lane = lanes.iter().position(std::option::Option::is_none);
        }
        let my_lane = if let Some(l) = my_lane {
            l
        } else {
            lanes.push(None);
            lanes.len() - 1
        };
        // `row` is the loop's own enumeration index, always in bounds for a
        // vec sized to `commits.len()` above.
        if let Some(slot) = lane_of.get_mut(row) {
            *slot = my_lane;
        }
        commit.lane = my_lane;
        // This commit now occupies the lane; clear any further reservation
        // for itself (it's been consumed) before assigning parents.
        // `my_lane` came either from `position()` (bounded by construction)
        // or from a fresh `push`, so it is always a valid index here.
        if let Some(slot) = lanes.get_mut(my_lane) {
            *slot = None;
        }

        let parents = commit.parents.clone();
        // Assign each parent to a lane. The first parent inherits this
        // commit's lane (keeps mainline straight); additional parents
        // (merges) branch off into the first free / new lane.
        for (pi, parent) in parents.iter().enumerate() {
            // Only draw an edge to parents that exist in our window.
            let Some(&to_row) = row_of.get(parent) else {
                continue;
            };

            // Does the parent already have a reserved lane? Reuse it so two
            // children of the same commit converge.
            let existing = lanes
                .iter()
                .position(|l| l.as_deref() == Some(parent.as_str()));
            let to_lane = if let Some(l) = existing {
                l
            } else if pi == 0 {
                // First parent: keep our column, growing the lane vec first
                // if this is a fresh lane that hasn't been indexed into yet.
                if my_lane >= lanes.len() {
                    lanes.resize(my_lane + 1, None);
                }
                if let Some(slot) = lanes.get_mut(my_lane) {
                    *slot = Some(parent.clone());
                }
                my_lane
            } else {
                // Merge parent: allocate a fresh lane.
                let free = lanes.iter().position(std::option::Option::is_none);
                let l = if let Some(l) = free {
                    l
                } else {
                    lanes.push(None);
                    lanes.len() - 1
                };
                // `l` came either from `position()` or from a fresh `push`
                // above, so it is always a valid index here.
                if let Some(slot) = lanes.get_mut(l) {
                    *slot = Some(parent.clone());
                }
                l
            };

            edges.push(GraphEdge {
                from_row: row,
                from_lane: my_lane,
                to_row,
                to_lane,
            });
        }

        max_lanes = max_lanes.max(lanes.len());
    }

    GraphLayout {
        lane_of,
        edges,
        lanes: max_lanes.max(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(id: &str, parents: &[&str]) -> Commit {
        Commit {
            id: id.to_string(),
            summary: id.to_string(),
            body: String::new(),
            author_name: "Test".into(),
            author_email: "t@example.com".into(),
            when: "now".into(),
            time: 0,
            parents: parents
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
            refs: vec![],
            lane: 0,
            files: vec![],
        }
    }

    /// Commit graph lanes: linear history stays in one lane, a merge uses a second
    /// lane, and every row has a lane. Each phase is a former test with its own fresh fixture.
    #[test]
    fn commit_graph_assigns_lanes() {
        linear_history_stays_in_one_lane();
        merge_commit_uses_a_second_lane();
        every_row_has_a_lane();
    }

    fn linear_history_stays_in_one_lane() {
        let mut commits = vec![commit("c", &["b"]), commit("b", &["a"]), commit("a", &[])];
        let layout = layout(&mut commits);
        assert!(layout.lane_of.iter().all(|&l| l == 0));
        assert_eq!(layout.lanes, 1);
        // Two edges: c->b, b->a.
        assert_eq!(layout.edges.len(), 2);
    }

    fn merge_commit_uses_a_second_lane() {
        // m merges feature(f) into main(a); f branched from a.
        let mut commits = vec![
            commit("m", &["main", "f"]),
            commit("f", &["a"]),
            commit("main", &["a"]),
            commit("a", &[]),
        ];
        let layout = layout(&mut commits);
        assert!(layout.lanes >= 2, "merge should open a second lane");
        // The merge row points at two parents → two edges from row 0.
        let from_zero = layout.edges.iter().filter(|e| e.from_row == 0).count();
        assert_eq!(from_zero, 2);
    }

    fn every_row_has_a_lane() {
        let mut commits = vec![commit("c", &["b"]), commit("b", &["a"]), commit("a", &[])];
        let layout = layout(&mut commits);
        assert_eq!(layout.lane_of.len(), commits.len());
    }
}
