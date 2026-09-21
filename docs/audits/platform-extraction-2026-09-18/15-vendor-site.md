# A15 — vendor, website, documentation and residual top-level audit

## Scope and method

Scope: `vendor/`, `site/`, maintained documentation, top-level plans and manifests, and fuzz targets. The audit covers platform guards and FFI, native paths and commands, browser detection, native dependency selection, architecture contracts, and vendor patch provenance. Application-framework internals, build output, task metadata, and upstream-diff reconstruction are outside its scope. Candidate counts are not issue counts; findings below contain the reviewed conclusions.

## Findings

**Current-tree note:** The `site/...` paths in A15-05 and A15-06 are
historical website-source anchors; this branch has no root `site/` tree. Keep
the release/provisioning and media-tooling architecture conclusions, but do
not present those specific website paths as current files.

**Source-anchor note:** Verified prefix repairs identify current moved paths where
available. Any `:line` suffix remains the original 2026-09-18 audit coordinate; it
is historical evidence and does not assert that the cited symbol is at that line
in the current tree.

### A15-01 — Destination is a proposed facade plus focused service crates, not an existing monolithic platform crate

- Evidence: `docs/architecture/platform-services.md` says proposed / implementation not started; `:14` specifies `ui::platform`, `:16` implementation under `crates/system` or `crates/runtime`; `:19` forbids dependency on apps; `:21` retains portal authorization and explicit unavailable results. `:50` rejects placing everything in the renderer or using source path includes.
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
