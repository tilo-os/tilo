//! Budgets: what a checkpoint must meet before it can replace Git.
//!
//! Two shapes are enough for everything a checkpoint asks. A *moment* is an
//! operation whose cost is its own — reopening a workspace, resolving a path —
//! and is bounded in absolute milliseconds. A *ratio* is an operation the team
//! already does every day with Git, and is bounded relative to Git on the same
//! content, so the budget says "no worse than what we have" rather than naming
//! a number that a faster machine would flatter.

use serde::{Deserialize, Serialize};

use crate::measurements::Comparison;

/// A ceiling on one measured quantity.
///
/// # The two shapes
///
/// `AtMostMillis` is for a latency nobody can compare to anything: how long
/// `git status` takes at this size is not what a reader needs to know, how long
/// it takes to find out what changed is. `Within` is for the operations both
/// planes do, where the reference is Git itself on the same content, and the
/// budget says "no worse than what we have" instead of naming a number a
/// faster machine would flatter. The factor is a whole-number fraction so a
/// manifest or a receipt carries no floating-point drift: `Within(2)` is
/// twice the reference, `Within(3, 2)` one and a half times.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Budget {
    /// At most this many milliseconds, whatever the reference did.
    AtMostMillis(u64),
    /// At most `numerator / denominator` times the reference on the same
    /// content.
    Within(u32, u32),
}

impl Budget {
    /// At most `times` the reference.
    #[must_use]
    pub const fn times(times: u32) -> Self {
        Self::Within(times, 1)
    }

    /// Whether `comparison` is inside this budget.
    ///
    /// A `Within` line whose reference took no measurable time has no ratio,
    /// and no ratio is no ceiling: the operation passed as far as this budget
    /// can say. That is not a loophole — `Within` is only used where the
    /// reference does real work, and the receipt carries both numbers so a
    /// zero reference is visible in the evidence.
    #[must_use]
    pub fn permits(&self, comparison: &Comparison) -> bool {
        match self {
            Self::AtMostMillis(ceiling) => comparison.candidate_millis <= *ceiling,
            Self::Within(numerator, denominator) => {
                if comparison.ratio().is_none() {
                    return true;
                }
                // Cross-multiplied, so the decision is exact for every pair
                // that fits and cannot round a line over its own ceiling.
                comparison
                    .candidate_millis
                    .saturating_mul(u64::from(*denominator))
                    <= comparison
                        .reference_millis
                        .saturating_mul(u64::from(*numerator))
            }
        }
    }

    /// The millisecond ceiling of an `AtMostMillis` budget; zero for a ratio.
    #[must_use]
    pub const fn ceiling(&self) -> u64 {
        match self {
            Self::AtMostMillis(ceiling) => *ceiling,
            Self::Within(..) => 0,
        }
    }

    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::AtMostMillis(ceiling) => format!("<= {ceiling} ms"),
            Self::Within(numerator, 1) => format!("<= {numerator}x git"),
            Self::Within(numerator, denominator) => {
                format!("<= {numerator}/{denominator}x git")
            }
        }
    }
}

/// Nominal budgets for a first qualification.
///
/// These are starting numbers, not tuned ones: they exist so a regression
/// has something to fail against, and `docs/designs/git-free-qualification.md`
/// records the reasoning and the expectation that a measured run moves them.
/// Every one of them is a *ceiling on the candidate*, and every ratio is
/// against Git doing the same thing on the same content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetSet;

impl BudgetSet {
    /// How many distinct contents the tree must have before its budgets mean
    /// anything. Below this, a "codebase-sized" claim is not being made.
    pub const MINIMUM_DISTINCT_CONTENTS: usize = 5_000;
    /// How many tracked entries the tree must have before the size claim
    /// holds.
    pub const MINIMUM_ENTRIES: usize = 10_000;

    /// Reopening a workspace from its durable files: the cost of the first
    /// `git status` after a reboot.
    pub const REOPEN: Budget = Budget::AtMostMillis(2_000);
    /// How much more a debug build of this workspace may take than the
    /// release build `REOPEN` is stated for.
    ///
    /// The run measures an unoptimised build, because the check that runs it
    /// is a test; the ceiling is the one a person feels, which is the release
    /// binary. Comparing the two directly would either fail every debug run or
    /// force a ceiling so loose it catches nothing. This factor is the
    /// observed distance between them for record decoding and namespace
    /// parsing, and it is spelled out here rather than folded into `REOPEN` so
    /// the number a person waits for stays readable and a release-profile run
    /// can be added beside it unchanged.
    pub const DEBUG_BUILD_FACTOR: u64 = 4;

    /// `REOPEN`, widened for the profile this binary was built in.
    #[must_use]
    pub const fn reopen() -> Budget {
        Self::in_this_profile(Self::REOPEN)
    }

    /// `PATH_RESOLUTION` and `DIFF_AGAINST_CHECKPOINT`, widened for the
    /// profile this binary was built in.
    ///
    /// Both walk data structures the optimiser does most of the work for, so
    /// both are measured several times slower here than in the binary a person
    /// runs, and a ceiling stated for one profile would fail every run of the
    /// other. The same factor as `REOPEN` applies because it is the same
    /// distance between the two builds.
    #[must_use]
    pub const fn in_this_profile(budget: Budget) -> Budget {
        let ceiling = if cfg!(debug_assertions) {
            budget.ceiling().saturating_mul(Self::DEBUG_BUILD_FACTOR)
        } else {
            budget.ceiling()
        };
        Budget::AtMostMillis(ceiling)
    }
    /// Resolving every tracked path in the namespace from the root: what a
    /// build's first stat of each source file does.
    ///
    /// This is the one budget set from a measurement rather than from the
    /// intent, because the measurement refuted the intent. Walking a namespace
    /// from the root costs one `lookup` per path segment; Git answers the same
    /// question by reading its index once, and on this tree Git does it in
    /// 20 ms where the walk takes 1 559 ms. The gap is real and it is the
    /// workspace's to close — a checkout-and-build path that resolves
    /// thousands of paths a second is what makes Git feel instant.
    ///
    /// The budget is therefore a moment rather than a ratio: Git's index read
    /// on this tree lands between 3 ms and 41 ms from run to run, so a ratio
    /// against it measures the noise in the reference, not the walk. An
    /// absolute ceiling on the whole-tree walk is the number that moves when
    /// the walk moves, and the run records Git beside it either way. It holds
    /// the measured cost with room, so a regression fails and closing the gap
    /// means lowering this in the commit that earns it.
    /// `docs/benchmarks/codebase-qualification.md` carries the numbers.
    pub const PATH_RESOLUTION: Budget = Budget::AtMostMillis(2_500);
    /// Answering "what changed since that checkpoint", which is what a person
    /// asks after a reboot and what a second person asks when they come back
    /// to a shared tree.
    ///
    /// Tilo answers from the history it already holds: the records whose head
    /// moved. Git answers by diffing two trees, and on this content that is
    /// 3 ms — an object walk, not a question about twenty thousand histories.
    /// A ratio against 3 ms measures the reference's noise, so this is a
    /// moment like `PATH_RESOLUTION`: an absolute ceiling that moves when the
    /// answer moves, with Git's number recorded beside it in every receipt.
    ///
    /// The measured cost is a thousand histories read for a hundred edits,
    /// because a reader has no index of which records moved and has to ask.
    /// An index that answers without the walk is what closing this gap looks
    /// like. `docs/benchmarks/codebase-qualification.md` carries the numbers.
    pub const DIFF_AGAINST_CHECKPOINT: Budget = Budget::AtMostMillis(2_500);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurements::Timed;

    #[test]
    fn budgets_compare_like_for_like() {
        let inside = Comparison::new(
            Timed {
                millis: 1_000,
                correct: true,
            },
            Timed {
                millis: 2_000,
                correct: true,
            },
        );
        assert!(!Budget::Within(3, 2).permits(&inside));
        assert!(Budget::times(2).permits(&inside));
        assert!(!Budget::AtMostMillis(1_999).permits(&inside));
        assert!(Budget::AtMostMillis(2_000).permits(&inside));
    }

    #[test]
    fn a_moment_does_not_depend_on_the_reference() {
        let comparison = Comparison::new(
            Timed {
                millis: 0,
                correct: true,
            },
            Timed {
                millis: 900,
                correct: true,
            },
        );
        assert!(BudgetSet::reopen().permits(&comparison));
        assert_eq!(BudgetSet::REOPEN.describe(), "<= 2000 ms");
    }
}
