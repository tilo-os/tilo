//! What was read of a workspace's last few governance states, remembered by
//! their heads, per workspace.
//!
//! A change is read where it is written and again where it is taken in, and
//! the state after it is read from the one before it. A host holds many
//! workspaces at once, so each keeps its own last states: one busy workspace
//! never pushes another's out, and a write finds the state it was made on.

use std::collections::{HashMap, VecDeque};

/// One workspace's kept states, the oldest first, each with its heads.
type States<T> = VecDeque<(Vec<automerge::ChangeHash>, T)>;

/// The last states read of each workspace, at most `per` of each, of at most
/// `workspaces` workspaces (the one read longest ago let go first).
pub(crate) struct KeptStates<T> {
    per: usize,
    workspaces: usize,
    clock: u64,
    /// Each workspace's states, with when it was last read or kept.
    kept: HashMap<String, (u64, States<T>)>,
}

impl<T: Clone> KeptStates<T> {
    pub(crate) fn new(per: usize, workspaces: usize) -> Self {
        Self {
            per,
            workspaces,
            clock: 0,
            kept: HashMap::new(),
        }
    }

    /// What was kept of `workspace` at `heads`.
    pub(crate) fn recall(&mut self, workspace: &str, heads: &[automerge::ChangeHash]) -> Option<T> {
        self.clock = self.clock.wrapping_add(1);
        let (used, states) = self.kept.get_mut(workspace)?;
        *used = self.clock;
        states
            .iter()
            .find(|(at, _)| at.as_slice() == heads)
            .map(|(_, value)| value.clone())
    }

    /// Every state kept of `workspace`, the newest first.
    pub(crate) fn all(&self, workspace: &str) -> Vec<(Vec<automerge::ChangeHash>, T)> {
        self.kept
            .get(workspace)
            .map(|(_, states)| states.iter().rev().cloned().collect())
            .unwrap_or_default()
    }

    /// Keep `value`, what `workspace` at `heads` reads as.
    pub(crate) fn keep(&mut self, workspace: &str, heads: Vec<automerge::ChangeHash>, value: T) {
        self.clock = self.clock.wrapping_add(1);
        if !self.kept.contains_key(workspace) && self.kept.len() >= self.workspaces {
            let oldest = self
                .kept
                .iter()
                .min_by_key(|(_, (used, _))| *used)
                .map(|(workspace, _)| workspace.clone());
            if let Some(oldest) = oldest {
                self.kept.remove(&oldest);
            }
        }
        let (used, states) = self.kept.entry(workspace.to_owned()).or_default();
        *used = self.clock;
        if states.len() >= self.per {
            states.pop_front();
        }
        states.push_back((heads, value));
    }
}

#[cfg(test)]
mod tests {
    use super::KeptStates;

    fn heads(n: u8) -> Vec<automerge::ChangeHash> {
        vec![automerge::ChangeHash([n; 32])]
    }

    #[test]
    fn a_busy_workspace_keeps_no_other_workspaces_states_out() {
        let mut kept = KeptStates::new(2, 2);
        kept.keep("quiet", heads(1), 1);
        for n in 10..20 {
            kept.keep("busy", heads(n), u32::from(n));
        }
        assert_eq!(kept.recall("quiet", &heads(1)), Some(1));
        assert_eq!(kept.recall("busy", &heads(19)), Some(19));
        assert_eq!(
            kept.recall("busy", &heads(17)),
            None,
            "two of each are kept"
        );
        assert_eq!(
            kept.all("busy")
                .into_iter()
                .map(|(_, n)| n)
                .collect::<Vec<_>>(),
            vec![19, 18]
        );
        // A third workspace lets the one read longest ago go.
        kept.recall("busy", &heads(19));
        kept.keep("new", heads(30), 30);
        assert_eq!(kept.recall("quiet", &heads(1)), None);
        assert_eq!(kept.recall("busy", &heads(19)), Some(19));
    }
}
