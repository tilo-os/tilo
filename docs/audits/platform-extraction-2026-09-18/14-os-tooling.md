# A14 — OS, tooling, packaging and build configuration

## Scope and method

Scope: `os/`, `scripts/`, `tools/`, CI and Cargo configuration, release and distribution files, fuzz targets, and root build configuration. The audit covers architecture guards, native libraries, sockets, process groups, FFI, service managers, devices, privileged helpers, package tools, paths, platform commands, and toolchain options. Application-framework internals are outside its scope. The appendix provides navigation anchors; a lexical match is not itself a defect.

## Destination alignment

Read `docs/architecture/platform-services.md`: it is a **proposed** design, not evidence that those APIs exist. Implement reusable services in focused `crates/system` / `crates/runtime` crates and expose the app-facing facade through `ui::platform`; these headless Python/shell tools should consume non-UI contracts via a service CLI/client, never import the renderer. Native engine bootstrap/build ownership must move with its backend (A14-07), while explicit distro/session/image deployment exceptions remain in place (A14-05–06).

## Confirmed extraction work

**Current-tree note:** The `scripts/install-on-open-demo.py`,
`scripts/demo-assemble.py`, `scripts/demo-home.py`, `scripts/demo-record.sh`,
and `scripts/yc-demo*.py` entries in A14-02 and the appendix are historical
tooling anchors; those files are absent from the current tree. The same applies
to `tools/run-imager-ephemeral-runner.sh` and
`os/usr/lib/systemd/system/tilo-power.service` in the findings/appendix.
Preserve the process-lifetime finding and mark those specific examples
historical rather than presenting them as current runnable tools.

**Source-anchor note:** Verified prefix repairs identify current moved paths where
available. Any `:line` suffix remains the original 2026-09-18 audit coordinate; it
is historical evidence and does not assert that the cited symbol is at that line
in the current tree.

### A14-01 — Control clients encode Unix transport and filesystem endpoint discovery throughout automation

- **Classification/platform:** leaked transport assumptions in otherwise semantic automation; Unix/macOS/Linux implementations are valid backends. P2 migration work, not an assertion that Unix sockets fail on supported current hosts.
- **Evidence:** `scripts/tilo-mcp-server.py:38` defaults to `/tmp/tilo.sock`; `:60` requires endpoint filesystem existence; `:66` creates `AF_UNIX`. `scripts/headless-examples/_control.py:23` embeds the same transport below newline-JSON request/reply; `scripts/headless-examples/tilo_client.py:176` repeats it. Examples, benchmark drivers and demos duplicate it (appendix). `release/local-models/smolvlm2-500m/smoke-gateway.py:52` independently speaks to the model gateway through AF_UNIX.
- **Impact:** endpoint lifecycle, Unix pathname limits, and transport choice leak into MCP and semantic action clients. A new platform transport would require touching numerous scenario scripts rather than only the control boundary.
- **Suggested boundary:** platform-owned local endpoint allocation/connect/request transport, exported by the existing `tilo-control`/runtime control API, preserving semantic actions and JSON payloads. Reuse `_control.py` and `tilo_client.py` as migration entry points; do not create another action registry or parallel automation interface. Keep distinct protocols (gateway versus compositor) distinct above a shared transport primitive.
- **Migration:** first expose a supported control client/CLI operation with endpoint handles and explicit timeout/error semantics; replace copied socket code with that supported client; keep Unix path calculation inside its Unix backend. Preserve streaming clients separately from one-request-per-connection clients.
- **Acceptance:** identical action enumeration/invocation/cancellation and structured errors through both existing Unix and any new backend; tests for unavailable endpoints, short/long runtime roots, response EOF and cleanup; no scenario constructs `AF_UNIX` except transport-specific tests.

### A14-02 — Shared harness process-lifetime helper silently loses its Linux guarantee

- **Classification/platform:** confirmed Linux/glibc-specific implementation exposed as generic helper; test/tooling extraction, P1 because the helper promises cleanup of costly compositors.
- **Evidence:** `scripts/headless-examples/reaper.py:57` loads `libc.so.6`, `:58` calls `prctl(PR_SET_PDEATHSIG, SIGKILL)`, `:59` swallows all exceptions; the integer return code is not checked. `:63` assumes parent PID 1 detects the orphan race. `:80` normal cleanup kills the direct tracked child only. Numerous proof scripts call `die_with_parent()` (appendix). Independently `scripts/app-launch-sweep.py:412`, `scripts/install-on-open-demo.py:254`, `scripts/two-shell-app-collab.py:169`, `scripts/sandboxed-app-collab.py:193` use `setsid`; their shutdown paths use `killpg`.
- **Impact:** source explicitly allows continuing after lifetime protection fails. On a host without glibc/prctl, the promised parent-death guarantee is unavailable; even Linux syscall rejection is not surfaced. This is a source-level guarantee mismatch, not a reproduced orphan leak. Process-tree ownership semantics are also repeated in scripts.
- **Suggested boundary:** a platform harness `OwnedProcessTree` capability with launch, wait, terminate, parent-death behavior and an explicit supported guarantee. Linux prctl/process-group backend; native Windows Job Object implementation where appropriate; a separately designed Darwin supervisor. Do not claim a normal-exit handler meets abrupt-parent-death semantics.
- **Migration:** centralize lifecycle first, replace the unsafe success-on-unsupported fallback with an explicit refusal or declared lower guarantee, then migrate callers. `tools/run-imager-ephemeral-runner.sh:84` already distinguishes procfs-backed group membership from a non-procfs fallback; `os/usr/share/tilo/windows/guest/Tilo-ProcessJob.ps1:1` documents a concrete Windows job contract, but is guest-specific and should not simply be imported as a universal host implementation.
- **Acceptance:** kill the harness abruptly in isolated integration tests and verify no owned compositor descendants remain; assert failure when required lifetime guarantees are unavailable; verify normal completion, signal propagation, process-tree teardown and intentional `--keep` behavior. Do not weaken cleanup expectations to make unsupported hosts pass.

### A14-03 — Generic build/benchmark entry points carry host diagnostics and GNU tool assumptions

- **Classification/platform:** Linux-oriented developer/CI tooling; extract only reusable host probes, keep image prerequisites target-specific. P2.
- **Evidence:** `tools/ci/check-build-env.sh:12` unconditionally checks seat/input/udev/DRM/Wayland/PipeWire/FUSE/DBus; `:21` demands `bwrap` and fontconfig; `:55` verifies only the existence of `/usr/bin/time`. `tools/ci/measure.sh:16` likewise tests existence before invoking GNU `time -f ... -o` at `:17`. `tools/ci/cargo-shim:43` uses `nproc`, `:44` `/proc/meminfo`; the script is explicitly for container builds and has fallback values. `scripts/perf/runtime_benchmark.py:123` implements Darwin hardware inventory with `sysctl`/`system_profiler`; `scripts/headless-examples/install-open-timing.py:549` obtains filesystem type from `/proc/mounts`, returning `unknown` on read failure.
- **Impact:** a generic preflight cannot distinguish full Linux image dependencies from native host tools; existence does not prove GNU-time compatibility. Benchmark metadata differs by host and repeated host probing makes comparisons and future ports harder. The measured command may not run if a non-GNU executable occupies `/usr/bin/time`; not reproduced during this audit.
- **Suggested boundary:** tooling-level target capability manifest and host diagnostics adapter (`build prerequisites`, `resource measurement`, `CPU/memory/display metadata`, `filesystem kind`). These need not be in the runtime platform library; a platform-owned CLI can export them. Keep container routing and Fedora image dependency lists in deployment tooling.
- **Migration:** select preflight by intended build target/profile; probe actual time implementation or use a backend that produces the existing normalized metrics; move host diagnostic logic into one module with explicit missing/unavailable fields and units.
- **Acceptance:** preflight fixtures for Linux-image, Darwin-native and Windows-native profiles; GNU/BSD-time selection tests ensure wrapped command executes exactly once and exit status is preserved; consistent metadata schemas and meaningful unavailable reasons, with no invented measurements.

### A14-04 — Fuzz harness manifest ties independent wire fuzzing to Linux sandbox dependencies

- **Classification/platform:** test-only build dependency coupling; P2 migration candidate with confirmed manifest evidence, no cross-platform compile failure asserted.
- **Evidence:** `fuzz/Cargo.toml:12` declares `landlock` and `:18` `launcher` unconditionally for all four fuzz binaries. `fuzz/fuzz_targets/sandbox_policy.rs:3` imports `launcher::landlock_shim`; `:15` enumerates Linux Landlock ABIs V1–V5. The other targets exercise qpkg ingress, identity wire and invitation chains.
- **Impact:** the fuzz package's dependency surface mixes platform-independent inputs with the Linux sandbox compiler; platform extraction should not force all protocol fuzzing through a Linux backend dependency graph.
- **Suggested boundary/migration:** preserve sandbox-policy fuzzing as a Linux backend target. Split its manifest, features, required features, or dedicated package from wire-format fuzz targets after inspecting the actual launcher exports. Keep the ABI iteration and production access-plan calls intact.
- **Acceptance:** independent wire fuzz targets resolve/build without selecting the Landlock backend on supported non-Linux hosts; Linux sandbox fuzz target continues exercising actual production policy compilation for every listed ABI. Missing native backend remains an explicit unsupported target, not silently skipped verification.

## Intentional platform backends and deployment — retain behind their boundaries

### A14-05 — Privileged Linux service, hardware, session and appliance helpers are existing backend entry points

- **Classification/platform:** deliberate Linux/Fedora backends; retain implementations, hide invocation details behind semantic platform capabilities where cross-platform callers need them.
- **Evidence:** `os/usr/libexec/tilo-power:21` escalates through sudo and `:37` invokes systemctl reboot/poweroff; `os/usr/libexec/tilo-battery-cap:12` enumerates sysfs battery controls with separate IdeaPad/Huawei paths at `:57`/`:68`; `os/usr/libexec/tilo-session:8` pins systemctl, `:26` validates XDG runtime paths, `:33` exports a private logout channel, `:86` starts the user session target. `dist/session/tilo-session-runner:24` and `:26` bind systemd/DBus, with environment handoff at `:179`. `os/usr/libexec/tilo-windows-owner:13` fixes appliance state paths, `:48` starts isolated networking and `:90` validates inherited portal stdin through procfs.
- **Related backend families:** every `os/usr/libexec/tilo-*` helper, systemd system/user units and generators, PAM/polkit rules, sysusers/tmpfiles, udev/sysctl, portal configuration, dracut hooks, bootc/UKI/LUKS/TPM/Secure Boot, installer/repair/recovery, libvirt and Windows guest PowerShell. The appendix enumerates matched files; unmatched small unit/drop-in files remain part of these deployment families.
- **Impact/boundary:** these files are not evidence of portability bugs merely because they use Linux commands. They are the appropriate backend layer. Define typed operations for power, charge-cap capability/set, session start/logout, deployment/rollback, storage install/recovery and VM appliance lifecycle; consumers should not know helper paths, command-line flags or service unit names.
- **Migration:** register or relocate backend ownership, leave privileged deployment artifacts installed at required paths, and update Rust/app callers through the relevant parent-audit findings. Preserve narrow portal-descriptor APIs, user identity, polkit authorization, private runtime ownership, namespace/network restrictions, durable trust and fail-closed behavior. Do not replace them with a generic privileged `run_command` escape hatch.
- **Acceptance:** existing OS/session invariant suites continue testing real production helpers; cross-platform frontends can report unsupported capabilities without invoking Linux tools; Linux integration verifies actual power/battery/session/VM semantics in isolated images. Battery unsupported is distinguishable from success; logout still stops the owned session target.

### A14-06 — Fedora images, native packages and toolchain flags are intentionally target-specific

- **Classification:** deployment/build backend inventory; retain, no blanket extraction.
- **Evidence:** `flake.nix:8` supports x86_64/aarch64 Linux; `dist/nix/package.nix:62` declares native Linux dependencies, `:73` embeds immutable trusted helper paths. `.cargo/config.toml:8`/`:20` scope ELF hardening to Linux, `:31`/`:38` separate non-Linux Rust options, and `:52` onward scopes C/C++ flags per target; these already avoid global Linux linker leakage. `Cargo.toml:377`/`:380` provide Wayland library/clipboard workspace settings; workspace dependencies alone do not force all members to compile a library. `scripts/browser/build-host.py:20` maps Linux/Darwin architectures; `scripts/browser/package.py:31`/`:65` selects app-bundle/code-signing versus Linux layouts. `.forgejo/workflows/release-imager.yml:437`/`:572` has separate Windows/macOS jobs. `tools/run-imager-ephemeral-runner.sh:91` guards procfs and `:106` has an alternate probe.
- **Related files:** all image Containerfiles, kernel configs/patches, `os/dev/`, `os/scripts/`, `os/hardware/`, `dist/{rpm,debian,arch,nix,ci,desktop,apps}/`, release models, workflow YAML and tests. They intentionally describe image layout, architecture, signing and publication. Vendor FEX patches are not candidates for mass refactoring.
- **Migration/boundary:** retain target-owned packaging/toolchain profiles. Centralize duplicated platform identifiers and capability selections only if platform extraction introduces a supported build-metadata contract; keep host-versus-target distinctions (e.g. bindgen host libclang) explicit. No evidence here requires changing x86-64-v3 policy or adding Darwin to a NixOS session flake.
- **Acceptance:** platform build matrix still applies ELF hardening only to ELF targets, native signing/bundle validation remains native, Nix immutable helper paths still work, Fedora image service/security contracts remain intact. Do not broaden this audit into unrelated release-pipeline rewrites.

### A14-07 — Native browser engine build wiring is application-owned and must follow backend extraction

- **Classification/platform:** framework ownership extraction for Linux/Darwin native engine tooling; separate from intentional final application packaging.
- **Evidence:** `scripts/browser/build-host.py:20` chooses native platform SDK identifiers, `:122` selects the host and `:133` builds the native blocker. `:136` configures CMake with source `apps/browser/chromium`, a direct application-owned backend source dependency. `scripts/browser/package.py:31` lays out the Darwin engine bundle, `:61` signs the runtime host, `:65` begins Linux engine layout. `scripts/browser/cef-lock.json` owns pinned platform SDK artifact metadata.
- **Impact:** moving only Rust imports into `ui::platform` would leave engine compilation, SDK selection, sandbox/native binary closure and bundle requirements owned by the Browser application; this would not meet the supplied design's backend ownership requirement.
- **Boundary/migration:** framework webview backend owns CEF SDK acquisition/lock, native engine CMake source, blocker native-link closure and required engine bundle metadata. Keep Browser product name/identifier and final app packaging workflow in a thin deployment layer that consumes the backend artifact manifest. Coordinate the actual CMake/native source move with the browser/framework agents rather than changing it from this tooling-only audit.
- **Acceptance:** engine backend builds without depending on application source paths; Browser packaging consumes declared native artifacts, preserves signing and helper layout, and verifies sandbox launch/native rendering on Linux and Darwin. Existing `scripts/browser/test_build_host.py` and `scripts/browser/test_package.py` remain meaningful after relocation. No native execution was done in this audit.

## Coverage cautions and adjacent ownership

This scope is dominated by intentional OS deployment. Largest practical extraction value is the automation transport/lifetime boundary, not moving Containerfiles into Rust. References to `ui` in tooling were observed only as build/test inputs; framework internals were not audited. App callers of libexec helpers, launcher Landlock exports, platform IPC backend implementations, and Rust system-service ownership belong to adjacent agents. `.config/nextest.toml` is test scheduling policy (including GPU isolation), not itself a native backend. JSON fixtures, checksums, release collection data and workflow filtering scripts are generally platform-neutral data/logic even when their strings mention Linux. No actual build or runtime outcome was tested.

## Per-file lexical candidate appendix

The following table is a candidate inventory, **not 588 confirmed defects**. OS/dist/workflow entries default to intentional deployment; tests/docs/fixtures default to evidence/support. A14-01–04 identify confirmed reusable seams; A14-05–06 record intentional backend ownership. Anchors are exact matching lines (first six per file); the count after `+` is additional lexical matching lines in that file.

| File | Matching line anchors |
|---|---|
| `.cargo/config.toml` | 8, 20, 31, 38, 60, 63 (+3) |
| `.forgejo/workflows/build-builder-image.yml` | 15, 16, 17, 18, 50, 61 (+17) |
| `.forgejo/workflows/build-image.yml` | 3, 139, 151, 172, 184, 398 (+76) |
| `.forgejo/workflows/build-kernel.yml` | 20, 21, 34, 35, 63, 75 (+10) |
| `.forgejo/workflows/build-wine.yml` | 10, 11, 12, 13, 15, 16 (+3) |
| `.forgejo/workflows/check.yml` | 138, 139, 213 |
| `.forgejo/workflows/development-compile.yml` | 164, 182, 208, 310, 313, 328 (+5) |
| `.forgejo/workflows/development-disk-environment.yml` | 29, 30, 49 |
| `.forgejo/workflows/development-finalize.yml` | 43, 74 |
| `.forgejo/workflows/development-inspect-guest-loaders.yml` | 25 |
| `.forgejo/workflows/development-kernel-resume.yml` | 12, 64, 92, 123 |
| `.forgejo/workflows/development-kernel.yml` | 12, 41, 59 |
| `.forgejo/workflows/development-native-resume.yml` | 99, 124 |
| `.forgejo/workflows/development-source.yml` | 14, 15, 16, 19, 20, 21 (+22) |
| `.forgejo/workflows/development-windows-guest.yml` | 50, 51, 61 |
| `.forgejo/workflows/development-wine-pair.yml` | 36, 69, 71 |
| `.forgejo/workflows/development-wpe-stage.yml` | 44, 48, 55, 65, 71, 78 (+5) |
| `.forgejo/workflows/fs-fuse.yml` | 21, 22, 23, 24, 25, 27 (+15) |
| `.forgejo/workflows/legacy-app-e2e.yml` | 95 |
| `.forgejo/workflows/mirror-base.yml` | 3, 8, 10, 30, 31, 67 (+2) |
| `.forgejo/workflows/package-desktop.yml` | 83, 84, 126, 180 |
| `.forgejo/workflows/release-apps.yml` | 192 |
| `.forgejo/workflows/release-imager.yml` | 56, 77, 89, 97, 105, 115 (+27) |
| `.forgejo/workflows/snapshot-rpms.yml` | 17, 19, 20, 22, 23, 24 (+27) |
| `.forgejo/workflows/snapshot-wpe-buildroot.yml` | 14, 15, 16, 17, 48, 64 (+8) |
| `.forgejo/workflows/tilo-relay.yaml` | 19, 42, 43, 54, 77, 78 (+6) |
| `.forgejo/workflows/wpe-candidate.yml` | 31, 37, 83 |
| `Cargo.toml` | 110, 111, 112, 113, 114, 115 (+6) |
| `dist/arch/PKGBUILD` | 6, 20, 21, 39, 40, 41 (+10) |
| `dist/arch/test-package.sh` | 10, 20, 23 |
| `dist/ci/install-build-deps.sh` | 14, 15, 21, 24, 25, 26 (+6) |
| `dist/ci/qemu-argv0-wrapper.c` | 2, 3, 13, 14, 15, 24 (+2) |
| `dist/ci/run-dev-session-test.sh` | 3 |
| `dist/ci/run-package-job.sh` | 4, 178, 179, 180, 195, 196 (+15) |
| `dist/ci/run-payload-contract-tests.sh` | 33, 71, 73, 77, 78, 79 (+15) |
| `dist/ci/verify-nix-result.sh` | 14, 17, 22, 33, 34, 38 |
| `dist/debian/build-deb.sh` | 26, 29, 31, 34, 38, 39 (+5) |
| `dist/debian/test-package.sh` | 16, 18 |
| `dist/dev/Containerfile.fedora44` | 3, 10, 13, 14 |
| `dist/dev/fedora44-vm-iterate.sh` | 7, 10, 11, 12, 13, 14 (+33) |
| `dist/dev/tests/fedora44-vm-iterate-test.sh` | 8, 14, 18, 21, 79, 101 (+2) |
| `dist/nix/package.nix` | 9, 10, 14, 16, 17, 18 (+20) |
| `dist/rpm/build-rpm.sh` | 2, 75, 96, 105, 106, 107 (+12) |
| `dist/rpm/test-package.sh` | 16, 24, 27, 28, 48 |
| `dist/rpm/test-rename-contract.sh` | 55, 56, 59 |
| `dist/rpm/tilo-session.rpmlintrc` | 1 |
| `dist/rpm/tilo-session.spec` | 1, 5, 7, 10, 11, 20 (+29) |
| `dist/rpm/usroverlay-install.sh` | 3, 5, 27, 28, 29, 30 (+5) |
| `dist/session/README.md` | 26, 32, 39 |
| `dist/session/tests/host-session-lifecycle.sh` | 12, 14, 30, 32, 52, 54 (+46) |
| `dist/session/tests/private-dbus-handoff.sh` | 3, 9, 24, 26, 35, 36 (+21) |
| `dist/session/tests/private-user-manager-handoff.sh` | 2, 8, 9, 15, 24, 51 (+42) |
| `dist/session/tilo-dev.desktop` | 4, 11, 12 |
| `dist/session/tilo-session` | 14 |
| `dist/session/tilo-session-dev` | 130, 136, 142 |
| `dist/session/tilo-session-runner` | 24, 25, 26, 37, 45, 46 (+16) |
| `dist/session/tilo.desktop` | 2, 4, 5, 15 |
| `dist/stage-payload.sh` | 26, 33, 34, 36, 37 |
| `dist/verify-devel-payload.sh` | 9, 16 |
| `dist/verify-payload.sh` | 9, 21, 24, 33, 34, 35 (+7) |
| `flake.nix` | 8, 47, 58, 59 |
| `fuzz/Cargo.toml` | 12 |
| `fuzz/fuzz_targets/sandbox_policy.rs` | 14, 16, 17, 18, 19, 20 |
| `os/.gitignore` | 10, 23, 24 |
| `os/CHANGELOG.md` | 10, 15, 17, 19, 23, 30 (+18) |
| `os/Containerfile` | 3, 9, 10, 17, 20, 25 (+474) |
| `os/Containerfile.builder` | 2, 16, 17, 22, 27, 28 (+5) |
| `os/Containerfile.development-compile` | 4, 5, 7, 8, 9, 12 |
| `os/Containerfile.development-loader` | 7, 8, 11, 14, 16, 17 (+1) |
| `os/Containerfile.fex-buildroot` | 6, 8, 9, 12, 18, 20 (+3) |
| `os/Containerfile.fex-payload` | 14, 16, 17, 18, 19 |
| `os/Containerfile.host` | 3, 5, 10, 18, 20, 30 (+46) |
| `os/Containerfile.installer` | 16, 29, 31, 32, 41, 44 (+24) |
| `os/Containerfile.libvirt` | 10, 22, 23, 27 |
| `os/Containerfile.rpms` | 32, 36, 37, 45, 59, 71 (+5) |
| `os/Containerfile.test` | 14, 15, 16, 17, 21, 24 (+1) |
| `os/Containerfile.windows-guest` | 16, 18, 19 |
| `os/Containerfile.windows-guest-rpms` | 12, 13 |
| `os/Containerfile.wine-rpms` | 9, 10 |
| `os/Containerfile.wine-tools` | 6, 7, 13, 28, 30, 31 (+1) |
| `os/Containerfile.wpe-buildroot` | 10, 11, 12, 14, 15, 21 (+1) |
| `os/account.env` | 4, 5, 14, 15 |
| `os/base-snapshot.env` | 1, 5, 19, 20, 24 |
| `os/build-host.sh` | 2, 18, 27, 36, 52 |
| `os/build.sh` | 8, 11, 21 |
| `os/containers/tilo-relay/Containerfile` | 5, 6, 10, 13, 17, 25 (+9) |
| `os/containers/tilo-relay/README.md` | 22, 32, 33, 53 |
| `os/containers/tilo-relay/compose.yaml` | 3, 22, 25, 26, 30 |
| `os/containers/tilo-relay/relay.toml.example` | 6, 7 |
| `os/containers/tilo-relay/test-contract.sh` | 25, 26, 28, 38, 39, 61 |
| `os/containers/tilo-relay/tilo-home-peer` | 4, 9, 10, 12, 16, 20 (+3) |
| `os/dev/FINALIZATION-OPERATOR.md` | 10, 37, 42, 85 |
| `os/dev/INSTALLED-DISK-VERIFICATION.md` | 9, 11, 14, 19, 29, 30 (+8) |
| `os/dev/README.md` | 1, 3, 6, 9, 11, 12 (+66) |
| `os/dev/REMOTE-SIGNING.md` | 10, 30, 31, 68, 70, 90 (+6) |
| `os/dev/SUPERVISED-DISK.md` | 3, 4, 11, 12, 17, 22 (+1) |
| `os/dev/_common.sh` | 2, 7, 8, 9, 11, 21 (+40) |
| `os/dev/adopt-development-wpe.py` | 63, 98, 141, 161 |
| `os/dev/approve-native-reconstruction.py` | 78, 80, 97, 102, 103, 208 |
| `os/dev/audit-rootfs-layers.py` | 287, 293 |
| `os/dev/authorize-signing-input.py` | 27, 59 |
| `os/dev/broker_signing_runner.py` | 47, 137 |
| `os/dev/build-custom-installer.sh` | 6, 7, 8, 11, 15, 17 (+19) |
| `os/dev/build-fex-payload.sh` | 28, 29, 46, 47, 72 |
| `os/dev/build-image.sh` | 2, 4, 7, 13, 14, 15 (+52) |
| `os/dev/build-windows-guest.sh` | 43 |
| `os/dev/build-wpe-buildroot.sh` | 16 |
| `os/dev/build-wpe.sh` | 9, 45, 47, 48, 50, 51 (+1) |
| `os/dev/cache-github-assets.sh` | 92, 94, 98 |
| `os/dev/compile-development-os.py` | 38, 44, 48, 129, 138, 243 (+27) |
| `os/dev/complete-initramfs-rebind.py` | 6, 118 |
| `os/dev/composefs/04-mode.toml` | 6, 13, 14 |
| `os/dev/composefs/Containerfile` | 5, 6, 8, 11, 13, 33 (+51) |
| `os/dev/composefs/build.sh` | 4, 7, 28, 32, 38, 39 (+18) |
| `os/dev/composefs/common.sh` | 2, 4, 5, 14, 28 |
| `os/dev/composefs/guest-check.py` | 4, 12, 83, 84, 128, 156 (+16) |
| `os/dev/composefs/install.sh` | 3, 5, 11, 12, 14, 22 (+27) |
| `os/dev/composefs/recovery-series.sh` | 4, 16, 21, 22, 28, 35 (+16) |
| `os/dev/composefs/repart.d/01-esp.conf` | 4 |
| `os/dev/composefs/seal.sh` | 2, 4, 6, 7, 36, 41 |
| `os/dev/composefs/uki.sh` | 5, 7, 12, 18, 46, 63 (+3) |
| `os/dev/composefs/update-series.sh` | 4, 6, 31, 33, 35, 36 (+13) |
| `os/dev/composefs/vm.sh` | 4, 6, 7, 8, 9, 10 (+12) |
| `os/dev/compute-development-composefs.py` | 2, 4, 19, 20, 25, 50 (+7) |
| `os/dev/create-development-disk.py` | 5, 10, 51, 52, 53, 74 (+17) |
| `os/dev/create-supervised-kata-disk.py` | 32, 33, 45, 46, 65, 71 (+35) |
| `os/dev/derive-rpm-package-set.py` | 17, 51, 58 |
| `os/dev/development-signer.py` | 106, 108, 111 |
| `os/dev/development_prediction.py` | 4, 5, 6, 7, 19, 102 (+7) |
| `os/dev/disk-image.sh` | 2, 3, 5, 8, 21, 25 (+70) |
| `os/dev/fetch-wine-source.py` | 9, 33, 35, 36, 38, 74 |
| `os/dev/finalization-worker.md` | 5, 28, 42 |
| `os/dev/finalize-development-rootfs.py` | 8, 97, 124, 158, 183, 185 (+8) |
| `os/dev/hardware-firmware-artifact.py` | 27, 30, 104, 130 |
| `os/dev/import-development-rootfs.py` | 4, 71, 89, 111, 112 |
| `os/dev/initramfs-trust.py` | 73 |
| `os/dev/inspect-disk-environment.py` | 45, 56, 71, 78, 79, 82 (+5) |
| `os/dev/installer_overlay.py` | 26, 27, 34 |
| `os/dev/installer_target_artifacts.py` | 4, 7, 32, 47 |
| `os/dev/kernel_checkpoint_restore.py` | 177, 293 |
| `os/dev/kernel_failure_checkpoint.py` | 23, 93, 105, 107, 108, 115 |
| `os/dev/launch-finalization-worker.py` | 28, 30, 158, 169, 170, 171 (+5) |
| `os/dev/launch-native-resume.py` | 54, 131, 151, 209, 210, 246 (+2) |
| `os/dev/native-reconstruction-approval.md` | 9 |
| `os/dev/native-source-compatibility.md` | 6, 32, 96 |
| `os/dev/native_builder_artifact.py` | 154, 183, 191 |
| `os/dev/native_resume.py` | 25, 52, 53, 54, 57, 58 (+6) |
| `os/dev/native_source_compatibility.py` | 10, 11, 112, 157 |
| `os/dev/next-generation.sh` | 4, 5, 35, 100, 149, 150 (+3) |
| `os/dev/prepare-wpe-stage.py` | 42, 101, 104, 105, 123 |
| `os/dev/private_disk_image.py` | 66 |
| `os/dev/produce-wpe-stage.py` | 49, 56, 67, 73, 82, 167 (+4) |
| `os/dev/promote-native-index.py` | 106, 109, 112, 126 |
| `os/dev/rebind-development-rootfs.py` | 35, 66, 77, 82, 86, 101 (+3) |
| `os/dev/rebind-development-trust.py` | 61 |
| `os/dev/reconstruction_cache_approval.py` | 34 |
| `os/dev/record-boot.py` | 10, 11, 13, 65, 69, 73 (+3) |
| `os/dev/release-provenance.sh` | 88, 112 |
| `os/dev/reseal-image.sh` | 35, 50, 55, 62, 68, 71 (+3) |
| `os/dev/resolve-windows-guest-rpms.sh` | 34, 39, 44 |
| `os/dev/resume-native-payload.py` | 63, 70, 74, 116, 117, 163 (+4) |
| `os/dev/retain-development-rootfs.py` | 114, 124, 136, 146, 153 |
| `os/dev/run-development-windows-guest.py` | 139 |
| `os/dev/run-wine-ci.sh` | 66, 86, 87 |
| `os/dev/secureboot-evidence.py` | 11, 12 |
| `os/dev/serial-console.py` | 2, 11, 14, 32, 48 |
| `os/dev/sign-development-rootfs.py` | 36, 75, 76, 102, 105, 108 (+4) |
| `os/dev/sign-target-prediction.sh` | 3, 28, 29, 42, 44, 82 (+9) |
| `os/dev/signing_spool.py` | 7, 96, 98, 99, 101 |
| `os/dev/stage-compile-tools-recipe.py` | 11 |
| `os/dev/stage-finalization-worker.py` | 9, 33, 103, 177, 179 |
| `os/dev/switch-to-stock-kernel.sh` | 8, 9, 11, 15, 29, 43 (+5) |
| `os/dev/test-encrypted-install.sh` | 4, 17, 114, 116, 120, 125 (+28) |
| `os/dev/test-installer-cycle.sh` | 4, 105, 143, 144, 157, 206 (+13) |
| `os/dev/test-measured-boot.sh` | 4, 7, 32, 75, 78, 89 (+3) |
| `os/dev/test-secureboot.sh` | 5, 7, 70, 71, 84, 85 (+11) |
| `os/dev/thin_cache_stage.py` | 43 |
| `os/dev/toolchains/fex-cross-x86_64.cmake` | 2, 3 |
| `os/dev/transfer-signing-spool.py` | 54 |
| `os/dev/uki-cmdline.py` | 15, 27, 28, 36, 54, 55 (+9) |
| `os/dev/uki.sh` | 8, 10, 17, 20, 23, 24 (+8) |
| `os/dev/use-published-image.sh` | 19, 25, 65, 127, 129, 130 |
| `os/dev/validate-fex-payload.py` | 17 |
| `os/dev/verify-development-source.py` | 4, 112, 122, 130, 132, 140 (+5) |
| `os/dev/verify-finalized-development-image.py` | 47 |
| `os/dev/verify-installed-development-disk.py` | 2, 9, 72, 162, 168, 175 (+16) |
| `os/dev/verify-rpm-snapshot-package-set.sh` | 8, 27 |
| `os/dev/vm.sh` | 3, 4, 6, 8, 9, 10 (+52) |
| `os/dev/windows_guest_contract.py` | 42 |
| `os/dev/wine-mock-producer.py` | 5, 128, 190, 199, 335, 337 |
| `os/dev/wine_rpm_contract.py` | 254, 273 |
| `os/dev/wpe-candidate-receipt.py` | 21 |
| `os/dev/wpe-stage-artifact.py` | 23, 156, 164, 179, 181, 184 (+4) |
| `os/docs/alongside-installation-acceptance.md` | 3, 6, 12, 19, 23, 25 (+4) |
| `os/docs/compat-sandbox-validation.md` | 5, 6, 7, 9, 13, 23 (+23) |
| `os/docs/fex-build-validation.md` | 17, 49, 69, 159, 179, 242 (+15) |
| `os/docs/kernel-capsule-validation.md` | 5, 10, 16, 23, 28, 36 (+28) |
| `os/docs/libvirt-package-build.md` | 20 |
| `os/docs/native8166-partial-admission.md` | 76, 83 |
| `os/docs/peer-sync.md` | 38, 39, 40, 51, 52, 53 (+3) |
| `os/docs/proton-runtime-validation.md` | 35, 88, 103, 160, 164, 174 (+4) |
| `os/docs/provisional-development-compilation.md` | 6, 80, 107, 126 |
| `os/docs/rpm-snapshot-packaging.md` | 4, 30, 38, 39, 43, 49 |
| `os/docs/supervised-kata-disk.md` | 8, 17, 21, 24 |
| `os/docs/windows-dxvk-provisioning.md` | 10, 119, 139, 140 |
| `os/docs/windows-guest-packaging.md` | 11, 17, 21, 47, 62, 64 (+7) |
| `os/docs/windows-provider-packaging.md` | 12, 46 |
| `os/docs/wine-rpm-packaging.md` | 52, 120, 121, 168, 170, 195 |
| `os/docs/wpe-native-validation.md` | 24, 43, 49 |
| `os/docs/wpe-stage-cache.md` | 12, 35 |
| `os/etc/chrony.conf` | 13, 14 |
| `os/etc/containers/registries.d/code.podesta.ai.yaml` | 3 |
| `os/etc/cups/cupsd.conf` | 29 |
| `os/etc/greetd/config.installer.toml` | 4, 11, 20 |
| `os/etc/greetd/config.toml` | 1, 6, 10, 13, 20 |
| `os/etc/pam.d/greetd` | 22 |
| `os/etc/pam.d/tilo-unlock` | 11 |
| `os/etc/polkit-1/rules.d/49-tilo-battery-cap.rules` | 4, 7, 13, 15 |
| `os/etc/polkit-1/rules.d/49-tilo-greeter-fingerprint.rules` | 14, 17, 19 |
| `os/etc/polkit-1/rules.d/49-tilo-greeter-power.rules` | 5, 6, 8, 22, 24, 31 (+1) |
| `os/etc/polkit-1/rules.d/49-tilo-home-peer.rules` | 3, 5, 7, 9 |
| `os/etc/polkit-1/rules.d/49-tilo-updates.rules` | 22, 26, 37 |
| `os/etc/polkit-1/rules.d/49-tilo-windows.rules` | 4, 5, 19, 23, 25 |
| `os/etc/systemd/zram-generator.conf` | 3, 4 |
| `os/etc/tilo/user-identity.example.toml` | 13, 15, 23 |
| `os/external-assets/fex-patches/0002-reject-arm-vector-prctls-for-x86.patch` | 34, 38, 44, 59, 61, 67 (+3) |
| `os/external-assets/fex-patches/README.md` | 13, 18 |
| `os/external-assets/fex-patches/series.json` | 10 |
| `os/hardware/README.md` | 97, 99, 152 |
| `os/kernel/Containerfile` | 20, 28, 34, 68, 75, 84 (+12) |
| `os/kernel/build-support.sh` | 68 |
| `os/kernel/config-fragments/tilo.config` | 17, 19, 49, 60, 61, 91 (+15) |
| `os/kernel/config-fragments/x86_64.config` | 25 |
| `os/kernel/tests/boot-init.c` | 2 |
| `os/kernel/tests/boot-smoke.sh` | 10, 11, 64 |
| `os/kernel/tilo-kernel-payload.spec` | 17, 22 |
| `os/scripts/bake-runtime-bundle.sh` | 32, 33, 42, 133 |
| `os/scripts/finalize-rpmdb.sh` | 4, 7 |
| `os/scripts/verify-runtime-crypto-packages.sh` | 14, 20, 68, 70, 73, 87 (+9) |
| `os/test-vm-headless.sh` | 22, 23 |
| `os/test-vm.sh` | 22 |
| `os/tests/Containerfile.development-source` | 2, 3, 6, 25 |
| `os/tests/airplay-encrypted-session.sh` | 29, 30 |
| `os/tests/app_store.sh` | 245, 255, 279, 310, 312, 315 (+1) |
| `os/tests/authenticode_envelope.sh` | 5 |
| `os/tests/boot_chain_invariants.sh` | 4, 5, 24, 41, 42, 99 (+18) |
| `os/tests/boot_pack_invariants.sh` | 7, 8, 9, 17, 19, 20 (+5) |
| `os/tests/boot_performance_gate.sh` | 16, 20, 24, 30, 41, 45 (+6) |
| `os/tests/boot_performance_gate_test.sh` | 16, 21, 37, 44, 48, 92 (+78) |
| `os/tests/boot_ux_invariants.sh` | 5, 6, 16, 21, 41, 51 (+1) |
| `os/tests/booted_app_store.sh` | 130, 131, 133, 135, 139, 143 (+3) |
| `os/tests/collaborative-files-image-contract.sh` | 9, 64, 65, 66, 67, 71 (+77) |
| `os/tests/container_build_context.py` | 2, 21, 22, 37, 49, 52 (+2) |
| `os/tests/containerfile_copy_invariants.sh` | 26, 29, 99, 100, 101, 102 |
| `os/tests/containerfile_sources.sh` | 16, 31 |
| `os/tests/development_layer_audit_native.py` | 2, 25, 28, 41, 68, 69 (+6) |
| `os/tests/development_prediction_native.py` | 2, 45, 50, 145, 149 |
| `os/tests/development_retention_native.py` | 45, 83, 87, 101, 135, 136 (+1) |
| `os/tests/development_rootfs.py` | 35, 36, 45, 77, 82, 106 (+15) |
| `os/tests/development_signer.py` | 5, 31, 80, 85, 88, 103 (+4) |
| `os/tests/development_source_registry.py` | 2, 4, 37, 59, 97, 99 (+3) |
| `os/tests/development_stage_reuse.py` | 2, 5, 18, 19, 23, 38 (+7) |
| `os/tests/development_wpe_artifact_native.py` | 28, 29, 41, 45, 50, 67 (+6) |
| `os/tests/esp_capacity.sh` | 12, 27, 89 |
| `os/tests/fex-cross-clang` | 9 |
| `os/tests/fex-proton-smoke.py` | 31, 76, 77, 78, 86, 107 (+4) |
| `os/tests/fex-wine-smoke.py` | 29 |
| `os/tests/first_boot_service_readiness.sh` | 6, 7, 8, 9, 10, 11 (+26) |
| `os/tests/fixtures/arm-capsule-vm-init.c` | 4, 8, 31, 46, 64, 66 (+4) |
| `os/tests/fixtures/arm-capsule-vm-run.sh` | 5, 9, 10, 13, 14, 15 (+10) |
| `os/tests/fixtures/fex-cpu-list.c` | 2, 44 |
| `os/tests/fixtures/kata-disk-environment.json` | 20, 36, 73, 167, 233, 247 (+66) |
| `os/tests/fixtures/landlock-private-tmp.c` | 2, 5, 52, 53, 54 |
| `os/tests/fixtures/rpm-default-editor-empty.xml` | 43, 46 |
| `os/tests/fixtures/windows-dxvk-load.c` | 2, 10 |
| `os/tests/generation_series_security.sh` | 47 |
| `os/tests/greeter_auth_security.sh` | 6 |
| `os/tests/home_peer_setup.sh` | 6, 7, 8, 9, 10, 11 (+15) |
| `os/tests/image_build_profile.py` | 28, 32, 33, 34, 35, 37 (+30) |
| `os/tests/image_trust_update_gate.sh` | 5, 6, 7, 8, 9, 18 (+47) |
| `os/tests/imager_broker_credentials.sh` | 20 |
| `os/tests/imager_ephemeral_runner.sh` | 24, 25, 29 |
| `os/tests/imager_media_profiles.py` | 42, 44, 106, 115 |
| `os/tests/imager_release_invariants.sh` | 25, 26, 273, 277, 282, 322 (+19) |
| `os/tests/installed_atspi_apps.sh` | 4, 7, 14, 17, 21, 33 (+2) |
| `os/tests/installer_boot_menu.sh` | 6, 7, 8, 29, 40, 100 (+1) |
| `os/tests/installer_guided_alongside.sh` | 8 |
| `os/tests/installer_profile_transport.sh` | 6, 50, 75, 80 |
| `os/tests/installer_target_invariants.sh` | 8, 9, 10, 13, 14, 15 (+36) |
| `os/tests/installer_target_modes.py` | 25, 37, 38, 44 |
| `os/tests/kernel-arm-capsule-smoke.py` | 6, 38, 40, 44, 45, 47 (+15) |
| `os/tests/kernel-landlock-private-tmp.sh` | 13, 14, 17, 43 |
| `os/tests/kernel_hardening_invariants.sh` | 8, 9, 13, 15, 119, 266 (+93) |
| `os/tests/kernel_runtime_probe.c` | 4, 51, 52 |
| `os/tests/kernel_runtime_probe.sh` | 28, 38 |
| `os/tests/live_installer_service_invariants.sh` | 7, 15, 16, 17, 18, 19 (+5) |
| `os/tests/live_recovery.sh` | 6, 7, 8, 9, 10, 21 (+13) |
| `os/tests/luks_enrolment.sh` | 5, 6, 8, 11, 21, 32 (+13) |
| `os/tests/managed-fex-dxvk-load.py` | 7, 82, 84, 170 |
| `os/tests/managed-fex-proton-smoke.py` | 5, 6, 7, 9, 56 |
| `os/tests/managed-fex-wine-smoke.py` | 8, 32 |
| `os/tests/named_public_trust_context.sh` | 3, 6, 8 |
| `os/tests/native_index_promotion.py` | 48, 76 |
| `os/tests/no_openssl_runtime.sh` | 33, 44, 45, 46, 47, 83 (+31) |
| `os/tests/os_actions_subprocess_invariants.sh` | 33, 36, 39 |
| `os/tests/portal_config_validity.sh` | 27, 36, 37, 157, 158, 159 |
| `os/tests/printing_isolation.sh` | 18, 19, 106, 110, 124, 135 (+5) |
| `os/tests/projection-boot-check.sh` | 5, 16 |
| `os/tests/prove_kernel_failure_cache.py` | 12, 15, 20 |
| `os/tests/prove_kernel_failure_cache_local.py` | 2, 10, 14, 21, 26, 32 (+4) |
| `os/tests/recovery-boot-check.sh` | 4, 30, 32, 33, 34, 48 (+40) |
| `os/tests/release_freshness_security.sh` | 12, 15, 29, 34 |
| `os/tests/release_pipeline_invariants.sh` | 25, 76, 91, 92, 93, 94 (+1) |
| `os/tests/release_security_invariants.sh` | 9, 10, 46, 47, 48, 94 (+3) |
| `os/tests/repair_mode.sh` | 28, 29, 30, 31, 32, 33 (+53) |
| `os/tests/rollback-boot-check.sh` | 4, 5, 10, 26, 47, 54 (+39) |
| `os/tests/rollback-initrd-hook.sh` | 5, 6, 18, 20, 22, 36 (+8) |
| `os/tests/rollback-wrappers.sh` | 2, 12, 25, 38, 46, 65 (+6) |
| `os/tests/rpm_snapshot_invariants.sh` | 11, 13, 30, 63, 83, 93 (+12) |
| `os/tests/run-fex-tests.py` | 36, 43 |
| `os/tests/secureboot_signing_test.sh` | 6, 18, 23, 25, 97, 102 (+56) |
| `os/tests/secureboot_vm_enroll.c` | 7, 32, 36, 50, 54, 55 (+2) |
| `os/tests/secureboot_vm_probe.sh` | 16, 23, 36, 100, 127 |
| `os/tests/selinux_policy_store_invariants.sh` | 4 |
| `os/tests/system_extension_boot_pack_stage.sh` | 4, 5, 8, 9, 15, 17 (+4) |
| `os/tests/system_extension_invariants.sh` | 9, 10, 11, 12, 13, 14 (+76) |
| `os/tests/test_adopt_development_wpe.py` | 23, 24, 69 |
| `os/tests/test_broker_signing_runner.py` | 19, 28, 55 |
| `os/tests/test_builder_candidate_workflow.py` | 17, 66, 68 |
| `os/tests/test_cache_reconstruction_approval.py` | 23, 24, 25, 36 |
| `os/tests/test_compile_tools_source.py` | 65 |
| `os/tests/test_create_development_disk.py` | 153, 155, 160, 164, 165, 166 (+7) |
| `os/tests/test_development_compile.py` | 15, 42, 82, 88, 148, 187 (+5) |
| `os/tests/test_development_composefs.py` | 33, 41, 42 |
| `os/tests/test_development_prediction.py` | 43, 44, 49, 86, 120 |
| `os/tests/test_development_signer.py` | 2, 14 |
| `os/tests/test_development_source.py` | 121, 167 |
| `os/tests/test_development_wine_repository.py` | 12 |
| `os/tests/test_fetch_wine_source.py` | 43, 54 |
| `os/tests/test_finalization_driver.py` | 30, 37, 149, 177 |
| `os/tests/test_finalization_staging.py` | 128 |
| `os/tests/test_finalization_worker.py` | 26, 87, 88, 110, 113, 114 |
| `os/tests/test_hardware_firmware_artifact.py` | 61, 130 |
| `os/tests/test_import_development_rootfs.py` | 152 |
| `os/tests/test_installed_development_disk.py` | 22, 23, 25, 29, 32, 35 (+7) |
| `os/tests/test_installer_overlay.py` | 15, 16 |
| `os/tests/test_kernel_checkpoint_restore.py` | 137 |
| `os/tests/test_kernel_failure_checkpoint.py` | 5, 39 |
| `os/tests/test_native_payload_durability.py` | 11, 21, 60 |
| `os/tests/test_native_reconstruction_approval.py` | 12, 21, 22, 23 |
| `os/tests/test_native_resume.py` | 11, 57, 72 |
| `os/tests/test_native_resume_admission.py` | 13 |
| `os/tests/test_native_resume_launcher.py` | 13, 44 |
| `os/tests/test_native_source_compatibility.py` | 95, 104, 133, 200 |
| `os/tests/test_prepare_wpe_stage.py` | 77 |
| `os/tests/test_private_disk_image.py` | 18, 23, 28, 33, 38, 39 |
| `os/tests/test_produce_wpe_stage.py` | 95 |
| `os/tests/test_project_kernel_crate.py` | 24, 28 |
| `os/tests/test_public_trust_install.py` | 223 |
| `os/tests/test_rebind_development_rootfs.py` | 27, 60 |
| `os/tests/test_rebind_development_trust.py` | 38, 97 |
| `os/tests/test_reconstructed_native_consumer.py` | 76 |
| `os/tests/test_reconstructed_partials.py` | 5, 13, 28 |
| `os/tests/test_reseal_image.py` | 20, 35 |
| `os/tests/test_rootfs_layer_audit.py` | 164, 173 |
| `os/tests/test_rpm_snapshot_boundaries.py` | 13 |
| `os/tests/test_sign_development_rootfs.py` | 29, 30, 39, 72, 87, 97 (+2) |
| `os/tests/test_signing_spool.py` | 29, 33, 47, 48, 51, 53 (+3) |
| `os/tests/test_signing_spool_transport.py` | 20, 152 |
| `os/tests/test_supervised_disk.py` | 40, 80, 85, 116, 120, 145 |
| `os/tests/test_target_uki_projection.py` | 13 |
| `os/tests/test_windows_guest_contract.py` | 134 |
| `os/tests/test_windows_provider.py` | 153 |
| `os/tests/test_wine_mock_producer.py` | 77 |
| `os/tests/test_wine_rpm_contract.py` | 166, 174 |
| `os/tests/test_wpe_candidate_receipt.py` | 21 |
| `os/tests/test_wpe_stage_artifact.py` | 32, 63, 100, 102, 117, 154 (+2) |
| `os/tests/tilo-session-lifecycle.sh` | 2, 8, 9, 11, 12, 13 (+19) |
| `os/tests/tilo_root_discovery.sh` | 11, 12, 13, 27, 43, 44 (+18) |
| `os/tests/tinker_subprocess_invariants.sh` | 28 |
| `os/tests/tls_registry_fixture.py` | 32, 36, 57, 73 |
| `os/tests/uki_hardware_profile.py` | 31, 32 |
| `os/tests/vm_release_gate_invariants.sh` | 5, 6, 7, 8, 36, 41 (+7) |
| `os/tests/windows_compat_stack.sh` | 18, 24, 41, 68, 71, 84 |
| `os/tests/windows_owner_backend.sh` | 2, 7, 8, 9, 10, 37 (+11) |
| `os/tests/windows_presentation_capabilities.sh` | 14, 15, 39 |
| `os/tests/windows_vm_isolation.sh` | 11, 12, 13, 14, 15, 16 (+13) |
| `os/tests/workflow_dependency_invariants.sh` | 56, 186, 224, 248, 251, 267 (+4) |
| `os/tests/wpe-webdriver-smoke.py` | 42 |
| `os/tests/wpe_reproducibility_invariants.sh` | 8, 9, 13, 14, 21, 74 (+8) |
| `os/tests/wpe_snapshot_credentials.sh` | 5, 63 |
| `os/tests/wpe_stage_cache.py` | 22, 23, 26, 41, 48, 68 (+7) |
| `os/usr/bin/fs-test.sh` | 4, 7, 13, 17, 21, 24 (+8) |
| `os/usr/lib/bootc/install/40-tilo.toml` | 1, 3, 4, 5, 6, 8 (+12) |
| `os/usr/lib/bootc/kargs.d/01-hardening.toml` | 1, 10 |
| `os/usr/lib/bootc/kargs.d/03-boot.toml` | 1, 4, 7, 25 |
| `os/usr/lib/bootc/kargs.d/04-mode.toml` | 6 |
| `os/usr/lib/dracut/modules.d/91tilo-rollback/module-setup.sh` | 4, 9, 14, 21, 22, 23 |
| `os/usr/lib/sysctl.d/60-tilo-hardening.conf` | 5, 13, 18, 34, 37, 41 (+1) |
| `os/usr/lib/sysctl.d/70-tilo-performance.conf` | 13 |
| `os/usr/lib/systemd/system/bootc-fetch-apply-updates.service.d/10-tilo-boot-entries.conf` | 9 |
| `os/usr/lib/systemd/system/bootc-fetch-apply-updates.service.d/20-tilo-update-policy.conf` | 5, 9 |
| `os/usr/lib/systemd/system/cups.service.d/tilo-confinement.conf` | 6, 17, 45, 56, 61, 62 (+5) |
| `os/usr/lib/systemd/system/cups.socket.d/tilo-local-only.conf` | 3, 18 |
| `os/usr/lib/systemd/system/fs-identity.path` | 4, 13 |
| `os/usr/lib/systemd/system/fs.service` | 2, 4, 8, 11, 12, 16 (+26) |
| `os/usr/lib/systemd/system/fs.service.d/20-selinux.conf` | 5 |
| `os/usr/lib/systemd/system/systemd-growfs-root.service.d/10-tilo-composefs.conf` | 1 |
| `os/usr/lib/systemd/system/systemd-tpm2-setup.service.d/10-tilo-live-installer.conf` | 4 |
| `os/usr/lib/systemd/system/tilo-battery-cap@.service` | 6, 11, 12, 13, 15 |
| `os/usr/lib/systemd/system/tilo-boot-entries.service` | 4, 14, 49, 50 |
| `os/usr/lib/systemd/system/tilo-boot-health.service` | 4, 17, 46 |
| `os/usr/lib/systemd/system/tilo-deployment-rollback.service` | 10, 13 |
| `os/usr/lib/systemd/system/tilo-deployment-update.service` | 9 |
| `os/usr/lib/systemd/system/tilo-device-key-init.service` | 3, 9, 10, 24, 27, 38 (+1) |
| `os/usr/lib/systemd/system/tilo-firstboot-user.service` | 12, 34, 38, 41 |
| `os/usr/lib/systemd/system/tilo-fs-replica.service` | 11, 28, 29, 38, 47 |
| `os/usr/lib/systemd/system/tilo-growfs-sysroot.service` | 3, 4, 5, 9, 16, 42 |
| `os/usr/lib/systemd/system/tilo-growfs-sysroot.timer` | 6 |
| `os/usr/lib/systemd/system/tilo-hidepid.service` | 7, 12, 13, 20, 27, 31 |
| `os/usr/lib/systemd/system/tilo-installer-setup.service` | 35, 37, 42, 44, 45, 46 |
| `os/usr/lib/systemd/system/tilo-live-recovery.service` | 5, 10, 14 |
| `os/usr/lib/systemd/system/tilo-local-model.service` | 6, 7, 15, 39, 50, 52 |
| `os/usr/lib/systemd/system/tilo-power.service` | 13, 22, 23 |
| `os/usr/lib/systemd/system/tilo-relay-firstboot.service` | 12, 16, 31, 47, 50 |
| `os/usr/lib/systemd/system/tilo-relay-state-migrate.service` | 3, 7, 16, 30, 33 |
| `os/usr/lib/systemd/system/tilo-relay.service` | 9, 12, 13, 29, 34, 50 (+3) |
| `os/usr/lib/systemd/system/tilo-repair.target` | 19 |
| `os/usr/lib/systemd/system/tilo-repair@.service` | 7, 11, 16, 21 |
| `os/usr/lib/systemd/system/tilo-rollback-confirm.service` | 13, 29, 42, 43, 44, 47 (+1) |
| `os/usr/lib/systemd/system/tilo-rollback-harden.service` | 7, 9, 10, 17, 21, 48 (+5) |
| `os/usr/lib/systemd/system/tilo-secureboot-sign-shutdown.service` | 14, 17, 22, 38, 39, 62 (+1) |
| `os/usr/lib/systemd/system/tilo-secureboot-sign.path` | 4, 9, 24, 26, 27, 29 (+2) |
| `os/usr/lib/systemd/system/tilo-secureboot-sign.service` | 8, 11, 15, 58, 60 |
| `os/usr/lib/systemd/system/tilo-security-audit.service` | 10, 23, 45, 46, 47, 50 |
| `os/usr/lib/systemd/system/tilo-system-extension-boot-pack-stage.path` | 4, 10, 11 |
| `os/usr/lib/systemd/system/tilo-system-extension-boot-pack-stage.service` | 2, 6, 7, 11, 24, 25 (+3) |
| `os/usr/lib/systemd/system/tilo-system-extension-boot-pack-status.service` | 4, 5, 9, 17, 33, 41 (+2) |
| `os/usr/lib/systemd/system/tilo-system-extension-boot-stage.service` | 6, 10, 14, 16, 31, 39 (+1) |
| `os/usr/lib/systemd/system/tilo-system-extension-dispatcher.socket` | 4, 8 |
| `os/usr/lib/systemd/system/tilo-system-extension-dispatcher@.service` | 11, 29, 37, 39, 43 |
| `os/usr/lib/systemd/system/tilo-system-extension-userspace.service` | 5, 9, 29, 37, 39, 40 |
| `os/usr/lib/systemd/system/tilo-update-policy@.service` | 6, 16, 17, 29, 37 |
| `os/usr/lib/systemd/system/tilo-windows-vm-egress-reaper.service` | 11, 27, 31, 38 |
| `os/usr/lib/systemd/system/tilo-windows-vm-network.service` | 4, 11, 30, 38 |
| `os/usr/lib/systemd/system-generators/tilo-boot-mode-generator` | 4, 7, 9, 12, 14, 15 (+3) |
| `os/usr/lib/systemd/system-generators/tilo-rollback-unlock-generator` | 5, 6, 16, 17, 19, 27 (+1) |
| `os/usr/lib/systemd/system-generators/tilo-root-generator` | 4, 5, 11, 12, 16, 18 (+2) |
| `os/usr/lib/systemd/user/tilo-app-host.service` | 8 |
| `os/usr/lib/systemd/user/tilo-fs-recipient.service` | 36 |
| `os/usr/lib/systemd/user/tilo-graphical-session.target` | 3 |
| `os/usr/lib/systemd/user/tilo-identity.service` | 38, 56, 59 |
| `os/usr/lib/systemd/user/tilo-model-gateway.service` | 33, 45 |
| `os/usr/lib/systemd/user/tilo-models.service` | 26, 38 |
| `os/usr/lib/systemd/user/tilo-ush.service` | 18, 23 |
| `os/usr/lib/sysusers.d/tilo-identity.conf` | 4, 6 |
| `os/usr/lib/sysusers.d/tilo-relay.conf` | 2 |
| `os/usr/lib/tilo/jit-entitled.conf` | 13 |
| `os/usr/lib/tmpfiles.d/tilo-host.conf` | 2, 10, 11, 12, 13, 18 (+1) |
| `os/usr/lib/tmpfiles.d/tilo.conf` | 1, 3, 5, 15, 20, 21 (+24) |
| `os/usr/lib/udev/rules.d/90-tilo-backlight.rules` | 3, 18, 19, 22, 23 |
| `os/usr/lib/user-tmpfiles.d/tilo-model-manager.conf` | 3, 6 |
| `os/usr/libexec/tilo-battery-cap` | 12, 57, 58, 68, 71 |
| `os/usr/libexec/tilo-boot-health` | 5, 21, 22, 46, 53, 55 (+5) |
| `os/usr/libexec/tilo-deployment` | 14, 15, 17, 19, 20, 47 (+55) |
| `os/usr/libexec/tilo-disk-encrypt` | 26, 57, 67, 71, 72, 73 (+60) |
| `os/usr/libexec/tilo-firstboot-user` | 5, 48, 79 |
| `os/usr/libexec/tilo-fluxcast` | 3 |
| `os/usr/libexec/tilo-guided-partition` | 43 |
| `os/usr/libexec/tilo-image-trust-staged` | 5, 7, 8, 32, 37, 39 (+6) |
| `os/usr/libexec/tilo-install` | 6, 46, 47, 48, 50, 53 (+97) |
| `os/usr/libexec/tilo-install-boot-menu` | 2, 5, 8, 34, 85 |
| `os/usr/libexec/tilo-installer-profile-extract` | 9, 29, 37 |
| `os/usr/libexec/tilo-live-recovery` | 5, 17, 23, 31, 33 |
| `os/usr/libexec/tilo-power` | 7, 8, 36, 37 |
| `os/usr/libexec/tilo-recovery` | 11, 13, 14, 15, 18, 33 (+31) |
| `os/usr/libexec/tilo-relay-firstboot` | 6, 34, 66, 70 |
| `os/usr/libexec/tilo-relay-state-migrate` | 4, 12, 20 |
| `os/usr/libexec/tilo-repair` | 15, 27, 29, 63, 79, 106 (+16) |
| `os/usr/libexec/tilo-rollback-common` | 16, 25, 31, 34, 40, 51 (+4) |
| `os/usr/libexec/tilo-rollback-confirm` | 15, 31 |
| `os/usr/libexec/tilo-rollback-harden` | 16, 18, 20 |
| `os/usr/libexec/tilo-rollback-initrd-unlock` | 2, 8, 13, 14, 22, 30 (+3) |
| `os/usr/libexec/tilo-root-discover` | 2, 10, 11, 12, 20, 29 (+1) |
| `os/usr/libexec/tilo-secureboot` | 43, 45, 63, 90, 133, 143 (+34) |
| `os/usr/libexec/tilo-security-audit` | 4, 15, 20, 21, 23, 25 (+87) |
| `os/usr/libexec/tilo-session` | 3, 8, 38, 86 |
| `os/usr/libexec/tilo-session-select` | 12, 16 |
| `os/usr/libexec/tilo-system-extension-activate` | 8, 10 |
| `os/usr/libexec/tilo-system-extension-dispatch` | 7, 12, 26, 31, 39, 46 (+3) |
| `os/usr/libexec/tilo-update-policy-check` | 4 |
| `os/usr/libexec/tilo-update-policy-set` | 4, 6, 60, 61 |
| `os/usr/libexec/tilo-verified-boot-install-preflight` | 7, 37, 38, 40 |
| `os/usr/libexec/tilo-verified-update` | 5, 6, 7, 8, 20, 22 (+3) |
| `os/usr/libexec/tilo-windows-owner` | 5, 9, 13, 14, 15, 16 (+22) |
| `os/usr/libexec/tilo-windows-provision` | 11, 21, 55, 84, 93 |
| `os/usr/libexec/tilo-windows-recovery-console` | 9, 23, 26, 27 |
| `os/usr/libexec/tilo-windows-vm-network` | 8 |
| `os/usr/share/dbus-1/system.d/zz-tilo-projector-wifi-direct.conf` | 3, 9, 12 |
| `os/usr/share/doc/tilo/model-gateway.md` | 3 |
| `os/usr/share/doc/tilo/system-extensions.md` | 10 |
| `os/usr/share/polkit-1/actions/org.tilo.home-peer.policy` | 10 |
| `os/usr/share/polkit-1/actions/org.tilo.windows.policy` | 16, 28, 43, 53, 63, 73 (+3) |
| `os/usr/share/tilo/installer-sudoers` | 25 |
| `os/usr/share/tilo/selinux/tilo-base.cil` | 12, 13, 24, 36, 45, 51 (+2) |
| `os/usr/share/tilo/selinux/tilo-installer.cil` | 14, 15, 16, 18, 20, 45 (+3) |
| `os/usr/share/tilo/system-extensions/README.md` | 3, 10, 22, 48, 55, 74 (+3) |
| `os/usr/share/tilo/windows/fex-emulator.json` | 9, 10, 11 |
| `os/usr/share/tilo/windows/fex-graphics-provider.json` | 4 |
| `os/usr/share/tilo/windows/guest/Install-TiloRemoteApp.ps1` | 133 |
| `os/usr/share/tilo/windows/guest/Tilo-ProcessJob.ps1` | 142, 145, 152, 160, 173, 176 (+5) |
| `os/usr/share/wayland-sessions/shell-peer-sync.desktop` | 2, 7, 14 |
| `os/usr/share/wayland-sessions/shell.desktop` | 2, 9, 18 |
| `os/verify-test.sh` | 14, 20, 28 |
| `release/local-models/smolvlm2-500m/smoke-gateway.py` | 52 |
| `scripts/app-grid.sh` | 124, 159, 160 |
| `scripts/app-launch-sweep.py` | 8, 83, 250, 412, 436, 440 |
| `scripts/browser/benchmark-pages.mjs` | 538 |
| `scripts/browser/build-host.py` | 22, 122 |
| `scripts/browser/package.py` | 31, 61, 62, 63, 65 |
| `scripts/browser/test_build_host.py` | 15, 16 |
| `scripts/browser/test_package.py` | 19, 36, 49 |
| `scripts/check-install-contract.py` | 59, 63, 76, 77, 78, 79 (+2) |
| `scripts/ci-fuse-preflight.sh` | 8, 9, 10, 13, 14, 19 (+3) |
| `scripts/demo-assemble.py` | 8 |
| `scripts/demo-home.py` | 103 |
| `scripts/demo-record.sh` | 21, 22, 91, 93, 111, 112 (+3) |
| `scripts/edited-handoff-proof.py` | 43 |
| `scripts/fs-collab-demo.sh` | 8, 34, 36, 37, 58, 70 (+1) |
| `scripts/headless-examples/_control.py` | 23 |
| `scripts/headless-examples/agent-admitted-proof.py` | 87 |
| `scripts/headless-examples/agent-collab-proof.py` | 128 |
| `scripts/headless-examples/agent-demo.py` | 7, 10, 91, 143, 181, 182 (+1) |
| `scripts/headless-examples/agent-second-session-proof.py` | 113, 271, 335, 339, 341, 365 (+2) |
| `scripts/headless-examples/app-collab-proof.py` | 149, 193 |
| `scripts/headless-examples/app-launch-proof.py` | 118, 166, 195 |
| `scripts/headless-examples/app-tab-name-proof.py` | 12, 77, 120, 179 |
| `scripts/headless-examples/collab-panel-proof.py` | 136, 164, 196 |
| `scripts/headless-examples/desktop-peer-cursor-proof.py` | 115 |
| `scripts/headless-examples/install-open-timing.py` | 108, 142, 549, 553, 554, 555 (+1) |
| `scripts/headless-examples/installed-app-collab-proof.py` | 177 |
| `scripts/headless-examples/invite-clipboard-proof.py` | 111, 135, 164 |
| `scripts/headless-examples/named-peer-proof.py` | 128, 171, 194, 245, 257, 335 (+2) |
| `scripts/headless-examples/no-pill-for-yourself-proof.py` | 147 |
| `scripts/headless-examples/older-binary-meets-newer-state.py` | 41 |
| `scripts/headless-examples/open-feedback-proof.py` | 97 |
| `scripts/headless-examples/peer-window-drawn-proof.py` | 108 |
| `scripts/headless-examples/peer-window-opens-here-proof.py` | 139, 187 |
| `scripts/headless-examples/peers-cross-both-ways.py` | 79, 134 |
| `scripts/headless-examples/press-lands-where-drawn-proof.py` | 99, 125, 246 |
| `scripts/headless-examples/reaper.py` | 14, 31, 36, 37, 57, 58 (+1) |
| `scripts/headless-examples/room-index-proof.py` | 127 |
| `scripts/headless-examples/tilo_client.py` | 176 |
| `scripts/headless-examples/word-processor-convergence-proof.py` | 161, 267 |
| `scripts/install-on-open-demo.py` | 49, 171, 254, 276, 280, 316 (+5) |
| `scripts/legacy-app-caret-e2e.py` | 8, 39, 140, 144, 174, 183 (+7) |
| `scripts/perf/README.md` | 12, 13, 14, 19, 20, 21 (+4) |
| `scripts/perf/bolt-optimize.sh` | 14, 16, 17 |
| `scripts/perf/frame-loop-bench.py` | 7, 35, 242, 254, 270, 323 (+4) |
| `scripts/perf/pgo-instrument.sh` | 15, 17, 18 |
| `scripts/perf/pgo-merge.sh` | 14, 16, 17 |
| `scripts/perf/pgo-rebuild.sh` | 14, 16, 17 |
| `scripts/perf/qemu-boot-measure.sh` | 2, 5, 6, 8, 13, 17 (+15) |
| `scripts/perf/qemu-install-boot-verify.sh` | 3, 5, 6, 7, 8, 10 (+45) |
| `scripts/perf/runtime_benchmark.py` | 123, 124, 125 |
| `scripts/runtime-auto-attach-proof.py` | 162 |
| `scripts/runtime-mcp-proof.py` | 48 |
| `scripts/sandboxed-app-collab.py` | 7, 13, 22, 44, 59, 61 (+8) |
| `scripts/screenshot-app.py` | 5, 7, 9, 184, 230, 274 (+1) |
| `scripts/shared-runtime-proof.py` | 54 |
| `scripts/test-flatpak-share.sh` | 5, 17, 54, 55, 56, 57 (+20) |
| `scripts/tilo-mcp-server.py` | 66 |
| `scripts/two-session-collab-demo.py` | 52, 66, 367 |
| `scripts/two-shell-app-collab.py` | 40, 83, 121, 169, 188, 189 (+8) |
| `scripts/yc-demo-insert.py` | 32, 136, 139, 141, 144, 148 (+7) |
| `scripts/yc-demo.py` | 282, 303, 405, 746, 977, 2684 (+4) |
| `tools/atspi_external_client.py` | 136, 214, 218 |
| `tools/benchmark-qpkg-startup.sh` | 4 |
| `tools/binary-budget.toml` | 48 |
| `tools/ci/cargo-shim` | 7, 43, 44, 76, 93, 99 (+3) |
| `tools/ci/check-build-env.sh` | 13, 14, 18, 21, 44, 46 (+2) |
| `tools/ci/measure.sh` | 16, 17 |
| `tools/ci/sccache.sh` | 14 |
| `tools/run-imager-ephemeral-runner.sh` | 91, 92, 180 |
| `tools/tests/cargo_shim.sh` | 59, 61, 73, 74 |
| `tools/tests/test_app_resource_action_contract.py` | 262, 271 |
| `tools/tests/test_workflow_package_paths.py` | 118 |
| `tools/verify-vendor-trees.py` | 32, 33 |
