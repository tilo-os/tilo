# qOS product focus

The **main-focus apps** below are the priority — for building, fixing, and
demoing. Everything else in `apps/` (dev/system tools: disks, hex_editor,
db_client, logs, api_tester, snippets, firewall, vpn, containers, backups,
permissions, remote, imager, themes, store, monitor…) is secondary for
now: keep them working, but do not prioritize them over the focus list.

## Focus apps + intent

| App | Intent / goals |
|---|---|
| **YouTube** (tube) | YouTube client now; later PeerTube et al. |
| **Notes** | Block/outline editor. Its widgets are reused by Calendar (attach notes to events). |
| **Word processor** | Rich text; shares annotation/comment primitives with PDF + ebook. |
| **Spreadsheet** (sheets) | — |
| **Email** (mail) | — |
| **Terminal** | Also becomes a reusable embeddable widget. |
| **Files** (file-explorer) | — |
| **Browser** | See "Browser engine" below — a real decision. Goals, in order: (1) a *serious* browser, (2) built-in ad blocking, (3) independent from Google/Chromium, (4) proper extension support incl. **Manifest v2**. |
| **Presentation** (slides) | — |
| **Calculator / graphing** | A *normal* calculator that also happens to do proper 2D/3D graphing — must feel at home as a calculator, not a graphing tool wearing a calculator hat. |
| **Typst editor** | — |
| **Video editor** | — |
| **Music** | Custom streaming: free YT-Music + downloads/caching + **our own library layer** for cross-device sync. (Spotify a fallback question.) |
| **Calendar** | Planify-style — between calendar and to-do. Reuse the Notes app/widgets to attach notes to events. |
| **Contacts** | — |
| **PDF reader** | Full signing + annotation + comments; later maybe full PDF *editing* (nothing does this natively on Linux — probably for a reason). |
| **Camera** | QR support now; full camera later (with phone support). |
| **Chat / video call** | Bespoke — see "Chat" below. Office/work needs it; it's collaborative + workspace-bound. |
| **RSS** (feeds) | Optional Gemini support. |
| **E-book** | Annotations/bookmarks/comments — shared primitives with PDF + word processor. |
| **2FA TOTP** | — |
| **Weather** | — |
| **Maps** | OSM-based; car/bike/foot nav (OSMand / Organic Maps-like); strong public transport (oeffi / public-transport-enabler-like); a custom crowdsourced real-time traffic + route-data layer over OSM. |

## Shared primitives to consolidate

- **Annotation / comment model** — PDF, ebook, word processor all need it. One shared model + selection HUD.
- **Notes widgets** — reused by Calendar (event notes). Notes is a dependency, not just an app.
- **Terminal-as-widget** — embeddable, reused by the IDE and anywhere a shell is needed.

## Chat / video — DECIDED: bespoke, over MoQ

Not Matrix. Rationale: qOS already provides most of what Matrix buys (identity,
ad-hoc P2P, permissions), and Matrix's home-server + auth model is
architecturally incompatible with our ad-hoc P2P model — integrating it would be
customization hell.

- **Chat**: channels tied to workspaces; integrates the permission system;
  real-time + persistent messages; edits + edit history; emoji reactions; full
  media + file sharing via the FUSE FS.
- **Video**: extremely low latency; P2P via **iroh + a MoQ impl that supports
  iroh**; client-server via standard relays; conferencing, mute, audio-only,
  permissions. **No traditional screen-share** — everyone is (or can be) on the
  same workspace, so instead use the native collaboration layer to "show stuff":
  clients follow the host's actions, with customizable follow settings (e.g.
  auto-scroll with them).

## Browser engine — open decision

Goals conflict, and no cleanly-embeddable engine satisfies all four today:

| Engine | Serious | Ad block | Independent of Google/Chromium | Manifest v2 | Cleanly embeddable in a custom shell |
|---|---|---|---|---|---|
| Chromium (CEF) | ✅ | ✅ (MV3 weakens) | ❌ | ❌ (killed) | ✅ |
| **Gecko** (Firefox) | ✅ | ✅✅ (uBO) | ~✅ | ✅ | ❌ (no supported desktop embed API) |
| WebKit (WebKitGTK) | ~✅ | ~ | ~ (Apple) | ❌ | ✅ |
| **Servo** (libservo) | ❌ not yet | ~ (has an adblock crate) | ✅✅ (Rust, LF) | ❌ | ✅ (already used by qOS) |
| Ladybird | ❌ early | ❌ | ✅ | ❌ | — |

The only engine that is serious **and** independent **and** keeps MV2 is
**Gecko** — but it's the hardest to embed. **Servo** is the perfect fit on
independence + Rust-native + already-integrated, but isn't production-serious yet.
Recommendation captured in chat; see the discussion for the two-track proposal
(Servo as the endgame; a serious interim engine chosen by which goal is sacrificed
short-term).

## Functional status (2026-07-05 code audit)

Read from the code, not screenshots. Dominant pattern: real engine/read path,
stubbed or absent write/action/connect path (a wiring job, not a rebuild).

- **real**: notes, sheets, file-explorer, calculator, contacts, pdf_viewer
  (fallback-text only), typst_editor, browser (engine), feeds, ebook, totp,
  weather, maps, monitor, api_tester, logs, git_client, containers, permissions
  (authoring), store, camera (feature-gated).
- **partial** (real engine, stubbed write/action/connect): word-processor,
  terminal, slides (no save), video_editor (no decode/export), music (default
  silent), calendar (no delete), chat (calls stubbed), disks, snippets, themes,
  firewall, vpn, imager (fake write), hex_editor (no save), db_client (demo DB),
  diffmerge (no save), code_editor, osk, settings.
- **shell** (no real backend): mail (no IMAP/SMTP), remote, backups, tube default.

**Focus-gap priority:** mail engine → slides .odp save → calendar delete →
browser ad-block → pdf pdfium text → video decode/export → chat calls → tube live.
