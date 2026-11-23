# CI tooling

This directory owns the implementation used only to plan, execute, measure, and
report continuous-integration work: the scripts CI runs,
compiler-cache setup, lint ratchets, builder resolution, and CI receipts.

Repository policy and release utilities remain in `tools/`; their behavior is
useful outside CI. Tests for both groups remain in `tools/tests/` so the single
repository-policy discovery command exercises the complete tooling surface.

Checks invoke these files through `tools/ci/...`. When adding a new CI helper,
place it here and add focused coverage under `tools/tests/` rather than growing
the flat `tools/` namespace again.

## Runner labels

Jobs ask for these labels in `runs-on`; `runner_labels.py` holds the same table
and the constants CI helpers put into job matrices. A fork keeps the names and
registers runners that carry them (Forgejo and GitHub self-hosted runners accept
any label list), or replaces them with the hosted equivalent.
`tools/tests/test_runner_labels.py` fails when a workflow uses a label that is
not listed here.

| Label | Architecture | Runs | Hosted equivalent |
|---|---|---|---|
| `runner` | x86_64 | General jobs and native Rust builds: 4-5 CPUs, 10 GiB memory, 50 GiB disk, Docker | ubuntu-24.04 |
| `runner-arm64` | aarch64 | `runner` on native ARM64 | ubuntu-24.04-arm |
| `runner-native-portable` | x86_64/aarch64 | Host-independent native SDK and WASM qualification: at least 22 GiB memory, Docker | self-hosted on either native architecture |
| `runner-quality` | x86_64 | Lint, change selection and result aggregation: 2 CPUs, 4 GiB memory | ubuntu-24.04 |
| `runner-large` | x86_64 | Whole-collection compiles: 12 CPUs, 32 GiB memory, 100 GiB disk | a 16-core larger runner |
| `runner-large-arm64` | aarch64 | `runner-large` on native ARM64 | a 16-core ARM64 larger runner |
| `runner-disk-100g` | x86_64 | `runner` with 100 GiB of disk for image and archive exports | ubuntu-24.04 with extra disk |
| `runner-disk-100g-arm64` | aarch64 | `runner-disk-100g` on native ARM64 | ubuntu-24.04-arm with extra disk |
| `runner-release` | x86_64 | OS image publication: large class, FUSE and loop devices, one slot per architecture | self-hosted |
| `runner-release-arm64` | aarch64 | `runner-release` on native ARM64 | self-hosted |
| `runner-native-storage` | x86_64 | Filesystem tests on a host kernel: Linux 6.14+ with pre-content fanotify, Btrfs, loop devices | self-hosted |
| `imager-linux-x86_64` | x86_64 | Imager release builds: /dev/kvm and a DRM render node | self-hosted |
| `imager-linux-aarch64` | aarch64 | `imager-linux-x86_64` on native ARM64 | self-hosted |
| `imager-windows-x86_64` | x86_64 | Imager release builds on Windows with signing tools | self-hosted |
| `imager-windows-aarch64` | aarch64 | `imager-windows-x86_64` on native ARM64 | self-hosted |
| `imager-macos-x86_64` | x86_64 | Imager and runtime builds on Intel macOS with Xcode | macos-15-intel |
| `imager-macos-aarch64` | aarch64 | Imager and runtime builds on Apple silicon with Xcode | macos-15 |
| `macos-aarch64` | aarch64 | Development builds and tests on a persistent Apple-silicon Mac; never release signing | macos-15 |

## Running CI's checks locally

Each check runs commands from this repository. Run them from the checkout on Linux, in the builder image
(`os/Containerfile.builder`) or a developer image with the same tools
(`dist/dev/Containerfile.fedora44`). Lint checks run in the smaller
`os/Containerfile.ci-fast`.

| Check                                                   | Local command                                                                                                                                                                                                                                                                                                 |
| ------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `security`                                              | `bash tools/run-security-boundary-invariants.sh`                                                                                                                                                                                                                                                              |
| `suite-<group>`                                         | `python3 tools/ci/suites.py run --group <group>` (`suites.py list` names the suites)                                                                                                                                                                                                                          |
| `test/<package>`                                        | `t test test/<package>`: nextest's `ci` profile over that package, in the test environment (`cargo nextest run --locked --profile ci --tests -p <package>` run locally is the same run)                                                         |
| `check/<package>`                                       | `bash tools/ci/check-all-targets.sh -p <package>...` (every target, the packages' test features enabled)                                                                                                                                                                                                      |
| `clippy/<package>`                                      | `cargo clippy --locked --all-targets -p <package>... -- -D warnings`                                                                                                                                                                                                                                          |
| `android`                                               | `bash tools/ci/install-android-ndk.sh`, then `tools/ci/clippy-android.sh`                                                                                                                                                                                                                                     |
| `vendored`                                              | `tools/ci/test-vendored-crates.sh [vendor/<crate>...]`: each patched vendored crate's own `cargo test`, from a copy of the workspace lock (`VENDORED_TEST_CRATES` in `package_sets.py`)                                                                                                                       |
| `workspace-service-features`                            | `for features in '' web apps agents apps,test-support agents,test-support; do cargo check --locked -p tilo-workspace-service --all-targets ${features:+--features "$features"} \|\| exit 1; done`, each feature set the crate is embedded with |
| `wasm`                                                  | `tools/ci/check-wasm.sh $(python3 tools/ci/package_sets.py browser)`                                                                                                                                                                                                                                          |
| `policy`, `fmt`, `deny`, `hakari`, `machete`, `changes` | `tools/policy/run.sh`, `cargo fmt --check`, `cargo deny --workspace check`, `cargo hakari generate --diff && cargo hakari manage-deps --dry-run`, `bash tools/ci/check-unused-dependencies.sh`, and the diff-based checks `changes` lists against `origin/main`                                                |
| `sdk-policy`                                            | `cargo build -p tilolang-cli --bin tilolang` then `python3 tools/apps/generate_catalogue_fleet.py --tilolang "$CARGO_TARGET_DIR/debug/tilolang" --check`; `python3 tools/sdk/publish_check.py --report-only`; the app contract is `tilo-app check --lint --all` in the `app-lints` check (on `idle` and `dispatch`)                                                                                                                                                        |
| `sdk-linux`                                             | `cargo build -p tilo-app-cli --bin tilo-app`, then `target/debug/tilo-app build --target linux apps/calculator` and `target/debug/tilo-app package --target linux apps/calculator --output calculator.tapp`                                                                                                   |
| `runtime-macos`                                         | `cargo check -p tilo-runtime --all-targets --locked`, on macOS                                                                                                                                                                                                                                                |
| `apple-client-macos`                                    | `dist/apple/build.sh test`, on macOS with Xcode (the client's tests import XCTest, which the command line tools alone do not carry); dispatch-only until the development Mac has Xcode                                                                                                                                                                             |
| `sdk-macos`                                             | the `sdk-linux` check's commands with `--target macos`, on macOS                                                                                                                                                                                                                                              |
| `apps-macos`                                            | `cargo build --locked $(python3 tools/ci/macos_apps.py)`, then `cargo nextest run` and the doctests with the same packages, on macOS                                                                                                                                                                          |
| `real-fuse`                                             | `bash scripts/ci-fuse-preflight.sh`, then `cargo test --locked -p tilo-fs --test it integration_test:: -- --test-threads=1` and the check's other named tests                                                                                                                                                      |
| `native-tier1`                                          | `tools/native-tier1.sh` on a host with pre-content fanotify, Btrfs and loop devices                                                                                                                                                                                                                           |
| `render-counters`                                       | `cargo build --release -p ui-examples --example ui-perf`, then `python3 scripts/perf/render_bench.py counters target/release/examples/ui-perf` (pixel parity, then allocation and raster-pixel budgets at 4K; see [docs/ui-performance-benchmark.md](../../docs/ui-performance-benchmark.md#where-this-runs)) |
| `perf-<group>`, `perf-scheduled` (nightly)              | `bash tools/ci/perf-scheduled.sh <group>`; `python3 tools/perf/compare.py --results <records.jsonl> --store https://code.tilo.so --groups <group>` compares records with the published store without writing to it; see [Performance results](../../docs/measurements/performance.md)                         |

`scripts/ci-run.sh` and `tools/ci/measure.sh`, which wrap some check commands,
only label and time a command; leave them out locally.

## Main and `stable`

`main` is an unstable integration branch: many agents push to it directly, no
pull request checks it, and a commit that fails is fixed by a later one rather
than reverted. `stable` is the release branch: it is moved by hand when a
release is cut (it requires signed commits), and it is the only ref
publications build from.

- `t` runs the checks for every head of `main`: its default checks (the test,
  clippy, check, suite, policy and `workspace-service-features` families) run
  on each new head in `t.toml`, keyed by the inputs they read, and
  `t main` names every failing item with the commit that broke it. Checks that
  cost a lot name `on = ["dispatch", "idle"]` instead: main's newest head runs
  them when a node is idle.
- The clippy families cover what a push leaves to them: every package for the
  host with the features the workspace's whole build gives it (`clippy`,
  `clippy-{arch}`, `clippy-idle`), the packages CI also checks as they ship
  (`clippy-shipped`), and the page's crates for `wasm32-unknown-unknown` under
  the page's own Clippy configuration (`clippy-page`), so a break only the web
  build sees is named on the head that introduced it. A push's own hook lints
  what it changes.
- `changes` reads the commits a push added, and the whole-tree checks (`fmt`,
  `hakari`, `lockfile`, `deny`, `machete`, `policy/{step}`) run
  on the same head, so no push's run stands in for another's.
- Release artifacts are built and installed by t on main's newest head when it
  has room (`package/*`, `install/*`); only a version tag signs and publishes
  them (`sign/*`, `publish-*`). The whole-tree coverage
  backstop is t's nightly set (`nightly-*`, `nightly-fuzz-*`, `certify`,
  `goldens`, `wasm`), scheduled at 03:00, and the environment publish chains
  run on their own crons; the artifact builds carry their own nightly schedule.

The test packages `tools/ci/nested-sandboxes.toml` lists start sandboxes of their own
(bubblewrap): their tests need a runner that lets an unprivileged process create user
namespaces. `python3 tools/ci/nested_sandboxes.py` prints them, one per line; a test shard
warns when its runner refuses user namespaces.
