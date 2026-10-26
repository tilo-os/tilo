# Diagnose a failed development check

Keep the first failure's check name, source snapshot and output. Fix the
reported problem before broadening the run or deleting artifacts.

## A t check failed or could not run

```sh
t log <check>
t test
```

Replace `<check>` with the reported check ID. Bare `t test` reruns your last
failures; name a check and `--test` to reproduce a single test or module. Use
`t log --help` for logs from another run or a test's output.

Exit `1` means a failure may be yours; `3` means all failures also occur on
main; `2` means t could not carry out the request. Do not report an execution
error as a passing check. [t](../t.md) explains baseline comparison and failure
ownership; [its setup section](../t.md#setting-up) covers access. If scope
discovery cannot find a base, inspect the reported Git/base error rather than
treating an empty selection as verification.

A generated-file failure may have a declared fix: `t fix <check>`, review the
diff, then rerun that check. [Dependencies](dependencies.md) and
[Git tools](tools/git.md) cover lockfiles and merge drivers. A lock-only
rejection can also mean the working tree and staged manifests differ; inspect
both before changing intent.

## The required runtime or capability is missing

Run the test in an environment that provides what it qualifies. Do not remove
`TILO_REQUIRE_*` settings, disable confinement or weaken an ownership check to
get past a missing capability.

- A Linux native-library failure: `tools/ci/check-build-env.sh` reports
  pkg-config modules, binaries, fonts and media elements in the environment
  where it runs. It is a broad diagnostic, not the minimum requirements for
  every package; use [setup](setup.md) for prerequisites.
- Installer materialization cannot create its private user namespace: use a
  qualified isolated Linux builder. The
  [contract runner's plan](tools/testing.md#check-installer-or-packaging-contracts)
  shows which selected step requires it.
- GPU/audio/media or kernel-dependent tests: inspect the
  [capability probe](test-isolation.md#declare-required-capabilities-honestly).
  Software rendering or private audio proves only that backend, not physical
  output. Root in a rootless container does not supply all host capabilities.
- PDF rendering: a text-extraction pass does not qualify rasterization. Inspect
  `TILO_PDFIUM_LIB` and the native library supplied by the test environment;
  missing PDFium must remain a failure for capture coverage.

Do not rerun a privileged storage suite on the host as a generic fix. Use a
named check with its declared environment; the [check definitions](../../t.toml)
describe requirements. For device problems, start with `tilo device doctor` and
`tilo device guide troubleshooting`.

## A test passes only outside the isolated environment

Look for a dependency on the real HOME, locale, installed application, bus,
display or keyring. Inject the input or start a private fixture instead of
passing your desktop sockets into the test. A manually started test binary must
use `tools/with-test-environment.sh`; see [test isolation](test-isolation.md).

For intermittent `ETXTBSY` on a fixture script, use `tilo_test_env::script` and
its shell-based `command()`/`exec_argv()`. For socket paths that are too long,
use `runtime_tempdir()`.

## Font assets are absent or rejected

Follow
[immutable font preparation](../../crates/sdk/ui-core/assets/fonts/ARTIFACTS.md).
Builds verify pinned bytes and notices; offline builds need a complete warm
cache or a matching prepared catalog. A remote build prepares assets remotely:
fetching only its executable does not also bring the catalog to the machine
where you run it. Preserve the catalog's expected path and notices. Do not
replace pinned fonts with whatever happens to be installed locally.

## Builds repeat work or storage is exhausted

Keep worktrees' target directories separate and leave `CARGO_TARGET_DIR` unset.
Check for changed toolchain, features, target or compiler flags before assuming
cache corruption. Use a Linux-filesystem checkout for Linux work in WSL rather
than sharing a Windows-drive target directory.

Use [build-speed measurements](../measurements/build-speed.md) for diagnosis,
not a cache purge as a first step. Check the actual filesystem/quota behind the
failing path and `TMPDIR`; there is no universal disk-size threshold or
filesystem repair command. Preserve active worktrees and their uncommitted
changes when reclaiming space.
