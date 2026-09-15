# Platform support

qOS targets native x86-64 and ARM64 throughout its components, dependency
artifacts, desktop packages and complete-system releases. Hardware models are
separate from CPU architectures: a board profile supplies firmware and an
optional device tree, while the shared kernel, userspace and installer paths
remain architecture-aware.

## Contracts

- Rust/release architecture names are `x86_64` and `aarch64`; OCI uses `amd64`
  and `arm64`. Normalize these at tool boundaries and reject unknown values.
- Native builds assert the runner's architecture before compiling. Cross
  compilation can check types and produce binaries; it does not replace
  native execution, sandbox or boot tests.
- Dependency repositories and kernel images must contain the requested
  architecture. Never relabel an x86-64 payload as ARM64, reuse a different
  architecture's cache as its release artifact, or skip unavailable required
  packages to make an image build succeed.
- A complete release index is promoted only after both architectures build
  and pass their required checks. Each descriptor's architecture and digest
  must agree with the verified child image; signing an index does not excuse
  missing or unverified children.
- Board-specific firmware remains local or comes from a reviewed redistribution
  source. Profile definitions, firmware hashes and kernel-specific DTBs enter
  the immutable image before UKI measurement and signing. See
  [hardware profiles](../../os/hardware/README.md).
- Runtime isolation, encrypted installation and verified boot requirements are
  identical across supported architectures. Architecture differences select
  the correct ABI, firmware or device model; they do not relax those boundaries.

## Validation and remaining work

The following tracks distinguish source support from released and boot-tested
support. A completed compile check alone is not a claim of hardware support.

| Area | Verified or implemented | Remaining validation/work |
| --- | --- | --- |
| Rust workspace | Native ARM64 `cargo check --workspace --all-targets --locked` passed | Native runtime/integration coverage in CI; external optional-feature builds |
| Compiler configuration | Linux hardening separated from Windows/macOS linker flags | Keep both native target configurations covered |
| Kernel | Native boot-image/RPM selection and ARM64 DTB payloads; shared hardening checks; verified two-architecture publication gate | Complete native kernel build and release-run acceptance |
| Hardware profiles | Declarative definitions, optional DTBs, exact firmware mappings, immutable UKI validation | Live acceptance per board and exact artifact |
| Build contexts | Actual offline Podman asset-stage builds pass for OS, RPM and CI-builder Containerfiles | Full native builds using the same contexts |
| Dependency snapshots | Base index has ARM64; native RPM/kernel publication verifies both signed children and their source revision | Produce complete ARM64 dependencies and update existing x86-only pins |
| Browser | WPE has native-architecture buildroot selection | Produce and validate an ARM64 WPE buildroot |
| Windows compatibility | Service crates compile on ARM64 | Fedora ARM64 lacks Wine/DXVK RPMs; pinned Proton and VM hardware definitions need an explicit ARM64 implementation |
| Imager | Native Windows host detection; ARM64 packaging checks PE architecture; both Windows release lanes and combined WinGet manifests required | Complete native Windows build, signed installer and UI acceptance |
| Desktop packaging | Build scripts contain ARM64 targets | Enable and validate native release lanes and architecture-specific assets |
| CI | Both native architectures selected for affected packages; builder publication waits for native cache tests; native Vulkan paths | Run new lanes against published native builder/dependency indexes; extend remaining nightly coverage |
| Speech | Unix socket backend scoped to Unix; portable speech tests pass on Linux ARM64 (31) and Windows ARM64 (28) | Live speech and accessibility acceptance on each supported host |

Validation was performed on a native ARM64 Fedora WSL environment. It exposes
software QEMU emulation but no `/dev/kvm`. Secure Boot, measured boot and
physical device support require their own acceptance evidence.

## Acceptance sequence

1. Compile every workspace component and its test targets on both native
   architectures. Exercise security-sensitive runtime tests natively.
2. Build the dependency, kernel, browser and CI-builder artifacts on native
   runners, keeping per-architecture caches and immutable identities separate.
3. Build the complete OS and installer for each architecture with the same
   component set; resolve platform-specific compatibility implementations
   explicitly and retain package completeness checks.
4. Validate the generic ARM64 installer-to-target cycle and the x86-64 cycle.
   Test hardware-profile images on their actual boards; a board DTB is not a
   QEMU `virt` device tree.
5. Publish only after the signed artifact and architecture completeness gates
   pass. Record the exact kernel, image, firmware and hardware identities for
   each accepted board.
