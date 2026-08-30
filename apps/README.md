# qOS apps

Every app here is a native qOS application built on the in-tree [`ui`](../ui) toolkit:
retained-mode and CRDT-native — durable state lives in a shared Automerge document, so
undo/redo, persistence and live collaboration ride one mechanism — drawn into a Vello
scene and laid out with Taffy. Each is its own workspace crate and has a `--shots PATH`
headless mode that renders one offscreen frame to a PNG (identical pixels to the live
binary) for review and CI. Apps that reach the network or hardware keep that behind an
off-by-default feature (`live`, `audio`, …) and fall back to bundled sample data, so the
default build and `--shots` stay hermetic.

Toolkit exception: `browser` embeds Servo for page rendering.

## Productivity & documents

| Crate | What it is |
|---|---|
| `word-processor` | Rich-text word processor over `qdoc`; round-trips .odt/.docx/.md plus PDF/HTML/RTF/EPUB via the `qdoc-*` codec crates. Binary `word-processor`. |
| `sheets` | Spreadsheet: A1 addressing, tokenizer→parser→evaluator formula engine with dependency-aware recalc; .xlsx (calamine / rust_xlsxwriter), .ods and .csv import/export. |
| `slides` | Presentation editor: slide editor, inspector, thumbnail rail, present mode; image import via the XDG file-chooser portal. |
| `notes` | Markdown note-taking: page side-tree, block editor, `[[Page]]` links, on-disk `~/notes/` storage with debounced save. |
| `calendar` | Calendar + task manager (Planify-style): month/week/day/agenda, recurrence, tasks; iCalendar interop and live CalDAV sync (PROPFIND/REPORT/ETag PUT, worker thread, `live` on by default). |
| `contacts` | Address book with vCard import/export. |
| `typst_editor` | Split-view Typst editor: source pane + live compiled preview (`typst` / `typst-render` / tiny-skia), diagnostics panel, page thumbnails, background compile. |
| `calculator` | Scientific calculator with 2D/3D function graphing. |
| `snippets` | Snippet manager: taggable library, `${placeholder}` expansion, syntax-highlighted preview. |
| `ebook` | EPUB reader ("Books"): in-tree ZIP+OPF+XHTML parser, reflowable pagination, real embedded cover art, shared annotation model. |
| `pdf_viewer` | Collaborative PDF reader ("PDF"): pdfium rasterisation (dynamically loaded) with a pure-Rust text-extract fallback; highlight/ink/sticky-note annotations + signing. |

## Media

| Crate | What it is |
|---|---|
| `music` | Streaming + local library: swappable YouTube-Music backend (rustypipe InnerTube under `live`), GStreamer `playbin` audio (`audio`), lrclib synced lyrics, iroh P2P library sync — all off by default over a bundled sample library. |
| `tube` | Privacy YouTube client: Invidious metadata, SponsorBlock skip, DeArrow de-clickbait; openh264 H.264 decode; optional rustypipe `live` backend and Grayjay device sync over the Noise protocol. |
| `video_editor` | Non-linear editor: multi-track timeline; clips reference FUSE `/var/workspace` paths and hydrate on demand; `ui::VideoPlayer`/openh264 preview decode and GStreamer export, both feature-gated. |
| `camera` | Camera + gallery + QR/barcode scanner (real decode via rqrr, encode via qrcode); live v4l2 capture via GStreamer behind a feature. |
| `imager` | Install-media creator + qOS installer ("Install Media"): `lsblk` target enumeration, dd-style ISO write + verify, structured/unattended install config. |
| `maps` | OpenStreetMap client: Nominatim search, OSRM routing (car/bike/foot), Transitous/MOTIS transit; Vello vector basemap; device location via the XDG Location portal (GeoClue2). |
| `weather` | Weather from the keyless Open-Meteo API: current conditions, 24h chart, 7-day forecast; reads display units from the shared Settings store. |

## Communication

| Crate | What it is |
|---|---|
| `mail` | Email client: MIME parse/build via mail-parser / mail-builder; live IMAP (async-imap) + SMTP (lettre) behind the `live` feature. |
| `chat` | Workspace-scoped collaborative chat + calls; Automerge document persisted to disk; voice/screen-share over iroh QUIC + GStreamer + the ScreenCast portal behind `live-media`. |
| `remote` | Remote-access app: SSH terminals, VNC/RDP desktops, and qOS-to-qOS relay sessions (SSH opens an embedded terminal grid). |
| `feeds` | RSS/Atom/JSON-feed + Gemini reader (feed-rs, reqwest, tokio-rustls TOFU); OPML import/export. |

## System & OS

| Crate | What it is |
|---|---|
| `settings` | OS settings hub; owns the shared external-accounts and region/format stores consumed by Mail/Calendar/Sheets/Weather; drives hardware via the `os-actions` crate. |
| `permissions` | qOS's OS-wide permission model (roles / members / contexts / registered `PermissionDef`s) — both a reusable library and a standalone role/permission editor. |
| `themes` | Theme studio: browse presets, edit the OS token set live, duplicate/export themes. |
| `store` | Unified qOS app store: verified registry packages plus Flatpak/Flathub search, details, remotes, install, update, launch, and removal. |
| `containers` | Rootless-podman + distrobox manager; mirrors live `podman` engine state with lifecycle actions and Logs/Stats/Inspect/Terminal tabs. |
| `disks` | Disk utility: `lsblk` + `smartctl` topology, proportional partition map, SMART health summary. |
| `backups` | Snapshot/backup manager: sources, snapshot timeline, storage gauges, retention policy, restore sheet. |
| `monitor` | Activity monitor: `sysinfo` process table + resource dashboard, process tree, Vello charts. |
| `logs` | systemd-journal viewer: `journalctl -o json`, unit/boot/priority filters, live tail. |
| `firewall` | firewalld/nftables front-end: zone rail, per-zone rules table, services; applies via `firewall-cmd`. |
| `vpn` | WireGuard / OpenVPN client: saved connection fleet, live detail pane with animated throughput chart. |
| `totp` | 2FA authenticator ("Authenticator"): RFC 6238 TOTP on RustCrypto HMAC/SHA, QR import (rqrr), XChaCha20-Poly1305 encrypted-at-rest vault. |
| `osk` | On-screen keyboard: data-driven layouts, responsive sizing, SHARK²/FUTO swipe-typing (`swipe` crate, ExecuTorch FFI), text injection via `virtual-keyboard-v1`. Foundation for the qOS phone keyboard. |

## Developer tools

| Crate | What it is |
|---|---|
| `terminal` | GPU-accelerated terminal: portable-pty + vte; embeddable as a tile widget in the shell. |
| `code_editor` | Code editor / light IDE ("Code"): file-tree, editor tabs, syntect highlighting, command palette, problems panel. |
| `git_client` | Graphical git client ("Git"): libgit2 via `git2`; changes with per-hunk staging, commit-graph history, branches/remotes. |
| `diffmerge` | Diff + 3-way merge tool ("Diff"): `similar` line diff, per-hunk conflict resolution, editable merged output. |
| `db_client` | Database client ("Database"): PostgreSQL/MySQL/SQLite, bundled SQLite sample (rusqlite), schema browser + SQL editor + results grid. |
| `hex_editor` | Hex editor ("Hex"): CRDT byte buffer, data inspector, hex/text search, goto-offset. |
| `api_tester` | HTTP client (Postman/Insomnia-class): reqwest, request collections, `{{variable}}` environments, JSON/raw/preview body viewer. |

## Files & web

| Crate | What it is |
|---|---|
| `file-explorer` | Spatial file manager: walkdir / trash / infer / mime_guess; opens editable documents in the word processor via `qdoc`. Binary `file-explorer`. |
| `agent` | `qos-agent`: a participant without a window. Joins a word-processor document through the session's collaboration broker and carries out instructions written in it (`Scribe: turn the bullets into a table`) with the word processor's own exported actions, under its own actor. |
| `browser` | Web browser embedding Servo (libservo + surfman + glow) with adblock; chrome is a `ui` widget (migrated off iced). |

## Building

All apps are workspace members; build from the repo root (see the top-level
[`README`](../README.md) and `DEVELOPMENT.md` for the distrobox build environment):

```bash
cargo build -p terminal -p file-explorer -p browser
cargo run -p notes -- --shots out.png        # headless offscreen frame
```

Each app depends on the in-tree `ui` toolkit (`ui = { path = "../../ui" }`), which resolves
only inside the workspace checkout where `ui/` is a sibling.
