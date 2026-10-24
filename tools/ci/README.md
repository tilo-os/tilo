# CI tooling

This directory owns the implementation used only to plan, execute, measure,
and report continuous-integration work: the scripts t's checks run (t.toml),
compiler-cache setup, lint ratchets, builder resolution, and CI receipts.

Repository policy and release utilities remain in `tools/`; their behavior is
useful outside CI. Tests for both groups remain in `tools/tests/` so the single
repository-policy discovery command exercises the complete tooling surface.

Checks invoke these files through `tools/ci/...`. When adding a new
CI helper, place it here and add focused coverage under `tools/tests/` rather
than growing the flat `tools/` namespace again.

## Runner labels

Jobs ask for these labels in `runs-on`; `runner_labels.py` holds the same
table and the constants CI helpers put into job matrices. A fork keeps the
names and registers runners that carry them (Forgejo and GitHub self-hosted
runners accept any label list), or replaces them with the hosted equivalent.
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

## Running t's checks locally

Each check runs commands from this repository; t.toml only chooses the node and
image. Run them from the checkout on Linux, in the builder image
(`os/Containerfile.builder`) or a developer image with the same tools
(`dist/dev/Containerfile.fedora44`). Lint checks run in the smaller
`os/Containerfile.ci-fast`.

| t check | Local command |
|---|---|
| `security` | `bash tools/run-security-boundary-invariants.sh` |
| `suite-<group>` | `python3 tools/ci/suites.py run --group <group>` (`suites.py list` names the suites) |
| `test/<package>` | `scripts/test-local.sh -p <package>...` (gateway selections enable `local-models,host-backend`, including server tests) (see [Running tests like CI](../../DEVELOPMENT.md#running-tests-like-ci)) |
| `check/<package>` | `cargo check --locked --all-targets -p <package>...` |
| `clippy/<package>` | `cargo clippy --locked --all-targets -p <package>... -- -D warnings` |
| `android` | `bash tools/ci/install-android-ndk.sh`, then `tools/ci/clippy-android.sh` |
| `vendored` | `tools/ci/test-vendored-crates.sh [vendor/<crate>...]`: each patched vendored crate's own `cargo test`, from a copy of the workspace lock (`VENDORED_TEST_CRATES` in `package_sets.py`) |
| `wasm`, `web-examples` | `tools/ci/check-wasm.sh $(python3 tools/ci/package_sets.py browser)`; `tools/ci/build-web-examples.sh example-<name>...` builds the web examples |
| `policy`, `fmt`, `deny`, `hakari`, `machete`, `changes` | `tools/policy/run.sh`, `cargo fmt --check`, `cargo deny --workspace check`, `cargo hakari generate --diff && cargo hakari manage-deps --dry-run`, `bash tools/ci/check-unused-dependencies.sh`, and the diff-based gates `changes` lists against `origin/main` |
| `sdk-policy` | `python3 tools/policy/check_app_portability.py`, `python3 tools/policy/check_app_capabilities.py`, `python3 tools/web/generate_web_desktop.py --check` |
| `sdk-linux` | `cargo build -p tilo-app-cli --bin tilo-app`, then `target/debug/tilo-app build --target linux apps/calculator` and `target/debug/tilo-app package --target linux apps/calculator --output calculator.tapp` |
| `sdk-web` | `target/debug/tilo-app build --target web --bundle desktop apps/calculator`; `node --test crates/platform/*/web/*.test.mjs` |
| `sdk-web-size` | `target/debug/tilo-app build --target web --bundle desktop --release apps/calculator`, then `python3 tools/web/wasm_size_ratchet.py measure desktop target/tilo-app/web/calculator/bundle`; then qualifies the release private image worker in headless Chromium. |
| `runtime-macos` | `cargo check -p tilo-runtime --all-targets --locked`, on macOS |
| `sdk-macos` | the `sdk-linux` check's commands with `--target macos`, on macOS |
| `apps-macos` | `cargo build --locked $(python3 tools/ci/macos_apps.py)`, then `cargo nextest run` and the doctests with the same packages, on macOS |
| `real-fuse` | `bash scripts/ci-fuse-preflight.sh`, then `cargo test --locked -p tilo-fs --test integration_test -- --test-threads=1` and the check's other named tests |
| `native-tier1` | `tools/native-tier1.sh` on a host with pre-content fanotify, Btrfs and loop devices |
| `render-counters` | `cargo build --release -p ui-examples --example ui-perf`, then `python3 scripts/perf/render_bench.py counters target/release/examples/ui-perf` (pixel parity, then allocation and raster-pixel budgets at 4K; see [docs/ui-performance-benchmark.md](../../docs/ui-performance-benchmark.md#where-this-runs)) |
| `perf-<group>`, `perf-scheduled` (nightly) | `bash tools/ci/perf-scheduled.sh <group>`; `python3 tools/perf/compare.py --results <records.jsonl> --store https://code.tilo.so --groups <group>` compares records with the published store without writing to it; see [Performance results](../../docs/measurements/performance.md) |

`scripts/ci-run.sh` and `tools/ci/measure.sh`, which wrap some check commands,
only label and time a command; leave them out locally.

## Forks on GitHub Actions or stock Forgejo

Workflows live in `.forgejo/workflows/` and use only GitHub-compatible syntax
and actions pinned by commit, so a fork can copy them to
`.github/workflows/` or run them on a Forgejo instance with stock runners.
They need:

- **Runners.** Register runners with the [runner labels](#runner-labels), or
  replace each `runs-on` label with its hosted equivalent. Forgejo runners map
  a label to a container image in their `labels:` list. The labels emitted
  into matrices come from `runner_labels.py`.
- **Images.** `images.toml` pins the CI images by digest. They are public, so
  a fork can pull them as they are. To build its own, publish
  `os/Containerfile.ci-fast` and `os/Containerfile.builder` (t's
  `image-ci-fast` and `image-ci-builder` checks in `t.toml`), change the pins, and set
  `TILO_CI_REGISTRY`, `TILO_CI_BUILDER_REPOSITORY` and
  `TILO_CI_REGISTRY_TOKEN_REALM` for the builder lookup.
- **Artifact actions.** The `actions/upload-artifact` and
  `actions/download-artifact` v4 pins are Forgejo's builds (tag `forgejo/v4`),
  since upstream's v4 accepts only GitHub-hosted servers. A copy on GitHub
  replaces those two commit pins with upstream's v4. A Forgejo instance must
  resolve `actions/upload-artifact` and `actions/download-artifact` to
  Forgejo's forks (`code.forgejo.org/forgejo/`), or name the forks in the
  steps.
- **Caches, optional.** Cargo downloads use `actions/cache` keyed on
  `Cargo.lock` under `$CARGO_HOME`. `tools/ci/sccache.sh` enables a compiler
  cache only when the runner provides one; without it jobs compile cold.
  Neither changes a result.
- **Secrets, only for publication.** Pull-request jobs need no secret beyond
  the job token. Image, package and release publication on main need registry,
  signing and release credentials and are not expected to run in a fork.

Jobs that need more than an unprivileged container:

| Job | Needs |
|---|---|
| `release-imager.yml` | `/dev/kvm` |
