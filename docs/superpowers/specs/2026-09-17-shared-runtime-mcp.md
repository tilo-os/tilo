# Shared runtime MCP

## User outcome
Implement MCP at the shared runtime layer so an external MCP client can discover real application operations, crop a nonsquare image to a square, export the edited image, and insert that actual image into a Writer document. The user will supply another task after delivery; therefore discovery/routing must be generic and contain no Photos/Writer workflow branches.

User constraints: YAGNI; maximize shared code; platform code only at edges; one MCP connection for all runtime-connected framework apps; automatic publication through ordinary framework startup. The user explicitly selected subagent-driven-development to implement the previously discussed design.

## Architecture
Extend existing shell::runtime_host::Session. It already starts the same production identity and collaboration brokers as qOS and launches unmodified apps with the SDK service connections and shared content store. ui::App registers action directories automatically, but native testing exposed that resources-only widgets were missing from bounded discovery. Build a data-only revision-bound directory projection during ordinary validated publication so all framework declarations are discoverable without per-app adapters or remote bulk enumeration. Retain that registry, authority, invocation lifecycle and blob transfer. Do not build another registry, workflow engine, permission system, or app-specific MCP server.

Add a transport-neutral MCP JSON-RPC adapter over AppActionBroker and its bounded pull-directory bridge, plus a thin native stdio executable that owns Session and configured child processes. The same adapter can be hosted by qOS. Identity/actor/workspace are host-owned constructor inputs, never tool arguments. Use current authority for directory queries and atomically re-resolve at invocation. Preserve explicit refusal of confirmation-required actions unless an existing real approval flow provides evidence; never let an arbitrary MCP boolean fabricate broker admission.

The first executable uses the existing macOS/Linux Unix launch edge. Newly added MCP/action logic contains no platform conditionals. Existing Windows server/transport gaps are documented accurately; this feature must not pretend the Unix-concrete shell runtime already runs on Windows. No remote federation or cloud relay is added.

## MCP surface and lifecycle
Expose bounded application discovery, resource/action query, exact resolution, invocation, status and cancellation. Stable addresses include a live instance peer id plus resource and action ids; duplicate app instances are distinct, disconnected/stale addresses fail. Use canonical typed parameters/contracts and ActionOutput/InvocationUpdate unchanged, including content resource references. Accepted is not Succeeded. Do not silently retry mutations.

Use MCP stdio JSON-RPC, initialize/ping/tools/list/tools/call/resources/list/resources/read. Advertise only implemented capabilities. Bound input lines and directory traversal; errors must be structured, stdout protocol-only, stderr diagnostics. Notifications have no replies. Preserve current legacy Python compositor MCP compatibility; it need not become a dependency of native runtime MCP.

A user-selected app manifest supplies app ids, executable paths and argv. These are startup configuration, not arbitrary model-supplied executable commands. Launch through Session::launch; app publication requires no MCP code in Photos or Writer. The runtime owns/reaps only its children, keeps a persistent selected profile, and releases profile locks and services on EOF/shutdown. Automatic publication applies to apps launched with runtime service connections; do not claim unrelated unmanaged processes attach automatically.

## Verification
Unit/integration tests cover protocol negotiation, malformed requests, bounded input/discovery, two distinct app instances, invalid typed args, denied/revoked/stale actions, confirmation-required refusal, terminal output propagation, owner-scoped status/cancel and disconnect without replay. Use existing real broker test seams for authority tests rather than a fake replacement dispatcher.

A native end-to-end test launches real Photos and Writer under Session through the production runtime executable, drives only MCP for crop/export/insertion, uses fresh discovery/resolution and actual terminal results, and independently verifies edited PNG dimensions/content hash and the Writer document's embedded image hash, independently read from signed events or its actual persisted Automerge record. Use an isolated profile and nonsquare patterned source; prove square pixels equal the expected crop, not merely reported dimensions. Retain protocol transcript, app logs, source/result images and document evidence. Exercise Calculator as a third GUI app through the same generic endpoint and verify a declared operation with fresh resource discovery. Retain the all-app source audit separately from native behavioral evidence. No code branches on the acceptance workflow or app ids in the runtime adapter.

Do not claim model-client installation, Windows-native validation or 60fps qualification from this proof. The prior performance goal has been superseded; its saved build/evidence remains intact.
