# Framework platform contract

Status: implementation contract for the active migration. Native support is not
certified by this document. The audit register and evidence ledger remain the
record of implementation progress.

## Ownership and direction

Applications depend on `tilo-app`, conventionally renamed `ui`. Its public API
offers semantic services; it does not offer OS-specific factories, handles or
Cargo feature names. The framework contains the native implementations and
selects them during host construction. Changing the host must not require
changing application Rust source or semantic feature declarations.

The framework includes the SDK, `ui` integration and focused headless services.
Existing service directories need not move merely to establish ownership:

| Responsibility | Existing owner to extend | Host implementation belongs here |
| --- | --- | --- |
| durable storage and locking | `crates/system/atomic-file`, `crates/runtime/runtime-fs` | private native storage modules and owned leases |
| application persistence | `crates/system/app-state`, `settings-store` | location/storage adapters; CRDT/schema logic stays shared |
| local service attachment | `crates/runtime/runtime-portal` | Unix/Windows transport, native bootstrap and handle transfer |
| identity and credentials | `runtime-identity`, `accounts`, identity service | native key custody, secure storage, privileged broker adapters |
| child lifecycle | `crates/system/bounded-command` | OS containment, inherited handles, cancellation/reaping |
| host actions and observation | `crates/system/os-actions`, power and Bluetooth services | host-specific commands, APIs, parsers, subscriptions |
| media | `crates/media/core` and framework renderer integration | native stream/device/codec/image import implementations |
| browser/document engines | framework webview and narrow browser/document engine owners | native engine sources, bootstrap, build/assets and tests |
| resource filesystem | `crates/runtime/runtime-fs`, `fs` shared core | native projection, names/identity, FUSE/xattr transport |
| deployment and installation | launcher/session services and extracted Imager services | privileged installer/media, confinement, boot/update backends |
| host integration | framework portal/sysdirs/DnD and desktop services | chooser, open/reveal, trash, locale, native artifact leases |

Dependencies run from apps to SDK/contracts to implementations. A host
composition root may depend narrowly on app entry points and widgets to assemble
the desktop. Reusable framework contracts, services and backends do not depend on
app-owned mechanisms. Every remaining host-to-app edge is classified; app-owned
record resolution, process services and drag state retain finding and removal
owners until extracted. Host composition is not a blanket exception. Core
service contracts do not depend on the renderer. Native services may depend on
appropriate OS libraries. Pure CRDT, format, crypto, policy and protocol modules
are not moved into native backends just because callers are platform-dependent.
Existing third-party native backends remain dependencies.

## Host and capability matrix

Target application hosts are Linux, macOS and Windows on x86_64 and aarch64, as
applicable to repository architecture contracts. This is the implementation
target, not a claim of published artifacts or validated host versions. Minimum
OS versions and native SDK requirements are established from qualified backend
and package tests before a target is certified. No unspecified host is silently
treated as Linux or x86_64. The evidence ledger enumerates every claimed
OS/architecture pair before certification.

R means required for the semantic feature when that feature is enabled. O means
optional/device-dependent, with explicit absence. T means an explicitly
target-specific administration/deployment capability, not required on other hosts.
No entry below has been certified by this planning document.

| Semantic capability | Linux | macOS | Windows | Qualification subject |
| --- | --- | --- | --- | --- |
| app UI, text input, accessibility/actions | R | R | R | same app and action/resource behavior |
| private app state and durable publication | R | R | R | effective native privacy, identity, crash/error behavior |
| cross-process leases and transaction locks | R | R | R | contention, process death, shared/exclusive semantics |
| known locations, locale, host architecture | R | R | R | actual host values, translation and no-home behavior |
| file/folder chooser and granted file access | R | R | R | native selection, cancel, authorization and ownership |
| open/reveal, clipboard, drag/drop | R | R | R | real native handoff and artifact lifetime |
| record/history/collaboration service clients | R | R | R | authenticated resource semantics; native mount not required |
| local authenticated service connection | R | R | R | identity, scope, transfer ownership and revocation |
| managed child process and trusted tools | R | R | R | exact owned tree lifecycle and bounded I/O |
| terminal session, if enabled | R | R | R | native PTY/shell/session; requested confinement honored |
| credential custody, if enabled | R | R | R | native custody or explicitly authorized equivalent policy |
| webview, PDF/media rendering, if enabled | R | R | R | real engine output and installed package discovery |
| speech output, if enabled | R | R | R | native audible speech and interruption/cancellation |
| camera/mic/screen capture | O | O | O | host support, hardware and consent; owned live session |
| hardware acceleration / native image handles | O | O | O | observed acceleration or correct CPU fallback |
| Bluetooth, sensors, battery, printer, location | O | O | O | accurate availability and hardware authorization |
| system facts and process inventory | R | R | R | supported observations; individual fields may be unknown |
| host setting/admin operations | O | O | O | semantic supported operation with native authorization |
| raw-media imaging in authorized host distribution | R | R | R | device identity, confirmation, exclusive authorized write |
| connected-folder transactions with strong witnesses | O | O | O | exact required native guarantees before availability |
| Linux FUSE projection | T | — | — | real Linux mount; semantic filesystem access remains R |
| Fedora Bootc/session/boot/rollback administration | T | — | — | isolated image and real privileged services |
| Linux Landlock/seccomp/ush implementation | T | — | — | actual Linux enforcement, not argv construction |
| Linux-host Wine/FEX/VM integration | T | — | — | confined guest/runtime, not Windows host certification |

The matrix gives the general classification; a capability required by a package
profile requires a functional framework implementation on every claimed host,
even when its row says O. Missing hardware, denied permission and transient
service unavailability remain typed outcomes. A missing backend is not device
absence, and certification cannot silently change an app's semantic profile by
OS. Genuinely optional features are excluded from the profile's parity promise
before certification. T entries remain the explicit target-specific exception.
Optional does not mean the app may implement an OS fallback itself. Framework
build composition selects implementation details. A confined distribution must
refuse launch or operation when required isolation cannot be provided; it must
not fall back to a trusted host mode. Explicit trusted-host distributions remain
a distinct host policy.

## Scope, authority and lifetime

Extend the existing `HostContext` preparation path; do not create a parallel app
identity or permissions registry. Host-created clients bind the existing package,
workspace, device, principal, instance and lifetime. Host composition may inject
test providers, but production app factories cannot choose a backend or supply
their own verified identity. Test provider injection belongs to host/test support.

Availability is descriptive, never a grant. Each operation checks live authority
and revocation at the owning service. A selected file, approved device, capture
session, service connection or transactional proof carries owned capability
state; a display path, integer PID, node ID or returned string cannot recreate it.
Required security evidence has private constructors. Privileged operations have
typed vocabularies, never arbitrary command execution.

Each operation preserves the authenticated invoking collaborator's authority and
delegated scope through asynchronous execution, intersected with package,
workspace, instance and resource limits. Factory identity binds the app and
instance context but is not caller authentication. Author strings and history
`ActionContext` are metadata, not caller authentication. Recheck access at
relevant native-access and result-publication boundaries. Expiry or revocation
invalidates pending work; a later regrant does not revive that operation or let a
later invocation inherit its grant.

Instance closure immediately revokes further access and result delivery and
cancels pending operations. The cleanup owner retains native resources and any
selected-resource or writer-exclusion lease until native quiescence is
acknowledged. Subsystem contracts define cancellation/completion races and the
status of already-committed effects: cancellation does not undo a completed
native write or handoff. Native handles remain local and never enter CRDT or
serialized resource state. Actions and accessibility invoke the same production
operation path as UI controls.

## Results and delivery

An operation distinguishes success, user cancellation, permission denial,
revocation/stale authority, unsupported capability, temporary unavailability,
busy/conflict and failed execution. Map native errors inside the framework;
preserve sanitized diagnostic context without requiring app OS branches.
Observations distinguish known false/empty/zero from unknown/unavailable.

Filesystem publication additionally distinguishes failure before publication
from failure after publication when durability is not proven. Retry/recovery must
not assume the old file survived a failed final flush. Advisory performance hints
are not mandatory security guarantees. Unknown process observation is not death.

Host interactions never block UI rendering. Integrate existing operation/wake
delivery rather than creating a second action bus. Cancellation must close/reap
the actual owned operation; dropping a response receiver alone is not sufficient.

## Compatibility and enforcement

Migrate API, implementation, callers and tests in coherent slices. Keep private
proof and helper protocols together. Preserve legacy stored identities, data
locations, file-name encodings, checkpoint/journal formats and wire messages;
explicit migrations must cover restart and concurrent launch. Compatibility
wrappers can temporarily forward from old shared APIs into framework services,
but never keep native implementation inside an app behind a new facade.

The initial source/dependency guard is a no-new-leaks ratchet. Exact exceptions
carry finding IDs and removal units. Later public API and target-resolved graph
checks close the gaps that lexical scans cannot prove. Native test/build/tooling
exceptions have narrow, documented owners. Final app runtime source has no native
implementation exceptions.

Completion requires app behavior, native service conformance and installed bundle
evidence on each claimed host/architecture. Cross-compilation, injected tests,
screenshots, source matching and green manifests each prove only their own scope.
Missing required native evidence remains outstanding work.
