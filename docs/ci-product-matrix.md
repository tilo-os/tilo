# Product packaging coverage and next Windows qualification

Source audit: main `24c000fa4` (after requested checkpoint `709ca95a1`),
2026-09-19. A defined workflow is not evidence that every target has a ready
runner, successful native build or releasable artifact.

| Product | Existing path | Remaining qualification or deferral |
| --- | --- | --- |
| Linux/qOS native apps | `release-apps.yml`: affected-app PR planning; explicit/tagged signed qpkg and runtime collection, x86-64 and ARM64 | Continuous main trigger remains paused. Dependency selection, compiler reuse, immutable runtime/package receipts and guarded publication must complete before enabling it. qOS ISA policy is explicit; this is not a universal Linux standalone installer. |
| Whole desktop / Linux distributions | `package-desktop.yml`: Arch, Nix, Ubuntu 24.04, Debian 13, Fedora 44; architecture jobs run distro builds serially | PR contracts remain cheap; weekly/manual distribution builds and main payload publication remain. ARM requires the workflow's native capability gate. Distribution install tests do not establish bootable OS/device acceptance. |
| Setup application, Linux | `release-imager.yml`: default explicit Linux x86 smoke; full native/tag lanes add ARM64, AppImage and release checks | Catalog, signing, installation and physical-media qualification remain distinct. |
| Setup application, Windows | Existing x86-64/ARM64 jobs use `imager-windows-x86_64` / `imager-windows-aarch64`, MSVC targets, imager tests, package build and signature checks | Enabled for imager tags or explicit `full_native=true`, not ordinary pushes. Actual native host availability must be checked before dispatch. |
| Native Windows apps | Merged toolkit plus `tools/windows/install-apps.ps1`; production allowlist currently contains 30 selectable apps | This is a local build/install path, not continuous distributable app packaging. Versioned archive/MSIX format, signing/promotion, native dependency identity, disposable installation and runtime tests are not a qualified release path. Consider broad app coverage after a bounded native qualification. |
| OS x86-64/ARM64 | `build-image.yml`: weekly integration, explicit builds and deliberate tag/manual publication | Retained component/development workflows cover targeted kernel, source, Windows compatibility guest and resume/finalize work. A generic architecture image is not qualification for every laptop/device. Keep device acceptance explicit. |
| macOS products | Imager and Homebrew cask lane definitions remain disabled | Deliberately deferred. Do not delete definitions or restore macOS publication with Windows work. No current standalone macOS app release qualification is asserted. |

At this audit, neither the admin runner listing nor the repository runner listing
advertised a Windows/imager Windows label. Ephemeral runners can be absent while
idle; this observation is not proof that provisioning is impossible. The existing
`tools/ci/runners/imager-ephemeral-windows.ps1` derives labels from native
processor architecture.
Do not route Windows work onto Linux `runner` just because it is available.

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
directories, reuse Linux qpkg runtime artifacts as Windows binaries, or set
`CARGO_TARGET_DIR`. The toolkit's dependency changes must invalidate affected
apps. Coalesce future pending packaging requests while preserving the active build;
keep cheap per-commit checks and deliberate OS releases independent.
