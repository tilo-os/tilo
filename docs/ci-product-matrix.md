# Product packaging coverage and next Windows qualification

Source audit: main `d34b49c38`, 2026-09-21. A defined workflow is not evidence
that every target has a ready runner, successful native build or releasable
artifact.

See the [CI and packaging roadmap](ci-production-roadmap.md) for the build graph,
promotion model, service levels and phased production plan.

## Routine change lifecycle

Routine work starts on a task branch and lands through one updated pull request.
The required check and lint contexts must pass for the exact head before merge.
`check.yml` and `lint.yml` preserve a result for every `main` commit; dependency
selection keeps the Rust lane focused while nightly CI remains the full-workspace
backstop.

A PR merges once the required checks pass for its current head and the branch
is up to date with `main`; reviews are informational and never block
integration. A new push requires fresh checks. See
[Routine delivery workflow](../DEVELOPMENT.md#routine-delivery-workflow) for the
rule, and [Release pipeline](release-pipeline.md) for heavy-build and
publication triggers.

## What runs for each event

| Event | Routine feedback | Packaging and release behavior |
| --- | --- | --- |
| Task-branch push | Fast quality for that commit: changed-file formatting, lint-exception policy, repository policy, `cargo deny`, and `cargo machete` | No app publication or OS build |
| Pull request to `main` | Fast quality plus affected reverse-dependency checks and applicable security contracts; the app workflow reports its dependency-selected plan | No tapp publication and no full OS build |
| Push to `main` | Fast quality and affected checks retain a result for that exact commit | Relevant app/package inputs publish the dependency-selected x86-64 app set, reuse unchanged collection members, and submit newly built tapps to the signed registry; no OS build or publication runs per push |
| Nightly, tag, or explicit dispatch | Full-workspace and product-specific qualification according to the workflow | App and desktop release matrices expand only on their documented triggers; an OS version is published by an OS tag, a `main` dispatch with `publish=true`, or the nightly `main` run when `main` has moved |

Do not interpret a skipped Rust row as missing CI. The aggregate accepts it only
for the audited documentation/helper allowlist; quality remains required. Use the
selector summary to see which packages and security contracts applied to the
exact head.

| Product | Existing path | Remaining qualification or deferral |
| --- | --- | --- |
| Linux/Tilo native apps | `release-apps.yml`: continuous dependency-selected x86-64 packaging on `main`; explicit/tagged signed tapp and runtime collection on x86-64 and ARM64 | Main compiles and packages selected app binaries and reuses immutable collection members; full-collection dependency features preserve the shared runtime ABI. ARM64 remains available for explicit/tagged releases while ordinary main packaging stays on the configured x86-64 lane. Tilo ISA policy is explicit; this is not a universal Linux standalone installer. |
| Whole desktop / Linux distributions | `release-desktop.yml`: Arch, Nix, Ubuntu 24.04, Debian 13, Fedora 44; architecture jobs run distro builds serially | PR contracts remain cheap; weekly/manual distribution builds and main payload publication remain. ARM requires the workflow's native capability gate. Distribution install tests do not establish bootable OS/device acceptance. |
| Setup application, Linux | `release-imager.yml`: default explicit Linux x86 smoke; full native/tag lanes add ARM64, AppImage and release checks | Catalog, signing, installation and physical-media qualification remain distinct. |
| Setup application, Windows | Existing x86-64/ARM64 jobs use `imager-windows-x86_64` / `imager-windows-aarch64`, MSVC targets, imager tests, package build and signature checks | Enabled for imager tags or explicit `full_native=true`, not ordinary pushes. Actual native host availability must be checked before dispatch. |
| Native Windows apps | Merged toolkit plus `tools/windows/install-apps.ps1`; production allowlist currently contains 30 selectable apps | This is a local build/install path, not continuous distributable app packaging. Versioned archive/MSIX format, signing/promotion, native dependency identity, disposable installation and runtime tests are not a qualified release path. Consider broad app coverage after a bounded native qualification. |
| OS x86-64/ARM64 | `os-dev.yml` (Build / OS dev): every main push and image PR, cached, publishes `tilo-ci`/`tilo-vm` dev tags. `build-image.yml` (Release / OS): nightly `main` publication (skipped when `main` is already published) and tag/manual releases, reproduced and signed | Retained component/development workflows cover targeted kernel, source, Windows compatibility guest and resume/finalize work. A generic architecture image is not qualification for every laptop/device. Keep device acceptance explicit. |
| Dev VM disks | Integration `build-image.yml` lanes build `tilo-vm` FROM the image they just built (`os/dev/build-dev-image.sh`), install and boot that disk as their only install/boot gate (the installed-desktop checks plus a root tilo-vmd handshake), and publish `tilo-vm:<series>-ci-<arch>`, the growing `tilo-vm:<series>-ci` index and a per-architecture `tilo-vm-disk-<arch>.json` from within the lane (`os/dev/publish-dev-disk.sh`). Publication lanes request `dev-disk.yml` for their own architecture from the signed child; it can also be dispatched by hand for an existing image | Each architecture publishes independently; `vm create` picks the newest tag with the host's architecture. Integration lanes boot the unsealed dev image, so only publications exercise the sealed UKI boot. UTM, libvirt and Hyper-V import are not qualified. |
| macOS Imager | Native x86-64 and ARM64 signed/notarized DMG and app-zip lanes; first-party Homebrew cask with native install tests | Enabled for `full_native` rehearsal and release tags; requires ephemeral native runners and protected signing/notarization brokers. This does not qualify unrelated standalone macOS apps. |

At this audit, neither the admin runner listing nor the repository runner listing
advertised a Windows/imager Windows label. Ephemeral runners can be absent while
idle; this observation is not proof that provisioning is impossible. The existing
`tools/ci/runners/imager-ephemeral-windows.ps1` derives labels from native
processor architecture.
Do not route Windows work onto Linux `runner` just because it is available.

## Production-readiness sequence

1. Operate the bounded automatic merge path and expand its path allowlist only
   with the same exact-head, identity-separation, recovery and reconciliation
   guarantees.
2. Observe dependency-selected app packaging on `main`, including shared-runtime
   compatibility, immutable receipts and atomic channel promotion; extend routine
   packaging to ARM64 after its native lane has sustained reliable throughput.
3. Publish the OS through a release tag, a `main` dispatch with `publish=true`,
   or the nightly `main` run, only after image, reproducibility and VM gates
   pass; never per push.
4. If native Windows app distribution becomes worthwhile, qualify a versioned,
   signed archive or MSIX on disposable native hosts. The shared toolkit and local
   installer already exist; distribution and clean-machine runtime evidence do not.
5. Keep the enabled macOS Imager and Homebrew gates fail-closed so missing
   native infrastructure or qualification evidence blocks publication.

## Executable planning without installation

```powershell
./tools/plan-windows-apps.ps1 -Apps calculator,notes -Architecture x86_64
./tools/tests/windows_app_plan.ps1
```

The planner reads the production installer's PowerShell AST allowlist without
executing it. It rejects unknown applications, identifies the exact native label
and MSVC host, emits one locked Cargo build for the selected packages plus the
notification helper when Clocks is selected, and records
relevant source/toolchain/runtime-helper input hashes. It does not download,
compile, write Start entries, sign, publish or change a workflow trigger. Its hash
inventory is diagnostic, not an artifact cache key or proof of dependency closure.

## Small next native workflow

Prepare a manual-only, nonpublishing qualification with explicit application list
and architecture. Run the planner first; admit only a confirmed native Windows
worker with PowerShell 7, pinned Rust/MSVC, Cap'n Proto, LLVM readobj and required
native libraries. Start with Calculator and Notes on x86-64, then repeat on ARM64;
add media/PDF/notification apps to exercise those distinct runtime boundaries.

Run the existing installer only in a disposable Windows user/VM: it intentionally
changes LocalAppData, Start entries and some per-user registration. Preserve its
DXC/PDFium checksum checks, PE-import-driven GStreamer assembly, architecture
consistency and crypto runtime checks. Require native launch/actions and installed
runtime tests, retain logs and binary/runtime checksums, and clean up that VM.
Do not call this a distribution package until a relocatable, versioned artifact
and its clean-machine consumer are defined and tested.

Use separate Windows architecture/compiler/SDK/native-library cache identities.
Reuse Cargo downloads and qualified compiler outputs; do not share writable target
directories, reuse Linux tapp runtime artifacts as Windows binaries, or set
`CARGO_TARGET_DIR`. The toolkit's dependency changes must invalidate affected
apps. Coalesce future pending packaging requests while preserving the active build;
keep cheap per-commit checks and deliberate OS releases independent.
