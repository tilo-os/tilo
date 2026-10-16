# Platform support

Tilo targets native x86-64 and ARM64 throughout its components, dependency
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
| Kernel | Native boot-image/RPM selection and ARM64 DTB payloads; shared hardening checks; verified two-architecture publication gate | Native recovered RPM payload verified; signed release and boot acceptance remain |
| Hardware profiles | Declarative definitions, optional DTBs, exact firmware mappings, immutable UKI validation | Live acceptance per board and exact artifact |
| Build contexts | Actual offline Podman asset-stage builds pass for OS, RPM and CI-builder Containerfiles | Full native builds using the same contexts |
| Dependency snapshots | Base index has ARM64; native RPM/kernel publication verifies both signed children and their source revision | Produce complete ARM64 dependencies and update existing x86-only pins |
| Browser | Chromium engine runtime built per architecture from the pinned CEF distribution in the image build; the rootfs check resolves every library it loads | Real-boot page-load acceptance on ARM64 hardware |
| Windows compatibility | Service crates compile on ARM64 | Fedora ARM64 lacks Wine/DXVK RPMs; pinned Proton and VM hardware definitions need an explicit ARM64 implementation |
| Imager | Native Windows ARM64 executable builds, loads, and renders an offscreen fixture; library tests pass (138); file identity comes from the open native handle; ARM64 packaging checks PE architecture; both Windows release lanes and combined WinGet manifests required | Signed installer and live UI acceptance; investigate host Vulkan loader warnings separately from the successful offscreen render |
| Desktop packaging | Build scripts contain ARM64 targets; tagged and explicit `full_native` desktop runs select native ARM64 with `TILO_DESKTOP_ARM64_RELEASES="1"` | Qualify architecture-specific release assets and installed journeys; reconcile advertised ARM64 desktop support through the product decision |
| CI | Both native architectures selected for affected packages and ordinary app builds by default; builder publication waits for native cache tests; native Vulkan paths | Run new lanes against published native builder/dependency indexes; extend remaining nightly coverage |
| Speech | Unix socket backend scoped to Unix; portable speech tests pass on Linux ARM64 (31) and Windows ARM64 (28) | Live speech and accessibility acceptance on each supported host |
| Media | Native media tests pass on Linux ARM64 (98) and Windows ARM64 (91); Linux decode/encode features and affected video apps compile; file URLs use native path conversion; Unix DMA buffers use the target's standard file ABI | Live GPU/media acceptance |
| Document quotas | Native file locks preserve cross-process serialization; quota regressions pass on Linux ARM64 and Windows ARM64 (10 each) | Full document storage integration coverage |
| Android app host: build | Calculator and Notes `<app>-android` examples pass `clippy -D warnings` for `aarch64-linux-android` and `x86_64-linux-android`, required by CI whenever they are affected; debug APKs build for both ABIs with NDK r29 | No CI APK build (it needs the Android SDK); release signing; the other apps have no Android example |
| Android app host: runtime | x86_64 debug APKs of both apps run on an API 36 x86_64 emulator: touch and tap-to-caret, the input method's initial selection (`dumpsys input_method`), a Storage Access Framework chooser returning to a redrawn app, and the landscape keypad layout | No arm64-v8a device acceptance; TalkBack explore-by-touch unverified (no TalkBack or hover injection on the emulator); hardware Enter does not reach text fields; no persisted pipeline cache, so each cold start compiles shaders |
| Hosted identity | Remote broker client remains portable; Windows rejects unsupported local authority storage without creating files | Local authority ownership and permission enforcement remains a Unix service; native Windows authority storage is not implemented |

Desktop lane selection is implemented in
[`release-desktop.yml`](../../.forgejo/workflows/release-desktop.yml), as described in
[the release pipeline](../release-pipeline.md#fast-validation-versus-release-gates).
Weekly and default manual desktop builds select amd64; PRs run preflight only.
[The install contract](../install-contract.json) still lists ARM64 desktop sessions as
planned. Enabled build lanes are not a release-support or hardware acceptance receipt.
The release surface/support-matrix decision remains with the product owner; this
source reconciliation does not
change that contract, select a launch surface, or waive the two-architecture complete-OS
and Imager requirements.

Validation was performed on a native ARM64 Fedora WSL environment. It exposes
software QEMU emulation but no `/dev/kvm`. Secure Boot, measured boot and
physical device support require their own acceptance evidence.

After the hosted portability changes, native Linux Imager test targets and
the required `cargo check -p shell --tests --locked` passed again. The Windows
Imager executable also passed its package-loading smoke probe. The offscreen
render uses fixture data and does not establish that an ARM64 OS image is
available or that live installation works.

### Native build evidence and compatibility gaps

A native ARM64 kernel build produced 5,826 AArch64 modules and the Lenovo
Yoga Slim7x DTB. After host disk exhaustion during build-layer export, the
preserved stage was packaged offline using the existing packaging tools image.
All 7,485 regular RPM files match the preserved archive, including their modes;
RPM owners are root, package digests and repository metadata checksums pass.
This recovery artifact is local and unsigned. A diskless QEMU ARM64 boot reached userspace with kernel 7.2.5. Its module
loader accepted the original signed dummy module and rejected unsigned and
tampered copies with EKEYREJECTED. This tests signature enforcement for that
module; it does not cryptographically reverify every module. UEFI, full OS and
physical-board acceptance remain pending.
The kernel build now removes compiler intermediates after staging and validation,
before the container builder exports its layer.

ARM64 Windows VMs also require an explicit protected UEFI variable-store path.
[QEMU's UEFI variable service](https://www.qemu.org/docs/master/devel/uefi-vars.html)
provides this on ARM through `uefi-vars-sysbus`.
[Libvirt's `varstore` configuration](https://libvirt.org/formatdomain.html)
supports it starting with libvirt 12.1.0. The queried Fedora ARM64 repository
provides libvirt 12.0.0, while its edk2 package already contains qemuvars firmware
and enrolled Secure Boot variable templates. A maintained newer libvirt package and
native VM acceptance are still required. Domain rendering now selects ARM64 `virt`, GICv3 and TPM TIS with
protected variable storage and explicitly enrolled Secure Boot keys; x86 keeps
its q35/SMM configuration. Both domain variants are covered by native ARM64
unit tests, but XML rendering alone does not establish guest boot support. Both
rendered definitions also pass the libvirt 12.7.0 Relax NG schema using
`os/tests/windows_vm_schema.sh`. That source archive has SHA-256
`7ec1a04e7e4f4069353d4daac117bbe869287f5b202695de61fe1b079efb6cb6`;
its detached release signature verifies against the key obtained from the
[official download server](https://download.libvirt.org/gpg_key.asc), fingerprint
`453B65310595562855471199CA68BE8010084C9C`. This records upstream HTTPS key
provenance, not independent certification of that key. Native RPM construction
from the unmodified upstream spec completed with 161 passing tests, one expected
failure and no skips. The resulting 79 RPMs pass package digest verification;
the client, library and QEMU driver are aarch64. These local packages have not
yet replaced the production package snapshot.

An isolated libvirt 12.7.0/QEMU 10.2.2 container successfully converted the
rendered ARM domain into QEMU arguments after substituting TCG and its `max`
CPU for the diagnostic host, which has no KVM. The conversion selects the
enrolled qemuvars firmware and `uefi-vars-sysbus`. It also exposed unsupported
ARM ACPI sleep declarations, which are now omitted, and the implicit multi-socket
CPU topology, now explicitly one socket with the allocated number of cores.
This proves domain conversion, not guest boot or native KVM acceptance.

The native FEX runtime compiled, but runtime acceptance remains open.
See [FEX build validation](../../os/docs/fex-build-validation.md) for the pinned
assembler regression fix, root filesystem prerequisites and remaining checks.

The pinned `virtio-win-0.1.302-1.noarch.rpm` was downloaded and verified against
its manifest SHA-256. Inspection found 76 ARM64 PE driver files, including
duplicated by-driver and by-OS layouts. Windows 11 ARM64 entries include
storage (`vioscsi`, `viostor`), networking (`NetKVM`), display (`viogpudo`),
input, serial, vsock, RNG and balloon drivers. The RPM's `noarch` label alone
was not used as evidence. Guest installer architecture, Windows signature
trust and actual device operation still need acceptance.

The reusable native kernel check is:

```sh
bash os/kernel/tests/boot-smoke.sh STAGE.tar.gz KERNEL_RELEASE NEW_OUTPUT_DIRECTORY
```

It boots disklessly under QEMU TCG, records input hashes and serial output,
and requires both userspace startup and signed/unsigned/tampered module checks.
Run it on each native architecture; the ARM64 path has been exercised locally,
while its x86-64 path still needs execution on an x86-64 builder. The kernel
workflow loads each native image, extracts its RPM and runs `test-rpm.sh` before
signing. The exact RPM test passed locally on ARM64; the remote workflow has
not yet run. Acceptance logs and input hashes are uploaded for both lanes.

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
