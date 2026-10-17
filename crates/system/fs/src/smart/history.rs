//! The mount's access history and the open-time predictor built on it.
//!
//! Every open the mount authorises is recorded here, so the next open can be
//! predicted from what the person actually did rather than from directory
//! order alone. Two signals, both cheap and local:
//!
//! - **sequences**: opens that walk a directory's same-type files in
//!   natural order (`page-9.pdf` then `page-10.pdf`, forwards or backwards)
//!   predict the next files in that direction, further ahead the longer the
//!   run;
//! - **opened together**: files opened within a few minutes of each other on
//!   at least two separate occasions predict each other, wherever they live.
//!
//! The history is per mount (one workspace), bounded, held in memory only
//! and keyed by inode: it never leaves the device and never names a file the
//! caller has not already authorised. It only proposes inodes; the caller
//! resolves them against the live, ACL-filtered namespace before anything is
//! fetched.

use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Opens remembered for sequence and co-access detection.
const HISTORY_LEN: usize = 128;
/// Co-access pairs remembered; the stalest pair makes room for a new one.
const MAX_PAIRS: usize = 2048;
/// How many distinct earlier opens one open can pair with.
const TOGETHER_LOOKBACK: usize = 8;
/// Two opens at most this far apart belong to one sequence.
pub const SEQUENCE_GAP: Duration = Duration::from_secs(10 * 60);
/// Opens this close together are "opened together"; a pair touched again
/// within the window is the same occasion, not a new one.
pub const TOGETHER_WINDOW: Duration = Duration::from_secs(2 * 60);
/// Occasions before a pair is trusted as a prediction.
pub const MIN_TOGETHER_OCCASIONS: u32 = 2;
/// Files ahead after an open that continues no run of three: the next two,
/// so `1, 2` already has `3, 4` on the way.
pub const COLD_LOOKAHEAD: usize = 2;
/// Files ahead once three or more were opened in order: someone paging
/// through a folder outruns a lookahead of two.
pub const RUN_LOOKAHEAD: usize = 4;
/// Co-access partners proposed per open.
pub const MAX_TOGETHER: usize = 2;

#[derive(Clone, Copy, Debug)]
struct Opened {
    dir: u64,
    file: u64,
    at: Instant,
}

#[derive(Clone, Copy, Debug)]
struct Together {
    occasions: u32,
    last: Instant,
}

/// What one open predicts, most useful first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prediction {
    /// Same-type siblings ahead in the detected direction. They come from
    /// the caller's own sibling list.
    pub siblings: Vec<u64>,
    /// Files opened together with this one before. They may live anywhere,
    /// so the caller must authorise each before fetching it.
    pub together: Vec<u64>,
}

/// A bounded, local record of which files were opened when.
#[derive(Debug, Default)]
pub struct AccessHistory {
    recent: VecDeque<Opened>,
    together: HashMap<(u64, u64), Together>,
}

impl AccessHistory {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `file` in directory `dir` was opened at `at`. An
    /// immediate reopen of the same file (an app opening it twice) is one
    /// open.
    pub fn record(&mut self, dir: u64, file: u64, at: Instant) {
        if let Some(last) = self.recent.back_mut()
            && last.file == file
        {
            last.at = at;
            last.dir = dir;
            return;
        }
        let mut partners = Vec::with_capacity(TOGETHER_LOOKBACK);
        for earlier in self.recent.iter().rev() {
            if at.saturating_duration_since(earlier.at) > TOGETHER_WINDOW
                || partners.len() == TOGETHER_LOOKBACK
            {
                break;
            }
            if earlier.file != file && !partners.contains(&earlier.file) {
                partners.push(earlier.file);
            }
        }
        for partner in partners {
            self.note_together(file, partner, at);
        }
        if self.recent.len() == HISTORY_LEN {
            self.recent.pop_front();
        }
        self.recent.push_back(Opened { dir, file, at });
    }

    fn note_together(&mut self, a: u64, b: u64, at: Instant) {
        let key = if a < b { (a, b) } else { (b, a) };
        if let Some(pair) = self.together.get_mut(&key) {
            if at.saturating_duration_since(pair.last) > TOGETHER_WINDOW {
                pair.occasions = pair.occasions.saturating_add(1);
            }
            pair.last = at;
            return;
        }
        if self.together.len() >= MAX_PAIRS {
            self.forget_stalest_pairs();
        }
        self.together.insert(
            key,
            Together {
                occasions: 1,
                last: at,
            },
        );
    }

    /// Make room by forgetting the stalest quarter of the pairs at once, so
    /// a full table costs one pass per [`MAX_PAIRS`] / 4 new pairs rather
    /// than one per pair.
    fn forget_stalest_pairs(&mut self) {
        let mut lasts: Vec<Instant> = self.together.values().map(|pair| pair.last).collect();
        let cut = lasts.len() / 4;
        let (_, threshold, _) = lasts.select_nth_unstable(cut);
        let threshold = *threshold;
        self.together.retain(|_, pair| pair.last > threshold);
    }

    /// Predict what follows the open of `file` in `dir`, which must have
    /// been [recorded](Self::record) already. `siblings` are the file's
    /// visible same-type siblings in [natural order](natural_cmp),
    /// including the file itself.
    #[must_use]
    pub fn predict(&self, dir: u64, file: u64, siblings: &[u64]) -> Prediction {
        let mut prediction = Prediction::default();
        let positions: HashMap<u64, usize> = siblings
            .iter()
            .enumerate()
            .map(|(position, ino)| (*ino, position))
            .collect();
        if let Some(&position) = positions.get(&file) {
            let (direction, run) = self.sequence(dir, position, &positions);
            let lookahead = if run >= 2 {
                RUN_LOOKAHEAD
            } else {
                COLD_LOOKAHEAD
            };
            prediction.siblings = walk(siblings, position, direction, lookahead);
            if run == 0 && prediction.siblings.len() < lookahead {
                // A cold open of the last files looks back instead: the
                // person is likelier to page towards the rest.
                let rest = lookahead.saturating_sub(prediction.siblings.len());
                prediction
                    .siblings
                    .extend(walk(siblings, position, direction.reversed(), rest));
            }
        }
        let mut partners: Vec<(u64, Together)> = self
            .together
            .iter()
            .filter(|(_, pair)| pair.occasions >= MIN_TOGETHER_OCCASIONS)
            .filter_map(|(&(a, b), pair)| {
                let partner = if a == file {
                    b
                } else if b == file {
                    a
                } else {
                    return None;
                };
                Some((partner, *pair))
            })
            .filter(|(partner, _)| !prediction.siblings.contains(partner))
            .collect();
        partners.sort_by(|(a_ino, a), (b_ino, b)| {
            b.occasions
                .cmp(&a.occasions)
                .then(b.last.cmp(&a.last))
                .then(a_ino.cmp(b_ino))
        });
        prediction.together = partners
            .into_iter()
            .take(MAX_TOGETHER)
            .map(|(partner, _)| partner)
            .collect();
        prediction
    }

    /// The direction and length of the run of in-order opens in `dir`
    /// that ends with the latest open, at `position`. No run is
    /// `(Forwards, 0)`: the cold guess looks forwards.
    fn sequence(
        &self,
        dir: u64,
        position: usize,
        positions: &HashMap<u64, usize>,
    ) -> (Direction, usize) {
        let mut opens = self.recent.iter().rev().filter(|opened| opened.dir == dir);
        let Some(latest) = opens.next() else {
            return (Direction::Forwards, 0);
        };
        let (mut direction, mut run) = (None, 0_usize);
        let (mut later_position, mut later_at) = (position, latest.at);
        for earlier in opens {
            if later_at.saturating_duration_since(earlier.at) > SEQUENCE_GAP {
                break;
            }
            let Some(&earlier_position) = positions.get(&earlier.file) else {
                break;
            };
            if earlier_position == later_position {
                // A reopen of the same file between others neither extends
                // nor breaks the run.
                later_at = earlier.at;
                continue;
            }
            let step = if earlier_position.checked_add(1) == Some(later_position) {
                Direction::Forwards
            } else if later_position.checked_add(1) == Some(earlier_position) {
                Direction::Backwards
            } else {
                break;
            };
            if *direction.get_or_insert(step) != step {
                break;
            }
            run = run.saturating_add(1);
            later_position = earlier_position;
            later_at = earlier.at;
        }
        (direction.unwrap_or(Direction::Forwards), run)
    }
}

/// Which way through a directory's natural order a run of opens goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Forwards,
    Backwards,
}

impl Direction {
    const fn reversed(self) -> Self {
        match self {
            Self::Forwards => Self::Backwards,
            Self::Backwards => Self::Forwards,
        }
    }
}

/// Up to `count` entries of `siblings` from `position` (exclusive) in
/// `direction`.
fn walk(siblings: &[u64], position: usize, direction: Direction, count: usize) -> Vec<u64> {
    match direction {
        Direction::Forwards => siblings
            .iter()
            .skip(position.saturating_add(1))
            .take(count)
            .copied()
            .collect(),
        Direction::Backwards => siblings
            .get(..position)
            .unwrap_or_default()
            .iter()
            .rev()
            .take(count)
            .copied()
            .collect(),
    }
}

/// Order file names the way a person reads them: runs of digits compare by
/// value (`page-9` before `page-10`), letters compare case-insensitively,
/// and the raw bytes break any remaining tie so the order is total.
#[must_use]
pub fn natural_cmp(a: &[u8], b: &[u8]) -> Ordering {
    let (mut a_rest, mut b_rest) = (a, b);
    loop {
        let (Some((a_first, a_tail)), Some((b_first, b_tail))) =
            (a_rest.split_first(), b_rest.split_first())
        else {
            return a_rest.len().cmp(&b_rest.len()).then_with(|| a.cmp(b));
        };
        if a_first.is_ascii_digit() && b_first.is_ascii_digit() {
            let (a_digits, a_after) = split_digits(a_rest);
            let (b_digits, b_after) = split_digits(b_rest);
            let (a_number, b_number) = (strip_zeros(a_digits), strip_zeros(b_digits));
            let numeric = a_number
                .len()
                .cmp(&b_number.len())
                .then_with(|| a_number.cmp(b_number));
            if numeric != Ordering::Equal {
                return numeric;
            }
            (a_rest, b_rest) = (a_after, b_after);
            continue;
        }
        let letters = a_first
            .to_ascii_lowercase()
            .cmp(&b_first.to_ascii_lowercase());
        if letters != Ordering::Equal {
            return letters;
        }
        (a_rest, b_rest) = (a_tail, b_tail);
    }
}

/// The leading run of ASCII digits of `bytes`, and what follows it.
fn split_digits(bytes: &[u8]) -> (&[u8], &[u8]) {
    let digits = bytes
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    bytes.split_at_checked(digits).unwrap_or((bytes, &[]))
}

fn strip_zeros(digits: &[u8]) -> &[u8] {
    let zeros = digits.iter().take_while(|digit| **digit == b'0').count();
    digits.get(zeros..).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arithmetic_side_effects, reason = "test fixtures")]

    use super::*;

    const DIR: u64 = 1;

    fn names_in_natural_order(names: &[&str]) -> Vec<String> {
        let mut sorted: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
        sorted.sort_by(|a, b| natural_cmp(a.as_bytes(), b.as_bytes()));
        sorted
    }

    /// Opens `files` one second apart starting at `start`, then predicts
    /// after the last.
    fn open_in_turn(
        history: &mut AccessHistory,
        start: Instant,
        files: &[u64],
        siblings: &[u64],
    ) -> Prediction {
        for (offset, file) in files.iter().enumerate() {
            history.record(DIR, *file, start + Duration::from_secs(offset as u64));
        }
        history.predict(DIR, *files.last().unwrap(), siblings)
    }

    #[test]
    fn natural_order_reads_numbers_by_value() {
        assert_eq!(
            names_in_natural_order(&["page-10.pdf", "page-2.pdf", "page-1.pdf", "Page-3.pdf"]),
            ["page-1.pdf", "page-2.pdf", "Page-3.pdf", "page-10.pdf"]
        );
        assert_eq!(
            names_in_natural_order(&["IMG_0100.jpg", "IMG_0099.jpg", "IMG_99.jpg"]),
            ["IMG_0099.jpg", "IMG_99.jpg", "IMG_0100.jpg"],
            "leading zeros tie on value and fall back to the bytes"
        );
        assert_eq!(
            names_in_natural_order(&["b", "a10", "a9b", "a9", "a"]),
            ["a", "a9", "a9b", "a10", "b"]
        );
        assert_eq!(natural_cmp(b"x", b"x"), Ordering::Equal);
        assert_ne!(
            natural_cmp(b"X", b"x"),
            Ordering::Equal,
            "the order is total"
        );
    }

    #[test]
    fn a_cold_open_predicts_the_next_two_files() {
        let siblings = [10, 11, 12, 13, 14];
        let mut history = AccessHistory::new();
        let prediction = open_in_turn(&mut history, Instant::now(), &[11], &siblings);
        assert_eq!(prediction.siblings, [12, 13]);
        assert!(prediction.together.is_empty());
    }

    #[test]
    fn a_sequence_predicts_further_ahead_the_longer_it_runs() {
        let siblings: Vec<u64> = (100..112).collect();
        let start = Instant::now();
        let mut history = AccessHistory::new();
        assert_eq!(
            open_in_turn(&mut history, start, &[100, 101], &siblings).siblings,
            [102, 103],
            "1, 2 → 3, 4"
        );
        let mut history = AccessHistory::new();
        assert_eq!(
            open_in_turn(&mut history, start, &[100, 101, 102], &siblings).siblings,
            [103, 104, 105, 106]
        );
        let mut history = AccessHistory::new();
        assert_eq!(
            open_in_turn(&mut history, start, &[108, 109, 110], &siblings).siblings,
            [111],
            "a run's lookahead stops at the last file"
        );
        let mut history = AccessHistory::new();
        assert_eq!(
            open_in_turn(&mut history, start, &[111], &siblings).siblings,
            [110, 109],
            "a cold open of the last file looks back"
        );
        let mut history = AccessHistory::new();
        assert_eq!(
            open_in_turn(&mut history, start, &[110], &siblings).siblings,
            [111, 109]
        );
    }

    #[test]
    fn a_backwards_sequence_predicts_backwards() {
        let siblings: Vec<u64> = (100..112).collect();
        let mut history = AccessHistory::new();
        assert_eq!(
            open_in_turn(&mut history, Instant::now(), &[106, 105, 104], &siblings).siblings,
            [103, 102, 101, 100]
        );
    }

    #[test]
    fn jumps_stale_opens_and_other_directories_break_a_sequence() {
        let siblings: Vec<u64> = (100..112).collect();
        let start = Instant::now();
        let mut history = AccessHistory::new();
        assert_eq!(
            open_in_turn(&mut history, start, &[100, 101, 105], &siblings).siblings,
            [106, 107],
            "a jump ends the run"
        );

        let mut history = AccessHistory::new();
        history.record(DIR, 100, start);
        let later = start + SEQUENCE_GAP + Duration::from_secs(1);
        history.record(DIR, 101, later);
        history.record(DIR, 102, later + Duration::from_secs(1));
        assert_eq!(
            history.predict(DIR, 102, &siblings).siblings,
            [103, 104],
            "an open long ago is not part of the run"
        );

        let mut history = AccessHistory::new();
        for (offset, (dir, file)) in [(DIR, 100), (2, 500), (DIR, 101), (2, 501), (DIR, 102)]
            .into_iter()
            .enumerate()
        {
            history.record(dir, file, start + Duration::from_secs(offset as u64));
        }
        assert_eq!(
            history.predict(DIR, 102, &siblings).siblings,
            [103, 104, 105, 106],
            "opens elsewhere in between do not break the run"
        );

        let mut history = AccessHistory::new();
        for (offset, (dir, file)) in [(DIR, 100), (DIR, 101), (2, 500), (DIR, 101), (DIR, 102)]
            .into_iter()
            .enumerate()
        {
            history.record(dir, file, start + Duration::from_secs(offset as u64));
        }
        assert_eq!(
            history.predict(DIR, 102, &siblings).siblings,
            [103, 104, 105, 106],
            "a reopen of the same file is one open"
        );
    }

    #[test]
    fn files_opened_together_on_two_occasions_predict_each_other() {
        let start = Instant::now();
        let mut history = AccessHistory::new();
        // The report and its figure, opened together this morning.
        history.record(DIR, 7, start);
        history.record(2, 900, start + Duration::from_secs(20));
        assert!(
            history.predict(DIR, 7, &[7]).together.is_empty(),
            "one occasion is not a habit"
        );
        // Opening both again inside the window is still the same occasion.
        history.record(DIR, 7, start + Duration::from_secs(40));
        history.record(2, 900, start + Duration::from_secs(50));
        assert!(history.predict(DIR, 7, &[7]).together.is_empty());

        // The next afternoon, the report alone is enough to predict the
        // figure, and the figure the report.
        let later = start + Duration::from_secs(6 * 3600);
        history.record(DIR, 7, later);
        history.record(2, 900, later + Duration::from_secs(30));
        history.record(3, 42, later + TOGETHER_WINDOW * 2);
        history.record(DIR, 7, later + TOGETHER_WINDOW * 4);
        assert_eq!(history.predict(DIR, 7, &[7]).together, [900]);
        assert_eq!(history.predict(2, 900, &[900]).together, [7]);
        assert!(
            history.predict(3, 42, &[42]).together.is_empty(),
            "a file opened alone pairs with nothing"
        );
    }

    #[test]
    fn co_access_partners_already_predicted_as_siblings_are_not_repeated() {
        let siblings = [1, 2, 3];
        let start = Instant::now();
        let mut history = AccessHistory::new();
        for occasion in 0..2_u64 {
            let at = start + Duration::from_secs(occasion * 3600);
            history.record(DIR, 1, at);
            history.record(DIR, 2, at + Duration::from_secs(5));
        }
        history.record(DIR, 1, start + Duration::from_secs(3 * 3600));
        let prediction = history.predict(DIR, 1, &siblings);
        assert_eq!(prediction.siblings, [2, 3]);
        assert!(prediction.together.is_empty());
    }

    #[test]
    fn the_history_is_bounded() {
        let start = Instant::now();
        let mut history = AccessHistory::new();
        for file in 0..(MAX_PAIRS as u64 * 2) {
            history.record(DIR, file, start + Duration::from_millis(file));
        }
        assert!(history.recent.len() <= HISTORY_LEN);
        assert!(history.together.len() <= MAX_PAIRS);
    }
}
