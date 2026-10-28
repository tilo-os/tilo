# Qualifying development without Git on this codebase

A dated measurement of
[`crates/dev/codebase-qualification`](../../crates/dev/codebase-qualification)
on this repository's own working set. It records what one run observed and why
the two gaps it shows are real. It is not a KPI: the numbers describe this tree
on this machine and go stale with the next merge. The design and the budgets
are in [`docs/designs/git-free-qualification.md`](../designs/git-free-qualification.md).

## The subject

| | |
| --- | --- |
| Tree | this checkout, minus `target/`, `.git/`, `node_modules/` |
| Tracked paths | 17 300 |
| Distinct contents | 17 300 |
| Imported records (files and directories) | 20 458 |
| Bytes in the content store after import | 294 MB |

The harness reads the working directory rather than Git's object database,
because the qualification runs where there is no `.git`, and the tree a person
has on disk is the subject either way. The reference side runs Git in a scratch
repository built from the same bytes, so both sides see identical content and
neither touches the live tree.

## What one run reported

```
QUALIFICATION reopen met reference=244 ms candidate=2260 ms ratio=9.262x budget=<= 8000 ms 20458 records, 20458 heads, 294003318 bytes
QUALIFICATION resolve-paths met reference=13 ms candidate=2199 ms ratio=169.154x budget=<= 10000 ms 17300 of 17300 paths resolved
QUALIFICATION diff-against-checkpoint met reference=4 ms candidate=854 ms ratio=213.500x budget=<= 10000 ms 100 of 20458 records moved after 100 edits
```

The ceilings above are the ones this unoptimised build is held to. In a
release-profile run the same lines are held to 8 000 ms, 2 500 ms and
2 500 ms; see "Accuracy of the numbers themselves".

Measured with an **unoptimised build** against the ceiling a release run would
use; see "Accuracy" below. Repeat runs on the same tree reported 2.2–2.9 s for
`reopen`, 1.9–2.4 s for `resolve-paths` and 0.85–1.0 s for
`diff-against-checkpoint`; Git's side moved between 4 ms and 63 ms for
`resolve-paths` and 3 ms and 13 ms for the diff.

## The two real gaps

Both are the cost of not having an index, and both are the workspace's to
close, not the harness's to hide.

**Resolving a path (`resolve-paths`, 169x).** Tilo walks the namespace from the
root, one `lookup` per path segment, for every path: 17 300 paths is a few tens
of thousands of CRDT map reads. Git reads its index once, sequentially, and
answers in 13 ms. A checkout-and-build path that resolves thousands of paths a
second is most of what makes Git feel instant, so this gap is the single most
valuable one to close.

**Answering what changed (`diff-against-checkpoint`, 213x).** Tilo reads every
record's history and counts the ones whose head moved, because there is no
index of which records moved; 100 edits cost 20 458 snapshot reads. Git walks
two trees and diffs them in 4 ms. An index that answers without the walk is
what closing this gap looks like.

Both budgets are therefore absolute ceilings rather than ratios against Git.
Git's number on this content moves by an order of magnitude between runs on a
loaded machine, so a ratio against it would measure the reference's noise; the
receipt still records Git's time beside every line so the gap stays visible and
goes on being the point.

## Accuracy

Every line is checked before it is timed: all 17 300 paths resolve, all 20 458
records reopen with a head, and both planes name exactly the same 100 changed
paths. A faster wrong answer is reported as `wrong` and the budget is never
consulted.

## Accuracy of the numbers themselves

The harness runs as a test binary, so it measures an unoptimised build, where
namespace parsing and record decoding are several times slower than in the
binary a person runs. `BudgetSet::in_this_profile` widens the ceilings by the
observed distance between the two builds (a factor of four) rather than folding
that into the stated budget, so the number a person waits for stays readable in
the source and a release-profile run can be recorded beside this one unchanged.

A receipt names the profile it was measured in, for exactly this reason: the
same tree read from two receipts is only comparable when the profiles match.

## What this run does not cover

One person, one machine, no Git: import, reopen, resolve, and answer what
changed. It does not measure a change reaching a second collaborator, undo over
a codebase-sized history, offline work and reconciliation, crash recovery,
backup and restore, migration and export, or what all of this costs after a
year of history rather than one checkpoint. See the design page.
