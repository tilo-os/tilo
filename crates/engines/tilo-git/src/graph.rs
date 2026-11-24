//! Commit-graph column assignment: from a newest-first list of commits with parents,
//! which column each dot occupies, by the active-columns sweep most git GUIs use. The
//! output is purely positional.

use crate::model::Commit;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GraphLayout {
    pub columns: usize,
}

/// Compute the column layout for `commits`, assumed newest first; mutates `commit.column` for callers
/// that carry it on the model.
pub fn layout(commits: &mut [Commit]) -> GraphLayout {
    let mut row_of: std::collections::HashMap<String, usize> =
        std::collections::HashMap::with_capacity(commits.len());
    for (i, c) in commits.iter().enumerate() {
        row_of.insert(c.id.clone(), i);
    }

    // Active columns: each slot holds the commit id that column is reserved for, or `None` when it is
    // free.
    let mut columns: Vec<Option<String>> = Vec::new();
    let mut max_columns = 0usize;

    for commit in commits.iter_mut() {
        let id = commit.id.clone();

        let mut my_column = columns
            .iter()
            .position(|l| l.as_deref() == Some(id.as_str()));
        if my_column.is_none() {
            // Not reserved (a tip or branch head): take the first free column.
            my_column = columns.iter().position(std::option::Option::is_none);
        }
        let my_column = if let Some(l) = my_column {
            l
        } else {
            columns.push(None);
            columns.len() - 1
        };
        commit.column = my_column;
        // This commit now occupies the column; clear any reservation for itself before assigning
        // parents. `my_column` came from `position()` or a fresh `push`, so it is a valid index.
        if let Some(slot) = columns.get_mut(my_column) {
            *slot = None;
        }

        let parents = commit.parents.clone();
        // Assign each parent to a column: the first parent inherits this commit's column, and
        // additional parents branch off into the first free or a new column.
        for (pi, parent) in parents.iter().enumerate() {
            // Only draw an edge to parents inside the window.
            let Some(&to_row) = row_of.get(parent) else {
                continue;
            };

            // Reuse a parent's reserved column so two children of the same commit converge.
            let existing = columns
                .iter()
                .position(|l| l.as_deref() == Some(parent.as_str()));
            let _to_column = if let Some(l) = existing {
                l
            } else if pi == 0 {
                // First parent: keep our column, growing the column vec first when this is a fresh
                // column.
                if my_column >= columns.len() {
                    columns.resize(my_column + 1, None);
                }
                if let Some(slot) = columns.get_mut(my_column) {
                    *slot = Some(parent.clone());
                }
                my_column
            } else {
                // Merge parent: allocate a fresh column.
                let free = columns.iter().position(std::option::Option::is_none);
                let l = if let Some(l) = free {
                    l
                } else {
                    columns.push(None);
                    columns.len() - 1
                };
                // `l` came from `position()` or a fresh `push`, so it is a valid index.
                if let Some(slot) = columns.get_mut(l) {
                    *slot = Some(parent.clone());
                }
                l
            };

            let _ = to_row;
        }

        max_columns = max_columns.max(columns.len());
    }

    GraphLayout {
        columns: max_columns.max(1),
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
            age: 0,
            date: String::new(),
            time: 0,
            parents: parents
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
            refs: vec![],
            column: 0,
        }
    }

    /// Commit graph columns: linear history stays in one column, a merge uses a second
    /// column, and every row has a column. Each phase is a former test with its own fresh fixture.
    #[test]
    fn commit_graph_assigns_columns() {
        linear_history_stays_in_one_column();
        merge_commit_uses_a_second_column();
    }

    fn linear_history_stays_in_one_column() {
        let mut commits = vec![commit("c", &["b"]), commit("b", &["a"]), commit("a", &[])];
        let layout = layout(&mut commits);
        assert!(commits.iter().all(|c| c.column == 0));
        assert_eq!(layout.columns, 1);
    }

    fn merge_commit_uses_a_second_column() {
        // m merges feature(f) into main(a); f branched from a.
        let mut commits = vec![
            commit("m", &["main", "f"]),
            commit("f", &["a"]),
            commit("main", &["a"]),
            commit("a", &[]),
        ];
        let layout = layout(&mut commits);
        assert!(layout.columns >= 2, "merge should open a second column");
        assert!(
            commits.iter().all(|c| c.column < layout.columns),
            "every row has a column of the layout"
        );
    }
}
