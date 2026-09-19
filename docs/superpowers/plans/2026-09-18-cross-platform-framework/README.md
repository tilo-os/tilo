# Cross-platform framework migration roadmap

**Status:** implementation in progress; see the [execution evidence](execution.md)
for accepted slices, active migrations and outstanding native qualification.
This is the dependency roadmap, not a claim that all subsystem implementation
plans or backend APIs are already specified. Write and review a separate executable
plan for each implementation unit against refreshed source before editing code.

**Goal:** an application built on the Tilo framework is cross-platform without
application-authored OS adapters, OS branches, backend feature selection or native
packaging recipes. Required functionality works on each supported host.

**The framework contains platform-specific code.** Linux, macOS and Windows
implementations are its responsibility. The public app API expresses operations;
the framework selects and runs the appropriate native implementation. Enforcement
applies to app code and the public API, never to the internal native backends that
make the abstraction work.

## Read first

- [Framework platform contract](framework-contract.md): active ownership,
  authority, capability and certification requirements.
- [Migration execution evidence](execution.md): implementation progress and
  outstanding qualification; it does not certify host support.
- [Audit review and framework findings](review.md).
- [Original audit](../../../audits/platform-extraction-2026-09-18/README.md):
  historical source inventory only; its generic `Unsupported` language cannot
  waive a current package-required feature on a claimed host.
- [Original design](../../specs/2026-09-18-framework-platform-services-design.md).
- [Exact finding-to-task register](finding-register.json): all 194 original IDs
  have one primary closure owner; each entry retains its domain report and source
  references where explicitly written. Grouped/shorthand source references remain
  in the linked domain report; this register is not a replacement for reading it.

## Architecture

```text
App policy, resources/actions, CRDT state, presentation
             |
             v
Tilo app SDK (imported as ui): semantic platform services
             |
   HostContext: scoped clients, permissions, lifetime
             |
             v
Framework service contracts and shared implementations
             |
             +-- Linux implementations / existing Tilo services
             +-- macOS implementations
             +-- Windows implementations
```

The framework is an ownership and dependency boundary, not synonymous with the
renderer directory. Reuse existing atomic-file, runtime-portal, runtime-fs,
accounts, bounded-command, os-actions and media services as framework components.
Their native modules belong below the public contracts. Record ownership
explicitly; leaving app-owned implementation behind a forwarding API does not
qualify. Headless tools use the lower service contracts without UI dependencies.

App Cargo features describe functionality. Host build composition resolves the
backend automatically. Native APIs, `cfg(target_os)`, OS command construction and
raw descriptors are legitimate inside framework backends. They do not belong in
ordinary app implementation or its public service types. Keep portable business
logic, standard formats, crypto and third-party upstream backends in their proper
owners; no wholesale rewrite of platform libraries is proposed.

## Global constraints

- Rust edition 2024; use the pinned toolchain and plain cargo.
- Never set CARGO_TARGET_DIR or share targets between worktrees.
- Do not regenerate Cargo.lock; review only necessary dependency-edge changes.
- Preserve existing checkout changes; implementation gets an isolated base that
  includes the intended current work, not an accidental older clean revision.
- Keep authorization, signed/wire formats, native names, history and CRDT schemas.
- No public unsafe evidence constructors, path-reopen shortcuts or unsandboxed
  fallback when a required protection cannot be provided.
- Actions/resources remain the public automation/accessibility surface.
- Unsupported optional/inherently host-specific capability is legitimate;
  unsupported required functionality on a claimed host is unfinished work.
- No ignored tests, weaker assertions or successful unobserved native checks.
- No publishing, signing, physical disk writes or user-desktop screenshots as part
  of planning. Native implementation tests use explicitly provisioned fixtures.

## Ordering and merge units

Start G01/G02/G03/G04. Put no-new-leaks checks in place before migration, and
run V01/V02 gates on every unit rather than deferring test/distribution work.
Then establish S01/S02, I01 and C01. Low-coupling location/desktop/inspection
callers can migrate as their particular contracts become available. Media,
engine, filesystem and Imager units follow their explicit dependencies.

A row below is a subsystem delivery unit, not necessarily one enormous PR.
Split its executable plan by independently testable contract and caller group.
For a security invariant spanning producer and consumer, migrate that closure
atomically or retain compatible protocol adapters until both sides have moved.
Do not run overlapping source moves in parallel. V01/V02 accompany each unit;
V03 is the final all-host certification gate, not permission to postpone coverage.

| Unit | Deliverable | Prerequisites |
| --- | --- | --- |
| [G01](#g01) | Framework ownership and support contract | None |
| [G02](#g02) | Scoped host service composition | G01 |
| [G03](#g03) | SDK surface and no-new-leaks enforcement | G01 |
| [G04](#g04) | Target and feature dependency gates | G01, G03 |
| [L01](#l01) | Known folders, locale and time zones | G02 |
| [L02](#l02) | Private locations and state migration | L01, S01 |
| [L03](#l03) | Host architecture and executable capability selection | G02 |
| [S01](#s01) | Private and durable filesystem primitives | G01 |
| [S02](#s02) | Cross-process locks and process incarnation | S01 |
| [S03](#s03) | Migrate durable storage consumers | S01, S02, L01 |
| [I01](#i01) | Owned authenticated local connections | G01, S01, S02 |
| [I02](#i02) | Migrate brokers and typed service clients | I01 |
| [I03](#i03) | Control and session activity transport | I01, I02 |
| [C01](#c01) | Managed process trees and tools | G01, S02 |
| [C02](#c02) | Terminal session service | I01, C01 |
| [D01](#d01) | Host-native choose, open and reveal | G02, I01, L01 |
| [D02](#d02) | Application discovery, launch and semantic shortcuts | D01, C01 |
| [D03](#d03) | Trash and owned drag artifacts | D01, S01 |
| [K01](#k01) | Credential custody and scoped secrets | S01, S02, I01, G02 |
| [K02](#k02) | Host trust and SSH identity discovery | K01, L01 |
| [H01](#h01) | Read-only system observations | G02, C01, L01 |
| [H02](#h02) | Hardware and desktop service providers | G02, C01, I01 |
| [H03](#h03) | Firewall and VPN operations | H01, H02, S01 |
| [H04](#h04) | Package, container, model and enrollment administration | H01, C01, I02, K01 |
| [M01](#m01) | Portable media and native-frame contract | G01, G02, G04 |
| [M02](#m02) | Capture and device sessions | M01, H02, I01, C01 |
| [M03](#m03) | Playback, audio graphs and desktop media sessions | M01, C01, H02 |
| [M04](#m04) | Decode, waveform and export jobs | M01, C01 |
| [E01](#e01) | Browser engine, private instances and bundle ownership | M01, C01, S02, D01, K01 |
| [E02](#e02) | One native PDF engine owner | G02, S01, C01 |
| [E03](#e03) | Speech, OCR, spellcheck and model assets | G02, C01, L01 |
| [W01](#w01) | Filesystem semantics independent of FUSE | G01, S01, I01 |
| [W02](#w02) | App workspace clients and hydration | W01, I02, G02 |
| [W03](#w03) | Native projections and filesystem service lifecycle | W01, I02, C01, H01 |
| [N01](#n01) | Imager profile codec and authorized media service | L03, S01, C01, H01 |
| [N02](#n02) | Imager private cache and migration storage | S01, S02, L02 |
| [N03](#n03) | Imager connected-folder transaction closure | N02, I01, C01 |
| [N04](#n04) | Desktop and Bootc installation providers | N01, N02, C01, I02 |
| [B01](#b01) | Sandbox and compositor backend boundaries | G04, I01, C01, S01 |
| [B02](#b02) | Login, boot, update and privileged deployment | H02, C01, I02, S01 |
| [B03](#b03) | Compatibility, VM and projection backends | B01, I02, C01, S01, M01 |
| [V01](#v01) | Portable and native verification suites | G04 |
| [V02](#v02) | Framework distribution and host tooling | G04 |
| [V03](#v03) | Closure audit and final support certification | L02, L03, S03, I03, C02, D02, D03, K02, H03, H04, M02, M03, M04, E01, E02, E03, W02, W03, N03, N04, B02, B03, V01, V02 |

## Granular delivery units

Source ownership below refers to the current checkout. Explicit per-finding source
paths and line anchors are in the register and linked domain report. Refresh them
before drafting the executable plan. The acceptance text is a requirement, not
existing verification evidence.

<a id="g01"></a>

### G01 — Framework ownership and support contract

**Primary audit findings:** A15-01.

**Implementation ownership:** Define the public SDK versus internal backend boundary in crates/app/tilo-app, ui and the existing service crates; record required/optional capabilities for Linux/macOS/Windows and applicable architectures. Services under crates/system/runtime/media can be framework-owned without moving them into the renderer.

**Acceptance:** One reviewed capability/host matrix identifies required implementations, optional absence, existing evidence and release status; every backend has an owner; no app must select an OS implementation.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="g02"></a>

### G02 — Scoped host service composition

**Primary audit findings:** A06-17, A08-02, A13-04.

**Implementation ownership:** Extend ui/src/application.rs::HostContext and ui/src/launch.rs to carry host-selected service clients bound to existing app/workspace/instance permissions and lifetime; integrate native runtime bootstrap in crates/runtime/runtime-portal/src/native.rs.

**Acceptance:** Two instances cannot access each other's grants; revocation invalidates pending operations; close releases owned work; packaged startup never invokes Cargo; invalid inherited authority is rejected.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="g03"></a>

### G03 — SDK surface and no-new-leaks enforcement

**Primary audit findings:** A15-03, A15-04, A15-07.

**Implementation ownership:** Curate crates/app/tilo-app/src/lib.rs exports in migration-sized steps; replace backend-named app features with semantic features; add tools/check_app_platform_contract.py beside the existing resource/action checker. Keep native windowing/GPU adapters and upstream patches inside framework dependencies.

**Acceptance:** Fixture tests reject app OS imports/selection and direct backend dependencies, permit native framework adapters and portable fixture data, and expire exact migrated exceptions; consumer compile tests reject native types through SDK aliases.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="g04"></a>

### G04 — Target and feature dependency gates

**Primary audit findings:** Framework review addition / cross-cutting gate.

**Implementation ownership:** Add target-resolved graph checks and SDK consumer fixtures under crates/app/tilo-app/tests; coordinate .forgejo/workflows/check.yml and lint.yml with native runner configuration.

**Acceptance:** Minimal SDK consumers exclude unrelated FUSE/DRM/media stacks;
semantic browser/media features resolve correct target backends; resolve
build-host tools separately; permit only classified host-composition edges to app
entry points/widgets; reject reusable framework contract/service/backend
dependencies on app-owned mechanisms and native API leaks; assign finding and
removal owners to remaining record-resolution, process-service and drag-state
edges.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="l01"></a>

### L01 — Known folders, locale and time zones

**Primary audit findings:** A01-11, A04-16.

**Implementation ownership:** Extend ui/src/sysdirs.rs and ui/src/i18n.rs using framework host providers; migrate word-processor/src/i18n.rs and settings input_sources/time_date discovery.

**Acceptance:** Redirected/localized folders and native language preferences work without POSIX environment requirements; suggestions confer no grant; unknown current zone stays unknown; old IANA identifiers remain compatible.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="l02"></a>

### L02 — Private locations and state migration

**Primary audit findings:** A01-01, A02-09, A03-06, A04-05, A05-04, A06-08, A08-18, A09-03, A10-05, A12-05.

**Implementation ownership:** Centralize data/state/config/cache/runtime roots using settings-store, app-state and runtime identity seams; migrate app path resolvers and service defaults inventoried in the register.

**Acceptance:** Old profiles/history/journals remain discoverable; concurrent upgrade has one winner; no private cwd/shared-temp fallback; per-user/app/workspace isolation and explicit test roots hold on each host.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="l03"></a>

### L03 — Host architecture and executable capability selection

**Primary audit findings:** A05-02, A08-14.

**Implementation ownership:** Consolidate apps/imager/src/host.rs, crates/packaging/install/src/isa.rs and app-registry/src/index.rs around process/native architecture and usable instruction-set observations.

**Acceptance:** Windows emulation and macOS translated-process fixtures select native architecture correctly; identical usable-feature sets produce identical package selection; unknown is never x86 by default.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="s01"></a>

### S01 — Private and durable filesystem primitives

**Primary audit findings:** A08-03, A08-17, A09-01, A10-01.

**Implementation ownership:** Extend crates/system/atomic-file and crates/runtime/runtime-fs native seams with semantic privacy, bounded no-follow reads, held-directory identity, exclusive creation and durable publication; preserve distinct user-key and machine-key policies.

**Acceptance:** Native Linux/macOS/Windows tests cover owner/ACL, ancestor and leaf links/reparse points, hardlinks, race replacement, create-new contention and post-publication flush failures; missing guarantee refuses before unsafe mutation.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="s02"></a>

### S02 — Cross-process locks and process incarnation

**Primary audit findings:** A08-15, A08-16, A09-02, A10-03.

**Implementation ownership:** Unify app-state/accounts leases and repair packaging install recovery, app-catalogue checkpoints, registry moderation and relay requester lifetime observation.

**Acceptance:** Real concurrent processes cannot recover a live transaction or regress checkpoints; crash releases locks; PID reuse is rejected; unknown observation cannot mean dead; process-local mutex is never accepted as cross-process protection.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="s03"></a>

### S03 — Migrate durable storage consumers

**Primary audit findings:** A01-09, A02-07, A03-07, A04-21, A06-06, A06-10, A07-02, A10-09, A12-03, A13-06.

**Implementation ownership:** Move native storage mechanics from Browser, Tube, PDF, backups, VPN/print, shell identity/nearby transfer, collab persistence and fs subscription/share state into the framework storage APIs.

**Acceptance:** Existing on-disk fixtures round-trip unchanged; production symlink/ownership/collision/restart tests follow backends; PDF pathname staging retains private publication; browser helper and app share the same cleanup lease protocol.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="i01"></a>

### I01 — Owned authenticated local connections

**Primary audit findings:** A08-01.

**Implementation ownership:** Evolve crates/runtime/runtime-portal around owned authenticated connections; keep fd_transport and UnixCapnpClient as backend internals; add host implementations without replacing trusted transport with ambient localhost connections.

**Acceptance:** Malformed/surplus handles close, transferred authority is adopted once, caller identity and incarnation are verified, framing limits hold, children cannot reuse inherited metadata; alternate hosts require native authentication evidence.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="i02"></a>

### I02 — Migrate brokers and typed service clients

**Primary audit findings:** A03-04, A04-02, A06-04, A06-05, A08-04, A10-08, A12-01, A12-04, A12-08, A13-03.

**Implementation ownership:** Move direct sockets/descriptors out of runtime clients, shell runtime host, Files/Agent, model gateway, format host, fs services and compatibility session callers. Retain each existing domain protocol and stronger per-service authorization.

**Acceptance:** Existing RPC round trips and adversarial peer tests pass; endpoint permission failure prevents startup; same-user identity alone cannot substitute for a certified launcher/workspace; portable clients expose no raw descriptor.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="i03"></a>

### I03 — Control and session activity transport

**Primary audit findings:** A01-03, A06-11, A07-01, A14-01.

**Implementation ownership:** Keep tilo-control protocol; migrate its connection layer and scripts/headless-examples/_control.py consumers. Move Clocks publisher/command receiver and shell topbar reader together into a session service.

**Acceptance:** Actions list/invoke/cancel and streaming retain protocol behavior; Clocks producer lifetime and command dedup remain authenticated; draw does no native file/proc I/O; one automation surface remains.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="c01"></a>

### C01 — Managed process trees and tools

**Primary audit findings:** A01-06, A02-12, A09-04, A14-02.

**Implementation ownership:** Extend crates/system/bounded-command with backend-owned tree supervision and streaming jobs; migrate LSP, OCR/tool runners and harness reaper guarantees while keeping domain argv/format policy where appropriate.

**Acceptance:** Controlled descendant helper proves timeout, output limit, cancellation and abrupt-owner-death guarantees separately; pipes unblock, children reap, secrets stay off argv, unrelated processes survive; unavailable required lifetime protection refuses.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="c02"></a>

### C02 — Terminal session service

**Primary audit findings:** A03-02, A03-03, A08-07.

**Implementation ownership:** Move apps/terminal/src/main_body.rs service discovery, shell selection, PTY and confinement into runtime session providers; retain portable-pty and Linux ush as implementations.

**Acceptance:** Same terminal app/emulation drives native resize/read/write/exit; mandatory confined routes never fall back to ambient shell; macOS lacks Linux namespace support unless actually implemented; service and trusted hosted modes stay distinct.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="d01"></a>

### D01 — Host-native choose, open and reveal

**Primary audit findings:** A01-02, A02-02, A03-01, A05-03, A06-01, A08-05.

**Implementation ownership:** Split ui/src/portal.rs semantic requests/delivery from host picker/opener implementations; move Imager native dialogs and app xdg-open/open selection into providers.

**Acceptance:** Same app request produces native chooser and explicit handoff on each supported host; cancel/denied/unavailable/error remain distinct; granted handles are retained; Unicode paths round-trip; no UI-thread blocking or sandbox bypass.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="d02"></a>

### D02 — Application discovery, launch and semantic shortcuts

**Primary audit findings:** A03-11, A04-04, A06-02, A09-09.

**Implementation ownership:** Extend desktop-entry/tool-intent authorities for installed app catalogues and authorized launch; migrate shell search, Files terminal intent and Browser shortcuts; keep Git credential bridge integrated with K01/K02.

**Acceptance:** Catalogue identity and invalidation hold; launch strips unrelated inherited authority; exact granted working directory preserved; semantic command modifier consistent; app submits intent rather than shell strings.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="d03"></a>

### D03 — Trash and owned drag artifacts

**Primary audit findings:** A01-10, A04-03, A15-02.

**Implementation ownership:** Move Files trash/symlink mechanics and word-processor spool heuristics into host providers; add framework-owned temporary artifact lease at ui DnD/vendor-winit handoff.

**Acceptance:** Delete/list/restore/purge refer to the same native item; ordinary winit-dnd-named files are never deleted; genuine spool releases exactly once on completion/cancel; unreadable cross-sandbox source transfers bytes without broad grants.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="k01"></a>

### K01 — Credential custody and scoped secrets

**Primary audit findings:** A01-05, A02-08, A03-09, A08-06, A09-10.

**Implementation ownership:** Reuse accounts vault/SecretCustody and runtime-identity SecretProvider; integrate Browser credential worker/Keychain, TOTP and Tube secret mediation through scoped providers.

**Acceptance:** Stable restart identity and existing ciphertext survive; origin/profile/app namespaces and consent hold; locked/denied never silently downgrade custody; no secrets in CRDT/logs/argv; native keychain tests run on each claimed host.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="k02"></a>

### K02 — Host trust and SSH identity discovery

**Primary audit findings:** A03-10, A09-11.

**Implementation ownership:** Separate tilo-native-certs immutable Fedora trust policy from native host trust loading; migrate Remote known-host private storage and user-key discovery plus authorized Git agent integration.

**Acceptance:** Image trust cannot be environment-redirected; native stores load intended roots; changed SSH host key still needs explicit trust; private trust files satisfy ACL/no-link policy; no unchecked root-user fallback.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="h01"></a>

### H01 — Read-only system observations

**Primary audit findings:** A02-04, A04-10, A04-11, A04-14, A10-07, A13-05.

**Implementation ownership:** Move Settings facts/security/users, Monitor sampling/identity, Logs journal reader and shared network/power/free-space observations into services; retain sysinfo and bounded Linux parsers.

**Acceptance:** Unavailable differs from false/empty; PID reuse blocks signaling; optional fd counts can be unknown; hardware memory is not invented; hydration/compute policy uses unknown conservatively; real host probes have native fixtures/integration evidence.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="h02"></a>

### H02 — Hardware and desktop service providers

**Primary audit findings:** A01-04, A02-03, A04-06, A06-12, A06-14, A07-05, A08-24, A09-05, A09-06, A09-08.

**Implementation ownership:** Extend os-actions Dispatcher and existing BlueZ/power/input/portal traits; move native defaults, Contacts priority notifications, OSK injection and shell hardware selection behind host factories.

**Acceptance:** No trait default dispatches Linux commands on another host; native user authorization, hotplug, pairing lifetime, input revocation and lock-before-suspend hold; optional unsupported devices remain explicit.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="h03"></a>

### H03 — Firewall and VPN operations

**Primary audit findings:** A04-09, A04-13.

**Implementation ownership:** Move apps/firewall/src/backend.rs and VPN native probes/profile storage behind semantic preview/apply/profile telemetry; retain os-actions NetworkManager authority.

**Acceptance:** Explicit apply gates and exact argv preserved; absent engine never succeeds; profile/interface identity is exact; partial failure/unknown counters observable; no unrelated tun-interface fallback.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="h04"></a>

### H04 — Package, container, model and enrollment administration

**Primary audit findings:** A04-12, A04-18, A04-19, A06-07, A08-11, A10-04, A12-02.

**Implementation ownership:** Use app-host snapshots and existing model ProcessSupervisor/LocalModelControl, container backend and relay privileged broker to replace app helper paths, scripts, systemctl and Flatpak probes.

**Acceptance:** Discovery and mutations use same authority; fixed privileged operation vocabulary, caller lifetime and granted inputs retained; cancellations reap helpers; missing tools explicit; model capacity measured on the selected store volume.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="m01"></a>

### M01 — Portable media and native-frame contract

**Primary audit findings:** A06-16, A10-10, A11-03, A11-04.

**Implementation ownership:** Refine crates/media/core engine/frame APIs and ui import integration: opaque native image ownership, negotiation and accounting; hide gst::Element, DMA-BUF and decoder factory details from ordinary apps.

**Acceptance:** CPU frame fallback preserves color/format; Linux zero-copy identity/modifiers/lifetime tests stay valid; other hosts never import Linux DMA-BUF merely because they are Unix; native codec wire behavior stays unchanged.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="m02"></a>

### M02 — Capture and device sessions

**Primary audit findings:** A02-01, A03-05, A06-13, A07-06, A09-07, A09-14.

**Implementation ownership:** Move Camera/Chat and shell remote capture/audio concrete implementations into providers with owned consent-bound sessions and bounded streams.

**Acceptance:** Real selected camera/mic/screen sources produce frames/audio; portal remains alive until capture ends; denial/revocation stops resources; no environment-variable availability guess; shots never open real devices.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="m03"></a>

### M03 — Playback, audio graphs and desktop media sessions

**Primary audit findings:** A02-05, A02-11, A04-15, A11-01, A11-02, A11-06.

**Implementation ownership:** Consolidate Music playback/MPRIS, shared audio_runtime, Settings audio and event sounds into media providers; native IDs become opaque handles and node roles.

**Acceptance:** Load/seek/gain/default-device controls retain behavior; accessibility transform restores previous graph on close; no unintended mic monitor; live output proof differs from fake sink; now-playing commands work through one semantic interface.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="m04"></a>

### M04 — Decode, waveform and export jobs

**Primary audit findings:** A02-06, A11-05.

**Implementation ownership:** Move Video Editor native audio/waveform/export implementations and GStreamer tuning into shared media backend jobs; retain timeline/project transforms and codec preferences in app.

**Acceptance:** Generated clips prove waveform, accurate seek and audio/video export fidelity; codec unavailable and cancellation are explicit; reported acceleration is observed, not inferred from plugin names.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="e01"></a>

### E01 — Browser engine, private instances and bundle ownership

**Primary audit findings:** A03-08, A03-13, A14-07.

**Implementation ownership:** Relocate apps/browser/chromium with CMake, resources and native tests under framework browser ownership; migrate scripts/browser/build-host.py; move WPE composition/environment and private-instance supervision out of apps.

**Acceptance:** Existing helper IPC, nested bundle, input/download/private-storage and secure-form tests pass; sandbox bootstrap and stripped capabilities remain; installed browser launches without checkout/developer PATH on each claimed host.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="e02"></a>

### E02 — One native PDF engine owner

**Primary audit findings:** A01-08, A11-07.

**Implementation ownership:** Consolidate apps/pdf-viewer/src/source.rs and crates/media/thumbnail/src/pdf.rs native loader/lifetime behind framework document engine ownership; preserve portable PDF domain processing.

**Acceptance:** Viewer/thumbnail concurrency uses one lifetime strategy; packaged native discovery works by target ABI; real page rasterization/edit/save tested; text-only fallback is not counted as rasterizer proof.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="e03"></a>

### E03 — Speech, OCR, spellcheck and model assets

**Primary audit findings:** A04-07, A09-12, A11-08, A11-09.

**Implementation ownership:** Move TTS Unix factories, Tesseract/Hunspell execution and OSK model lookup behind existing speaker/text/media seams; retain built-in portable algorithms.

**Acceptance:** Native supported speech actually audible/observed; stop/priority and worker timeout/restart hold; language inventory truthful; OCR geometry and output bounds unchanged; minimal non-native consumers compile.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="w01"></a>

### W01 — Filesystem semantics independent of FUSE

**Primary audit findings:** A13-02.

**Implementation ownership:** Separate fs/src/namespace.rs and production history/access/restore operations from fuser representations; keep collab_types filenames, records, ACL and domain outcomes.

**Acceptance:** Identical checked-restore/history/access outcomes through semantic API and Linux mount; raw names preserved; hidden names stay hidden; no fuser types/errno in public semantic contracts.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="w02"></a>

### W02 — App workspace clients and hydration

**Primary audit findings:** A01-07, A02-10, A04-01, A11-10.

**Implementation ownership:** Migrate Files xattr/history/sharing, Ebook workspace classification, Video Editor and media::Source resolution to authorized workspace/resource clients.

**Acceptance:** Projected metadata is not hydration; foreground read hydrates; stale cursor/identity, checked operations and ACL handling preserved; same app works without /var/workspace or xattr knowledge.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="w03"></a>

### W03 — Native projections and filesystem service lifecycle

**Primary audit findings:** A06-09, A08-12, A13-01, A13-07.

**Implementation ownership:** Retain Linux FUSE/app-fs as backend; isolate mount/unmount, recipient projection subprocess, publisher helper and service activation from reusable filesystem logic.

**Acceptance:** Actual kernel mount suite retains owner/read-only policy, revoke/unmount cleanup and readiness; hosted semantic access does not require a mount; no projection advertises ready when unsupported; bearer data stays off argv.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="n01"></a>

### N01 — Imager profile codec and authorized media service

**Primary audit findings:** A04-08, A05-01, A05-10.

**Implementation ownership:** Extract profile model/codec normally from apps/imager/profile-tool path includes; move Imager and Disks inventory/raw writer native operations into optional framework installer/media services.

**Acceptance:** App and standalone codec share production format tests without GUI closure; UI/helper confirmation agrees; stale/system/live-device writes refused; native host test uses safe provisioned media and no mutation in shots.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="n02"></a>

### N02 — Imager private cache and migration storage

**Primary audit findings:** A05-06, A05-07.

**Implementation ownership:** Move setup/destination_security, nofollow, migration storage/journals and internal_cache providers with required sealed types/tests; preserve held-handle bundle validation.

**Acceptance:** Existing checkpoint/journal bytes remain valid; Linux/macOS/Windows guarantee differences explicit; collision/ACL/reparse/race/disk-full/resume tests exercise moved code; never claim durability beyond evidence.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="n03"></a>

### N03 — Imager connected-folder transaction closure

**Primary audit findings:** A05-08, A05-09, A08-22.

**Implementation ownership:** Move setup/connection endpoint/model/reconcile/store/transaction and native witness/lease receiver with necessary migration_source/native journal and NTFS broker contracts as a coherent service.

**Acceptance:** No framework-to-app imports or public evidence constructors; Linux lease-break/helper-death/inotify-overflow/crash recovery tests remain; Windows read/store availability is distinct from transaction execution; helpers preserve confinement.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="n04"></a>

### N04 — Desktop and Bootc installation providers

**Primary audit findings:** A05-05, A08-21.

**Implementation ownership:** Move Imager desktop probe/install/continuation/worker and session installer OS effects to deployment providers; keep UI/product choices app-side.

**Acceptance:** Pinned package/device identity, authenticated continuation, destructive confirmation and cancel/reap remain; Linux installation tests use isolated images; other hosts do not invoke Linux installers merely to satisfy an API.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="b01"></a>

### B01 — Sandbox and compositor backend boundaries

**Primary audit findings:** A06-03, A07-03, A07-04, A08-08, A08-09, A08-10, A10-06.

**Implementation ownership:** Retain Wayland/DRM and launcher/ush Linux enforcement as framework/platform backends; isolate shared launch/compute policy and eliminate duplicate unused enforcement APIs only after caller verification.

**Acceptance:** Production confinement/netgate/FD inheritance tests pass; native hosted mode cannot be a sandbox failure fallback; compute pinned output collection survives swaps; minimal portable clients avoid native compositor closure.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="b02"></a>

### B02 — Login, boot, update and privileged deployment

**Primary audit findings:** A04-17, A08-19, A08-20, A09-15, A10-02, A14-05.

**Implementation ownership:** Move UI invocation of greetd/PAM/Bootc/systemd and identity enrollment mechanics behind fixed semantic operations; retain OS helpers, units, image/trust and signing tooling as target deployment owners.

**Acceptance:** Wrong password/helper crash cannot unlock; update/rollback formats and monotonic trust preserved; service restore works on failure; Linux boot/privilege tests stay mandatory; no arbitrary privileged exec API.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="b03"></a>

### B03 — Compatibility, VM and projection backends

**Primary audit findings:** A04-20, A06-15, A09-13, A12-06, A12-07, A12-09.

**Implementation ownership:** Keep Wine/FEX/DXVK/libvirt/RAIL and private audio implementation as explicit Linux framework backends; expose existing registry/executor/delegate/resource contracts and migrate Windows app/shell callers.

**Acceptance:** Explain/catalogue policy independent of Linux; exact file/audio/network grants and guest trust maintained; detached descendants reap without affecting unrelated children; Windows guest support never labeled Windows host support.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="v01"></a>

### V01 — Portable and native verification suites

**Primary audit findings:** A01-12, A02-13, A04-22, A05-12, A06-18, A07-08, A10-11, A11-11, A13-08, A14-04.

**Implementation ownership:** For every extraction, move native tests with backend, retain app-level action/resource tests, split fuzz targets and remove success-on-unobserved-subject paths without weakening assertions.

**Acceptance:** Required native lane fails missing prerequisites; portable fixture tests never counted as real device/security proof; all affected test targets compile; final cargo check -p shell --tests --locked runs.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="v02"></a>

### V02 — Framework distribution and host tooling

**Primary audit findings:** A01-13, A02-14, A03-12, A05-11, A07-07, A08-13, A08-23, A09-16, A12-10, A14-03, A14-06, A15-05, A15-06.

**Implementation ownership:** Centralize helper/bundle/native dependency ownership, semantic app packaging inputs and target prerequisites; retain Fedora/PE/Mach-O/ELF release mechanics and approved vendor tooling at explicit platform owners.

**Acceptance:** Installed artifacts on clean host locate helpers/SDKs/assets; no developer PATH or build-on-launch; native target+ABI correct; sandbox/signing rules unchanged; canonical install/release metadata distinguishes published support.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

<a id="v03"></a>

### V03 — Closure audit and final support certification

**Primary audit findings:** Framework review addition / cross-cutting gate.

**Implementation ownership:** Close each finding with source diff and evidence; remove migration exceptions; compile identical app sources and semantic features under supported target profiles; verify distribution/action parity.

**Acceptance:** All 194 audit dispositions accounted for plus framework findings; no app-native implementation or backend selection; required host matrix fully passes; unavailable native tests remain outstanding and cannot be reported complete.

- [ ] Refresh source and design the exact contract/compatibility change.
- [ ] Write the executable subsystem plan, with concrete tests and file changes.
- [ ] Implement and migrate all callers assigned to this unit.
- [ ] Record native/portable evidence and remove the unit's boundary exceptions.

## Executable-plan requirements

Each subsystem plan must name exact created/modified/test files, input/output API
signatures, compatibility wrappers and removal points, red/green regression test
code for behavioral changes, native runner prerequisites, packaging changes and
rollback behavior. Preserve already adequate behavioral tests for pure moves.
Do not create dummy tests merely to prove source relocation.

A plan must explicitly describe authentication/permission checks, ownership and
cancellation, error mapping, persistence/wire migration and required capabilities.
These details cannot be inferred from a trait name. Before beginning any subsystem,
read all its assigned domain findings and the supporting producers/consumers from
other reports. One primary owner closes each ID; supporting tasks supply evidence.

Close a finding only with its production code location, migrated callers/tests,
app-source/API/dependency checks and required host evidence. A retained backend or
tool can close with reviewed ownership/dependency and applicable native tests;
retention never means that calling apps may still implement its mechanism.

## Verification matrix and completion

For each declared host/architecture and capability, record separately:

1. Source/API/dependency isolation and applicable test-target compilation.
2. Portable policy/behavior tests through production app dispatch.
3. Native permission, persistence, lifecycle, device or engine conformance.
4. Installed artifact tests without checkout/developer PATH.
5. Release availability, which only changes after actual publication validation.

Linux full-desktop/Fedora-image certification remains additional to hosted-app
certification. Hosted macOS/Windows apps need not implement Linux kernel APIs;
they must implement required semantic functionality with their native mechanisms.
No-default-features compilation alone cannot certify the feature-rich app users
receive. Compile shared semantic features on all supported hosts and keep separate
native backend feature combinations in framework CI.

Run focused package tests and affected test-target checks per unit. Run the
repository-required `cargo check -p shell --tests --locked` at implementation
completion. Record missing host infrastructure as outstanding validation. This
planning review has not run those application builds or tests.

Additional source discoveries remain in scope alongside the original194 findings. [X01: Settings account mutation recovery](account-mutation-recovery-recon.md) is assigned to S03; the register tracks it separately so original audit IDs/counts remain intact.
