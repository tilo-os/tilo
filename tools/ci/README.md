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
