//! Qualifies Tilo's own filesystem, history and collaboration plane against
//! the working set this repository actually has, rather than against a
//! synthetic fixture.
//!
//! The question is not "can the record store hold a repository" but "what does
//! a person or an agent get when the thing under version control is this
//! codebase". Four parts, none of which writes to the live repository:
//!
//! - [`codebase`] reads the source tree from the working directory, without a
//!   `.git` at all, and turns it into a manifest of every path, its kind and
//!   its bytes, plus the paths an import deliberately leaves behind (build
//!   output and the object database, so neither side is flattered).
//! - [`workspace`] imports that manifest into one real workspace: the
//!   production `Namespace`, `ContentStore` and `FileHistoryStore`.
//! - [`comparisons`] is the reference side of every measurement: Git doing the
//!   same thing on the same content.
//! - [`budgets`] and [`measurements`] are the contract: what a checkpoint must
//!   meet, and the receipt that records whether it did.
//!
//! A run is the `qualification` test binary, which exists only behind
//! `--features benchmark`:
//!
//! ```sh
//! cargo test -p codebase-qualification --features benchmark --test qualification -- --nocapture
//! ```
//!
//! It has no skip path and no subset switch. Set
//! `TILO_CODEBASE_QUALIFICATION_RECEIPT` to a path to keep its JSON receipt.
//! `docs/designs/git-free-qualification.md` says what it measures and why;
//! `docs/benchmarks/codebase-qualification.md` carries the numbers of a run and
//! the gaps they record.
//!
//! Nothing here is a version-control authority of its own. It drives the same
//! types the mount drives, and compares the result against Git as a reference
//! implementation — which is what a qualification of this kind is.

#![cfg_attr(
    test,
    allow(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "in a test these are the assertion and fixture arithmetic, not a hazard"
    )
)]

pub mod budgets;
pub mod codebase;
pub mod comparisons;
pub mod measurements;
#[cfg(feature = "filesystem")]
pub mod workspace;

pub use budgets::{Budget, BudgetSet};
pub use codebase::{Kind, Manifest, ManifestEntry, SourceTree};
pub use comparisons::Scratch;
pub use measurements::{Comparison, ComparisonLine, Receipt, Timed, Verdict};
