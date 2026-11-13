//! Commit-graph lane assignment: from a newest-first list of commits with parents,
//! which lane each dot occupies and the edges connecting a row to its parents'
//! rows, by the active-lanes sweep most git GUIs use. The output is purely
//! positional.

use crate::model::Commit;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphEdge {
    pub from_row: usize,
    pub from_lane: usize,
    pub to_row: usize,
    pub to_lane: usize,
}

#[derive(Clone, Debug, Default)]
pub struct GraphLayout {
    pub lane_of: Vec<usize>,
    pub edges: Vec<GraphEdge>,
    pub lanes: usize,
}

/// Compute the lane layout for `commits`, assumed newest first; mutates `commit.lane` for callers
/// that carry it on the model.
pub fn layout(commits: &mut [Commit]) -> GraphLayout {
    let mut row_of: std::collections::HashMap<String, usize> =
        std::collections::HashMap::with_capacity(commits.len());
    for (i, c) in commits.iter().enumerate() {
        row_of.insert(c.id.clone(), i);
    }

    // Active lanes: each slot holds the commit id that lane is reserved for, or `None` when it is
    // free.
    let mut lanes: Vec<Option<String>> = Vec::new();
    let mut lane_of = vec![0usize; commits.len()];
    let mut edges: Vec<GraphEdge> = Vec::new();
    let mut max_lanes = 0usize;

    for (row, commit) in commits.iter_mut().enumerate() {
        let id = commit.id.clone();

        let mut my_lane = lanes.iter().position(|l| l.as_deref() == Some(id.as_str()));
        if my_lane.is_none() {
            // Not reserved (a tip or branch head): take the first free lane.
            my_lane = lanes.iter().position(std::option::Option::is_none);
        }
        let my_lane = if let Some(l) = my_lane {
            l
        } else {
            lanes.push(None);
            lanes.len() - 1
        };
        // `row` is the loop's own index, always in bounds for a vec sized to `commits.len()`.
        if let Some(slot) = lane_of.get_mut(row) {
            *slot = my_lane;
        }
        commit.lane = my_lane;
        // This commit now occupies the lane; clear any reservation for itself before assigning
        // parents. `my_lane` came from `position()` or a fresh `push`, so it is a valid index.
        if let Some(slot) = lanes.get_mut(my_lane) {
            *slot = None;
        }

        let parents = commit.parents.clone();
        // Assign each parent to a lane: the first parent inherits this commit's lane, and
        // additional parents branch off into the first free or a new lane.
        for (pi, parent) in parents.iter().enumerate() {
            // Only draw an edge to parents inside the window.
            let Some(&to_row) = row_of.get(parent) else {
                continue;
            };

            // Reuse a parent's reserved lane so two children of the same commit converge.
            let existing = lanes
                .iter()
                .position(|l| l.as_deref() == Some(parent.as_str()));
            let to_lane = if let Some(l) = existing {
                l
            } else if pi == 0 {
                // First parent: keep our column, growing the lane vec first when this is a fresh
                // lane.
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
                // `l` came from `position()` or a fresh `push`, so it is a valid index.
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
