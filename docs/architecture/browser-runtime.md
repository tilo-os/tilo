# Browser runtime

The Cloud web console renders the Tilo desktop in the browser by running the
Tilo UI runtime itself as WebAssembly. The page is a real peer: it holds a
device identity certified by the user's root, replicates workspace documents
with the user's machine through `tilo-relay`, and renders them locally. Video
streaming of a remote desktop was considered and rejected; it cannot work
offline, cannot keep per-document authority on the device, and costs a GPU
encoder per viewer.

This document records what the earlier qOS browser runtime did, the contract
Cloud consumes, what blocks `wasm32-unknown-unknown` on `main`, the transport
from browser to machine, and the milestones. It follows the host model in
[`hosts.md`](hosts.md): the page is one more host that supplies its own
implementations of the toolkit's interfaces.

## What the qOS runtime did

The unmerged qOS lineage (`feat/cloud-owner-machine-linking` and siblings)
shipped `crates/browser/qos-browser-runtime`, a `cdylib` built with
`wasm-bindgen --target web` plus about 25k lines of hand-written ES modules
under `web/`. It is reference material only; nothing is ported verbatim.

| Concern | qOS approach |
|---|---|
| Rendering | The real `ApplicationDefinition` (Writer, Chat, the desktop) through the portable `tilo-ui` build. Vello scenes, scaled by one DPR transform, presented through wgpu's **WebGPU** backend on a `<canvas>`. WebGL2 was explicitly unsupported (Vello needs compute). GPU errors and device loss surfaced as a failed status; `recover()` recreated device, surface and renderer without recreating documents. |
| Event loop and input | One winit web event loop per page, with independently owned windows per mount. winit delivered pointer, wheel and keys; a DOM adapter published modifier state first. A hidden `<textarea>` carried IME composition and `beforeinput` into the shared host IME dispatcher. |
| Accessibility | The toolkit's semantic routes projected into an accessible DOM tree; DOM actions invoked the real widget actions. |
| Clipboard | Text only, and only inside trusted clipboard events. |
| Fonts | The product fonts from `crates/sdk/ui-core/assets/fonts/manifest.toml`, copied into the bundle with a `fonts.json` (name, bytes, sha256, `browser_startup`). `font-loader.mjs` fetched same-origin, verified hashes, loaded startup fonts before first frame and optional ones on demand. |
| Images and assets | Host-driven `ImageStore`; a `loadAsset` callback or same-origin fetch; qdoc blobs hash-checked. |
| Storage | IndexedDB with strict-durability transactions storing bytes and Automerge actor together, in a dedicated worker. An exclusive Web Lock per profile/device serialised tabs. A write counted as saved only after commit. Failed closes retained custody until `retryPendingRecovery()`. |
| Identity | Browser-held keys (a Cloud owner root and a device key per Cloud machine) persisted in IndexedDB under Web Locks (`cloud-identity-custody.mjs`). Enrollment went through an injected `CloudEnrollmentAdapter` (`inspect`/`apply` against the Cloud Store); `signCloudMachineMesh` signed machine-link grants inside WASM. |
| Peer transport | "One browser state owner with authenticated HTTPS delivery; no relay fallback": a long-poll HTTPS carrier (`tilo-delivery-protocol`) authenticated with the peer key. Live presence used a separate channel; media used MoQ over WebTransport. `iroh` was linked for wasm but not used as the state carrier. |
| Session peer | `qos-peer-state` held admitted records, heads and outgoing queues; `PeerSnapshot` and `SessionHealth` v1 reported them. |

What worked and is worth keeping: WebGPU-only Vello presentation, DOM IME and
semantics projection, verified font catalog, strict-durability IndexedDB under
Web Locks, and "saved means durable". What should not come back: the parallel
HTTPS delivery protocol (Tilo `main` already has an authenticated relay), the
bespoke qOS identity shapes (superseded by `tilo-user-identity`), and the size
of the JS layer — most of that logic belongs in Rust behind a small JS shim.

## The contract Cloud consumes

Cloud (`src/services/*`) loads the runtime from same-origin static paths
prepared by `scripts/prepare-qos-runtime.mjs` and `scripts/dev-runtime.mjs`.

| Cloud module | Entry | Uses |
|---|---|---|
| `localPeer.ts` via `public/qos-runtime-loader.mjs` | `/qos-runtime/host.mjs` | `mountDesktop(el, {app, profile, device, room, author, instance, signal, onStatus})` → `{cleanup, flush}`; `RuntimeStatus.phase` (`gpu-pending`/`presented`/`failed`/`closed`), `persistence`. |
| `certifiedPeer.ts` | `/qos-session-runtime/peer.mjs` | `connectPeer({enrollmentBaseUrl, invitation, signal, onStatus})` → `CertifiedPeer {getSnapshot, subscribe, mountDesktop, recover, reconnect, retryPersistence, dispose, actionsV1, actionsV2, mediaV1}`; `pendingPeerRecoveryCount`, `retryPendingPeerRecovery`. |
| `hostedPeer.ts`, `hosted/cloudSession.ts` | `/qos-runtime/cloud.mjs` | `initializeCloudRuntime()` → `{runtime.cloud_enrollment_request, connectPeer}`; `connectPeer({cloud: {accountId, machineId, label, enrollment}})`; `signCloudMachineMesh`. |
| `sessionHealth.ts` | snapshot | `SessionHealth` v1: failures by scope, `RecordHealth` per record, `RecoveryRequest`/`recover`. |
| `sessionActions.ts`, `nativeAgents.ts` | `actionsV1`/`actionsV2` | Broker catalogue, private invocations, tickets; agent control. |
| navigation | `DesktopMount.navigation` / `onNavigation` | `DesktopNavigationV1`: route (workspace/strip/group/tab), activity views, commands. |

### Keep, reshape, drop

Keep the shape Cloud already codes against where it is sound — it reduces
Cloud churn to module paths and a few types:

- **`mountDesktop`** and `RuntimeStatus` phases: keep. The mount resolves
  after first presentation; `cleanup` is idempotent; `flush` resolves when
  pending writes are durable.
- **`connectPeer` → `CertifiedPeer`**: keep the lifecycle (`getSnapshot`,
  `subscribe`, `mountDesktop`, `reconnect`, `dispose`) and `PeerSnapshot.phase`.
- **`SessionHealth` v1** and `recover`: keep the vocabulary (scopes, record
  health, recovery requests), but derive it from the relay session and
  `DurableStore` save status instead of qOS delivery cursors.
- **Navigation v1**: keep; it maps directly onto `tilo-desktop-model`
  (workspace, strip, window group, tab).

Reshape to fit `main`:

- **Identity.** One user root and root-signed `DeviceCert`s from
  `tilo-user-identity`. The browser generates a device key (WebCrypto
  non-extractable Ed25519 where available, otherwise a sealed seed in
  IndexedDB under a Web Lock) and obtains a `DeviceCert` from a device that
  holds root custody. The qOS "Cloud owner root per machine" model goes away:
  there is one root per user, and machines and browsers are devices of it.
  Cloud enrollment becomes "certify this browser device" (the user's machine
  signs, Cloud only relays the request); `signCloudMachineMesh` becomes a
  root-signed membership/mesh statement through `tilo-user-identity` rather
  than a runtime-private signer.
- **Actions.** `actionsV1`/`actionsV2` become a thin JS view over
  `tilo_actions::remote` (versioned vocabulary, `Caller` from the transport,
  idempotent invocation ids). The broker catalogue is the remote action
  directory; the ticket model maps to invocation ids.
- **Content keys.** Encrypted channels use `channel-crypto` (OpenMLS) exactly
  as native peers do; the browser is a member device. Its member state lives
  in the profile's durable store, sealed under a vault key that is itself
  sealed under a non-extractable AES-GCM key in IndexedDB
  (`channel-vault.mjs`) and unsealed into wasm memory only.
- **Transport.** `tilo-relay` over iroh (below), replacing HTTPS delivery.

Drop: `retryPendingRecovery`/`pendingComponentCloseCount` diagnostics that
exist only for qOS actor custody, `failNextWrite`/`destroyDevice`/
`probePixels` (move to a test-only export), and the invited-identity
custody variants until invitations are designed on `main`.

## Blockers on `main` for `wasm32-unknown-unknown`

Measured with
`cargo tree -p tilo-ui --target wasm32-unknown-unknown --offline -e normal
--no-default-features` (393 crates) and
`cargo check -p tilo-ui --no-default-features --lib --target
wasm32-unknown-unknown --keep-going`.

Two rules shape the fixes:

1. Toolkit **behaviour** stays behind `tilo-ui`'s default-on `native`
   feature, as `hosts.md` requires.
2. Missing **platform capabilities** — sockets, threads, owner-checked files,
   JS entropy — are handled where the crate already distinguishes platforms:
   `cfg(unix)`/`cfg(windows)` gain a third arm, and third-party backends are
   selected in `[target.'cfg(target_family = "wasm")'.dependencies]`. Tokio
   enforces this at compile time (`fs`, `io-std`, `net`, `process`,
   `rt-multi-thread`, `signal` are a `compile_error!` on wasm), so it cannot be
   a Cargo feature a far-away dependent might unify back on.

`std::fs`, `std::thread` and `std::net` compile on wasm32 and fail at run
time, so they are not compile blockers; they are listed under run-time gaps.

| # | Blocker | Crate(s) | Fix | Status |
|---|---|---|---|---|
| 1 | `getrandom` 0.2/0.4 need JS backends | `tilo-ui` tree (RustCrypto, `channel-crypto`, `tilo-seal`, `threshold-identity`) | wasm target deps in `tilo-ui`: `getrandom/wasm_js`, `getrandom 0.2/js`, `uuid/js`, `chrono/wasmbind` | fixed |
| 2 | tokio `full` (mio, sockets, signals) | `tilo-ui`, `tilo-host` | `tilo-ui` portable build uses `rt`+`sync`; `native` and `broker-transport` add `tokio/full`. `tilo-host` makes tokio optional under `native` (only its native backends use it) | fixed |
| 3 | tokio `net` | `runtime-portal` | Moved to `[target.'cfg(unix)']`; only the Unix socket path used it, non-Unix already returned `NotAvailable` | fixed |
| 4 | tokio `rt-multi-thread` | `tilo-rt` | `daemon()` runtimes are not built on wasm; `app()`, `cpu()` and `io` remain. Positioned I/O gains an `Unsupported` arm | fixed |
| 5 | OpenMLS refuses wasm without `js` | `channel-crypto` | wasm target dep `openmls/js`; required `getrandom` 0.4.1 → 0.4.3 in `Cargo.lock` | fixed |
| 6 | `ring`/`rustls` (pairing TLS in `identity_flow`) | `tilo-ui` | As in PR 541: `rustls` and `private-fs` are native-only dependencies; `remote_call` answers `None` in the browser. The bundle no longer contains `ring` or C code | fixed |
| 7 | No private-file platform | `private-fs` | Third `platform` arm refusing with `Unsupported` | fixed |
| 8 | `libc::clock_gettime`; `Url::from_file_path` | `media` | Non-Unix `monotonic_now_ns` reports unreadable (0), as a failed `clock_gettime` did; `path_to_file_uri` returns `Unsupported` | fixed |
| 9 | `Url::to_file_path` | `tilo-host::portal::to_local_path` | Non-Unix/Windows hosts have no local paths: `None` | fixed |
| 10 | `web_time::SystemTime` ≠ `std::time::SystemTime` on wasm | `tilo-ui` (settings stamps, signed ops, identity flow) | `ui::time::{to_std, from_std}` — identity on native, epoch arithmetic on the web — at the six crossings | fixed |
| 11 | wgpu handles are `!Send` on wasm; the renderer asserts `Send` | `tilo-ui::render_thread`, `offscreen` | wasm target dep `wgpu/fragile-send-sync-non-atomic-wasm` | fixed (compile) |
| 12 | Non-Unix dead-code warnings | `native_scenario`, `webview::chromium`, `tilo-rt` | Gated to their only (Unix/daemon) users | fixed |
| 13 | Identity file publication (`CString`, `OsStrExt`) | `tilo-user-identity` | As in PR 541: `persist` is not compiled for wasm32; the browser keeps its device key in host custody | fixed |
| 14 | `uuid` entropy | `tilo-relay-protocol` | wasm target dep `uuid/js` | fixed |
| 15 | clippy `uninhabited_references` (`std::fs::File` is uninhabited on wasm) | `advisory-lock` | Crate-level allow scoped to wasm; the crate should leave the bundle with milestone 5 | worked around |
| 16 | No live-host frame API | `tilo-ui::headless` | Ported from 541: `frame_schedule`, `poll_frame`, `frame_render_input`, `ime_window`, `send_host_event`, `from_component_with_wake_notifier`; `ui::ime` shared with `App` | fixed |
| 17 | No browser fonts | `tilo-ui` text | Ported from 541: manifest `browser_startup`, `FontCatalog::load_browser_startup_bytes`, `TextRenderer::with_browser_font_system`, demand/install, browser emoji shaping | fixed |
| 18 | App image stores spawn decode threads | `tilo-ui::image_store` | Ported from 541: `ImageStore::with_host_factory_with_max_dimension` | fixed |
| 19 | Apps depend on `tilo-app` with native defaults | every app (`apps/*`) | Per-app `native` feature forwarding `tilo-app/native`, default on; browser builds use `default-features = false`. Notes also needs `portal` under `native` (`tilo-ui/portal` implies `native`) and `tilo-content` without `device` (`device` → `runtime-fs` → tokio `net`) | remaining |
| 20 | Shell desktop (`crates/session/shell`) is native | shell | The portable part is `tilo-desktop-surface` (below): the toolkit's strip widgets over `tilo-desktop-model`. Pure tree operations the shell owned (`reconcile_strip_topology`, tab removal) moved into `tilo-desktop-model`; the shell delegates. Desktop search is shared through `tilo-desktop-search` (result kinds, the cross-provider ranking, portable providers, the palette session); the surface draws it with the toolkit's `CommandPalette` on Super/Cmd+K, Ctrl+K off macOS, Super+Space and the `global-search`/`commands` navigation commands. Other shell chrome (top bar, wheel, overview) stays native | fixed (surface, search) |
| 21 | `std::time` clocks panic at run time | `tilo-clock`, `qdoc::history` | `tilo-clock` uses `web-time` on wasm (target-only dep); `qdoc` stamps commits through `tilo-clock`. Other direct `SystemTime::now`/`Instant::now` callers remain run-time hazards | fixed (record flush path) |
| 22 | Strip widgets hide their children | `tilo-ui` `GroupRow`/`WindowGroup` | `GroupRow::exposing_children` (opt-in) reports groups, handles, tab bars and active tiles in layout order, so in-process components get semantics, focus and lifecycle | fixed |
| 23 | `ring` C code | `ring` | Builds with Apple clang 21 and upstream clang (wasm32 target). CI needs a clang with the wasm backend. Apple `ar` writes empty archives for wasm objects. That only breaks linking when a crate needs ring's C code at link time (it did while iroh, whose `iroh-relay` links a `cdylib`, was in the bundle); if it recurs, point `CC_wasm32_unknown_unknown`/`AR_wasm32_unknown_unknown` at LLVM (e.g. Homebrew `llvm@21`) | note |

With these, `tilo-ui --no-default-features` and `tilo-relay-client` (iroh
1.0.3, whose browser build dials relays over WebSocket via
`ws_stream_wasm`) check cleanly for wasm32.

### Run-time gaps (compile, but must be supplied by the browser host)

| Gap | Where | Browser implementation |
|---|---|---|
| Threads | `render_thread` (`ui-render`), `tilo_rt::app()` (a dedicated thread), `cpu_pool` (rayon), `ThreadPool` background executor, collaboration broker worker | *Done except the broker.* Render on the page thread (`FrameHarness` + Vello on the canvas). `tilo_rt::app()` on wasm is a `LocalSet` polled by one `wasm_bindgen_futures::spawn_local` task (same `spawn`/`spawn_then`/`spawn_blocking` signatures; `block_on` refuses with `UiThread`; tokio `time`/`net` have no driver). `cpu()` runs as an app task, `cpu_pool()` is the page thread alone, `tilo_rt::io` refuses. `tilo-host`'s default `Background` on wasm is `PageExecutor` (microtasks + `setTimeout`). The broker worker is behind `broker-transport` (milestone 5). Frames follow input, component wakes, animation and wake deadlines only (`FrameHarness::frame_schedule`, as the native runner): an idle mount presents nothing. |
| Files | `settings-store`, `app-state::FsStore`, `xdg-paths`, `tilo_files::native::atomic`, `advisory-lock`, `font_catalog` file sources | *Done.* `DurableStore` = `MemoryStore::queued` per profile/device, committed by `web/durable-host.mjs` in strict IndexedDB transactions under the `tilo-writer:[profile,device]` Web Lock; the store's write hook schedules commits, `flush()` resolves when durable. Settings: `settings_store::SettingsSource`, installed by the page as a `HostDocument` kept in the same store (`StorageKey::Package("so.tilo.settings")`), optionally seeded by the host. Fonts from bytes (`load_browser_startup_bytes`). |
| Local daemons | none linked (milestone 5) | `CollaborationManager::with_transport` with a relay-backed `CollabTransport` (not wired yet); the certified peer installs its user as `ui::local_identity` ([Seams](#seams-the-page-installs)). |
| Fonts | cosmic-text `fontconfig` finds nothing | Verified product fonts from the manifest loaded as bytes before first frame. |
| Speech, spellcheck | `tts`, `spellcheck` | *Done.* `NullSpeaker`. The English dictionary is compiled in, so spell checking works in-process; the warm-up thread is skipped on wasm. Other languages would need dictionaries fetched as assets. |
| Clipboard, pickers, URI, notifications | `HostServices` | *Done* (`src/host_services.rs`, `web/host-services.mjs`). Clipboard: trusted clipboard events plus `execCommand('copy')` for clicked Copy (the async Clipboard API cannot give Cut the synchronous answer it needs). Files: `FilePicker::pick_contents`/`save_contents` (new, with path-reading defaults for native hosts) answered by `<input type=file>` and a download; path pickers refuse. URIs: `window.open` for http/https/mailto/tel only, without an opener. Notifications API with permission requested on first use. |
| TCP pairing | `identity_flow` | Replaced by relay-mediated certification. |

### Seams the page installs

Each process-wide input has exactly one seam, replaceable at run time, with
native behaviour unchanged when nothing is installed:

| Input | Seam | Native (nothing installed) | Browser |
|---|---|---|---|
| Desktop settings | `settings_store::{SettingsSource, install_source, load, stamp}` | `settings.json` and its mtime | `HostDocument` over the profile's durable store (`so.tilo.settings`), seeded by `mountDesktop({settings})`/`openProfileStorage` |
| Local person | `ui::local_identity::{LocalIdentitySource, install_source}` | `RuntimeIdentity` (`local-daemons`) | `FixedIdentity` of the certified user, installed by `WasmPeer.start`, removed on `dispose`; mounts without `author` attribute edits to it |
| Device key | `tilo_relay_client::DeviceSigner` | a `SigningKey` | page custody (`device-custody.mjs`, one non-extractable key per profile) |

`DeviceSigner` is not a second identity seam: it signs with the device key
and never names the person; the person comes from the root-signed
`DeviceCert` custody obtained, which is what `local_identity` publishes.
Daemons' `ui::settings_source` was folded into `settings_store` (which also
merges writes, which the toolkit's settings readers and apps need).

### Local daemons

`tilo-ui`'s `local-daemons` feature (part of `native`; `broker-transport`
requires it) owns the clients of the local session daemons and the file
they share:

| Dependency | Portable replacement |
|---|---|
| `runtime-portal` (inherited service descriptors: floating-surface service, attachment) | `FloatingSurfaceServiceClient::discover` returns `None` |
| `runtime-identity` (launcher-bound identity, `primary.json`) | `ui::local_identity::{LocalIdentitySource, install_source}`; the native default is `RuntimeIdentity` |
| `settings.json` reads | Not gated: `settings_store::{SettingsSource, install_source}`; nothing installed natively (the file), the page installs a `HostDocument` |
| `runtime-collab/client`, `capnp`, `capnp-rpc` | already behind `broker-transport`; the host passes a `CollabTransport` |

`runtime-portal` itself gained a default `rpc` feature (`CapnpClient` and the
service-opening helpers); `cli`, reached through `app-state`, uses only its
discovery half, so capnp leaves the wasm graph too.

Measured with `cargo tree -p tilo-ui --target wasm32-unknown-unknown
--no-default-features -e normal --offline`: 1058 → 1012 lines, 387 → 376
distinct crates. Dropped: `runtime-identity`, `capnp`,
`capnp-futures`, `capnp-rpc`, `futures`, `futures-channel`, `futures-io`,
`futures-sink`, `tokio-macros`, `tokio-util`. The bundle-size effect is
negligible: on wasm these were reachable only from `cfg(unix)` callers or,
for capnp, not at all, so the linker already discarded them.

Still in the wasm graph, left for later:

- `app-state` → `cli` → `runtime-portal` (discovery only), `advisory-lock`,
  `xdg-paths`: `FsStore` and the package path helpers share
  `app-state/src/lib.rs` with `DocOwner`/`DocWriter`. Splitting `FsStore`
  behind an `app-state` feature would drop `cli`, `advisory-lock` and
  `anyhow`; `xdg-paths` stays through `spellcheck`.
- `tts`: on wasm only its vocabulary (`Speaker`, `Priority`, `NullSpeaker`)
  compiles; the SSIP backend is `cfg(unix)`.
- `spellcheck`: pure Rust, but embeds a ~690 KB word list. Candidate for a
  feature or fetched dictionary if the bundle budget needs it.
- `media` (with `rustfft`), `syntect`/`two-face`, `image`, `openmls` and
  `threshold-identity`: portable and used by portable widgets.
- WebView/adblock, WPE, GPU video, MPRIS, XDG portal and notifications were
  already opt-in features and are not in the portable graph.

## Transport from browser to machine

The network model is the "Figma-style" one PR 541 built for qOS: one
authoritative host per workspace, clients that edit a local replica
optimistically, and a separate low-latency live plane for ephemeral state.
What changes on `main` is where it lives (in `tilo-relay`, not a parallel
host) and the carrier.

### What PR 541 built (reference)

- **One serialized authority per workspace.** `tilo-host::owner_turn`
  (commit `408bde012`) defines `HostOwner`: the host's single serialized
  owner (the device host's committed publication, or the Cloud Store).
  Services never hold the owner. They borrow one read-only `OwnerTurn`
  (committed peer state, room policy, workspace, owner continuity, trusted
  clock) per decision. Every durable change is admitted inside the owner:
  signature, author certificate, role and capability, and canonical form are
  checked, the change is appended to a host-ordered replay log, and a store
  barrier runs before the change counts as delivered.
- **Durable plane: signed Automerge changes over an HTTPS delivery
  carrier.** The browser uploads its own signed changes from a durable
  outbox (`POST /peer/v3/state`). It reads the host-ordered replay through
  held long-poll chains (`/peer/v3/events`) starting at a host-signed opaque
  cursor, and acknowledges consumption (`/peer/v3/acks`). A reconnecting
  browser resumes from its cursor and re-uploads anything unacknowledged.
  The host de-duplicates by digest/operation id. When history is gone it
  answers with a reset to a snapshot (`/peer/v3/snapshots`). Every request
  carries a device-signed HTTP proof.
- **Conflicts** are CRDT merges (Automerge: deterministic per-property
  last-writer-wins, the same outcome on every replica). The host decides
  *admission*, not merge results.
- **Live plane (`LIVE_V1`).** Ephemeral and never authority
  (`tilo-delivery-protocol::delivery_v3::live_wire`):
  - A latest-value `presence` track (datagrams): `PresenceFrame {seq,
    sent_at_ms, focus, pointer, selection anchors, drag}`. Receivers drop
    older sequence numbers.
  - One ordered `ops/<record>` track per record: `OpsFrame::Preview` (text
    preview ops against CRDT anchors) and `OpsFrame::SignedChange`, an
    early forward of a durable change.
  - The host relay validates every frame with the canonical codec,
    rate-limits it (120 presence/s, 240 ops/s, 16 open records), and stamps
    the publisher namespace.
  - The shared `LiveService` (commit `67c31b331`) issues 60-second
    host-signed live grants and re-checks each session's current roster
    permission from one shared owner turn every 50 ms, so revocation cuts
    live sessions within about 100 ms.
  - The carrier is moq-lite over WebTransport (`/moq/live`), with an HTTPS
    streaming fallback (`/peer/v3/live/fallback/*`). There are no WebSockets.
- **Browser speculation.** `peer_live/speculation.rs` keeps a *durable
  basis* plus an in-memory *view*: basis + early-forwarded changes. The
  view is what renders. A durable cut settles each outstanding change:
  promoted if covered or still admissible, withdrawn after 10 s otherwise.
  A local edit promotes everything it builds on.
  - `live-presence.mjs` coalesces local presence to at most 120/s, with
    loss repeats at 20/200 ms and a 3 s keepalive.
  - For remote peers it interpolates with a jitter-adaptive 4–34 ms delay,
    extrapolates at most 80 ms, snaps ended drags, keeps a released drag
    until the durable change lands (or 5 s), and marks peers stale after
    10 s.
  - `live_view.rs` maps editor carets and selections to compact CRDT-keyed
    anchors. `live-overlay.mjs` draws remote pointers and drag ghosts on a
    separate layer.

### What `main` does

The same model and 541's carriers, served by `main`'s host (`tilo-relay`)
rather than a second host:

- **The authority is `tilo-relay`'s workspace runtime.**
  - Each workspace is serialized behind one lock that validates the author
    against the authenticated device, checks membership, persists the change
    (retained checkpoint/tail or per-device pending queue), and fans it out.
  - `Envelope::Commit` is a `Delta` that asks for an acknowledgement. The
    host answers `Committed {op, seq}` once the change is persisted, with
    `seq` in host order, and fans it out as an ordinary `Delta`, so native
    shells interoperate unchanged.
- **Browser sessions are ordinary relay peers.**
  - After the same `DeviceCert` challenge handshake an iroh peer runs, the
    host starts its own `run_peer` loop over an in-process duplex stream.
  - The HTTPS routes only move framed `Envelope`s in and out of it. That
    one loop runs subscribe, commit ordering, store-and-forward, the live
    plane and the revocation audit.
  - An HTTPS peer and an iroh peer therefore see one host order
    (`tilo-browser-peer/tests/host_order.rs`).
- **Durable plane over HTTPS.** Routes live under `/peer/v1`
  (`tilo_relay_protocol::web`); bodies are postcard.
  - `/session` and `/session/finish` run the handshake and return a
    session token.
  - `/send` carries the browser's envelopes (signed
    `NeighbourMsg::AppDocument` commits: the shell's own wire, with
    `channel-crypto` ciphertext on encrypted routes).
  - `/events` is a long-poll that returns the host's envelopes after a
    host-MAC'd cursor and acknowledges the previous one. A page carries at
    most 1 MiB of payload (and at least one envelope), so a late join's
    checkpoint and tail stream as acknowledged pages.
  - The browser applies its edit to its local replica first and keeps it in
    a persisted outbox until `Committed`. The host keeps a durable ledger of
    admitted commits by author and operation id: a resend is answered with
    its original order and neither persisted nor fanned out again.
  - Documents larger than one frame travel as signed segments; the session
    reassembles them (bounded, digest-checked) before a mount sees them.
    The page merges one drain's changes per record in one pass (a
    checkpoint, then its tail as one Automerge change set), so catch-up is
    linear in the history.
  - Reconnect runs the handshake again, re-subscribes (the host replays
    its retained coverage and this device's queue), and resends the whole
    outbox; the ledger absorbs what was already committed.
  - A session holds at most 16 MiB of unacknowledged events. Past it the
    host stops reading for that session; a room whose live deliveries the
    connection could not take resumes from its position once the queues
    drained, so the session stays and nothing is dropped.
  - The host keeps each record's retained history short. Once a route's
    tail grows (256 entries, or 1 MiB and half its snapshot) and the route
    has been quiet for 2 s, the host folds every cleartext entry it can
    verify into one Automerge snapshot signed as the host device (publisher
    `peer_handle(host)`), retained but not fanned out, and removes exactly
    the entries whose every change the snapshot holds, by change hash.
    Entries retained during the fold, and anything it cannot read, stay.
    The host's certificate is retained beside its snapshot, so a replay
    carries it first. A sealed route the host cannot read is compacted by
    a member: the host asks a page that volunteered
    (`Envelope::CompactRequest`), the page's runtime answers with its
    replica, the session commits it sealed and then sends a device-signed
    `CoverageStatement` naming the retained entries (by digest of their
    body) it had delivered or published before the request. The host
    removes exactly those. Rooms whose replay a native shell selects
    (`shell_authority`) are left to that selection.
- **Live plane over WebTransport.** It uses 541's `LIVE_V1` relay
  (`crates/hosting/tilo-moq`, ported from `qos-moq`), `moq-lite` at
  `/moq/live` on the same port number over UDP.
  - The frames are the vocabulary in `tilo-relay-protocol::live`, validated
    with the host's canonical codec.
  - A grant (`/live/grant`) is bound to the HTTPS session and presented in
    the carrier's admission preface.
  - One observer checks every grant every 50 ms (session open, device not
    revoked, still admitted), and the relay re-checks each session against
    that snapshot. This is 541's batched `LiveService` observation, run on a
    tokio interval.
  - Browsers without WebTransport use 541's HTTPS stream fallback
    (`/peer/v1/live/fallback/{open,send,resume}`) with the same frames.
  - Live frames from iroh peers are bridged into the relay, so native and
    browser members share one live room.
- **Early forwarding is dropped.** In 541 it hid long-poll latency. Here a
  browser's own edits are optimistic locally until `Committed`, and live
  previews and drags are overlays until the record's durable change lands
  (`durableChanged`). Remote durable changes arrive already persisted,
  through `/events`.
- **No owner loop.** 541's HTTPS owner ran `service.handle`/`maintain`
  synchronously inside a `tokio::select!` loop and retried with
  `std::thread::sleep`. Here every handler is async and awaits the relay's
  own locks, workspace I/O already runs on the blocking pool, and the only
  periodic work (the 50 ms observation and the session janitor) runs on
  tokio intervals. No thread ever sleeps in async code.
- **The browser side** is `crates/browser/tilo-browser-peer`: custody,
  session, outbox and replay, live state, and the `CollabTransport`.
  - It reaches the host through an `HttpClient` seam (`fetch` in the page).
    iroh is an optional feature for native peers, and the wasm bundle links
    no iroh, noq or quinn.
  - The page's live client is 541's `web/live-transport.mjs` and
    `web/moq-carrier.mjs` over the bundled `@moq/net`, plus
    `web/live-fallback.mjs`.

**Where it runs.** The listener is `Relay::serve_web(WebConfig)` in
`crates/hosting/tilo-relay/src/web/`, behind the `web` feature. HTTPS runs on
TCP (HTTP/2 and HTTP/1.1, rustls) and WebTransport on UDP, both on
`WebConfig::listen`. `allowed_origins` lists the console origins allowed to
call it cross-origin.

**Certificates.** The page must trust the host's certificate for both
HTTPS and WebTransport:

- **Development:** `TlsSource::Development` generates a self-signed ECDSA
  P-256 certificate valid 13 days. `POST /peer/v1/host` publishes its
  SHA-256, and the page pins it for WebTransport with
  `serverCertificateHashes` (limited to certificates valid at most 14 days).
  For HTTPS the developer trusts it once, or serves the page from the same
  origin.
- **Home host:** `TlsSource::Files` loads a PEM chain and key from an ACME
  client (certbot, lego, Caddy) for a name that resolves to the host.
  Restart the listener after renewal.
- **Cloud machine:** `TlsSource::Files` with the Cloud-managed certificate
  of the machine's name. Alternatively, terminate TLS at the Cloud edge, but
  the edge must pass UDP through (or terminate HTTP/3 and forward
  WebTransport), which ordinary HTTP load balancers do not.
- `TlsSource::Managed` (ACME inside the host) is a configuration stub that
  refuses with this guidance.

**QUIC stack.** iroh uses `noq`, its quinn fork. WebTransport and MoQ exist
only for quinn (`web-transport-quinn`, `moq-net` over
`web-transport-trait`), and there is no noq implementation. The `web`
feature therefore accepts quinn for this listener only:

- Hosts that do not serve browsers link no second QUIC stack.
- The feature costs quinn, quinn-proto and quinn-udp, web-transport-quinn
  and -proto, moq-net, and a platform verifier: 16 lock entries.
- A noq port of `web-transport-quinn` (about 2k lines over
  `web-transport-proto`) would remove the duplication.

Alternatives considered: iroh through an iroh relay server over `wss://`,
which this branch did first. It needs no host certificates, but it requires
Cloud to run an iroh relay server and routes all traffic through it. The
native iroh carrier remains for native peers.

### Device certification

An uncertified browser holds only a device key. It sends a
`CertificationRequest` (its device key, a label and a nonce, self-signed as
proof of possession) to `POST /peer/v1/certify`, then long-polls
`/peer/v1/certify/wait` for the answer. Native devices use the ALPN
`tilo-device-certify/1`. Both share one pending set on the host.

- The host queues the request. It is shown to approvers as a six-digit
  pairing code that the page also displays.
- An approver is a device of the same user that holds root custody
  (`can_admit_devices`). It authenticates with its own `DeviceCert` on the
  same ALPN, lists pending requests, and answers each one with a root-signed
  `DeviceCert` or a denial.
- The host checks the certificate against the request and the browser
  policy (no `can_admit_devices`, no `can_act_as_host`, bounded expiry, not
  revoked), then hands it to the waiting browser.
- The host never holds the root. Revocation is enforced at request, at
  issuance and at every later handshake.

### A browser-held root

A person with only a browser holds their user root in the page
(`web/root-identity.mjs`). The root is generated with WebCrypto and stored
only wrapped, as a root envelope that any server may keep:

```json
{"version": 1, "root_public_key": "<hex>", "wraps": [
  {"kind": "passkey", "credential_id": "…", "rp_id": "…", "prf_salt": "…", "nonce": "…", "ciphertext": "…"},
  {"kind": "recovery", "salt": "…", "nonce": "…", "ciphertext": "…"}]}
```

Each wrap is AES-256-GCM over the root's PKCS#8 encoding under
HKDF-SHA256 of a passkey's WebAuthn PRF output or of the recovery key (160
random bits, shown once as `XXXX-XXXX-…` Crockford base32), bound to its kind,
the root public key and the credential. `tilo_user_identity::root_envelope`
reads and writes the same format natively. Unlocking unwraps straight into a
non-extractable `CryptoKey`; the root never exists in the clear outside the
page's memory.

Everything below is exported by `root-identity.mjs` and re-exported by
`peer.mjs`. The identity created or unlocked last is the page's current one
and the default wherever an `identity` is taken.

```ts
createIdentity({rpId, userName, userId, displayName?, passkey?: boolean, prfSalt?})
  : Promise<{envelope, recoveryKey: string, identity: RootIdentity}>;
unlockIdentity(envelope, {passkey: true, rpId?} | {prfOutput, credentialId} | {recoveryKey: string})
  : Promise<RootIdentity>;
addPasskey(envelope, {identity?, rpId?, userName?, userId?}?): Promise<envelope>;
// A native node that hosts and governs the person's workspaces: host, owns
// and reads all workspaces, admits no devices, one year. Never the root.
certifyDevice(nodeDevicePublicKey | {devicePublicKey, identity?, runtime?, label?, role?: 'node' | 'browser', encoding?: 'canonical' | 'json'})
  : Promise<string /* canonical DeviceCert, base64 (what a native device keeps on disk); or JSON */>;
installNodeCertificate({origin, certificate, installToken}): Promise<{device_public_key, user_public_key}>;
// The page's own device, certified by the root itself against the host's
// certificate (which must be a host of this root), no approver. Defaults:
// a profile per identity, its device key, the page host's runtime.
connectPeer({origin, identity, signal, …}): Promise<CertifiedPeer>;
// Another person's workspace: what an invitation offers, then join it as
// this identity (its own root certifies the device; the owner admits it).
inspectInvitation(relayUrl, invitation): Promise<{workspaceName, role, room, workspaceId, expiresAtNs, inviter?: {name, userPublicKey}}>;
joinWorkspace({origin, invitation, identity?, displayName, signal, onProgress?}): Promise<CertifiedPeer>;
// onProgress({phase: 'asking' | 'waiting' | 'admitted'}); rejects when declined or refused
// RootIdentity: {kind, publicKey, userPublicKey, sign(bytes), addPasskey(envelope, opts?), lock()}
```

A peer connected to a room its host governs administers it as the owner
(`web/membership.mjs`; each call is one `/rooms` request the host
authorizes):

```ts
peer.createInvitation({role?: 'editor' | 'commenter' | 'viewer', expiresIn?: seconds /* ≤ 30 days, default 7 */})
  : Promise<string /* invitation text, base64url */>;
peer.membership.list(): Promise<{room, name, generation,
  members: [{userPublicKey, name, role, owner, removed}],
  waiting: [{requestId, userPublicKey, devicePublicKey, name, role, requestedAtMs}],
  invitations: [{inviteId, role, expiresAtMs}]}>;
peer.membership.admit(requestId) / decline(requestId) / setRole(userPublicKey, role)
  / remove(userPublicKey) / cancelInvitation(inviteId);
// The rooms the host governs, before the peer is in one (connect again with `room`):
peer.workspaces.list(): Promise<[{room, name, generation, members}]>;
peer.workspaces.create(name) / enter(room): Promise<{room, name, generation, members}>;
```

An invitation names its inviter: the host records the user root of the
device that asked and the `displayName` the peer connected with, and signs
both into the invitation (`tilo_peer_state::join::Inviter`, invitation
version 2), so `inspectInvitation` shows who sent it as the host attests.

`createIdentity` refuses a passkey without PRF (`IdentityError` code
`prf-unsupported`); `passkey: false` creates a recovery-key-only envelope.
All of an identity's passkeys share one PRF input (`prf_salt`; random unless
`prfSalt` is given), so a login that already asserted a passkey with
`extensions.prf.eval` of it unlocks with `{prfOutput, credentialId}` and no
second prompt. `addPasskey` excludes the envelope's existing credentials, so
the new passkey lands on another authenticator instead of replacing one; its
`userName`/`userId` default to those the identity was created with.

A node provisioned without a certificate (`tilo-relay run --web …
--install-token-file PATH`) creates its device key, prints
`{"event": "awaiting_certificate", "devicePublicKey": …}` and serves only
`POST /peer/v1/install` (the certificate as JSON or canonical bytes, the token in
`x-tilo-install-token`; every other route answers `503`). Once a valid host
certificate for its key is installed it starts as that identity's host on the
same origin.

Browsers accept a device as a room's governor when its certificate owns
workspaces (`can_own_workspaces`), so such a node governs and stewards the
person's rooms; a plain host certificate never does. A page whose host is
such a node takes it as the governor of every room it joins there, also a
room of another person it was invited to, unless a genesis frame from a
device of its own person names another governor.

The node governs the rooms created on it through `/rooms` (the company
workspace) and stewards their channels (`tilo-relay` `rooms/stewardship.rs`):
it admits to a channel exactly the room's governed member devices that ask
for it, removes those governance removed, and retains the channel's
governance frames as its own writes. Members' records reach the relay only
sealed; it keeps no copy of a sealed record. Channel state is sealed on disk
under a key derived from the node's device key. `tests/node_workspace.rs`
(`tilo-browser-peer`) runs the whole path: a passkey root in Chrome
certifies and installs a waiting node, the page writes a document the node
retains only as ciphertext, and a person invited by name joins and reads its
whole history.

### Live layer interface

The live and speculation layer is its own module so the desktop mount
consumes it without owning it. The desktop does not reimplement 541's
desktop speculation. It uses these two interfaces.

**Rust: documents.** `tilo_browser_peer::transport::RelayCollabTransport`
goes into `CollaborationManager::with_transport`. `WasmPeer::collab_transport()`
hands one to a mount inside the runtime crate.

- It joins the route's room and live plane on `Hello`, and acknowledges the
  primary route when the host answers `Ready`.
- It publishes document mutations as signed commits. Inbound documents
  arrive already verified as `CollabEvent::Document`.
- Its publication barrier completes when the host has persisted every
  earlier change.
- Toolkit presence maps onto the live plane:
  - `Pointer` becomes `PresenceFrame.pointer`.
  - `Caret` and `Selection` become anchors
    `[1, uvarint(start), postcard(PresenceKey)]` and `[2, uvarint(end)]`,
    as in 541's `live_view`.
  - Remote frames come back as `RemoteCursorMoved` and
    `RemotePresenceUpdate`, so the editors draw remote carets natively.
- A durable change for a record drops that record's remote previews.

**JS: overlay state.** `certifiedPeer.live`, from `web/peer.mjs`
`createLiveLayer`, is one room's live layer and serves overlays (remote
pointers, drag ghosts, text previews):

```ts
interface LiveLayer {
  room: string;
  publishPresence(partial: Partial<{focus: LiveRecord | null; pointer: [number, number] | null;
    selection: {anchor: string; head: string} | null; drag: LiveDrag | null}>): boolean;
  publishPreview(record: LiveRecord, ops: LivePreviewOp[]): void;
  endPreview(record: LiveRecord): void;
  watchRecord(record: LiveRecord): () => void; // receive previews for a record
  durableChanged(record: LiveRecord): void;   // local durable change landed
  readonly state: 'connecting' | 'live' | 'reconnecting' | 'unavailable' | 'closed';
  remoteState(nowMs?: number): Array<LiveRemoteState & {previews: Record<string, LivePreviewOp[]>}>;
  members(): string[];
  people(): Array<{device: string; name: string; color?: string}>; // named by their presence
  agents(): Array<{publisher: string; agentId: string; name: string}>;
  readonly publisherId?: string; readonly agent?: string;
  onPresence(cb): () => void; onPreview(cb): () => void;
  onMember(cb): () => void; onDurable(cb): () => void; onAgentActivity(cb): () => void;
}
```

The value shapes are PR 541's `live.d.ts` (`LiveRecord`, `LiveDrag`,
`LivePreviewOp`, `LiveRemoteState`: the pointer is interpolated one frame
behind, drags snap on end or cancel, released drags are kept until the
durable change or 5 s). Publishing is coalesced to at most 120 frames/s with
loss repeats and a keepalive (`live-presence.mjs`, unchanged from 541).

- The grant is asked for once the room admitted the device (`phase` is
  `ready`); the relay refuses it before.
- Every mount publishes through its own view of the layer
  (`live-speakers.mjs`): the mount the person acted in last (pointer, key or
  focus inside it) speaks, and says its caret again when it takes over, so
  an idle or hidden mount never overwrites the person's caret.
- A mount publishes the focused window's editor caret when keyboard focus
  is outside that window (`tilo_desktop_surface::published_selection`).
- `certifiedPeer.agentLive(agent)` is the layer of an agent the page runs:
  its grant (`WasmPeer.liveGrant(room, agent)`, `/live/grant/agent`)
  publishes under the agent's own namespace, labelled as the agent; a mount
  with `agent` publishes through it.
- `whenReady()`, `records()`, `hasRecord(app, record)` and
  `whenRecord(app, record)` expose the room's admission and the records the
  page holds. A package's room document is the record `''` on the wire, as
  native peers send it.
- `web/relay.mjs` `connectRelayPeer({origin, userPublicKey, room, label,
  onPairingCode})` opens the profile's device key and connects in one call
  (the API `tilo-room-service`'s `rooms.mjs` uses). A room on a loopback
  `http:` origin is accepted for local review: WebTransport goes to its TLS
  port (`webtransportPort`), the fallback to the plain origin.

### JS API Cloud targets

```ts
import {openDeviceCustody, idbStorage} from '/tilo-runtime/device-custody.mjs';
import {connectPeer} from '/tilo-runtime/peer.mjs';

const custody = await openDeviceCustody({profile, storage: idbStorage()});
const peer: CertifiedPeer = await connectPeer({
  origin,             // host HTTPS origin, e.g. 'https://machine.example:8443'
  userPublicKey,      // hex user root public key (never the secret)
  label,              // shown to the approving device
  room,               // room uuid to join
  custody,
  profile, device,    // the profile's durable storage keeps certificates and the
                      // outbox; desktops mount in it (preferred)
  store,              // or {get, put, delete} for public peer state, without a profile
  displayName,        // the person's name for history and presence
  runtime,            // the wasm module (hostInfo, certifyDevice, WasmPeer, live codec)
  fetch,              // optional; defaults to globalThis.fetch
  liveCarriers,       // optional; defaults to WebTransport with the HTTPS fallback
  signal, onStatus, onPairingCode,
});

interface CertifiedPeer {
  readonly host: {hostCert: DeviceCert; webtransportPort: number; certificateSha256: string | null};
  getSnapshot(): PeerSnapshot;   // phase: authenticating|admitting|ready|incomplete|failed|closed
  subscribe(listener: (snapshot: PeerSnapshot) => void): () => void;
  readonly live?: LiveLayer;
  readonly agents?: Agents;      // see "Agents"
  mountDesktop(target: HTMLElement, options): Promise<DesktopMount>;  // host.mjs mount in the peer's
                                 // profile and room; members' pointers drawn over it
  flush(): Promise<void>;        // every published change durable on the host
  reconnect(): Promise<void>;
  recover(request: {kind: 'retry-connection'} | {kind: 'reauthenticate'}): Promise<{operationId: string}>;
  setLocalState(app: string, room: string, record: string,
    state: 'unknown' | 'pending' | 'committing' | 'durable' | 'failed'): void;
  dispose(): Promise<void>;
}
```

`PeerSnapshot` carries `health: SessionHealth` v1:

- `transport.phase` (`connecting|connected|reconnecting|offline|failed|closed`),
  `authenticated`, contact times and `retry`.
- `admission` (rooms waiting for `Ready`).
- `records[]` with `outgoing.pendingOperations/durableOperations`,
  `acknowledgement` (`pending|covered`, backed by host-ordered `Committed`),
  and `local` (the page store's save state, via `setLocalState`).
- `failures[]` with codes (`transport-unavailable`, `transport-ended`,
  `certificate-expired`, `certificate-rejected`, `permission-required`,
  `protocol-rejected`, `signature-rejected`, `storage-transaction-failed`)
  and applicable `recovery` requests.

Compared with the qOS contract: `enrollmentBaseUrl`, invitation text and
`cloud.enrollment` are replaced by the host `origin` + `userPublicKey`
plus a pairing code; `retryPersistence`, `actionsV1/V2` and `mediaV1` are
not here yet (milestone 7). Agent control, the agent half of the qOS
`agentsV1`, is `peer.agents` (see "Agents").

## Agents

Agents work in a room the way a person's device does. They are not a side
channel into apps. They reach an app through the same door a person's
input reaches, their edits go through the same host order, and their
cursor is drawn by every participant.

### Survey of `main`

- **`tilo_actions`** is the action model. Apps publish a `Resource` tree
  whose `ActionSpec`s are replayable Automerge mutations or view and effect
  operations. `door::survey` turns the tree into inert, stably addressed
  descriptors, and the door resolves an address and binds arguments
  strictly. `tilo_actions::remote` carries the door across machines:
  `RemoteActionFrame`, `CatalogueQuery`, `InvocationLedger` for idempotent
  invocation ids, `ActionHost`, and a `Caller` built only by the transport.
- **The semantic projection.** `tilo_ui::a11y_tree::SemanticTree::action_resources`
  adds a `ui:<node>` resource with `focus`, `activate`, `set-value`,
  `increment` and similar actions for every operable control of every app.
  An app's published surface is its declared resources plus that
  projection. This is the generic path to parity: whatever a person can
  click is an addressable action, with no per-app code.
- **Native desktop.** The shell's `BrokerActionHost` is the one
  `ActionHost` over every running app's published surface. The palette,
  shell-network peers (`PeerActionHost`), the MCP bridge
  (`runtime_host::mcp`, `tilo-runtime-mcp`) and local agents
  (`Caller::local_agent`) all go through it.
- **`tilo-relay`** routes `Envelope::RemoteAction` between the devices of a
  workspace. It answers frames addressed to itself through an
  `AppServingPlane`, but nothing in production supplied one before this
  branch.
- **`apps/agent` (`so.tilo.agent`)** joins one word-processor document
  through the local broker and links the word processor, chat and photos
  crates to reuse their actions. Its model planner (`tools.rs`,
  `plan.rs`) chooses from the discovered catalogue and cannot name an
  operation it was not shown.
- **Headless drive** (`--headless-drive`, `Driver::invoke_action`) invokes
  published actions by address through the same resolution, availability,
  confirmation and binding as the live host. `tilo-automation` Shortcuts
  lower to the same catalogue.

The mechanism chosen for browser sessions is generic: **mount the app's
`ApplicationDefinition` component headless on the room's record and use
only its published surface.** This is the same component the browser
runtime mounts, so an agent reaches exactly what a person in the page can.
Declared actions carry an app's domain vocabulary; the semantic projection
covers everything else. On a native desktop, the same agent code can
target `BrokerActionHost` instead, because both are the same door.

### Identity and consent

An agent is a device of the user:

- **Its own device key.** It is certified by ordinary device
  certification, and the root-holding approver is where consent is given:
  the request is labelled `Agent: <name>`.
- **A capability-less certificate.** The approver issues it with
  `CertificationPolicy::agent()`. `verify_issued_cert` now accepts a grant
  narrower than the request.
- **A root-signed `AgentDelegation`** (`tilo_relay_protocol::agents`)
  naming the agent id, label, cursor colour, rooms and apps, whether it
  may satisfy confirmation steps, and an expiry.

**One consent step.** A page asks the host for a grant
(`agents#request-grant {label, rooms, apps, expiresInSecs}`). The host
generates the agent's key and queues a certification request labelled
`Agent: <label>` that carries the `AgentProposal` (label, colour, rooms,
apps, confirmation, expiry). An approver lists proposals
(`ApproverRequest::AgentProposals`) beside the pending request and answers
with `ApproverDecision::IssueAgent {cert, delegation}`: the capability-less
certificate and the root-signed delegation together. The host refuses a bare
`Issue` for an agent request, a delegation for another device, and one wider
than proposed (`AgentProposal::admits`), so an agent never exists without its
delegation and the host never holds the root. `tilo-relay dev`'s approver
approves proposals as they come; a production approver shows them.

`Relay::register_agent` verifies the delegation against the host's user:

- The device is refused outside its rooms, at subscribe and on every
  one-second access audit.
- `Relay::withdraw_agent` refuses it entirely.
- Remote actions from the device carry `Caller::delegated_agent`: the
  agent is its own principal, never the person who delegated it.

Browsers get the delegation (hex) in `AgentInfo.delegation` and verify it
in wasm (`verifyAgentDelegation`) before calling an agent "yours".

### Participation (`crates/agent/agent-participant`)

`AgentParticipant` holds a `tilo_browser_peer::PeerHandle` under the
agent's key. A host-side agent reaches the host over iroh; the session code
is the same one a page runs.

- **Joining.** It joins the room's durable and live planes. The host's
  `Ready` follows its replay, so a task never starts on a half-seen room.
- **Opening an app.** `open(app, record)` seeds the record from the
  durable changes the host replayed and mounts the component (`HostedApp`)
  as an agent principal holding the editor role. Without a record it
  creates one and publishes it.
- **Which apps.** `component_apps()` is the browser desktop's registry:
  `tools/web/generate_web_desktop.py` writes the same manifest-derived app
  list into agent-participant (`src/web_apps.rs` and its Cargo block) as
  into `tilo-web-desktop`, and `--check` covers both.
- **Acting.** `invoke` goes through the app's door
  (`FrameHarness::invoke_action`), so availability, confirmation and
  argument binding are the app's own. It then commits the change the
  action made as a signed `Commit`. The host orders it with every other
  participant's change and fans it out; browsers receive it through
  `/events`, attributed to the agent's device.
  - `HostedApp::take_mutation` returns only this participant's own
    changes. Changes it received are never republished.
  - The first take of a new record is a snapshot.
- **Presence.** Around each action it publishes a presence frame: focus on
  the record, the pointer where a person would point (the control a
  semantic action drives; for a declared action the control whose name
  matches its label, else the record's own surface), and
  `participant: {kind: Agent, label, colour, activity}`, with a 1 s
  keepalive. `remote_members()` is everyone else's presence as the agent
  sees it. The pointer is in the focused record's own surface: a page
  draws it inside the window showing that record (`live-cursors.mjs`
  `placePointer` over the runtime's `desktop_record_windows`, from the
  desktop surface's `shown_tabs`), within its bounds, and not at all while
  no window there shows the record. Pointers without a focus stay in the
  page surface's coordinates.
- **Seeding.** Only a record's creator writes its initial content. The
  component adapter tells an adapted root how it came to its record while
  it is built (`ui::launch::record_launch()`: `Created` or `Joined`). A
  root on a record never reads or writes its app-wide state file (Sheets'
  `workbook.automerge` was imported into shared workbooks), and a joined
  root leaves an uninitialized record untouched until the creator's
  content arrives, because get-or-create reads would write a second root
  object that competes with the creator's in the merge and hides every
  edit made in the loser. Components (Notes) initialize only on
  `LaunchTarget::New` and refuse an uninitialized existing record.
  `replicas::only_the_creator_seeds_a_record` checks every web app.
- **Controls.** `operate(route, control, input)` does what a person's
  input does to an on-screen control: press, focus, set its value, type
  into it, scroll it, toggle, step, expand. It uses
  `FrameHarness::controls`/`operate_control` (below), commits the change
  and points the agent's cursor at the control.
- **Tasks.** Each task is planned by a `Planner` into steps
  (`StepAction`: invoke a published action, operate a control, open an
  app, or continue planning once earlier steps changed what is
  published). The planner sees only descriptors and controls, and every
  step is re-checked against the delegation and the door.
  - `DirectPlanner` reads steps written as JSON (`{app, resource|control,
    action, arguments}` or `{app, control, input, text|value|dx|dy}`).
  - `ModelPlanner` reads JSON steps the same way and sends any other text
    to a `PlanModel`. `GatewayModel` is `apps/agent`'s action discovery
    (`tilo_agent::tools` over a `surface::Catalogue`) through
    `tilo-model-gateway`. The model must list an operation before its plan
    may name it, and each answer is mapped back to an offered address, so
    `spawn({task})` takes plain words. Tests use a deterministic
    `PlanModel` over the same tools.

### Controls in the headless harness

`FrameHarness::controls()` lists every operable element of the exported
AccessKit tree — buttons, fields, tabs, toggles, list items, including the
synthetic elements a canvas or a framework view exposes — with role,
name, value, bounds and verbs. `operate_control(id, input)` dispatches
through `perform_a11y_action`, the route a screen reader's request takes.
When an exported element has no dispatch handle, it falls back to pointer
and keyboard input at the element's bounds. A control bound to an app action
runs through the door, and an unconfirmed confirmation step comes back as an
error. `invoke_action` now also runs projected `ui:<node>` actions.

Two `tilo-ui` fixes were needed to reach adapted apps at all:

- `AdaptedRoot`, the decorator every legacy root is mounted through,
  registers its layout node.
- The harness builds its semantic tree over the exported node map, like
  the `App`.

Before these fixes, no accessibility node under an adapted root resolved,
and no control was projected for it.

### Agents on a host (`AgentHost`, `tilo-relay --agents`)

`crates/agent/agent-participant/src/host.rs` runs a supervisor in a host
process. Each agent's participant runs on its own thread with the host's
model, pumping its session until it stops or the host shuts down. Grants
(agent key, certificate, delegation) and agents (status, room, task log,
conversation) are kept in `<state>/agents/agents.json` (owner-only). A
ticker settles expired grants: the agent stops and the relay refuses its
device. On start the file is restored: approved grants are registered again,
agents that were neither stopped nor failed start again with their queued
tasks, a task that was running at shutdown is marked failed, paused agents
come back paused, a grant still waiting for its approver is denied.

`tilo_relay::agents::start` (feature `agents`) wires it into the relay: the
supervisor is the app-serving plane, `RelayAgentLink` queues approvals on the
certification ALPN and registers and withdraws delegations, and the agents
reach the host over iroh. `tilo-relay dev --agents` and `run --agents` use
it. `--agent-model` chooses the planner:

- `settings` (default): the provider selected in Settings through
  `tilo-model-gateway` (`GatewayModel`).
- `openai:<base url>` with `--agent-model-name` and `TILO_AGENT_API_KEY`:
  `OpenAiModel`, the same discovery tools over chat-completions tool calls
  (OpenAI, vLLM, llama.cpp, Ollama, OpenRouter), called from the host.
- `scripted`: `ScriptedModel`, a deterministic keyword planner for tests and
  demos. It reads clauses (`;`, "then"), opens the app a clause names and
  fills text parameters from quoted phrases.

`ModelPlanner` also offers the room's existing records of an app before a
new one, so "add a sheet named …" edits the sheet people already have.

**Each agent has a profile of its own.** Agents' apps run in the host
process, so without care they would open the host user's files and
Keychain (TOTP's vault, Passwords, Sheets' workbook). Each agent's thread
runs under `tilo_paths::scope_profile(<state>/agents/<agent id>/profile)`,
which moves `home()`, the per-user base folders, the XDG variables and
`AppPaths::for_app` below that root and mounts no workspace, and under
`tilo_secrets::scope_thread` with a store of its own kept in memory (lost
when the host stops). Apps resolve their folders through `tilo-paths`
rather than reading `HOME` or `XDG_*` themselves. The planner and the
apps' theme and fonts stay the host's: planning runs `unscoped`, so
`settings` finds the host user's provider and gateway. `--agent-host-profile`
(`AgentHost::share_host_profile`) grants agents the host user's own
profile and secret store instead. Scopes are per thread: a worker thread
an app starts sees the process's folders, and a store keeps the backend it
was created with.

`host_agents` covers it end to end: a page requests a grant, a laptop
approves it with its delegation, the page spawns with a plain-words task, the
agent edits Notes and Sheets, the page merges the edits and sees the agent's
cursor move between the controls it used with its activity; after a host
restart the agent resumes on the room's sheet, a stopped agent stays
stopped, and revoking a grant ends its agent and forgets its key.
`a_browser_sees_every_task_of_an_agent_live_and_after_a_reload` has an agent
add two sheets to a workbook a page created and checks both live and from a
new session's replay; `agent_profile` checks an agent's apps keep to its
profile.

### Steering from the browser

`AgentSupervisor` holds the approved delegations (Cloud's grant offers)
and the running agents. It answers the vocabulary in
`tilo_relay_protocol::agents` as `tilo_actions` remote actions. A host
serves it as its `AppServingPlane`
(`ActionHostPlane::new(SupervisorHost(..))`), so a page steers agents with
the same frames `actionsV1` uses for apps. Only devices of the delegating
user may steer; an agent caller is refused.

| Cloud (`nativeAgents.ts`) | Remote action | Answer |
|---|---|---|
| capabilities, list | `agents#status` | `AgentsStatus {agents: AgentInfo[], grants: AgentGrant[]}` |
| `spawnAgent({name, task, grantSetId})` | `agents#spawn` `{grant, task}` | `{agentId, taskId}` |
| grant picker | `agents#request-grant` `{label, rooms, apps, expiresInSecs, color?, mayConfirm?}` | `{grant, pairingCode}` |
| revoke | `agent:<id>#revoke` | `{}` |
| `sendMessage(agentId, text)` | `agent:<id>#message` (text) | `{taskId}` |
| `stopTask` | `agent:<id>#stop` | `{}` |
| (new) pause, resume | `agent:<id>#pause`, `#resume` | `{}` |

`AgentGrant` carries `state` (`pending|approved|denied|expired|revoked`),
`color`, `expiresAtMs`, `mayConfirm`, and the `pairingCode` while pending.
`AgentInfo` carries `status` (`starting|running|waiting|paused|stopped|failed`), `room`,
`tasks[]` (`queued|running|completed|failed|cancelled`, each with the app
actions it performed), the conversation, the activity line, the device
key (the live publisher of its cursor) and the delegation.

- **Pause** holds the agent before its next step.
- **Stop** cancels the running task and every queued one. The agent then
  clears its presence and leaves.

```ts
interface Agents {                       // certifiedPeer.agents (web/agents.mjs)
  status(): Promise<AgentsStatus>;
  requestGrant(input: {label: string; rooms: string[]; apps: string[]; expiresInSecs: number;
    color?: string; mayConfirm?: boolean}): Promise<{grant: string; pairingCode: string}>;
  spawn(input: {grant: string; task: string; room?: string}): Promise<{agentId: string; taskId: string}>;
  message(agentId: string, text: string): Promise<{taskId: string}>;
  pause(agentId: string): Promise<void>;
  resume(agentId: string): Promise<void>;
  stop(agentId: string): Promise<void>;
  revoke(grantId: string): Promise<void>;
  verify(info: AgentInfo): VerifiedDelegation | null;  // against the user root, in wasm
  cursors(nowMs?: number): Array<{device: string; label: string; color: string;
    activity: string | null; focus: LiveRecord | null; pointer: [number, number] | null;
    selection: {anchor: string; head: string} | null; stale: boolean}>;
}
// Errors: AgentsError {code: 'refused' | 'unknown_action' | 'not_permitted' | 'timeout' | 'undeliverable' | ...}
```

Presence gained `participant: {kind: 'person' | 'agent', label, color:
'#rrggbb', activity} | null` in the wire frame, the wasm codec,
`live.publishPresence` and `live.remoteState()`. A page publishes its own
person label and colour the same way.

### `actionsV1` for apps

An agent answers `tilo_actions::remote` frames addressed to its device for
the records it has open, through the same door its own steps use
(`participant/actions.rs`):

- A catalogue has one provider per open record (`record:<app>:<record>`)
  with every action the record publishes, declared and projected. The
  room's provider (`workspace:<room>`) offers `app.<app>#open` for each
  delegated app.
- An invocation binds the caller's argument (`None`, `Text` as the
  prompted value, `Json` as typed arguments). An explicit `Confirmed`
  satisfies a confirmation step. The change is committed and the cursor
  moves as for the agent's own steps.
- Ids are idempotent: a retry gets the original receipt, and a different
  request under a used id is `InvocationConflict`. A request carrying a
  stale catalogue `revision` is refused.
- `Observe` answers `None` and `Cancel` answers `NotCancellable`: app actions
  complete when invoked.

The page reaches it as `peer.agents.actions(agentDevice)`:

```ts
interface AgentActions {                 // actionsV1 over the agent's device
  catalogue(query?: CatalogueQuery): Promise<CataloguePage>;
  invoke(target: {provider: string; address: {resource: string; action: string}},
         argument?: 'None' | {Text: string} | {Json: object},
         options?: {confirm?: boolean}): Promise<ActionOutput>;
}
```

### Every participant sees every cursor

Presence from iroh members was already bridged into the WebTransport live
relay. The other direction is new:

- `tilo_moq::live::LiveRelay::set_presence_tap` sees every presence frame
  a client session publishes. It never sees host-injected frames, so
  bridging both ways cannot loop.
- The web listener fans each such frame out to the workspace's iroh live
  members as `Envelope::Live` (`Workspace::relay_bridged_live`).

Browsers, native iroh peers and agents therefore share one live room.
Browser previews (`ops/<record>` tracks) are not bridged to iroh yet.

### Per-app coverage

Two tests exercise every registered web app, finding a user-equivalent
edit without app knowledge. They try, in order: the app's declared document
actions; filling empty non-search fields with a value of the kind the
field's name asks for; typing into text surfaces; other declared actions;
then buttons, submit-like first, skipping destructive or external ones. They
survey again after every attempt.

- `app_coverage` runs this on headless mounts.
- `session_coverage` runs it over the real session: an iroh agent with its
  own device and delegation edits, and an HTTPS/WebTransport browser merges
  the agent-authored changes into its own mount and sees the agent's
  cursor. It then runs a plain-words task through `ModelPlanner` and the
  `actionsV1` checks.

| App | User-equivalent edit | Via | Browser merged | Agent cursor |
|---|---|---|---|---|
| calculator | `calculator.workspace#add-function` | declared | yes | yes |
| clocks | none: device-local state file | — | yes (open) | yes |
| contacts | `contacts.book#new-contact` | declared | yes | yes |
| diffmerge | `diffmerge.merge#edit-merged` | declared | yes | yes |
| ebook | `ebook.library#toggle-hide-read` | declared | yes | yes |
| hex-editor | none: edits a local file | — | yes (open) | yes |
| image-editor | `image-editor.project#exposure` | declared | yes | yes |
| notes | `notes.notebook#new-page` | declared | yes | yes |
| passwords | none: device secret store | — | yes (open) | yes |
| scratchpad | `scratchpad.note#edit` | declared | yes | yes |
| sheets | `sheets.workbook#insert-column-chart` | declared | yes | yes |
| slides | `slides.deck#add-slide` | declared | yes | yes |
| snippets | `snippets.library#new-snippet` | declared | yes | yes |
| themes | `themes.library#new-theme` | declared | yes | yes |
| totp | fill Issuer/Account/Secret, press "Add account" | control input | yes | yes, with pointer |
| typst-editor | set "Typst source" | control input | yes | yes |
| word-processor | `word-processor.document#insert-table` | declared | yes | yes |

Clocks, hex-editor and passwords keep their state off the shared record
by design. The agent operates them like anyone else, and there is nothing to
sync. Image-editor's declared actions needed `component_actions` in its
definition, a fix that applies to every in-process host.

## Reused from PR 541

PR 541 (`refs/remotes/pr/541`, head `3ea4f4c9`) carries a working qOS-era
browser runtime. Parts that work on `main`'s APIs were ported, renamed
`qos-*` → `tilo-*`, and adapted; nothing was merged wholesale.

| Destination | Source in PR 541 | How |
|---|---|---|
| `crates/browser/tilo-browser-runtime/src/browser.rs` (1.2k lines) | `qos-browser-runtime/src/browser.rs` (2.1k) | Component path only: page-lifetime winit loop, WebGPU init, device-loss `recover`, scene limits, DPR viewport, status/semantics publication, IME, semantic actions, clipboard events, font demand/install, host-driven images, frame metrics. Records via `RecordOwners`/`InstanceViewStore` over `MemoryStore` instead of `PortableStorage` |
| `src/viewport.rs`, `src/clipboard.rs`, `src/semantics_status.rs` | same names | Copied; lint and API fixes (`ui::time`, `InspectionSnapshot` fields) |
| `src/frame_metrics.rs`, `src/throttle.rs` | `frame_metrics.rs`, `admitted_records.rs` (`PublicationSchedule`) | Trimmed to the parts the host uses |
| `src/storage.rs` | `storage_key.rs`, `load_cache`/`pending_writes`/`acknowledge_storage` | Rewritten over `durable_store::MemoryStore::queued` (hydrate/drain/acknowledge), keeping 541's JS row shape `{key, bytes, actor, revision}` |
| `web/host.mjs` | `web/host.mjs` | Mechanically trimmed: desktop navigation, action palette, desktop commands, speculation, live overlay and peer persistence removed |
| `web/host-core.mjs`, `font-loader.mjs`, `clipboard.mjs`, `semantics.mjs`, `storage.mjs`, `storage-inline.mjs`, `persistence-worker.mjs`, `persistence-protocol.mjs`, `transfer-staging.mjs`, `asset-recovery.mjs`, `close-recovery.mjs`, `testing/fake-storage.mjs` and their tests | same names | Copied and renamed; `createIdentityStore` removed from `storage.mjs`; peer-batch and desktop-navigation tests dropped. 120 node tests pass |
| `scripts/build.sh` | `scripts/build.sh` | MoQ bundle dropped; optional `--wasm-opt`; fonts from the manifest's `browser_startup` flags |
| `tilo-ui` `ime.rs`, `headless.rs` frame API, `image_store.rs` host factory | same files | Ported onto `main`'s `FrameHarness`; 541-only system-search and caller-palette branches left out |
| `tilo-ui` browser fonts (`font_catalog.rs`, `text_render.rs`, `query_text.rs`, `name_field.rs`, `rich_text_editor.rs`, manifest, `tilo-font-catalog`) | same files | Patch applied; `richtext.rs` browser-emoji entry points hand-ported without 541's paragraph chunking and layout reuse |
| `tilo-ui` rustls/`private-fs` exclusion, `tilo-user-identity` `persist` exclusion, `channel-crypto` `openmls/js`, wasm entropy deps | same | Ported; `tilo-ui` gating otherwise stays feature-based (`native`/`broker-transport` add `tokio/full`) |

## Deliberately not taken

| PR 541 part | Why |
|---|---|
| `qos-desktop`, `desktop_host.rs` (8.7k), speculation, desktop navigation/activity, committed desktop, system search | The desktop comes from `main`'s `tilo-desktop-model` and the shell's desktop through `ui::app`; a second browser-only compositor would fork the desktop |
| `peer*.rs`, `peer_delivery*`, `peer_journal`, `admission_health`, `admitted_records`, `peer_storage_batch`, `post_ready_repair` | A parallel HTTPS delivery/journal/admission stack. The browser peer uses `tilo-relay-client` over iroh (below) |
| `root_secret`/`device_secret` in IndexedDB, `cloud-identity-custody.mjs`, `identity.rs`, Cloud enrollment state machine, `peer_cloud/mesh.rs` (`ALLOWED_ACTIONS`) | Mesh policy belongs on the host. A page holds a device key (non-extractable WebCrypto, or wrapped) certified under the user's single root; a person with only a browser also holds that root, never stored unwrapped ([A browser-held root](#a-browser-held-root)) |
| `tilo-relay-protocol` → `qos-peer-state` dependency, `qos-agent-control-protocol` | qOS-lineage state model and agent protocol; `main` has `tilo_actions::remote` |
| `live_view`, `peer_live`, `peer_media`, MoQ (`media-*.mjs`, `moq-carrier.mjs`) | Deferred. Reusable later: `live-overlay.mjs` presence rendering, `media-audio*.mjs` worklets, `moq-carrier.mjs` WebTransport carrier |
| `HostImageLoader::retry`/`loaded`, `probe_pixels`, `destroy_device`, component close-recovery custody | Depend on 541-only internals or are test hooks; asset retry answers `false` for now |

## JS API for Cloud

Cloud should target `host.mjs` from the bundle (`/tilo-runtime/host.mjs`):

```ts
export const APPLICATIONS: {desktop: 'desktop', scratch: 'so.tilo.browser-scratch'};
export function mountDesktop(target: HTMLElement, options?: {
  app?: string;               // omit (or 'desktop') for the session desktop; else one app
  apps?: string[];            // desktop: initial windows, one per app id (default: all registered)
  profile?: string; device?: string; room?: string; author?: string;
  instance?: number; record?: string; newDocument?: boolean; mac?: boolean;
  signal?: AbortSignal;
  onStatus?: (status: RuntimeStatus) => void;   // phase: gpu-pending | presented | failed | closed
  onNavigation?: (navigation: DesktopNavigationV1) => void;  // desktop mounts, once ready
  loadAsset?: (url: string, signal: AbortSignal) => Promise<Uint8Array>;
  settings?: object;          // settings.json-shaped; seeds a profile with none stored
}): Promise<{
  cleanup(): Promise<void>; flush(): Promise<void>; state(): RuntimeStatus | undefined;
  navigation?: DesktopNavigationV1;             // desktop mounts
  canvas: HTMLCanvasElement; editor: HTMLTextAreaElement; semantics: HTMLElement;
  requestSemantics(): void; recover(): unknown; retryAsset(id: string): Promise<void>;
}>;
export function shutdown(): Promise<void>;
export function pendingRecoveryCount(): number;
export function retryPendingRecovery(): Promise<void>;
// The profile's durable storage for state kept outside a mount (the peer).
// Shares the writer lease with mounts of the same profile/device.
export function openProfileStorage(profile: string, device: string, options?: {settings?: object}): Promise<{
  flush(): Promise<void>;     // resolves when every accepted write is durable; retries failed keys
  commit(writes: Row[]): Promise<void>; hydrate(): Promise<Row[]>;
  status: {persistence: 'committed' | 'pending' | 'failed'; pendingWrites: number; error: string | null};
  subscribe(listener: (status) => void): () => void;
  release(): Promise<void>;   // the last release flushes, then frees the lock; keeps custody on failure
}>;
```

Rust state kept in the profile (the relay peer's) writes, inside
`tilo-browser-runtime`, to `crate::storage::store(profile, device)` under a
reserved `StorageKey::Package` id once the namespace is open; the durable host
commits it with everything else.

This matches what `localPeer.ts` and `DesktopSurface.tsx` already call
(Cloud omits `app` for the canonical session desktop). `DesktopNavigationV1`
is Cloud's `domain/navigation.ts` shape: `select`, `close`, `width`,
`maximize`, `strip` and `workspace` are performed; `workspace-activity`,
`history`, `pan`, `scroll`, `global-search` and `commands` are refused with a
reason and advertised as unavailable in `capabilities`. `connectPeer`/
`CertifiedPeer`, `SessionHealth` v1 and `actionsV1` are milestones 6–7.

## Desktop surface

`crates/session/desktop-surface` (`tilo-desktop-surface`) is the desktop any
host running the portable toolkit can present:

- `DesktopSurface` shows the active workspace's active strip as the
  toolkit's `GroupRow` of `WindowGroup`s of `TileHost`s. Each strip's row is
  built from the `DesktopTree` the first time it is shown and kept, so
  applications survive switching strips or workspaces.
- Gestures in the row (tab drag, split, close, activation, resize,
  maximize, reorder, the new-tab control) are harvested into the tree each
  update pass through `tilo-desktop-model`'s shared operations, as the
  shell's overlay host does. Navigation commands are admitted against the
  tree when dispatched and applied to tree and widgets in the next pass.
  A tree changed elsewhere goes through `DesktopHandle::replace_tree`
  (page: `desktop_apply_tree`): the next update pass rebuilds the rows by
  tab id, moving surviving tabs' running content and closing removed tabs'
  applications.
- Workspace Activity, the notification models and their Fluent catalogue live
  in `desktop-surface`; `tilo-desktop` reexports their native paths. Hosts
  install a `HistorySource` for Catch up, Explore and Replay. The browser
  reads the certified room's retained desktop document (or its profile's
  persisted Automerge log) and detached histories of the records its windows
  show. It retains author/time attribution and device-private seen frontiers;
  opening the pane does not mutate the live documents. Navigation publishes
  `inSurface` when the shared pane draws the view.
- Browser application notices use `AppNotifier` and the shared banner stack
  and centre. Expired and dismissed banners enter real retained history;
  the browser saves that bounded history through its profile's durable store.
  The native history format preserves age and app attribution across reloads
  while dropping actions whose sender belonged to the previous session.
  `notification-center` opens/closes it through the navigation API, whose
  `notifications` snapshot reports active and retained counts.
- Hosts supply tab contents through `TileSource`. The browser
  (`tilo-browser-runtime/src/desktop.rs`) maps `TileContent::LocalApp
  {app_id}` to a registered `ApplicationDefinition` and mounts it with
  `instantiate`, one instance id (the tab id), record owner per package and
  shared view store; component wakes mark the mount, which relayouts.
- `navigation` is `DesktopNavigationV1` over the model (wire types from
  PR 541's `desktop_navigation.rs` on desktop-model ids).

The room's tree and each tab's record id are kept in the profile's durable
store (`StorageKey::PackageRecord("so.tilo.browser-desktop", room)`, JSON)
and committed with the records, so a reload restores the same windows on
the same documents; a profile without one is seeded (one workspace, one
strip, a window per requested application). In a certified room the
tree is also shared with the room's other pages ([Shared sessions](#shared-sessions)).

## Shared sessions

A mount given `{peer}` (what `CertifiedPeer.mountDesktop` does) replicates
through that peer's room (`src/replication.rs`, one room per started
peer). The room's event queue is opened when the peer starts, so the
host's admission replay reaches mounts created later.

- **Records.** Every record a mount has open is published on the native
  shell's route, `NeighbourMsg::AppDocument {app, room, record}`, as
  Automerge deltas from a per-record `DocumentFrontier`
  (`RecordStore::replication_delta`). The first publication of a record in
  a session is a snapshot, which the host retains as the route's checkpoint
  for peers that join later. Inbound mutations merge through
  `RecordStore::merge_replica` (identity-validated on a private copy; local
  undo stays local) and the mount relayouts; a record no mount has open is
  merged into its stored document (`RecordId::storage_key`), so opening it
  later shows the edits. Conflicts are Automerge merges; the host orders
  commits (`Commit`/`Committed`) and fans them out before persisting.
- **Desktop.** The tree plus each tab's record is one small Automerge
  document on `so.tilo.browser-desktop/desktop` whose `desktop` value is the
  shared JSON; concurrent layouts resolve last-writer-wins identically on
  every page, and an inbound value reaches the surface through
  `replace_tree` (running tabs survive). Records go out before the desktop
  in each publication, so a page never sees a tab before its record.
  Desktop ids are salted with the certified device key.
- **Readiness.** Nothing is published before the host answered `Ready` for
  the room, so a page never announces a stale snapshot over what the room
  holds.
- **Carets and names.** The focused editor's caret or selection
  (`FrameHarness::focus_presence`) goes out in the status as live selection
  anchors; `host.mjs` publishes it with the page's `LiveParticipant {kind:
  person, label, color}` (`displayName`, else the certified device label).
  Remote frames are applied with `remote_presence` into the harness's
  `PresenceCtx`, and text inputs draw the member's caret or selection with
  its name. The participant kind (`person`/`agent`) is the agents branch's
  field in `tilo-relay-protocol::live`; agent cursors are marked there.
- **Liveness.** The WebTransport listener pings idle connections every
  3 s: pages' own keepalives run on timers that browsers throttle in
  background tabs, and without host pings an idle page's session dropped
  every 20–30 s at the 10 s QUIC idle timeout. The live layer retries a
  member's presence or ops subscription that ends while the member stays.

Measured in two headless Chrome instances on one machine (dev bundle,
`tilo-relay dev`): a keystroke in page A is presented in page B 39–49 ms
later (A's `/send` 10 ms after the key, B's peer event 5 ms after that,
B's frame 15–20 ms later); a tab opened in B appears in A after 64–67 ms;
B shows A's caret labelled "Ada"; after reloading B it has the same tabs
and A's text; neither page's live session changed state during 120 s.

### Local host

```sh
cargo run -p tilo-relay --features web -- dev            # 127.0.0.1:9443, allows Cloud's dev:tilo origins
cargo run -p tilo-relay --features agents -- dev --agents --agent-model scripted   # with agents
cargo run -p tilo-relay --features web -- run --web 0.0.0.0:9443 \
  --allowed-origin https://console.example --tls files:chain.pem,key.pem
```

`dev` is for development only: it keeps a throwaway user root, host device
and approver under `--state` (default `$TMPDIR/tilo-relay-dev`) so
certified browsers survive restarts, binds any room of that user on first
use, and certifies every browser that asks. It prints the connect JSON and
the matching Cloud command.

## Milestones

Status after integrating the parallel branches (`feat/browser-runtime`).

1. **Portable toolkit compiles for wasm32.** *Done.*
2. **Browser host.** *Done.* Component mount, WebGPU, IME, clipboard,
   semantics, fonts; the durable host (wasm `tilo_rt::app()`,
   `PageExecutor`, one IndexedDB writer lease per profile with write-hook
   commits, durable `flush`, retry of failed commits, custody-keeping
   release); host-provided settings; browser `HostServices`. Idle mounts
   present no frames. Pointer and Tab focus and real key events type into
   a component's text input (Scratch's page did not forward input and
   shared its child's layout node). Open: non-English spellcheck
   dictionaries, reading the clipboard outside a paste event.
3. **Portable apps.** *Open.* Per-app `native` feature forwarding
   (blocker 19); only the built-in Scratch is registered.
4. **Desktop surface.** *Done for one strip.* `tilo-desktop-surface` over
   `tilo-desktop-model` (blocker 20), navigation v1, persisted per room,
   tree→widget reconcile, shared per room. Open: semantic `activate` on
   the new-tab button; shell chrome; desktop input ownership
   (`desktop_input_ownership`).
5. **Make daemon clients optional.** *Done;* see
   [Local daemons](#local-daemons) and [Seams](#seams-the-page-installs).
6. **Real peer.** *Done: sessions, records, desktop, carets* ([Shared sessions](#shared-sessions)).
   - Host (`tilo-relay`): `Commit`/`Committed` ordering, device
     certification (ALPN `tilo-device-certify/1` and HTTPS), the browser
     listener behind `web` (HTTPS durable plane, WebTransport `moq-lite`
     live plane from `crates/hosting/tilo-moq`, HTTPS fallback).
   - `crates/browser/tilo-browser-peer`: session over HTTPS, outbox and
     replay, live state, `SessionHealth` v1, `RelayCollabTransport`;
     `tilo-relay dev` (and `examples/dev_host`) serve a local host with a
     development certificate and an auto-approving throwaway root.
   - Runtime: `WasmPeer`, `certifyDevice`, `hostInfo`, live codec,
     `web/peer.mjs`, `web/device-custody.mjs`, `live-cursors.mjs`.
     `connectPeer({profile, device})` keeps certificates and the outbox in
     the profile's durable store and mounts desktops there.
   - Verified with two headless Chrome instances (see [Shared
     sessions](#shared-sessions) for the numbers).
   - Open: a native shell in the same room is not verified yet (same
     routes, untested); text previews (`publishPreview`) are not produced
     by editors; a text input's caret is keyed by its document field, so
     two open records of one app share carets until inputs take a record
     scope; the desktop merges last-writer-wins rather than per tab; live
     frames from WebTransport members are not bridged back to iroh members
     (the agents branch adds that); `channel-crypto` group provisioning for
     the browser; `TlsSource::Managed`; a noq WebTransport; a native
     approver UI; member compaction by native shells (they do not
     volunteer) and by toolkit-only pages (they volunteer but ignore
     requests; the host asks the next volunteer on the route's next growth,
     30 s after the unanswered request).
7. **Actions.** *Agents done; `actionsV1` for apps open.*
   `tilo_actions::remote` as `actionsV1`/`actionsV2`, so agents can drive
   apps through the same routes as users. See [Agents](#agents).
   - Done: `tilo-relay-protocol::agents` (delegation, control vocabulary,
     `LiveParticipant` in presence); `Relay::register_agent`/`withdraw_agent`,
     `Caller::delegated_agent`, the browser-to-iroh presence bridge;
     remote-action frames in `tilo-browser-peer`;
     `crates/agent/agent-participant` (`HostedApp`, `AgentParticipant`,
     `AgentSupervisor` + `SupervisorHost`, `DirectPlanner`); `peer.agents`
     (`web/agents.mjs`), `sendRemoteAction`, `takeRemoteActionEvents`,
     `verifyAgentDelegation`. Tests: `shared_session`, `replicas`,
     `app_coverage`, `agents.test.mjs`, the moq presence-tap test.
   - `feat/sdk-agents`: agents open every web app from the manifest
     registry; headless controls in `tilo-ui`; `ModelPlanner`;
     `actionsV1` for apps on the agent's device (`peer.agents.actions`);
     `app_coverage` and `session_coverage` over all 17 web apps.
   - `feat/sdk-agenthost`: `AgentHost` in `tilo-relay dev|run --agents`
     (threads, persisted grants and agents, resume on restart, expiry and
     revocation), grants requested from the page and approved with their
     delegation in one step, `--agent-model settings|scripted|openai:`,
     pointers for declared actions, agent-styled cursors with activity.
   - Open: a native approver UI that shows the proposal (dev auto-approves);
     bridging browser `ops/<record>` previews to iroh; `actionsV1` served
     by the host for records no agent has open; the Cloud UI for agent
     cursors and the grant picker.
8. **Cloud cut-over.** *Open.*

## Verification

Keep these green on every change touching the portable toolkit:

```sh
cargo check -p tilo-ui --no-default-features --lib --target wasm32-unknown-unknown
cargo check -p tilo-relay-client --lib --target wasm32-unknown-unknown
cargo check -p tilo-ui --no-default-features --lib
cargo check -p tilo-ui --lib
cargo test -p tilo-desktop-surface -p tilo-desktop-model
cargo clippy --no-deps -p tilo-browser-runtime --lib --target wasm32-unknown-unknown
(cd scripts/moq-browser && npm ci && node build.mjs)   # once: the live tests import @moq/net
for t in crates/browser/tilo-browser-runtime/web/*.test.mjs; do node --test "$t"; done
crates/browser/tilo-browser-runtime/scripts/build.sh /abs/empty/dir --profile dev   # milestones only
```

For a browser run against a real host, serve the bundle and start
`cargo run -p tilo-relay --features web -- dev --web 127.0.0.1:8443
--allowed-origin http://127.0.0.1:8765`; pages call `connectPeer` with the
printed origin and user key and any room uuid (a test browser needs
`--ignore-certificate-errors` for the development certificate's HTTPS;
WebTransport pins it by hash).

Cloud (`npm run dev:tilo`, console on port 9998):

```sh
# in this repository
cargo run -p tilo-relay --features web -- dev
# in Cloud, with the user key the host printed
npm run dev:tilo -- --port 9998 --host-origin https://localhost:9443 --user-public-key <hex>
```
