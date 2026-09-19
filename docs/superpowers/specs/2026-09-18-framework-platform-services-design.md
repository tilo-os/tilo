# Framework-owned platform services

Status: proposed design for review; implementation has not started.

## Objective

Move app-owned OS integration into the app framework. Apps express operations
and render results; framework backends select and implement host behavior.
Cover Imager, Browser, Terminal, Settings, Monitor and the remaining apps,
including platform-dependent test helpers and build/package wiring.

## Boundary

Expose focused service modules through `ui::platform`, preserving its existing
UI convention APIs. Implement non-rendering services in narrowly scoped crates
under `crates/system` or `crates/runtime`, reusing existing services where they
already own the operation. Headless tools can depend on those crates without
pulling in the UI renderer. Dependencies point from apps to framework services;
framework crates must never import app code.

Native Tilo operations retain the existing portal and authorization boundary.
Hosted backends retain the existing platform behavior and permissions. Extraction
does not confer additional filesystem, credential or process access. Unsupported
operations remain explicitly unavailable; probe failure is not evidence that a
security property is satisfied.

## Components

- File launch and reveal: move command selection from Browser capabilities into
  the framework. Preserve validation, error reporting and explicit user actions.
- Credentials: reuse the accounts secret-store boundary where applicable; move
  native credential adapters behind framework APIs. Retain consent and namespace
  isolation. Browser-specific form interpretation remains browser application logic.
- Browser engine: framework webview backends own native engine bootstrap,
  sandbox setup and associated native build wiring. Preserve sandbox enforcement.
- System inspection: framework services own process visibility, descriptor counts,
  namespace/seccomp/Bubblewrap detection, root encryption and EFI Secure Boot
  probing. Apps retain presentation and policy decisions based on typed results.
- Filesystem and recovery: extract Imager's native endpoint, transaction, private
  storage, durability and recovery implementation together with the types required
  to enforce their invariants. Keep handles and evidence private and move-only;
  do not widen visibility of unsafe primitives merely to cross a crate boundary.
  Imager retains setup UI and installation workflow choices. Hardware operations
  belong to an optional framework backend so ordinary apps do not acquire its
  dependencies or authority.
- Remaining apps: audit OS imports, conditional compilation, FFI, platform paths,
  command invocations, sockets, permissions and symlinks. Move host mechanisms
  into the appropriate service; portable application logic stays in apps.

## Alternatives

Putting everything directly in the UI crate would be simple to discover but
would couple installer and headless tools to rendering dependencies. Merely
moving source files with path includes would retain app coupling and would not
establish framework ownership. Focused implementation crates with a framework
facade avoid both problems.

## Migration and verification

1. Record a complete inventory, distinguishing production integrations from test
   fixtures and platform-specific packaging. Preserve existing checkout changes.
2. Extract small independent services and migrate their callers, retaining their
   existing behavioral coverage.
3. Extract the Imager dependency closure in coherent units with its security,
   recovery and fault-injection tests. Preserve on-disk formats and protocols.
4. Move engine adapters and native build ownership; migrate remaining app calls.
5. Add a source-boundary check against app-owned OS implementation, with narrowly
   documented packaging exceptions if required. A facade over app-owned code does
   not satisfy the check.
6. Run focused service and app tests, compile affected test targets, and run
   `cargo check -p shell --tests --locked`. Validate Linux-only operations on Linux
   and native backends on their supported hosts; report unavailable verification
   explicitly. Compile checks do not establish runtime security behavior.

Do not regenerate Cargo.lock. Any necessary workspace package or dependency-edge
changes must produce a minimal reviewed lockfile diff. Never disable failing
tests, weaken assertions or replace production-path checks with helper-only tests.

## Acceptance

All inventoried host mechanisms are framework-owned, callers use semantic APIs,
framework crates have no dependencies on app crates, and existing authorization,
recovery, persistence and user-visible behavior remain intact. The final report
lists verification evidence and any platform validation that could not run.
