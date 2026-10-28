//! The measurements a qualification run records, and what they may claim.

use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use crate::budgets::Budget;

/// One side of a paired measurement: a wall-clock duration and its outcome.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timed {
    pub millis: u64,
    /// Whether the operation did what it was asked, before it was timed.
    pub correct: bool,
}

impl Timed {
    #[must_use]
    pub fn of(started: std::time::Instant, correct: bool) -> Self {
        Self {
            millis: millis(started.elapsed()),
            correct,
        }
    }
}

/// Whole milliseconds, saturating: a receipt never claims more time than a
/// `u64` of milliseconds can hold.
#[must_use]
pub fn millis(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

/// One comparable quantity: the Tilo path against the Git baseline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comparison {
    pub reference_millis: u64,
    pub candidate_millis: u64,
    /// The reference got the right answer, so the pair means something.
    pub reference_correct: bool,
    pub candidate_correct: bool,
}

impl Comparison {
    #[must_use]
    pub fn new(reference: Timed, candidate: Timed) -> Self {
        Self {
            reference_millis: reference.millis,
            candidate_millis: candidate.millis,
            reference_correct: reference.correct,
            candidate_correct: candidate.correct,
        }
    }

    /// How long the candidate took per unit of the reference, as a ratio.
    /// `None` when the reference took no measurable time, where a ratio says
    /// nothing.
    #[must_use]
    pub fn ratio(&self) -> Option<f64> {
        // Milliseconds are whole numbers far below 2^52, so the conversion is
        // exact; the ratio itself is only ever a human-readable summary.
        (self.reference_millis > 0).then(|| {
            let candidate = f64::from(u32::try_from(self.candidate_millis).unwrap_or(u32::MAX));
            let reference = f64::from(u32::try_from(self.reference_millis).unwrap_or(u32::MAX));
            candidate / reference
        })
    }

    /// Whether the pair is a like-for-like comparison: both did what they
    /// were asked. A faster wrong answer is not a pass.
    #[must_use]
    pub fn like_for_like(&self) -> bool {
        self.reference_correct && self.candidate_correct
    }
}

/// What a run observed about one quantity: the paired comparison and the
/// budget it must meet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonLine {
    pub name: String,
    pub comparison: Comparison,
    pub budget: Budget,
    /// Free text: the counts behind the number, so a reader can tell a
    /// 21 000-file import from a 200-file one.
    pub detail: String,
}

impl ComparisonLine {
    /// The budget's decision. Both sides must be right, and the candidate
    /// must be inside its ceiling.
    #[must_use]
    pub fn verdict(&self) -> Verdict {
        if !self.comparison.like_for_like() {
            return Verdict::Wrong;
        }
        if self.budget.permits(&self.comparison) {
            Verdict::Met
        } else {
            Verdict::Exceeded
        }
    }
}

/// A budget line's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Met,
    Exceeded,
    /// One side did not produce the right answer, so the timing is not
    /// evidence of anything.
    Wrong,
}

impl Verdict {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Met => "met",
            Self::Exceeded => "exceeded",
            Self::Wrong => "wrong",
        }
    }
}

/// Everything one run measured. Written as `QUALIFICATION <name> <verdict>
/// <value>` lines on stderr, and as a JSON file when a path is given, so a
/// person reads the same numbers the test asserted on.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Receipt {
    pub origin: String,
    /// The tree this was measured on, for a reader who finds the receipt
    /// beside a build.
    pub commit: String,
    pub entries: usize,
    pub distinct_contents: usize,
    pub bytes: u64,
    pub excluded: Vec<String>,
    pub comparisons: Vec<ComparisonLine>,
    pub notes: Vec<String>,
}

impl Receipt {
    #[must_use]
    pub fn new(origin: &str, commit: &str) -> Self {
        Self {
            origin: origin.to_owned(),
            commit: commit.to_owned(),
            ..Self::default()
        }
    }

    pub fn push(&mut self, line: ComparisonLine) {
        self.comparisons.push(line);
    }

    pub fn note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }

    /// Announce a line on stderr as it is measured, so a failing run shows
    /// every number up to the one that failed.
    pub fn announce(&self, line: &ComparisonLine) {
        let ratio = line
            .comparison
            .ratio()
            .map_or_else(|| "-".to_owned(), |ratio| format!("{ratio:.3}x"));
        eprintln!(
            "QUALIFICATION {} {} reference={} ms candidate={} ms ratio={} budget={} {}",
            line.name,
            line.verdict().as_str(),
            line.comparison.reference_millis,
            line.comparison.candidate_millis,
            ratio,
            line.budget.describe(),
            line.detail,
        );
    }

    /// Assert every line, reporting all failures at once rather than the
    /// first: a qualification run is worth less if it hides nine problems
    /// behind one.
    ///
    /// # Panics
    ///
    /// Panics with every line that is not `met`, naming what it measured.
    pub fn assert_all_met(&self) {
        let failures: Vec<String> = self
            .comparisons
            .iter()
            .filter(|line| line.verdict() != Verdict::Met)
            .map(|line| {
                format!(
                    "{}: {} (reference {} ms, candidate {} ms, {}), {}",
                    line.name,
                    line.verdict().as_str(),
                    line.comparison.reference_millis,
                    line.comparison.candidate_millis,
                    line.budget.describe(),
                    line.detail,
                )
            })
            .collect();
        assert!(
            failures.is_empty(),
            "{} of {} qualification lines did not meet their budget:\n{}",
            failures.len(),
            self.comparisons.len(),
            failures.join("\n"),
        );
    }

    /// Write the receipt as JSON.
    ///
    /// # Errors
    ///
    /// Returns the write error, or a serialisation error as one.
    pub fn write(&self, path: &std::path::Path) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(self)?;
        std::fs::write(path, json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timed(millis: u64, correct: bool) -> Timed {
        Timed { millis, correct }
    }

    fn line(budget: Budget, reference: Timed, candidate: Timed) -> ComparisonLine {
        ComparisonLine {
            name: "probe".to_owned(),
            comparison: Comparison::new(reference, candidate),
            budget,
            detail: String::new(),
        }
    }

    #[test]
    fn a_faster_wrong_answer_is_not_a_pass() {
        let line = line(Budget::times(2), timed(100, true), timed(10, false));
        assert_eq!(line.verdict(), Verdict::Wrong);
        let receipt = Receipt {
            comparisons: vec![line],
            ..Receipt::default()
        };
        let failed = std::panic::catch_unwind(|| receipt.assert_all_met());
        assert!(failed.is_err(), "a wrong answer must fail the run");
    }

    #[test]
    fn a_ratio_needs_a_measurable_reference() {
        let zero = line(Budget::times(2), timed(0, true), timed(5, true));
        assert_eq!(zero.comparison.ratio(), None);
        assert_eq!(zero.verdict(), Verdict::Met, "no ratio is no ceiling");
        let over = line(Budget::times(2), timed(100, true), timed(201, true));
        assert_eq!(over.verdict(), Verdict::Exceeded);
        let inside = line(Budget::times(2), timed(100, true), timed(200, true));
        assert_eq!(inside.verdict(), Verdict::Met);
    }

    #[test]
    fn moments_are_bounded_and_scaled() {
        let moment = Budget::AtMostMillis(1_500);
        assert_eq!(
            line(moment, timed(0, true), timed(1_500, true)).verdict(),
            Verdict::Met
        );
        assert_eq!(
            line(moment, timed(0, true), timed(1_501, true)).verdict(),
            Verdict::Exceeded
        );
    }
}
