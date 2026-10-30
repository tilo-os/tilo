# Test a change

Start with the behavior you changed, not the whole workspace.

## Test what you changed

From your worktree, with your edits committed or not:

```sh
tools/ci/test-changed.sh
```

It plans what your change affects against the merge-base with `integration`
(`origin/main` where there is no `integration` branch; `--base REF` for
another), with uncommitted and untracked files included, and runs Clippy over
the changed packages and their direct dependants and the tests of the packages
whose behavior can change, with the filters, features and test environment CI
uses. A dependant further away is linted by the batch check (below), or here
with `--closure`. Clippy and the tests run at
the same time on the builders the `cargo` shim forwards to (rcargo; tests and
lints go to the x86 builder): each job runs inside the builder's container, so
the binaries the tests need are built there and nothing is fetched to your
machine. Tests that failed in your last run against the same base run first,
alone, so a fix is confirmed within the time of its own tests. A big test set
is shared between the x86 and arm builders.

Narrow it while you iterate:

```sh
tools/ci/test-changed.sh -- overview_add_workspace::  # tests whose names match
tools/ci/test-changed.sh -E 'package(shell) and binary(it)'
tools/ci/test-changed.sh --stage test                  # no Clippy
tools/ci/test-changed.sh --closure                     # lint every reverse dependency
```

The terminal shows failures as they happen and a summary of each job's wall
time; each job's whole output is in `target/local-affected/<job>.log`. A
failing job's diagnostics are printed at the end.

For one test target, plain cargo is still the fastest loop:

```sh
cargo nextest run --profile ci -p tilo-clock
cargo test --locked -p shell --test it MODULE::
```

Without the builders, both commands run on your machine; `--local` makes
`test-changed.sh` do so explicitly.

## Check a batch of commits

`tools/ci/local-affected.sh --base REF` is the same run for committed work:
the selection is HEAD's against the merge-base with REF (`origin/main` by
default), the worktree must be clean, and every stage runs (Clippy, tests,
visual goldens, certification, example scenarios and scoped reader tests;
`--tapps` adds the size-budgeted app packages). It is what a batch is checked
with before it is promoted to main. `--stage` narrows it; `--fail-fast` stops
the remaining jobs at the first failure.

What both commands run is
[tools/ci/local_affected.py](../../tools/ci/local_affected.py)'s; the package
selection is the planner's, [tools/ci/affected_packages.py](../../tools/ci/affected_packages.py).

## Choose evidence that can see the behavior

- Rust logic and integration: affected package tests, plus compilation/lints
  appropriate to the target and features.
- Installer or app-packaging contracts: use the cheap, non-compiling
  [contract checks](tools/testing.md#check-installer-or-packaging-contracts).
- Compositor/UI: use the [headless guide](../headless-testing.md) for semantic
  driving and disposable captures. Check visual changes in multiple themes.
  Rendering goldens run in the `goldens` stage of the commands above, alone,
  after the package's other tests.
- Kernel, device, installation or session behavior: use a qualified environment.
  `tilo device guide scenarios` covers device scenarios. A headless run or a
  contract fixture is not evidence of hardware behavior.
- Performance: follow the relevant
  [measurement guide](../measurements/performance.md); ordinary test durations
  are not a before/after benchmark.

Not every check is needed for every edit. In your PR, say what you tested and
anything you couldn't run. A focused pass is not release qualification.

## Keep tests meaningful and safe

Exercise the production interface, including denial, cancellation, interruption
and reconnect where relevant. Do not weaken assertions, ignore a failure or add
arbitrary sleeps to get a green run. A check unable to observe its subject must
fail; capability skips are not proof that the behavior works.

Inject paths and stores, keep tests away from personal data and the live
desktop, and clean up every process and resource a fixture starts. See
[test isolation](test-isolation.md) when writing a test, and
[troubleshooting](troubleshooting.md) when a run cannot observe its subject.
