# Framework platform services

Applications express semantic operations through the Tilo framework. The
framework selects a host implementation for Linux, macOS, or Windows and owns
the native APIs, platform branches, process construction, and packaging needed
to perform the operation. Application code retains product policy, portable
formats, resources and actions, CRDT state, and presentation.

## Dependency boundary

```text
application policy and UI
        |
        v
Tilo application SDK and HostContext
        |
        v
service contracts and shared implementations
        |
        +-- Linux backend
        +-- macOS backend
        +-- Windows backend
```

The framework is an ownership boundary rather than one renderer crate. Focused
services remain in their appropriate `crates/system`, `crates/runtime`, media,
identity, storage, or engine owners and may be used by headless tools. Reusable
framework services do not depend on app-owned native mechanisms. A host
composition root may depend narrowly on app entry points to assemble a desktop.

Public APIs expose operations and typed results, not OS handles, backend
factories, commands, or target-specific Cargo features. Host composition selects
implementations. Platform code is expected inside the backend that makes the
abstraction work; ordinary application code must not choose or implement it.

## Authority and lifetime

Extend the existing `HostContext` identity and permissions rather than creating
a parallel registry. Host-created clients bind package, workspace, device,
principal, instance, and lifetime. Availability describes a capability and
never grants it. Every operation checks current authority and revocation at the
owning service.

Native handles and security evidence are private, owned capabilities. A display
path, PID, node ID, or serialized string cannot recreate them. Privileged
operations use typed vocabularies. Application closure revokes new work and
result delivery, cancels pending operations, and retains owned resources until
native quiescence. Cancellation does not undo an already committed native
effect.

Results distinguish success, user cancellation, denial, revocation, unsupported
capability, temporary unavailability, conflict, and execution failure. Native
errors are mapped within the framework. Observations distinguish a known empty
or false value from an unknown value.

## Service ownership

- durable storage, private locations, locking, and transaction recovery;
- authenticated local service connections and descriptor or handle transfer;
- identity, credential custody, and native secret stores;
- owned process trees, terminals, and trusted tools;
- file selection, granted access, open/reveal, trash, clipboard, and drag data;
- host observations, settings, hardware, media, and capture sessions;
- browser, document, speech, OCR, and other native engines;
- native resource projection and connected-folder transactions;
- installer, raw-media, boot, update, and distribution backends.

Optional hardware or denied permission is a typed runtime outcome. A missing
required backend is unfinished support, not device absence. A package profile
must implement every capability it requires on each host it claims.

## Migration and qualification

Migrate each service contract, implementation, callers, compatibility path, and
tests as one coherent unit. Preserve on-disk formats, identities, native names,
wire protocols, recovery rules, and authorization boundaries. A forwarding
facade over app-owned native code does not complete the migration.

Use a no-new-leaks source and dependency check while moving existing code, with
narrow temporary exceptions and an owner for removing each one. Qualification
requires application behavior, native service conformance, and installed-bundle
evidence on every claimed host and architecture. Cross-compilation and injected
tests establish only their own scope.
