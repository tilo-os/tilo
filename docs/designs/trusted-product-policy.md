# Automatic read-only product policy

`tools/ci/product_policy.py` derives the check/lint expectations itself. Agents do
not write per-PR policy maps or coordinate runner slots. The eventual platform
invoker supplies its existing reviewed base, reviewer allowlist and admitted app
source roots from stable operator configuration; these are not candidate inputs.
Product execution remains disabled and every receipt says `merge_authorized:false`.

```sh
python3 tools/ci/product_policy.py --pr NUMBER \
  --reviewed-base REVIEWED_CURRENT_MAIN_SHA --git-root /trusted/clean-base \
  --reviewer claude --routine-root apps/calculator/src \
  --out /outside-checkout/new-shadow-receipt.json
```

The audited `integration_readiness.py` and `product_report.py` modules are
included with this command. The Git checkout must be complete, clean and
at the reviewed current main, with candidate commit objects already fetched. It
never checks out or executes candidate files. It performs only GET API reads,
local Git reads, base Cargo metadata resolution and the committed base selector.
No compilation or new Cargo target directory is involved. Cargo metadata may
populate the operator's normal download cache; it uses `--locked --no-deps`.

The reviewed check, lint, nightly and `tools/ci/affected_packages.py` hashes pin the feedback contract
shipped at main `c83e9b15c`. Routine PR policy requires one x86 affected job:
all selected reverse dependants receive all-target checks, while direct owners
and existing contract packages receive tests. The generator derives both scopes
from the reviewed base Cargo graph and rejects missing, extra or mismatched
compilation/test assignments. Expected job names include `test_packages` and
never come from observed jobs. Manual dispatch and the release branch retain
full native selection; the pinned nightly workflow covers both architectures.
Nightly is a coverage backstop, not a per-PR status requirement.

Installed Forgejo implicit matrix names sort keys and truncate ASCII-derived
names to 255 UTF-8 bytes. Colliding names or any missing, extra or unexpectedly
skipped job reject the audit. Zero-Rust keeps the explicit skipped sentinel and
independently recomputed selector proof. PR-only report skipping never excuses
security, builder, selector, aggregate or quality failures. Changed workflow or
selector bytes require another independent contract review.

Scope is deliberately bounded to explicitly admitted existing app `src` directories
and already audited `docs/designs/*.md`. Unknown paths, build metadata, workflow and
policy edits, deletions, renames, mode/type changes, and app packages with custom
build scripts reject routine derivation. All tracked non-routine inputs are
protected as a compact tree closure, including transitive Python/shell helpers,
Cargo/config inputs and custom build entrypoints. Admission of a source root is a
one-time operator review, including cross-package build-script imports; arbitrary
dynamic Rust build-script reads cannot be proven absent by a file-name classifier.
This shadow tool is not production merge authority.

Newest exact-head PR check/lint runs are selected regardless of success. Actual
jobs, current attempts, official independent exact-head reviews, base and harness
are then audited with existing tools. A final run-inventory/main read rejects
new-run creation or base drift. Failures produce a blocked receipt; a later change
can always invalidate a snapshot. There is no retry, rebase, merge, status posting,
permission change, service activation or CI-slot coordination.

Ten policy-generator tests exercise the reviewed workflow/selector contract with
real Cargo metadata and temporary Git history. They cover zero-Rust and feedback
matrices, reverse-dependant compilation versus direct-owner test scope, missing
coverage, mismatched test assignments, nightly contract drift, long matrix names,
exact reviews, latest-run selection, scope changes, deletions and custom-build
helper exclusion. No native code is compiled. Independent exact-head review and
current-base validation remain required; every result is a read-only observation,
never merge authorization.
