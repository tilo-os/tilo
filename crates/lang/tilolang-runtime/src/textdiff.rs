//! A small character diff, so a changed text field becomes a few splices instead of a
//! rewrite of the whole CRDT text (which would leave a tombstone for every character).
//!
//! The common prefix and suffix are trimmed first, which settles almost every keystroke.
//! What is left is diffed by longest common subsequence while the table stays small, and
//! replaced by a single splice beyond that: still only the differing middle, never the
//! whole text.

/// Largest middle (in cells of the LCS table) that is diffed exactly.
const MAX_CELLS: usize = 1 << 20;

/// Replace `delete` characters at character index `at` of the old text with `insert`.
/// Indexes refer to the old text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Splice {
    pub at: usize,
    pub delete: usize,
    pub insert: String,
}

/// The splices that turn `old` into `new`, ascending and disjoint. Apply them last to first
/// so earlier indexes stay valid.
pub fn diff(old: &str, new: &str) -> Vec<Splice> {
    let a: Vec<char> = old.chars().collect();
    let b: Vec<char> = new.chars().collect();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let room = a.len().min(b.len()).saturating_sub(prefix);
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(room)
        .take_while(|(x, y)| x == y)
        .count();
    let a_mid = a
        .get(prefix..a.len().saturating_sub(suffix))
        .unwrap_or_default();
    let b_mid = b
        .get(prefix..b.len().saturating_sub(suffix))
        .unwrap_or_default();
    if a_mid.is_empty() && b_mid.is_empty() {
        return Vec::new();
    }
    lcs_splices(a_mid, b_mid, prefix).unwrap_or_else(|| {
        vec![Splice {
            at: prefix,
            delete: a_mid.len(),
            insert: b_mid.iter().collect(),
        }]
    })
}

struct Table {
    width: usize,
    cells: Vec<u32>,
}

impl Table {
    fn get(&self, i: usize, j: usize) -> u32 {
        let at = i.saturating_mul(self.width).saturating_add(j);
        self.cells.get(at).copied().unwrap_or(0)
    }

    fn set(&mut self, i: usize, j: usize, v: u32) {
        let at = i.saturating_mul(self.width).saturating_add(j);
        if let Some(c) = self.cells.get_mut(at) {
            *c = v;
        }
    }
}

/// Longest-common-subsequence lengths of every pair of suffixes.
fn lcs_table(a: &[char], b: &[char]) -> Option<Table> {
    let width = b.len().checked_add(1)?;
    let height = a.len().checked_add(1)?;
    let size = width.checked_mul(height)?;
    if size > MAX_CELLS {
        return None;
    }
    let mut t = Table {
        width,
        cells: vec![0; size],
    };
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            let v = if a.get(i) == b.get(j) {
                t.get(i.saturating_add(1), j.saturating_add(1))
                    .saturating_add(1)
            } else {
                t.get(i.saturating_add(1), j)
                    .max(t.get(i, j.saturating_add(1)))
            };
            t.set(i, j, v);
        }
    }
    Some(t)
}

fn lcs_splices(from: &[char], to: &[char], base: usize) -> Option<Vec<Splice>> {
    let table = lcs_table(from, to)?;
    let (mut i, mut j) = (0usize, 0usize);
    let mut out = Vec::new();
    let mut open: Option<Splice> = None;
    while i < from.len() || j < to.len() {
        if i < from.len() && j < to.len() && from.get(i) == to.get(j) {
            out.extend(open.take());
            i = i.saturating_add(1);
            j = j.saturating_add(1);
            continue;
        }
        let delete = j >= to.len()
            || (i < from.len()
                && table.get(i.saturating_add(1), j) >= table.get(i, j.saturating_add(1)));
        let run = open.get_or_insert_with(|| Splice {
            at: base.saturating_add(i),
            delete: 0,
            insert: String::new(),
        });
        if delete {
            run.delete = run.delete.saturating_add(1);
            i = i.saturating_add(1);
        } else {
            run.insert.extend(to.get(j));
            j = j.saturating_add(1);
        }
    }
    out.extend(open);
    Some(out)
}

/// Apply splices to a string, for tests and for checking a diff.
pub fn apply(old: &str, splices: &[Splice]) -> String {
    let mut chars: Vec<char> = old.chars().collect();
    for s in splices.iter().rev() {
        let end = s.at.saturating_add(s.delete).min(chars.len());
        let start = s.at.min(end);
        chars.splice(start..end, s.insert.chars());
    }
    chars.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(old: &str, new: &str) -> Vec<Splice> {
        let d = diff(old, new);
        assert_eq!(apply(old, &d), new, "{old:?} -> {new:?}: {d:?}");
        d
    }

    #[test]
    fn a_keystroke_is_one_small_splice() {
        let d = check("hello world", "hello, world");
        assert_eq!(
            d,
            vec![Splice {
                at: 5,
                delete: 0,
                insert: ",".into()
            }]
        );
    }

    #[test]
    fn identical_text_has_no_splices() {
        assert!(check("same", "same").is_empty());
    }

    #[test]
    fn scattered_edits_stay_separate() {
        let d = check("the quick brown fox", "the slow brown cat");
        assert!(d.len() >= 2, "{d:?}");
        let touched: usize = d.iter().map(|s| s.delete).sum();
        assert!(touched < 19, "must not rewrite the whole text: {d:?}");
    }

    #[test]
    fn unicode_and_empty_cases() {
        check("", "abc");
        check("abc", "");
        check("naive café", "naïve cafe");
        check("a\u{1F600}b", "a\u{1F600}\u{1F600}b");
    }

    #[test]
    fn a_huge_middle_falls_back_to_one_splice() {
        let old = "a".repeat(3000);
        let new = "b".repeat(3000);
        let d = check(&old, &new);
        assert_eq!(d.len(), 1);
    }
}
