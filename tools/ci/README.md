# CI tooling

This directory owns the implementation used only to plan, execute, measure,
and report continuous-integration work. It includes affected-package
selection, compiler-cache setup, lint ratchets, builder resolution, and CI
receipts.

Repository policy and release utilities remain in `tools/`; their behavior is
useful outside CI. Tests for both groups remain in `tools/tests/` so the single
repository-policy discovery command exercises the complete tooling surface.

Workflows should invoke these files through `tools/ci/...`. When adding a new
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

## Running pull-request CI locally

Each pull-request job runs commands from this repository; the workflow only
chooses the runner and container and passes the event. Run them from the checkout on Linux, in
the builder image (`os/Containerfile.builder`) or a developer image with the
same tools (`dist/dev/Containerfile.fedora44`). Lint and planning jobs run in
the smaller `os/Containerfile.ci-fast`.

| Workflow / job | Local command |
|---|---|
| `check.yml` select | `python3 tools/ci/affected_packages.py --metadata "$TILO_CURRENT_WORKTREE_METADATA" --base origin/main` previews the plan: packages per shard, suites, examples |
| `check.yml` security | `bash tools/run-security-boundary-invariants.sh` |
| `check.yml` suites | `python3 tools/ci/suites.py run <suite>...` (`suites.py list` names them) |
| `check.yml` test | `scripts/test-local.sh -p <package>...` (see [Running tests like CI](../../DEVELOPMENT.md#running-tests-like-ci)) |
| `check.yml` check | `cargo check --locked --all-targets -p <package>...` |
| `check.yml` clippy | `tools/ci/clippy-check.sh -p <package>...` or `--workspace` |
| `check.yml` android | `bash tools/ci/install-android-ndk.sh`, then `cargo clippy --locked -p <app> --examples --features android --target aarch64-linux-android -- -D warnings` |
| `check.yml` wasm | `tools/ci/check-wasm.sh`; `tools/ci/build-web-examples.sh example-<name>...` builds the web examples |
| `lint.yml` | `tools/policy/run.sh`, `bash tools/ci/check-changed-rustfmt.sh origin/main`, `python3 tools/ci/check_changed_rust_allows.py origin/main`, `cargo deny --workspace check`, `cargo hakari generate --diff && cargo hakari manage-deps --dry-run`, `bash tools/ci/check-unused-dependencies.sh --changed origin/main ''` |
| `app-sdk.yml` policy | `python3 tools/policy/check_app_portability.py`, `python3 tools/policy/check_app_capabilities.py`, `python3 tools/web/generate_web_desktop.py --check` |
| `app-sdk.yml` linux | `cargo build -p tilo-app-cli --bin tilo-app`, then `target/debug/tilo-app build --target linux apps/calculator` and `target/debug/tilo-app package --target linux apps/calculator --output calculator.tapp` |
| `app-sdk.yml` web | `target/debug/tilo-app build --target web --bundle desktop apps/calculator`; `node --test crates/platform/*/web/*.test.mjs` |
| `app-sdk.yml` web-size | `target/debug/tilo-app build --target web --bundle desktop --release apps/calculator`, then `python3 tools/web/wasm_size_ratchet.py measure desktop target/tilo-app/web/calculator/bundle`; uploads/downloads the exact release worker, verifies hashes and qualifies it in disposable staging. Runs alongside the dev mount. |
| `app-sdk.yml` macos | the linux lane's commands with `--target macos`, on macOS with Xcode |
| `ci-filesystem.yml` real-fuse | `bash scripts/ci-fuse-preflight.sh`, then `cargo test --locked -p tilo-fs --test integration_test -- --test-threads=1` and the job's other named tests |
| `ci-filesystem.yml` native-tier1 | `tools/native-tier1.sh` on a host that meets `runner-native-storage` |
| `ci-render-counters.yml` counters | `cargo build --release -p ui-examples --example ui-perf`, then `python3 scripts/perf/render_bench.py counters target/release/examples/ui-perf` (pixel parity, then allocation and raster-pixel budgets at 4K; see [docs/ui-performance-benchmark.md](../../docs/ui-performance-benchmark.md#where-this-runs)) |
| `perf-scheduled.yml` (nightly, not per PR) | `python3 tools/perf/compare.py --results <records.jsonl> --store https://code.tilo.so --groups <group>` compares records with the published store without writing to it; see [Performance results](../../DEVELOPMENT.md#performance-results) |

`scripts/ci-run.sh` and `tools/ci/measure.sh`, which wrap most job commands,
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
  `os/Containerfile.ci-fast`, `os/Containerfile.ci-control` and
  `os/Containerfile.builder` (`build-ci-fast-images.yml`,
  `build-ci-builder.yml`), change the pins, and set `TILO_CI_REGISTRY`,
  `TILO_CI_BUILDER_REPOSITORY` and `TILO_CI_REGISTRY_TOKEN_REALM` for the
  builder lookup.
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
| `ci-filesystem.yml` real-fuse | `/dev/fuse` and `CAP_SYS_ADMIN` in the job container |
| `ci-filesystem.yml` native-tier1 | a privileged container on Linux 6.14+ with pre-content fanotify, Btrfs and loop devices |
| `ci-staging-loop.yml`, `build-env-tools.yml` | a privileged container |
| `ci-development-signing.yml` | Docker that can start privileged containers |
| `dev-disk.yml`, `build-legacy-guest.yml` | `/dev/fuse`, loop devices and, for speed, `/dev/kvm` |
| `release-imager.yml` | `/dev/kvm` |
| Image, kernel and buildroot builds | Docker with Buildx |

## Planning without local Cargo

`affected_packages.py` invokes local `cargo metadata` by default. On a machine
where local Cargo is prohibited, pass an existing metadata export explicitly:

```sh
python3 tools/ci/affected_packages.py \
  --metadata "${TILO_CURRENT_WORKTREE_METADATA:?validated current-worktree export required}" \
  --base origin/main --shards 2
```

Use a complete `cargo metadata --locked --no-deps --format-version 1` export
from the existing remote builder or CI, verified against this checkout's
workspace manifest, lockfile and every member manifest. Package manifest,
dependency and target paths in the export must refer to this checkout; a
remote absolute path cannot be consumed unchanged. A stale or incomplete
export is not selection evidence. Do not substitute a hand-written partial
graph. Bind the export to the exact source revision and compare every root/member
manifest and the lockfile with the producing worktree before setting
`TILO_CURRENT_WORKTREE_METADATA`. Keep that validation evidence beside the export;
refresh it after any source, manifest, lockfile or target-inventory change. The
prescribed `AGENTS.md` invocation fails before starting Python when the validated
export variable is unset or empty. Missing JSON fails rather than invoking Cargo.

Use the stable runtime guard when invoking a selector from an older worktree.
It validates the bundle before running that worktree's historical selector and
allows only Git subprocesses during selection; Cargo, alternate executable
launchers and shell fallbacks are rejected even if old selector code ignores
the metadata argument. No source files in the caller worktree are changed.
Set `TILO_CI_TOOLS` to `tools/ci` in a current trusted checkout, then run:

```sh
python3 "$TILO_CI_TOOLS/offline_affected_plan.py" --root "$PWD" \
  --bundle "${TILO_CURRENT_WORKTREE_METADATA_BUNDLE:?source-bound CI export required}" \
  --base origin/main --shards 2
```

This is the canonical agent caller in `AGENTS.md`. The direct selector's current
version rejects its local default before Cargo starts. Old relative entrypoints
do not inherit that change automatically: migrate their callers to this stable
launcher, rather than relying on prose in an old branch. The normal CI selector
retains its Cargo-backed default in the Forgejo job environment.

For a clean committed worktree, the explicit CI export workflow provides this
supported path without local Cargo:

```sh
source_sha=$(git rev-parse HEAD)
fgj actions workflow run dev-source-metadata.yml -R tilo/tilo \
  --ref main -f source_sha="$source_sha"
# After the export run succeeds, replace RUN_NUMBER with that exact run:
fgj actions run artifacts RUN_NUMBER -R tilo/tilo \
  --download "current-source-metadata-$source_sha" --output /tmp/source-metadata.zip
python3 -m zipfile -e /tmp/source-metadata.zip /tmp/source-metadata-export
python3 tools/ci/current_source_metadata.py validate --root . \
  --input /tmp/source-metadata-export/current-source-metadata.json \
  --output target/ci/current-source-metadata.json
export TILO_CURRENT_WORKTREE_METADATA="$PWD/target/ci/current-source-metadata.json"
export TILO_CURRENT_WORKTREE_METADATA_BUNDLE=/tmp/source-metadata-export/current-source-metadata.json
```

The bundle records the producing commit, tree and hashes of all tracked Cargo
manifests, the lockfile and Cargo configuration. Validation rejects dirty or
uncommitted source, mismatched source or manifests, incomplete workspace package
inventories, external paths and absent targets. It preserves every dependency
record, including optional, build and target-specific dependencies, and remaps
package IDs and paths to the validated caller worktree. Keep the bundle beside
its validated export. Refresh after a source change.

A Cargo drop-in may execute `metadata` locally because it is a noncompiling
subcommand. Verify where it runs instead of assuming a remote export. Until a
verified export is available, leave the affected plan to the normal CI selector. The regression
`tools/tests/test_ci_plan_without_cargo.py` exercises the real metadata-file
CLI with a Cargo blocker and retains target, optional and build dependency
selection plus the security gate.
