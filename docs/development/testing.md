# Test a change

Start with the behavior you changed, not the whole workspace. If your package
builds on your host, you can use ordinary Cargo without a build-service account.
For example, a change to the clock library can be checked with:

```sh
cargo test --locked -p tilo-clock
```

Choose your own package and test filter. For Linux services, boot tests or a
prepared remote environment, [t](../t.md) can check working-tree edits without
building locally. Hosted access currently requires a maintainer-issued token.

## With hosted t: choose the scope

From the checkout:

```sh
t scope
t suggest
```

`t scope` names affected default checks. `t suggest` also identifies relevant
opt-in checks; choose those that can observe the change. Neither command runs
checks. Name the selected checks explicitly with `t test`; the
[testing tools](tools/testing.md) page explains discovery and selection.

For example, while changing the shell's workspace overview:

```sh
t test test-x86_64/shell --test 'concept_parity_spec::overview_add_workspace::'
t test clippy/shell
t cargo check --locked -p shell --tests
```

The first command runs one module, not the package's full test coverage. Follow
focused feedback with the affected package/integration checks. The last command
checks test compilation but does not execute tests; on a non-Linux builder it
cannot check Linux-only compositor code.

For browser-built crates, use `check/<package>` or `clippy/<package>`: their
wrapper checks the library for `wasm32-unknown-unknown` as well as natively.
Native tests alone cannot compile wasm-only code. Select `wasm` when the
complete linked web runtime needs checking; see
[t's check model](../t.md#adding-a-check).

## Run only what needs rerunning

```sh
t log test-x86_64/shell
t test
```

The log gives the failure's output; bare `t test` reruns what failed for you
last time, not a default suite. Use `--test` with the failing test's name for a
narrower rerun. A pass after a retry does not explain a race.

Use `t test --rev HEAD <check>` when you need evidence for the committed
snapshot rather than the current working tree. Keep those results distinct. The
push gate checks the pushed commit and deliberately does not run the test
suites. Delivery and failure ownership are covered in [t's workflow](../t.md),
not by a second checklist here.

## Choose evidence that can see the behavior

- Rust logic and integration: affected package tests, plus compilation/lints
  appropriate to the target and features.
- Installer or app-packaging contracts: use the cheap, non-compiling
  [contract checks](tools/testing.md#check-installer-or-packaging-contracts).
- Compositor/UI: use the [headless guide](../headless-testing.md) for semantic
  driving and disposable captures. Check visual changes in multiple themes.
  Rendering goldens have their own `goldens/<package>` checks; discover the
  current selection with `t suggest`, rather than copying a filter expression.
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
