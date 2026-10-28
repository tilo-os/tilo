# Qualifying development without Git

Tilo's filesystem, history and collaboration plane are meant to replace not
just a working copy but the repository behind it: a person should be able to
check out a change, see what moved, undo it, hand a change to a second person
and recover after a reboot, with no Git in the loop. That claim is empty until
it is measured **on a tree the size of a codebase**, because every record
store is fast on a hundred files.

`crates/dev/codebase-qualification` is the harness that asks the question.
This page says what it measures, why each measurement is paired with Git, and
what a failure means. The numbers of a run live in
[`docs/benchmarks/codebase-qualification.md`](../benchmarks/codebase-qualification.md),
which goes stale with the tree; the budgets here are the contract.

## What is measured

| Line | The question a person asks | Tilo's answer | Git's answer |
| --- | --- | --- | --- |
| `reopen` | "I just sat down — is everything still here?" | reload the saved `Namespace` and the `FileHistoryStore`, count records and heads | `git status` on a fresh checkout |
| `resolve-paths` | "where is this source file?" (a build's first stat) | walk the namespace from the root, one `lookup` per segment, for every path | `git ls-files -s` |
| `diff-against-checkpoint` | "what changed since I was last here?" | read every record's history and count the records whose head moved | `git diff-tree` between two commits |

Each pair is *like for like*: both sides are given the same content, and both
must produce the right answer before either time counts. A faster wrong answer
is a failure, not a pass — `ComparisonLine::verdict` returns `Wrong` and the
budget is not consulted.

The tree under test is the working directory this crate lives in, with
`target/`, `.git/` and `node_modules/` excluded by name. Reading the working
directory rather than Git's object database is deliberate: the harness must run
in a sandbox that has no `.git`, and the tree a person has on disk is the more
faithful subject anyway.

## Why Git is the reference

Not because Git is the target implementation, but because it is the operation
the team already performs every day. A budget expressed as "no worse than what
we have" is a claim a reader can check without knowing anything about Tilo's
internals, and it refuses the answer "fast enough, we think".

## The two budget shapes

- **A moment** (`Budget::AtMostMillis`) bounds an operation whose cost is its
  own: how long a person waits to reopen a workspace, how long a build spends
  resolving paths. Git's time is recorded beside it for context but is not the
  ceiling.
- **A ratio** (`Budget::Within`) bounds an operation against Git on the same
  content, for the operations where "at least as good as Git" is the actual
  requirement. The factor is a whole-number fraction so a receipt carries no
  floating-point drift.

A budget was originally a ratio for every line. Two of them are moments now,
and both for the same measured reason: Git's side is 3–64 ms, which lands
anywhere between runs on a loaded machine, so a ratio against it measures the
noise in the reference rather than the work being qualified. The measured cost
of those two paths is recorded as the ceiling, so a regression fails and
closing the gap means lowering the number in the commit that earns it.

## What a run does not claim

The harness qualifies **one person, one machine, no Git**: import, reopen,
resolve, and answer what changed. It does not yet measure, and this page does
not claim:

- a change reaching a second collaborator (the `CROSS_PERSON_APPLY` shape, for
  which Git's equivalent is a push plus fetch);
- undo and redo over a codebase-sized history;
- working offline and reconciling afterwards;
- crash recovery — a checkpoint torn mid-write;
- backup and restore of a whole workspace;
- migrating an existing repository, and exporting back out of one;
- sustained growth: what an import, a reopen and a diff cost after a year of
  commits rather than one checkpoint.

Those are the next qualification, not a smaller pass. A line that cannot be
measured yet is absent from the receipt rather than present with a budget it
would always meet.

## Running it

```sh
cargo test -p codebase-qualification --features benchmark --test qualification -- --nocapture
```

The binary exists only behind `--features benchmark`, so a plain `cargo test`
never compiles it, and running it is an explicit request to qualify. There is
no environment switch that skips it and no way to measure a subset: it imports
every entry of the tree in front of it, and fails if that tree is not
codebase-sized (10 000 entries, 5 000 distinct contents) — a harness that
quietly measures a hundred files proves nothing about a hundred thousand.

Set `TILO_CODEBASE_QUALIFICATION_RECEIPT=/some/path.json` to keep the JSON
receipt that is otherwise only printed.

## What a failure means

`assert_all_met` reports **every** line that missed at once, naming the
measured quantity, both times and the budget. A receipt is a record of one
tree on one machine: the same tree on a quieter machine will report smaller
numbers, and the profile matters more than either (both are recorded in the
receipt's notes).

The size assertions are unconditional. A tree that shrinks below the minimum
fails the run, because that is a failure to observe the subject, not a
smaller pass.
