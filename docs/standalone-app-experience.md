# Standalone Tilo apps on Windows and macOS

Status: implementation plan and source audit, 2026-09-18. This document does not
claim that standalone sharing or universal history browsing is available yet.

## Product requirement

A person opens Calculator, Notes or another Tilo app from their normal desktop
and starts working. They do not first configure a desktop session, learn a
workspace model, create an account, or run a companion application manually.
Windows and macOS retain their native window management and decorations.

Advanced features belong to one shared host layer, not forty app implementations:

- **Search** finds content and actions in the current app. Broader search is an
  explicit scope, includes only admitted resources, and labels where a result
  will open. Native Windows Search/Spotlight integration is an optional adapter,
  not a prerequisite for the in-app experience.
- **History** shows changes to the current document, with clear authors, times,
  comparisons and available restore/undo operations. Browsing does not alter the
  document. A change of focused document must never show or mutate another
  document's history.
- **Share** starts from the document or item the person is using. It explains
  exactly what will be shared, with whom and with which permissions. Identity
  setup is contextual, only when needed. Existing private documents and other
  app windows remain private. Sharing status, leaving and revocation are visible.
- Local use remains available offline. Losing the host service or a peer does
  not block the first frame or discard local changes. Pending, offline, refused
  and completed operations must remain distinguishable.

Terms such as broker, CRDT, capability and workspace identifier are implementation
concerns, not first-use steps. The collaboration context can still be represented
by the existing workspace/resource model internally; this is not a second model.

## What macOS actually implements today

The comparison is from production source, not a completed native qualification.

| Capability | Shared standalone implementation | macOS-specific difference |
| --- | --- | --- |
| Search and actions | `ui::App::open_action_palette` uses the app's resource tree and registered providers; Ctrl+K or Command+K opens it | Platform modifier and default native app menu |
| Undo/redo | Shared action recording, focused-component routing and view restoration | Platform modifier |
| History | Shared history model, timeline and comparison widgets; some apps expose their own history UI | No universal standalone History command found |
| Collaboration | Existing shared CRDT, presence, permissions and Cap'n Proto clients require an admitted host service | A launcher-less macOS app also lacks that service; Windows service discovery explicitly returns unavailable |
| Storage | Package/record-scoped local persistence with a non-routable fallback context | Native data-directory convention |
| Native windows | Shared winit runner with host decorations | Cocoa backend and basic About/Services/Hide/Quit menu |

macOS also has a separate shell runner in `shell/src/compositor/macos.rs`. It
restores a workspace tree and record store and constructs `ShellOverlayHost`,
but passes peer synchronization as false. It is not evidence that standalone
apps already provide connected sharing, or a reason to put a Linux desktop
inside each Windows app. The macOS qualification inventory records `not_run`.

Unix socket support also does not establish authenticated macOS hosting. The
launcher currently checks Linux process birth information under `/proc`; its
non-Linux implementation returns no attestation. Both macOS and Windows need
an authenticated launch adapter before they can reuse the broker safely.

Windows already has a native Common Item Dialog adapter. The current macOS
picker follows the Unix portal path; that gap should be fixed in the shared
picker backend rather than copied into Windows.

## Shared implementation boundaries

1. **App-owned services presentation.** Both native launch paths in
   `ui/src/launch.rs` should install the same shared services controller. Use
   existing themed/accessibility-aware buttons and modal surfaces inside app
   content, beneath native decorations. Preserve compositor embedding and
   immersive modes. Search, History and Share must also be host-owned semantic
   resources/actions, so mouse, keyboard, accessibility and automation enter
   the same route. Apps declare content capabilities, not OS-specific menus.
2. **Correct record binding.** History resolves the focused component through
   `CallbackBinding::with_record_frame`, or the active legacy `RecordSet`.
   Reuse `HistorySource`, `ActivityTimeline`, record-owned history and comparison
   models. Capture the record identity when opening; invalidate stale bindings
   on replacement or switching. Never assume `App.state.doc` owns the focused
   component's document. Reverts retain normal permissions and attribution.
3. **Automatic local host service.** Extract/reuse existing host admission,
   action-directory, persistence, identity and collaboration services. Start or
   connect on demand without blocking ordinary app use. Keep the existing
   protocol and authorization checks; do not create an app-owned peer stack,
   unauthenticated localhost service, or broad filesystem proxy.
4. **Narrow platform adapters.** Isolate process/service discovery and IPC,
   protected credential storage, native dialogs/menus, notifications and media
   controls. The common controller, history, search, invitation state machine,
   permissions and document synchronization do not branch per operating system.

The Windows service transport needs an explicit admission design. A same-user
pipe alone is not proof of app identity or sandbox confinement. Ordinary native
apps must not be advertised as having Linux's Bubblewrap/Landlock boundary.
Do not weaken the existing Linux boundary to simplify another host. Capability
checks and remote isolation remain required even when local apps are trusted.

### Reuse the service protocol, separate its platform boundary

`runtime-portal` currently combines acquiring an inherited Unix descriptor
with constructing a Cap'n Proto RPC client. Separate those concerns: keep the
Unix descriptor adapter, and let the common client accept owned connected
asynchronous I/O. Retain the existing `LocalSet` execution model. A Windows
handle must be an owned transport object, not an integer cast to a Unix fd.
Transport construction itself does not authenticate a caller.

Likewise, separate broker admission from serving an already admitted connection.
The platform launcher must supply a verified app/workspace identity tied to the
process lifetime. The existing membership authority, certified joins, filtered
action directory and invocation-time permission checks remain authoritative.
Discovery is not permission to invoke later: revocation must take effect after
a result is listed too.

Keep signing material in the host through the existing identity broker and
secure-store provider boundary. Use the existing `MlsVaultProvider` abstraction
instead of carrying the concrete Secret Service implementation into portable
host code. Broker audit logs and document history remain distinct; document
history comes from the permitted record's actual state.

The first integrated host should serve one real local workspace and two ordinary
app processes. Prove shared edits, history and identity across restart, exact
search navigation and immediate revocation before enabling remote peers.

## Implementation order and evidence

1. Add visible common services entry points and canonical action routing.
   Verify the same actions through pointer-free automation and native keyboard
   commands in more than one app, with no per-app platform branches.
2. Wire generic history browsing/comparison to actual record ownership. Test
   two documents/components, close/reopen, focus switching, read-only access,
   denied restore, permission changes and stale panel bindings.
3. Establish the authenticated standalone host connection and protected identity
   lifecycle. Test concurrent launches, a stopped/crashed service, spoofed client,
   unavailable custody, corrupted state and reconnect; startup must stay local
   and responsive.
4. Wire contextual sharing to existing invitations and permission policy. Prove
   two-process edits and attributed history, scoped discovery, revocation,
   reconnect/offline changes and refusal of unauthorized resources. A visible
   Share button or an inert client is not completion evidence.
5. Add admitted cross-app search and optional OS search adapters. Test that
   private/closed/revoked resources are not leaked and that each result opens
   the exact resource through its existing action contract.

Headless model, persistence, protocol and rendering checks are the default.
Use a few targeted native checks only for behavior requiring the actual window
system, accessibility bridge, native menu or OS integration. macOS source parity
must not be described as runtime-verified macOS behavior on a Windows machine.

## Source map

- `ui/src/launch.rs`: component and legacy standalone native construction.
- `ui/src/app.rs`: action palette, admitted dispatch, focused undo/history routing.
- `ui/src/application.rs`: explicit HostContext identity/authority/record inputs.
- `ui/src/bound_dispatch.rs`: focused component and record-bound callbacks.
- `ui/src/record.rs`: RecordSet, RecordStore and record-owned history.
- `ui/src/history_source/` and `ui/src/widget/activity_timeline.rs`: shared history.
- `ui/src/collaboration.rs`, `crates/runtime/runtime-collab/` and
  `crates/runtime/runtime-portal/`: existing service client and transport boundary.
- `shell/src/compositor/macos.rs`, `shell/src/compositor/hosted.rs` and
  `shell/src/shell/overlay_host/mod.rs`: current host implementations and room policy.
- `shell/src/shell/launcher.rs` and `shell/src/shell/collab_broker_capnp.rs`:
  process attestation, service admission and filtered action-directory serving.
- `shell/src/identity_broker.rs`, `crates/runtime/runtime-identity/src/secure_store.rs`
  and `shell/src/shell/collab_mls_authority.rs`: identity and MLS custody boundaries.
- `docs/superpowers/specs/2026-09-12-macos-app-qualification.json`: qualification status.
