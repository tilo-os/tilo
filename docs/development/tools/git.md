# Work with Git hooks and generated files

For branch integration, the push gate and release ownership, use
[the t workflow](../../t.md#how-changes-reach-a-release). This page covers
checkout-local tooling.

## Install hooks without replacing someone else's

From inside the checkout:

```sh
python3 tools/dev/install-git-hooks.py
```

The [installer](../../../tools/dev/install-git-hooks.py) verifies executable
hooks and configures the generated-file merge drivers. It normally writes
clone-local configuration shared by linked worktrees. If worktree-specific
configuration is enabled, run it in each linked worktree.

It refuses unrelated `core.hooksPath` or custom default hooks rather than
overwriting them. Arrange explicit chaining if needed.
`python3 tools/dev/install-git-hooks.py --help` shows usage without changing Git
configuration.

## Check what you intend to commit

Stage explicit paths, preserve concurrent edits, and inspect the index:

```sh
git diff --cached --stat
git diff --cached
.githooks/pre-commit
```

The [pre-commit hook](../../../.githooks/pre-commit) performs cheap source,
workspace and staged-change checks, not compilation or tests. Some inventory
checks inspect the working tree as well. It checks text/LF policy, added-line
secrets/conflict markers/`dbg!`, lint allowances, ignored-test reasons and
test-module declarations. A failure should be fixed at its reported source, not
bypassed with `--no-verify`.

Do not stage generated build output or newly added binary assets. Pinned
non-text exceptions admit existing bytes unchanged, not new or modified
binaries. A credential-shaped fixture needs `example` or `placeholder` on the
same line; a real exposed credential needs rotation, not an exemption.

Vendored source is kept as upstream shipped it. Changed-source formatting
normally skips vendor code and sources marked `@generated` in their first five
lines; tilo-app-generated registries are deliberately formatted. These are
generator boundaries, not ways to exempt handwritten code.

## Use an accepted commit subject

Use `type(optional-scope)!: description`, for example
`fix(shell): preserve window identity` or `docs: explain test isolation`. The
`!` is optional. Separate a body from the subject with a blank line.

Accepted types are `build`, `chore`, `ci`, `docs`, `dev`, `env`, `feat`, `fix`,
`perf`, `refactor`, `revert`, `style`, and `test`. The
[message checker](../../../tools/policy/check_commit_message.py) also recognizes
genuine Git merge, target-bound revert and existing-target autosquash messages;
writing a fake merge subject does not bypass validation. Messages and branch
names must make sense to public readers without private tracker or discussion
references.

## Resolve generated-file conflicts

[.gitattributes](../../../.gitattributes) identifies whole generated files and
files with generated blocks. During a rebase, the installed merge driver keeps
the base branch's conflicting generated text so the generator can put back what
your commit needs. Conflicts outside generated blocks still need manual
resolution. Do not assume the same side names mean the same thing during an
ordinary merge.

After resolving source changes, use the relevant `t fix <check>`. Common owners
are `lockfile`, `hakari`, `t-toml`, `codemap` and `sdk-policy`. Review and stage
the result: a fix changes files, not commits.
[Dependency changes](../dependencies.md) explains relocking, lock-only update
declarations and feature unification. Never delete `Cargo.lock` to resolve a
conflict.

## Give unrelated tasks separate worktrees

```sh
python3 tools/dev.py worktree music-fix --base origin/main
```

This creates branch `codex/music-fix` from the specified committed revision and
prints its path and exact commit. It leaves uncommitted edits in their original
checkout and refuses an existing destination or branch. The destination is under
`.worktrees/` beside the common Git directory, not relative to a temporary
linked checkout. Use `python3 tools/dev.py worktree --help` for the current
interface.

Each worktree owns its normal `target/`; never set `CARGO_TARGET_DIR` or share
target directories. Do not remove a worktree with uncommitted work or use a
shared stash to move someone else's edits out of the way.
