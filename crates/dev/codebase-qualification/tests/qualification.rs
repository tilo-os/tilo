//! Qualifies this repository's own working set against its filesystem,
//! history and collaboration plane.
//!
//! The tree under test is the one this checkout is, read from the working
//! directory so that what a person has on disk is what is measured. Nothing is
//! written back: every measurement happens in a scratch directory, and Git is
//! driven in a scratch repository built from the same bytes, so both sides see
//! exactly the same content and every pair of numbers answers the same
//! question.
//!
//! This binary is only built for `--features benchmark`, so a run of it is a
//! request to qualify. It has no skip path and no subset switch: it imports
//! every entry of the tree in front of it, and fails if that is not a
//! codebase-sized tree, because a harness that quietly measures a hundred
//! files proves nothing about a hundred thousand.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "a qualification harness: a tree, a scratch repository or a store \
              that cannot be read is the failure the run reports"
)]

use std::path::{Path, PathBuf};

use codebase_qualification::budgets::{Budget, BudgetSet};
use codebase_qualification::codebase::SourceTree;
use codebase_qualification::comparisons::{Scratch, entries_per_top_level};
use codebase_qualification::measurements::{Comparison, ComparisonLine, Receipt, Timed, millis};
use codebase_qualification::workspace::{self, Imported};

const RECEIPT_ENV: &str = "TILO_CODEBASE_QUALIFICATION_RECEIPT";
/// How many paths the second checkpoint changes.
const CHANGES: usize = 100;

/// `<repo>` from `CARGO_MANIFEST_DIR` = `<repo>/crates/dev/codebase-qualification`.
fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("the crate is four levels below the repository root")
        .to_path_buf()
}

/// Everything one run of the harness holds while it measures.
struct Qualification {
    source: SourceTree,
    reference: Scratch,
    reference_tree: String,
    imported: Imported,
    runtime: tokio::runtime::Runtime,
    scratch: PathBuf,
}

fn compare(
    receipt: &mut Receipt,
    name: &str,
    budget: Budget,
    reference: Timed,
    candidate: Timed,
    detail: String,
) {
    let line = ComparisonLine {
        name: name.to_owned(),
        comparison: Comparison::new(reference, candidate),
        budget,
        detail,
    };
    receipt.announce(&line);
    receipt.push(line);
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a tokio runtime")
}

#[test]
fn codebase_sized_workspace_meets_its_budgets() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let root = scratch.path().to_path_buf();
    let source = SourceTree::from_directory(repository_root()).expect("read this tree");
    assert!(
        source.manifest().len() > 1_000,
        "this tree should have thousands of tracked paths, found {}",
        source.manifest().len()
    );

    // The scratch repository holds the same content, so every comparison is
    // like for like and neither side sees the live working directory.
    let mut reference = Scratch::create(root.join("git")).expect("a scratch repository");
    reference
        .import(&source)
        .expect("import the tree into the scratch repository");
    let reference_tree = reference.tree().expect("the import made a tree").to_owned();

    let runtime = runtime();
    let imported = runtime
        .block_on(workspace::import(&source, &root.join("workspace")))
        .expect("import the tree into a workspace");
    let imported_entries = imported.inodes.len();
    assert!(
        imported.complete(imported_entries),
        "{} of {imported_entries} entries were imported: {:?}",
        imported.inodes.len(),
        imported.failed,
    );

    let mut run = Qualification {
        source,
        reference,
        reference_tree,
        imported,
        runtime,
        scratch: root,
    };
    let mut receipt = run.receipt();

    run.reopen_against_git_status(&mut receipt);
    run.resolve_every_path(&mut receipt);
    run.diff_against_checkpoint(&mut receipt);
    Qualification::assert_codebase_sized(&receipt);

    if let Ok(path) = std::env::var(RECEIPT_ENV) {
        receipt.write(Path::new(&path)).expect("write the receipt");
        eprintln!("QUALIFICATION receipt written to {path}");
    }
    receipt.assert_all_met();
}

impl Qualification {
    /// The receipt up to the point where measurements have not been taken.
    fn receipt(&self) -> Receipt {
        let manifest = self.source.manifest();
        let mut receipt = Receipt::new(&manifest.origin, &manifest.origin);
        receipt.entries = self.imported.inodes.len();
        receipt.distinct_contents = self.imported.distinct_contents();
        let by_path = manifest.by_path();
        receipt.bytes = self
            .imported
            .inodes
            .keys()
            .filter_map(|path| by_path.get(path.as_str()).copied())
            .map(|entry| entry.bytes)
            .sum();
        receipt.excluded.clone_from(&manifest.excluded);
        // Which build produced these numbers: an unoptimised `fs` is several
        // times slower than the one a person runs, and a reader comparing two
        // receipts needs to know which they are holding.
        receipt.note(if cfg!(debug_assertions) {
            "measured with an unoptimised build"
        } else {
            "measured with an optimised build"
        });
        receipt.note(format!(
            "imported {} entries as {} distinct contents",
            receipt.entries, receipt.distinct_contents
        ));
        receipt.note(format!(
            "{} tracked blobs, {} paths",
            manifest.distinct_contents(),
            manifest.len()
        ));
        for (top, count) in entries_per_top_level(manifest).into_iter().take(8) {
            receipt.note(format!("{count} entries under {top}"));
        }
        receipt
    }

    /// Reopening the workspace from its durable files: what a person waits for
    /// at the start of a session, which is Git's status of a fresh checkout.
    fn reopen_against_git_status(&self, receipt: &mut Receipt) {
        let checkout = self.scratch.join("checkout");
        let status = self
            .reference
            .status_of_checkout(&checkout, &self.reference_tree)
            .expect("status of a checked-out tree");
        assert_eq!(
            status.1.total(),
            0,
            "the scratch checkout should be clean, it reported {:?}",
            status.1
        );
        let started = std::time::Instant::now();
        let saved = std::fs::read(&self.imported.namespace_path).expect("the saved namespace");
        let namespace = fs::Namespace::from_bytes("qualification-reopen", &saved)
            .expect("reload the namespace");
        let history = fs::FileHistoryStore::open(&self.imported.history_path).expect("history");
        let heads: usize = self
            .imported
            .records
            .values()
            .filter_map(|record_id| history.snapshot(*record_id))
            .map(|snapshot| snapshot.heads.len())
            .sum();
        let reopened = Timed {
            millis: millis(started.elapsed()),
            correct: namespace.indexed_record_count() >= self.imported.inodes.len()
                && heads >= self.imported.inodes.len(),
        };
        assert!(
            reopened.correct,
            "reopened {} records and {heads} heads for {} imported entries",
            namespace.indexed_record_count(),
            self.imported.inodes.len()
        );
        let detail = format!(
            "{} records, {heads} heads, {} bytes",
            namespace.indexed_record_count(),
            receipt.bytes
        );
        compare(
            receipt,
            "reopen",
            BudgetSet::reopen(),
            Timed {
                millis: status.0.millis,
                correct: true,
            },
            reopened,
            detail,
        );
    }

    /// Resolving every imported path from the root, which is what a build's
    /// first stat of each source file does. Inodes are a local allocation the
    /// reloaded namespace hands out as it is walked, so the walk is the only
    /// honest form of this measurement — and the one a person's build makes.
    fn resolve_every_path(&self, receipt: &mut Receipt) {
        let saved = std::fs::read(&self.imported.namespace_path).expect("the saved namespace");
        let mut namespace = fs::Namespace::from_bytes("qualification-resolve", &saved)
            .expect("reload the namespace");
        let started = std::time::Instant::now();
        let mut resolved = 0_usize;
        let mut unresolved: Option<&str> = None;
        for path in self.imported.inodes.keys() {
            let mut ino = fs::ROOT_INO;
            let mut found = true;
            for segment in path.split('/') {
                let Some(attributes) = namespace.lookup(ino, segment) else {
                    found = false;
                    break;
                };
                ino = attributes.ino.0;
            }
            if found {
                resolved = resolved.saturating_add(1);
            } else if unresolved.is_none() {
                unresolved = Some(path.as_str());
            }
        }
        let resolve = Timed {
            millis: millis(started.elapsed()),
            correct: resolved == self.imported.inodes.len() && !self.imported.inodes.is_empty(),
        };
        let checkout = self.scratch.join("checkout");
        let (git_resolve, git_paths) = self
            .reference
            .resolve_all(&checkout)
            .expect("resolve every path through git");
        assert_eq!(
            git_paths,
            self.imported.inodes.len(),
            "git and the workspace should hold the same number of paths"
        );
        receipt.note(
            "resolving a path here walks the namespace from the root, where git reads its index \
             once: this is the widest gap the run records, and git's time is in the line beside \
             it so the gap stays visible",
        );
        let detail = match unresolved {
            Some(path) => format!(
                "{resolved} of {} paths resolved, first missing {path}",
                self.imported.inodes.len()
            ),
            None => format!(
                "{resolved} of {} paths resolved",
                self.imported.inodes.len()
            ),
        };
        compare(
            receipt,
            "resolve-paths",
            BudgetSet::in_this_profile(BudgetSet::PATH_RESOLUTION),
            git_resolve,
            resolve,
            detail,
        );
    }

    /// What changed since the checkpoint: the question a person asks after a
    /// reboot, and a second person asks when they come back to a shared tree.
    /// A hundred paths change, which is a working day's worth, and both planes
    /// are asked which ones did — Git by diffing two commits, Tilo by reading
    /// the heads of every record it holds.
    fn diff_against_checkpoint(&mut self, receipt: &mut Receipt) {
        let edited: Vec<String> = self
            .imported
            .inodes
            .keys()
            .filter(|path| path.to_lowercase().ends_with(".rs"))
            .take(CHANGES)
            .cloned()
            .collect();
        assert_eq!(
            edited.len(),
            CHANGES,
            "this tree should have at least {CHANGES} Rust files to change"
        );
        let edit_set: Vec<(String, Vec<u8>)> = edited
            .iter()
            .map(|path| (path.clone(), format!("// {path}\n").into_bytes()))
            .collect();
        let advanced = self
            .runtime
            .block_on(workspace::advance(&self.imported, &edit_set))
            .expect("a second checkpoint");
        assert_eq!(
            advanced.changed, CHANGES,
            "every edited path should have moved in the namespace"
        );
        assert_eq!(
            advanced.committed, CHANGES,
            "every edited path should have gained a version"
        );

        // Git's answer to the same question: the tree of the edited paths on
        // top of the imported one, diffed against it.
        let edited_tree = self
            .reference
            .tree_with(&edit_set)
            .expect("a second tree in the scratch repository");
        let (git_diff, changed_paths) = self
            .reference
            .changed_paths(&self.reference_tree, &edited_tree)
            .expect("diff the two trees through git");
        assert_eq!(
            changed_paths.len(),
            CHANGES,
            "git should see the same number of changed paths"
        );
        // Tilo's answer, from the history alone: the heads that are not the
        // ones the first checkpoint left.
        let started = std::time::Instant::now();
        let history =
            fs::FileHistoryStore::open(&self.imported.history_path).expect("reopen history");
        let moved = self
            .imported
            .records
            .values()
            .filter(|record_id| {
                history
                    .snapshot(**record_id)
                    .is_some_and(|snapshot| snapshot.versions.len() > 1)
            })
            .count();
        let tilo_diff = Timed {
            millis: millis(started.elapsed()),
            correct: moved == CHANGES,
        };
        assert_eq!(
            moved, CHANGES,
            "the history should have moved {CHANGES} records"
        );
        receipt.note(
            "answering what changed reads every record's history, where git walks two trees: \
             this is the second real gap the run records, and the budget is an absolute \
             ceiling for the same reason as resolve-paths",
        );
        let detail = format!(
            "{moved} of {} records moved after {CHANGES} edits",
            self.imported.records.len()
        );
        compare(
            receipt,
            "diff-against-checkpoint",
            BudgetSet::in_this_profile(BudgetSet::DIFF_AGAINST_CHECKPOINT),
            git_diff,
            tilo_diff,
            detail,
        );
    }

    /// The size claim: numbers taken on a toy tree say nothing about a
    /// codebase, so a tree smaller than one is a failure to observe the subject
    /// rather than a smaller pass.
    fn assert_codebase_sized(receipt: &Receipt) {
        assert!(
            receipt.entries >= BudgetSet::MINIMUM_ENTRIES,
            "a codebase-sized qualification needs at least {} entries, this tree imported {}",
            BudgetSet::MINIMUM_ENTRIES,
            receipt.entries
        );
        assert!(
            receipt.distinct_contents >= BudgetSet::MINIMUM_DISTINCT_CONTENTS,
            "a codebase-sized qualification needs at least {} distinct contents, this tree imported {}",
            BudgetSet::MINIMUM_DISTINCT_CONTENTS,
            receipt.distinct_contents
        );
    }
}
