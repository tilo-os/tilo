# Automatic read-only product policy

`tools/ci_product_policy.py` derives the check/lint expectations itself. Agents do
not write per-PR policy maps or coordinate runner slots. The eventual platform
invoker supplies its existing reviewed base, reviewer allowlist and admitted app
source roots from stable operator configuration; these are not candidate inputs.
Product execution remains disabled and every receipt says `merge_authorized:false`.

```sh
python3 tools/ci_product_policy.py --pr NUMBER \
  --reviewed-base REVIEWED_CURRENT_MAIN_SHA --git-root /trusted/clean-base \
  --reviewer claude --routine-root apps/calculator/src \
  --out /outside-checkout/new-shadow-receipt.json
```

The audited `ci_integration_readiness.py` and `ci_product_report.py` modules are
included with this command. The Git checkout must be complete, clean and
at the reviewed current main, with candidate commit objects already fetched. It
never checks out or executes candidate files. It performs only GET API reads,
local Git reads, base Cargo metadata resolution and the committed base selector.
No compilation or new Cargo target directory is involved. Cargo metadata may
populate the operator's normal download cache; it uses `--locked --no-deps`.

The reviewed current check/lint/selector hashes pin the supported workflow
contract. The command derives all selected packages and both native architectures
from the unchanged base Cargo graph; expected job names never come from observed
jobs. Installed Forgejo implicit matrix names sort keys and truncate ASCII-derived
names to 255 UTF-8 bytes. Colliding names or any missing/extra/native skipped job
reject the audit. Zero-Rust keeps the explicit skipped sentinel and independently
recomputed selector proof. PR-only report skipping never excuses security, builder,
selector, aggregate or quality failures.

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

Eight tests execute the current workflow/selector contract with real Cargo metadata
and temporary Git history, covering zero/native matrices, missing native coverage,
long matrix names, exact reviews, latest-run selection, scope changes, deletions
and custom-build helper exclusion. No native code is compiled. A live PR61 shadow
attempt also stopped when main advanced beyond the reviewed base; no product PR
has been declared ready by this slice.

## Reviewed developer-feedback contract (2026-09-19)

The read-only generator pins the check, lint, nightly and selector bytes shipped
at main `0cb62f077`. Routine PR policy requires the single x86 affected job to
check every reverse dependant and run direct-owner plus existing contract tests.
Its exact matrix name includes `test_packages`; the generator rejects missing,
extra or mismatched compilation/test assignments. Manual dispatch and the release
branch retain full native selection; the pinned nightly workflow covers both
architectures. Nightly is a coverage backstop, not a fabricated per-PR status.

Changed workflow/selector bytes require another independent contract review.
Exact-head independent reviewer approval and current-base checks are unchanged.
This remains a GET-only readiness report and never grants merge authorization.
