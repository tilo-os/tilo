# Review of the platform extraction audit

Date: 2026-09-18. Scope: audit review, framework source inspection and migration
planning. No production implementation, native builds or runtime validation.

## Conclusion

The audit is a useful source inventory and security-preserving handoff. Its 13
packages correctly consolidate repeated mechanisms across apps and services.
It is not yet a complete design for making framework apps cross-platform by
construction. That requires changes inside the framework, which the audit
explicitly excluded, and a support contract stronger than “compiles and returns
unavailable.” Use its 194 finding groups as traceable inputs, not 194 independent
refactors or a count of defects.

The user's requirement is the acceptance criterion: the same application source
and semantic capability declarations run on supported hosts without app-owned
OS detection, native implementation, backend selection or platform packaging
recipes. The framework and its build/distribution tools own those differences.

The framework itself must contain platform-specific code. Its Linux, macOS and
Windows backends are the bridge to each host. The restriction is on application
code and the public API it consumes, not on framework internals. Existing service
crates become explicitly framework-owned components; their directory need not be
`ui/`. The audit's exclusion of `ui/` and `crates/app/` describes its search scope,
not the desired limit of framework ownership.

## Changes required in the destination design

### 1. Make the application SDK a real boundary

`crates/app/tilo-app/src/lib.rs` currently does `pub use ui_core::*`. Its manifest
forwards `webview-wpe`, `dmabuf-import` and `mpris`. `ui/src/lib.rs` publicly
exports native integration modules, including Linux DMA-BUF import when enabled.
Therefore an app can depend on the SDK and still select and consume a native
implementation. This is source-confirmed, not a claimed cross-target build result.

Keep the existing `ui` dependency alias for applications, but curate the SDK
exports and feature vocabulary. Apps request browser, media, file selection,
storage and other semantic capabilities. Framework host composition chooses
WPE/CEF, PipeWire/native media, XDG/native dialogs and the relevant transport.
Internal renderer/backend crates retain access to native APIs. Avoid a blanket
re-export of those internals from the public SDK. Stage removals with caller
migrations so intermediate commits remain buildable.

### 2. Extend the existing host context; do not introduce ambient authority

`ui/src/application.rs::HostContext` already binds app, instance, device, records,
permissions, author, action context and lifetime. Integrate platform service
access with this construction path and `ui/src/launch.rs`, rather than allowing
apps to manufacture a global service registry from environment variables.

The host supplies scoped service clients. The client operation checks current
permission, instance/workspace identity and revocation; a capability snapshot is
not authorization. Resources supplied by the service remain owned and opaque.
Closing an instance cancels its pending work and releases its leases. Services
and CLIs below the UI use the same contracts without depending on the renderer.
Do not turn `HostContext` into an OS-command executor.

### 3. Reconcile existing framework services before adding interfaces

`ui/src/portal.rs` already provides asynchronous choose/open/reveal requests and
an `OpenedFile` retaining a selected file. However, its production chooser is
XDG-backed, its launch implementation imports Unix descriptor APIs under the
`portal` feature, and its result types collapse several distinct failures into
strings. `ui/src/sysdirs.rs` already supplies known-folder discovery and injected
test values, but also provides guessed display paths. A known location or display
suggestion must not be interpreted as a file-access grant.

Extend these seams. Preserve current behavior while separating semantic requests,
host implementation and UI delivery. Return cancellation, denied, unsupported,
temporarily unavailable and operation failure distinctly. Keep host-native error
details available for diagnostics without exposing secrets or requiring OS-specific
app branches. Choose a compatible rollout for current `PortalOutcome` consumers.

`ui/src/portal.rs::LaunchRequest::send` currently returns `Opened` when test
suppression is active. Keep that as explicitly injected test behavior; it cannot
serve as evidence of a real host handoff. Prefer per-context test services over
process-global switches as callers migrate.

### 4. Separate required portability from optional host capabilities

Define the framework application support matrix for Linux, macOS and Windows,
including x86_64/aarch64 applicability, before claiming cross-platform completion.
Keep source support, packaged support and published release support separate.

For required functionality on a supported host, a backend must actually work;
returning Unsupported is an incomplete port. Optional capabilities may be absent
on a host or device. Inherently host-specific operations such as managing Fedora
Bootc, exercising Landlock, or mounting the Linux FUSE backend may be unavailable
elsewhere. Apps express required/optional functionality, never `if linux` policy.
Availability changes and permission denial still need ordinary app handling.

Do not confuse the existing Linux-host Windows compatibility service with a
Windows-native framework host. Likewise, macOS no-mount filesystem tests do not
establish a native filesystem projection implementation.

### 5. Include API, dependency and distribution enforcement

The existing `tools/check_app_resource_action_contract.py` already enforces the
`tilo-app` dependency and rejects direct ui-core dependencies. Extend this model
with a separate platform contract check. Source checks alone cannot detect a
backend leaking through public types, Cargo feature unification or transitive
dependencies. Add public API consumer tests and target-resolved dependency checks.

Start a no-new-leaks ratchet immediately. Existing exceptions identify exact
source/symbol, finding ID, owner and removal task. No whole-app exclusions. Each
migrated service removes its exceptions; final application production code has
none. Literal strings in fixture data, protocol names and portable APIs are not
automatically defects. Review native test/build exceptions independently.

Installed app bundles must include and find the selected engines, helpers and
assets without the source tree or developer PATH. The audit's P13 is a gate on
every package, not just a final cleanup phase. App metadata remains declarative;
shared packaging owns host-specific mechanics. Fedora image and release tooling
remain deliberate deployment backends.

### 6. Treat safety-contract repairs separately from mechanical moves

Some findings require behavioral fixes, not just relocation. In particular:

- A08-15: non-Linux process observation can classify every owner as dead.
- A08-16: an in-process mutex does not replace an interprocess registry lock.
- A09-01: `atomic-file::write_with_mode` explicitly ignores privacy mode off Unix.
- A01-10/A15-02: a filename pattern does not prove ownership of a drag artifact.
- A02-10/A11-10: existence of projected metadata does not prove hydration.
- A04-14: an unavailable firewall probe must not be displayed as a negative fact.

Record a failing production-path regression before changing these semantics.
Keep the fix reviewable apart from large source relocation. Preserve on-disk and
wire compatibility, including raw filename encodings and native identity evidence.
Never replace private handle operations with path-based reopening for convenience.

### 7. Preserve coherent security units, especially Imager

P11 depends on P01/P03/P04, but a generic rename/open API is insufficient for its
transaction semantics. Move endpoints, store/descriptor codecs, executor, leases,
witnesses, lifetime-bound evidence, helper dispatch and their tests together.
Keep constructors private. Portable reconciliation may belong in the extracted
service when necessary to avoid dependencies back into the app. App-specific
presentation and installation choices remain app-owned.

Do not generalize every low-level operation on the first move. Reuse primitives
only after proving their requirements match. Linux transaction execution and
Windows endpoint/storage support are distinct capabilities. Do not advertise the
latter as a Windows transaction executor.

## Evidence refresh

Compared the audit's 1,500 recorded anchors against the current checkout across
1,122 distinct referenced files. No referenced file was missing. Twelve files
had a different full-file SHA-256; nine saved anchor lines did not exactly match
the current line. These are source freshness checks, not proof of report truth.
Five line mismatches were in historical documentation whose stored file hash
still matched; resolve by symbol/context rather than trusting line numbers alone.

Changed source files at review time:

- `apps/backups/src/app.rs`
- `apps/notes/tests/framework_history.rs`
- `apps/osk/src/widget.rs`
- `apps/pdf-viewer/src/app.rs`
- `apps/photos/tests/framework_history_launch.rs`
- `apps/slides/src/framework_history_tests.rs`
- `apps/terminal/Cargo.toml`
- `apps/terminal/src/main.rs`
- `apps/typst-editor/src/framework_history_tests.rs`
- `apps/weather/src/framework_history_tests.rs`
- `apps/word-processor/src/widget.rs`
- `apps/word-processor/src/widget/tests.rs`

The original audit stays unchanged. Re-resolve source anchors at the start of
each implementation task and record its actual base. Review inspected consolidated
findings and domain evidence plus selected production sources; it did not repeat
a line-by-line audit of all candidate files or validate all security claims.

## Planning outcome

See [the migration roadmap](README.md) and [finding register](finding-register.json).
The roadmap splits the audit into service-owned changes with dependencies,
source ownership, acceptance and native evidence gates. It deliberately separates
program sequencing from executable subsystem plans. Each subsystem's exact API,
test code and patch sequence must be written against its refreshed sources before
implementation; proposed service names are not existing APIs.
