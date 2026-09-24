# CI performance and cache budget

Current deployment and measured startup results are recorded in the
[emergency runbook](ci-emergency-relief.md). CI has three complementary paths:

- `check.yml` uses one x86-64 shard for targeted push/PR feedback. It checks all
  targets in the affected reverse-dependency closure, while linking and running
  tests for directly changed packages plus `collab-types`, `pkg`, and `qdoc`.
  Repository-wide build inputs and unmapped workspace paths retain full-workspace
  check/test selection. Manual dispatch and the explicit desktop release branch
  request full checks on both native architectures (x86-64 and ARM64).
- `lint.yml` runs formatting, repository policy, dependency policy and dependency
  usage checks on the reserved quality lane with its pinned lean tools image.
- `nightly.yml` runs the complete workspace, all targets, examples, and live
  integration coverage. It is the coverage backstop rather than duplicated on
  every push.

Software-Vulkan golden tests are omitted from the affected-package check jobs
and run once at the end of the nightly test job. A nextest test group can
serialize goldens with each other, but it cannot make them exclusive of the
thousands of ordinary tests in the same invocation (or of the other shard).
Loaded llvmpipe/lavapipe runs have produced stable false pixel deltas; the
isolated tail reuses the nightly job's compiled artifacts, so it preserves full
daily visual coverage without another workspace build or additional test CPU.
The check, nightly, and baseline-refresh workflows also pin Vulkan's ICD to
Mesa lavapipe and require wgpu's `llvmpipe` adapter. Runner nodes can expose
different physical GPUs, and `WGPU_BACKEND=vulkan` alone selects the API rather
than a particular adapter; a golden produced on one node is not canonical
unless both the driver and adapter are fixed.

Nightly visual verification and the manual candidate renderer additionally use
the same registry image by immutable manifest digest. Advance those two digest
references together only after reviewing the new image's candidate bundle.
The candidate workflow uploads changed PNGs, a binary patch, its SHA-256, and a
diff inventory; it deliberately has no repository-write step.

Targeted feedback uses one target tree to avoid recompiling shared dependencies
across shards. Global build inputs and unmapped workspace changes use up to two
cost-balanced x86-64 shards, each with the existing four Cargo workers. Small
selections stay on one shard; coverage and runner labels are unchanged. This
bounds ordinary affected checks at eight Cargo workers; shared dependency work
can still be repeated across the two target trees. Full native qualification
may use up to two weighted shards per
architecture; small selections use fewer. Do not manually dispatch `check.yml`
as a cheap repeat of ordinary feedback: dispatch explicitly selects the entire
workspace on both architectures. Submit the updated task branch/PR normally;
the [scheduler owns admission and runner slots](../AGENTS.md#ci).

Check scheduling lives in `tools/ci/affected_packages.py`; app releases share
only the Git and Cargo graph helpers in `tools/ci/workspace_graph.py`. Changing
check shard counts does not invalidate apps. Graph changes still invalidate
release selection and the coordinator's trusted source hashes. Actual verifier
changes still require current acceptance and currently rebuild apps; reusing
binaries for verification-only changes remains separate work.

Root `Cargo.lock` and `Cargo.toml` changes are attributed rather than treated
as repository-wide when the difference can be proved. Changed, added or removed
lockfile records are propagated through the old and new locked graphs (which
cover every feature, target and dependency kind); workspace packages whose own
record changed or that depend directly on a changed external record are tested,
and every package the change reaches is checked with its reverse dependencies.
In the root manifest, `[workspace] members` edits select the added members and
the dependants of removed ones, and `[workspace.dependencies]` edits select the
members using those dependencies. Everything else (lints, profiles, patches,
`workspace.package`, `exclude`, glob members, a malformed or missing file, or a
lockfile header change) and every other global input keep the full workspace.
The native dependency job runs for an attributed lockfile change only when it
reaches the vendored `openh264-sys2` graph. App release planning shares the
same lockfile attribution.

To inspect the committed branch's selection locally, without starting a build
or requesting a runner, run from the checkout:

```sh
git fetch origin main
python3 tools/ci/affected_packages.py --base origin/main --head HEAD
```

This reads locked Cargo metadata and Git changes; it does not compile Rust.
The JSON distinguishes `selected` packages to check from `tested` packages to
link and run. Use a clean checkout: the changed-file list covers committed
changes, while metadata comes from the working tree. The preview does not replace
CI for the exact pushed head. Routine feedback is separate from the
[continuous app packaging and manual OS release roadmap](ci-product-matrix.md).

## Documentation and audited helper feedback

Rust compilation is explicitly inapplicable when the complete change consists
only of `docs/designs/*.md` and/or the audited files `AGENTS.md`, `DEVELOPMENT.md`,
`docs/ci-performance.md`, and `docs/ci-emergency-relief.md`. This is an allowlist,
not an exemption for all Markdown. Compiled docs such as
`crates/sdk/ui-core/CHARTS.md`, packaging
contracts and unknown paths keep normal selection. Mixed Rust changes still
select their affected closure, and manual full checks override the exception.

The exact source/test pairs in
[`QUALITY_ONLY_HELPERS`](../tools/ci/affected_packages.py) also use mandatory
quality CI without Rust compilation. This includes `check-changed-rustfmt.sh`,
`check_changed_rust_allows.py`, and their shared
`tools/tests/test_changed_rust_comparison.py`. Both the mapped source and test
must exist; deleting either restores normal Rust selection. Unknown helpers,
workflows and compiler wrappers retain normal selection, and mixing an audited
helper with Rust or global build inputs still selects the affected closure or
full workspace. Do not dispatch a full native check just to replace an intentional
skipped Rust row for an audited helper edit.

On pull requests only, that same existing-file prose allowlist and the exact
`QUALITY_ONLY_HELPERS` source/test pairs may also make the independent security
suite inapplicable. This security exception is narrower than the Rust exception:
workflow and other `QUALITY_ONLY_CONTROL_INPUTS`, OS or security files, manifests,
unknown paths, mixed changes, deletions, and helpers whose mapped observer is
missing all keep the security suite required. Main pushes and manual dispatches
always require it. The selector emits `security_required=true` by default and
only the pull-request workflow enables the audited exception.

For a change to either lint helper, run its production-path regression tests
locally without compiling Rust:

```sh
python3 -m unittest discover -s tools/tests -p 'test_changed_rust*.py'
```

This covers the original Rust-allow and rustfmt checks as well as their shared
PR comparison regressions. CI's `tools/policy/run.sh` still discovers all
`tools/tests/test_*.py`; the local subset does not replace required quality CI.

The workflow retains a skipped sentinel matrix row because a literally empty
matrix removed the job and stranded its aggregate on this Forgejo installation.
The builder and selector must succeed before the aggregate accepts any
intentional skip. It accepts a skipped security job only for a pull request with
an exact `security_required=false` selector output; missing or malformed outputs
fail. Repository policy, cargo-deny and cargo-machete remain active, so an
allowlisted change can still expose a broken baseline; skipped native and
security jobs do not certify those independent checks.

For local documentation of installer or packaging behavior, run the relevant
existing contract directly before waiting for CI:

```sh
python3 tools/dev.py check installer --step target-modes
python3 tools/dev.py check app-packaging --plan
```

Use the reported step names to select the contract you changed. Subset receipts
list omitted coverage and do not replace the full required CI result. The warm
production lint result in the runbook still took about 128 seconds including
policy/dependency checks. A future dependency-audit reuse optimization must bind
an accepted result to all relevant source/manifests, policy and tool identities;
path-only skipping of cargo-machete would hide pre-existing failures. That reuse
is not implemented or claimed here.

## Differential Clippy

`tools/ci/clippy-diff.sh` fails a change only for Clippy diagnostics that are
new relative to the comparison base. It counts one fingerprint per lint, file
and message. Threshold lints (`too_many_lines`, `cognitive_complexity`,
`too_many_arguments`, the size lints, and similar) report a measurement in their
message, such as `(104/100)`. The numbers are replaced before counting, so a
function that was already over its limit can change length without being
reported. A function that newly goes from under to over a limit adds a
fingerprint in its file and fails. A function that is already over its limit
and grows further is deliberately accepted.

Pull requests usually compare with the current `main` commit. The `clippy-base`
job in `check.yml` runs on every push to `main`. It lints the complete workspace
once and stores the normalized diagnostics in the Forgejo Actions cache. The
cache key includes the commit, builder image, `rustc`/Clippy versions, compiler
flags and the hashes of both Clippy helpers. The affected shard restores that
exact key and restricts the record to the workspace members `cargo clippy` lints
for its selected packages, so it does not have to check out and lint the base
itself.

A missing, stale or mismatched record falls back to the fresh base checkout,
as does a pull request whose base is not a recorded `main` commit. A comparison
against the record that finds new sites is repeated against a fresh base
checkout before the job fails, so the record never rejects a change that the
per-package comparison accepts. The reverse is a small, accepted gap. The
workspace-wide run unifies Cargo features across all members and also lints
the test targets of dependencies that are not themselves selected. The record
can therefore hold a few extra base sites. A new site is hidden only if it has
the same lint and message, in the same file, as one of those extra sites.
Affected selection always selects changed packages themselves, and those are
linted with every target in both runs.

The record follows the shared-cache trust model described below: any job can
write the Actions cache. The Clippy diff is a lint ratchet, not a security gate.

## Cache layers

- sccache stores compiler outputs in S3 namespaces configured by the runner,
  builder and `tools/ci/sccache.sh`. Inspect the effective namespace rather than
  assuming every lane has a separate prefix. This is the main cross-runner Rust
  cache. The builder workflow proves a cold write and a warm read in separate containers against
  an isolated canary prefix whenever the image changes.
- `actions/cache` stores only Cargo registry indexes, crate archives, and Git
  checkouts. We deliberately do not archive `target/`: it is large, expensive
  to compress and upload from every revision, and overlaps the compiler
  objects already handled by sccache. Within one job, later Cargo steps reuse
  the live target tree directly.
- The OS image build uses a registry-backed BuildKit cache because its
  intermediate container layers are not compiler outputs.

The layers solve different problems; replacing them with a single target-tree
archive would duplicate compiler storage and spend runner CPU compressing
revision-specific linker outputs.

### Compiler-cache trust

Namespaces organize compiler outputs and select the objects a build can reuse;
they do not restrict who can write them. The September 2026 review of the broker
configuration and infrastructure IAM policy found ordinary CI jobs and
interactive agents sharing cache credentials with bucket-wide object access.
This was a source/configuration audit, not a live IAM attestation. Changing a
prefix alone therefore does not establish a trusted writer boundary. Compiler
cache keys distinguish incompatible inputs; they do not authenticate the
producer of a cached object.

Ordinary development builds already use this shared cache trust model. Before
extending reuse to a security check or release gate that requires stronger
provenance, enforce writer isolation with separate credentials and storage
permissions, or promote verified objects into a cache whose writers are
restricted. Otherwise retain uncached verification for that gate. Keep the
[OS release's second no-cache build](security/threat-model.md#abuse-case-analysis-and-current-controls)
as a separate requirement; a cold-write/warm-read cache canary proves reuse,
not writer isolation.

The [kernel module signing fixture](../os/tests/kernel_module_signing_test.sh)
currently clears `RUSTC_WRAPPER` when building its helper. Retain that bypass
until its cache trust requirements are reviewed; the separate libkmod parser
checks runtime interoperability, not compiler-cache provenance. See
[local compiler-cache guidance](../DEVELOPMENT.md#sizing-the-compilation-cache)
and [packaging cache usage](packaging.md) for the existing reuse paths.

## Build-profile reuse

Required and nightly tests set both `CARGO_PROFILE_TEST_DEBUG=0` and
`CARGO_PROFILE_DEV_DEBUG=0`. Cargo puts the built-in test and dev profiles in
the same `target/debug` tree, but debuginfo is part of an artifact's
fingerprint. Leaving dev at its default therefore made the all-target and
example steps rebuild dependencies that nextest had already emitted, solely to
add debugger data that CI never consumes.

A controlled qdoc-pdf probe used fresh target directories and no compiler
cache. After `cargo test --no-run`, building its examples took 3.35 seconds
with matching zero-debuginfo profiles versus 54.47 seconds with Cargo's default
dev debuginfo. A smaller collab-types test/check probe reduced the subsequent
all-target check from 10.00 to 7.51 seconds. The workflows retain separate
all-target and linked-example gates—the coverage is unchanged—while sharing
compatible dependency artifacts between them.

## Measurements

Numbers below come from Forgejo's `/usr/bin/time` output and sccache counters.
They include all 11.5k workspace tests, not a synthetic microbenchmark.

| Run | Date | Wall | User + system CPU | sccache hit rate |
| --- | --- | ---: | ---: | ---: |
| nightly 1279 | 2026-08-31 | 573.00 s | 1,708.26 s | 20.61% |
| nightly 1298 | 2026-08-31 | 436.71 s | 1,731.32 s | 93.35% |

The effective remote cache reduced end-to-end wall time by 23.8% while total
process CPU stayed within 1.4%. Run 1298 recorded 3,883 compile requests, 2,806
hits and 200 misses among 3,015 executed requests (Rust 90.60%, C/C++ 99.86%).
Older fully warm runs reached 100% and completed compilation in roughly two
minutes; cold run 944 was effectively uncached at 0.07%.

These are operational baselines, not permanent targets. Keep `tools/ci/measure.sh`
around the test/check commands and compare wall time *and* total CPU before
adding shards or caches; a faster wall clock that doubles runner CPU is a
regression for this installation.

For current workflow startup, `tools/ci/timing_report.py` on main at `d86e8f437`
also reports task receipt to container creation (`runner_to_create_seconds`).
This interval remains measurable when no Docker pull event is logged; the
pull-to-create interval then remains unavailable instead of becoming zero.
Absence of a pull event alone does not prove a cache hit. Match the exact run,
image digest and worker/isolated-seed evidence before attributing startup savings
to reuse. Queue time, container setup and check execution remain separate costs.
