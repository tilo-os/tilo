# Choose a testing tool

Use this page to pick an entry point, not to reproduce the build service's
operator manual. [Test a change](../testing.md) covers the working loop.

## Select and inspect remote checks

The canonical workflow and check model are in [t](../../t.md). Its per-command
help is the current CLI reference:

```sh
t test --help
t scope --help
t suggest --help
t log --help
```

| Need                                                               | Entry point                                       |
| ------------------------------------------------------------------ | ------------------------------------------------- |
| Find affected default checks without running them                  | `t scope`                                         |
| Find relevant opt-in checks too                                    | `t suggest`                                       |
| Discover every available check                                     | `t suggest --all`                                 |
| Run one native package's tests                                     | `t test test/tilo-fs`                             |
| Run shell tests (x86_64 only)                                      | `t test test-x86_64/shell`                        |
| Check/lint a package, including its browser target when applicable | `t test check/tilo-ui` or `t test clippy/tilo-ui` |
| Inspect a failing check                                            | `t log <check>`                                   |
| Rerun your last failures                                           | `t test`                                          |
| Apply a declared generator/formatter fix                           | `t fix <check>`, then review the diff             |

Selectors can be check IDs, package names, families or quoted globs. A package
selector runs its checks, not just tests; a family can be much broader. `--test`
selects a full test name or module path within a test check. It is not Cargo's
`--test <integration-target>` option. Copy the name from a failure or the
source; an empty match fails rather than supplying coverage.

Definitions live in [t.toml](../../../t.toml). Test families use
[the nextest wrapper](../../../tools/ci/t-nextest.sh), which supplies package
features, fixtures and filters. Check/clippy families use
[the compile wrapper](../../../tools/ci/t_cargo_compile.py). Prefer these checks
over rebuilding their commands by hand.

## Check installer or packaging contracts

On Linux (including WSL), with Python 3.11+, the focused contract runner does
not compile Rust, assemble an OS or start a VM:

```sh
python3 tools/dev.py check --help
python3 tools/dev.py check installer --plan
python3 tools/dev.py check app-packaging --plan
python3 tools/dev.py check installer --step target-modes
python3 tools/dev.py check app-packaging
```

Read the plan before execution: it lists commands and required tools. `--step`
is repeatable; use it to rerun the failed contract, not the suite. Receipts
under `target/dev-runs/<id>/run.json` identify omitted and unrun steps. They are
contract evidence, not VM, hardware or release attestations. The
[runner source](../../../tools/dev.py) defines the suites and receipt format;
its help covers output selection and failure handling.

The installer materialization contract needs a private user namespace for a
non-root caller. If unavailable, use a qualified isolated Linux builder; do not
bypass the ownership check or change host security settings.

## When a local test runner is actually needed

[scripts/test-local.sh](../../../scripts/test-local.sh) builds selected packages
using `CARGO` (default `cargo`), then runs nextest from a cleared environment.
`scripts/test-local.sh --help` describes package arguments, the `--` separator
and `--goldens` without compiling or running tests. Set `CARGO` to your build
executor if you want compilation elsewhere.

[tools/test-env.sh](../../../tools/test-env.sh) is an execution wrapper, not a
dependency probe: it requires software Vulkan, can generate native fixtures and
starts private audio.
[tools/with-test-environment.sh](../../../tools/with-test-environment.sh)
provides just the isolated home/session environment. Use the
[isolation guide](../test-isolation.md) for direct binary runs. Neither wrapper
promises that your machine matches a remote check's complete runtime, feature
set or capabilities.
