# Tilo apps

Use hyphens for multiword package names, app directories, and executables. Rust
library names and imports use underscores (`code_editor`).

Every app here is a native Tilo application built on the public
[`tilo-app`](../crates/sdk/tilo-app) SDK (normally renamed to `ui` in Cargo).
`tilo-app` re-exports the complete retained-widget, layout, canvas,
custom-painting, native-view and GPU API. The internal
[`ui-core`](../crates/sdk/ui-core) package is a dependency of the shell and SDK;
apps use `tilo-app`.

Apps are retained-mode and CRDT-native: durable state lives in a shared
Automerge document for undo/redo, persistence and collaboration. Vello and Taffy
provide rendering and layout. Each app is its own workspace crate and has a
`--shots PATH` headless mode that renders the live production root offscreen for
review and CI. Apps that reach the network or hardware keep that behind an
off-by-default feature (`live`, `audio`, …) and fall back to bundled sample
data, so default builds and screenshots stay hermetic.

Toolkit exception: `browser` embeds Servo for page rendering.

## Application contract (required)

There is **one** resource / action / view model. Every app is a
[`tilo-app`](../crates/sdk/tilo-app) `ResourceApp`: its state is a
`tilo_resource::Resource`, its mutations are that resource's `Action` applied
through the framework dispatcher, and its pixels are a `ViewCx` /
`ResourceSurface` that only reads resource state and submits actions.
`tilo_resource::publish` is the only bridge to the tree the palette, agents,
scripts and the shell door see. Sync (Automerge, history, per-author undo,
collaboration) is on by default — an app does not implement its own sync loop.
External systems (CalDAV, a shared filesystem file) are projections drained by
an `Effect`; undo of a document action that wrote a queue entry is the inverse
action through `apply`.

Do **not** hand-build a second `tilo_actions::Resource` tree for the same state,
mutate a private `AutoCommit` from the widget, or keep an app-local action
vocabulary that is not `Resource::Action`.

UI controls, scripts and agents invoke the same typed operation:

1. `ResourceApp` + `tilo_resource::publish` expose stable resource and action
   IDs, labels, availability, permission, risk/confirmation, retry, execution,
   discovery, result, and typed parameters (including nested child resources).
2. CRDT mutations are the resource's serializable `Action`, registered under
   `Resource::ACTION_TYPE`. Views and effects are serializable operations
   handled only by `ResourceOperations::start_operation`; every operation type
   is declared by `local_resource_operation_handlers`.
3. Inputs needed by a headless caller are parameters, not hidden UI state. A
   file action may offer a portal when a person omits `path`, but agents and
   scripts must be able to provide the path explicitly. Saving into the shared
   filesystem is a document action on a file resource, not an effect.
4. `Immediate` means the work and its promised result exist before the handler
   returns. Queued, portal, network, worker, or service work returns `Accepted`,
   is polled by invocation ID, and terminates as succeeded, failed, or
   cancelled.
5. Permission and confirmation are contract data enforced again by the broker.
   Apps never accept a boolean such as `trusted` as authority and never report
   simulated external success.

For a zero-argument control:

```rust
let sync = ui::Button::new("sync", "Sync")
    .on_effect(move || sync_now());
let preview = ui::Button::new("preview", "Preview")
    .on_view(move || preview_state.toggle());
```

To bind a declared resource action:

```rust
let resource = project_resource(&project);
let binding = ui::tilo_actions::ActionBinding::unit_for(&resource, "build")?;
let build = ui::Button::new("build", "Build").on_action(binding);
```

Controls emit inert intents. The host permission-filters and re-resolves each
address against fresh resources, uses the standard confirmation UI where
required, and routes document/effect/view work through the shared execution and
lifecycle machinery. Do not mirror the operation in `on_click`, a private
message bus, or a hand-maintained command list.

For Ctrl+K, the host recursively flattens `Widget::resources` into command rows,
semantic actions and the same broker catalogue consumed by scripts and agents.
Custom widgets retain arbitrary rendering by owning an `ActionEmitter<T>`,
exposing its metadata through `control_action_bindings`, draining
`take_control_invocation`, and declaring explicit retained or synthetic
accessibility semantics whose activation emits the same intent.

The toolkit validates the live surface before publishing it, and the broker
validates untrusted descriptors again. Presubmit runs
`tools/policy/check_app_resource_action_contract.py` to reject missing resources
and unattached operation handlers. Invoke and inspect actions with:

```bash
cargo run -q --bin tilo-control -- actions list
cargo run -q --bin tilo-control -- actions invoke <provider> <id> --args-json '{"name":"value"}'
cargo run -q --bin tilo-control -- actions status <broker-invocation-id>
cargo run -q --bin tilo-control -- actions cancel <broker-invocation-id>
```

The same catalogue and invocation/status/cancel protocol is available to
workspace agents through the certified collaboration channel. Do not add a
second app-specific automation protocol.

Validate one app or the graphical workspace:

```bash
tilo-app check apps/my-app
tilo-app certify apps/my-app
tilo-app certify . --workspace
```

`check` includes structural validation, app-local formatting and Clippy, tests,
runtime certification and audited scenarios. `certify` runs the focused
production-root native audit. Direct `ui-core` dependencies, legacy callbacks,
stale bindings, missing semantics, and invalid or unhandled operations are
certification failures. The SDK exposes direct CRDT field controls as searchable
actions and records their mutations in shared history.

Generic controls are toolkit-owned. Application code must compose retained
widgets or their documented immediate-mode faces; drawing a button, segmented
selector, card, field, dialog, list row, or sidebar from raw rounded rectangles
is not a normal implementation option. Custom painting is reserved for the app's
domain visualization (for example a graph, map, waveform, or document canvas),
and every interactive custom region must publish a stable semantic target, name,
bounds, state, and the same action used by pointer input. Source presubmit
ratchets app-owned rounded-rectangle painting at its existing debt ceiling, so
new exceptions require an explicit baseline change and review.

For a control inside a legitimate domain canvas, construct one shared face and
reuse it for paint, input, and semantics. `minimum_target` preserves compact
painting while adapting the other bounds to the host pointer environment:

```rust
let add = ui::ButtonFace::icon_only(add_rect, ui::Icon::Plus, "Add function")
    .ghost()
    .minimum_target(self.interaction);

ui::Button::paint(scene, transform, text, theme, &add);
if add.hit_test(pointer) {
    self.invoke("graph:add");
}
let semantics = add.accessibility("graph:add");
```

Store the environment delivered by `Widget::set_interaction_environment`; do not
assume mouse sizing. `ButtonFace` and `SegmentedFace` keep paint, input, and
accessibility paired, while icon-only faces require a spoken name. Native
certification rejects discrete actionable targets below the 24×24 WCAG floor;
the preferred toolkit baseline remains 32×32 for fine pointers and 48×48 for
touch. Expanded neighbouring targets must not overlap.

## Productivity & documents

| Crate            | What it is                                                                                                                                                                                 |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `word-processor` | Rich-text word processor over `tilo-doc`; round-trips .odt/.docx/.md plus PDF/HTML/RTF/EPUB via the `tilo-doc-*` codec crates. Binary `word-processor`.                                    |
| `sheets`         | Spreadsheet: A1 addressing, tokenizer→parser→evaluator formula engine with dependency-aware recalc; .xlsx and .ods import/export through tilo-doc's bounded codecs, and .csv.              |
| `slides`         | Presentation editor: slide editor, inspector, thumbnail rail, present mode; image import via the XDG file-chooser portal.                                                                  |
| `notes`          | Collaborative block notes: nested note tree, rich Markdown editing, `[[Note]]` links, tables, formulas, images, metadata, history-aware actions, and host-owned Automerge storage.         |
| `calendar`       | Calendar + task manager (Planify-style): month/week/day/agenda, recurrence, tasks; iCalendar interop and live CalDAV sync (PROPFIND/REPORT/ETag PUT, worker thread, `live` on by default). |
| `contacts`       | Address book with vCard import/export.                                                                                                                                                     |
| `typst-editor`   | Split-view Typst editor: source pane + live compiled preview (`typst` / `typst-render` / tiny-skia), diagnostics panel, page thumbnails, background compile.                               |
| `calculator`     | Scientific calculator with 2D/3D function graphing.                                                                                                                                        |
| `shortcuts`      | Build and run shortcuts: pipelines of the actions your apps publish, typed bindings between steps, a grant review before saving.                                                           |
| `snippets`       | Snippet manager: taggable library, `${placeholder}` expansion, syntax-highlighted preview.                                                                                                 |
| `ebook`          | EPUB reader ("Books"): in-tree ZIP+OPF+XHTML parser, reflowable pagination, embedded cover art, shared annotation model.                                                                   |
| `pdf-viewer`     | Collaborative PDF reader ("PDF"): pdfium rasterisation and text layer (dynamically loaded); highlight/ink/sticky-note annotations + signing.                                               |

## Media

| Crate          | What it is                                                                                                                                                                                                                          |
| -------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `music`        | Streaming + local library: swappable YouTube-Music backend (rustypipe InnerTube under `live`), GStreamer `playbin` audio (`audio`), lrclib synced lyrics, iroh P2P library sync — all off by default over a bundled sample library. |
| `tube`         | Privacy YouTube client: Invidious metadata, SponsorBlock skip, DeArrow de-clickbait; playback through the shared GStreamer media engine; optional rustypipe `live` backend and Grayjay device sync over the Noise protocol.         |
| `video-editor` | Non-linear editor: multi-track timeline; clips reference FUSE `/var/workspace` paths and hydrate on demand; `ui::VideoPlayer`/GStreamer preview decode and GStreamer export, both feature-gated.                                    |
| `camera`       | Camera with QR/barcode and multi-page document scanning; typed SANE capture, reversible crop/rotation, and shared structured OCR; captured photos and videos open in their owning media apps; live v4l2 capture via GStreamer.      |
| `imager`       | Install-media creator + Tilo installer ("Install Media"): `lsblk` target enumeration, dd-style ISO write + verify, structured/unattended install config.                                                                            |
| `maps`         | OpenStreetMap client: Nominatim search, OSRM routing (car/bike/foot), Transitous/MOTIS transit; Vello vector basemap; device location via the XDG Location portal (GeoClue2).                                                       |
| `weather`      | Weather from the keyless Open-Meteo API: current conditions, 24h chart, 7-day forecast; reads display units from the shared Settings store.                                                                                         |

## Communication

| Crate    | What it is                                                                                                                                      |
| -------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| `mail`   | Email client: MIME parse/build via mail-parser / mail-builder; live IMAP (async-imap) + SMTP (lettre) behind the `live` feature.                |
| `chat`   | Workspace-scoped collaborative chat + calls; the room's own record, synced like every document; calls through the shell's native voice service. |
| `remote` | Remote-access app: SSH terminals, VNC/RDP desktops, and Tilo-to-Tilo relay sessions (SSH opens an embedded terminal grid).                      |
| `feeds`  | RSS/Atom/JSON-feed + Gemini reader (feed-rs, reqwest, tokio-rustls TOFU); OPML import/export.                                                   |

## System & OS

| Crate          | What it is                                                                                                                                                                                              |
| -------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `settings`     | OS settings hub; owns the shared external-accounts and region/format stores consumed by Mail/Calendar/Sheets/Weather; drives hardware via the `os-actions` crate.                                       |
| `passwords`    | Password and secret manager: encrypted metadata and credentials, strong-password generation, exact-origin matching, and consent-brokered app access.                                                    |
| `permissions`  | Tilo's OS-wide permission model (roles / members / contexts / registered `PermissionDef`s) — both a reusable library and a standalone role/permission editor.                                           |
| `themes`       | Theme studio: browse presets, edit the OS token set live, duplicate/export themes.                                                                                                                      |
| `store`        | Unified Tilo app store: verified registry packages plus Flatpak/Flathub search, details, remotes, install, update, launch, and removal.                                                                 |
| `containers`   | Rootless-podman + distrobox manager; mirrors live `podman` engine state with lifecycle actions and Logs/Stats/Inspect/Terminal tabs.                                                                    |
| `disks`        | Disk utility: `lsblk` + `smartctl` topology, proportional partition map, SMART health summary.                                                                                                          |
| `backups`      | Snapshot/backup manager: sources, snapshot timeline, storage gauges, retention policy, restore sheet.                                                                                                   |
| `monitor`      | Activity monitor: `sysinfo` process table + resource dashboard, process tree, Vello charts.                                                                                                             |
| `print-centre` | Print Centre: live queues, per-job progress/recovery, IPP diagnostics and supplies, secure driverless discovery, and scanner inventory over the shared printing service.                                |
| `logs`         | systemd-journal viewer: `journalctl -o json`, unit/boot/priority filters, live tail.                                                                                                                    |
| `firewall`     | firewalld/nftables front-end: zone rail, per-zone rules table, services; applies via `firewall-cmd`.                                                                                                    |
| `vpn`          | WireGuard / OpenVPN client: saved connection fleet, live detail pane with animated throughput chart.                                                                                                    |
| `speed-test`   | Responsive internet speed test: latency, jitter, download and upload measurement with region selection and result history.                                                                              |
| `totp`         | 2FA authenticator ("Authenticator"): RFC 6238 TOTP on RustCrypto HMAC/SHA, QR import (rqrr), XChaCha20-Poly1305 encrypted-at-rest vault.                                                                |
| `osk`          | On-screen keyboard: data-driven layouts, responsive sizing, SHARK²/FUTO swipe-typing (`swipe` crate, ExecuTorch FFI), text injection via `virtual-keyboard-v1`. Foundation for the Tilo phone keyboard. |

## Developer tools

| Crate         | What it is                                                                                                                           |
| ------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `terminal`    | GPU-accelerated terminal: portable-pty + vte; embeddable as a tile widget in the shell.                                              |
| `code-editor` | Code editor / light IDE ("Code"): file-tree, editor tabs, syntect highlighting, command palette, problems panel.                     |
| `git-client`  | Graphical git client ("Git"): libgit2 via `git2`; changes with per-hunk staging, commit-graph history, branches/remotes.             |
| `diffmerge`   | Diff + 3-way merge tool ("Diff"): `similar` line diff, per-hunk conflict resolution, editable merged output.                         |
| `db-client`   | Database client ("Database"): PostgreSQL/MySQL/SQLite, bundled SQLite sample (rusqlite), schema browser + SQL editor + results grid. |
| `hex-editor`  | Hex editor ("Hex"): CRDT byte buffer, data inspector, hex/text search, goto-offset.                                                  |
| `api-tester`  | HTTP client (Postman/Insomnia-class): reqwest, request collections, `{{variable}}` environments, JSON/raw/preview body viewer.       |

## Files & web

| Crate           | What it is                                                                                                                                                                                                                                                                           |
| --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `file-explorer` | Spatial file manager with safe native ZIP/TAR inspection, extraction and creation; opens editable documents in the word processor via `tilo-doc`. Binary `file-explorer`.                                                                                                            |
| `agent`         | `tilo-agent`: a participant without a window. Consumes the broker-published live catalogue for arbitrary applications, plans typed arguments from their descriptors, invokes through broker permission/confirmation policy, and follows invocation receipts through terminal status. |
| `browser`       | Web browser embedding Servo (libservo + surfman + glow) with adblock; chrome is a `ui` widget (migrated off iced).                                                                                                                                                                   |

## Building

All apps are workspace members; build from the repo root. Use the
[platform setup](../docs/development/README.md#platform-setup) for your host and
the apps you're changing:

```bash
cargo build -p terminal -p file-explorer -p browser
cargo run -p notes -- --shots out.png        # headless offscreen frame
```

Each app depends on the public SDK under the concise source name
`ui = { package = "tilo-app", path = "../../crates/sdk/tilo-app" }`. Direct
dependencies on the internal `ui` package are reserved for framework code and
are rejected at the app boundary.
