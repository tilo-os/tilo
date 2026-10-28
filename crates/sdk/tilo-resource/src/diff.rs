//! Structural diffs between two versions of the same resource.
//!
//! Items in a resource keep their identity across every edit, so two versions
//! align by id rather than by fuzzy content matching: [`align`] pairs the
//! items both versions hold and names the ones only one side has. Text within
//! a paired item diffs by character ([`text_runs`]).

use std::collections::HashMap;
use std::hash::Hash;

use serde::{Deserialize, Serialize};

/// One step of an alignment of two keyed sequences, in reading order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step<'a, K> {
    /// Present in both versions, in the same relative order.
    Both(&'a K),
    /// Only in the old version (removed, or moved out of order).
    OldOnly(&'a K),
    /// Only in the new version (added, or moved out of order).
    NewOnly(&'a K),
}

/// Align two sequences of item ids: the longest run of ids both keep in the
/// same order is [`Step::Both`], the rest is removed or added. Between two
/// kept ids, the removed ones come first.
///
/// Ids are expected to be unique within each sequence (a repeated id pairs
/// once). Common leading and trailing runs are paired directly, so the usual
/// local edit costs as much as the edit; the middle aligns in
/// `O(n log n)` as the longest increasing run of new positions.
pub fn align<'a, K: Eq + Hash>(old: &'a [K], new: &'a [K]) -> Vec<Step<'a, K>> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let (old_rest, new_rest) = (
        old.get(prefix..).unwrap_or(&[]),
        new.get(prefix..).unwrap_or(&[]),
    );
    let suffix = old_rest
        .iter()
        .rev()
        .zip(new_rest.iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old_mid = old_rest
        .get(..old_rest.len().saturating_sub(suffix))
        .unwrap_or(&[]);
    let new_mid = new_rest
        .get(..new_rest.len().saturating_sub(suffix))
        .unwrap_or(&[]);

    let mut steps = Vec::with_capacity(old.len().max(new.len()));
    steps.extend(old.get(..prefix).unwrap_or(&[]).iter().map(Step::Both));

    let mut position: HashMap<&K, usize> = HashMap::with_capacity(new_mid.len());
    for (index, key) in new_mid.iter().enumerate() {
        position.entry(key).or_insert(index);
    }
    let targets: Vec<Option<usize>> = old_mid
        .iter()
        .map(|key| position.get(key).copied())
        .collect();
    let pairs = increasing_pairs(&targets);
    let (mut next_old, mut next_new) = (0usize, 0usize);
    for (old_index, new_index) in pairs {
        steps.extend(
            old_mid
                .get(next_old..old_index)
                .unwrap_or(&[])
                .iter()
                .map(Step::OldOnly),
        );
        steps.extend(
            new_mid
                .get(next_new..new_index)
                .unwrap_or(&[])
                .iter()
                .map(Step::NewOnly),
        );
        if let Some(key) = new_mid.get(new_index) {
            steps.push(Step::Both(key));
        }
        next_old = old_index.saturating_add(1);
        next_new = new_index.saturating_add(1);
    }
    steps.extend(
        old_mid
            .get(next_old..)
            .unwrap_or(&[])
            .iter()
            .map(Step::OldOnly),
    );
    steps.extend(
        new_mid
            .get(next_new..)
            .unwrap_or(&[])
            .iter()
            .map(Step::NewOnly),
    );
    steps.extend(
        old_rest
            .get(old_rest.len().saturating_sub(suffix)..)
            .unwrap_or(&[])
            .iter()
            .map(Step::Both),
    );
    steps
}

/// `(old index, new index)` of one longest strictly increasing run of the
/// present `targets`, in order.
fn increasing_pairs(targets: &[Option<usize>]) -> Vec<(usize, usize)> {
    let mut tails: Vec<usize> = Vec::new();
    let mut previous: Vec<Option<usize>> = vec![None; targets.len()];
    for (index, target) in targets.iter().enumerate() {
        let Some(target) = *target else { continue };
        let slot = tails.partition_point(|&tail| {
            targets
                .get(tail)
                .copied()
                .flatten()
                .is_some_and(|t| t < target)
        });
        if let Some(before) = slot.checked_sub(1)
            && let Some(link) = previous.get_mut(index)
        {
            *link = tails.get(before).copied();
        }
        if slot == tails.len() {
            tails.push(index);
        } else if let Some(tail) = tails.get_mut(slot) {
            *tail = index;
        }
    }
    let mut chain = Vec::with_capacity(tails.len());
    let mut cursor = tails.last().copied();
    while let Some(index) = cursor {
        if let Some(Some(target)) = targets.get(index) {
            chain.push((index, *target));
        }
        cursor = previous.get(index).copied().flatten();
    }
    chain.reverse();
    chain
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextRun {
    pub op: RunOp,
    pub text: String,
}

/// The kind of a [`TextRun`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunOp {
    Equal,
    Insert,
    Delete,
}

/// Above this many differing characters a text diff falls back to a coarse
/// whole-span replace, so a pathologically long edit can't drive the
/// quadratic character alignment.
const RUN_DIFF_CHAR_CAP: usize = 8_192;

/// Character-level diff of two texts into `Equal`/`Insert`/`Delete` runs: the
/// common start and end are equal, the span between them aligns by longest
/// common subsequence (or, past a size cap, is replaced whole).
pub fn text_runs(old: &str, new: &str) -> Vec<TextRun> {
    let a: Vec<char> = old.chars().collect();
    let b: Vec<char> = new.chars().collect();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a
        .get(prefix..)
        .unwrap_or(&[])
        .iter()
        .rev()
        .zip(b.get(prefix..).unwrap_or(&[]).iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let a_mid = a.get(prefix..a.len().saturating_sub(suffix)).unwrap_or(&[]);
    let b_mid = b.get(prefix..b.len().saturating_sub(suffix)).unwrap_or(&[]);

    let mut ops: Vec<(RunOp, char)> = Vec::with_capacity(a.len().max(b.len()));
    ops.extend(
        a.get(..prefix)
            .unwrap_or(&[])
            .iter()
            .map(|c| (RunOp::Equal, *c)),
    );
    if a_mid.len() > RUN_DIFF_CHAR_CAP || b_mid.len() > RUN_DIFF_CHAR_CAP {
        ops.extend(a_mid.iter().map(|c| (RunOp::Delete, *c)));
        ops.extend(b_mid.iter().map(|c| (RunOp::Insert, *c)));
    } else {
        ops.extend(lcs_char_ops(a_mid, b_mid));
    }
    ops.extend(
        a.get(a.len().saturating_sub(suffix)..)
            .unwrap_or(&[])
            .iter()
            .map(|c| (RunOp::Equal, *c)),
    );
    coalesce(ops)
}

/// `dp[i][j]`, or `0` past either edge.
fn dp_at(dp: &[Vec<u32>], i: usize, j: usize) -> u32 {
    dp.get(i).and_then(|row| row.get(j)).copied().unwrap_or(0)
}

/// Per-character LCS edit script over two char slices.
fn lcs_char_ops(old: &[char], new: &[char]) -> Vec<(RunOp, char)> {
    let n = old.len();
    let m = new.len();
    let mut dp = vec![vec![0u32; m.saturating_add(1)]; n.saturating_add(1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let value = match (old.get(i).copied(), new.get(j).copied()) {
                (Some(oi), Some(nj)) if oi == nj => {
                    dp_at(&dp, i.saturating_add(1), j.saturating_add(1)).saturating_add(1)
                }
                _ => dp_at(&dp, i.saturating_add(1), j).max(dp_at(&dp, i, j.saturating_add(1))),
            };
            if let Some(cell) = dp.get_mut(i).and_then(|row| row.get_mut(j)) {
                *cell = value;
            }
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        let (Some(oi), Some(nj)) = (old.get(i).copied(), new.get(j).copied()) else {
            break;
        };
        if oi == nj {
            ops.push((RunOp::Equal, oi));
            i = i.saturating_add(1);
            j = j.saturating_add(1);
        } else if dp_at(&dp, i.saturating_add(1), j) >= dp_at(&dp, i, j.saturating_add(1)) {
            ops.push((RunOp::Delete, oi));
            i = i.saturating_add(1);
        } else {
            ops.push((RunOp::Insert, nj));
            j = j.saturating_add(1);
        }
    }
    ops.extend(
        old.get(i..)
            .unwrap_or(&[])
            .iter()
            .map(|c| (RunOp::Delete, *c)),
    );
    ops.extend(
        new.get(j..)
            .unwrap_or(&[])
            .iter()
            .map(|c| (RunOp::Insert, *c)),
    );
    ops
}

/// Merge a per-character op stream into contiguous runs.
fn coalesce(ops: Vec<(RunOp, char)>) -> Vec<TextRun> {
    let mut runs: Vec<TextRun> = Vec::new();
    for (op, ch) in ops {
        match runs.last_mut() {
            Some(run) if run.op == op => run.text.push(ch),
            _ => runs.push(TextRun {
                op,
                text: ch.to_string(),
            }),
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(steps: &[Step<'_, &str>]) -> String {
        steps
            .iter()
            .map(|step| match step {
                Step::Both(k) => format!("={k}"),
                Step::OldOnly(k) => format!("-{k}"),
                Step::NewOnly(k) => format!("+{k}"),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn a_local_edit_aligns_everything_else() {
        let old = ["a", "b", "c", "d", "e"];
        let new = ["a", "b", "x", "d", "e"];
        assert_eq!(render(&align(&old, &new)), "=a =b -c +x =d =e");
        let new = ["a", "d", "b", "c", "e"];
        assert_eq!(render(&align(&old, &new)), "=a +d =b =c -d =e");
    }

    #[test]
    fn disjoint_and_empty_sides() {
        let none: [&str; 0] = [];
        assert_eq!(render(&align(&["a", "b"], &none)), "-a -b");
        assert_eq!(render(&align(&none, &["a"])), "+a");
        assert_eq!(render(&align(&["a"], &["b"])), "-a +b");
    }

    #[test]
    fn alignment_keeps_a_longest_common_order_on_large_inputs() {
        let old: Vec<String> = (0..20_000).map(|n| format!("id{n}")).collect();
        let mut new = old.clone();
        new.remove(5_000);
        new.insert(15_000, "fresh".into());
        let moved = new.remove(100);
        new.push(moved);
        let steps = align(&old, &new);
        let both = steps.iter().filter(|s| matches!(s, Step::Both(_))).count();
        assert_eq!(both, 20_000 - 2);
        let added = steps
            .iter()
            .filter(|s| matches!(s, Step::NewOnly(_)))
            .count();
        assert_eq!(added, 2);
    }

    #[test]
    fn text_runs_keep_the_common_start_and_end() {
        let runs = text_runs("the quick fox", "the quick brown fox");
        assert_eq!(
            runs,
            [
                TextRun {
                    op: RunOp::Equal,
                    text: "the quick ".into()
                },
                TextRun {
                    op: RunOp::Insert,
                    text: "brown ".into()
                },
                TextRun {
                    op: RunOp::Equal,
                    text: "fox".into()
                },
            ]
        );
        assert!(text_runs("", "").is_empty());
        assert_eq!(
            text_runs("ab", "b").first().map(|r| r.op),
            Some(RunOp::Delete)
        );
    }
}
