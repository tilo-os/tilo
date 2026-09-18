# CI build-graph spike

Decision and limitations: [experiment report](../../docs/designs/ci-build-graph-experiments.md).
Nothing here is connected to the production workflows. Generated workspaces are
kept outside this repo. No OS image or GUI fleet is built by these scripts.

## Reproduce

Use a clean source checkout at `032ea136f5d6a7cbd2ddcf87e5dd7ab80fbd971a`, Python
3.12+, native Linux ARM64, gcc/tar, Rust 1.97.1, Bazel 9.2.0 and sccache 0.12.0.
Bazel downloads rules_rust/toolchain dependencies. The probes limit compilation to
two jobs and use fresh experiment-local output/cache directories. Every `--out`
must be a new directory. No Cargo target directory is shared or overridden.

Example, substituting paths for your machine:

```sh
python3 experiments/ci-build-graph/probe.py \
  --root /workspace/qos --out /scratch/qos-ci-probe \
  --bazel /tools/bazel
python3 experiments/ci-build-graph/sccache_probe.py \
  --slice /scratch/qos-ci-probe/slice --out /scratch/qos-ci-sccache
python3 experiments/ci-build-graph/summarize.py /scratch/qos-ci-probe
python3 experiments/ci-build-graph/queue_model.py
python3 experiments/ci-build-graph/verify_results.py
```

`verify_results.py` checks the **committed results**, not a new run. To check a
rerun, copy its `results.json` to `results/build-probe.json`, the sccache result to
`results/sccache-probe.json`, and the summarizer/model output to
`results/bazel-actions.json` / `results/queue-model.json` first.

On a Windows Git worktree read through WSL, Linux Git cannot interpret the Windows
`.git` indirection. Run `git rev-parse HEAD` on Windows and pass that exact value as
`--revision` to the probe. It records SHA-256 hashes of copied source modules too.
Do not reinterpret this override as checking uncommitted source against the revision.

Bazel binary used:
[official Linux ARM64 9.2.0](https://releases.bazel.build/9.2.0/release/bazel-9.2.0-linux-arm64).
Verified SHA-256:
`049dd21f40ad979db11c3ee68c96a42ce75f1185e69ac61ab20de1501427a410`.

## Contents and scope

- `probe.py`: real Cargo selector inputs plus a generated Cargo/Bazel workspace.
  Calculator, qdoc-order and atomic-file production modules are copied byte for
  byte initially. Tiny drivers and shared-library topology are synthetic.
  Source mutations append an unused exported function. No toolkit, ABI change,
  native/generated dependency, Windows build, qpkg signing or OS is modeled.
- `sccache_probe.py`: two source paths plus a repeat at the original path with a
  fresh Cargo target; cache is local and separate from production caches.
- `summarize.py`: removes huge input inventories from Bazel execution logs, retaining
  target labels, cache outcomes and execution status.
- `queue_model.py`: hypothetical arrival/service trace; no network or CI dispatch.
- `results/`: successful complete run (local run6), sccache run3 and the model output.
  Earlier harness attempts exposed packaging a sandbox symlink rather than binary
  contents; fixed with tar dereferencing, then rerun with extraction/execution checks.
  They are not counted as successful package validation.

Bazel ignores system/user/workspace rc files and explicitly disables remote cache,
execution and downloader endpoints. sccache uses an empty experiment-local config,
clears inherited `SCCACHE_*` options, and limits its local cache to 128 MiB.

The archive actions use host tar and the compiler host toolchain. Their inputs are
sufficient for this same-machine experiment, not a production hermeticity contract.
The Bazel fresh-output test shares a local disk action cache; remote HTTP service
performance, credentials and cross-host correctness are deliberately unclaimed.

The first Bazel invocation includes dependency bootstrap, while Cargo already has
its compiler installed. Cargo times do not include equivalent packaging, build
flags differ, and some runs overlapped a required shell check. Use action
counts and cache outcomes here, not relative elapsed times, to make decisions.

The full qualification and product matrix remain in the linked report and design
documents. This spike is not an adoption of Bazel or a completed CI migration.

Local setup note: the experiment downloaded Bazel under the WSL scratch directory
and used the already-installed Linux Rust 1.97.1 toolchain. An earlier Windows
metadata command also installed the named Rust 1.94.1 toolchain through rustup;
it did not change the default toolchain. The final build probes use Linux 1.97.1.
