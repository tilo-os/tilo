# Platform extraction audit

## Objective and scope

Find platform-specific mechanisms outside the application framework and record
the service boundary, source evidence, migration constraints, and acceptance
conditions needed to move their ownership.

The excluded application framework is `ui/` and `crates/app/`. Independent service crates, even where consumed by `ui`, are included. All 55 directories under `apps/` and every remaining crate domain were assigned. Compositor, filesystem, native browser integration, build scripts, tests, packaging, OS deployment, and first-party vendor patches were included. Upstream vendor backends are inventoried as retained dependencies, not an instruction to rewrite third-party code.

This audit follows the [framework platform-services boundary](../../architecture/platform-services.md). Its destination is an app-facing semantic facade backed by focused non-rendering services, reusing existing authorities. Capability names in this report describe contracts to implement or extend; they are not claims that corresponding APIs already exist. Re-resolve source anchors against current code before editing.

## Findings at a glance

Platform-specific mechanisms are spread through app code, shared domain code, service implementations, and tooling. Moving only conditional imports will not fix that distribution. The main repeated seams are:

- Host paths, file opening/reveal, native dialogs, locale and desktop app discovery.
- Private storage, durable replacement, file identity, locking, recovery and native path semantics.
- Local transport, peer authentication, descriptor/handle transfer and caller-lifetime evidence.
- Child process trees, bounded execution, cancellation and privileged helper lifecycle.
- Credentials, native trust stores and identity provisioning.
- Hardware inspection, settings authorities, media/capture, native browser and document engines.
- Filesystem mount/hydration and deployment-specific operations that need explicit backend boundaries.

Some findings are hard portability defects; others are correct native implementations owned by the wrong layer. Examples of source-evidenced defects include unconditional Unix imports in shared audio and TTS code, Linux peer credentials in the model gateway, non-Unix locking/liveness fallbacks, and target-specific helper discovery or trust paths. Each domain report gives its exact platform and feature condition. No cross-target builds or live OS tests were run; source deductions are not runtime validation.

## How to use this handoff

1. Read the destination design and the work-package table below.
2. Choose one service boundary, then read every linked domain report for its producers and callers. Do not assign separate implementations of the same service to different app teams.
3. Use the stable `Axx-yy` finding IDs to track disposition: extract, reuse an existing service, retain as a platform backend, retain as deployment/tooling, or dismiss with evidence.
4. Move implementation, required types and behavioral tests together. Migrate all callers in that work package and make native dependencies target/feature-specific.
5. Verify behavior on the actual supported hosts and record evidence. A successful compile or offscreen screenshot alone does not establish permission, persistence, recovery, process-lifetime or native-device behavior.

Do not treat the lexical inventories as a defect list. They contain deliberate native adapters, tests, paths in fixtures, and false positives. The reviewed findings are the actionable layer; the inventories are coverage evidence and a way to find additional related call sites.

## Consolidated implementation work packages

These packages deduplicate the fifteen reports. Ordering reflects dependency and security risk, not a demand to add a large new platform crate. Existing `atomic-file`, `accounts`, `bounded-command`, `os-actions`, runtime service clients and media abstractions should be evaluated and extended before introducing equivalents.

| Package | Mechanisms and callers to migrate | Evidence / owners | Acceptance outcome |
| --- | --- | --- | --- |
| P01 — Private storage and durability | Atomic replace, no-follow opens, native file identity, ownership/privacy, directory flushes, locks, exclusive creation, journals and recovery. Used by identity, app-state, Browser, Tube, TOTP, Imager, filesystem and compatibility services. | [System A09-01/02](09-system.md), [Identity A10-01/09](10-collab-identity.md), [Imager A05-06–09](05-imager.md), [Apps A01-03/05/09](01-apps-productivity.md), [Tube A02-07](02-apps-media.md), [Browser A03-07](03-apps-communication.md), [FS A13-06](13-filesystem.md). | Equivalent privacy, exclusive ownership, crash durability and recovery semantics on each supported host. No success-shaped fallback for an unavailable guarantee. Preserve formats and fault-injection tests. |
| P02 — Paths, environment and host identity | Known folders, config/data/cache/runtime roots, locale/time-zone discovery, host architecture versus binary architecture, deployment paths and process identity. | [Productivity A01](01-apps-productivity.md), [Media A02-09](02-apps-media.md), [Imager A05-02–04](05-imager.md), [Shell A06-08](06-shell-state.md), [System A09-03](09-system.md), [Identity A10-05](10-collab-identity.md), [Other crates A12-05](12-other-crates.md). | Apps consume typed paths/context; native defaults and migration from old locations are covered. No silent state reset or merging of unrelated users/workspaces. |
| P03 — Authenticated local services | Local endpoints, IPC framing, trusted peer identity, native handles, descriptor transfer, listener cleanup, service discovery and process-lifetime leases. | [Terminal/Agent A03-02/04](03-apps-communication.md), [Shell A06-03–05](06-shell-state.md), [Backends A07-01](07-shell-backends.md), [Runtime A08](08-runtime-packaging.md), [Identity A10-03/04](10-collab-identity.md), [Other crates A12-01/04/08](12-other-crates.md), [FS A13-03](13-filesystem.md), [Tools A14-01](14-os-tooling.md). | Callers use semantic service clients. Linux/macOS/Windows transports preserve authentication and handle ownership; unsupported implementations fail explicitly. Keep the existing tilo-control protocol and authorization story. |
| P04 — Process and helper lifecycle | Bounded subprocesses, process-tree ownership, cancellation/reaping, inherited handles, PTYs, host-shell selection, helper location and privilege elevation. | [Terminal A03-03](03-apps-communication.md), [Shell A06-07/11/15](06-shell-state.md), [Runtime A08](08-runtime-packaging.md), [System A09-04](09-system.md), [Identity A10-04/06/07](10-collab-identity.md), [Tools A14-02/03](14-os-tooling.md). | Timeouts/cancellation terminate the owned process tree and release handles. No PID-reuse assumptions, stale leases, orphan helpers, loss of privilege checks or blocking execution on UI paths. |
| P05 — Desktop interactions | Open/reveal URI or granted file, native chooser calls, desktop app discovery, drag export lifetime, desktop integration and shortcuts. | [Productivity A01](01-apps-productivity.md), [Camera A02-02](02-apps-media.md), [External opening A03-01/11](03-apps-communication.md), [Imager A05-03](05-imager.md), [Shell A06-01/02](06-shell-state.md), [System A09-09](09-system.md), [Vendor A15-02/03](15-vendor-site.md). | Apps issue authorized asynchronous operations and receive typed success/cancel/unavailable/error results. Preserve granted-path and drag temporary-file lifetime contracts. |
| P06 — Credentials and trust | Native credential custody, Secret Service/Keychain adapters, SSH trust-store discovery, native CA loading, secret-file fallback and caller namespace. | [TOTP A01](01-apps-productivity.md), [Tube A02-08](02-apps-media.md), [Browser/Remote A03-09/10](03-apps-communication.md), [System A09-10/11](09-system.md), [Identity A10-01/04](10-collab-identity.md). | Reuse accounts authority where appropriate; preserve consent, namespace isolation, no-follow/private-storage properties and trust policy. Host trust is explicit; do not disable certificate checks or silently widen trust. |
| P07 — System inspection and settings authorities | Processes/descriptors, network/metering, battery/power, Bluetooth, disks, users, firewall, printers, encryption, Secure Boot, displays and device capabilities. | [System apps A04](04-apps-system.md), [Music A02-04](02-apps-media.md), [Shell A06-12/14](06-shell-state.md), [Backends A07-05](07-shell-backends.md), [System A09-05–08](09-system.md), [FS A13-05](13-filesystem.md). | Existing OS services remain authorities. Apps render typed observations and request semantic actions; unknown/unavailable/error stays distinguishable from false/safe/success. |
| P08 — Audio, video, capture and speech | PipeWire/V4L2/GStreamer, native handles/DMA-BUF, capture consent, media sessions, sound playback, TTS and child engines. | [Media apps A02-01/05/06/12](02-apps-media.md), [Chat A03-05](03-apps-communication.md), [Shell A06-13/16](06-shell-state.md), [Backends A07-06](07-shell-backends.md), [System A09-07/12/14](09-system.md), [Media A11-01–06](11-media-browser-text.md). | Portable data/control contracts select optional native implementations. A non-media app does not inherit Linux-only media requirements. Preserve capture privacy, resource lifetime and process cancellation. |
| P09 — Native engines and text services | Chromium helper source/bootstrap/sandbox/build, PDFium loading/lifetime, OCR/Tesseract and Hunspell. | [Browser A03-08/12](03-apps-communication.md), [Productivity A01](01-apps-productivity.md), [Media/text A11-07–09](11-media-browser-text.md), [Tooling A14](14-os-tooling.md). | Engine adapters and build ownership move out of apps with their tests. Apps retain browser/document policy. Native engine sandboxing and helper trust stay enforced; supported host packaging actually includes/discovers helpers. |
| P10 — Workspace filesystem | Mount lifecycle, FUSE/POSIX representations, publication/hydration, xattrs, free-space probes, recipient mounting and workspace resolution. | [Editor A02-10](02-apps-media.md), [Files A04](04-apps-system.md), [Shell A06-09/10](06-shell-state.md), [Media A11-10](11-media-browser-text.md), [FS A13](13-filesystem.md). | Shared namespace/history logic is independent of native mount types. Hydration state is authoritative; existence/open/stat is not proof that remote bytes are local. Retain Linux FUSE deployment as a backend. |
| P11 — Imager and installer closure | Device evidence, raw media authorization, native private storage/cache, endpoints, connected-folder transactions, leases, witnesses and recovery; desktop installation. | [Imager A05](05-imager.md), [Runtime/session A08](08-runtime-packaging.md). | Extract coherent security/recovery units behind optional services. Move-only handles/evidence remain private. Ordinary apps gain neither raw-device dependencies nor installer authority. Preserve disk formats, identity checks, crash recovery and fault tests. |
| P12 — Sandboxes, compatibility and deployment | Landlock/seccomp/bubblewrap, ush, image trust/boot/update, Wine/DXVK, VM appliance and privileged helpers. | [Backends A07-03/04](07-shell-backends.md), [Runtime A08](08-runtime-packaging.md), [System A09-13/15/16](09-system.md), [Identity A10-02/06](10-collab-identity.md), [Other crates A12-02/06–09](12-other-crates.md), [OS A14-05/06](14-os-tooling.md). | Keep these as explicit platform backends, composed through semantic contracts. A missing host equivalent is unavailable, never an unsandboxed fallback. Fedora image scripts remain target-specific. |
| P13 — Build, test and dependency boundaries | Target-gated native libraries/features, Cap'n Proto/native prerequisites, thin runtime closure, helper bundles, host tooling and backend test lanes. | Every report's test/build sections; especially [Runtime A08](08-runtime-packaging.md), [Tools A14](14-os-tooling.md), [Vendor/site A15](15-vendor-site.md). | Portable consumers compile without unrelated native SDKs. Integration tests run where their subject is observable and fail if required observation is absent. Tooling exceptions are documented rather than hidden by broad skips. |

Domain reports are the source of detailed line anchors and conditions. Work packages intentionally cross domain boundaries; track each stable finding ID exactly once even when several packages coordinate on it.

## Migration sequencing

### Phase 1 — Establish contracts without changing authority

Resolve shared ownership for P01–P04 and P06. Preserve existing service interfaces where possible. Define typed availability/errors and keep file descriptors, native handles and unsafe constructors private to backends. Add target-scoped dependencies at the service boundary. Do not create a framework-to-app dependency or use `#[path]` includes back into app source.

### Phase 2 — Migrate low-coupling app callers

Move duplicated directories, launch/reveal, locale and host probes through P02/P05/P07. Keep app policy, UI, domain parsing and CRDT/resource state in apps. Ensure capabilities remain exposed through the existing actions/resource registry, not only click handlers. Preserve persistence migration and permission-granted paths.

### Phase 3 — Move native engines and security-sensitive closures

Migrate media/browser/text backends and their native build ownership through P08/P09. Extract Imager's coherent closures using P01/P03/P04 foundations; do not split evidence-producing and evidence-consuming operations across an unsafe public API. Coordinate FS/identity/relay changes with P10/P11/P12 and retain protocol/on-disk compatibility.

### Phase 4 — Enforce ownership and verify distribution

Introduce an app-source boundary check for OS imports, target conditionals, native FFI, platform command selection and host path constants. Use narrow reviewed exceptions for backend build/package metadata and native integration tests. A forwarding facade with native implementation still in apps does not pass. Test installed bundles on hosts without the source checkout or developer PATH.

These are handoff work packages, not an already-approved exact API design or a claim that every Linux operation must gain a Windows equivalent. Unsupported capabilities should be represented explicitly while supported app functionality remains usable.

## Verification required for implementation

- Run focused behavioral tests for each moved production path, plus affected test-target compilation. Follow repository requirements including `cargo check -p shell --tests --locked` when finishing implementation. Do not regenerate Cargo.lock or set CARGO_TARGET_DIR.
- Exercise supported native Linux, macOS and Windows adapters on those hosts. Record unavailable host validation; cross-compilation is not a substitute for native permission, filesystem, media or device tests.
- Preserve and migrate existing tests for symlink/reparse races, file ownership, locking, crash recovery, fsync/durability, credential namespace, authenticated peers, handle transfer, PID reuse and process-tree cleanup where applicable.
- Verify first-run/upgrade state migration, simultaneous instances, cancellation, unavailable services, permission denial, and installed-package helper discovery.
- Verify absence of unintended native dependencies for minimal app/service builds and optional feature combinations. Keep headless consumers independent of the renderer.
- Never ignore a failing test, weaken assertions, replace production-path tests with tautological helpers or report an unobservable hardware/service check as passing.
- Maintain action/resource accessibility and automation. Use tilo-control for the running desktop; do not add a second control system or screenshot the user's real desktop.

## Evidence and limitations

The audit combines broad lexical searches with contextual review. Categories can
overlap, and a match is not a defect. Generated output, task metadata, private
key material, and large binary assets are outside the scan. Static search can
miss indirect or generated mechanisms, so the reports do not claim exhaustive
platform-dependency discovery.

No application code was changed for this audit, and no builds, tests, hardware operations or application launches were performed. Audit validation checks report completeness and explicit source-reference existence/range, not cross-platform behavior. The detailed reports distinguish source-confirmed mechanics from inferred impact and unvalidated runtime assumptions.

## Detailed reports and finding register

The [finding register](INDEX.md) links all fifteen domain reports and lists the
stable finding IDs. Read the domain findings before implementing a work package;
their acceptance criteria preserve details omitted from this overview.
