# A15 — vendor, website, documentation and residual top-level audit

## Scope and method

Read-only audit of `vendor/`, `site/`, `docs/`, `plan/`, root manifests/docs and `fuzz/`; root `flake.nix` classified as deployment glue (detailed deployment/tooling audit belongs to the assigned packaging agent). Excludes `ui/`, `crates/app/`, `target/`, `.git/`, `.codex/`, `.superpowers/`. No source edits, builds, tests, desktop access, or upstream network validation. Read root instructions and vendor patch provenance, searched OS cfg/FFI/imports, native paths, commands, browser detection, native dependency selectors and architecture contracts. Vendor upstream provenance claims are documented claims, not independently reconstructed diffs. No scoped AGENTS.md found in these roots.

Inventory (regular files, including binary assets/lockfiles; excludes site node_modules/.astro/dist): vendor 422, site 109, docs 289, plan 12. Vendor breakdown: winit 216, wgpu-hal 85, vello_shaders 71, vello_encoding 24, vello 19, genawaiter-proc-macro 5, automerge 1, plus PATCHES.md. Narrow platform lexical scan matched 79 vendor files (winit 53, wgpu-hal 22, vello 3, vello_shaders 1), one site file, 90 docs, two plan files. These are candidate counts, not issue counts; command/provisioning review additionally found site media.py and guide data outside that narrow expression. `vendor/automerge` contains only Cargo.lock, not an extractable backend. No platform mechanisms found in the four fuzz targets; transitive lockfile libc entries are not ownership leaks.

## Findings

### A15-01 — Destination is a proposed facade plus focused service crates, not an existing monolithic platform crate

- Evidence: `docs/superpowers/specs/2026-09-18-framework-platform-services-design.md:3` says proposed / implementation not started; `:14` specifies `ui::platform`, `:16` implementation under `crates/system` or `crates/runtime`; `:19` forbids dependency on apps; `:21` retains portal authorization and explicit unavailable results. `:50` rejects placing everything in the renderer or using source path includes.
- Classification: architecture constraint, all hosts. Impact: inventing a destination crate/API or assuming the entire framework already exists produces an unactionable migration.
- Action: map every service to existing owners first, add narrowly scoped implementation ownership only where absent, expose semantic operations through the proposed facade. Keep headless integrations independent of the UI renderer. This report intentionally does not inspect the excluded framework implementation.
- Acceptance: dependency graph has no framework-to-app edges; no path includes back into apps; existing authorization, typed unavailable results, and recovery invariants remain intact. The document's proposal status is not proof of approved or completed implementation.

### A15-02 — Tilo-specific Wayland drag/drop includes storage policy inside the vendor backend

- Evidence: `vendor/winit/src/platform_impl/linux/wayland/seat/data_device.rs:1` documents first-party receive/send additions and sandbox rationale; `:107` prefers bytes over URI paths, `:119` decodes file URIs, `:185` selects XDG_RUNTIME_DIR/XDG_CACHE_HOME/HOME/temp, `:206` probes writability using a PID filename, `:240` delivers payloads and `:252` constructs spool filenames before fs::write. `:432` implements outgoing drag. `vendor/winit/src/platform/wayland.rs:100` exposes start_drag; seat setup at `vendor/winit/src/platform_impl/linux/wayland/seat/mod.rs:153`; state delivery at `vendor/winit/src/platform_impl/linux/wayland/state.rs:170`.
- Platform: Wayland Unix builds. Classification: keep protocol transport backend; isolate first-party host storage policy behind a platform-owned contract. This is the clearest extraction candidate in this scope, not a recommendation to fork/refactor every upstream backend.
- Impact: receiving files crosses sandbox namespaces and lifetime/storage boundaries. Moving the current path-only event API without its byte preference and spool guarantees can break confined clients. Current probing and writing are separate operations; migration must not assume a probe proves the later path is private/safe. This is code-level evidence, not a demonstrated exploit.
- Boundary: semantic received-transfer bytes/MIME with a platform-owned private temporary artifact handle, explicit completion/error and lifetime, while Wayland objects stay inside winit. Preserve surface-local coordinate mapping, offer finish/destroy, cancellation and asynchronous event-loop handoff. Decide ownership/cleanup explicitly rather than moving filenames alone.
- Migration: document vendor API contract; introduce callback/adapter seam without vendor depending on app crates; transfer directory/private-file policy to the focused host storage service; migrate framework caller outside this audit's excluded scope; keep portable MIME/URI parsing independently testable.
- Acceptance: confined sender/receiver transfer bytes when paths are unreadable; read-only XDG runtime fallback; unavailable writable destination produces observable error; collision/symlink-resistant artifact creation and cleanup; no extra filesystem authority; drag cancel, seat removal and no data-device-manager remain safe. Retain the production-path DnD tests currently in this source.

### A15-03 — Gesture normalization is an intentional vendor platform boundary

- Evidence: `Cargo.toml:469`; `vendor/winit/src/platform_impl/linux/wayland/seat/pointer/gestures.rs:5` cumulative Wayland scale to incremental winit delta, `:81` update fold; registration `vendor/winit/src/platform_impl/linux/wayland/seat/mod.rs:135`, `vendor/winit/src/platform_impl/linux/wayland/state.rs:212`; AppKit swipe translation `vendor/winit/src/platform_impl/macos/view.rs:706`; event contract `vendor/winit/src/event.rs:316` and `:327`; public Wayland extensions `vendor/winit/src/platform/wayland.rs`.
- Classification: retain backend, do not extract all native windowing internals. Platform: Wayland and macOS with other upstream winit backends left intact.
- Impact: callers need normalized gesture semantics, not protocol objects, Cocoa selectors or duplicate OS branches.
- Migration/boundary: maintain winit event adapter as platform dependency; describe pinch delta and discrete swipe sign/phase in a framework-facing semantic input contract. Preserve existing event lifecycle and upstream history.
- Acceptance: cumulative scale sequences yield expected incremental deltas; start/end/cancel reset state; AppKit left/up signs map to expected coordinates; unsupported gesture protocol is unavailable without breaking pointer input. Test native backend and portable consumer separately.

### A15-04 — Opt-in Metal presentation feedback must retain its asynchronous native contract

- Evidence: `Cargo.toml:472`; `vendor/wgpu-hal/Cargo.toml:53` qos-presentation-feedback; `vendor/wgpu-hal/src/metal/surface.rs:28` documents registration and callback contract, `:41` checks macOS 10.15.4/iOS 10.3/tvOS 10.2 availability; `vendor/wgpu-hal/src/metal/mod.rs:502` observer storage and `:607` one-shot addPresentedHandler.
- Classification: intentional native GPU backend. No mass extraction of wgpu-hal Vulkan/GLES/DX12/Metal.
- Impact: raw Metal host-time seconds, drawable identity and missing/zero observations cannot be treated as portable display success or wall-clock timestamps.
- Boundary/action: renderer platform adapter should expose typed presentation observations with clock provenance and unavailable state; retain ObjC and availability guards in the dependency backend; keep feature default-off and callback registration serialized with presentation.
- Acceptance: feature disabled retains original presentation path; unsupported OS and double registration fail explicitly; zero/missing callback never counts as displayed; callback never blocks/panics; native macOS runtime validation in addition to compilation.

### A15-05 — Site platform choices are release/provisioning contracts, not runtime detection to move into Rust

- Evidence: `site/src/scripts/start-path.ts:340` takes explicit query choices and user buttons (`:326`); no navigator userAgent/platform detection found in site source. `site/src/data/install-contract.json:5` host matrix, `:26` complete-system CPU matrix, `:38` distro release gates; `site/src/data/install-guides.ts:12` dnf, `:29` apt, `:60` Nix configuration. `site/scripts/check-install-contract.mjs:29` compares canonical `docs/install-contract.json` only when --qos is passed. `site/scripts/check-package-guides.mjs:25` requires NixOS x86_64-linux; `site/scripts/check-release-manifest.mjs:19` checks signing/release assets. `site/src/pages/docs/verify-release.astro:36` publishes gpg and sha256sum commands.
- Classification: retain target-specific website and packaging documentation. Platform: host-specific install media and Linux session delivery. Explicit release support differs from source support: docs/architecture/platform-support.md records Windows ARM64 compile/fixture evidence while site host matrix currently lists Windows x86_64 only; this is not by itself a bug or permission to advertise ARM64.
- Impact: indiscriminate extraction would erase distinctions between host OS, guest architecture, available release and planned support. Separate runtime capability from release availability.
- Action: keep guides in site; consume canonical release/install metadata and always run canonical parity comparison during release; migrate only reusable architecture-name normalization if supported by actual packaging owners. `flake.nix:8` enumerates both Linux architectures while site Nix release gate is x86_64, another intentional source/released distinction to preserve.
- Acceptance: guides preserve explicit architecture and supported distribution; unpublished assets remain unlinked; canonical contract parity succeeds; no user-agent inference silently selects destructive provisioning; native runtime support cannot be inferred from a website choice.

### A15-06 — Media capture has intentional Linux host escape/tool discovery, outside app runtime

- Evidence: `site/scripts/media:1` POSIX wrapper; `:9` checks ffprobe and flatpak-spawn then `:10` executes flatpak-spawn --host python3. `site/scripts/media.py:91` PATH-first tool lookup, `:99` /usr/bin,/usr/local/bin,/home/linuxbrew fallback; `:562` QOS_SOURCE; `:576` builds shots and later app binaries. `site/scripts/books-continuity-demo.py:133` uses ffmpeg from PATH. `site/docs/media-capture.md:6` explains capture workflow.
- Classification: development/media tooling, retain target-specific; not a product sandbox mechanism and must not be imported into runtime platform services.
- Impact: host reentry is explicit developer tooling and depends on host permissions; tool availability is not platform capability/security evidence.
- Action: retain wrapper with documented supported environments and explicit failures; if consolidating tool discovery, do so in tooling helpers, not framework runtime. Do not propagate flatpak-spawn host escape into app adapters.
- Acceptance: PATH-first invocation, missing tools fail clearly, supported sandbox host path finds same source/tool environment, capture does not access a real user desktop. No commands executed in this audit.

### A15-07 — Portable renderer patches and compiler compatibility are false positives for OS extraction

- Evidence: `vendor/PATCHES.md:5` exportable baseline/topic histories; `:49` Vello patch explanation; `vendor/vello/src/bump_budget.rs`, `vendor/vello/src/lib.rs:387`, `vendor/vello/src/render.rs:371`, `vendor/vello/src/wgpu_engine.rs:2775`, `vendor/vello_encoding/src/config.rs:599`, `vendor/vello_encoding/src/resolve.rs`, `vendor/vello_encoding/src/image_cache.rs`, `vendor/vello_encoding/src/lib.rs`; matching shaders remain `vendor/vello_shaders/`. `vendor/genawaiter-proc-macro/README.md:1` defines diagnostic compatibility fork; `Cargo.toml:474` and `:479` activate patches.
- Classification: retain portable dependency patches. Vello scratch allocation/atlas diagnostics are rendering mechanisms, not an OS service; genawaiter compile diagnostics are not host integration. Normal upstream winit/wgpu-hal native dependency sections, FFI and build.rs are deliberate backend implementations.
- Action: preserve patch provenance and matching shader/encoding API versions, continue upstreaming rather than transplanting sources into platform framework. Treat vendor PATCHES.md as a starting inventory: observed DnD, macOS swipe and Metal changes also require documented contracts beyond its main Vello narrative.
- Acceptance: dependency feature matrix and native backend checks; patch histories/tree comparisons remain reviewable; no Cargo.lock regeneration. No test execution claimed here.

## Documentation executable snippets and coverage limits

`docs/architecture/platform-support.md:11` requires Rust/OCI architecture normalization at tool boundaries and rejects unknown values; `:14` requires native runtime validation beyond cross-compilation. Its boot commands remain Linux deployment procedures. Root `DEVELOPMENT.md:39` gives native Windows compiler prerequisites, `:62` macOS GStreamer Homebrew setup; these are documented provisioning, not implementations to relocate. Documentation includes many historical design/plan snippets: lexical hits must be traced to current production ownership by the corresponding subsystem audit before migration. No standalone .rs/.py/.sh/.nix/.toml/.yaml/.yml executable files were found under docs/plan by rg; JSON configuration includes canonical install contract and macOS qualification specs. The following appendix records every narrow-pattern matching documentation file, but does not assert each is an active requirement. Source support versus published hardware support must remain explicit.

## Lexical inventory appendix

Pattern: `target_os|target_arch|target_family|cfg(unix|cfg(windows|std::os|libc::|nix::|extern C/S|/proc/|/sys/|/dev/|/tmp/|systemctl|navigator.platform/userAgent`. Vendor files are counted/grouped rather than flooding the report with upstream native symbols. Documentation list follows.
- `docs/agent-action-api.md:145`
- `docs/app-model/cli-apps.md:26`
- `docs/app-model/formats.md:178`
- `docs/app-model/hello-world.md:34`
- `docs/app-model/installation.md:182`
- `docs/app-model/packaging.md:215`
- `docs/app-model/runtime.md:244`
- `docs/app-model/sandbox.md:201`
- `docs/app-model/trust.md:101`
- `docs/app-model/ush.md:169`
- `docs/architecture/platform-support.md:56`
- `docs/architecture.md:30`
- `docs/collaborative-files-product.md:185`
- `docs/demo-script.md:175`
- `docs/development-signing.md:25`
- `docs/flatpak-share-testing.md:43`
- `docs/headless-testing.md:45`
- `docs/identity-security.md:33`
- `docs/imager-release-operations.md:82`
- `docs/installer-walkthrough.md:51`
- `docs/legacy-app-caret-e2e.md:24`
- `docs/performance-flags.md:149`
- `docs/printing-threat-model.md:120`
- `docs/recovery.md:163`
- `docs/regression/2026-09-15-local-parallels.md:90`
- `docs/runtime-mcp.md:57`
- `docs/security/boot-chain.md:160`
- `docs/security/composefs-decision.md:260`
- `docs/security/defects.md:154`
- `docs/security/repair-mode.md:15`
- `docs/security/rollback.md:68`
- `docs/security/tinkering.md:98`
- `docs/selinux-installer.md:105`
- `docs/superpowers/audits/2026-09-15-all-app-ui-review/implementation/composition-task-3-fix-1-report.md:42`
- `docs/superpowers/audits/2026-09-15-all-app-ui-review/implementation/composition-task-3-report.md:52`
- `docs/superpowers/audits/2026-09-15-all-app-ui-review/implementation/composition-task-3-review.md:49`
- `docs/superpowers/audits/2026-09-15-all-app-ui-review/implementation/shared-task-3-report.md:82`
- `docs/superpowers/audits/2026-09-15-all-app-ui-review/implementation/shared-task-4-report.md:53`
- `docs/superpowers/audits/2026-09-15-all-app-ui-review/review-2.md:12`
- `docs/superpowers/audits/2026-09-15-all-app-ui-review/review-3.md:9`
- `docs/superpowers/audits/2026-09-15-framework-layout-evidence.md:15`
- `docs/superpowers/audits/2026-09-15-framework-layout-progress.md:109`
- `docs/superpowers/audits/2026-09-15-layout-model-comparison.md:21`
- `docs/superpowers/audits/2026-09-16-ui-alignment-review/README.md:90`
- `docs/superpowers/audits/2026-09-16-ui-alignment-review/a1-devtools-1.md:26`
- `docs/superpowers/audits/2026-09-16-ui-alignment-review/a10-large-editors-browser.md:212`
- `docs/superpowers/audits/2026-09-16-ui-alignment-review/a3-browse-preferences.md:112`
- `docs/superpowers/audits/2026-09-16-ui-alignment-review/a4-communication.md:157`
- `docs/superpowers/audits/2026-09-16-ui-alignment-review/a5-management-install.md:33`
- `docs/superpowers/audits/2026-09-16-ui-alignment-review/a6-content-1.md:5`
- `docs/superpowers/audits/2026-09-16-ui-alignment-review/a7-office-editors.md:185`
- `docs/superpowers/audits/2026-09-16-ui-alignment-review/a8-media.md:221`
- `docs/superpowers/audits/2026-09-16-ui-alignment-review/a9-documents-files.md:200`
- `docs/superpowers/audits/2026-09-16-ui-alignment-review/f1-layout-containers-tokens.md:27`
- `docs/superpowers/audits/2026-09-16-ui-alignment-review/f3-composition-data-display.md:44`
- `docs/superpowers/audits/2026-09-16-ui-alignment-review/f4-popups-menus-overlays.md:6`
- `docs/superpowers/audits/2026-09-16-ui-alignment-review/f5-timeline-media-capture.md:83`
- `docs/superpowers/audits/2026-09-16-ui-alignment-review/f6-window-chrome-identity.md:51`
- `docs/superpowers/audits/2026-09-16-ui-alignment-review/s1-shell-topbar-wheel-strips.md:225`
- `docs/superpowers/audits/2026-09-16-ui-alignment-review/s2-shell-panels-system.md:22`
- `docs/superpowers/audits/2026-09-17-ui-1x-polish/README.md:29`
- `docs/superpowers/audits/2026-09-17-ui-1x-polish/a11-imager.md:66`
- `docs/superpowers/audits/2026-09-17-ui-1x-polish/a2-utilities.md:4`
- `docs/superpowers/audits/2026-09-17-ui-1x-polish/a5-management.md:159`
- `docs/superpowers/audits/2026-09-17-ui-1x-polish/f4-media-history-surface.md:5`
- `docs/superpowers/audits/2026-09-17-ui-1x-polish/s1-host-bar-quick-settings.md:4`
- `docs/superpowers/audits/2026-09-17-ui-1x-polish/s2-panels-sheets-system.md:226`
- `docs/superpowers/audits/2026-09-17-ui-alignment-review-r2/a1-devtools-1.md:26`
- `docs/superpowers/audits/2026-09-17-ui-alignment-review-r2/a5-management-install.md:133`
- `docs/superpowers/audits/2026-09-17-ui-alignment-review-r2/a6-content-1.md:5`
- `docs/superpowers/audits/2026-09-17-ui-alignment-review-r2/a7-office-editors.md:201`
- `docs/superpowers/audits/2026-09-17-ui-alignment-review-r2/f4-popups-menus-overlays.md:5`
- `docs/superpowers/audits/2026-09-17-ui-alignment-review-r2/f5-timeline-media-capture.md:5`
- `docs/superpowers/audits/2026-09-17-ui-alignment-review-r2/x1-text-scale-centring.md:323`
- `docs/superpowers/audits/framework-layout-verification/final-fix-report.md:9`
- `docs/superpowers/audits/runtime-auto-attach/task-2-report.md:15`
- `docs/superpowers/audits/runtime-auto-attach/task-2-review.md:20`
- `docs/superpowers/audits/shared-runtime-mcp-final/final-fix-report.md:49`
- `docs/superpowers/plans/2026-09-15-framework-layout-improvements.md:109`
- `docs/superpowers/plans/2026-09-15-measured-ui-components.md:23`
- `docs/superpowers/plans/2026-09-15-shell-macos-runner.md:37`
- `docs/superpowers/plans/2026-09-16-framework-history.md:20`
- `docs/superpowers/plans/2026-09-16-guided-setup-foundation.md:82`
- `docs/superpowers/plans/2026-09-16-internal-installer-boot-proof.md:26`
- `docs/superpowers/specs/2026-09-15-ui-quality-design.md:38`
- `docs/superpowers/specs/2026-09-16-internal-install-feasibility.md:12`
- `docs/supply-chain.md:139`
- `docs/testing.md:33`
- `docs/typography.md:342`
- `docs/windows-compatibility.md:75`
- `plan/os-layer.md:51`
- `plan/security.md:120`
