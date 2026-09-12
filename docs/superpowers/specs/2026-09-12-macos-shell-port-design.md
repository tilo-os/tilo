# A portable qOS application host for macOS and Linux

Status: proposed specification, revised 2026-09-12 using Superpowers brainstorming.
Repository baseline for the architectural audit: `e4dce157d`.
Scope: architecture and acceptance criteria; implementation is tracked separately.

## 1. Product contract

Port the shell, application framework and runtime so qOS apps run **inside the
shell** on macOS and Linux from the same application source. The shell provides
the workspace and app hosting; the shared SDK translates application requests
into host services. App authors should not maintain separate Mac and Linux UI,
collaboration, persistence or host-integration implementations.

The working assumption is source portability: rebuild the same source for each
OS/CPU. Running existing Linux ELF executables unchanged is outside this design.
This assumption remains explicit because binary compatibility would require a
different architecture.

“Port once” is an acceptance requirement for apps using the supported SDK.
Existing code that bypasses it may need a one-time migration into shared
services. That migration can touch app crates, but must leave one application
implementation used on both platforms. Portable Rust libraries and ordinary
computation do not need to be wrapped merely to satisfy an abstraction rule.

On macOS the shell is one resizable, optionally fullscreen application window
containing qOS app tiles, chrome and workspaces. On Linux it retains its current
native compositor, nested and headless modes. macOS Spaces, login, system lock
and arbitrary host-app windows remain host-owned. Opening an external Mac app
is an auxiliary host operation, not the way a qOS app is presented.

Initial target: Apple Silicon. macOS 13 is a proposed deployment minimum to
validate against the pinned SDK/toolchain and physical machines before release.
Intel support is a separately qualified target.

### First milestone and product scope

The first milestone is **real Notes and Files, with a complete capability map
for every other app**. It hosts `dev.qos.notes` and `dev.qos.file-explorer` from
their actual app crates on both platforms. The framework features in section 6
are part of this milestone, including working
Linux–Mac collaboration; merely opening both app surfaces does not satisfy it.

The capability map is a deliverable of this milestone, not an assertion that
every app already works. It covers every app manifest and names the shared
provider or component-contract gap that blocks each unsupported app. Terminal,
Browser and media apps follow by completing those shared services and verifying
their existing components. Keep their app logic in the common codebase.

The recommended delivery model is one Mac application containing qOS tiles.
“Native” means native execution of qOS components and integration with the host;
it does not require rewriting their widgets in AppKit. macOS and Linux build
the same component definitions against their respective providers. Linux keeps
its compositor and package-execution paths throughout the migration.

## 2. Architectural approaches

| Approach | Benefit | Cost and fit |
| --- | --- | --- |
| **Shared native SDK and application host — recommended** | Reuses Rust widgets, app models, actions, Vello and current collaboration. Platform adapters serve every app. | Requires eliminating direct platform dependencies and completing a common app entry point. Fits the requested product. |
| Portable bytecode/component runtime | Could provide a common executable format and a stronger runtime boundary. | Requires a new execution/FFI model for native dependencies, custom painting and app state. Separate future project, not a prerequisite for this port. |
| Linux guest with app surfaces displayed by the Mac shell | Preserves Linux execution and existing packages. | Retains a Linux environment and introduces guest lifecycle, transport and GPU constraints. Useful only if unchanged Linux binaries become a requirement. |

Choose the first approach. Reuse existing contracts rather than introduce a
parallel Mac SDK. Start with trusted first-party components linked into the
shell; define independent packaged-process hosting as a later extension of the
same application contract. Do not claim that running inside the shell means
all future apps must share its process.

## 3. What the repository provides today

| Existing boundary | What can be reused / what must change |
| --- | --- |
| [`qos-app`](../../../crates/app/qos-app/src/lib.rs) | The application-facing SDK facade already re-exports `ui`. It is the right public boundary, but currently does not prevent OS dependencies elsewhere in an app. |
| [`AppWidget` and `AppHostIntent`](../../../ui/src/widget/app_widget.rs) | In-process tile contract for drawing, input, metadata, commands and host intents. Extend this path instead of creating a Mac widget trait. |
| [`Widget`](../../../ui/src/widget/mod.rs), [`ui::App`](../../../ui/src/app.rs) | Shared widgets and standalone application infrastructure. Separate reusable app construction from ownership of a native window/event loop. |
| [`FreezeThaw` and `View`](../../../ui/src/view.rs) | Existing versioned state/view serialization. AppWidget hosting needs lifecycle wiring; the presence of this trait alone does not prove every embedded app restores correctly. |
| [`runtime`](../../../crates/runtime/runtime/src/lib.rs) and [`runtime-portal`](../../../crates/runtime/runtime-portal/src/lib.rs) | Shared collaboration, files, identity and connected-descriptor plumbing. Keep these APIs; add host providers beneath them where required. |
| [`shell::to_ui_tile`](../../../shell/src/shell/mod.rs) | Embeds some real app widgets, but also shell-owned lightweight Notes/Editor implementations. Those are not substitutes for the full corresponding applications. |
| [`runtime-dylib`](../../../crates/runtime/runtime-dylib/src/lib.rs) | Link-time sharing of Rust runtime/toolkit code. It is not a stable plugin ABI or a cross-process app transport. |
| [`app-host`](../../../crates/packaging/app-host/src/lib.rs) | Package store, registry and control service. Its name does not mean it already hosts widget trees. |
| [`shell::launcher`](../../../shell/src/shell/launcher.rs) | Packaged apps run as processes and their Wayland surfaces are adopted by the Linux shell. That presentation path does not automatically exist on macOS. |

The [existing app-contract design](2026-07-30-app-contract-state-view-design.md)
provides the product model: shared widgets, actions/resources, collaboration,
state/view serialization and accessibility. This specification makes that model
portable and hostable; it does not replace it.

## 4. Responsibility boundaries

```text
qOS application: document model, domain actions, widget tree, local view
                              |
                 qos-app + shared runtime APIs
                              |
       shell app host: instances, workspace, lifecycle, authority
                              |
          shared providers            OS-specific providers
     collaboration, records,       windows/input, secrets, files,
     actions, layout, rendering     dialogs, media, process services
                              |
                  Linux adapter / macOS adapter
```

### Application component

Extract one `ApplicationDefinition` from [`LaunchSpec`](../../../ui/src/launch.rs),
which already owns root factories, arguments and standard startup. `LaunchSpec`
becomes a CLI/startup adapter around that definition; standalone, embedded and
headless runners all consume it. Reuse the existing `AppWidget: Widget` content
contract rather than introduce a Mac widget trait.

The definition contains a stable package `app_id`, display metadata, required
and optional service capabilities, and one fallible factory:
`create(HostContext, LaunchTarget, RestoreInput) -> Result<AppComponent, LaunchError>`.
These names specify the intended interface; they are not claims that these types
exist yet.
`HostContext` supplies instance identity, current lifetime and scoped services;
`LaunchTarget` identifies the requested document/resource or a new document;
`RestoreInput` supplies versioned restoration data. `AppComponent` supplies the
real widget tree, live resources/actions and lifecycle/freeze hooks. It does not
start an event loop. Fixed `AppKind` categories can remain presentation hints,
but cannot serve as catalogue identity or restrict the set of registered apps.

Expose a reusable factory from each application library. The standalone binary,
headless runner and shell registry instantiate that same component. A small
adapter can expose an existing root widget through `AppWidget`; it must not copy
the app's business logic or reimplement a simpler shell-only version. Changes
needed to expose this entry point are shared app-model work, not a macOS branch
of the app.

Identify each catalogue entry by its `qos-app.toml` package ID and one canonical
application definition exported by that app crate. All three runners invoke
that definition. Registration/conformance checks reject duplicate IDs, missing
factories and substitute shell-owned implementations registered under a real
app's ID. A matching title or screenshot is insufficient evidence of app parity.

The host owns mount, focus, resize, suspension, restoration, unmount and service
cancellation. Use stable instance IDs to associate records, events and commands.
Results arriving after an instance closes are discarded or durably completed
according to the operation contract; they cannot mutate a replacement instance.
Register factories through one catalogue used by both platforms rather than
adding OS-specific app lists or hardcoded constructor switches.

The lifecycle sequence is resolve app/resource → validate capabilities → create
instance scope → construct component → restore document and local view → mount.
Only mounted content receives input. Background work captures its instance
lifetime at construction; pre-mount wakes are retained until binding, and
completions never switch their destination to another instance. On close,
stop routing new work, persist its latest recoverable state, cancel outstanding
instance work and detach content. Reopening a persistent instance ID creates a
fresh lifetime token, so an old completion cannot target the new mount. A
construction or restoration failure leaves a retryable error tile and retains
the original persisted data.

| Identity | Owner and meaning |
| --- | --- |
| `app_id` | Catalogue identity of application code, taken from `qos-app.toml`. |
| `record_id` and resource address | Canonical collaborative document and its resources/history, scoped to the workspace. |
| `instance_id` | Durable identity of one tile and its lifecycle; separate from the record it displays. |
| `(device, instance_id)` | Locally persisted View: focus, selection, scroll and other participant-specific presentation. |
| Mount lifetime token | Fresh per construction; invalidated on close and never reused by a replacement mount. |

Two tiles opening one record share its document, but keep independent Views and
operation contexts. Restoration reattaches to the latest canonical record; a
stale frozen instance must not overwrite changes already merged into that record.
The host's record service owns durable shared state; apps retain document schemas
and domain operations. A standalone runner supplies the same service contract.

This requires a shared migration of real
[`NotesApp`](../../../apps/notes/src/app.rs), whose current standalone startup
owns workspace/page savers and disables framework autosave. Embedded startup
must bind those persistence operations to the host record service, with one
writer and an explicit import of existing Notes data. Import preserves source
data and is idempotent; an existing host record is not silently replaced by a
local import. App-local caches or exports may remain derived data. Similarly,
[`FileExplorerWidget`](../../../apps/file-explorer/src/files_app.rs) receives
workspace roots, grants and path policy from its host context instead of
discovering ambient home/FUSE state inside the hosted factory.

Replace the process-global event-loop destination in
[`LocalState` / `WakeHandle`](../../../ui/src/local_state.rs) with host-assigned
wake routing and instance lifetime tracking. A background update from one app
must wake the correct host and cannot resurrect a closed tile. Keep standalone
startup automatic; apps should not install their own platform event-loop proxy.

### Shared host and services

Keep shell-only coordination in `shell`; place reusable service interfaces and
providers below the SDK so standalone apps can use them too. Dependency direction
is apps → SDK/runtime and shell → SDK/runtime. Apps and `ui` must not depend on
`shell` to request a service. Avoid turning the existing client-only `runtime`
umbrella into the compositor implementation.

Host service contexts carry app instance, workspace/resource scope, capability
handles and cancellation lifetime. In-process calls and future RPC calls must
observe the same semantics. Continue using existing typed runtime errors;
add distinctions where necessary for unsupported operations, denied/revoked
permission, user cancellation, transient unavailability and version mismatch.

A capability query drives UI availability but never grants authority. Validate
scope again at execution. Long operations return asynchronously with bounded
queues; coalesce replaceable previews, never durable document mutations. Shared
code must not carry AppKit objects, Smithay objects, DRM handles or Linux paths
as portable service results.

Trusted embedded components share the shell's address space and failure domain.
Service scoping prevents accidental cross-app operations but is not a security
sandbox against malicious native code. Persist state independently of orderly
shutdown; do not promise isolation from crashes or memory corruption here.

### Packaging and process separation

The first delivery statically links the supported first-party component
factories using one toolchain/lockfile per build. This avoids inventing a stable
Rust trait-object ABI. Optional runtime dynamic linking remains a build/package
optimization and requires matching toolchain/runtime artifacts.

Independently installed native apps remain a separate hosting milestone.
`AppWidget` contains in-process Rust objects, so it cannot simply cross a Unix
socket or be loaded from arbitrary `.dylib` files. A future SDK-owned child host
must translate app presentation/input and lifecycle through a versioned bridge,
including accessibility, IME, focus, resize, frame ownership, backpressure and
crash recovery. Apps would still supply the same component factory.

That bridge and its confinement model need a dedicated design before package
execution is advertised on Mac. Existing Linux package launch and Wayland
integration remain supported throughout. This first specification establishes
the component/service contract; it does not silently replace Linux's isolated
apps with in-process loading.

## 5. Framework services to provide centrally

| Service | Shared application contract | Provider responsibilities |
| --- | --- | --- |
| Presentation and input | Widget layout/paint, focus, commands, IME, cursor and drag/drop. | Shell supplies the tile viewport; winit/AppKit or Linux input feeds the same widget contract. |
| Documents and files | Open/import/export, durable records, workspace file access and file grants. | Keep `runtime-fs` semantics; separate its service from the Linux FUSE mount. Resolve host paths and external file grants locally. |
| Clipboard and host dialogs | Text/images, file selection, save destinations and URL opening. | Reuse common clipboard helpers; route portal/native differences beneath the API. |
| Identity and collaboration | App-scoped identity, channels, actions, presence, invitations and roles. | Shared broker/protocol; platform secret storage and authenticated host bindings. |
| State and history | Document serialization, per-user View, undo/redo and resource history. | Host saves/restores component state and integrates common actions; apps retain domain semantics. |
| Web content | Existing `WebViewEngine` contract. | Supply an actual macOS engine behind the shared widget; preserve the Linux engine. Test clipping, scrolling, focus, IME and capture, not only page loading. |
| Media | Shared playback/capture/encoding interfaces. | Platform decoder/audio/capture providers with typed support and permission results. |
| Terminal/process tools | PTY lifecycle, resize, process exit and authorized tool launch. | Use portable facilities where possible; move Linux policy/probing and opener commands out of the terminal UI. Commands available on the host may still differ. |
| Accessibility and notifications | Shared semantic tree/actions, announcements and notification requests. | Window accessibility adapter and native notification delivery; retain Linux compositor publication. |

Start by making the baseline document-app contract complete: embedding,
input/IME, clipboard, file access, actions, accessibility, persistence and
collaboration. Then qualify service families such as terminal, browser and
media using the same app source. A live browser is not complete while backed
by the current deterministic test engine.

Concrete migration examples from the current tree:

- [`apps/terminal/src/main_body.rs`](../../../apps/terminal/src/main_body.rs)
  contains Linux process/sandbox probes and host-opener behavior. Move these
  behind shared process/opener services; preserve Linux enforcement.
- [`apps/file-explorer/src/fileops.rs`](../../../apps/file-explorer/src/fileops.rs)
  and [`model.rs`](../../../apps/file-explorer/src/model.rs) know about host
  trash, metadata and filesystem details. Centralize operations whose semantics
  differ by OS; preserve the Files app's domain actions and view.
- [`ui::webview`](../../../ui/src/webview/mod.rs) and
  [`media`](../../../crates/media/src/lib.rs) already provide useful provider
  boundaries. Extend them rather than put Mac engine selection in Browser or
  individual video applications.

Reuse [`ClipboardBackend`](../../../ui/src/clipboard.rs), the file chooser's
existing asynchronous outcomes, and Files' injected document-opener hook.
For file authority, prefer granted handles/resource IDs with display names over
bare path strings. A host provider resolves those handles and enforces their
scope; the app does not infer access rights from a displayed path.

Native child views require special host integration. The existing
[`NativeViewHost`](../../../ui/src/widget/native_view.rs) carries a real parent
window. For embedded components, the provider must honor tile clipping, overlay
ordering, focus, accessibility and hide/unmount lifetimes. That window reference
cannot cross a packaged-process bridge; a later process host needs an SDK-owned
cooperative/offscreen provider or an explicitly designed native-view transport.

Qualification follows services, not a separate port project for every app.
Maintain a complete fleet inventory: component entry point, required service
families, direct OS dependencies and conformance status. A claimed supported
app must use its real application component and pass its primary user flows.
Use every `apps/*/qos-app.toml` package ID in the source tree as the inventory's
source of truth. Record the audited baseline and include additions through
delivery; a manifest missing from the inventory fails CI. Each row is supported
or explicitly blocked with a reason and a tracked follow-up. Every supported
row names its canonical app definition; a blocked row remains discoverable
without constructing substitute content.

For each app, record the applicable framework capabilities from section 6 and
their results on both hosts: actions/resources, collaboration, presence/following,
restoration, offline recovery and accessibility. "Supported" requires those
gates as well as primary flows. Mark a capability not applicable only when the
app has no corresponding feature; a missing Mac provider is a blocker, not an
exemption from conformance.

The first release is not “all apps supported” until that inventory proves it.
Unsupported hardware/system operations remain explicit capabilities; preserve
unavailable app documents and placement rather than deleting their records.

## 6. Features supplied by the host from the start

These are acceptance requirements for the Notes/Files milestone. Preserve the
existing app-contract pillars—shared widgets, resources/actions, collaboration,
serialization and accessibility—through the common component boundary.

| Feature | Required user-visible behavior |
| --- | --- |
| **Tiled, persistent workspaces** | Open, focus, arrange and close real app instances. Restore documents, tile arrangement and each participant's local view after restart. Two Notes instances retain independent focus, undo context and background updates. |
| **Unified actions and resource discovery** | The shell palette, GUI, CLI and agents discover and invoke the same authorized app/resource actions. A palette invocation targets the selected instance and produces the same result and undo boundary as the equivalent app interaction. |
| **Collaboration and identity** | A fresh Mac profile enrolls without prior Linux setup. Mac and Linux participants exchange real document edits through the existing authenticated, encrypted protocol, with invitations, roles and revocation enforced. |
| **Presence and explicit following** | Show collaborators' presence and allow follow/unfollow. Following never merges another participant's view into the local view or turns a remote focus update into document mutation. |
| **Shared activity and history** | Reuse app history and the shell's workspace activity surface. Opening an app's activity request selects its actual resource/version. Preserve domain undo/redo semantics and do not manufacture duplicate history entries at the host boundary. |
| **Offline persistence and recovery** | Local document edits survive restart and reconcile after reconnect. Corrupt records are preserved with a recoverable error; failed services expose their state. Operations that need a remote system report that requirement. |
| **File and clipboard integration** | Files performs its supported create, rename, copy, move and trash flows through shared providers. Native file selection, clipboard and drag/drop retain grants and instance scope. Open-in-app routing uses the catalogue and reports unsupported document types. |
| **Accessible, consistent interaction** | Shared widgets retain theme and scale settings, keyboard navigation and semantic actions. Support Command/Control conventions, IME and VoiceOver on Mac while retaining Linux accessibility. Theme and font policy belongs to the framework. |

The host reuses the existing `qos-actions` dispatch and live `Widget::resources()`
model. Portable hosting discovers commands from that resource model and resolves
them again at invocation time. Migrate legacy `AppCommand` contributions to
resource operations; the portable catalogue does not execute their direct
Automerge callbacks as a parallel command path. GUI, palette and automation all
observe the existing operation kinds:

| Operation kind | Required behavior |
| --- | --- |
| Document | An authorized domain mutation enters the shared document with the existing action's undo/history boundary exactly once. |
| View | A local presentation change does not enter document history or another participant's View. Follow/Reveal remains explicit. |
| Effect | A reauthorized host/broker operation reports progress and a terminal outcome. It is not document undo and never replays because an instance is restored. |

Do not create a second Mac command system or a second collaboration document
merely to host an app.

For Files, shared records and collaborative metadata remain distinct from
machine-local paths and grants. Collaborating on a resource does not copy the
owner's filesystem authority to another participant. Unsupported or denied file
access remains an explicit result without discarding the shared resource.

State restoration must not replay external effects. In particular, terminal
CRDT state describes its screen projection; thawing it does not restart a
process or re-execute commands. Reconnect only to a valid host-owned session,
or report that a new session is needed. Missing required terminal services must
fail explicitly rather than silently launching an unconstrained host shell.

OS administration, arbitrary host-window embedding, screen sharing, a new app
store and independently installed Mac packages are later capabilities. Web and
media provider support is required before claiming their dependent apps work;
deferring a provider cannot be hidden behind a fake-success stub.

## 7. Shared rendering with separate platform runners

Reuse `WorkspaceTree`, shell chrome, actions and `FrameBuilder` as the scene
producer. Retain Linux's compositor runner and add a macOS application runner.
Legacy `--mode hosted` is useful reference code, not proof of production parity:
it has separate orchestration, and `main.rs::prepare_identity` currently refuses
its first identity creation.

Before the extraction, split presentation snapshots from Linux-owned objects:

- [`compositor/frame.rs`](../../../shell/src/compositor/frame.rs) must consume
  neutral geometry/image/transfer data instead of Smithay-owned floating-window
  types; move the shared OSK identifier out of compositor state.
- [`compositor/foreign.rs`](../../../shell/src/compositor/foreign.rs) must separate
  image storage/widgets from Wayland buffer import, DMA-BUF synchronization and
  client authority. A displayed image does not confer input authority.
- [`backend/mod.rs`](../../../shell/src/compositor/backend/mod.rs) must stop
  naming DRM's gesture action type from a shared event enum.

On macOS, winit `EventLoop::run_app` drives an `ApplicationHandler` on the main thread.
Process redraw and lifecycle callbacks in place; networking workers wake it
through `EventLoopProxy`. The existing Linux backend pumps and buffers events,
which cannot be adopted unchanged: the
[vendored winit guidance](../../../vendor/winit/src/platform/pump_events.rs)
requires callback-synchronous handling of important Mac window events.

Reuse [`ui::gpu::RenderContext`](../../../ui/src/gpu.rs), Vello and wgpu Metal.
The current wgpu version supports Metal; repository shader changes still need
real Metal validation. [wgpu 29 documentation](https://docs.rs/wgpu/29.0.0/wgpu/)

Render the shared scene into a retained texture, then GPU-blit valid output to
the acquired window surface. A new swapchain image cannot be assumed to contain
the previous frame. Keep production overflow/status checks and bounded renderer
recovery. Normal Mac presentation does not read framebuffer pixels back to the
CPU or perform the Linux dumb-buffer XRGB conversion.

Layout uses logical coordinates; damage uses physical target pixels. Convert
conservatively and invalidate history on scale, extent, format or device changes.
Handle zero-size windows, surface/device loss, Retina/external display moves,
fullscreen, occlusion and sleep/wake. Begin with SDR and on-demand rendering.
Preserve the bounded retirement/backpressure contract in
[`present.rs`](../../../shell/src/compositor/present.rs); keep Linux's render
thread. Validate Mac callback-time presentation before moving its work to a
separate render thread.

Reuse [`ui::platform`](../../../ui/src/platform.rs) for shortcut conventions.
Use the shared semantic tree with the Mac window accessibility adapter; retain
Linux's direct compositor publisher. Controlled font fixtures and theme/scale
settings are required for reproducible rendering tests.

Headless execution remains a first-class runner of this same host contract.
Preserve the current Linux CLI, action schema, semantic inspection, output files
and failure exit behavior. Choosing headless mode must not initialize an AppKit
window, require a logged-in desktop or consult the user's interactive profile.
Semantic-only execution and offscreen GPU rendering retain their distinct
requirements: the latter still needs a working graphics adapter. Test both in
the Fedora Parallels VM using isolated state and the production app factories.

## 8. Platform services, authority and storage

Give shell, SDK and service providers one injectable path policy. Preserve
Linux paths and explicit test overrides. On Mac, use user-scoped Application
Support and Caches locations, private runtime endpoints and a single writer per
profile. Benchmarks and tests must not load the user's real state or secrets.

Implement the existing
[`SecretProvider`](../../../crates/runtime/runtime-identity/src/secure_store.rs)
with Keychain Services, a distinct backend ID and accurate protection reporting.
Keep Ed25519/FROST formats; Keychain storage is not a claim of Secure Enclave
signing. Denial leaves identity access visibly unavailable and retryable without
creating a replacement identity or silently weakening storage.
[Apple Keychain Services](https://developer.apple.com/documentation/security/keychain-services)

Separate user identity enrollment from Linux machine first boot. Keep signing
keys with the host broker and reuse the same app-scoped authorization checks.
Unix RPC can be shared, but Linux `/proc` process identity and peer-credential
checks require platform implementations before granting child processes access.
Protect against PID reuse and descriptor substitution; claimed app IDs and
environment variables are not authority.

Local file grants, executable paths, Keychain references and host capability
availability do not become global facts in the collaborative document. Preserve
existing resource IDs and wire formats. For workspace files, extract a portable
service backend; the Linux FUSE projection remains an additional interface.

macOS native-code confinement is not provided by in-process service contexts.
Apple App Sandbox is entitlement-based; signing/Hardened Runtime alone does not
supply Linux's per-app bwrap/Landlock/seccomp guarantees. Any future untrusted
package runner needs its own containment design and negative access tests.
[Apple App Sandbox](https://developer.apple.com/documentation/security/app-sandbox)

Request host permissions when the corresponding feature is invoked. The core
workspace needs neither screen capture nor cross-application control permission.
A later capture provider can use ScreenCaptureKit, with denial/revocation and
qOS sharing authorization checked independently.
[Apple ScreenCaptureKit](https://developer.apple.com/documentation/screencapturekit)

## 9. Build and distribution rules

Keep `qos-app`, shared `ui`, runtime schemas and portable app logic in the common
build graph. Use target-specific dependencies and module gates for Linux and
macOS implementations; features select capabilities, not the operating system.
A runtime `cfg!` branch does not remove unavailable APIs from typechecking.

Concrete initial blockers:

- [`.cargo/config.toml`](../../../.cargo/config.toml) applies ELF flags by CPU
  architecture and forces global Linux C/C++ flags. Scope Rust and native-code
  flags by OS and architecture; preserve the current Linux hardening floor.
- [`shell/Cargo.toml`](../../../shell/Cargo.toml) and its module roots pull Linux
  services unconditionally. Gate both dependencies and code, including examples,
  binaries and test modules. Keep Linux defaults and HDR behavior.
- [`runtime-identity/src/backup.rs`](../../../crates/runtime/runtime-identity/src/backup.rs)
  unconditionally uses `SYS_renameat2` / `RENAME_NOREPLACE`. Supply portable
  exclusive publication with the same no-overwrite/crash-safety contract.
- Transitive font discovery, media, process tools and build scripts need target
  audits. Keep resolver 2 and inspect `cargo tree --target aarch64-apple-darwin
  -p shell -e features`, then perform native builds of the real supported set.

Preserve Linux CLI defaults and commands. Add `--mode application`, default on
Mac, to select the common component host. Keep installation context separate
from run mode; a Mac application is not a full qOS installation. Reject explicit
incompatible machine/session options clearly. Linux application mode can test
host-contract parity without changing its production compositor scheduler.

Distribute a signed `qOS.app` with its component catalogue, resources and native
provider dependencies. It must launch from Finder without development library
paths. Use a stable bundle ID, Developer ID signing, Hardened Runtime and
notarization for direct distribution; App Store delivery is separate work.
[Apple notarization guidance](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)

Current package/runtime manifests identify CPU architecture while loader paths
assume Linux. Never select a same-CPU Linux executable on Mac. Future independent
packages need OS/ABI-aware manifests and, when relevant, a matching runtime build
identity. Preserve old Linux artifact meanings. Initial bundled components do
not require a new dynamic package format.

## 10. Workstreams and acceptance gates

This is the architectural spec. Detailed implementation plans should be written
per workstream after review; it is too broad for one undifferentiated task list.

| Workstream | Deliverable | Acceptance gate / dependency |
| --- | --- | --- |
| A. Portable component contract | Shared factory, lifecycle, service context and catalogue; full-app embedding rather than duplicate mini-apps. | The same real app component runs embedded and standalone on Linux; documents, actions and view round-trip. Defines the interface for B and C. |
| B. Platform-free host core | Target-safe build graph, neutral FrameBuilder data, common service interfaces and preserved Linux adapters. | Portable core compiles natively on Mac; relevant Linux tests/builds remain green. |
| C. Mac shell runner | Main-thread winit loop, Metal presentation, tile input, IME and accessibility. | Real app interaction inside the shell, correct Retina resize and retained/full-render equivalence. Uses A/B. |
| D. Baseline framework services | Files/records, clipboard/dialogs, identity, collaboration and recovery providers. | Fresh-user setup; Linux–Mac document/actions/presence; offline restart/reconnect; permission denial and revocation tests. Uses A/B; integrates with C. |
| E. Service-family qualification | Fleet inventory and shared terminal/web/media providers. | Every app claimed as supported passes real flows with unchanged cross-platform app source; unsupported capabilities are explicit. |
| F. Distribution and regression | Clean-host bundle, runtime diagnostics, both-OS CI and GPU baselines. | Finder launch on declared supported OS versions, baseline host contract complete, relevant Linux checks green. |
| Later: independent packages | Versioned child presentation bridge, compatible package selection and confinement. | Separate spec and negative isolation tests before activation on Mac. |

The first useful release requires A–D and F for real Notes and Files, plus the
complete app/service capability map from E. Later service-family releases qualify
Terminal, Browser, media and other apps through the same contract. This sequence
is an initial delivery step toward the entire SDK-conforming app set; it does
not redefine unsupported apps as ported.

Implementation order: establish the component and service boundary with Linux
behavior intact, bring up the Mac host, complete baseline providers, then qualify
additional service families. Do not combine this with an unrelated renderer
upgrade, document schema rewrite or Linux scheduling rewrite.

## 11. Verification and definition of success

Use one framework conformance suite with Linux and Mac providers. Cover:

- Identical app source and shared factory instantiated inside the shell; no
  Mac-specific app UI or duplicate business logic. Check dependency graphs and
  audit direct platform calls, not only source text for `cfg(target_os)`.
- Multiple app instances with independent focus, document scope and service
  contexts; cancellation/close/reopen and late async completions.
- Resource discovery and action execution through GUI and automation with the
  same authority, history and undo behavior.
- State/View freeze/thaw, corrupt/unsupported records, offline edits, crash
  recovery and unavailable-app retention. Do not sync local view into peers.
- Fresh Mac identity setup and Linux–Mac encrypted/authenticated collaboration,
  presence, explicit follow, reconnect, revoked roles and rejected cross-app access.
- Real IME, clipboard, drag/drop, Retina changes, VoiceOver, window lifecycle and
  host dialogs. Mock providers supplement, not replace, native integration tests.
- Provider-family flows: real terminal/PTY, file operations, web content and media
  for every family included in the advertised compatibility matrix.

Exercise two embedded apps with independent background wakes for the first
milestone. Before qualifying Terminal, test thaw/reconnect for duplicate effects
and verify that a required service failure cannot trigger a weaker execution
path. Before qualifying apps with native child views, verify those views beneath
shell overlays with correct clipping, focus and accessibility.

Use a native Apple Silicon CI worker for compilation/portable tests and a
logged-in physical Mac for window, permission, accessibility and GPU qualification.
Keep Linux compositor, nested and headless checks; run physical DRM tests whenever
changes affect its input, presenter or buffer lifetimes. A Mac pass cannot replace
those Linux gates. Keep relevant CI green before integration.

### Required host matrix

Run both hosts from the same git commit and lockfile, with separate native build
directories. Record the selected targets, discovered test counts, outcomes and
any unavailable hardware. A successful compile or a small smoke suite does not
constitute a complete test pass.

| Host | Required coverage |
| --- | --- |
| Fedora Parallels VM | Full workspace membership and formatting checks; workspace tests, all-target checks and example tests from the existing nightly workflow; Linux shell/headless integration and the real app action scenarios. Preserve Linux service and compositor coverage. |
| Native macOS | Every shared SDK/runtime/app test across the workspace, including portable behavior in apps with unavailable host capabilities; all supported targets and examples; Mac provider tests, real embedded-app flows and Metal rendering. Native window, IME and accessibility checks supplement automated headless coverage. |
| Linux hardware runner | Existing hardware-dependent DRM/input/presentation checks whenever the affected boundary requires them. VM graphics results do not stand in for physical scanout. |

Use the repository's nextest process isolation and GPU scheduling rules. Keep
the isolated visual-golden tail so those tests are executed after other GPU
work drains. After target-gating genuine OS-specific dependencies and targets,
run `cargo nextest run --workspace --locked --profile ci` on both hosts. The
non-golden and golden selections must be complementary and exhaustive, without
a separate Mac package exclusion list. Also run the workspace membership guard,
`cargo fmt --all --check`, `cargo check --workspace --locked --all-targets` and
`cargo test --workspace --examples --locked`.

On Fedora ARM64, select the ARM64 Mesa software-Vulkan ICD for the
canonical software suite; record separate results for a hardware/virtual GPU.
Keep Mac rendering expectations explicit about Metal, fonts, scale and pixel
tolerance instead of weakening Linux goldens to make them pass on another backend.

Maintain a target applicability inventory for the complete workspace. A test may
be Linux-only because it exercises a Linux facility; a portable action, document,
collaboration or lifecycle test cannot be excluded just because its current
module also imports Linux APIs. Extract that shared behavior and test it on both
hosts. Inventory every ignored test with prerequisites and a pass/fail/not-applicable
result; run it where applicable, including hardware and external-service probes.
Inspect logs for soft skips such as a missing adapter instead of counting an
early return as coverage. Every required failure remains a delivery blocker.

### Minimum end-to-end walkthrough

This walkthrough establishes an initial working host; the fleet inventory still
governs every additional app advertised as supported.

1. Start a fresh Mac profile, enroll its identity and open the real Notes and
   Files components inside the shell through the shared catalogue. Create a
   Notes record, discover it in Files' workspace resources and open it through
   the registered Notes definition. This uses a record binding, not an invented
   file extension or the shell's lightweight Notes implementation.
2. Open that record in two instances with independent Views. Edit through
   keyboard/IME and invoke a document action through the common palette. Verify
   one mutation/history boundary, appropriate undo behavior and scoped focus
   and background updates. Open the record's activity from Files and verify the
   selected resource/version. Exercise file operations in an explicitly granted
   test directory.
3. Launch the same Notes definition through the shared catalogue inside the
   production Linux shell/compositor and join the same workspace. Verify edits
   and record-scoped presence in both directions, explicit follow/unfollow,
   offline edits and reconnect without replacing either participant's local view.
   Revoke a participant and verify that new mutations are rejected with a visible
   access status; preserve already stored local data according to existing policy.
   A standalone test client or legacy packaged-app path does not satisfy this
   host-parity gate.
4. Quit and reopen the Mac shell. Restore documents, arrangement and local view;
   opening a denied or revoked file grant reports the failure and preserves the
   affected document. Closing a tile rejects its late asynchronous updates.
5. Run the corresponding real app action and persistence scenarios headlessly
   in Fedora without opening a desktop window. Verify resulting state and exit
   status rather than treating a rendered screenshot as proof of functionality.

For GPU validation, keep three clearly labeled measurements:

| Measurement | Boundary |
| --- | --- |
| Shared offscreen shell workload | 1,000 deterministic actions through production `FrameBuilder` and renderer at 3840×2160 physical pixels. Time input through GPU completion, including production recovery; no per-frame framebuffer readback. Run Linux and Metal. |
| Existing Linux `redraw_bench` | Preserve GPU completion, damage-aware readback and XRGB conversion, with current mean ≤25 ms and p99 ≤50 ms gates. It excludes physical scanout. |
| Mac window cadence | Real app host on a named display; record refresh rate, extent, frame delivery/drops and stalls. Submission time is not presentation latency. |

Before timing, compare retained/full renders on each backend with documented
pixel tolerance and controlled fonts. Keep verification pixel readbacks outside
offscreen timing. Record commit, profile, adapter/backend, OS, target extent,
warmup, mean/p50/p95/p99, allocation rates, RSS and tracked GPU memory. Report
fallback adapters and unavailable timestamps explicitly. Set enforced Mac
thresholds from measured baselines on named hardware; investigate against the
actual display's frame budget without promising unmeasured 60/120 Hz performance.

A successful port demonstrates the user-facing invariant: an SDK-conforming app
gets the host's Mac support by rebuilding, while its view, domain actions, document
logic and collaboration implementation remain shared with Linux.
